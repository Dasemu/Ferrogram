//! Real Telegram client on top of `grammers-client` (pure-Rust MTProto).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use grammers_client::client::{LoginToken, PasswordToken};
use grammers_client::message::{InputMessage, InputReactions};
use grammers_client::peer::Peer;
use grammers_client::sender::UpdatesConfiguration;
use grammers_client::{Client, InvocationError, SenderPool, SignInError};
use grammers_session::types::{PeerId, PeerRef};
use secrecy::{ExposeSecret, SecretString};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::session::EncryptedSession;
use super::types::{self, dialog_info, event_from_update, message_from_tl};
use super::{Event, TelegramApi, UploadKind, backoff};
use crate::auth::SignInOutcome;
use crate::auth::vault::Vault;
use crate::config::ApiCredentials;
use crate::error::{AuthError, Error, Phone, Result};
use crate::model::{Chat, ChatId, ChatKind, ConnState, Message, MessageId, User};

type UpdatesRx = mpsc::Receiver<grammers_session::updates::UpdatesLike>;

pub struct GrammersApi {
    client: Client,
    api_hash: SecretString,
    session: Arc<EncryptedSession>,
    updates_rx: Mutex<Option<UpdatesRx>>,
    login_token: Mutex<Option<LoginToken>>,
    password_token: Mutex<Option<PasswordToken>>,
    peers: Mutex<HashMap<ChatId, PeerRef>>,
    self_id: AtomicI64,
    events_tx: mpsc::UnboundedSender<Event>,
    events_rx: Mutex<Option<mpsc::UnboundedReceiver<Event>>>,
}

fn lock<T>(m: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>> {
    m.lock().map_err(|_| Error::Net("lock poisoned".into()))
}

/// Map grammers errors to crate errors without leaking payloads.
pub fn map_err(e: InvocationError) -> Error {
    match e {
        InvocationError::Rpc(rpc) => types::rpc_error(rpc.code, &rpc.name, rpc.value),
        InvocationError::Io(e) => Error::Net(format!("io: {}", e.kind())),
        InvocationError::Transport(_) => Error::Net("transport".into()),
        InvocationError::Deserialize(_) => Error::Protocol("deserialize".into()),
        InvocationError::Dropped(_) => Error::Cancelled,
        _ => Error::Net("invocation".into()),
    }
}

fn map_sign_in(e: SignInError) -> Error {
    match e {
        SignInError::InvalidCode => Error::Auth(AuthError::InvalidCode),
        SignInError::InvalidPassword(_) => Error::Auth(AuthError::InvalidPassword),
        SignInError::SignUpRequired => Error::Auth(AuthError::SignUpRequired),
        SignInError::PasswordRequired(_) => Error::Protocol("unexpected password request".into()),
        SignInError::Other(e) => map_err(e),
        #[allow(unreachable_patterns)]
        _ => Error::Protocol("sign-in".into()),
    }
}

fn user_from(u: &grammers_client::peer::User) -> User {
    User {
        id: u.id().bare_id_unchecked(),
        name: u.full_name(),
        username: u.username().map(str::to_owned),
    }
}

fn chat_kind(peer: &Peer, self_id: i64) -> ChatKind {
    match peer {
        Peer::User(u) if u.id().bare_id_unchecked() == self_id => ChatKind::SavedMessages,
        Peer::User(u) if u.is_bot() => ChatKind::Bot,
        Peer::User(_) => ChatKind::User,
        Peer::Group(_) => ChatKind::Group,
        Peer::Channel(_) | Peer::Community(_) => ChatKind::Channel,
    }
}

impl GrammersApi {
    /// Create the client and spawn the sender pool. Must run inside a tokio
    /// runtime (it does: iced tasks run on the tokio executor).
    pub fn start(creds: ApiCredentials, vault: Vault) -> Result<Arc<Self>> {
        let session = Arc::new(EncryptedSession::load(vault)?);
        let SenderPool {
            runner,
            handle,
            updates,
        } = SenderPool::new(Arc::clone(&session), creds.api_id);
        let client = Client::new(handle);
        tokio::spawn(runner.run());
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let api = Arc::new(Self {
            client,
            api_hash: creds.api_hash,
            session: Arc::clone(&session),
            updates_rx: Mutex::new(Some(updates)),
            login_token: Mutex::new(None),
            password_token: Mutex::new(None),
            peers: Mutex::new(HashMap::new()),
            self_id: AtomicI64::new(0),
            events_tx,
            events_rx: Mutex::new(Some(events_rx)),
        });
        // Periodically flush the (encrypted) session so update state survives crashes.
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(20)).await;
                let s = Arc::clone(&session);
                let _ = tokio::task::spawn_blocking(move || s.save_if_dirty()).await;
            }
        });
        Ok(api)
    }

    fn emit(&self, ev: Event) {
        let _ = self.events_tx.send(ev);
    }

    async fn peer_ref(&self, chat: ChatId) -> Result<PeerRef> {
        if let Some(p) = lock(&self.peers)?.get(&chat).copied() {
            return Ok(p);
        }
        let id = PeerId::from_bot_api_dialog_id(chat)
            .ok_or_else(|| Error::Protocol("bad chat id".into()))?;
        // Fall back to the session cache (filled by grammers from any response).
        use grammers_session::Session as _;
        match self.session.peer_ref(id).await {
            Ok(Some(p)) => Ok(p),
            _ => Err(Error::Protocol("unknown peer".into())),
        }
    }

    fn convert(&self, m: &grammers_client::message::Message) -> Option<Message> {
        let mut out = message_from_tl(&m.raw)?;
        if let Some(sender) = m.sender() {
            if out.sender_name.is_none() {
                out.sender_name = sender.name().map(str::to_owned);
            }
        }
        Some(out)
    }

    /// Start pumping the update stream into `events_tx` (once).
    async fn start_updates(self: &Arc<Self>) {
        let Ok(mut guard) = self.updates_rx.lock() else {
            return;
        };
        let Some(rx) = guard.take() else {
            return;
        };
        drop(guard);
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let mut stream = match me
                .client
                .stream_updates(rx, UpdatesConfiguration { catch_up: true })
                .await
            {
                Ok(s) => s,
                Err(_) => {
                    warn!("could not start update stream");
                    me.emit(Event::Connection(ConnState::Disconnected));
                    return;
                }
            };
            me.emit(Event::Connection(ConnState::Connected));
            let mut attempt = 0u32;
            loop {
                match stream.next().await {
                    Ok(update) => {
                        if attempt > 0 {
                            attempt = 0;
                            me.emit(Event::Connection(ConnState::Connected));
                        }
                        let event = match &update {
                            grammers_client::update::Update::NewMessage(m) => {
                                me.convert(m).map(Event::NewMessage)
                            }
                            grammers_client::update::Update::MessageEdited(m) => {
                                me.convert(m).map(Event::MessageEdited)
                            }
                            other => event_from_update(other.raw()),
                        };
                        if let Some(ev) = event {
                            me.emit(ev);
                        }
                    }
                    Err(e) => {
                        let err = map_err(e);
                        if err.is_session_revoked() {
                            me.emit(Event::SessionRevoked);
                            return;
                        }
                        if matches!(err, Error::Cancelled) {
                            return;
                        }
                        attempt += 1;
                        me.emit(Event::Connection(ConnState::Reconnecting { attempt }));
                        tokio::time::sleep(backoff::delay(attempt, backoff::jitter())).await;
                    }
                }
            }
        });
    }

    async fn after_login(self: &Arc<Self>, user: &User) {
        self.self_id.store(user.id, Ordering::Release);
        self.start_updates().await;
    }

    fn outcome_user(&self, u: &grammers_client::peer::User) -> SignInOutcome {
        let user = user_from(u);
        self.self_id.store(user.id, Ordering::Release);
        SignInOutcome::SignedIn(user)
    }
}

/// `Arc` wrapper so `TelegramApi` methods can spawn work that outlives `&self`.
pub struct Grammers(pub Arc<GrammersApi>);

#[async_trait]
impl TelegramApi for Grammers {
    async fn restore(&self) -> Result<Option<User>> {
        let api = &self.0;
        if !api.client.is_authorized().await.map_err(map_err)? {
            return Ok(None);
        }
        let me = api.client.get_me().await.map_err(map_err)?;
        let user = user_from(&me);
        api.after_login(&user).await;
        info!("session restored");
        Ok(Some(user))
    }

    async fn request_code(&self, phone: Phone) -> Result<()> {
        let api = &self.0;
        let token = api
            .client
            .request_login_code(&phone.normalized(), api.api_hash.expose_secret())
            .await
            .map_err(map_err)?;
        *lock(&api.login_token)? = Some(token);
        debug!(phone = %phone, "login code requested");
        Ok(())
    }

    async fn sign_in(&self, code: String) -> Result<SignInOutcome> {
        let api = &self.0;
        let token = lock(&api.login_token)?
            .take()
            .ok_or_else(|| Error::Auth(AuthError::InvalidCode))?;
        match api.client.sign_in(&token, &code).await {
            Ok(u) => {
                let out = api.outcome_user(&u);
                if let SignInOutcome::SignedIn(user) = &out {
                    api.after_login(user).await;
                }
                Ok(out)
            }
            Err(SignInError::PasswordRequired(pt)) => {
                let hint = pt.hint().map(str::to_owned);
                *lock(&api.password_token)? = Some(pt);
                Ok(SignInOutcome::PasswordRequired { hint })
            }
            Err(SignInError::InvalidCode) => {
                // Keep the token so the user can retry the code.
                *lock(&api.login_token)? = Some(token);
                Err(Error::Auth(AuthError::InvalidCode))
            }
            Err(e) => Err(map_sign_in(e)),
        }
    }

    async fn check_password(&self, password: SecretString) -> Result<SignInOutcome> {
        let api = &self.0;
        let token = lock(&api.password_token)?
            .take()
            .ok_or_else(|| Error::Auth(AuthError::InvalidPassword))?;
        // SRP is computed by grammers (`check_password`).
        match api
            .client
            .check_password(token, password.expose_secret().as_bytes())
            .await
        {
            Ok(u) => {
                let out = api.outcome_user(&u);
                if let SignInOutcome::SignedIn(user) = &out {
                    api.after_login(user).await;
                }
                Ok(out)
            }
            Err(SignInError::InvalidPassword(pt)) => {
                *lock(&api.password_token)? = Some(pt);
                Err(Error::Auth(AuthError::InvalidPassword))
            }
            Err(e) => Err(map_sign_in(e)),
        }
    }

    async fn persist_session(&self) -> Result<()> {
        let s = Arc::clone(&self.0.session);
        tokio::task::spawn_blocking(move || s.save())
            .await
            .map_err(|_| Error::Cancelled)?
    }

    async fn log_out(&self) -> Result<()> {
        let _ = self.0.client.sign_out().await;
        let s = Arc::clone(&self.0.session);
        tokio::task::spawn_blocking(move || s.clear())
            .await
            .map_err(|_| Error::Cancelled)?
    }

    async fn dialogs(&self, limit: usize) -> Result<Vec<Chat>> {
        let api = &self.0;
        let self_id = api.self_id.load(Ordering::Acquire);
        let now = chrono::Utc::now().timestamp();
        let mut iter = api.client.iter_dialogs();
        let mut out = Vec::new();
        while out.len() < limit {
            let Some(d) = iter.next().await.map_err(map_err)? else {
                break;
            };
            let Some(chat_id) = d.peer_id().bot_api_dialog_id() else {
                continue;
            };
            if let Ok(Some(r)) = d.peer().to_ref().await {
                lock(&api.peers)?.insert(chat_id, r);
            }
            let kind = chat_kind(d.peer(), self_id);
            let title = if kind == ChatKind::SavedMessages {
                "Saved Messages".to_owned()
            } else {
                d.peer().name().unwrap_or("Deleted Account").to_owned()
            };
            let info = dialog_info(&d.raw, now);
            let mut chat = Chat::new(chat_id, kind, title);
            chat.unread_count = info.unread;
            chat.pinned = info.pinned;
            chat.muted = info.muted;
            chat.read_inbox_max_id = info.read_inbox;
            chat.read_outbox_max_id = info.read_outbox;
            chat.last_message = d.last_message.as_ref().and_then(|m| api.convert(m));
            chat.last_date = chat.last_message.as_ref().map_or(0, |m| m.date);
            out.push(chat);
        }
        Ok(out)
    }

    async fn history(
        &self,
        chat: ChatId,
        before: Option<MessageId>,
        limit: usize,
    ) -> Result<Vec<Message>> {
        let api = &self.0;
        let peer = api.peer_ref(chat).await?;
        let mut iter = api.client.iter_messages(peer).limit(limit);
        if let Some(b) = before {
            iter = iter.offset_id(b);
        }
        let mut out = Vec::with_capacity(limit);
        while let Some(m) = iter.next().await.map_err(map_err)? {
            if let Some(m) = api.convert(&m) {
                out.push(m);
            }
        }
        out.reverse();
        Ok(out)
    }

    async fn send_text(
        &self,
        chat: ChatId,
        text: String,
        reply_to: Option<MessageId>,
    ) -> Result<Message> {
        let api = &self.0;
        let peer = api.peer_ref(chat).await?;
        let sent = api
            .client
            .send_message(peer, InputMessage::new().text(text).reply_to(reply_to))
            .await
            .map_err(map_err)?;
        api.convert(&sent)
            .ok_or_else(|| Error::Protocol("empty sent message".into()))
    }

    async fn send_file(
        &self,
        chat: ChatId,
        path: PathBuf,
        kind: UploadKind,
        caption: String,
        reply_to: Option<MessageId>,
    ) -> Result<Message> {
        let api = &self.0;
        let peer = api.peer_ref(chat).await?;
        let uploaded = api.client.upload_file(&path).await?;
        let msg = InputMessage::new().text(caption).reply_to(reply_to);
        let msg = match kind {
            UploadKind::Photo => msg.photo(uploaded),
            UploadKind::Document => msg.document(uploaded),
        };
        let sent = api.client.send_message(peer, msg).await.map_err(map_err)?;
        api.convert(&sent)
            .ok_or_else(|| Error::Protocol("empty sent message".into()))
    }

    async fn edit_message(&self, chat: ChatId, id: MessageId, text: String) -> Result<()> {
        let api = &self.0;
        let peer = api.peer_ref(chat).await?;
        api.client
            .edit_message(peer, id, InputMessage::new().text(text))
            .await
            .map_err(map_err)
    }

    async fn delete_messages(&self, chat: ChatId, ids: Vec<MessageId>) -> Result<()> {
        let api = &self.0;
        let peer = api.peer_ref(chat).await?;
        api.client
            .delete_messages(peer, &ids)
            .await
            .map_err(map_err)?;
        Ok(())
    }

    async fn react(&self, chat: ChatId, id: MessageId, emoji: Option<String>) -> Result<()> {
        let api = &self.0;
        let peer = api.peer_ref(chat).await?;
        let r = match emoji {
            Some(e) => InputReactions::emoticon(e),
            None => InputReactions::remove(),
        };
        api.client
            .send_reactions(peer, id, r)
            .await
            .map_err(map_err)
    }

    async fn mark_read(&self, chat: ChatId) -> Result<()> {
        let api = &self.0;
        let peer = api.peer_ref(chat).await?;
        api.client.mark_as_read(peer).await.map_err(map_err)
    }

    async fn search(&self, chat: ChatId, query: String) -> Result<Vec<Message>> {
        let api = &self.0;
        let peer = api.peer_ref(chat).await?;
        let mut iter = api.client.search_messages(peer).query(&query).limit(50);
        let mut out = Vec::new();
        while let Some(m) = iter.next().await.map_err(map_err)? {
            if let Some(m) = api.convert(&m) {
                out.push(m);
            }
        }
        Ok(out)
    }

    async fn download_photo(&self, chat: ChatId, id: MessageId) -> Result<Vec<u8>> {
        let api = &self.0;
        let peer = api.peer_ref(chat).await?;
        let msgs = api
            .client
            .get_messages_by_id(peer, &[id])
            .await
            .map_err(map_err)?;
        let photo = msgs
            .into_iter()
            .flatten()
            .next()
            .and_then(|m| m.photo())
            .ok_or_else(|| Error::Protocol("no photo".into()))?;
        let mut dl = api.client.iter_download(&photo);
        let mut bytes = Vec::new();
        while let Some(chunk) = dl.next().await.map_err(map_err)? {
            bytes.extend_from_slice(&chunk);
            if bytes.len() > 20 * 1024 * 1024 {
                return Err(Error::Protocol("photo too large".into()));
            }
        }
        Ok(bytes)
    }

    fn events(&self) -> BoxStream<'static, Event> {
        let rx = self.0.events_rx.lock().ok().and_then(|mut g| g.take());
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
