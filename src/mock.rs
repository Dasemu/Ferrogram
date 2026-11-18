//! Mock data and a fake Telegram backend, used by tests and `--mock` mode.
//!
//! [`FakeTelegram`] implements [`TelegramApi`] entirely in memory, so the
//! whole app (login state machine, chat list, history paging, sending,
//! edits, deletes, reactions, search, notifications) can be exercised
//! without an account or network.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use secrecy::{ExposeSecret, SecretString};
use tokio::sync::mpsc;

use crate::auth::SignInOutcome;
use crate::error::{AuthError, Error, Phone, Result};
use crate::model::{
    CHANNEL_OFFSET, Chat, ChatId, ChatKind, Media, Message, MessageId, Reaction, User,
};
use crate::net::{Event, TelegramApi, UploadKind};

/// Fixed "now" so screenshots and tests are deterministic (2026-11-18 14:32 UTC).
pub const MOCK_NOW: i64 = 1_795_012_320;
pub const SELF_ID: i64 = 100;
pub const ALICE: ChatId = 201;
pub const ECHO_BOT: ChatId = 206;
pub const RUST_GROUP: ChatId = -301;
pub const DEV_CHANNEL: ChatId = -CHANNEL_OFFSET - 401;
pub const NEWS_CHANNEL: ChatId = -CHANNEL_OFFSET - 402;
/// Login code that leads to the 2FA screen.
pub const CODE_2FA: &str = "12345";
/// Login code that signs in without 2FA.
pub const CODE_PLAIN: &str = "22222";
pub const PASSWORD: &str = "hunter2";

pub fn me() -> User {
    User {
        id: SELF_ID,
        name: "Sam Rivera".into(),
        username: Some("samr".into()),
    }
}

const MIN: i64 = 60;
const HOUR: i64 = 3600;
const DAY: i64 = 86_400;

fn msg(chat: ChatId, id: MessageId, out: bool, date: i64, text: &str) -> Message {
    let mut m = Message::text(chat, id, out, date, text);
    if !out {
        m.sender_id = Some(chat);
    } else {
        m.sender_id = Some(SELF_ID);
    }
    m
}

fn group_msg(chat: ChatId, id: MessageId, who: (i64, &str), date: i64, text: &str) -> Message {
    let mut m = Message::text(chat, id, who.0 == SELF_ID, date, text);
    m.sender_id = Some(who.0);
    if who.0 != SELF_ID {
        m.sender_name = Some(who.1.into());
    }
    m
}

const FILLER: &[&str] = &[
    "Did you see the release notes?",
    "Yes! The new scheduler looks great",
    "I'll try it on the laptop tonight",
    "Haha, that's exactly what I said",
    "Let me check and get back to you",
    "Sounds good 👍",
    "Can you send me the link again?",
    "Sure, one sec",
    "On my way",
    "Perfect, thanks!",
    "What time works for you?",
    "After lunch is fine",
];

/// Alice: a long private history (for infinite scroll) ending in a scripted
/// conversation with a photo, a file, a reply, an edit and reactions.
fn alice_history() -> Vec<Message> {
    let mut v = Vec::new();
    let mut id = 1;
    let start = MOCK_NOW - 6 * DAY;
    for i in 0..140 {
        let out = i % 3 == 1;
        let t = start + i64::from(i) * 50 * MIN;
        v.push(msg(ALICE, id, out, t, FILLER[i as usize % FILLER.len()]));
        id += 1;
    }
    let base = MOCK_NOW - 2 * HOUR;
    let mut push = |out: bool, dt: i64, text: &str, f: &mut dyn FnMut(&mut Message)| {
        let mut m = msg(ALICE, id, out, base + dt, text);
        f(&mut m);
        v.push(m);
        id += 1;
    };
    push(
        false,
        0,
        "Hey Sam! Are we still on for the hike on Saturday? 🏔️",
        &mut |_| {},
    );
    push(
        true,
        2 * MIN,
        "Absolutely! I already packed my bag",
        &mut |_| {},
    );
    push(false, 3 * MIN, "", &mut |m| {
        m.media = Some(Media::Photo {
            id: 9001,
            width: 1280,
            height: 853,
        });
        m.text = "The view from the top last time 😍".into();
    });
    push(true, 5 * MIN, "Wow, that's stunning", &mut |m| {
        m.reactions = vec![Reaction {
            emoji: "❤️".into(),
            count: 1,
            chosen: false,
        }];
    });
    push(false, 9 * MIN, "", &mut |m| {
        m.media = Some(Media::File {
            name: "trail-map.pdf".into(),
            size: 2_457_600,
            mime: "application/pdf".into(),
        });
    });
    push(
        false,
        10 * MIN,
        "Here's the trail map. It's about 14 km round trip",
        &mut |_| {},
    );
    push(
        true,
        12 * MIN,
        "Great, I'll print it out. Should we start at 8?",
        &mut |m| {
            m.reply_to = Some(145);
            m.edited = true;
        },
    );
    push(
        false,
        15 * MIN,
        "8 is perfect. I'll bring coffee ☕",
        &mut |m| {
            m.reactions = vec![
                Reaction {
                    emoji: "👍".into(),
                    count: 1,
                    chosen: true,
                },
                Reaction {
                    emoji: "🔥".into(),
                    count: 1,
                    chosen: false,
                },
            ];
        },
    );
    push(true, 16 * MIN, "You're the best!", &mut |_| {});
    push(false, 118 * MIN, "See you on Saturday then!", &mut |_| {});
    v
}

fn chats_and_history() -> (Vec<Chat>, BTreeMap<ChatId, Vec<Message>>) {
    let mut chats = Vec::new();
    let mut hist: BTreeMap<ChatId, Vec<Message>> = BTreeMap::new();

    let mut add = |id: ChatId,
                   kind: ChatKind,
                   title: &str,
                   unread: u32,
                   muted: bool,
                   pinned: bool,
                   msgs: Vec<Message>| {
        let mut c = Chat::new(id, kind, title);
        c.unread_count = unread;
        c.muted = muted;
        c.pinned = pinned;
        c.last_message = msgs.last().cloned();
        c.last_date = c.last_message.as_ref().map_or(0, |m| m.date);
        let max_out = msgs.iter().filter(|m| m.outgoing).map(|m| m.id).max();
        c.read_outbox_max_id = max_out.unwrap_or(0);
        c.read_inbox_max_id = msgs
            .len()
            .saturating_sub(unread as usize)
            .try_into()
            .unwrap_or(0);
        chats.push(c);
        hist.insert(id, msgs);
    };

    add(
        SELF_ID,
        ChatKind::SavedMessages,
        "Saved Messages",
        0,
        false,
        true,
        vec![msg(
            SELF_ID,
            1,
            true,
            MOCK_NOW - 3 * DAY,
            "Wi-Fi at the cabin: ask Alice",
        )],
    );
    add(
        ALICE,
        ChatKind::User,
        "Alice Johnson",
        1,
        false,
        false,
        alice_history(),
    );

    let ana = (311, "Ana Torres");
    let leo = (312, "Leo Park");
    add(
        RUST_GROUP,
        ChatKind::Group,
        "Rustaceans Madrid",
        12,
        false,
        false,
        vec![
            group_msg(
                RUST_GROUP,
                1,
                ana,
                MOCK_NOW - 5 * HOUR,
                "Meetup next Thursday at the usual place",
            ),
            group_msg(
                RUST_GROUP,
                2,
                leo,
                MOCK_NOW - 4 * HOUR,
                "Who's giving the async talk?",
            ),
            group_msg(
                RUST_GROUP,
                3,
                (SELF_ID, "Sam"),
                MOCK_NOW - 3 * HOUR,
                "I can do a short one on iced 0.14",
            ),
            group_msg(
                RUST_GROUP,
                4,
                ana,
                MOCK_NOW - 25 * MIN,
                "Leo: has anyone tried the new borrow checker?",
            ),
        ],
    );
    add(
        DEV_CHANNEL,
        ChatKind::Channel,
        "Ferrogram Dev",
        3,
        false,
        false,
        vec![group_msg(
            DEV_CHANNEL,
            1,
            (0, "Ferrogram Dev"),
            MOCK_NOW - 47 * MIN,
            "Build 0.1.0 is out: reactions, search and notifications",
        )],
    );
    add(
        202,
        ChatKind::User,
        "Bob Martinez",
        0,
        false,
        false,
        vec![
            msg(
                202,
                1,
                false,
                MOCK_NOW - DAY - 2 * HOUR,
                "Thanks for the help yesterday",
            ),
            msg(202, 2, true, MOCK_NOW - DAY - HOUR, "Anytime! 🙌"),
        ],
    );
    add(
        ECHO_BOT,
        ChatKind::Bot,
        "Echo Bot",
        0,
        false,
        false,
        vec![msg(
            ECHO_BOT,
            1,
            false,
            MOCK_NOW - 2 * DAY,
            "Send me anything and I'll repeat it.",
        )],
    );
    add(
        -302,
        ChatKind::Group,
        "Family",
        0,
        false,
        false,
        vec![group_msg(
            -302,
            1,
            (313, "Mom"),
            MOCK_NOW - 2 * DAY - HOUR,
            "Sunday lunch at 2?",
        )],
    );
    add(
        203,
        ChatKind::User,
        "Carla Gómez",
        2,
        false,
        false,
        vec![
            msg(
                203,
                1,
                false,
                MOCK_NOW - 3 * DAY,
                "Are you coming to the concert?",
            ),
            msg(
                203,
                2,
                false,
                MOCK_NOW - 3 * DAY + MIN,
                "I have an extra ticket 🎟️",
            ),
        ],
    );
    add(
        NEWS_CHANNEL,
        ChatKind::Channel,
        "Tech News Daily",
        128,
        true,
        false,
        vec![group_msg(
            NEWS_CHANNEL,
            1,
            (0, "Tech News Daily"),
            MOCK_NOW - 4 * DAY,
            "Wayland adoption hits a new record",
        )],
    );
    add(
        -303,
        ChatKind::Group,
        "Weekend Hiking",
        0,
        true,
        false,
        vec![group_msg(
            -303,
            1,
            (314, "Marta"),
            MOCK_NOW - 5 * DAY,
            "Photo",
        )],
    );
    add(
        204,
        ChatKind::User,
        "David Chen",
        0,
        false,
        false,
        vec![msg(
            204,
            1,
            true,
            MOCK_NOW - 6 * DAY,
            "Let's catch up next week",
        )],
    );
    add(
        -304,
        ChatKind::Group,
        "Book Club",
        0,
        false,
        false,
        vec![group_msg(
            -304,
            1,
            (315, "Irene"),
            MOCK_NOW - 9 * DAY,
            "Next book: The Left Hand of Darkness",
        )],
    );
    if let Some(v) = hist.get_mut(&-303)
        && let Some(m) = v.last_mut()
    {
        m.text.clear();
        m.media = Some(Media::Photo {
            id: 9002,
            width: 800,
            height: 600,
        });
    }
    if let Some(c) = chats.iter_mut().find(|c| c.id == -303) {
        c.last_message = hist.get(&-303).and_then(|v| v.last().cloned());
    }
    (chats, hist)
}

/// Chats exactly as the fake backend would return them.
pub fn mock_chats() -> Vec<Chat> {
    chats_and_history().0
}

pub fn mock_history(chat: ChatId) -> Vec<Message> {
    chats_and_history().1.remove(&chat).unwrap_or_default()
}

#[derive(Debug, Clone)]
pub struct FakeOptions {
    /// Start already logged in.
    pub logged_in: bool,
    /// Echo Bot answers and peers "read" our messages after a delay.
    pub live_simulation: bool,
    pub latency: Duration,
}

impl Default for FakeOptions {
    fn default() -> Self {
        Self {
            logged_in: false,
            live_simulation: true,
            latency: Duration::from_millis(0),
        }
    }
}

struct State {
    authorized: bool,
    code_sent: bool,
    password_pending: bool,
    chats: Vec<Chat>,
    history: BTreeMap<ChatId, Vec<Message>>,
    calls: Vec<String>,
    next_id: MessageId,
}

pub struct FakeTelegram {
    state: Mutex<State>,
    opts: FakeOptions,
    events_tx: mpsc::UnboundedSender<Event>,
    events_rx: Mutex<Option<mpsc::UnboundedReceiver<Event>>>,
}

impl FakeTelegram {
    pub fn new(opts: FakeOptions) -> Arc<Self> {
        let (chats, history) = chats_and_history();
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        Arc::new(Self {
            state: Mutex::new(State {
                authorized: opts.logged_in,
                code_sent: false,
                password_pending: false,
                chats,
                history,
                calls: Vec::new(),
                next_id: 10_000,
            }),
            opts,
            events_tx,
            events_rx: Mutex::new(Some(events_rx)),
        })
    }

    fn st(&self) -> Result<std::sync::MutexGuard<'_, State>> {
        self.state
            .lock()
            .map_err(|_| Error::Net("fake poisoned".into()))
    }

    /// Names of the API calls made so far (for integration tests).
    pub fn calls(&self) -> Vec<String> {
        self.st().map(|s| s.calls.clone()).unwrap_or_default()
    }

    /// Inject a server-side event, as if it came from the update stream.
    pub fn push_event(&self, ev: Event) {
        if let Ok(mut s) = self.st()
            && let Event::NewMessage(m) = &ev
        {
            s.history.entry(m.chat_id).or_default().push(m.clone());
        }
        let _ = self.events_tx.send(ev);
    }

    async fn call(&self, name: &str) -> Result<()> {
        if !self.opts.latency.is_zero() {
            tokio::time::sleep(self.opts.latency).await;
        }
        self.st()?.calls.push(name.to_owned());
        Ok(())
    }

    fn require_auth(&self) -> Result<()> {
        if self.st()?.authorized {
            Ok(())
        } else {
            Err(Error::Auth(AuthError::SessionRevoked))
        }
    }

    fn append_outgoing(&self, mut m: Message) -> Result<Message> {
        let mut s = self.st()?;
        s.next_id += 1;
        m.id = s.next_id;
        s.history.entry(m.chat_id).or_default().push(m.clone());
        Ok(m)
    }

    fn simulate_after_send(&self, sent: &Message) {
        if !self.opts.live_simulation {
            return;
        }
        let tx = self.events_tx.clone();
        let chat = sent.chat_id;
        let id = sent.id;
        let text = sent.text.clone();
        let echo_id = self.st().map(|mut s| {
            s.next_id += 1;
            s.next_id
        });
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(900)).await;
            let _ = tx.send(Event::ReadOutbox { chat, max_id: id });
            if chat == ECHO_BOT
                && let Ok(echo_id) = echo_id
            {
                tokio::time::sleep(Duration::from_millis(600)).await;
                let now = chrono::Utc::now().timestamp();
                let reply = if text.is_empty() {
                    "Nice file!".to_owned()
                } else {
                    text
                };
                let mut m = msg(ECHO_BOT, echo_id, false, now, &reply);
                m.reply_to = Some(id);
                let _ = tx.send(Event::NewMessage(m));
            }
        });
    }
}

#[async_trait]
impl TelegramApi for FakeTelegram {
    async fn restore(&self) -> Result<Option<User>> {
        self.call("restore").await?;
        Ok(self.st()?.authorized.then(me))
    }

    async fn request_code(&self, phone: Phone) -> Result<()> {
        self.call("request_code").await?;
        if !phone.is_plausible() || phone.normalized().starts_with("+000") {
            return Err(Error::Auth(AuthError::InvalidPhone));
        }
        self.st()?.code_sent = true;
        Ok(())
    }

    async fn sign_in(&self, code: String) -> Result<SignInOutcome> {
        self.call("sign_in").await?;
        let mut s = self.st()?;
        if !s.code_sent {
            return Err(Error::Auth(AuthError::InvalidCode));
        }
        match code.as_str() {
            CODE_2FA => {
                s.password_pending = true;
                Ok(SignInOutcome::PasswordRequired {
                    hint: Some("usual one".into()),
                })
            }
            CODE_PLAIN => {
                s.authorized = true;
                Ok(SignInOutcome::SignedIn(me()))
            }
            _ => Err(Error::Auth(AuthError::InvalidCode)),
        }
    }

    async fn check_password(&self, password: SecretString) -> Result<SignInOutcome> {
        self.call("check_password").await?;
        let mut s = self.st()?;
        if s.password_pending && password.expose_secret() == PASSWORD {
            s.password_pending = false;
            s.authorized = true;
            Ok(SignInOutcome::SignedIn(me()))
        } else {
            Err(Error::Auth(AuthError::InvalidPassword))
        }
    }

    async fn persist_session(&self) -> Result<()> {
        self.call("persist_session").await
    }

    async fn log_out(&self) -> Result<()> {
        self.call("log_out").await?;
        self.st()?.authorized = false;
        Ok(())
    }

    async fn dialogs(&self, limit: usize) -> Result<Vec<Chat>> {
        self.call("dialogs").await?;
        self.require_auth()?;
        let s = self.st()?;
        let mut chats = s.chats.clone();
        for c in &mut chats {
            if let Some(last) = s.history.get(&c.id).and_then(|h| h.last()) {
                c.last_date = last.date;
                c.last_message = Some(last.clone());
            }
        }
        chats.truncate(limit);
        Ok(chats)
    }

    async fn history(
        &self,
        chat: ChatId,
        before: Option<MessageId>,
        limit: usize,
    ) -> Result<Vec<Message>> {
        self.call("history").await?;
        self.require_auth()?;
        let s = self.st()?;
        let all = s.history.get(&chat).cloned().unwrap_or_default();
        let older: Vec<_> = all
            .into_iter()
            .filter(|m| before.is_none_or(|b| m.id < b))
            .collect();
        let skip = older.len().saturating_sub(limit);
        Ok(older.into_iter().skip(skip).collect())
    }

    async fn send_text(
        &self,
        chat: ChatId,
        text: String,
        reply_to: Option<MessageId>,
    ) -> Result<Message> {
        self.call("send_text").await?;
        self.require_auth()?;
        let mut m = msg(chat, 0, true, chrono::Utc::now().timestamp(), &text);
        m.reply_to = reply_to;
        let sent = self.append_outgoing(m)?;
        self.simulate_after_send(&sent);
        Ok(sent)
    }

    async fn send_file(
        &self,
        chat: ChatId,
        path: PathBuf,
        kind: UploadKind,
        caption: String,
        reply_to: Option<MessageId>,
    ) -> Result<Message> {
        self.call("send_file").await?;
        self.require_auth()?;
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into());
        let mut m = msg(chat, 0, true, chrono::Utc::now().timestamp(), &caption);
        m.reply_to = reply_to;
        m.media = Some(match kind {
            UploadKind::Photo => Media::Photo {
                id: 50_000 + i64::try_from(size % 1000).unwrap_or(0),
                width: 640,
                height: 420,
            },
            UploadKind::Document => Media::File {
                name,
                size,
                mime: "application/octet-stream".into(),
            },
        });
        let sent = self.append_outgoing(m)?;
        self.simulate_after_send(&sent);
        Ok(sent)
    }

    async fn edit_message(&self, chat: ChatId, id: MessageId, text: String) -> Result<()> {
        self.call("edit_message").await?;
        let edited = {
            let mut s = self.st()?;
            let m = s
                .history
                .get_mut(&chat)
                .and_then(|h| h.iter_mut().find(|m| m.id == id))
                .ok_or_else(|| Error::Protocol("MESSAGE_ID_INVALID".into()))?;
            m.text = text;
            m.edited = true;
            m.clone()
        };
        let _ = self.events_tx.send(Event::MessageEdited(edited));
        Ok(())
    }

    async fn delete_messages(&self, chat: ChatId, ids: Vec<MessageId>) -> Result<()> {
        self.call("delete_messages").await?;
        if let Some(h) = self.st()?.history.get_mut(&chat) {
            h.retain(|m| !ids.contains(&m.id));
        }
        let channel = chat < -CHANNEL_OFFSET;
        let _ = self.events_tx.send(Event::MessagesDeleted {
            chat: channel.then_some(chat),
            ids,
        });
        Ok(())
    }

    async fn react(&self, chat: ChatId, id: MessageId, emoji: Option<String>) -> Result<()> {
        self.call("react").await?;
        let reactions = {
            let mut s = self.st()?;
            let m = s
                .history
                .get_mut(&chat)
                .and_then(|h| h.iter_mut().find(|m| m.id == id))
                .ok_or_else(|| Error::Protocol("MESSAGE_ID_INVALID".into()))?;
            crate::app::state::toggle_reaction(&mut m.reactions, emoji.as_deref());
            m.reactions.clone()
        };
        let _ = self.events_tx.send(Event::Reactions {
            chat,
            id,
            reactions,
        });
        Ok(())
    }

    async fn mark_read(&self, chat: ChatId) -> Result<()> {
        self.call("mark_read").await?;
        let mut s = self.st()?;
        if let Some(c) = s.chats.iter_mut().find(|c| c.id == chat) {
            c.unread_count = 0;
        }
        Ok(())
    }

    async fn search(&self, chat: ChatId, query: String) -> Result<Vec<Message>> {
        self.call("search").await?;
        let q = query.to_lowercase();
        let s = self.st()?;
        let mut out: Vec<_> = s
            .history
            .get(&chat)
            .map(|h| {
                h.iter()
                    .filter(|m| m.text.to_lowercase().contains(&q))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        out.reverse();
        out.truncate(50);
        Ok(out)
    }

    async fn download_photo(&self, _chat: ChatId, id: MessageId) -> Result<Vec<u8>> {
        self.call("download_photo").await?;
        Ok(mock_photo_png(i64::from(id), 640, 427))
    }

    fn events(&self) -> BoxStream<'static, Event> {
        let rx = self.events_rx.lock().ok().and_then(|mut g| g.take());
        match rx {
            Some(rx) => {
                futures::stream::unfold(
                    rx,
                    |mut rx| async move { rx.recv().await.map(|ev| (ev, rx)) },
                )
                .boxed()
            }
            None => futures::stream::empty().boxed(),
        }
    }
}

/// A procedurally generated landscape-ish PNG (sky gradient, mountains),
/// so mock mode has a "photo" without shipping any image assets.
pub fn mock_photo_png(seed: i64, w: u32, h: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    let s = (seed.rem_euclid(97)) as f32 / 97.0;
    for y in 0..h {
        for x in 0..w {
            let fx = x as f32 / w as f32;
            let fy = y as f32 / h as f32;
            let ridge1 =
                0.55 + 0.12 * ((fx * 9.0 + s * 3.0).sin() * 0.6 + (fx * 23.0).sin() * 0.25);
            let ridge2 = 0.72 + 0.08 * ((fx * 5.0 + 1.3).sin() + (fx * 17.0 + s).sin() * 0.3);
            let (r, g, b) = if fy > ridge2 {
                (40.0 + 30.0 * fy, 70.0 + 40.0 * fy, 50.0)
            } else if fy > ridge1 {
                (70.0 + 20.0 * fx, 85.0 + 25.0 * fx, 120.0 + 20.0 * fy)
            } else {
                let t = fy / ridge1;
                (
                    250.0 - 120.0 * t + 20.0 * s,
                    170.0 - 40.0 * t,
                    120.0 + 100.0 * t,
                )
            };
            rgba.extend_from_slice(&[r as u8, g as u8, b as u8, 255]);
        }
    }
    encode_png(w, h, &rgba).unwrap_or_default()
}

/// Encode RGBA8 as PNG.
pub fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc
            .write_header()
            .map_err(|_| Error::Io("png header".into()))?;
        writer
            .write_image_data(rgba)
            .map_err(|_| Error::Io("png data".into()))?;
    }
    Ok(out)
}
