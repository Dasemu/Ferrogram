//! Real Telegram client on top of `grammers-client` (pure-Rust MTProto).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
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
use super::types::{self, dialog_info, event_from_update_as, message_from_tl, presence_from_tl};
use super::{Event, EventHub, TelegramApi, UploadKind, backoff};
use crate::auth::SignInOutcome;
use crate::auth::vault::Vault;
use crate::config::ApiCredentials;
use crate::error::{AuthError, Error, Phone, Result};
use crate::model::{
    Chat, ChatId, ChatKind, ConnState, Folder, GlobalHit, Message, MessageId, PeerInfo, Presence,
    User,
};

type UpdatesRx = mpsc::Receiver<grammers_session::updates::UpdatesLike>;

pub struct GrammersApi {
    client: Client,
    api_hash: SecretString,
    session: Arc<EncryptedSession>,
    updates_rx: Mutex<Option<UpdatesRx>>,
    login_token: Mutex<Option<LoginToken>>,
    password_token: Mutex<Option<PasswordToken>>,
    peers: Mutex<HashMap<ChatId, PeerRef>>,
    /// `InputPeer`s (with access hashes) named by the account's folders, so a
    /// folder can be saved again even for chats outside the loaded list.
    folder_peers: Mutex<HashMap<ChatId, grammers_client::tl::enums::InputPeer>>,
    /// Where to download the preview of the photos/videos a shared-media
    /// page listed (bounded FIFO), so no extra lookup per thumbnail.
    thumbs:
        Mutex<std::collections::VecDeque<((ChatId, MessageId), grammers_client::media::PhotoSize)>>,
    /// The dialog iterator survives between calls so "load more" continues
    /// from the offset instead of re-reading the list.
    dialogs_iter: tokio::sync::Mutex<Option<grammers_client::client::DialogIter>>,
    /// Last `messages.dhConfig` (for `dhConfigNotModified`).
    dh: Mutex<Option<super::calls::dh::DhConfig>>,
    self_id: AtomicI64,
    events: EventHub,
    /// Set when the stored session was unreadable and had to be discarded.
    session_reset: std::sync::atomic::AtomicBool,
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

/// Id of the peer's current profile photo (`None` = no photo).
fn peer_photo_id(peer: &Peer) -> Option<i64> {
    match peer {
        Peer::User(u) => u.photo().map(|p| p.photo_id),
        Peer::Group(g) => g.photo().map(|p| p.photo_id),
        Peer::Channel(c) => c.photo().map(|p| p.photo_id),
        Peer::Community(c) => c.photo().map(|p| p.photo_id),
    }
}

/// Is this user in our contacts?
fn peer_is_contact(peer: &Peer) -> bool {
    match peer {
        Peer::User(u) => match &u.raw {
            grammers_client::tl::enums::User::User(x) => x.contact,
            _ => false,
        },
        _ => false,
    }
}

fn peer_presence(peer: &Peer, now: i64) -> Presence {
    match peer {
        Peer::User(u) => match &u.raw {
            grammers_client::tl::enums::User::User(x) => x
                .status
                .as_ref()
                .map_or(Presence::Unknown, |s| presence_from_tl(s, now)),
            _ => Presence::Unknown,
        },
        _ => Presence::Unknown,
    }
}

impl GrammersApi {
    /// Create the client and spawn the sender pool. Must run inside a tokio
    /// runtime (it does: iced tasks run on the tokio executor). The keyring
    /// read and vault decryption run on a blocking thread.
    pub async fn start(creds: ApiCredentials, vault: Vault) -> Result<Arc<Self>> {
        let (session, reset) =
            tokio::task::spawn_blocking(move || EncryptedSession::load_or_reset(vault))
                .await
                .map_err(|_| Error::Cancelled)??;
        let session = Arc::new(session);
        let SenderPool {
            runner,
            handle,
            updates,
        } = SenderPool::new(Arc::clone(&session), creds.api_id);
        let client = Client::new(handle);
        tokio::spawn(runner.run());
        let api = Arc::new(Self {
            client,
            api_hash: creds.api_hash,
            session: Arc::clone(&session),
            updates_rx: Mutex::new(Some(updates)),
            login_token: Mutex::new(None),
            password_token: Mutex::new(None),
            peers: Mutex::new(HashMap::new()),
            folder_peers: Mutex::new(HashMap::new()),
            thumbs: Mutex::new(std::collections::VecDeque::new()),
            dialogs_iter: tokio::sync::Mutex::new(None),
            dh: Mutex::new(None),
            self_id: AtomicI64::new(0),
            events: EventHub::new(),
            session_reset: std::sync::atomic::AtomicBool::new(reset),
        });
        // Periodically flush the (encrypted) session so update state survives crashes.
        // The flusher stops once the session is cleared (logout/revoke) so a
        // stale task can never write the old auth key back.
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(20)).await;
                if session.is_closed() {
                    break;
                }
                let s = Arc::clone(&session);
                let _ = tokio::task::spawn_blocking(move || s.save_if_dirty()).await;
            }
        });
        Ok(api)
    }

    /// Pull up to `limit` dialogs from the persistent iterator.
    async fn take_dialogs(&self, limit: usize) -> Result<Vec<Chat>> {
        let self_id = self.self_id.load(Ordering::Acquire);
        let now = chrono::Utc::now().timestamp();
        let mut guard = self.dialogs_iter.lock().await;
        let iter = guard.get_or_insert_with(|| self.client.iter_dialogs());
        let mut out = Vec::new();
        while out.len() < limit {
            let Some(d) = iter.next().await.map_err(map_err)? else {
                break;
            };
            let Some(chat_id) = d.peer_id().bot_api_dialog_id() else {
                continue;
            };
            if let Ok(Some(r)) = d.peer().to_ref().await {
                lock(&self.peers)?.insert(chat_id, r);
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
            chat.photo_id = peer_photo_id(d.peer());
            chat.presence = peer_presence(d.peer(), now);
            chat.contact = peer_is_contact(d.peer());
            chat.draft = info.draft;
            chat.last_message = d.last_message.as_ref().and_then(|m| self.convert(m));
            chat.last_date = chat.last_message.as_ref().map_or(0, |m| m.date);
            out.push(chat);
        }
        Ok(out)
    }

    fn remember_folder_peers(&self, peers: Vec<(ChatId, grammers_client::tl::enums::InputPeer)>) {
        if let Ok(mut m) = self.folder_peers.lock() {
            // Bounded: a folder holds at most a few hundred chats.
            if m.len() > 4096 {
                m.clear();
            }
            m.extend(peers);
        }
    }

    fn resolve_input_peer(&self, chat: ChatId) -> Option<grammers_client::tl::enums::InputPeer> {
        if let Some(p) = self.peers.lock().ok()?.get(&chat).copied() {
            return Some(p.into());
        }
        self.folder_peers.lock().ok()?.get(&chat).cloned()
    }

    fn remember_thumb(&self, key: (ChatId, MessageId), size: grammers_client::media::PhotoSize) {
        const MAX: usize = 800;
        if let Ok(mut q) = self.thumbs.lock() {
            if q.len() >= MAX {
                q.pop_front();
            }
            q.push_back((key, size));
        }
    }

    fn thumb_for(&self, key: (ChatId, MessageId)) -> Option<grammers_client::media::PhotoSize> {
        self.thumbs
            .lock()
            .ok()?
            .iter()
            .rev()
            .find(|(k, _)| *k == key)
            .map(|(_, s)| s.clone())
    }

    fn emit(&self, ev: Event) {
        self.events.emit(ev);
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
        if let Some(sender) = m.sender()
            && out.sender_name.is_none()
        {
            out.sender_name = sender.name().map(str::to_owned);
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
                            other => {
                                let self_id = me.self_id.load(Ordering::Acquire);
                                if let grammers_client::tl::enums::Update::DialogFilter(u) =
                                    other.raw()
                                    && let Some(f) = &u.filter
                                    && let Some((_, peers)) = types::folder_from_tl(f, self_id)
                                {
                                    me.remember_folder_peers(peers);
                                }
                                event_from_update_as(other.raw(), self_id)
                            }
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

/// The preview of a photo / video / GIF to show in the grid: the 320 px "m"
/// size, else the 800 px "x" or 100 px "s" one; only sizes that can be shown
/// (not the stripped or vector placeholders).
fn thumb_size(media: &grammers_client::media::Media) -> Option<grammers_client::media::PhotoSize> {
    use grammers_client::media::{Media, PhotoSize};
    let sizes = match media {
        Media::Photo(p) => p.thumbs(),
        Media::Document(d) => d.thumbs(),
        _ => return None,
    };
    let usable = |s: &&PhotoSize| {
        matches!(
            s,
            PhotoSize::Size(_) | PhotoSize::Progressive(_) | PhotoSize::Cached(_)
        )
    };
    ["m", "x", "s"]
        .iter()
        .find_map(|t| sizes.iter().filter(usable).find(|s| s.photo_type() == *t))
        .or_else(|| sizes.iter().find(usable))
        .cloned()
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
            .ok_or(Error::Auth(AuthError::InvalidCode))?;
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
            .ok_or(Error::Auth(AuthError::InvalidPassword))?;
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
        // Drop the connection and the in-memory auth key; the app builds a
        // brand-new client for the next login.
        self.0.client.disconnect();
        let s = Arc::clone(&self.0.session);
        tokio::task::spawn_blocking(move || s.clear())
            .await
            .map_err(|_| Error::Cancelled)?
    }

    async fn dialogs(&self, limit: usize) -> Result<Vec<Chat>> {
        let api = &self.0;
        *api.dialogs_iter.lock().await = Some(api.client.iter_dialogs());
        api.take_dialogs(limit).await
    }

    async fn more_dialogs(&self, limit: usize) -> Result<Vec<Chat>> {
        self.0.take_dialogs(limit).await
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
        // Prefer the 800 px ("x") or 320 px ("m") thumbnail over the original.
        let thumbs = photo.thumbs();
        let thumb = ["x", "m"].iter().find_map(|t| {
            thumbs.iter().find(|s| {
                s.photo_type() == *t
                    && matches!(
                        s,
                        grammers_client::media::PhotoSize::Size(_)
                            | grammers_client::media::PhotoSize::Progressive(_)
                    )
            })
        });
        let mut dl = match thumb {
            Some(t) => api.client.iter_download(t),
            None => api.client.iter_download(&photo),
        };
        let mut bytes = Vec::new();
        while let Some(chunk) = dl.next().await.map_err(map_err)? {
            bytes.extend_from_slice(&chunk);
            if bytes.len() > super::MAX_PHOTO_BYTES {
                return Err(Error::Protocol("photo too large".into()));
            }
        }
        Ok(bytes)
    }

    async fn search_global(&self, query: String, limit: usize) -> Result<Vec<GlobalHit>> {
        let api = &self.0;
        let self_id = api.self_id.load(Ordering::Acquire);
        let mut iter = api.client.search_all_messages().query(&query).limit(limit);
        let mut out = Vec::new();
        while let Some(m) = iter.next().await.map_err(map_err)? {
            let Some(message) = api.convert(&m) else {
                continue;
            };
            let Some(peer) = m.peer() else {
                continue;
            };
            // Remember the peer so the chat can be opened from the hit.
            if let Ok(Some(r)) = m.peer_ref().await {
                lock(&api.peers)?.insert(message.chat_id, r);
            }
            let kind = chat_kind(peer, self_id);
            let chat_title = if kind == ChatKind::SavedMessages {
                "Saved Messages".to_owned()
            } else {
                peer.name().unwrap_or("Deleted Account").to_owned()
            };
            out.push(GlobalHit {
                message,
                chat_title,
                chat_kind: kind,
                photo_id: peer_photo_id(peer),
            });
        }
        Ok(out)
    }

    async fn download_avatar(&self, chat: ChatId, photo_id: i64) -> Result<Vec<u8>> {
        use grammers_client::tl;
        let api = &self.0;
        let peer = api.peer_ref(chat).await?;
        let photo = grammers_client::media::ChatPhoto {
            raw: tl::enums::InputFileLocation::InputPeerPhotoFileLocation(
                tl::types::InputPeerPhotoFileLocation {
                    big: false,
                    peer: peer.into(),
                    photo_id,
                },
            ),
        };
        let mut dl = api.client.iter_download(&photo);
        let mut bytes = Vec::new();
        while let Some(chunk) = dl.next().await.map_err(map_err)? {
            bytes.extend_from_slice(&chunk);
            if bytes.len() > crate::store::avatars::MAX_AVATAR_BYTES {
                return Err(Error::Protocol("avatar too large".into()));
            }
        }
        Ok(bytes)
    }

    async fn save_draft(
        &self,
        chat: ChatId,
        text: String,
        reply_to: Option<MessageId>,
    ) -> Result<()> {
        use grammers_client::tl;
        let api = &self.0;
        let peer = api.peer_ref(chat).await?;
        api.client
            .invoke(&tl::functions::messages::SaveDraft {
                no_webpage: false,
                invert_media: false,
                reply_to: reply_to.map(|id| {
                    tl::enums::InputReplyTo::Message(tl::types::InputReplyToMessage {
                        reply_to_msg_id: id,
                        top_msg_id: None,
                        reply_to_peer_id: None,
                        quote_text: None,
                        quote_entities: None,
                        quote_offset: None,
                        monoforum_peer_id: None,
                        todo_item_id: None,
                        poll_option: None,
                        quote_rich: None,
                    })
                }),
                peer: peer.into(),
                message: text,
                entities: None,
                media: None,
                effect: None,
                suggested_post: None,
                rich_message: None,
            })
            .await
            .map(drop)
            .map_err(map_err)
    }

    async fn set_typing(&self, chat: ChatId, active: bool) -> Result<()> {
        use grammers_client::tl::enums::SendMessageAction;
        let api = &self.0;
        let peer = api.peer_ref(chat).await?;
        let sender = api.client.action(peer);
        if active {
            sender
                .oneshot(SendMessageAction::SendMessageTypingAction)
                .await
        } else {
            sender.cancel().await
        }
        .map_err(map_err)
    }

    async fn download_media(&self, chat: ChatId, id: MessageId, path: PathBuf) -> Result<()> {
        let api = &self.0;
        let peer = api.peer_ref(chat).await?;
        let msg = api
            .client
            .get_messages_by_id(peer, &[id])
            .await
            .map_err(map_err)?
            .into_iter()
            .flatten()
            .next()
            .ok_or_else(|| Error::Protocol("message not found".into()))?;
        let media = msg
            .media()
            .ok_or_else(|| Error::Protocol("no media".into()))?;
        // Stream to disk with a hard cap (Telegram's own maximum file size).
        const MAX_FILE: u64 = 4 * 1024 * 1024 * 1024;
        let mut file = tokio::fs::File::create(&path).await?;
        let mut dl = api.client.iter_download(&media);
        let mut total = 0u64;
        while let Some(chunk) = dl.next().await.map_err(map_err)? {
            total += chunk.len() as u64;
            if total > MAX_FILE {
                drop(file);
                let _ = tokio::fs::remove_file(&path).await;
                return Err(Error::Protocol("file too large".into()));
            }
            tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await?;
        }
        tokio::io::AsyncWriteExt::flush(&mut file).await?;
        Ok(())
    }

    async fn peer_info(&self, chat: ChatId) -> Result<PeerInfo> {
        use grammers_client::tl;
        let api = &self.0;
        let peer = api.peer_ref(chat).await?;
        if chat > 0 {
            let r = api
                .client
                .invoke(&tl::functions::users::GetFullUser { id: peer.into() })
                .await
                .map_err(map_err)?;
            Ok(types::peer_info_from_user_full(&r, chat))
        } else if chat > -crate::model::CHANNEL_OFFSET {
            let r = api
                .client
                .invoke(&tl::functions::messages::GetFullChat { chat_id: -chat })
                .await
                .map_err(map_err)?;
            Ok(types::peer_info_from_chat_full(&r, chat))
        } else {
            let r = api
                .client
                .invoke(&tl::functions::channels::GetFullChannel {
                    channel: peer.into(),
                })
                .await
                .map_err(map_err)?;
            Ok(types::peer_info_from_chat_full(&r, chat))
        }
    }

    async fn set_muted(&self, chat: ChatId, muted: bool) -> Result<()> {
        use grammers_client::tl;
        let api = &self.0;
        let peer = api.peer_ref(chat).await?;
        api.client
            .invoke(&tl::functions::account::UpdateNotifySettings {
                peer: tl::enums::InputNotifyPeer::Peer(tl::types::InputNotifyPeer {
                    peer: peer.into(),
                }),
                settings: tl::enums::InputPeerNotifySettings::Settings(
                    tl::types::InputPeerNotifySettings {
                        show_previews: None,
                        silent: None,
                        // "Forever" is the largest timestamp, like the
                        // official clients; 0 lifts the mute.
                        mute_until: Some(if muted { i32::MAX } else { 0 }),
                        sound: None,
                        stories_muted: None,
                        stories_hide_sender: None,
                        stories_sound: None,
                    },
                ),
            })
            .await
            .map(drop)
            .map_err(map_err)
    }

    async fn shared_media(
        &self,
        chat: ChatId,
        kind: crate::model::SharedKind,
        offset_id: MessageId,
        limit: usize,
    ) -> Result<crate::model::SharedPage> {
        let api = &self.0;
        let peer = api.peer_ref(chat).await?;
        let mut iter = api
            .client
            .search_messages(peer)
            .filter(types::shared_filter(kind))
            .limit(limit);
        if offset_id > 0 {
            iter = iter.offset_id(offset_id);
        }
        let mut raws = Vec::with_capacity(limit);
        while let Some(m) = iter.next().await.map_err(map_err)? {
            if kind.has_thumbs()
                && let Some(size) = m.media().and_then(|media| thumb_size(&media))
            {
                api.remember_thumb((chat, m.id()), size);
            }
            raws.push(m.raw.clone());
        }
        Ok(types::shared_page_from_tl(&raws, kind, limit))
    }

    async fn download_thumb(&self, chat: ChatId, id: MessageId) -> Result<Vec<u8>> {
        let api = &self.0;
        let size = match api.thumb_for((chat, id)) {
            Some(s) => s,
            None => {
                // Not listed by a shared-media page (cache evicted): look the
                // message up.
                let peer = api.peer_ref(chat).await?;
                let msg = api
                    .client
                    .get_messages_by_id(peer, &[id])
                    .await
                    .map_err(map_err)?
                    .into_iter()
                    .flatten()
                    .next()
                    .ok_or_else(|| Error::Protocol("message not found".into()))?;
                msg.media()
                    .and_then(|m| thumb_size(&m))
                    .ok_or_else(|| Error::Protocol("no preview".into()))?
            }
        };
        use grammers_client::media::PhotoSize;
        if let PhotoSize::Cached(c) = &size {
            return Ok(c.bytes.clone());
        }
        let mut dl = api.client.iter_download(&size);
        let mut bytes = Vec::new();
        while let Some(chunk) = dl.next().await.map_err(map_err)? {
            bytes.extend_from_slice(&chunk);
            if bytes.len() > super::MAX_THUMB_BYTES {
                return Err(Error::Protocol("thumbnail too large".into()));
            }
        }
        Ok(bytes)
    }

    async fn dialog_filters(&self) -> Result<Vec<Folder>> {
        use grammers_client::tl;
        let api = &self.0;
        let raw = api
            .client
            .invoke(&tl::functions::messages::GetDialogFilters {})
            .await
            .map_err(map_err)?;
        let (folders, peers) = types::folders_from_tl(&raw, api.self_id.load(Ordering::Acquire));
        api.remember_folder_peers(peers);
        Ok(folders)
    }

    async fn save_dialog_filter(&self, folder: Folder) -> Result<()> {
        use grammers_client::tl;
        let api = &self.0;
        let filter = types::folder_to_tl(&folder, api.self_id.load(Ordering::Acquire), &|c| {
            api.resolve_input_peer(c)
        })
        .map_err(|_| Error::Protocol("unknown peer".into()))?;
        api.client
            .invoke(&tl::functions::messages::UpdateDialogFilter {
                id: folder.id,
                filter: Some(filter),
            })
            .await
            .map(drop)
            .map_err(map_err)
    }

    async fn delete_dialog_filter(&self, id: i32) -> Result<()> {
        use grammers_client::tl;
        self.0
            .client
            .invoke(&tl::functions::messages::UpdateDialogFilter { id, filter: None })
            .await
            .map(drop)
            .map_err(map_err)
    }

    async fn reorder_dialog_filters(&self, order: Vec<i32>) -> Result<()> {
        use grammers_client::tl;
        self.0
            .client
            .invoke(&tl::functions::messages::UpdateDialogFiltersOrder { order })
            .await
            .map(drop)
            .map_err(map_err)
    }

    async fn call_config(&self) -> Result<super::calls::CallConfig> {
        use grammers_client::tl;
        let r = self
            .0
            .client
            .invoke(&tl::functions::phone::GetCallConfig {})
            .await
            .map_err(map_err)?;
        let tl::enums::DataJson::Json(j) = r;
        Ok(super::calls::CallConfig::from_json(&j.data))
    }

    async fn dh_config(&self) -> Result<super::calls::dh::DhConfig> {
        use grammers_client::tl;
        let api = &self.0;
        let cached = lock(&api.dh)?.clone();
        let r = api
            .client
            .invoke(&tl::functions::messages::GetDhConfig {
                version: cached.as_ref().map_or(0, |c| c.version),
                random_length: 256,
            })
            .await
            .map_err(map_err)?;
        let cfg = types::dh_config_from_tl(&r, cached.as_ref())
            .ok_or_else(|| Error::Protocol("dh config".into()))?;
        *lock(&api.dh)? = Some(cfg.clone());
        Ok(cfg)
    }

    async fn request_call(
        &self,
        user: ChatId,
        g_a_hash: Vec<u8>,
    ) -> Result<super::calls::PhoneCall> {
        use grammers_client::tl;
        use rand::Rng as _;
        let api = &self.0;
        if user <= 0 {
            return Err(Error::Protocol("calls need a user".into()));
        }
        let random_id: i32 = rand::rng().random();
        let peer = api.peer_ref(user).await?;
        let r = api
            .client
            .invoke(&tl::functions::phone::RequestCall {
                video: false,
                user_id: peer.into(),
                random_id,
                g_a_hash,
                protocol: types::protocol_to_tl(&super::calls::Protocol::ours()),
            })
            .await
            .map_err(map_err)?;
        Ok(types::phone_call_from_response(&r))
    }

    async fn received_call(&self, call: super::calls::CallRef) -> Result<()> {
        use grammers_client::tl;
        self.0
            .client
            .invoke(&tl::functions::phone::ReceivedCall {
                peer: types::input_phone_call(call),
            })
            .await
            .map_err(map_err)?;
        Ok(())
    }

    async fn accept_call(
        &self,
        call: super::calls::CallRef,
        g_b: Vec<u8>,
    ) -> Result<super::calls::PhoneCall> {
        use grammers_client::tl;
        let r = self
            .0
            .client
            .invoke(&tl::functions::phone::AcceptCall {
                peer: types::input_phone_call(call),
                g_b,
                protocol: types::protocol_to_tl(&super::calls::Protocol::ours()),
            })
            .await
            .map_err(map_err)?;
        Ok(types::phone_call_from_response(&r))
    }

    async fn confirm_call(
        &self,
        call: super::calls::CallRef,
        g_a: Vec<u8>,
        key_fingerprint: i64,
    ) -> Result<super::calls::PhoneCall> {
        use grammers_client::tl;
        let r = self
            .0
            .client
            .invoke(&tl::functions::phone::ConfirmCall {
                peer: types::input_phone_call(call),
                g_a,
                key_fingerprint,
                protocol: types::protocol_to_tl(&super::calls::Protocol::ours()),
            })
            .await
            .map_err(map_err)?;
        Ok(types::phone_call_from_response(&r))
    }

    async fn discard_call(
        &self,
        call: super::calls::CallRef,
        duration: i32,
        reason: super::calls::DiscardReason,
    ) -> Result<()> {
        use grammers_client::tl;
        self.0
            .client
            .invoke(&tl::functions::phone::DiscardCall {
                video: false,
                peer: types::input_phone_call(call),
                duration,
                reason: types::discard_reason_to_tl(reason),
                connection_id: 0,
            })
            .await
            .map_err(map_err)?;
        Ok(())
    }

    async fn send_call_signaling(&self, call: super::calls::CallRef, data: Vec<u8>) -> Result<()> {
        use grammers_client::tl;
        self.0
            .client
            .invoke(&tl::functions::phone::SendSignalingData {
                peer: types::input_phone_call(call),
                data,
            })
            .await
            .map_err(map_err)?;
        Ok(())
    }

    fn take_notice(&self) -> Option<String> {
        self.0.session_reset.swap(false, Ordering::AcqRel).then(|| {
            "Your saved session could not be read and was reset. Please log in again.".into()
        })
    }

    fn events(&self) -> BoxStream<'static, Event> {
        self.0.events.subscribe()
    }
}
