//! One-to-one calls: the signaling state machine (pure) and the async steps
//! the app runs as tasks (DH, `phone.*`). The media engine lives in
//! `net::calls::engine`; this module decides when to start and stop it.
//!
//! ```text
//! outgoing: Requesting → Waiting → Ringing → ExchangingKeys → Connecting → Active
//!           (requestCall)  (receive_date)  (Accepted: confirmCall)  (engine)
//! incoming: Ringing → ExchangingKeys → Connecting → Active
//!           (Requested: receivedCall) (acceptCall … phoneCall: check g_a)
//! any phase → Ended(reason) on phoneCallDiscarded, hang-up or failure
//! ```

use std::time::Duration;

use crate::error::{Error, Result};
use crate::model::ChatId;
use crate::net::ApiHandle;
use crate::net::calls::dh::{self, CallKey, DhConfig, Secret};
use crate::net::calls::engine::{EngineConfig, LinkState};
use crate::net::calls::{self, CallConfig, CallRef, DiscardReason, PhoneCall};

/// Signaling packets kept while the engine is not running yet.
pub const MAX_EARLY_SIGNALING: usize = 64;
/// How long the "Call ended" card stays.
pub const ENDED_LINGER: i64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Outgoing,
    Incoming,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndReason {
    HungUp,
    /// The peer declined (busy).
    Declined,
    /// Nobody answered / the caller gave up.
    Missed,
    Busy,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// Outgoing: waiting for the server.
    Requesting,
    /// Outgoing: the server has the call, the peer's device has not rung yet.
    Waiting,
    /// Outgoing: the peer's device rings. Incoming: we ring.
    Ringing,
    ExchangingKeys,
    Connecting,
    /// Media flows since `since` (unix seconds).
    Active {
        since: i64,
        relay: bool,
    },
    Reconnecting {
        since: i64,
    },
    /// Shown briefly, then the session is dropped.
    Ended {
        reason: EndReason,
        at: i64,
    },
}

/// Our DH half: config, secret exponent and public value.
#[derive(Debug, Clone)]
pub struct DhHalf {
    pub config: DhConfig,
    pub secret: Secret,
    pub public: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct CallSession {
    pub peer: ChatId,
    pub direction: Direction,
    pub phase: Phase,
    pub call: Option<CallRef>,
    /// Incoming: hash of the caller's `g_a` from `phoneCallRequested`.
    pub g_a_hash: Option<Vec<u8>>,
    /// Outgoing: our `g_a`; incoming: the caller's (for the emoji).
    pub g_a: Option<Vec<u8>>,
    pub dh: Option<DhHalf>,
    pub key: Option<CallKey>,
    pub emoji: Option<[&'static str; 4]>,
    /// The final `phoneCall` (connections, protocol) once known.
    pub active: Option<PhoneCall>,
    pub muted: bool,
    pub quality: u8,
    /// When the session entered its current ringing phase (unix seconds).
    pub ring_started: i64,
    pub early_signaling: Vec<Vec<u8>>,
    /// `g_b` / final call that arrived before our DH half was ready.
    pending_g_b: Option<Vec<u8>>,
    pending_finish: Option<(Vec<u8>, i64)>,
}

/// Things the app must do; returned by the reducer, run as tasks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmd {
    /// Outgoing: DH config, `g_a`, `phone.requestCall`.
    Request {
        peer: ChatId,
    },
    /// `phone.receivedCall` (we are ringing) + notification.
    Received {
        call: CallRef,
        peer: ChatId,
    },
    /// Turn down a call we cannot take (already in a call).
    Reject {
        call: CallRef,
        reason: DiscardReason,
    },
    /// Outgoing: derive the key from `g_b` and `phone.confirmCall`.
    Confirm {
        call: CallRef,
        g_b: Vec<u8>,
    },
    /// Incoming: DH config, `g_b`, `phone.acceptCall`.
    Accept {
        call: CallRef,
    },
    /// Incoming: check the revealed `g_a`, derive the key, check the
    /// fingerprint.
    Finish {
        g_a: Vec<u8>,
        fingerprint: i64,
    },
    /// Start the media engine (key and connections are in the session).
    StartEngine,
    StopEngine,
    Discard {
        call: CallRef,
        duration: i32,
        reason: DiscardReason,
    },
}

/// The app's call slot (at most one call at a time).
#[derive(Debug, Default, Clone)]
pub struct Calls {
    pub session: Option<CallSession>,
    pub config: CallConfig,
}

impl CallSession {
    fn new(peer: ChatId, direction: Direction, phase: Phase, now: i64) -> Self {
        Self {
            peer,
            direction,
            phase,
            call: None,
            g_a_hash: None,
            g_a: None,
            dh: None,
            key: None,
            emoji: None,
            active: None,
            muted: false,
            quality: 0,
            ring_started: now,
            early_signaling: Vec::new(),
            pending_g_b: None,
            pending_finish: None,
        }
    }

    pub fn is_ended(&self) -> bool {
        matches!(self.phase, Phase::Ended { .. })
    }

    /// Seconds of connected time (for the duration and `discardCall`).
    pub fn duration(&self, now: i64) -> i32 {
        match self.phase {
            Phase::Active { since, .. } | Phase::Reconnecting { since } => {
                i32::try_from((now - since).max(0)).unwrap_or(i32::MAX)
            }
            _ => 0,
        }
    }

    /// The label under the name, like the official apps.
    pub fn status_text(&self, now: i64) -> String {
        match &self.phase {
            Phase::Requesting => "Requesting…".into(),
            Phase::Waiting => "Waiting…".into(),
            Phase::Ringing => match self.direction {
                Direction::Outgoing => "Ringing…".into(),
                Direction::Incoming => "Incoming call".into(),
            },
            Phase::ExchangingKeys => "Exchanging encryption keys…".into(),
            Phase::Connecting => "Connecting…".into(),
            Phase::Active { .. } => format_duration(self.duration(now)),
            Phase::Reconnecting { .. } => "Reconnecting…".into(),
            Phase::Ended { reason, .. } => match reason {
                EndReason::HungUp => "Call ended".into(),
                EndReason::Declined => "Call declined".into(),
                EndReason::Missed => "No answer".into(),
                EndReason::Busy => "Line busy".into(),
                EndReason::Failed(_) => "Call failed".into(),
            },
        }
    }
}

pub fn format_duration(s: i32) -> String {
    let s = s.max(0);
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

impl Calls {
    pub fn is_busy(&self) -> bool {
        self.session.as_ref().is_some_and(|s| !s.is_ended())
    }

    pub fn is_ringing_in(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|s| s.direction == Direction::Incoming && s.phase == Phase::Ringing)
    }

    /// The user pressed "Call".
    pub fn start(&mut self, peer: ChatId, now: i64) -> Vec<Cmd> {
        if self.is_busy() || peer <= 0 {
            return Vec::new();
        }
        self.session = Some(CallSession::new(
            peer,
            Direction::Outgoing,
            Phase::Requesting,
            now,
        ));
        vec![Cmd::Request { peer }]
    }

    /// `requestCall` returned (our DH half and the call).
    pub fn placed(&mut self, dh_half: DhHalf, call: &PhoneCall, me: i64, now: i64) -> Vec<Cmd> {
        let Some(s) = self.session.as_mut() else {
            return discard_orphan(call);
        };
        if s.direction != Direction::Outgoing || s.call.is_some_and(|c| c.id != call.id()) {
            return Vec::new();
        }
        s.g_a = Some(dh_half.public.clone());
        s.dh = Some(dh_half);
        let mut cmds = self.on_phone_call(call, me, now);
        if let Some(s) = self.session.as_mut()
            && let (Some(g_b), Some(call)) = (s.pending_g_b.take(), s.call)
        {
            cmds.push(Cmd::Confirm { call, g_b });
        }
        cmds
    }

    /// `requestCall` / `acceptCall` / key steps failed.
    pub fn failed(&mut self, why: &str, now: i64) -> Vec<Cmd> {
        let Some(s) = self.session.as_mut() else {
            return Vec::new();
        };
        if s.is_ended() {
            return Vec::new();
        }
        let call = s.call;
        let duration = s.duration(now);
        s.phase = Phase::Ended {
            reason: EndReason::Failed(why.to_owned()),
            at: now,
        };
        let mut cmds = vec![Cmd::StopEngine];
        if let Some(call) = call {
            cmds.push(Cmd::Discard {
                call,
                duration,
                reason: DiscardReason::Disconnect,
            });
        }
        cmds
    }

    /// The user accepted the incoming call.
    pub fn accept(&mut self) -> Vec<Cmd> {
        let Some(s) = self.session.as_mut() else {
            return Vec::new();
        };
        match (s.direction, &s.phase, s.call) {
            (Direction::Incoming, Phase::Ringing, Some(call)) => {
                s.phase = Phase::ExchangingKeys;
                vec![Cmd::Accept { call }]
            }
            _ => Vec::new(),
        }
    }

    /// `acceptCall` returned.
    pub fn accepted(&mut self, dh_half: DhHalf, call: &PhoneCall, me: i64, now: i64) -> Vec<Cmd> {
        let Some(s) = self.session.as_mut() else {
            return discard_orphan(call);
        };
        s.dh = Some(dh_half);
        let mut cmds = self.on_phone_call(call, me, now);
        if let Some(s) = self.session.as_mut()
            && let Some((g_a, fingerprint)) = s.pending_finish.take()
        {
            cmds.push(Cmd::Finish { g_a, fingerprint });
        }
        cmds
    }

    /// Hang up / decline / cancel, with the reason the official apps use.
    pub fn hang_up(&mut self, now: i64) -> Vec<Cmd> {
        let Some(s) = self.session.as_mut() else {
            return Vec::new();
        };
        if s.is_ended() {
            self.session = None;
            return Vec::new();
        }
        let reason = match (s.direction, &s.phase) {
            (Direction::Outgoing, Phase::Requesting | Phase::Waiting | Phase::Ringing) => {
                DiscardReason::Missed
            }
            (Direction::Incoming, Phase::Ringing) => DiscardReason::Busy,
            _ => DiscardReason::Hangup,
        };
        let duration = s.duration(now);
        let call = s.call;
        s.phase = Phase::Ended {
            reason: EndReason::HungUp,
            at: now,
        };
        let mut cmds = vec![Cmd::StopEngine];
        if let Some(call) = call {
            cmds.push(Cmd::Discard {
                call,
                duration,
                reason,
            });
        }
        cmds
    }

    /// The key is ready (outgoing after confirm, incoming after finish).
    pub fn key_ready(&mut self, key: CallKey, emoji: [&'static str; 4]) -> Vec<Cmd> {
        let Some(s) = self.session.as_mut() else {
            return Vec::new();
        };
        s.key = Some(key);
        s.emoji = Some(emoji);
        if s.active.is_some() && matches!(s.phase, Phase::ExchangingKeys) {
            s.phase = Phase::Connecting;
            return vec![Cmd::StartEngine];
        }
        Vec::new()
    }

    /// Media engine state.
    pub fn link(&mut self, state: LinkState, now: i64) {
        let Some(s) = self.session.as_mut() else {
            return;
        };
        s.phase = match (state, &s.phase) {
            (_, Phase::Ended { .. }) => return,
            (
                LinkState::Connected { relay },
                Phase::Active { since, .. } | Phase::Reconnecting { since },
            ) => Phase::Active {
                since: *since,
                relay,
            },
            (LinkState::Connected { relay }, _) => Phase::Active { since: now, relay },
            (LinkState::Reconnecting, Phase::Active { since, .. }) => {
                Phase::Reconnecting { since: *since }
            }
            (LinkState::Reconnecting | LinkState::Connecting, p) => p.clone(),
        };
    }

    /// Once a second: ring timeout and clearing the "ended" card.
    pub fn tick(&mut self, now: i64) -> Vec<Cmd> {
        let Some(s) = self.session.as_ref() else {
            return Vec::new();
        };
        match &s.phase {
            Phase::Ended { at, .. } if now - at >= ENDED_LINGER => {
                self.session = None;
                Vec::new()
            }
            Phase::Waiting | Phase::Ringing | Phase::Requesting
                if s.direction == Direction::Outgoing
                    && now - s.ring_started
                        >= i64::try_from(self.config.ring_timeout_ms / 1000).unwrap_or(90) =>
            {
                let cmds = self.hang_up(now);
                if let Some(s) = self.session.as_mut() {
                    s.phase = Phase::Ended {
                        reason: EndReason::Missed,
                        at: now,
                    };
                }
                cmds
            }
            _ => Vec::new(),
        }
    }

    /// `updatePhoneCall` (and the `phone.phoneCall` RPC results).
    pub fn on_phone_call(&mut self, pc: &PhoneCall, me: i64, now: i64) -> Vec<Cmd> {
        // A new incoming call.
        if let PhoneCall::Requested {
            call,
            admin_id,
            participant_id,
            g_a_hash,
            ..
        } = pc
        {
            if *participant_id != me {
                return Vec::new();
            }
            if self.session.as_ref().is_some_and(|s| s.call == Some(*call)) {
                return Vec::new();
            }
            if self.is_busy() {
                return vec![Cmd::Reject {
                    call: *call,
                    reason: DiscardReason::Busy,
                }];
            }
            let mut s = CallSession::new(*admin_id, Direction::Incoming, Phase::Ringing, now);
            s.call = Some(*call);
            s.g_a_hash = Some(g_a_hash.clone());
            self.session = Some(s);
            return vec![Cmd::Received {
                call: *call,
                peer: *admin_id,
            }];
        }
        let Some(s) = self.session.as_mut() else {
            return Vec::new();
        };
        // Adopt the id of our outgoing call if the update beats the RPC reply.
        let ours = match s.call {
            Some(c) => c.id == pc.id(),
            None => {
                s.direction == Direction::Outgoing
                    && matches!(pc, PhoneCall::Waiting { participant_id, admin_id, .. }
                        | PhoneCall::Accepted { participant_id, admin_id, .. }
                        if *participant_id == s.peer && *admin_id == me)
            }
        };
        if !ours || s.is_ended() {
            return Vec::new();
        }
        match pc {
            PhoneCall::Waiting {
                call, receive_date, ..
            } => {
                s.call = Some(*call);
                match s.direction {
                    Direction::Outgoing => {
                        if matches!(s.phase, Phase::Requesting | Phase::Waiting) {
                            let next = if receive_date.is_some() {
                                Phase::Ringing
                            } else {
                                Phase::Waiting
                            };
                            if next != s.phase {
                                s.ring_started = now;
                            }
                            s.phase = next;
                        }
                    }
                    Direction::Incoming => s.phase = Phase::ExchangingKeys,
                }
                Vec::new()
            }
            PhoneCall::Accepted { call, g_b, .. } if s.direction == Direction::Outgoing => {
                s.call = Some(*call);
                if s.key.is_some() || s.phase == Phase::ExchangingKeys {
                    return Vec::new();
                }
                s.phase = Phase::ExchangingKeys;
                if s.dh.is_none() {
                    s.pending_g_b = Some(g_b.clone());
                    return Vec::new();
                }
                vec![Cmd::Confirm {
                    call: *call,
                    g_b: g_b.clone(),
                }]
            }
            PhoneCall::Active {
                g_a_or_b,
                key_fingerprint,
                ..
            } => {
                let first = s.active.is_none();
                s.active = Some(pc.clone());
                if !first {
                    return Vec::new();
                }
                match s.direction {
                    Direction::Outgoing => {
                        if s.key.is_some() {
                            s.phase = Phase::Connecting;
                            vec![Cmd::StartEngine]
                        } else {
                            s.phase = Phase::ExchangingKeys;
                            Vec::new()
                        }
                    }
                    Direction::Incoming => {
                        s.phase = Phase::ExchangingKeys;
                        if s.dh.is_none() {
                            s.pending_finish = Some((g_a_or_b.clone(), *key_fingerprint));
                            return Vec::new();
                        }
                        vec![Cmd::Finish {
                            g_a: g_a_or_b.clone(),
                            fingerprint: *key_fingerprint,
                        }]
                    }
                }
            }
            PhoneCall::Discarded { reason, .. } => {
                let reason = match (s.direction, reason, &s.phase) {
                    (
                        Direction::Outgoing,
                        Some(DiscardReason::Busy),
                        Phase::Requesting | Phase::Waiting | Phase::Ringing,
                    ) => EndReason::Declined,
                    (_, Some(DiscardReason::Missed), _) => EndReason::Missed,
                    (_, Some(DiscardReason::Busy), _) => EndReason::Busy,
                    (
                        _,
                        Some(DiscardReason::Disconnect),
                        Phase::Active { .. } | Phase::Reconnecting { .. },
                    ) => EndReason::Failed("disconnected".into()),
                    _ => EndReason::HungUp,
                };
                s.phase = Phase::Ended { reason, at: now };
                vec![Cmd::StopEngine]
            }
            _ => Vec::new(),
        }
    }

    /// Buffer signaling that arrives before the engine runs.
    pub fn early_signaling(&mut self, call_id: i64, data: Vec<u8>) {
        if let Some(s) = self.session.as_mut()
            && s.call.is_some_and(|c| c.id == call_id)
            && s.early_signaling.len() < MAX_EARLY_SIGNALING
        {
            s.early_signaling.push(data);
        }
    }
}

/// A `phone.phoneCall` came back for a session that no longer exists (the
/// user hung up meanwhile): end it on the server too.
fn discard_orphan(call: &PhoneCall) -> Vec<Cmd> {
    match call {
        PhoneCall::Waiting { call, .. } | PhoneCall::Active { call, .. } => vec![Cmd::Discard {
            call: *call,
            duration: 0,
            reason: DiscardReason::Hangup,
        }],
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Async steps (run by the app as tasks; integration tests call them too)

fn dh_err(e: dh::DhError) -> Error {
    Error::Protocol(format!("call key exchange: {e}"))
}

/// Check the DH config and make our half (CPU heavy: off the UI thread).
pub async fn make_half(config: DhConfig) -> Result<DhHalf> {
    tokio::task::spawn_blocking(move || {
        dh::check_config(config.g, &config.p).map_err(dh_err)?;
        // A secret whose public value is out of range is simply redrawn.
        for _ in 0..4 {
            let secret = dh::new_secret(&config.random).map_err(dh_err)?;
            if let Ok(public) = dh::public_value(config.g, &secret, &config.p) {
                return Ok(DhHalf {
                    config,
                    secret,
                    public,
                });
            }
        }
        Err(Error::Protocol(
            "call key exchange: no usable secret".into(),
        ))
    })
    .await
    .map_err(|_| Error::Cancelled)?
}

/// Outgoing: DH half + `phone.requestCall`.
pub async fn place(api: ApiHandle, peer: ChatId) -> Result<(DhHalf, PhoneCall)> {
    let config = api.dh_config().await?;
    let half = make_half(config).await?;
    let hash = dh::g_a_hash(&half.public).to_vec();
    let call = api.request_call(peer, hash).await?;
    Ok((half, call))
}

/// Incoming: DH half + `phone.acceptCall`.
pub async fn accept(api: ApiHandle, call: CallRef) -> Result<(DhHalf, PhoneCall)> {
    let config = api.dh_config().await?;
    let half = make_half(config).await?;
    let pc = api.accept_call(call, half.public.clone()).await?;
    Ok((half, pc))
}

/// Outgoing: key from `g_b`, then `phone.confirmCall`.
pub async fn confirm(
    api: ApiHandle,
    call: CallRef,
    half: DhHalf,
    g_b: Vec<u8>,
) -> Result<(CallKey, [&'static str; 4], PhoneCall)> {
    let g_a = half.public.clone();
    let (key, fp, emoji) = tokio::task::spawn_blocking(move || {
        let key = dh::shared_key(&g_b, &half.secret, &half.config.p).map_err(dh_err)?;
        let fp = dh::key_fingerprint(&key);
        let emoji = dh::emoji_fingerprint(&key, &half.public);
        Ok::<_, Error>((key, fp, emoji))
    })
    .await
    .map_err(|_| Error::Cancelled)??;
    let pc = api.confirm_call(call, g_a, fp).await?;
    Ok((key, emoji, pc))
}

/// Incoming: verify the caller's `g_a` against its hash, derive the key and
/// compare fingerprints.
pub async fn finish(
    half: DhHalf,
    g_a_hash: Vec<u8>,
    g_a: Vec<u8>,
    fingerprint: i64,
) -> Result<(CallKey, [&'static str; 4])> {
    tokio::task::spawn_blocking(move || {
        dh::check_g_a_hash(&g_a, &g_a_hash).map_err(dh_err)?;
        let key = dh::shared_key(&g_a, &half.secret, &half.config.p).map_err(dh_err)?;
        if dh::key_fingerprint(&key) != fingerprint {
            return Err(dh_err(dh::DhError::FingerprintMismatch));
        }
        let emoji = dh::emoji_fingerprint(&key, &g_a);
        Ok((key, emoji))
    })
    .await
    .map_err(|_| Error::Cancelled)?
}

/// Engine settings for an established call.
pub fn engine_config(
    session: &CallSession,
    config: &CallConfig,
    loopback: bool,
) -> std::result::Result<EngineConfig, String> {
    let Some(PhoneCall::Active {
        protocol,
        connections,
        p2p_allowed,
        custom_parameters,
        ..
    }) = &session.active
    else {
        return Err("call not established".into());
    };
    if protocol.chosen_supported().is_none() {
        return Err("the other app uses an incompatible call version".into());
    }
    let key = session.key.clone().ok_or("no key")?;
    if calls::custom_flag(custom_parameters.as_deref(), "network_use_mtproto") {
        tracing::warn!("server asked for the MTProto media transport, which is not supported");
    }
    Ok(EngineConfig {
        key,
        outgoing: session.direction == Direction::Outgoing,
        reflectors: calls::reflectors(connections),
        standalone_reflectors: calls::custom_flag(
            custom_parameters.as_deref(),
            "network_standalone_reflectors",
        ),
        p2p: *p2p_allowed || loopback,
        loopback,
        connect_timeout: Duration::from_millis(config.connect_timeout_ms),
        packet_timeout: Duration::from_millis(config.packet_timeout_ms),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::calls::Protocol;

    const ME: i64 = 1;
    const PEER: i64 = 42;

    fn cref(id: i64) -> CallRef {
        CallRef { id, access_hash: 7 }
    }

    fn half() -> DhHalf {
        DhHalf {
            config: DhConfig {
                g: 2,
                p: vec![0xff; 256],
                version: 1,
                random: vec![0; 256],
            },
            secret: dh::secret_from_bytes(&[1; 256]),
            public: vec![2; 256],
        }
    }

    fn waiting(id: i64, rang: bool) -> PhoneCall {
        PhoneCall::Waiting {
            call: cref(id),
            admin_id: ME,
            participant_id: PEER,
            video: false,
            receive_date: rang.then_some(5),
        }
    }

    fn active(id: i64, admin: i64, participant: i64) -> PhoneCall {
        PhoneCall::Active {
            call: cref(id),
            admin_id: admin,
            participant_id: participant,
            g_a_or_b: vec![3; 256],
            key_fingerprint: 99,
            protocol: Protocol::ours(),
            connections: Vec::new(),
            p2p_allowed: true,
            start_date: 0,
            custom_parameters: None,
        }
    }

    fn discarded(id: i64, reason: DiscardReason) -> PhoneCall {
        PhoneCall::Discarded {
            id,
            reason: Some(reason),
            duration: None,
            need_rating: false,
        }
    }

    fn phase(c: &Calls) -> Phase {
        c.session.as_ref().unwrap().phase.clone()
    }

    #[test]
    fn outgoing_happy_path() {
        let mut c = Calls::default();
        assert_eq!(c.start(PEER, 0), vec![Cmd::Request { peer: PEER }]);
        assert!(c.start(PEER, 0).is_empty(), "one call at a time");
        assert_eq!(c.placed(half(), &waiting(10, false), ME, 1), vec![]);
        assert_eq!(phase(&c), Phase::Waiting);
        c.on_phone_call(&waiting(10, true), ME, 2);
        assert_eq!(phase(&c), Phase::Ringing);
        let cmds = c.on_phone_call(
            &PhoneCall::Accepted {
                call: cref(10),
                admin_id: ME,
                participant_id: PEER,
                g_b: vec![4; 256],
                protocol: Protocol::ours(),
            },
            ME,
            3,
        );
        assert_eq!(
            cmds,
            vec![Cmd::Confirm {
                call: cref(10),
                g_b: vec![4; 256]
            }]
        );
        assert_eq!(phase(&c), Phase::ExchangingKeys);
        assert!(c.key_ready(CallKey([1; 256]), ["a"; 4]).is_empty());
        assert_eq!(
            c.on_phone_call(&active(10, ME, PEER), ME, 4),
            vec![Cmd::StartEngine]
        );
        assert_eq!(phase(&c), Phase::Connecting);
        c.link(LinkState::Connected { relay: true }, 5);
        assert_eq!(
            phase(&c),
            Phase::Active {
                since: 5,
                relay: true
            }
        );
        c.link(LinkState::Reconnecting, 9);
        assert_eq!(phase(&c), Phase::Reconnecting { since: 5 });
        c.link(LinkState::Connected { relay: false }, 10);
        assert_eq!(c.session.as_ref().unwrap().status_text(70), "01:05");
        let cmds = c.hang_up(70);
        assert_eq!(
            cmds,
            vec![
                Cmd::StopEngine,
                Cmd::Discard {
                    call: cref(10),
                    duration: 65,
                    reason: DiscardReason::Hangup
                }
            ]
        );
        assert!(!c.is_busy());
        c.tick(70 + ENDED_LINGER);
        assert!(c.session.is_none());
    }

    #[test]
    fn incoming_happy_path_and_busy() {
        let mut c = Calls::default();
        let req = PhoneCall::Requested {
            call: cref(20),
            admin_id: PEER,
            participant_id: ME,
            video: false,
            g_a_hash: vec![9; 32],
            protocol: Protocol::ours(),
        };
        assert_eq!(
            c.on_phone_call(&req, ME, 0),
            vec![Cmd::Received {
                call: cref(20),
                peer: PEER
            }]
        );
        assert!(c.is_ringing_in());
        assert!(c.on_phone_call(&req, ME, 0).is_empty(), "duplicate update");
        // A second caller gets "busy".
        let other = PhoneCall::Requested {
            call: cref(21),
            admin_id: 77,
            participant_id: ME,
            video: false,
            g_a_hash: vec![],
            protocol: Protocol::ours(),
        };
        assert_eq!(
            c.on_phone_call(&other, ME, 0),
            vec![Cmd::Reject {
                call: cref(21),
                reason: DiscardReason::Busy
            }]
        );
        assert_eq!(c.accept(), vec![Cmd::Accept { call: cref(20) }]);
        let w = PhoneCall::Waiting {
            call: cref(20),
            admin_id: PEER,
            participant_id: ME,
            video: false,
            receive_date: Some(1),
        };
        assert!(c.accepted(half(), &w, ME, 1).is_empty());
        assert_eq!(phase(&c), Phase::ExchangingKeys);
        assert_eq!(
            c.on_phone_call(&active(20, PEER, ME), ME, 2),
            vec![Cmd::Finish {
                g_a: vec![3; 256],
                fingerprint: 99
            }]
        );
        assert_eq!(
            c.key_ready(CallKey([1; 256]), ["b"; 4]),
            vec![Cmd::StartEngine]
        );
        assert_eq!(phase(&c), Phase::Connecting);
    }

    #[test]
    fn decline_and_missed_and_remote_busy() {
        // Declining an incoming call sends Busy (as the official apps do).
        let mut c = Calls::default();
        let req = PhoneCall::Requested {
            call: cref(30),
            admin_id: PEER,
            participant_id: ME,
            video: false,
            g_a_hash: vec![],
            protocol: Protocol::ours(),
        };
        c.on_phone_call(&req, ME, 0);
        assert_eq!(
            c.hang_up(3),
            vec![
                Cmd::StopEngine,
                Cmd::Discard {
                    call: cref(30),
                    duration: 0,
                    reason: DiscardReason::Busy
                }
            ]
        );
        // Caller gave up: Missed on our side.
        let mut c = Calls::default();
        c.on_phone_call(&req, ME, 0);
        c.on_phone_call(&discarded(30, DiscardReason::Missed), ME, 5);
        assert_eq!(
            phase(&c),
            Phase::Ended {
                reason: EndReason::Missed,
                at: 5
            }
        );
        // Outgoing, peer declined.
        let mut c = Calls::default();
        c.start(PEER, 0);
        c.placed(half(), &waiting(31, true), ME, 0);
        c.on_phone_call(&discarded(31, DiscardReason::Busy), ME, 2);
        assert_eq!(
            phase(&c),
            Phase::Ended {
                reason: EndReason::Declined,
                at: 2
            }
        );
        // Outgoing cancelled by us while ringing: Missed.
        let mut c = Calls::default();
        c.start(PEER, 0);
        c.placed(half(), &waiting(32, true), ME, 0);
        assert_eq!(
            c.hang_up(4)[1],
            Cmd::Discard {
                call: cref(32),
                duration: 0,
                reason: DiscardReason::Missed
            }
        );
    }

    #[test]
    fn ring_timeout_and_orphans() {
        let mut c = Calls {
            config: CallConfig {
                ring_timeout_ms: 30_000,
                ..Default::default()
            },
            ..Default::default()
        };
        c.start(PEER, 0);
        c.placed(half(), &waiting(40, true), ME, 0);
        assert!(c.tick(10).is_empty());
        let cmds = c.tick(31);
        assert!(cmds.contains(&Cmd::Discard {
            call: cref(40),
            duration: 0,
            reason: DiscardReason::Missed
        }));
        assert_eq!(
            phase(&c),
            Phase::Ended {
                reason: EndReason::Missed,
                at: 31
            }
        );
        // RPC reply for a call the user already hung up.
        let mut c = Calls::default();
        assert_eq!(
            c.placed(half(), &waiting(41, false), ME, 0),
            vec![Cmd::Discard {
                call: cref(41),
                duration: 0,
                reason: DiscardReason::Hangup
            }]
        );
        // Updates for other calls are ignored; the id is adopted early.
        let mut c = Calls::default();
        c.start(PEER, 0);
        assert!(c.on_phone_call(&active(50, ME, 999), ME, 0).is_empty());
        c.on_phone_call(&waiting(51, true), ME, 0);
        assert_eq!(c.session.as_ref().unwrap().call, Some(cref(51)));
        c.early_signaling(51, vec![1]);
        c.early_signaling(52, vec![1]);
        assert_eq!(c.session.as_ref().unwrap().early_signaling.len(), 1);
    }

    #[test]
    fn updates_that_beat_the_rpc_reply() {
        let mut c = Calls::default();
        c.start(PEER, 0);
        let acc = PhoneCall::Accepted {
            call: cref(70),
            admin_id: ME,
            participant_id: PEER,
            g_b: vec![4; 256],
            protocol: Protocol::ours(),
        };
        assert!(c.on_phone_call(&acc, ME, 0).is_empty(), "no DH half yet");
        assert_eq!(
            c.placed(half(), &waiting(70, true), ME, 1),
            vec![Cmd::Confirm {
                call: cref(70),
                g_b: vec![4; 256]
            }]
        );
        let mut c = Calls::default();
        let req = PhoneCall::Requested {
            call: cref(71),
            admin_id: PEER,
            participant_id: ME,
            video: false,
            g_a_hash: vec![],
            protocol: Protocol::ours(),
        };
        c.on_phone_call(&req, ME, 0);
        c.accept();
        assert!(c.on_phone_call(&active(71, PEER, ME), ME, 1).is_empty());
        let w = PhoneCall::Waiting {
            call: cref(71),
            admin_id: PEER,
            participant_id: ME,
            video: false,
            receive_date: None,
        };
        assert_eq!(
            c.accepted(half(), &w, ME, 2),
            vec![Cmd::Finish {
                g_a: vec![3; 256],
                fingerprint: 99
            }]
        );
    }

    #[test]
    fn failure_discards_with_disconnect() {
        let mut c = Calls::default();
        c.start(PEER, 0);
        c.placed(half(), &waiting(60, true), ME, 0);
        let cmds = c.failed("boom", 1);
        assert_eq!(
            cmds,
            vec![
                Cmd::StopEngine,
                Cmd::Discard {
                    call: cref(60),
                    duration: 0,
                    reason: DiscardReason::Disconnect
                }
            ]
        );
        assert!(c.failed("again", 2).is_empty());
        assert_eq!(c.session.as_ref().unwrap().status_text(2), "Call failed");
    }

    #[test]
    fn engine_config_checks_version() {
        let mut c = Calls::default();
        c.start(PEER, 0);
        let mut s = c.session.clone().unwrap();
        assert!(engine_config(&s, &CallConfig::default(), false).is_err());
        s.key = Some(CallKey([0; 256]));
        s.active = Some(active(1, ME, PEER));
        let cfg = engine_config(&s, &CallConfig::default(), false).unwrap();
        assert!(cfg.outgoing);
        assert_eq!(cfg.connect_timeout, Duration::from_secs(30));
        if let Some(PhoneCall::Active { protocol, .. }) = s.active.as_mut() {
            protocol.library_versions = vec!["13.0.0".into()];
        }
        assert!(
            engine_config(&s, &CallConfig::default(), false)
                .unwrap_err()
                .contains("incompatible")
        );
        assert_eq!(format_duration(3725), "1:02:05");
    }
}
