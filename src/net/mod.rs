//! Protocol client. Owns sockets, never touches UI types.
//!
//! The app talks to Telegram only through the [`TelegramApi`] trait so it can
//! be driven by [`crate::mock::FakeTelegram`] in tests and `--mock` mode, and
//! by [`mtproto::GrammersApi`] (grammers-client) for real accounts.

pub mod backoff;
pub mod calls;
pub mod mtproto;
pub mod session;
pub mod types;

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use secrecy::SecretString;

use crate::auth::SignInOutcome;
use crate::error::{Phone, Result};
use crate::model::{
    Chat, ChatId, ConnState, Draft, Folder, GlobalHit, Message, MessageId, PeerInfo, Presence,
    Reaction, SharedKind, SharedPage, TypingAction,
};

/// Hard cap on a shared-content thumbnail.
pub const MAX_THUMB_BYTES: usize = 512 * 1024;

/// Hard cap on photo bytes downloaded for display.
pub const MAX_PHOTO_BYTES: usize = 20 * 1024 * 1024;

/// Events pushed by the protocol client (architecture §3, `Net(..)`).
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Connection(ConnState),
    NewMessage(Message),
    MessageEdited(Message),
    /// `chat` is `None` for private chats / basic groups (Telegram does not
    /// say which chat a message id belonged to there).
    MessagesDeleted {
        chat: Option<ChatId>,
        ids: Vec<MessageId>,
    },
    Reactions {
        chat: ChatId,
        id: MessageId,
        reactions: Vec<Reaction>,
    },
    /// The peer read our messages up to `max_id`.
    ReadOutbox {
        chat: ChatId,
        max_id: MessageId,
    },
    /// We read incoming messages (possibly on another device).
    ReadInbox {
        chat: ChatId,
        max_id: MessageId,
        still_unread: u32,
    },
    /// Someone started (`Some`) or stopped (`None`) an action in `chat`.
    Typing {
        chat: ChatId,
        user: i64,
        action: Option<TypingAction>,
    },
    /// A user's online status changed.
    UserStatus {
        user: i64,
        presence: Presence,
    },
    /// The draft of `chat` changed (on another device). `None` = cleared.
    Draft {
        chat: ChatId,
        draft: Option<Draft>,
    },
    /// A chat was muted or unmuted (`updateNotifySettings`), maybe on
    /// another device.
    Muted {
        chat: ChatId,
        muted: bool,
    },
    /// A folder was created, edited (`Some`) or deleted (`None`), maybe on
    /// another device (`updateDialogFilter`).
    FolderChanged {
        id: i32,
        folder: Option<Folder>,
    },
    /// The folder order changed (`updateDialogFilterOrder`).
    FoldersOrder(Vec<i32>),
    /// Something about the folders changed that the update does not say
    /// (`updateDialogFilters`): fetch them again.
    FoldersStale,
    SessionRevoked,
}

/// How to send a local file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadKind {
    /// Compressed photo.
    Photo,
    /// Uncompressed file / document.
    Document,
}

impl UploadKind {
    /// Photos are sent compressed like the official client's default.
    pub fn for_path(path: &std::path::Path) -> Self {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        match ext.as_deref() {
            Some("jpg" | "jpeg" | "png" | "webp") => Self::Photo,
            _ => Self::Document,
        }
    }
}

#[async_trait]
pub trait TelegramApi: Send + Sync {
    /// Connect and check for an authorised session. `Some(user)` = logged in.
    async fn restore(&self) -> Result<Option<crate::model::User>>;
    async fn request_code(&self, phone: Phone) -> Result<()>;
    async fn sign_in(&self, code: String) -> Result<SignInOutcome>;
    async fn check_password(&self, password: SecretString) -> Result<SignInOutcome>;
    /// Persist the session (auth key) to the vault.
    async fn persist_session(&self) -> Result<()>;
    async fn log_out(&self) -> Result<()>;

    /// The first `limit` dialogs (restarts paging).
    async fn dialogs(&self, limit: usize) -> Result<Vec<Chat>>;
    /// The next `limit` dialogs after the previous `dialogs` / `more_dialogs`
    /// call. Fewer than `limit` means the list is exhausted.
    async fn more_dialogs(&self, limit: usize) -> Result<Vec<Chat>>;
    /// Messages older than `before` (exclusive), oldest first.
    async fn history(
        &self,
        chat: ChatId,
        before: Option<MessageId>,
        limit: usize,
    ) -> Result<Vec<Message>>;
    async fn send_text(
        &self,
        chat: ChatId,
        text: String,
        reply_to: Option<MessageId>,
    ) -> Result<Message>;
    async fn send_file(
        &self,
        chat: ChatId,
        path: PathBuf,
        kind: UploadKind,
        caption: String,
        reply_to: Option<MessageId>,
    ) -> Result<Message>;
    async fn edit_message(&self, chat: ChatId, id: MessageId, text: String) -> Result<()>;
    async fn delete_messages(&self, chat: ChatId, ids: Vec<MessageId>) -> Result<()>;
    /// `None` removes our reaction.
    async fn react(&self, chat: ChatId, id: MessageId, emoji: Option<String>) -> Result<()>;
    async fn mark_read(&self, chat: ChatId) -> Result<()>;
    async fn search(&self, chat: ChatId, query: String) -> Result<Vec<Message>>;
    /// Bytes of a photo (JPEG/PNG) for display (a thumbnail when available).
    async fn download_photo(&self, chat: ChatId, id: MessageId) -> Result<Vec<u8>>;
    /// Server-side search over every chat (`messages.searchGlobal`), newest
    /// first.
    async fn search_global(&self, query: String, limit: usize) -> Result<Vec<GlobalHit>>;
    /// The small (160 px) profile photo `photo_id` of `chat` (JPEG).
    /// Capped at [`crate::store::avatars::MAX_AVATAR_BYTES`].
    async fn download_avatar(&self, chat: ChatId, photo_id: i64) -> Result<Vec<u8>>;
    /// Save (or, with empty text, clear) the draft of `chat` on the server,
    /// the way the official client syncs drafts between devices.
    async fn save_draft(
        &self,
        chat: ChatId,
        text: String,
        reply_to: Option<MessageId>,
    ) -> Result<()>;
    /// Tell the peer we are typing (`true`) or stopped (`false`).
    async fn set_typing(&self, chat: ChatId, active: bool) -> Result<()>;
    /// Save a message's media (file, photo) to `path`.
    async fn download_media(&self, chat: ChatId, id: MessageId, path: PathBuf) -> Result<()>;
    /// Bio / description, username, phone (when visible) and member count
    /// for the info panel (`users.getFullUser`, `messages.getFullChat`,
    /// `channels.getFullChannel`).
    async fn peer_info(&self, chat: ChatId) -> Result<PeerInfo>;
    /// Mute (forever) or unmute a chat (`account.updateNotifySettings`).
    async fn set_muted(&self, chat: ChatId, muted: bool) -> Result<()>;
    /// One page of a shared-content tab (`messages.search` with the tab's
    /// filter), newest first, older than `offset_id` (0 = from the newest).
    async fn shared_media(
        &self,
        chat: ChatId,
        kind: SharedKind,
        offset_id: MessageId,
        limit: usize,
    ) -> Result<SharedPage>;
    /// A small preview (JPEG/PNG, at most [`MAX_THUMB_BYTES`]) of a photo,
    /// video or GIF message.
    async fn download_thumb(&self, chat: ChatId, id: MessageId) -> Result<Vec<u8>>;
    /// The account's chat folders in order (`messages.getDialogFilters`).
    async fn dialog_filters(&self) -> Result<Vec<Folder>>;
    /// Create or replace a folder (`messages.updateDialogFilter`); a new
    /// folder has an id no other folder uses.
    async fn save_dialog_filter(&self, folder: Folder) -> Result<()>;
    /// Delete a folder (`messages.updateDialogFilter` without a filter).
    async fn delete_dialog_filter(&self, id: i32) -> Result<()>;
    /// Reorder folders (`messages.updateDialogFiltersOrder`).
    async fn reorder_dialog_filters(&self, order: Vec<i32>) -> Result<()>;
    /// One-shot user-facing notice from the backend (e.g. session reset).
    fn take_notice(&self) -> Option<String> {
        None
    }

    /// The live update stream. A new call takes over from the previous one.
    fn events(&self) -> BoxStream<'static, Event>;
}

/// Cloneable, debuggable handle used inside iced messages.
#[derive(Clone)]
pub struct ApiHandle(pub Arc<dyn TelegramApi>);

impl fmt::Debug for ApiHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiHandle")
    }
}

impl std::ops::Deref for ApiHandle {
    type Target = dyn TelegramApi;
    fn deref(&self) -> &Self::Target {
        &*self.0
    }
}

/// Fan-in point for protocol events. `subscribe()` can be called again
/// (e.g. after logout → login): the newest subscriber takes over and the
/// previous stream ends, so events are never silently swallowed by a stale
/// receiver.
pub struct EventHub {
    tx: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<Event>>>,
    /// Events emitted while nobody is subscribed (bounded).
    backlog: std::sync::Mutex<std::collections::VecDeque<Event>>,
}

const BACKLOG_MAX: usize = 256;

impl Default for EventHub {
    fn default() -> Self {
        Self::new()
    }
}

impl EventHub {
    pub fn new() -> Self {
        Self {
            tx: std::sync::Mutex::new(None),
            backlog: std::sync::Mutex::new(std::collections::VecDeque::new()),
        }
    }

    pub fn emit(&self, ev: Event) {
        let Ok(guard) = self.tx.lock() else {
            return;
        };
        if let Some(tx) = guard.as_ref()
            && tx.send(ev.clone()).is_ok()
        {
            return;
        }
        drop(guard);
        if let Ok(mut b) = self.backlog.lock() {
            if b.len() >= BACKLOG_MAX {
                b.pop_front();
            }
            b.push_back(ev);
        }
    }

    pub fn subscribe(&self) -> BoxStream<'static, Event> {
        use futures::StreamExt;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        if let Ok(mut b) = self.backlog.lock() {
            for ev in b.drain(..) {
                let _ = tx.send(ev);
            }
        }
        if let Ok(mut g) = self.tx.lock() {
            *g = Some(tx);
        }
        futures::stream::unfold(
            rx,
            |mut rx| async move { rx.recv().await.map(|ev| (ev, rx)) },
        )
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_kind_from_extension() {
        assert_eq!(UploadKind::for_path("a/b.JPG".as_ref()), UploadKind::Photo);
        assert_eq!(
            UploadKind::for_path("a/b.pdf".as_ref()),
            UploadKind::Document
        );
        assert_eq!(UploadKind::for_path("noext".as_ref()), UploadKind::Document);
    }
}
