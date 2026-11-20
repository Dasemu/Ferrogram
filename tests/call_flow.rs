//! Full call signaling against the fake backend: the app-side reducer
//! (`app::call`), the same async steps the app runs (DH, `phone.*`) and the
//! real engine, talking to `FakeTelegram`'s call server whose remote peer
//! runs its own engine on loopback. Covers outgoing, incoming, remote
//! hang-up, decline and ring timeout.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use ferrogram::app::call::{self, Calls, Cmd, Direction, EndReason, Phase};
use ferrogram::mock::{self, ALICE, FakeOptions, FakeTelegram};
use ferrogram::mock_call::PeerBehaviour;
use ferrogram::net::calls::audio::Shared;
use ferrogram::net::calls::engine::{self, EngineEvent, EngineHandle};
use ferrogram::net::calls::{CallConfig, DiscardReason};
use ferrogram::net::{ApiHandle, Event};
use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::sync::mpsc;

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

struct Driver {
    fake: Arc<FakeTelegram>,
    api: ApiHandle,
    events: BoxStream<'static, Event>,
    calls: Calls,
    me: i64,
    engine: Option<(EngineHandle, Arc<Shared>, mpsc::UnboundedReceiver<EngineEvent>)>,
    frames: Option<mpsc::Sender<Vec<u8>>>,
}

impl Driver {
    fn new(behaviour: PeerBehaviour) -> Self {
        let fake = FakeTelegram::new(FakeOptions {
            logged_in: true,
            live_simulation: false,
            ..Default::default()
        });
        *fake.phone().behaviour.lock().unwrap() = behaviour;
        let api = ApiHandle(fake.clone());
        let events = api.events();
        Self {
            fake,
            api,
            events,
            calls: Calls::default(),
            me: mock::me().id,
            engine: None,
            frames: None,
        }
    }

    /// Run commands exactly as `app/phone.rs` does (minus iced).
    async fn run(&mut self, cmds: Vec<Cmd>) {
        let mut queue: std::collections::VecDeque<Cmd> = cmds.into();
        while let Some(cmd) = queue.pop_front() {
            let api = self.api.clone();
            let more = match cmd {
                Cmd::Request { peer } => {
                    let (half, pc) = call::place(api, peer).await.unwrap();
                    self.calls.placed(half, &pc, self.me, now())
                }
                Cmd::Received { call, .. } => {
                    self.api.received_call(call).await.unwrap();
                    Vec::new()
                }
                Cmd::Reject { call, reason } => {
                    self.api.discard_call(call, 0, reason).await.unwrap();
                    Vec::new()
                }
                Cmd::Discard { call, duration, reason } => {
                    self.api.discard_call(call, duration, reason).await.unwrap();
                    Vec::new()
                }
                Cmd::Confirm { call, g_b } => {
                    let half = self.calls.session.as_ref().unwrap().dh.clone().unwrap();
                    let (key, emoji, pc) = call::confirm(api, call, half, g_b).await.unwrap();
                    let mut c = self.calls.key_ready(key, emoji);
                    c.extend(self.calls.on_phone_call(&pc, self.me, now()));
                    c
                }
                Cmd::Accept { call } => {
                    let (half, pc) = call::accept(api, call).await.unwrap();
                    self.calls.accepted(half, &pc, self.me, now())
                }
                Cmd::Finish { g_a, fingerprint } => {
                    let s = self.calls.session.as_mut().unwrap();
                    s.g_a = Some(g_a.clone());
                    let (half, hash) = (s.dh.clone().unwrap(), s.g_a_hash.clone().unwrap());
                    let (key, emoji) = call::finish(half, hash, g_a, fingerprint).await.unwrap();
                    self.calls.key_ready(key, emoji)
                }
                Cmd::StartEngine => {
                    let s = self.calls.session.as_mut().unwrap();
                    let cfg = call::engine_config(s, &CallConfig::default(), true).unwrap();
                    let shared = Shared::new();
                    let (tx, rx) = mpsc::channel(64);
                    let e = engine::start(cfg, Arc::clone(&shared), rx);
                    for d in s.early_signaling.drain(..) {
                        e.signaling(d);
                    }
                    let ev = e.take_events().unwrap();
                    // Our "microphone": the reducer does not care, the peer
                    // checks that it hears something.
                    let ftx = tx.clone();
                    tokio::spawn(async move {
                        let mut tick = tokio::time::interval(Duration::from_millis(20));
                        let mut enc = ferrogram::net::calls::audio::OpusEncoder::new().unwrap();
                        let mut t = 0f32;
                        loop {
                            tick.tick().await;
                            let pcm: Vec<i16> = (0..960)
                                .map(|_| {
                                    t += 1.0;
                                    ((t * 660.0 / 48_000.0 * std::f32::consts::TAU).sin() * 9000.0) as i16
                                })
                                .collect();
                            if ftx.send(enc.encode(&pcm).unwrap()).await.is_err() {
                                return;
                            }
                        }
                    });
                    self.frames = Some(tx);
                    self.engine = Some((e, shared, ev));
                    Vec::new()
                }
                Cmd::StopEngine => {
                    if let Some((e, _, _)) = self.engine.take() {
                        e.stop();
                    }
                    self.frames = None;
                    Vec::new()
                }
            };
            queue.extend(more);
        }
    }

    /// Pump server events and engine events until `done` or timeout.
    async fn pump_until(&mut self, what: &str, done: impl Fn(&Calls) -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
        while !done(&self.calls) {
            assert!(tokio::time::Instant::now() < deadline, "timed out waiting for {what}: {:?}", self.calls.session.as_ref().map(|s| &s.phase));
            let engine_ev = async {
                match self.engine.as_mut() {
                    Some((_, _, rx)) => rx.recv().await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                Some(ev) = self.events.next() => {
                    let cmds = match &ev {
                        Event::PhoneCall(pc) => self.calls.on_phone_call(pc, self.me, now()),
                        Event::CallSignaling { call_id, data } => {
                            match &self.engine {
                                Some((e, _, _)) => e.signaling(data.clone()),
                                None => self.calls.early_signaling(*call_id, data.clone()),
                            }
                            Vec::new()
                        }
                        _ => Vec::new(),
                    };
                    self.run(cmds).await;
                }
                Some(ev) = engine_ev => {
                    match ev {
                        EngineEvent::Signaling(d) => {
                            let call = self.calls.session.as_ref().unwrap().call.unwrap();
                            self.api.send_call_signaling(call, d).await.unwrap();
                        }
                        EngineEvent::State(s) => self.calls.link(s, now()),
                        EngineEvent::Quality(q) => self.calls.session.as_mut().unwrap().quality = q,
                        EngineEvent::Failed(f) => panic!("engine failed: {f}"),
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    let cmds = self.calls.tick(now());
                    self.run(cmds).await;
                }
            }
        }
    }

    fn phase(&self) -> Phase {
        self.calls.session.as_ref().unwrap().phase.clone()
    }

    /// Audio both ways for a moment.
    async fn check_audio(&mut self) {
        let until = std::time::Instant::now() + Duration::from_millis(1500);
        self.pump_until("audio", move |_| std::time::Instant::now() >= until).await;
        let ours = self.engine.as_ref().unwrap().1.jitter.lock().unwrap().stats.received;
        assert!(ours > 20, "we hear the peer: {ours}");
        assert!(self.fake.phone().peer_received() > 20, "the peer hears us");
    }
}

fn fast() -> PeerBehaviour {
    PeerBehaviour {
        ring_after: Duration::from_millis(20),
        answer_after: Duration::from_millis(50),
        answers: true,
    }
}

fn is_active(c: &Calls) -> bool {
    matches!(c.session.as_ref().map(|s| &s.phase), Some(Phase::Active { .. }))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outgoing_call_connects_and_hangs_up() {
    let mut d = Driver::new(fast());
    let cmds = d.calls.start(ALICE, now());
    d.run(cmds).await;
    d.pump_until("ringing", |c| {
        matches!(c.session.as_ref().map(|s| &s.phase), Some(Phase::Ringing | Phase::ExchangingKeys | Phase::Connecting | Phase::Active { .. }))
    })
    .await;
    d.pump_until("active", is_active).await;
    let s = d.calls.session.as_ref().unwrap();
    assert_eq!(s.direction, Direction::Outgoing);
    assert_eq!(s.emoji.unwrap().len(), 4);
    assert!(matches!(d.phase(), Phase::Active { relay: false, .. }));
    d.check_audio().await;
    let cmds = d.calls.hang_up(now());
    assert!(cmds.iter().any(|c| matches!(c, Cmd::Discard { reason: DiscardReason::Hangup, .. })));
    d.run(cmds).await;
    assert!(d.engine.is_none());
    let api_calls = d.fake.calls();
    for name in ["dh_config", "request_call", "confirm_call", "discard_call"] {
        assert!(api_calls.iter().any(|c| c == name), "{name} in {api_calls:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incoming_call_is_accepted_and_peer_hangs_up() {
    let mut d = Driver::new(fast());
    d.fake.phone().ring(ALICE).await.unwrap();
    d.pump_until("ringing", |c| c.is_ringing_in()).await;
    assert!(d.fake.calls().iter().any(|c| c == "received_call"));
    let cmds = d.calls.accept();
    d.run(cmds).await;
    d.pump_until("active", is_active).await;
    let s = d.calls.session.as_ref().unwrap();
    assert_eq!(s.direction, Direction::Incoming);
    assert_eq!(s.peer, ALICE);
    d.check_audio().await;
    d.fake.phone().peer_hang_up();
    d.pump_until("ended", |c| c.session.as_ref().is_some_and(|s| s.is_ended())).await;
    assert!(matches!(d.phase(), Phase::Ended { reason: EndReason::HungUp, .. }));
    assert!(d.engine.is_none(), "engine stopped");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn declined_incoming_and_unanswered_outgoing() {
    // Declining sends Busy.
    let mut d = Driver::new(fast());
    d.fake.phone().ring(ALICE).await.unwrap();
    d.pump_until("ringing", |c| c.is_ringing_in()).await;
    let cmds = d.calls.hang_up(now());
    assert!(cmds.iter().any(|c| matches!(c, Cmd::Discard { reason: DiscardReason::Busy, .. })));
    d.run(cmds).await;

    // Nobody answers: the ring timeout cancels with Missed.
    let mut d = Driver::new(PeerBehaviour {
        answers: false,
        ..fast()
    });
    d.calls.config.ring_timeout_ms = 2000;
    let cmds = d.calls.start(ALICE, now());
    d.run(cmds).await;
    d.pump_until("missed", |c| c.session.as_ref().is_some_and(|s| s.is_ended())).await;
    assert!(matches!(d.phase(), Phase::Ended { reason: EndReason::Missed, .. }));
    assert!(d.fake.calls().iter().any(|c| c == "discard_call"));
}
