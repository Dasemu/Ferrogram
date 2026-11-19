//! App state + update (the "Elm" side). No I/O happens here directly: every
//! side effect is a `Task` that calls `net`, `store` or `auth`.

pub mod message;
pub mod screenshot;
pub mod state;

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::widget::{Id, image, operation};
use iced::{Subscription, Task, Theme, event, keyboard, window};
use tracing::{info, warn};

pub use message::{Done, Message, Ui};
use state::{ChatList, ComposeMode, Conversation, Lru, PAGE};

use crate::auth::secrets::{KeyringStore, MemoryStore, SecretStore};
use crate::auth::vault::Vault;
use crate::auth::{AuthCommand, AuthEvent, AuthState, SignInOutcome};
use crate::config::{Config, ScreenshotTarget};
use crate::error::{AuthError, Error, Redacted};
use crate::mock::{FakeOptions, FakeTelegram, MOCK_NOW};
use crate::model::{ChatId, ConnState, Media, Message as ChatMessage, MessageId, SendState, User};
use crate::net::{ApiHandle, Event, UploadKind, backoff};
use crate::store::Store;
use crate::ui::theme::FerroTheme;

pub const HISTORY_ID: &str = "history";
pub const COMPOSER_ID: &str = "composer";
pub const AUTH_INPUT_ID: &str = "auth-input";
pub const CHAT_SEARCH_ID: &str = "chat-search";
/// Image cache budget (architecture §3).
pub const IMAGE_BUDGET: usize = 48 * 1024 * 1024;
/// Photos fetched per open conversation (newest first).
pub const PHOTO_PREFETCH: usize = 20;

#[derive(Debug, Clone, Default)]
pub struct ChatSearch {
    pub query: String,
    pub results: Vec<ChatMessage>,
    pub searching: bool,
    pub submitted: bool,
}

/// First-run form for the Telegram API credentials.
#[derive(Debug, Clone, Default)]
pub struct SetupForm {
    pub api_id: String,
    pub api_hash: crate::auth::SecretInput,
    pub error: Option<String>,
    pub busy: bool,
}

/// A file waiting for the user to confirm sending (picker or drag & drop).
#[derive(Debug, Clone)]
pub struct PendingFile {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub kind: UploadKind,
}

pub struct App {
    pub config: Config,
    pub theme: FerroTheme,
    pub iced_theme: Theme,
    pub api: Option<ApiHandle>,
    /// Bumped whenever a new client is built so the update subscription restarts.
    pub api_gen: u64,
    pub store: Store,
    pub secrets: Arc<dyn SecretStore>,
    pub setup: Option<SetupForm>,
    pub auth: AuthState,
    pub restore_attempt: u32,
    pub me: Option<User>,
    pub chats: ChatList,
    pub selected: Option<ChatId>,
    pub conversation: Option<Conversation>,
    pub sidebar_query: String,
    /// Cached messages matching the sidebar query (all chats).
    pub sidebar_messages: Vec<ChatMessage>,
    pub composer: String,
    pub compose_mode: ComposeMode,
    pub pending_file: Option<PendingFile>,
    pub emoji_open: bool,
    pub menu_for: Option<MessageId>,
    pub chat_search: Option<ChatSearch>,
    pub highlight: Option<MessageId>,
    pub conn: ConnState,
    pub focused: bool,
    pub at_bottom: bool,
    pub photos: Lru<(ChatId, MessageId), image::Handle>,
    pub photo_loading: HashSet<(ChatId, MessageId)>,
    pub viewer: Option<(ChatId, MessageId)>,
    pub toast: Option<String>,
    pub main_menu: bool,
    pub now: i64,
    pub tz_offset: i32,
    pub started: Instant,
    pub first_frame_logged: bool,
}

/// Hashable wrapper so the update stream can be a `Subscription::run_with`.
/// The generation makes iced start a fresh stream for every new client.
struct EventsSource(ApiHandle, u64);

impl Hash for EventsSource {
    fn hash<H: Hasher>(&self, state: &mut H) {
        "ferrogram-events".hash(state);
        self.1.hash(state);
    }
}

fn events_stream(src: &EventsSource) -> impl futures::Stream<Item = Message> + use<> {
    use futures::StreamExt;
    src.0.events().map(Message::Net)
}

/// Errors after which `restore()` should simply be retried (we may well
/// have a valid session and just be offline).
pub fn is_transient(e: &Error) -> bool {
    matches!(e, Error::Net(_) | Error::Cancelled)
}

impl App {
    pub fn boot(config: Config) -> (Self, Task<Message>) {
        let theme = if config.light_theme {
            FerroTheme::day()
        } else {
            FerroTheme::night()
        };
        let tz_offset = chrono::Local::now().offset().local_minus_utc();

        let (store, secrets): (Store, Arc<dyn SecretStore>) = if config.mock {
            (open_memory_store(), Arc::new(MemoryStore::new()))
        } else {
            let store = Store::open(&config.cache_db_path()).unwrap_or_else(|e| {
                warn!(error = %e, "cache unavailable, using in-memory store");
                open_memory_store()
            });
            (store, Arc::new(KeyringStore))
        };

        // First frame comes from the cache, never from the network.
        let cached = store.load_chats().unwrap_or_default();
        let app = Self {
            iced_theme: theme.iced(),
            theme,
            api: None,
            api_gen: 0,
            store,
            secrets,
            setup: None,
            auth: AuthState::Restoring,
            restore_attempt: 0,
            me: None,
            chats: ChatList::new(cached),
            selected: None,
            conversation: None,
            sidebar_query: String::new(),
            sidebar_messages: Vec::new(),
            composer: String::new(),
            compose_mode: ComposeMode::Normal,
            pending_file: None,
            emoji_open: false,
            menu_for: None,
            chat_search: None,
            highlight: None,
            conn: ConnState::Connecting,
            focused: true,
            at_bottom: true,
            photos: Lru::new(IMAGE_BUDGET),
            photo_loading: HashSet::new(),
            viewer: None,
            toast: None,
            main_menu: false,
            now: if config.mock {
                MOCK_NOW
            } else {
                chrono::Utc::now().timestamp()
            },
            tz_offset: if config.mock { 0 } else { tz_offset },
            started: Instant::now(),
            first_frame_logged: !config.trace_startup,
            config,
        };

        let task = if app.config.mock {
            let logged_in = !matches!(
                app.config.screenshot.as_ref().map(|s| s.0),
                Some(
                    ScreenshotTarget::LoginPhone
                        | ScreenshotTarget::LoginCode
                        | ScreenshotTarget::LoginPassword
                )
            ) && std::env::var("FERROGRAM_MOCK_LOGGED_OUT").is_err();
            let fake = FakeTelegram::new(FakeOptions {
                logged_in,
                live_simulation: app.config.screenshot.is_none(),
                latency: Duration::from_millis(if app.config.screenshot.is_some() {
                    0
                } else {
                    150
                }),
            });
            Task::done(Message::Done(Done::ApiReady(Ok(ApiHandle(fake)))))
        } else {
            app.start_real_api()
        };
        let script = screenshot::script(&app.config);
        (app, Task::batch([task, script]))
    }

    /// Build a brand-new grammers client (boot, and after logout/revoke so
    /// no auth key, update receiver or event channel is reused).
    fn start_real_api(&self) -> Task<Message> {
        let vault = Vault::new(self.secrets.clone(), self.config.session_blob_path());
        let config = self.config.clone();
        let secrets = self.secrets.clone();
        Task::perform(
            async move {
                let creds = tokio::task::spawn_blocking(move || {
                    config.load_api_credentials(secrets.as_ref())
                })
                .await
                .map_err(|_| Error::Cancelled)??;
                let api = crate::net::mtproto::GrammersApi::start(creds, vault).await?;
                Ok(ApiHandle(Arc::new(crate::net::mtproto::Grammers(api))))
            },
            |r| Message::Done(Done::ApiReady(r)),
        )
    }

    pub fn title(&self) -> String {
        let unread = self.chats.total_unread();
        if unread > 0 && self.auth.is_signed_in() {
            format!("Ferrogram ({unread})")
        } else {
            "Ferrogram".into()
        }
    }

    pub fn theme(&self) -> Theme {
        self.iced_theme.clone()
    }

    /// Show the main window (not the login screen): signed in, or still
    /// restoring a session while offline with a cached chat list.
    pub fn shows_main(&self) -> bool {
        self.auth.is_signed_in()
            || (self.auth == AuthState::Restoring
                && self.setup.is_none()
                && !self.chats.all().is_empty())
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let mut subs = vec![event::listen_with(|ev, _status, _id| match ev {
            iced::Event::Window(window::Event::Focused) => Some(Message::Ui(Ui::Focus(true))),
            iced::Event::Window(window::Event::Unfocused) => Some(Message::Ui(Ui::Focus(false))),
            iced::Event::Window(window::Event::FileDropped(p)) => {
                Some(Message::Ui(Ui::FileDropped(p)))
            }
            iced::Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(keyboard::key::Named::Escape),
                ..
            }) => Some(Message::Ui(Ui::Escape)),
            _ => None,
        })];
        if let (Some(api), true) = (&self.api, self.auth.is_signed_in()) {
            subs.push(Subscription::run_with(
                EventsSource(api.clone(), self.api_gen),
                events_stream,
            ));
        }
        if !self.config.mock {
            subs.push(iced::time::every(Duration::from_secs(30)).map(|_| Message::Ui(Ui::Tick)));
        }
        if !self.first_frame_logged {
            subs.push(window::frames().map(|_| Message::Ui(Ui::Tick)));
        }
        Subscription::batch(subs)
    }

    // ------------------------------------------------------------- update

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Ui(ui) => self.update_ui(ui),
            Message::Net(ev) => self.on_event(ev),
            Message::Done(done) => self.on_done(done),
        }
    }

    fn auth(&mut self, ev: AuthEvent) -> Task<Message> {
        let Some(cmd) = crate::auth::reduce(&mut self.auth, ev) else {
            return Task::none();
        };
        let Some(api) = self.api.clone() else {
            // No client yet (still starting, or it failed): fail the step so
            // the form never stays busy forever.
            let err = Error::Net("not connected".into());
            let ev = match cmd {
                AuthCommand::RequestCode(_) => AuthEvent::CodeRequested(Err(err)),
                AuthCommand::SignIn { .. } | AuthCommand::CheckPassword(_) => {
                    AuthEvent::SignIn(Err(err))
                }
                AuthCommand::PersistSession => return Task::none(),
            };
            crate::auth::reduce(&mut self.auth, ev);
            return Task::none();
        };
        match cmd {
            AuthCommand::RequestCode(phone) => {
                Task::perform(async move { api.request_code(phone).await }, |r| {
                    Message::Done(Done::CodeRequested(r))
                })
            }
            AuthCommand::SignIn { code } => {
                Task::perform(async move { api.sign_in(code).await }, |r| {
                    Message::Done(Done::SignIn(r))
                })
            }
            AuthCommand::CheckPassword(pw) => {
                Task::perform(async move { api.check_password(pw).await }, |r| {
                    Message::Done(Done::SignIn(r))
                })
            }
            AuthCommand::PersistSession => {
                Task::perform(async move { api.persist_session().await }, |r| {
                    Message::Done(Done::Persisted(r))
                })
            }
        }
    }

    fn on_signed_in(&mut self, user: Option<User>) -> Task<Message> {
        let mut tasks = vec![self.fetch_dialogs()];
        if let Some(u) = &user {
            let id = u.id.to_string();
            tasks.push(self.store_bg(move |s| s.set_kv("self_id", &id)));
            self.me = user;
        }
        self.conn = ConnState::Connected;
        self.restore_attempt = 0;
        if self.config.screenshot.is_none() {
            tasks.push(operation::focus(Id::new(COMPOSER_ID)));
        }
        Task::batch(tasks)
    }

    fn fetch_dialogs(&self) -> Task<Message> {
        let Some(api) = self.api.clone() else {
            return Task::none();
        };
        Task::perform(async move { api.dialogs(200).await }, |r| {
            Message::Done(Done::Dialogs(r))
        })
    }

    fn api_call<F>(&self, f: F) -> Task<Message>
    where
        F: FnOnce(ApiHandle) -> futures::future::BoxFuture<'static, crate::error::Result<()>>
            + Send
            + 'static,
    {
        let Some(api) = self.api.clone() else {
            return Task::none();
        };
        Task::perform(f(api), |r| Message::Done(Done::Call(r)))
    }

    /// An optimistic call that restores `old` if it fails.
    fn api_call_or_rollback<F>(&self, old: ChatMessage, f: F) -> Task<Message>
    where
        F: FnOnce(ApiHandle) -> futures::future::BoxFuture<'static, crate::error::Result<()>>
            + Send
            + 'static,
    {
        let Some(api) = self.api.clone() else {
            return Task::done(Message::Done(Done::Rollback {
                old,
                error: Error::Net("not connected".into()),
            }));
        };
        Task::perform(f(api), move |r| match r {
            Ok(()) => Message::Done(Done::Call(Ok(()))),
            Err(error) => Message::Done(Done::Rollback {
                old: old.clone(),
                error,
            }),
        })
    }

    fn store_bg<F>(&self, f: F) -> Task<Message>
    where
        F: FnOnce(&Store) -> crate::error::Result<()> + Send + 'static,
    {
        let store = self.store.clone();
        Task::perform(async move { store.call(f).await }, |r| {
            Message::Done(Done::Call(r))
        })
    }

    fn open_chat(&mut self, chat: ChatId) -> Task<Message> {
        if self.selected == Some(chat) && self.conversation.is_some() {
            return Task::none();
        }
        self.selected = Some(chat);
        self.conversation = Some(Conversation::new(chat));
        self.compose_mode = ComposeMode::Normal;
        self.pending_file = None;
        self.menu_for = None;
        self.chat_search = None;
        self.highlight = None;
        self.emoji_open = false;
        self.at_bottom = true;
        self.chats.mark_read(chat);
        let store = self.store.clone();
        // The newest cached page is shown immediately; the network page that
        // is always requested alongside replaces/extends it.
        let cache = Task::perform(
            async move { store.call(move |s| s.load_history(chat, None, PAGE)).await },
            move |result| {
                Message::Done(Done::CacheHistory {
                    chat,
                    before: None,
                    complete: false,
                    result,
                })
            },
        );
        let net = self.net_history(chat, None);
        let read = self.api_call(move |api| Box::pin(async move { api.mark_read(chat).await }));
        let unread = self.store_bg(move |s| s.set_unread(chat, 0, 0));
        Task::batch([
            cache,
            net,
            read,
            unread,
            operation::focus(Id::new(COMPOSER_ID)),
        ])
    }

    fn net_history(&self, chat: ChatId, before: Option<MessageId>) -> Task<Message> {
        let Some(api) = self.api.clone() else {
            return Task::none();
        };
        Task::perform(
            async move { api.history(chat, before, PAGE).await },
            move |result| {
                Message::Done(Done::NetHistory {
                    chat,
                    before,
                    result,
                })
            },
        )
    }

    fn load_older(&mut self) -> Task<Message> {
        let Some(conv) = &mut self.conversation else {
            return Task::none();
        };
        if conv.loading_older || conv.reached_start {
            return Task::none();
        }
        let Some(before) = conv.oldest_id() else {
            return Task::none();
        };
        conv.loading_older = true;
        let chat = conv.chat_id;
        let store = self.store.clone();
        Task::perform(
            async move {
                store
                    .call(move |s| s.load_older_cached(chat, before, PAGE))
                    .await
            },
            move |result| {
                let (result, complete) = match result {
                    Ok((msgs, complete)) => (Ok(msgs), complete),
                    Err(e) => (Err(e), false),
                };
                Message::Done(Done::CacheHistory {
                    chat,
                    before: Some(before),
                    complete,
                    result,
                })
            },
        )
    }

    /// Request (thumbnail) downloads for photos in the open conversation.
    fn fetch_photos(&mut self) -> Task<Message> {
        let Some(conv) = &self.conversation else {
            return Task::none();
        };
        let Some(api) = self.api.clone() else {
            return Task::none();
        };
        let mut tasks = Vec::new();
        let photos = conv
            .messages()
            .iter()
            .rev()
            .filter(|m| m.id > 0 && matches!(m.media, Some(Media::Photo { .. })))
            .take(PHOTO_PREFETCH);
        for m in photos {
            let key = (m.chat_id, m.id);
            if self.photos.peek(&key).is_some() || !self.photo_loading.insert(key) {
                continue;
            }
            let api = api.clone();
            tasks.push(Task::perform(
                async move { api.download_photo(key.0, key.1).await },
                move |result| Message::Done(Done::Photo { key, result }),
            ));
        }
        Task::batch(tasks)
    }

    fn toast(&mut self, text: String) -> Task<Message> {
        self.toast = Some(text);
        Task::perform(
            async { tokio::time::sleep(Duration::from_secs(4)).await },
            |_| Message::Ui(Ui::DismissToast),
        )
    }

    fn update_ui(&mut self, ui: Ui) -> Task<Message> {
        match ui {
            Ui::SetupApiId(s) => {
                if let Some(f) = &mut self.setup {
                    f.api_id = s.chars().filter(char::is_ascii_digit).take(12).collect();
                }
                Task::none()
            }
            Ui::SetupApiHash(s) => {
                if let Some(f) = &mut self.setup {
                    f.api_hash = s;
                }
                Task::none()
            }
            Ui::SetupSubmit => {
                let Some(f) = &mut self.setup else {
                    return Task::none();
                };
                let hash = f.api_hash.0.trim().to_owned();
                let (Ok(api_id), true) = (
                    f.api_id.parse::<i32>(),
                    hash.len() >= 16 && hash.chars().all(|c| c.is_ascii_alphanumeric()),
                ) else {
                    f.error =
                        Some("Enter the numeric api_id and the 32-character api_hash.".into());
                    return Task::none();
                };
                f.busy = true;
                f.error = None;
                let config = self.config.clone();
                let secrets = self.secrets.clone();
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            config.save_api_credentials(secrets.as_ref(), api_id, &hash)
                        })
                        .await
                        .map_err(|_| Error::Cancelled)?
                    },
                    |r| Message::Done(Done::CredentialsSaved(r)),
                )
            }
            Ui::AuthInput(s) => self.auth(AuthEvent::Input(s)),
            Ui::AuthSubmit => self.auth(AuthEvent::Submit),
            Ui::AuthBack => self.auth(AuthEvent::Back),

            Ui::SelectChat(id) => self.open_chat(id),
            Ui::SidebarSearch(q) => {
                self.sidebar_query = q.clone();
                if q.trim().chars().count() < 2 {
                    self.sidebar_messages.clear();
                    return Task::none();
                }
                let ids: Vec<ChatId> = self.chats.all().iter().map(|c| c.id).collect();
                let store = self.store.clone();
                let tag = q.clone();
                Task::perform(
                    async move {
                        store
                            .call(move |s| {
                                let mut out = Vec::new();
                                for id in ids {
                                    out.extend(s.search_local(id, q.trim(), 5)?);
                                }
                                out.sort_by_key(|m| std::cmp::Reverse(m.date));
                                out.truncate(30);
                                Ok(out)
                            })
                            .await
                    },
                    move |result| {
                        Message::Done(Done::SidebarResults {
                            query: tag.clone(),
                            result,
                        })
                    },
                )
            }
            Ui::ClearSidebarSearch => {
                self.sidebar_query.clear();
                self.sidebar_messages.clear();
                Task::none()
            }
            Ui::OpenResult(chat, id) => {
                let open = self.open_chat(chat);
                self.highlight = Some(id);
                open
            }

            Ui::HistoryScrolled(vp) => {
                let off = vp.absolute_offset().y;
                self.at_bottom = off < 60.0;
                let dist =
                    state::distance_from_top(off, vp.content_bounds().height, vp.bounds().height);
                if dist < state::LOAD_THRESHOLD {
                    self.load_older()
                } else {
                    Task::none()
                }
            }
            Ui::ComposerChanged(s) => {
                self.composer = s;
                Task::none()
            }
            Ui::Send => {
                if self.pending_file.is_some() {
                    return self.update_ui(Ui::SendPendingFile);
                }
                self.send()
            }
            Ui::Attach => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .set_title("Send file")
                        .pick_file()
                        .await
                        .map(|f| f.path().to_path_buf())
                },
                |p| Message::Ui(Ui::FilePicked(p)),
            ),
            Ui::FilePicked(Some(path)) | Ui::FileDropped(path) => {
                if self.selected.is_none() || !self.auth.is_signed_in() {
                    return self.toast("Open a chat first to send a file.".into());
                }
                Task::perform(
                    async move {
                        let r = tokio::fs::metadata(&path)
                            .await
                            .map(|m| (m.len(), m.is_dir()))
                            .map_err(Error::from);
                        (path, r)
                    },
                    |(path, result)| Message::Done(Done::FileStat { path, result }),
                )
            }
            Ui::FilePicked(None) => Task::none(),
            Ui::SendPendingFile => match self.pending_file.take() {
                Some(f) => self.send_file(f),
                None => Task::none(),
            },
            Ui::CancelPendingFile => {
                self.pending_file = None;
                Task::none()
            }
            Ui::ToggleEmoji => {
                self.emoji_open = !self.emoji_open;
                Task::none()
            }
            Ui::InsertEmoji(e) => {
                self.composer.push_str(e);
                Task::batch([
                    operation::focus(Id::new(COMPOSER_ID)),
                    operation::move_cursor_to_end(Id::new(COMPOSER_ID)),
                ])
            }
            Ui::MessageMenu(id) => {
                // No actions on messages that are still being sent.
                let id = id.filter(|i| *i > 0);
                self.menu_for = if self.menu_for == id { None } else { id };
                Task::none()
            }
            Ui::Reply(id) if id > 0 => {
                self.menu_for = None;
                self.compose_mode = ComposeMode::Reply(id);
                operation::focus(Id::new(COMPOSER_ID))
            }
            Ui::Edit(id) if id > 0 => {
                self.menu_for = None;
                if let Some(m) = self.conversation.as_ref().and_then(|c| c.get(id)) {
                    self.composer = m.text.clone();
                    self.compose_mode = ComposeMode::Edit(id);
                }
                Task::batch([
                    operation::focus(Id::new(COMPOSER_ID)),
                    operation::move_cursor_to_end(Id::new(COMPOSER_ID)),
                ])
            }
            Ui::Delete(id) if id > 0 => {
                self.menu_for = None;
                let Some(conv) = &mut self.conversation else {
                    return Task::none();
                };
                let chat = conv.chat_id;
                let Some(old) = conv.get(id).cloned() else {
                    return Task::none();
                };
                conv.remove_local(id);
                self.api_call_or_rollback(old, move |api| {
                    Box::pin(async move { api.delete_messages(chat, vec![id]).await })
                })
            }
            Ui::React(id, emoji) if id > 0 => {
                self.menu_for = None;
                let Some(conv) = &mut self.conversation else {
                    return Task::none();
                };
                let chat = conv.chat_id;
                let Some(old) = conv.get(id).cloned() else {
                    return Task::none();
                };
                match conv.toggle_reaction(id, &emoji) {
                    Some(target) => self.api_call_or_rollback(old, move |api| {
                        Box::pin(async move { api.react(chat, id, target).await })
                    }),
                    None => Task::none(),
                }
            }
            Ui::Reply(_) | Ui::Edit(_) | Ui::Delete(_) | Ui::React(..) => Task::none(),
            Ui::Copy(id) => {
                self.menu_for = None;
                match self.conversation.as_ref().and_then(|c| c.get(id)) {
                    Some(m) => iced::clipboard::write(m.text.clone()),
                    None => Task::none(),
                }
            }
            Ui::SaveMedia(chat, id) => {
                self.menu_for = None;
                let suggested = self
                    .conversation
                    .as_ref()
                    .and_then(|c| c.get(id))
                    .and_then(|m| match &m.media {
                        Some(Media::File { name, .. }) => Some(name.clone()),
                        Some(Media::Photo { .. }) => Some(format!("photo-{id}.jpg")),
                        _ => None,
                    })
                    .unwrap_or_else(|| format!("file-{id}"));
                Task::perform(
                    async move {
                        rfd::AsyncFileDialog::new()
                            .set_title("Save file")
                            .set_file_name(suggested)
                            .save_file()
                            .await
                            .map(|f| f.path().to_path_buf())
                    },
                    move |path| Message::Done(Done::SavePicked { chat, id, path }),
                )
            }
            Ui::CancelCompose => {
                if matches!(self.compose_mode, ComposeMode::Edit(_)) {
                    self.composer.clear();
                }
                self.compose_mode = ComposeMode::Normal;
                Task::none()
            }
            Ui::ToggleChatSearch => {
                if self.chat_search.take().is_none() {
                    self.chat_search = Some(ChatSearch::default());
                    return operation::focus(Id::new(CHAT_SEARCH_ID));
                }
                Task::none()
            }
            Ui::ChatSearchInput(q) => {
                if let Some(s) = &mut self.chat_search {
                    s.query = q;
                    s.submitted = false;
                }
                Task::none()
            }
            Ui::ChatSearchSubmit => {
                let (Some(s), Some(chat), Some(api)) =
                    (&mut self.chat_search, self.selected, self.api.clone())
                else {
                    return Task::none();
                };
                let query = s.query.trim().to_owned();
                if query.is_empty() {
                    return Task::none();
                }
                s.searching = true;
                s.submitted = true;
                info!(query = %Redacted(&query), "in-chat search");
                let store = self.store.clone();
                let q2 = query.clone();
                let local = Task::perform(
                    async move { store.call(move |s| s.search_local(chat, &q2, 50)).await },
                    {
                        let query = query.clone();
                        move |result| {
                            Message::Done(Done::SearchResults {
                                chat,
                                query: format!("local:{query}"),
                                result,
                            })
                        }
                    },
                );
                let q3 = query.clone();
                let remote =
                    Task::perform(async move { api.search(chat, q3).await }, move |result| {
                        Message::Done(Done::SearchResults {
                            chat,
                            query: query.clone(),
                            result,
                        })
                    });
                Task::batch([local, remote])
            }
            Ui::ScrollToBottom => {
                self.at_bottom = true;
                operation::snap_to(Id::new(HISTORY_ID), operation::RelativeOffset::START)
            }
            Ui::OpenMedia(chat, id) => {
                self.viewer = Some((chat, id));
                Task::none()
            }
            Ui::CloseMedia => {
                self.viewer = None;
                Task::none()
            }
            Ui::ToggleMainMenu => {
                self.main_menu = !self.main_menu;
                Task::none()
            }
            Ui::DismissToast => {
                self.toast = None;
                Task::none()
            }
            Ui::LogOut => {
                self.main_menu = false;
                let api = self.api.clone();
                let store = self.store.clone();
                Task::perform(
                    async move {
                        if let Some(api) = api {
                            let _ = api.log_out().await;
                        }
                        let _ = store.call(|s| s.clear()).await;
                    },
                    |_| Message::Done(Done::LoggedOut),
                )
            }
            Ui::Focus(f) => {
                self.focused = f;
                if f && let Some(chat) = self.selected {
                    let had_unread = self.chats.get(chat).is_some_and(|c| c.unread_count > 0);
                    self.chats.mark_read(chat);
                    if had_unread {
                        return self.api_call(move |api| {
                            Box::pin(async move { api.mark_read(chat).await })
                        });
                    }
                }
                Task::none()
            }
            Ui::Escape => {
                // Close the top-most layer only, like the official client.
                let closed = std::mem::take(&mut self.main_menu)
                    || self.viewer.take().is_some()
                    || self.menu_for.take().is_some()
                    || self.pending_file.take().is_some()
                    || std::mem::take(&mut self.emoji_open);
                if closed {
                    Task::none()
                } else if self.compose_mode != ComposeMode::Normal {
                    self.update_ui(Ui::CancelCompose)
                } else if self.chat_search.take().is_some() {
                    Task::none()
                } else if !self.sidebar_query.is_empty() {
                    self.update_ui(Ui::ClearSidebarSearch)
                } else {
                    Task::none()
                }
            }
            Ui::Tick => {
                if !self.first_frame_logged {
                    self.first_frame_logged = true;
                    let ms = self.started.elapsed().as_secs_f64() * 1000.0;
                    info!(first_frame_ms = format!("{ms:.1}"), "first frame");
                    eprintln!("first_frame_ms={ms:.1}");
                }
                if !self.config.mock {
                    self.now = chrono::Utc::now().timestamp();
                }
                Task::none()
            }
        }
    }

    fn send(&mut self) -> Task<Message> {
        let text = self.composer.trim().to_owned();
        let Some(chat) = self.selected else {
            return Task::none();
        };
        if text.is_empty() {
            return Task::none();
        }
        self.composer.clear();
        self.emoji_open = false;
        let mode = std::mem::replace(&mut self.compose_mode, ComposeMode::Normal);
        if let ComposeMode::Edit(id) = mode {
            let Some(conv) = &mut self.conversation else {
                return Task::none();
            };
            let Some(old) = conv.get(id).cloned() else {
                return Task::none();
            };
            conv.edit_local(id, &text);
            return self.api_call_or_rollback(old, move |api| {
                Box::pin(async move { api.edit_message(chat, id, text).await })
            });
        }
        let reply_to = match mode {
            ComposeMode::Reply(id) => Some(id),
            _ => None,
        };
        let mut pending = ChatMessage::text(chat, 0, true, self.now_ts(), &text);
        pending.reply_to = reply_to;
        pending.sender_id = self.me.as_ref().map(|u| u.id);
        let temp = match &mut self.conversation {
            Some(conv) => conv.push_pending(pending.clone()),
            None => return Task::none(),
        };
        pending.id = temp;
        pending.state = SendState::Sending;
        self.chats.set_last(&pending);
        self.at_bottom = true;
        let Some(api) = self.api.clone() else {
            return Task::done(Message::Done(Done::Sent {
                chat,
                temp,
                result: Err(Error::Net("not connected".into())),
            }));
        };
        Task::batch([
            Task::perform(
                async move { api.send_text(chat, text, reply_to).await },
                move |result| Message::Done(Done::Sent { chat, temp, result }),
            ),
            operation::snap_to(Id::new(HISTORY_ID), operation::RelativeOffset::START),
        ])
    }

    fn send_file(&mut self, file: PendingFile) -> Task<Message> {
        let Some(chat) = self.selected else {
            return Task::none();
        };
        let caption = std::mem::take(&mut self.composer);
        let reply_to = match std::mem::replace(&mut self.compose_mode, ComposeMode::Normal) {
            ComposeMode::Reply(id) => Some(id),
            _ => None,
        };
        let mut pending = ChatMessage::text(chat, 0, true, self.now_ts(), &caption);
        pending.reply_to = reply_to;
        pending.media = Some(Media::File {
            name: file.name.clone(),
            size: file.size,
            mime: String::new(),
        });
        let temp = match &mut self.conversation {
            Some(conv) => conv.push_pending(pending),
            None => return Task::none(),
        };
        let Some(api) = self.api.clone() else {
            return Task::none();
        };
        let PendingFile { path, kind, .. } = file;
        Task::perform(
            async move { api.send_file(chat, path, kind, caption, reply_to).await },
            move |result| Message::Done(Done::Sent { chat, temp, result }),
        )
    }

    fn now_ts(&self) -> i64 {
        if self.config.mock && self.config.screenshot.is_some() {
            self.now
        } else {
            chrono::Utc::now().timestamp()
        }
    }

    // ------------------------------------------------------------- events

    fn on_event(&mut self, ev: Event) -> Task<Message> {
        let open = self.selected.filter(|_| self.focused);
        let mut tasks = Vec::new();
        match &ev {
            Event::Connection(c) => {
                self.conn = *c;
                return Task::none();
            }
            Event::SessionRevoked => return self.session_revoked(),
            Event::NewMessage(m) => {
                if self.config.notifications
                    && state::should_notify(
                        self.chats.get(m.chat_id),
                        m,
                        self.selected,
                        self.focused,
                        chrono::Utc::now().timestamp(),
                    )
                {
                    let (title, body) = state::notification_text(
                        self.chats.get(m.chat_id),
                        m,
                        self.config.notification_preview,
                    );
                    crate::desktop::notify(title, body);
                }
                let m2 = m.clone();
                tasks.push(self.store_bg(move |s| s.upsert_messages(&[m2])));
                if open == Some(m.chat_id) && !m.outgoing {
                    let chat = m.chat_id;
                    tasks.push(
                        self.api_call(move |api| {
                            Box::pin(async move { api.mark_read(chat).await })
                        }),
                    );
                }
            }
            Event::MessageEdited(m) => {
                let m2 = m.clone();
                tasks.push(self.store_bg(move |s| s.upsert_messages(&[m2])));
            }
            Event::MessagesDeleted { chat, ids } => {
                let (chat, ids) = (*chat, ids.clone());
                tasks.push(self.store_bg(move |s| s.delete_messages(chat, &ids).map(|_| ())));
            }
            Event::Reactions {
                chat,
                id,
                reactions,
            } => {
                let (chat, id, r) = (*chat, *id, reactions.clone());
                tasks.push(self.store_bg(move |s| s.set_reactions(chat, id, &r)));
            }
            Event::ReadOutbox { chat, max_id } => {
                let (chat, max_id) = (*chat, *max_id);
                tasks.push(self.store_bg(move |s| s.set_read_outbox(chat, max_id)));
            }
            Event::ReadInbox { .. } => {}
        }
        self.chats.apply_event(&ev, open);
        if let Some(conv) = &mut self.conversation {
            conv.apply_event(&ev);
            if self.at_bottom {
                conv.trim();
            }
        }
        if matches!(ev, Event::NewMessage(_)) {
            tasks.push(self.fetch_photos());
        }
        Task::batch(tasks)
    }

    /// The server revoked our session: wipe secrets and cache off the UI
    /// thread, and build a fresh client for the next login.
    fn session_revoked(&mut self) -> Task<Message> {
        warn!("session revoked");
        let vault = Vault::new(self.secrets.clone(), self.config.session_blob_path());
        let store = self.store.clone();
        let wipe = Task::perform(
            async move {
                let _ = tokio::task::spawn_blocking(move || vault.clear()).await;
                store.call(|s| s.clear()).await
            },
            |r| Message::Done(Done::Call(r)),
        );
        self.reset_session();
        crate::auth::reduce(&mut self.auth, AuthEvent::LoggedOut);
        Task::batch([wipe, self.rebuild_api()])
    }

    /// Replace the client after logout/revoke so nothing from the old
    /// session (auth key, update pump, event channel) is reused.
    fn rebuild_api(&mut self) -> Task<Message> {
        if self.config.mock {
            // The fake backend supports logout → login on the same instance;
            // its event stream restarts on the next subscription.
            self.api_gen += 1;
            return Task::none();
        }
        self.api = None;
        self.start_real_api()
    }

    fn reset_session(&mut self) {
        self.me = None;
        self.chats = ChatList::default();
        self.selected = None;
        self.conversation = None;
        self.composer.clear();
        self.compose_mode = ComposeMode::Normal;
        self.pending_file = None;
        self.chat_search = None;
        self.sidebar_messages.clear();
        self.photos = Lru::new(IMAGE_BUDGET);
        self.photo_loading.clear();
    }

    fn on_done(&mut self, done: Done) -> Task<Message> {
        match done {
            Done::ApiReady(Ok(api)) => {
                self.api = Some(api.clone());
                self.api_gen += 1;
                self.setup = None;
                let notice = api.take_notice().map_or_else(Task::none, |n| self.toast(n));
                let restore = Task::perform(async move { api.restore().await }, |r| {
                    Message::Done(Done::Restored(r))
                });
                Task::batch([notice, restore])
            }
            Done::ApiReady(Err(Error::Auth(AuthError::MissingApiCredentials))) => {
                self.conn = ConnState::Disconnected;
                self.setup = Some(SetupForm::default());
                Task::none()
            }
            Done::ApiReady(Err(e)) => {
                warn!(error = %e, "telegram client unavailable");
                self.conn = ConnState::Disconnected;
                self.auth(AuthEvent::Restored(Err(e)))
            }
            Done::CredentialsSaved(Ok(())) => {
                self.setup = None;
                self.start_real_api()
            }
            Done::CredentialsSaved(Err(e)) => {
                if let Some(f) = &mut self.setup {
                    f.busy = false;
                    f.error = Some(e.user_message());
                }
                Task::none()
            }
            Done::Restored(Err(e)) if is_transient(&e) && self.auth == AuthState::Restoring => {
                // Offline with a possibly valid session: keep showing the
                // cache and retry with backoff; never fall back to the phone
                // screen (that would cost an SMS and risk FLOOD_WAIT).
                self.restore_attempt += 1;
                self.conn = ConnState::Reconnecting {
                    attempt: self.restore_attempt,
                };
                let delay = backoff::delay(self.restore_attempt, backoff::jitter());
                warn!(attempt = self.restore_attempt, "restore failed, retrying");
                Task::perform(async move { tokio::time::sleep(delay).await }, |_| {
                    Message::Done(Done::RetryRestore)
                })
            }
            Done::RetryRestore => {
                let Some(api) = self.api.clone() else {
                    return Task::none();
                };
                self.conn = ConnState::Connecting;
                Task::perform(async move { api.restore().await }, |r| {
                    Message::Done(Done::Restored(r))
                })
            }
            Done::Restored(r) => {
                let user = r.as_ref().ok().cloned().flatten();
                if let Err(e) = &r
                    && e.is_session_revoked()
                {
                    return self.session_revoked();
                }
                let mut t = Task::none();
                if matches!(r, Ok(None)) && !self.chats.all().is_empty() {
                    // Definitely logged out: drop the stale cache.
                    self.reset_session();
                    t = self.store_bg(|s| s.clear());
                }
                let t = Task::batch([t, self.auth(AuthEvent::Restored(r))]);
                if let Some(u) = user {
                    return Task::batch([t, self.on_signed_in(Some(u))]);
                }
                if !self.auth.is_signed_in() {
                    self.conn = ConnState::Connected;
                    if self.config.screenshot.is_none() {
                        return Task::batch([t, operation::focus(Id::new(AUTH_INPUT_ID))]);
                    }
                }
                t
            }
            Done::CodeRequested(r) => {
                let ok = r.is_ok();
                let t = self.auth(AuthEvent::CodeRequested(r));
                if ok {
                    Task::batch([t, operation::focus(Id::new(AUTH_INPUT_ID))])
                } else {
                    t
                }
            }
            Done::SignIn(r) => {
                let user = match &r {
                    Ok(SignInOutcome::SignedIn(u)) => Some(u.clone()),
                    _ => None,
                };
                let t = self.auth(AuthEvent::SignIn(r));
                match user {
                    Some(u) if self.auth.is_signed_in() => {
                        Task::batch([t, self.on_signed_in(Some(u))])
                    }
                    _ => Task::batch([t, operation::focus(Id::new(AUTH_INPUT_ID))]),
                }
            }
            Done::Persisted(r) => match r {
                Err(e) => self.toast(e.user_message()),
                Ok(()) => Task::none(),
            },
            Done::Dialogs(Ok(chats)) => {
                let to_store = chats.clone();
                self.chats.replace(chats);
                self.store_bg(move |s| s.replace_chats(&to_store).map(|_| ()))
            }
            Done::Dialogs(Err(e)) => self.on_error(e),
            Done::CacheHistory {
                chat,
                before,
                complete,
                result,
            } => {
                let Some(conv) = self.conversation.as_mut().filter(|c| c.chat_id == chat) else {
                    return Task::none();
                };
                let page = result.unwrap_or_default();
                let short = page.len() < PAGE;
                conv.merge(page);
                match before {
                    // Initial open: the network request is already in flight.
                    None => self.fetch_photos(),
                    // Older page proven complete by the contiguity range.
                    Some(_) if complete => {
                        conv.loading_older = false;
                        if short {
                            conv.reached_start = true;
                        }
                        self.fetch_photos()
                    }
                    // Gap or unknown range: ask the network.
                    Some(b) => self.net_history(chat, Some(b)),
                }
            }
            Done::NetHistory {
                chat,
                before,
                result,
            } => {
                let Some(conv) = self.conversation.as_mut().filter(|c| c.chat_id == chat) else {
                    return Task::none();
                };
                conv.loading_older = false;
                match result {
                    Ok(page) => {
                        if page.len() < PAGE {
                            conv.reached_start = true;
                        }
                        let to_store = page.clone();
                        conv.merge(page);
                        Task::batch([
                            self.store_bg(move |s| {
                                s.record_network_page(chat, before, &to_store, PAGE)
                            }),
                            self.fetch_photos(),
                        ])
                    }
                    Err(e) => self.on_error(e),
                }
            }
            Done::Sent { chat, temp, result } => {
                let ok = result.as_ref().ok().cloned();
                if let Some(conv) = self.conversation.as_mut().filter(|c| c.chat_id == chat) {
                    conv.resolve_pending(temp, ok.clone());
                }
                match result {
                    Ok(m) => {
                        self.chats.set_last(&m);
                        let t = self.fetch_photos();
                        Task::batch([t, self.store_bg(move |s| s.upsert_messages(&[m]))])
                    }
                    Err(e) => self.on_error(e),
                }
            }
            Done::Rollback { old, error } => {
                if let Some(conv) = self
                    .conversation
                    .as_mut()
                    .filter(|c| c.chat_id == old.chat_id)
                {
                    conv.merge(vec![old]);
                }
                self.on_error(error)
            }
            Done::Call(Ok(())) => Task::none(),
            Done::Call(Err(e)) => self.on_error(e),
            Done::SidebarResults { query, result } => {
                // Drop results for a query the user has already changed.
                if query == self.sidebar_query {
                    self.sidebar_messages = result.unwrap_or_default();
                }
                Task::none()
            }
            Done::SearchResults {
                chat,
                query,
                result,
            } => {
                let Some(s) = &mut self.chat_search else {
                    return Task::none();
                };
                if self.selected != Some(chat) {
                    return Task::none();
                }
                if let Some(local) = query.strip_prefix("local:") {
                    // Instant results from the cache, replaced by the server's.
                    if s.searching && local == s.query.trim() {
                        s.results = result.unwrap_or_default();
                    }
                    return Task::none();
                }
                if query == s.query.trim() {
                    s.searching = false;
                    match result {
                        Ok(r) => s.results = r,
                        Err(e) => return self.on_error(e),
                    }
                }
                Task::none()
            }
            Done::FileStat { path, result } => match result {
                Ok((_, true)) => self.toast("Folders can't be sent. Drop a file instead.".into()),
                Ok((size, false)) => {
                    self.pending_file = Some(PendingFile {
                        name: path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "file".into()),
                        kind: UploadKind::for_path(&path),
                        size,
                        path,
                    });
                    operation::focus(Id::new(COMPOSER_ID))
                }
                Err(e) => self.on_error(e),
            },
            Done::SavePicked {
                chat,
                id,
                path: Some(path),
            } => {
                let Some(api) = self.api.clone() else {
                    return Task::none();
                };
                Task::perform(
                    async move { api.download_media(chat, id, path).await },
                    |r| Message::Done(Done::MediaSaved(r)),
                )
            }
            Done::SavePicked { path: None, .. } => Task::none(),
            Done::MediaSaved(Ok(())) => self.toast("File saved.".into()),
            Done::MediaSaved(Err(e)) => self.on_error(e),
            Done::Photo { key, result } => {
                self.photo_loading.remove(&key);
                if let Ok(bytes) = result {
                    if !state::photo_is_acceptable(&bytes) {
                        warn!(bytes = bytes.len(), "photo rejected by size/pixel cap");
                        return Task::none();
                    }
                    // Budget by the decoded size when known.
                    let size = state::image_dimensions(&bytes)
                        .map_or(bytes.len() * 6, |(w, h)| (w as usize) * (h as usize) * 4);
                    self.photos
                        .insert(key, image::Handle::from_bytes(bytes), size);
                }
                Task::none()
            }
            Done::LoggedOut => {
                self.reset_session();
                self.auth = AuthState::Phone {
                    input: String::new(),
                    error: None,
                    busy: false,
                };
                self.rebuild_api()
            }
            Done::Screenshot(shot) => screenshot::save(&self.config, shot),
            Done::ScreenshotSaved => iced::exit(),
        }
    }

    /// Per-call failures become a toast; they never change the connection
    /// state (only the update stream and restore do), so a single failed
    /// request cannot leave a stale "Waiting for network…" banner.
    fn on_error(&mut self, e: Error) -> Task<Message> {
        if e.is_session_revoked() {
            return self.session_revoked();
        }
        warn!(error = %e, "operation failed");
        self.toast(e.user_message())
    }
}

fn open_memory_store() -> Store {
    match Store::open_in_memory() {
        Ok(s) => s,
        Err(e) => {
            // In-memory SQLite failing means the process is unusable anyway.
            eprintln!("fatal: cannot open in-memory store: {e}");
            std::process::exit(1)
        }
    }
}
