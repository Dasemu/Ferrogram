//! Domain types shared by `net`, `store` and `app`.
//!
//! These are plain data: no grammers, no iced, no rusqlite types leak in here,
//! so every layer can be tested in isolation.

use std::fmt;

/// Chat identifier in "Bot API dialog id" form: users are positive, basic
/// groups are `-id`, channels/supergroups are `-100…id`. Unique across kinds.
pub type ChatId = i64;
/// Message id, unique within a chat.
pub type MessageId = i32;

/// Offset used by the Bot API encoding of channel ids.
pub const CHANNEL_OFFSET: i64 = 1_000_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChatKind {
    User,
    Bot,
    Group,
    Channel,
    SavedMessages,
}

impl ChatKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Bot => "bot",
            Self::Group => "group",
            Self::Channel => "channel",
            Self::SavedMessages => "saved",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "bot" => Self::Bot,
            "group" => Self::Group,
            "channel" => Self::Channel,
            "saved" => Self::SavedMessages,
            _ => Self::User,
        }
    }
}

/// A conversation in the chat list.
#[derive(Clone, PartialEq)]
pub struct Chat {
    pub id: ChatId,
    pub kind: ChatKind,
    pub title: String,
    pub unread_count: u32,
    pub muted: bool,
    pub pinned: bool,
    /// Highest outgoing message id the peer has read.
    pub read_outbox_max_id: MessageId,
    /// Highest incoming message id we have read.
    pub read_inbox_max_id: MessageId,
    pub last_message: Option<Message>,
    /// Sort key for the list (unix seconds of the last activity).
    pub last_date: i64,
    /// Telegram's id of the current profile photo, `None` when the chat has
    /// none (the avatar then falls back to the initials).
    pub photo_id: Option<i64>,
    /// Last-seen status of the other side of a private chat (not persisted).
    pub presence: Presence,
}

/// A user's online status as Telegram reports it (privacy settings can
/// blur the exact time into the coarse buckets).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Presence {
    #[default]
    Unknown,
    /// Online until the given unix time (the client must go offline then).
    Online {
        until: i64,
    },
    Offline {
        was_online: i64,
    },
    Recently,
    LastWeek,
    LastMonth,
    LongAgo,
}

impl fmt::Debug for Chat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Chat")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("title_len", &self.title.len())
            .field("unread", &self.unread_count)
            .finish_non_exhaustive()
    }
}

impl Chat {
    pub fn new(id: ChatId, kind: ChatKind, title: impl Into<String>) -> Self {
        Self {
            id,
            kind,
            title: title.into(),
            unread_count: 0,
            muted: false,
            pinned: false,
            read_outbox_max_id: 0,
            read_inbox_max_id: 0,
            last_message: None,
            last_date: 0,
            photo_id: None,
            presence: Presence::Unknown,
        }
    }

    /// Initials for the placeholder avatar ("Alice Smith" → "AS").
    pub fn initials(&self) -> String {
        initials(&self.title)
    }
}

pub fn initials(title: &str) -> String {
    let mut out = String::new();
    for word in title.split_whitespace().take(2) {
        if let Some(c) = word.chars().find(|c| c.is_alphanumeric()) {
            out.extend(c.to_uppercase());
        }
    }
    if out.is_empty() {
        out.push('?');
    }
    out
}

/// Telegram-style avatar colour index (0..7), stable per id.
pub fn avatar_color_index(id: i64) -> usize {
    // Telegram Desktop uses `id % 7` on the bare id; mirror that.
    let bare = if id < -CHANNEL_OFFSET {
        -id - CHANNEL_OFFSET
    } else {
        id.abs()
    };
    (bare % 7) as usize
}

/// A message found by the server-side (global) search, with enough chat
/// information to show and open a chat that is not in the loaded list.
#[derive(Clone, PartialEq)]
pub struct GlobalHit {
    pub message: Message,
    pub chat_title: String,
    pub chat_kind: ChatKind,
    pub photo_id: Option<i64>,
}

impl fmt::Debug for GlobalHit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GlobalHit")
            .field("chat", &self.message.chat_id)
            .field("id", &self.message.id)
            .finish_non_exhaustive()
    }
}

impl GlobalHit {
    /// A list entry for the hit's chat (used when opening it).
    pub fn to_chat(&self) -> Chat {
        let mut c = Chat::new(
            self.message.chat_id,
            self.chat_kind,
            self.chat_title.clone(),
        );
        c.photo_id = self.photo_id;
        c.last_date = self.message.date;
        c
    }
}

/// What someone is doing in a chat (the "typing…" family of statuses).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TypingAction {
    Typing,
    RecordingVoice,
    RecordingVideo,
    UploadingPhoto,
    UploadingVideo,
    UploadingFile,
    ChoosingSticker,
    Other,
}

impl TypingAction {
    /// Phrase for a private chat header / chat list row.
    pub fn private_text(self) -> &'static str {
        match self {
            Self::Typing | Self::Other => "typing…",
            Self::RecordingVoice => "recording voice message…",
            Self::RecordingVideo => "recording video…",
            Self::UploadingPhoto => "sending photo…",
            Self::UploadingVideo => "sending video…",
            Self::UploadingFile => "sending file…",
            Self::ChoosingSticker => "choosing sticker…",
        }
    }

    /// Verb phrase used after a name in groups ("Ana is typing…").
    pub fn group_verb(self) -> &'static str {
        match self {
            Self::Typing | Self::Other => "typing",
            Self::RecordingVoice => "recording voice message",
            Self::RecordingVideo => "recording video",
            Self::UploadingPhoto => "sending photo",
            Self::UploadingVideo => "sending video",
            Self::UploadingFile => "sending file",
            Self::ChoosingSticker => "choosing sticker",
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Media {
    Photo {
        id: i64,
        width: u32,
        height: u32,
    },
    File {
        name: String,
        size: u64,
        mime: String,
    },
    Sticker {
        emoji: String,
    },
    /// Anything we render as a labelled placeholder (poll, geo, voice, …).
    Other {
        label: String,
    },
}

impl Media {
    /// Short label used in the chat list preview.
    pub fn preview_label(&self) -> String {
        match self {
            Self::Photo { .. } => "Photo".into(),
            Self::File { name, .. } => name.clone(),
            Self::Sticker { emoji } => format!("{emoji} Sticker"),
            Self::Other { label } => label.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Reaction {
    pub emoji: String,
    pub count: u32,
    pub chosen: bool,
}

/// Delivery state of an outgoing message (rendered as clock / ✓ / ✓✓).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendState {
    Sending,
    Sent,
    Failed,
}

#[derive(Clone, PartialEq)]
pub struct Message {
    pub chat_id: ChatId,
    pub id: MessageId,
    pub sender_id: Option<i64>,
    pub sender_name: Option<String>,
    pub outgoing: bool,
    /// Unix seconds.
    pub date: i64,
    pub text: String,
    pub media: Option<Media>,
    pub reply_to: Option<MessageId>,
    pub edited: bool,
    pub reactions: Vec<Reaction>,
    /// Service messages ("X joined the group") are centered pills.
    pub service: bool,
    pub state: SendState,
}

impl fmt::Debug for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print bodies or names (architecture §6).
        f.debug_struct("Message")
            .field("chat_id", &self.chat_id)
            .field("id", &self.id)
            .field("outgoing", &self.outgoing)
            .field(
                "text",
                &format_args!("MessageBody(len={})", self.text.len()),
            )
            .field("has_media", &self.media.is_some())
            .finish_non_exhaustive()
    }
}

impl Message {
    pub fn text(chat_id: ChatId, id: MessageId, outgoing: bool, date: i64, text: &str) -> Self {
        Self {
            chat_id,
            id,
            sender_id: None,
            sender_name: None,
            outgoing,
            date,
            text: text.to_owned(),
            media: None,
            reply_to: None,
            edited: false,
            reactions: Vec::new(),
            service: false,
            state: SendState::Sent,
        }
    }

    /// One-line preview for the chat list / reply header.
    pub fn preview(&self) -> String {
        let text = self.text.lines().next().unwrap_or_default().trim();
        match (&self.media, text.is_empty()) {
            (Some(m), true) => m.preview_label(),
            (Some(m), false) => format!("{}, {text}", m.preview_label()),
            (None, _) => text.to_owned(),
        }
    }
}

/// The logged-in account.
#[derive(Clone, PartialEq)]
pub struct User {
    pub id: i64,
    pub name: String,
    pub username: Option<String>,
}

impl fmt::Debug for User {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("User")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// Connection state (architecture §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    Disconnected,
    Connecting,
    Connected,
    Reconnecting { attempt: u32 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_from_titles() {
        assert_eq!(initials("Alice Smith"), "AS");
        assert_eq!(initials("rust"), "R");
        assert_eq!(initials("  "), "?");
        assert_eq!(initials("Équipe ñandú"), "ÉÑ");
    }

    #[test]
    fn avatar_colors_are_stable_and_bounded() {
        assert_eq!(avatar_color_index(7), 0);
        assert_eq!(avatar_color_index(-1_000_000_000_008), 1);
        for id in [-5, 1, 99, 123_456_789] {
            assert!(avatar_color_index(id) < 7);
        }
    }

    #[test]
    fn preview_combines_media_and_text() {
        let mut m = Message::text(1, 1, false, 0, "hello\nsecond line");
        assert_eq!(m.preview(), "hello");
        m.media = Some(Media::Photo {
            id: 1,
            width: 1,
            height: 1,
        });
        assert_eq!(m.preview(), "Photo, hello");
        m.text.clear();
        assert_eq!(m.preview(), "Photo");
    }

    #[test]
    fn debug_never_prints_bodies() {
        let m = Message::text(1, 1, false, 0, "top secret plans");
        assert!(!format!("{m:?}").contains("secret"));
    }
}
