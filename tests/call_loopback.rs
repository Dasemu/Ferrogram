//! Two call engines in one process, talking over loopback UDP: signaling is
//! shuttled between them as `phone.sendSignalingData` would, then ICE, DTLS,
//! SRTP and Opus run for real. A sine tone goes in on each side and the other
//! side must hear it. The second test blocks the direct path and routes
//! everything through an in-process Telegram-style reflector.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use ferrogram::net::calls::audio::{CapturePipeline, PlaybackPipeline, Shared};
use ferrogram::net::calls::dh::CallKey;
use ferrogram::net::calls::engine::{self, EngineConfig, EngineEvent, EngineHandle, LinkState};
use ferrogram::net::calls::ice::ReflectorConfig;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

fn key() -> CallKey {
    CallKey(core::array::from_fn(|i| (i * 13 + 1) as u8))
}

fn config(outgoing: bool, reflectors: Vec<ReflectorConfig>, p2p: bool) -> EngineConfig {
    EngineConfig {
        key: key(),
        outgoing,
        reflectors,
        standalone_reflectors: false,
        p2p,
        loopback: true,
        connect_timeout: Duration::from_secs(20),
        packet_timeout: Duration::from_secs(10),
    }
}

/// Feed a 440/660 Hz tone into `frames` in real time (20 ms frames).
fn tone(freq: f32, frames: mpsc::Sender<Vec<u8>>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let shared = Shared::new();
        let mut cap = CapturePipeline::new(48_000, 1).unwrap();
        let mut phase = 0f32;
        let mut tick = tokio::time::interval(Duration::from_millis(20));
        loop {
            tick.tick().await;
            let pcm: Vec<f32> = (0..960)
                .map(|_| {
                    let v = (phase * std::f32::consts::TAU).sin() * 0.5;
                    phase = (phase + freq / 48_000.0).fract();
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

/// Forward signaling from `from`'s events to `to`, and collect states.
fn pump(
    mut events: mpsc::UnboundedReceiver<EngineEvent>,
    to: Arc<EngineHandle>,
    states: mpsc::UnboundedSender<EngineEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(e) = events.recv().await {
            match e {
                EngineEvent::Signaling(d) => to.signaling(d),
                other => {
                    let _ = states.send(other);
                }
            }
        }
    })
}

/// Energy of what a side would play: drain its jitter buffer through the
/// playback pipeline for `ms` milliseconds.
fn played_rms(shared: &Shared, ms: usize) -> f32 {
    let mut play = PlaybackPipeline::new(48_000, 1).unwrap();
    let mut out = vec![0f32; 48 * ms];
    play.fill(&mut out, shared);
    (out.iter().map(|s| s * s).sum::<f32>() / out.len() as f32).sqrt()
}

async fn wait_connected(states: &mut mpsc::UnboundedReceiver<EngineEvent>) -> LinkState {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(25);
    loop {
        let e = tokio::time::timeout_at(deadline, states.recv())
            .await
            .expect("timed out waiting for media")
            .expect("engine gone");
        match e {
            EngineEvent::State(s @ LinkState::Connected { .. }) => return s,
            EngineEvent::Failed(f) => panic!("engine failed: {f}"),
            _ => {}
        }
    }
}

async fn call(reflectors: Vec<ReflectorConfig>, p2p: bool) -> (LinkState, LinkState) {
    let shared_a = Shared::new();
    let shared_b = Shared::new();
    let (fa_tx, fa_rx) = mpsc::channel(64);
    let (fb_tx, fb_rx) = mpsc::channel(64);
    let a = Arc::new(engine::start(
        config(true, reflectors.clone(), p2p),
        Arc::clone(&shared_a),
        fa_rx,
    ));
    let b = Arc::new(engine::start(
        config(false, reflectors, p2p),
        Arc::clone(&shared_b),
        fb_rx,
    ));
    let (sa_tx, mut sa_rx) = mpsc::unbounded_channel();
    let (sb_tx, mut sb_rx) = mpsc::unbounded_channel();
    let pa = pump(a.take_events().unwrap(), Arc::clone(&b), sa_tx);
    let pb = pump(b.take_events().unwrap(), Arc::clone(&a), sb_tx);
    let ta = tone(440.0, fa_tx);
    let tb = tone(660.0, fb_tx);
    let state_a = wait_connected(&mut sa_rx).await;
    let state_b = wait_connected(&mut sb_rx).await;
    // Let audio flow for a second.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let rms_b = played_rms(&shared_b, 600);
    let rms_a = played_rms(&shared_a, 600);
    assert!(rms_b > 0.1, "callee hears the caller: rms {rms_b}");
    assert!(rms_a > 0.1, "caller hears the callee: rms {rms_a}");
    let jb = shared_b.jitter.lock().unwrap().stats;
    assert!(jb.received > 30, "{jb:?}");
    a.stop();
    b.stop();
    for h in [pa, pb, ta, tb] {
        h.abort();
    }
    (state_a, state_b)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_call_carries_audio_both_ways() {
    let (a, b) = call(Vec::new(), true).await;
    assert_eq!(a, LinkState::Connected { relay: false });
    assert_eq!(b, LinkState::Connected { relay: false });
}

/// A Telegram-style UDP reflector: answers hellos and forwards data packets
/// to the endpoint whose tag is in bytes 12..16.
async fn fake_reflector() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = sock.local_addr().unwrap();
    let h = tokio::spawn(async move {
        let mut peers: HashMap<[u8; 4], SocketAddr> = HashMap::new();
        let mut buf = vec![0u8; 4096];
        loop {
            let Ok((n, from)) = sock.recv_from(&mut buf).await else {
                return;
            };
            let p = &buf[..n];
            if n < 24 {
                continue;
            }
            let own: [u8; 4] = p[12..16].try_into().unwrap();
            if p[16..28].iter().all(|&b| b == 0xff) {
                peers.insert(own, from);
                let _ = sock.send_to(p, from).await;
                continue;
            }
            let sender: [u8; 4] = p[16..20].try_into().unwrap();
            peers.insert(sender, from);
            if let Some(dest) = peers.get(&own) {
                let _ = sock.send_to(p, *dest).await;
            }
        }
    });
    (addr, h)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relayed_call_carries_audio_both_ways() {
    let (addr, h) = fake_reflector().await;
    let refl = vec![ReflectorConfig {
        server: 1,
        addr,
        peer_tag: [0x5a; 16],
    }];
    let (a, b) = call(refl, false).await;
    assert_eq!(a, LinkState::Connected { relay: true });
    assert_eq!(b, LinkState::Connected { relay: true });
    h.abort();
}
