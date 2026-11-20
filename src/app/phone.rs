//! App glue for one-to-one calls: runs the commands of [`super::call`] as
//! tasks, owns the per-call media engine and audio streams, and feeds engine
//! events back into the reducer. Nothing here runs while there is no call.

use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use iced::{Subscription, Task};
use tokio::sync::mpsc;
use tracing::{info, warn};

use super::call::{self, Cmd};
use super::{App, Done, Message, Ui};
use crate::net::Event;
use crate::net::calls::audio::{self, AudioHandle, Shared};
use crate::net::calls::engine::{self, EngineEvent, EngineHandle};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceKind {
    Input,
    Output,
}

/// The running engine and audio of the current call.
pub struct CallMedia {
    pub call_id: i64,
    pub engine: EngineHandle,
    pub audio: Option<AudioHandle>,
    pub shared: Arc<Shared>,
    pub frames: mpsc::Sender<Vec<u8>>,
}

impl std::fmt::Debug for CallMedia {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallMedia")
            .field("audio", &self.audio.is_some())
            .finish_non_exhaustive()
    }
}

/// Carries a started engine (or new audio streams) through a `Message`.
#[derive(Clone, Default)]
pub struct MediaSlot {
    pub media: Arc<Mutex<Option<CallMedia>>>,
    pub audio: Arc<Mutex<Option<AudioHandle>>>,
}

impl std::fmt::Debug for MediaSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MediaSlot")
    }
}

/// UI-only call state (panel expanded, device lists).
#[derive(Debug, Default, Clone)]
pub struct CallUi {
    pub expanded: bool,
    pub menu: Option<DeviceKind>,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    /// `None` = system default.
    pub input: Option<String>,
    pub output: Option<String>,
}

/// Hashable source for the engine's event stream (one per call).
struct EngineEvents(Arc<Mutex<Option<mpsc::UnboundedReceiver<EngineEvent>>>>, i64);

impl Hash for EngineEvents {
    fn hash<H: Hasher>(&self, state: &mut H) {
        "ferrogram-call-engine".hash(state);
        self.1.hash(state);
    }
}

fn engine_stream(src: &EngineEvents) -> impl futures::Stream<Item = Message> + use<> {
    let rx = src.0.lock().ok().and_then(|mut g| g.take());
    futures::stream::unfold(rx, |rx| async move {
        let mut rx = rx?;
        let ev = rx.recv().await?;
        Some((Message::Ui(Ui::CallEngine(ev)), Some(rx)))
    })
}

/// Drop media off the UI thread (joining the audio thread can take a moment).
fn drop_media(media: CallMedia) -> Task<Message> {
    media.engine.stop();
    Task::perform(
        async move {
            let _ = tokio::task::spawn_blocking(move || drop(media)).await;
        },
        |()| Message::Done(Done::CallSent(Ok(()))),
    )
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

impl App {
    fn me_id(&self) -> i64 {
        self.me.as_ref().map_or(0, |u| u.id)
    }

    pub(crate) fn call_subscriptions(&self, subs: &mut Vec<Subscription<Message>>) {
        if self.calls.session.is_some() {
            subs.push(
                iced::time::every(std::time::Duration::from_secs(1))
                    .map(|_| Message::Ui(Ui::CallTick)),
            );
        }
        if let Some(m) = &self.call_media {
            subs.push(Subscription::run_with(
                EngineEvents(Arc::clone(&m.engine.events), m.call_id),
                engine_stream,
            ));
        }
    }

    /// `updatePhoneCall` / `updatePhoneCallSignalingData`.
    pub(crate) fn on_call_event(&mut self, ev: &Event) -> Task<Message> {
        match ev {
            Event::PhoneCall(pc) => {
                let cmds = self.calls.on_phone_call(pc, self.me_id(), now());
                self.run_call_cmds(cmds)
            }
            Event::CallSignaling { call_id, data } => {
                match &self.call_media {
                    Some(m) if m.call_id == *call_id => m.engine.signaling(data.clone()),
                    _ => self.calls.early_signaling(*call_id, data.clone()),
                }
                Task::none()
            }
            _ => Task::none(),
        }
    }

    pub(crate) fn on_call_ui(&mut self, ui: Ui) -> Task<Message> {
        match ui {
            Ui::CallStart(peer) => {
                if self.calls.is_busy() {
                    self.toast = Some("You are already in a call.".into());
                    return Task::none();
                }
                let cmds = self.calls.start(peer, now());
                self.call_ui = super::phone::CallUi {
                    expanded: true,
                    ..std::mem::take(&mut self.call_ui)
                };
                let config = self.fetch_call_config();
                Task::batch([config, self.run_call_cmds(cmds)])
            }
            Ui::CallAccept => {
                self.call_ui.expanded = true;
                let cmds = self.calls.accept();
                self.run_call_cmds(cmds)
            }
            Ui::CallHangUp => {
                let cmds = self.calls.hang_up(now());
                self.run_call_cmds(cmds)
            }
            Ui::CallToggleMute => {
                if let Some(s) = self.calls.session.as_mut() {
                    s.muted = !s.muted;
                    if let Some(m) = &self.call_media {
                        m.shared
                            .muted
                            .store(s.muted, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                Task::none()
            }
            Ui::CallExpand(on) => {
                self.call_ui.expanded = on;
                self.call_ui.menu = None;
                Task::none()
            }
            Ui::CallDevices(kind) => {
                self.call_ui.menu = kind;
                if kind.is_none() {
                    return Task::none();
                }
                Task::perform(
                    async {
                        tokio::task::spawn_blocking(audio::list_devices)
                            .await
                            .unwrap_or_default()
                    },
                    |(i, o)| Message::Done(Done::CallDevicesListed(i, o)),
                )
            }
            Ui::CallPickDevice(kind, name) => {
                self.call_ui.menu = None;
                match kind {
                    DeviceKind::Input => self.call_ui.input = Some(name),
                    DeviceKind::Output => self.call_ui.output = Some(name),
                }
                self.restart_audio()
            }
            Ui::CallTick => {
                let cmds = self.calls.tick(now());
                let mut t = self.run_call_cmds(cmds);
                if self.calls.session.is_none()
                    && let Some(m) = self.call_media.take()
                {
                    t = Task::batch([t, drop_media(m)]);
                }
                t
            }
            Ui::CallEngine(ev) => self.on_engine_event(ev),
            _ => Task::none(),
        }
    }

    fn on_engine_event(&mut self, ev: EngineEvent) -> Task<Message> {
        match ev {
            EngineEvent::Signaling(data) => {
                let Some(call) = self.calls.session.as_ref().and_then(|s| s.call) else {
                    return Task::none();
                };
                self.api_call_quiet("call signaling", move |api| {
                    Box::pin(async move { api.send_call_signaling(call, data).await })
                })
            }
            EngineEvent::State(s) => {
                self.calls.link(s, now());
                Task::none()
            }
            EngineEvent::Quality(q) => {
                if let Some(s) = self.calls.session.as_mut() {
                    s.quality = q;
                }
                Task::none()
            }
            EngineEvent::Failed(why) => {
                warn!(reason = %why, "call failed");
                let cmds = self.calls.failed(&why, now());
                self.run_call_cmds(cmds)
            }
        }
    }

    pub(crate) fn on_call_done(&mut self, d: Done) -> Task<Message> {
        let me = self.me_id();
        match d {
            Done::CallConfig(Ok(c)) => {
                self.calls.config = c;
                Task::none()
            }
            Done::CallConfig(Err(_)) | Done::CallSent(_) => Task::none(),
            Done::CallPlaced(Ok((half, pc))) => {
                let cmds = self.calls.placed(half, &pc, me, now());
                self.run_call_cmds(cmds)
            }
            Done::CallAccepted(Ok((half, pc))) => {
                let cmds = self.calls.accepted(half, &pc, me, now());
                self.run_call_cmds(cmds)
            }
            Done::CallConfirmed(Ok((key, emoji, pc))) => {
                let mut cmds = self.calls.key_ready(key, emoji);
                cmds.extend(self.calls.on_phone_call(&pc, me, now()));
                self.run_call_cmds(cmds)
            }
            Done::CallKey(Ok((key, emoji))) => {
                let cmds = self.calls.key_ready(key, emoji);
                self.run_call_cmds(cmds)
            }
            Done::CallPlaced(Err(e))
            | Done::CallAccepted(Err(e))
            | Done::CallConfirmed(Err(e))
            | Done::CallKey(Err(e)) => {
                warn!(error = %e, "call setup failed");
                let text = match &e {
                    crate::error::Error::Protocol(p) if p.contains("PRIVACY") => {
                        "This user does not accept calls from you.".to_owned()
                    }
                    other => other.user_message(),
                };
                self.toast = Some(text);
                let cmds = self.calls.failed("setup", now());
                self.run_call_cmds(cmds)
            }
            Done::CallMedia(slot) => {
                let media = slot.media.lock().ok().and_then(|mut g| g.take());
                let Some(media) = media else {
                    // New audio streams after a device change.
                    let audio = slot.audio.lock().ok().and_then(|mut g| g.take());
                    match (self.call_media.as_mut(), audio) {
                        (Some(m), Some(a)) => m.audio = Some(a),
                        (None, Some(a)) => {
                            return Task::perform(
                                async move {
                                    let _ = tokio::task::spawn_blocking(move || drop(a)).await;
                                },
                                |()| Message::Done(Done::CallSent(Ok(()))),
                            );
                        }
                        (_, None) => {}
                    }
                    return Task::none();
                };
                let live = self.calls.session.as_ref().is_some_and(|s| {
                    !s.is_ended() && s.call.is_some_and(|c| c.id == media.call_id)
                });
                if !live {
                    return drop_media(media);
                }
                if media.audio.is_none() {
                    self.toast = Some("No microphone or speaker available.".into());
                }
                if let Some(s) = self.calls.session.as_mut() {
                    media
                        .shared
                        .muted
                        .store(s.muted, std::sync::atomic::Ordering::Relaxed);
                    for d in s.early_signaling.drain(..) {
                        media.engine.signaling(d);
                    }
                }
                let old = self.call_media.replace(media);
                old.map_or_else(Task::none, drop_media)
            }
            Done::CallDevicesListed(i, o) => {
                self.call_ui.inputs = i;
                self.call_ui.outputs = o;
                Task::none()
            }
            _ => Task::none(),
        }
    }

    fn fetch_call_config(&self) -> Task<Message> {
        let Some(api) = self.api.clone() else {
            return Task::none();
        };
        Task::perform(async move { api.call_config().await }, |r| {
            Message::Done(Done::CallConfig(r))
        })
    }

    /// Reopen the audio devices after a device change (the engine keeps
    /// running).
    fn restart_audio(&mut self) -> Task<Message> {
        let Some(m) = self.call_media.as_mut() else {
            return Task::none();
        };
        let old = m.audio.take();
        let shared = Arc::clone(&m.shared);
        let frames = m.frames.clone();
        let (input, output) = (self.call_ui.input.clone(), self.call_ui.output.clone());
        let slot = MediaSlot::default();
        let out = slot.clone();
        Task::perform(
            async move {
                let _ = tokio::task::spawn_blocking(move || {
                    drop(old);
                    match AudioHandle::start(input, output, shared, frames) {
                        Ok(a) => {
                            if let Ok(mut g) = out.audio.lock() {
                                *g = Some(a);
                            }
                        }
                        Err(e) => warn!(error = %e, "audio restart failed"),
                    }
                })
                .await;
            },
            // An empty media slot that carries only the new audio streams.
            move |()| Message::Done(Done::CallMedia(slot.clone())),
        )
    }

    fn run_call_cmds(&mut self, cmds: Vec<Cmd>) -> Task<Message> {
        let tasks: Vec<Task<Message>> = cmds.into_iter().map(|c| self.run_call_cmd(c)).collect();
        Task::batch(tasks)
    }

    fn run_call_cmd(&mut self, cmd: Cmd) -> Task<Message> {
        let Some(api) = self.api.clone() else {
            return Task::none();
        };
        match cmd {
            Cmd::Request { peer } => Task::perform(call::place(api, peer), |r| {
                Message::Done(Done::CallPlaced(r))
            }),
            Cmd::Received { call, peer } => {
                let name = self
                    .chats
                    .get(peer)
                    .map(|c| c.title.clone())
                    .or_else(|| self.user_names.get(&peer).cloned())
                    .unwrap_or_else(|| "Unknown".into());
                if self.config.notifications {
                    crate::desktop::notify(crate::desktop::Toast {
                        title: name,
                        body: "Incoming call".into(),
                        chat: Some(peer),
                    });
                }
                self.call_ui.expanded = true;
                let show = if self.window.open {
                    Task::none()
                } else {
                    self.update_ui(Ui::Remote(crate::remote::Remote::Show { chat: None }))
                };
                let config = self.fetch_call_config();
                let received = self.api_call_quiet("receivedCall", move |api| {
                    Box::pin(async move { api.received_call(call).await })
                });
                Task::batch([show, config, received])
            }
            Cmd::Reject { call, reason } => self.api_call_quiet("discardCall", move |api| {
                Box::pin(async move { api.discard_call(call, 0, reason).await })
            }),
            Cmd::Discard {
                call,
                duration,
                reason,
            } => self.api_call_quiet("discardCall", move |api| {
                Box::pin(async move { api.discard_call(call, duration, reason).await })
            }),
            Cmd::Confirm { call, g_b } => {
                let Some(half) = self.calls.session.as_ref().and_then(|s| s.dh.clone()) else {
                    return Task::none();
                };
                Task::perform(call::confirm(api, call, half, g_b), |r| {
                    Message::Done(Done::CallConfirmed(r))
                })
            }
            Cmd::Accept { call } => Task::perform(call::accept(api, call), |r| {
                Message::Done(Done::CallAccepted(r))
            }),
            Cmd::Finish { g_a, fingerprint } => {
                let Some(s) = self.calls.session.as_mut() else {
                    return Task::none();
                };
                let (Some(half), Some(hash)) = (s.dh.clone(), s.g_a_hash.clone()) else {
                    return Task::none();
                };
                s.g_a = Some(g_a.clone());
                Task::perform(call::finish(half, hash, g_a, fingerprint), |r| {
                    Message::Done(Done::CallKey(r))
                })
            }
            Cmd::StartEngine => self.start_engine(),
            Cmd::StopEngine => self.call_media.take().map_or_else(Task::none, drop_media),
        }
    }

    fn start_engine(&mut self) -> Task<Message> {
        let Some(s) = self.calls.session.as_ref() else {
            return Task::none();
        };
        let Some(call_id) = s.call.map(|c| c.id) else {
            return Task::none();
        };
        let cfg = match call::engine_config(s, &self.calls.config, self.config.mock) {
            Ok(c) => c,
            Err(e) => {
                self.toast = Some(e.clone());
                let cmds = self.calls.failed(&e, now());
                return self.run_call_cmds(cmds);
            }
        };
        let (input, output) = (self.call_ui.input.clone(), self.call_ui.output.clone());
        let with_audio = self.config.screenshot.is_none();
        info!("starting call media");
        let slot = MediaSlot::default();
        let out = slot.clone();
        Task::perform(
            async move {
                let shared = Shared::new();
                let (tx, rx) = mpsc::channel(64);
                let engine = engine::start(cfg, Arc::clone(&shared), rx);
                let audio = if with_audio {
                    let (sh, ftx) = (Arc::clone(&shared), tx.clone());
                    tokio::task::spawn_blocking(move || AudioHandle::start(input, output, sh, ftx))
                        .await
                        .ok()
                        .and_then(|r| r.map_err(|e| warn!(error = %e, "no call audio")).ok())
                } else {
                    None
                };
                if let Ok(mut g) = out.media.lock() {
                    *g = Some(CallMedia {
                        call_id,
                        engine,
                        audio,
                        shared,
                        frames: tx,
                    });
                }
            },
            move |()| Message::Done(Done::CallMedia(slot.clone())),
        )
    }
}
