//! The `Message` enum: UI events, network events and task completions.

use std::path::PathBuf;

use iced::widget::scrollable::Viewport;

use crate::auth::{SecretInput, SignInOutcome};
use crate::error::Error;
use crate::model::{Chat, ChatId, Message as ChatMessage, MessageId, User};
use crate::net::{ApiHandle, Event};

#[derive(Debug, Clone)]
pub enum Message {
    Ui(Ui),
    Net(Event),
    Done(Done),
}

#[derive(Debug, Clone)]
pub enum Ui {
    // first-run API credential setup
    SetupApiId(String),
    SetupApiHash(SecretInput),
    SetupSubmit,
    /// "Advanced: use your own API credentials" on the login screen.
    OpenApiSetup,
    CancelSetup,
    /// Remove the user override and use the built-in credentials.
    UseBuiltinCredentials,
    // login
    AuthInput(SecretInput),
    AuthSubmit,
    AuthBack,
    // sidebar
    SelectChat(ChatId),
    SidebarSearch(String),
    ClearSidebarSearch,
    /// Open a chat at a specific message from global/in-chat search.
    OpenResult(ChatId, MessageId),
    // chat pane
    HistoryScrolled(Viewport),
    ComposerChanged(String),
    Send,
    Attach,
    FilePicked(Option<PathBuf>),
    FileDropped(PathBuf),
    SendPendingFile,
    CancelPendingFile,
    ToggleEmoji,
    InsertEmoji(&'static str),
    MessageMenu(Option<MessageId>),
    Reply(MessageId),
    Edit(MessageId),
    Delete(MessageId),
    Copy(MessageId),
    React(MessageId, String),
    SaveMedia(ChatId, MessageId),
    CancelCompose,
    ToggleChatSearch,
    ChatSearchInput(String),
    ChatSearchSubmit,
    ScrollToBottom,
    OpenMedia(ChatId, MessageId),
    CloseMedia,
    LogOut,
    ToggleMainMenu,
    DismissToast,
    // window / keyboard
    Focus(bool),
    Escape,
    Tick,
    /// 1 s while somebody is typing: expire stale "typing…" statuses.
    TypingTick,
    /// Click on a received file: open with the system handler (or save,
    /// for executables).
    OpenFile(ChatId, MessageId),
}

#[derive(Debug, Clone)]
pub enum Done {
    ApiReady(Result<ApiHandle, Error>),
    Restored(Result<Option<User>, Error>),
    /// Retry `restore()` after a network failure (backoff elapsed).
    RetryRestore,
    CredentialsSaved(Result<(), Error>),
    /// The old client was shut down; start a new one with fresh credentials.
    RestartApi,
    CodeRequested(Result<(), Error>),
    SignIn(Result<SignInOutcome, Error>),
    Persisted(Result<(), Error>),
    Dialogs(Result<Vec<Chat>, Error>),
    CacheHistory {
        chat: ChatId,
        before: Option<MessageId>,
        /// The cache page is inside the network-confirmed range.
        complete: bool,
        result: Result<Vec<ChatMessage>, Error>,
    },
    NetHistory {
        chat: ChatId,
        before: Option<MessageId>,
        result: Result<Vec<ChatMessage>, Error>,
    },
    Sent {
        chat: ChatId,
        temp: MessageId,
        result: Result<ChatMessage, Error>,
    },
    /// An optimistic edit/delete failed: restore `old`.
    Rollback {
        old: ChatMessage,
        error: Error,
    },
    /// A fire-and-forget call (react, mark read, cache write) finished.
    Call(Result<(), Error>),
    SearchResults {
        chat: ChatId,
        query: String,
        result: Result<Vec<ChatMessage>, Error>,
    },
    SidebarResults {
        query: String,
        result: Result<Vec<ChatMessage>, Error>,
    },
    FileStat {
        path: PathBuf,
        result: Result<(u64, bool), Error>,
    },
    SavePicked {
        chat: ChatId,
        id: MessageId,
        path: Option<PathBuf>,
    },
    MediaSaved(Result<(), Error>),
    Photo {
        key: (ChatId, MessageId),
        result: Result<Vec<u8>, Error>,
    },
    /// A profile photo arrived (or failed). `Err(Cancelled)` = not available
    /// right now (offline), retried later.
    Avatar {
        chat: ChatId,
        photo_id: i64,
        result: Result<Vec<u8>, Error>,
    },
    /// A received file was downloaded and is ready for the system handler.
    FileReady(Result<PathBuf, Error>),
    FileOpened(Result<(), Error>),
    LoggedOut,
    Screenshot(iced::window::Screenshot),
    ScreenshotSaved,
}
