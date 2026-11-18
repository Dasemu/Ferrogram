//! The `Message` enum: UI events, network events and task completions.

use std::path::PathBuf;

use iced::widget::scrollable::Viewport;

use crate::auth::SignInOutcome;
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
    // login
    AuthInput(String),
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
    ToggleEmoji,
    InsertEmoji(&'static str),
    MessageMenu(Option<MessageId>),
    Reply(MessageId),
    Edit(MessageId),
    Delete(MessageId),
    Copy(MessageId),
    React(MessageId, String),
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
}

#[derive(Debug, Clone)]
pub enum Done {
    ApiReady(Result<ApiHandle, Error>),
    Restored(Result<Option<User>, Error>),
    CodeRequested(Result<(), Error>),
    SignIn(Result<SignInOutcome, Error>),
    Persisted(Result<(), Error>),
    Dialogs(Result<Vec<Chat>, Error>),
    CacheHistory {
        chat: ChatId,
        before: Option<MessageId>,
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
    /// A fire-and-forget call (edit, delete, react, mark read) finished.
    Call(Result<(), Error>),
    SearchResults {
        chat: ChatId,
        query: String,
        result: Result<Vec<ChatMessage>, Error>,
    },
    Photo {
        key: (ChatId, MessageId),
        result: Result<Vec<u8>, Error>,
    },
    LoggedOut,
    Screenshot(iced::window::Screenshot),
    ScreenshotSaved,
}
