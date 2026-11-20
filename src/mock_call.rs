//! The fake Telegram call server for `--mock` and tests: it plays the server
//! (call ids, `updatePhoneCall`, signaling relay) and the remote user, whose
//! side runs the real call engine on loopback with a test tone. So a mock
//! call really exchanges keys, connects and carries audio.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rand::RngCore;
use tokio::sync::mpsc;
use tracing::debug;

use crate::app::call::{self, DhHalf};
use crate::error::{Error, Result};
use crate::model::ChatId;
use crate::net::calls::audio::{CapturePipeline, Shared};
use crate::net::calls::dh::{self, CallKey, DhConfig};
use crate::net::calls::engine::{self, EngineConfig, EngineEvent, EngineHandle};
use crate::net::calls::{CallRef, DiscardReason, PhoneCall, Protocol};
use crate::net::{Event, EventHub};

/// RFC 3526 group 14: a well-known 2048-bit safe prime (g = 2 is valid).
const P_HEX: &str = "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74020BBEA63B139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7EDEE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3DC2007CB8A163BF0598DA48361C55D39A69163FA8FD24CF5F83655D23DCA3AD961C62F356208552BB9ED529077096966D670C354E4ABC9804F1746C08CA18217C32905E462E36CE3BE39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9DE2BCBF6955817183995497CEA956AE515D2261898FA051015728E5A8AACAA68FFFFFFFFFFFFFFFF";

pub fn dh_config() -> DhConfig {
    let p = (0..P_HEX.len())
        .step_by(2)
        .filter_map(|i| u8::from_str_radix(&P_HEX[i..i + 2], 16).ok())
        .collect();
    let mut random = vec![0u8; 256];
    rand::rng().fill_bytes(&mut random);
    DhConfig {
        g: 2,
        p,
        version: 1,
        random,
    }
}

/// How the fake peer behaves.
#[derive(Debug, Clone, Copy)]
pub struct PeerBehaviour {
    /// Delay before the peer's device rings, then before it answers.
    pub ring_after: Duration,
    pub answer_after: Duration,
    /// Never answer (to test cancelling and ring timeouts).
    pub answers: bool,
}

impl Default for PeerBehaviour {
    fn default() -> Self {
        Self {
            ring_after: Duration::from_millis(300),
            answer_after: Duration::from_millis(1500),
            answers: true,
        }
    }
}

struct Peer {
    shared: Arc<Shared>,
    engine: EngineHandle,
    tone: tokio::task::JoinHandle<()>,
}

struct Live {
    call: CallRef,
    /// The app user placed the call.
    app_is_caller: bool,
    peer_user: ChatId,
    /// The peer's DH half.
    half: Option<DhHalf>,
    /// Caller's `g_a_hash` (app is caller) / the peer's `g_a` (peer calls).
    g_a_hash: Vec<u8>,
    peer_g_a: Vec<u8>,
    peer: Option<Peer>,
    early: Vec<Vec<u8>>,
}

pub struct FakeCalls {
    me: i64,
    events: Arc<EventHub>,
    live: Arc<Mutex<Option<Live>>>,
    pub behaviour: Mutex<PeerBehaviour>,
}

fn lock<T>(m: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>> {
    m.lock().map_err(|_| Error::Net("fake call poisoned".into()))
}

fn active(live: &Live, g_a_or_b: Vec<u8>, key_fingerprint: i64, me: i64) -> PhoneCall {
    let (admin_id, participant_id) = if live.app_is_caller {
        (me, live.peer_user)
    } else {
        (live.peer_user, me)
    };
    PhoneCall::Active {
        call: live.call,
        admin_id,
        participant_id,
        g_a_or_b,
        key_fingerprint,
        protocol: Protocol {
            library_versions: vec!["9.0.0".into()],
            ..Protocol::ours()
        },
        connections: Vec::new(),
        p2p_allowed: true,
        start_date: 0,
        custom_parameters: None,
    }
}

/// A 440 Hz tone into the peer's engine (what the app user hears).
fn tone(frames: mpsc::Sender<Vec<u8>>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let shared = Shared::new();
        let Some(mut cap) = CapturePipeline::new(48_000, 1) else {
            return;
        };
        let mut phase = 0f32;
        let mut tick = tokio::time::interval(Duration::from_millis(20));
        loop {
            tick.tick().await;
            let pcm: Vec<f32> = (0..960)
                .map(|_| {
                    let v = (phase * std::f32::consts::TAU).sin() * 0.2;
                    phase = (phase + 440.0 / 48_000.0).fract();
                    v
                })
                .collect();
            let mut out = Vec::new();
            cap.push(&pcm, false, &shared, |p| out.push(p));
            for p in out {
                if frames.send(p).await.is_err() {
                    return;
                }
            }
        }
    })
}

impl FakeCalls {
    pub fn new(me: i64, events: Arc<EventHub>) -> Self {
        Self {
            me,
            events,
            live: Arc::new(Mutex::new(None)),
            behaviour: Mutex::new(PeerBehaviour::default()),
        }
    }

    /// What the peer's side heard so far (received packets), for tests.
    pub fn peer_received(&self) -> u64 {
        lock(&self.live)
            .ok()
            .and_then(|l| {
                l.as_ref()?
                    .peer
                    .as_ref()?
                    .shared
                    .jitter
                    .lock()
                    .ok()
                    .map(|j| j.stats.received)
            })
            .unwrap_or(0)
    }

    fn new_ref() -> CallRef {
        let mut b = [0u8; 16];
        rand::rng().fill_bytes(&mut b);
        let id = i64::from_le_bytes(b[..8].try_into().unwrap_or([1; 8])).abs().max(1);
        let access_hash = i64::from_le_bytes(b[8..].try_into().unwrap_or([2; 8]));
        CallRef { id, access_hash }
    }

    /// Start the peer's engine (and relay its signaling to the app).
    fn start_peer(live: &Arc<Mutex<Option<Live>>>, events: &Arc<EventHub>, key: CallKey) {
        let Ok(mut g) = live.lock() else { return };
        let Some(l) = g.as_mut() else { return };
        let cfg = EngineConfig {
            key,
            outgoing: !l.app_is_caller,
            reflectors: Vec::new(),
            standalone_reflectors: false,
            p2p: true,
            loopback: true,
            connect_timeout: Duration::from_secs(30),
            packet_timeout: Duration::from_secs(10),
        };
        let shared = Shared::new();
        let (tx, rx) = mpsc::channel(64);
        let engine = engine::start(cfg, Arc::clone(&shared), rx);
        for d in l.early.drain(..) {
            engine.signaling(d);
        }
        let call_id = l.call.id;
        if let Some(mut ev) = engine.take_events() {
            let events = Arc::clone(events);
            tokio::spawn(async move {
                while let Some(e) = ev.recv().await {
                    match e {
                        EngineEvent::Signaling(data) => {
                            events.emit(Event::CallSignaling { call_id, data });
                        }
                        other => debug!(?other, "fake peer engine"),
                    }
                }
            });
        }
        l.peer = Some(Peer {
            shared,
            engine,
            tone: tone(tx),
        });
    }

    // ---- the server side of the app's API calls

    pub async fn request_call(&self, user: ChatId, g_a_hash: Vec<u8>) -> Result<PhoneCall> {
        let call = Self::new_ref();
        *lock(&self.live)? = Some(Live {
            call,
            app_is_caller: true,
            peer_user: user,
            half: None,
            g_a_hash,
            peer_g_a: Vec::new(),
            peer: None,
            early: Vec::new(),
        });
        let behaviour = *lock(&self.behaviour)?;
        let (live, events, me) = (Arc::clone(&self.live), Arc::clone(&self.events), self.me);
        tokio::spawn(async move {
            tokio::time::sleep(behaviour.ring_after).await;
            if !matches!(live.lock().ok().as_deref(), Some(Some(l)) if l.call == call) {
                return;
            }
            events.emit(Event::PhoneCall(PhoneCall::Waiting {
                call,
                admin_id: me,
                participant_id: user,
                video: false,
                receive_date: Some(1),
            }));
            if !behaviour.answers {
                return;
            }
            tokio::time::sleep(behaviour.answer_after).await;
            let Ok(half) = call::make_half(dh_config()).await else {
                return;
            };
            let g_b = half.public.clone();
            {
                let Ok(mut g) = live.lock() else { return };
                match g.as_mut() {
                    Some(l) if l.call == call => l.half = Some(half),
                    _ => return,
                }
            }
            events.emit(Event::PhoneCall(PhoneCall::Accepted {
                call,
                admin_id: me,
                participant_id: user,
                g_b,
                protocol: Protocol::ours(),
            }));
        });
        Ok(PhoneCall::Waiting {
            call,
            admin_id: self.me,
            participant_id: user,
            video: false,
            receive_date: None,
        })
    }

    pub async fn confirm_call(&self, call: CallRef, g_a: Vec<u8>, fp: i64) -> Result<PhoneCall> {
        let (half, hash) = {
            let g = lock(&self.live)?;
            let l = g
                .as_ref()
                .filter(|l| l.call == call)
                .ok_or_else(|| Error::Protocol("400 CALL_PEER_INVALID".into()))?;
            (
                l.half.clone().ok_or_else(|| Error::Protocol("400 CALL_ALREADY_DECLINED".into()))?,
                l.g_a_hash.clone(),
            )
        };
        let g_b = half.public.clone();
        // The peer derives the same key from g_a, exactly as the app would.
        let (key, _) = call::finish(half, hash, g_a, fp).await?;
        let pc = {
            let g = lock(&self.live)?;
            let l = g.as_ref().ok_or(Error::Cancelled)?;
            active(l, g_b, fp, self.me)
        };
        Self::start_peer(&self.live, &self.events, key);
        Ok(pc)
    }

    /// The fake peer calls the app user.
    pub async fn ring(&self, from: ChatId) -> Result<()> {
        let half = call::make_half(dh_config()).await?;
        let call = Self::new_ref();
        let g_a_hash = dh::g_a_hash(&half.public).to_vec();
        *lock(&self.live)? = Some(Live {
            call,
            app_is_caller: false,
            peer_user: from,
            peer_g_a: half.public.clone(),
            half: Some(half),
            g_a_hash: g_a_hash.clone(),
            peer: None,
            early: Vec::new(),
        });
        self.events.emit(Event::PhoneCall(PhoneCall::Requested {
            call,
            admin_id: from,
            participant_id: self.me,
            video: false,
            g_a_hash,
            protocol: Protocol::ours(),
        }));
        Ok(())
    }

    pub async fn accept_call(&self, call: CallRef, g_b: Vec<u8>) -> Result<PhoneCall> {
        let (half, g_a) = {
            let g = lock(&self.live)?;
            let l = g
                .as_ref()
                .filter(|l| l.call == call && !l.app_is_caller)
                .ok_or_else(|| Error::Protocol("400 CALL_PEER_INVALID".into()))?;
            (
                l.half.clone().ok_or(Error::Cancelled)?,
                l.peer_g_a.clone(),
            )
        };
        let key = tokio::task::spawn_blocking(move || dh::shared_key(&g_b, &half.secret, &half.config.p))
            .await
            .map_err(|_| Error::Cancelled)?
            .map_err(|e| Error::Protocol(e.to_string()))?;
        let fp = dh::key_fingerprint(&key);
        let (pc, waiting) = {
            let g = lock(&self.live)?;
            let l = g.as_ref().ok_or(Error::Cancelled)?;
            (
                active(l, g_a, fp, self.me),
                PhoneCall::Waiting {
                    call,
                    admin_id: l.peer_user,
                    participant_id: self.me,
                    video: false,
                    receive_date: Some(1),
                },
            )
        };
        Self::start_peer(&self.live, &self.events, key);
        // The caller confirms; the server then sends the final phoneCall.
        let events = Arc::clone(&self.events);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            events.emit(Event::PhoneCall(pc));
        });
        Ok(waiting)
    }

    pub fn signaling(&self, call: CallRef, data: Vec<u8>) -> Result<()> {
        let mut g = lock(&self.live)?;
        let Some(l) = g.as_mut().filter(|l| l.call == call) else {
            return Err(Error::Protocol("400 CALL_PEER_INVALID".into()));
        };
        match &l.peer {
            Some(p) => p.engine.signaling(data),
            None if l.early.len() < 64 => l.early.push(data),
            None => {}
        }
        Ok(())
    }

    pub fn discard(&self, call: CallRef, duration: i32, reason: DiscardReason) -> Result<()> {
        let live = lock(&self.live)?.take();
        if let Some(l) = live.filter(|l| l.call == call) {
            if let Some(p) = l.peer {
                p.engine.stop();
                p.tone.abort();
            }
            self.events.emit(Event::PhoneCall(PhoneCall::Discarded {
                id: call.id,
                reason: Some(reason),
                duration: Some(duration),
                need_rating: false,
            }));
        }
        Ok(())
    }

    /// The peer hangs up.
    pub fn peer_hang_up(&self) {
        let call = lock(&self.live).ok().and_then(|g| g.as_ref().map(|l| l.call));
        if let Some(call) = call {
            let _ = self.discard(call, 0, DiscardReason::Hangup);
        }
    }
}
