//! Protocol client. Owns sockets, never touches UI types.
//!
//! The app talks to Telegram only through the [`TelegramApi`] trait so it can
//! be driven by [`crate::mock::FakeTelegram`] in tests and `--mock` mode, and
//! by [`mtproto::GrammersApi`] (grammers-client) for real accounts.

pub mod backoff;
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
use crate::model::{Chat, ChatId, ConnState, Message, MessageId, Reaction};

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

    async fn dialogs(&self, limit: usize) -> Result<Vec<Chat>>;
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
    /// Bytes of a photo (JPEG/PNG) for display.
    async fn download_photo(&self, chat: ChatId, id: MessageId) -> Result<Vec<u8>>;

    /// The live update stream. Only the first call returns a live stream.
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
