//! Local cache: SQLite via rusqlite (architecture §8).
//!
//! The store is a cache: it can be deleted at any time and is rebuilt from
//! the network. It never contains credentials.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{Error, Result};
use crate::model::{
    CHANNEL_OFFSET, Chat, ChatId, ChatKind, Draft, Media, Message, MessageId, NotifyDefaults,
    NotifySettings, Reaction, SendState,
};

pub mod avatars;

const MIGRATIONS: &[&str] = &[
    include_str!("schema.sql"),
    include_str!("migration_002.sql"),
    include_str!("migration_003.sql"),
    include_str!("migration_004.sql"),
    include_str!("migration_005.sql"),
    include_str!("migration_006.sql"),
];

/// Cheap-to-clone handle. Sync methods are for `boot()` and tests; the app
/// uses [`Store::call`] so disk access happens on a blocking thread.
#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Store")
    }
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            crate::config::ensure_private_dir(dir)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        let store = Self::init(conn)?;
        // The cache holds message text in plaintext: keep it owner-only.
        for suffix in ["", "-wal", "-shm"] {
            let mut p = path.as_os_str().to_owned();
            p.push(suffix);
            crate::config::restrict_file(std::path::Path::new(&p));
        }
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let version: usize = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version) {
            conn.execute_batch(sql)?;
            conn.pragma_update(None, "user_version", i + 1)?;
        }
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn conn(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| Error::Store("connection lock poisoned".into()))
    }

    /// Run `f` on a blocking thread.
    pub async fn call<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Store) -> Result<T> + Send + 'static,
    {
        let me = self.clone();
        tokio::task::spawn_blocking(move || f(&me))
            .await
            .map_err(|_| Error::Cancelled)?
    }

    pub fn schema_version(&self) -> Result<usize> {
        Ok(self
            .conn()?
            .pragma_query_value(None, "user_version", |r| r.get(0))?)
    }

    // ---------------------------------------------------------------- chats

    pub fn upsert_chats(&self, chats: &[Chat]) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        for c in chats {
            tx.execute(
                "INSERT INTO chats (id, kind, title, unread, muted, pinned, read_outbox, read_inbox, last_date, last_message_id, photo_id,
                   draft_text, draft_reply, draft_date, contact,
                   notify_mute_until, notify_silent, notify_previews)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
                 ON CONFLICT(id) DO UPDATE SET kind=excluded.kind, title=excluded.title, unread=excluded.unread,
                   muted=excluded.muted, pinned=excluded.pinned, read_outbox=excluded.read_outbox,
                   read_inbox=excluded.read_inbox, last_date=excluded.last_date, last_message_id=excluded.last_message_id,
                   photo_id=excluded.photo_id, draft_text=excluded.draft_text,
                   draft_reply=excluded.draft_reply, draft_date=excluded.draft_date,
                   contact=excluded.contact, notify_mute_until=excluded.notify_mute_until,
                   notify_silent=excluded.notify_silent, notify_previews=excluded.notify_previews",
                params![
                    c.id,
                    c.kind.as_str(),
                    c.title,
                    c.unread_count,
                    c.muted,
                    c.pinned,
                    c.read_outbox_max_id,
                    c.read_inbox_max_id,
                    c.last_date,
                    c.last_message.as_ref().map(|m| m.id),
                    c.photo_id,
                    c.draft.as_ref().map(|d| d.text.as_str()),
                    c.draft.as_ref().and_then(|d| d.reply_to),
                    c.draft.as_ref().map(|d| d.date),
                    c.contact,
                    c.notify.mute_until,
                    c.notify.silent,
                    c.notify.show_previews,
                ],
            )?;
            if let Some(m) = &c.last_message {
                insert_message(&tx, m)?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Replace the chat list with the server's: upsert `chats` and prune
    /// chats (with their messages) that are no longer in the list.
    pub fn replace_chats(&self, chats: &[Chat]) -> Result<usize> {
        self.upsert_chats(chats)?;
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let keep: std::collections::HashSet<ChatId> = chats.iter().map(|c| c.id).collect();
        let existing: Vec<ChatId> = {
            let mut stmt = tx.prepare("SELECT id FROM chats")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let mut pruned = 0;
        for id in existing.into_iter().filter(|id| !keep.contains(id)) {
            tx.execute("DELETE FROM chats WHERE id = ?1", [id])?;
            tx.execute("DELETE FROM messages WHERE chat_id = ?1", [id])?;
            tx.execute("DELETE FROM history_ranges WHERE chat_id = ?1", [id])?;
            pruned += 1;
        }
        tx.commit()?;
        Ok(pruned)
    }

    /// Chats ordered like the official client: pinned first, then by activity.
    pub fn load_chats(&self) -> Result<Vec<Chat>> {
        let now = chrono::Utc::now().timestamp();
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, kind, title, unread, muted, pinned, read_outbox, read_inbox, last_date, last_message_id, photo_id,
                    draft_text, draft_reply, draft_date, contact,
                    notify_mute_until, notify_silent, notify_previews
             FROM chats ORDER BY pinned DESC, last_date DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            let mut c = Chat::new(
                r.get(0)?,
                ChatKind::parse(&r.get::<_, String>(1)?),
                r.get::<_, String>(2)?,
            );
            c.unread_count = r.get(3)?;
            c.notify = NotifySettings {
                mute_until: r.get(15)?,
                silent: r.get(16)?,
                show_previews: r.get(17)?,
            };
            c.muted = c.notify.muted_at(now).unwrap_or(false);
            c.pinned = r.get(5)?;
            c.read_outbox_max_id = r.get(6)?;
            c.read_inbox_max_id = r.get(7)?;
            c.last_date = r.get(8)?;
            c.photo_id = r.get(10)?;
            c.contact = r.get(14)?;
            c.draft = r.get::<_, Option<String>>(11)?.map(|text| Draft {
                text,
                reply_to: r.get(12).ok().flatten(),
                date: r.get(13).ok().flatten().unwrap_or(0),
            });
            Ok((c, r.get::<_, Option<MessageId>>(9)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (mut chat, last) = row?;
            if let Some(id) = last {
                chat.last_message = get_message(&conn, chat.id, id)?;
            }
            out.push(chat);
        }
        Ok(out)
    }

    /// Cache a chat's own notification settings.
    pub fn set_notify(&self, chat: ChatId, n: &NotifySettings) -> Result<()> {
        self.conn()?.execute(
            "UPDATE chats SET notify_mute_until = ?2, notify_silent = ?3, notify_previews = ?4 WHERE id = ?1",
            params![chat, n.mute_until, n.silent, n.show_previews],
        )?;
        Ok(())
    }

    /// Cache the account-wide notification defaults (JSON in `kv`).
    pub fn save_notify_defaults(&self, d: &NotifyDefaults) -> Result<()> {
        let json = serde_json::to_string(d).map_err(|e| Error::Store(e.to_string()))?;
        self.set_kv("notify_defaults", &json)
    }

    /// The cached defaults; unset when missing or unreadable.
    pub fn load_notify_defaults(&self) -> Result<NotifyDefaults> {
        Ok(self
            .get_kv("notify_defaults")?
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default())
    }

    /// Forget a chat and its messages (deleted or left).
    pub fn delete_chat(&self, chat: ChatId) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM chats WHERE id = ?1", [chat])?;
        tx.execute("DELETE FROM messages WHERE chat_id = ?1", [chat])?;
        tx.execute("DELETE FROM history_ranges WHERE chat_id = ?1", [chat])?;
        tx.commit()?;
        Ok(())
    }

    /// Drop a chat's messages but keep the chat (history cleared).
    pub fn clear_chat(&self, chat: ChatId) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM messages WHERE chat_id = ?1", [chat])?;
        tx.execute("DELETE FROM history_ranges WHERE chat_id = ?1", [chat])?;
        tx.execute(
            "UPDATE chats SET unread = 0, last_message_id = NULL WHERE id = ?1",
            [chat],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Set (or with `None` clear) a chat's draft.
    pub fn set_draft(&self, chat: ChatId, draft: Option<&Draft>) -> Result<()> {
        self.conn()?.execute(
            "UPDATE chats SET draft_text = ?2, draft_reply = ?3, draft_date = ?4 WHERE id = ?1",
            params![
                chat,
                draft.map(|d| d.text.as_str()),
                draft.and_then(|d| d.reply_to),
                draft.map(|d| d.date)
            ],
        )?;
        Ok(())
    }

    pub fn set_unread(&self, chat: ChatId, unread: u32, read_inbox: MessageId) -> Result<()> {
        self.conn()?.execute(
            "UPDATE chats SET unread = ?2, read_inbox = MAX(read_inbox, ?3) WHERE id = ?1",
            params![chat, unread, read_inbox],
        )?;
        Ok(())
    }

    pub fn set_read_outbox(&self, chat: ChatId, max_id: MessageId) -> Result<()> {
        self.conn()?.execute(
            "UPDATE chats SET read_outbox = MAX(read_outbox, ?2) WHERE id = ?1",
            params![chat, max_id],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------- messages

    pub fn upsert_messages(&self, msgs: &[Message]) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        for m in msgs {
            insert_message(&tx, m)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Up to `limit` messages older than `before` (exclusive), oldest first.
    pub fn load_history(
        &self,
        chat: ChatId,
        before: Option<MessageId>,
        limit: usize,
    ) -> Result<Vec<Message>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT chat_id, id, sender_id, sender_name, outgoing, date, text, media, reply_to, edited, reactions, service
             FROM messages WHERE chat_id = ?1 AND id < ?2 ORDER BY id DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![chat, before.unwrap_or(i32::MAX), limit as i64],
            row_to_message,
        )?;
        let mut out = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        out.reverse();
        Ok(out)
    }

    /// Network-confirmed contiguous range for a chat.
    pub fn history_range(&self, chat: ChatId) -> Result<Option<(MessageId, MessageId)>> {
        Ok(self
            .conn()?
            .query_row(
                "SELECT lo, hi FROM history_ranges WHERE chat_id = ?1",
                [chat],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    /// Store a network history page and extend the contiguous range.
    /// `before` is the request cursor (`None` = newest page).
    pub fn record_network_page(
        &self,
        chat: ChatId,
        before: Option<MessageId>,
        page: &[Message],
        limit: usize,
    ) -> Result<()> {
        self.upsert_messages(page)?;
        let Some(new) = page_range(before, page, limit) else {
            return Ok(());
        };
        let merged = merge_range(self.history_range(chat)?, new);
        self.conn()?.execute(
            "INSERT INTO history_ranges (chat_id, lo, hi) VALUES (?1, ?2, ?3)
             ON CONFLICT(chat_id) DO UPDATE SET lo = excluded.lo, hi = excluded.hi",
            params![chat, merged.0, merged.1],
        )?;
        Ok(())
    }

    /// Older page from the cache, and whether it can be trusted as complete
    /// (it lies inside the network-confirmed range). When it cannot, an
    /// empty page is returned and the caller must ask the network.
    pub fn load_older_cached(
        &self,
        chat: ChatId,
        before: MessageId,
        limit: usize,
    ) -> Result<(Vec<Message>, bool)> {
        let Some((lo, hi)) = self.history_range(chat)? else {
            return Ok((Vec::new(), false));
        };
        if !(lo < before && before <= hi.saturating_add(1)) {
            return Ok((Vec::new(), false));
        }
        let msgs: Vec<Message> = self
            .load_history(chat, Some(before), limit)?
            .into_iter()
            .filter(|m| m.id >= lo)
            .collect();
        let complete = msgs.len() >= limit || lo == 0;
        Ok((if complete { msgs } else { Vec::new() }, complete))
    }

    pub fn get_message(&self, chat: ChatId, id: MessageId) -> Result<Option<Message>> {
        get_message(&*self.conn()?, chat, id)
    }

    /// Delete messages. When `chat` is `None` (private chats / basic groups
    /// in Telegram's update model), every non-channel chat is affected.
    pub fn delete_messages(&self, chat: Option<ChatId>, ids: &[MessageId]) -> Result<usize> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let mut n = 0;
        for id in ids {
            n += match chat {
                Some(c) => tx.execute(
                    "DELETE FROM messages WHERE chat_id = ?1 AND id = ?2",
                    params![c, id],
                )?,
                None => tx.execute(
                    "DELETE FROM messages WHERE chat_id > ?1 AND id = ?2",
                    params![-CHANNEL_OFFSET, id],
                )?,
            };
        }
        tx.commit()?;
        Ok(n)
    }

    pub fn set_reactions(&self, chat: ChatId, id: MessageId, reactions: &[Reaction]) -> Result<()> {
        self.conn()?.execute(
            "UPDATE messages SET reactions = ?3 WHERE chat_id = ?1 AND id = ?2",
            params![chat, id, serde_json::to_string(reactions)?],
        )?;
        Ok(())
    }

    /// Local full-text-ish search (LIKE) used before the network answers.
    pub fn search_local(&self, chat: ChatId, query: &str, limit: usize) -> Result<Vec<Message>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT chat_id, id, sender_id, sender_name, outgoing, date, text, media, reply_to, edited, reactions, service
             FROM messages WHERE chat_id = ?1 AND text LIKE ?2 ESCAPE '\\' ORDER BY id DESC LIMIT ?3",
        )?;
        let pattern = format!(
            "%{}%",
            query
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let rows = stmt.query_map(params![chat, pattern, limit as i64], row_to_message)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    // ------------------------------------------------------------------- kv

    pub fn set_kv(&self, key: &str, value: &str) -> Result<()> {
        self.conn()?.execute(
            "INSERT INTO kv (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get_kv(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn()?
            .query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    /// Cache the folder list (JSON in `kv`) for the first frame.
    pub fn save_folders(&self, folders: &[crate::model::Folder]) -> Result<()> {
        let json = serde_json::to_string(folders)
            .map_err(|_| crate::error::Error::Store("folders encode".into()))?;
        self.set_kv("folders", &json)
    }

    /// The cached folders; empty when none or unreadable.
    pub fn load_folders(&self) -> Result<Vec<crate::model::Folder>> {
        Ok(self
            .get_kv("folders")?
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default())
    }

    /// Drop everything (logout).
    pub fn clear(&self) -> Result<()> {
        self.conn()?.execute_batch(
            "DELETE FROM messages; DELETE FROM chats; DELETE FROM kv; DELETE FROM history_ranges;",
        )?;
        Ok(())
    }
}

/// Range of ids a network page proves complete.
pub fn page_range(
    before: Option<MessageId>,
    page: &[Message],
    limit: usize,
) -> Option<(MessageId, MessageId)> {
    let min = page.iter().map(|m| m.id).filter(|id| *id > 0).min();
    let max = page.iter().map(|m| m.id).max();
    let lo = if page.len() < limit { Some(0) } else { min };
    let hi = match before {
        Some(b) => Some(b - 1),
        None => max,
    };
    Some((lo?, hi?))
}

/// Union of two ranges when they touch or overlap; otherwise the newer
/// range wins (the old one can no longer be proven contiguous with it).
pub fn merge_range(
    old: Option<(MessageId, MessageId)>,
    new: (MessageId, MessageId),
) -> (MessageId, MessageId) {
    match old {
        Some((lo, hi)) if new.1.saturating_add(1) >= lo && new.0 <= hi.saturating_add(1) => {
            (lo.min(new.0), hi.max(new.1))
        }
        _ => new,
    }
}

fn insert_message(conn: &Connection, m: &Message) -> Result<()> {
    let media = m.media.as_ref().map(serde_json::to_string).transpose()?;
    let reactions = if m.reactions.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&m.reactions)?)
    };
    conn.execute(
        "INSERT INTO messages (chat_id, id, sender_id, sender_name, outgoing, date, text, media, reply_to, edited, reactions, service)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(chat_id, id) DO UPDATE SET sender_id=excluded.sender_id,
           sender_name=COALESCE(excluded.sender_name, sender_name), outgoing=excluded.outgoing, date=excluded.date,
           text=excluded.text, media=excluded.media, reply_to=excluded.reply_to, edited=excluded.edited,
           reactions=excluded.reactions, service=excluded.service",
        params![
            m.chat_id,
            m.id,
            m.sender_id,
            m.sender_name,
            m.outgoing,
            m.date,
            m.text,
            media,
            m.reply_to,
            m.edited,
            reactions,
            m.service,
        ],
    )?;
    Ok(())
}

fn get_message(conn: &Connection, chat: ChatId, id: MessageId) -> Result<Option<Message>> {
    Ok(conn
        .query_row(
            "SELECT chat_id, id, sender_id, sender_name, outgoing, date, text, media, reply_to, edited, reactions, service
             FROM messages WHERE chat_id = ?1 AND id = ?2",
            params![chat, id],
            row_to_message,
        )
        .optional()?)
}

fn row_to_message(r: &rusqlite::Row<'_>) -> rusqlite::Result<Message> {
    let media: Option<String> = r.get(7)?;
    let reactions: Option<String> = r.get(10)?;
    Ok(Message {
        chat_id: r.get(0)?,
        id: r.get(1)?,
        sender_id: r.get(2)?,
        sender_name: r.get(3)?,
        outgoing: r.get(4)?,
        date: r.get(5)?,
        text: r.get(6)?,
        media: media.and_then(|s| serde_json::from_str::<Media>(&s).ok()),
        reply_to: r.get(8)?,
        edited: r.get(9)?,
        reactions: reactions
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default(),
        service: r.get(11)?,
        state: SendState::Sent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(id: ChatId, date: i64, pinned: bool) -> Chat {
        let mut c = Chat::new(id, ChatKind::User, format!("Chat {id}"));
        c.last_date = date;
        c.pinned = pinned;
        c.last_message = Some(Message::text(id, 10, false, date, "last"));
        c
    }

    #[test]
    fn notify_settings_roundtrip_and_unset_stays_unset() {
        let s = Store::open_in_memory().unwrap();
        let mut c = chat(1, 5, false);
        c.notify = NotifySettings {
            mute_until: Some(1_900_000_000),
            silent: Some(true),
            show_previews: Some(false),
        };
        s.upsert_chats(&[c, chat(2, 4, false)]).unwrap();
        let loaded = s.load_chats().unwrap();
        let one = loaded.iter().find(|c| c.id == 1).unwrap();
        assert_eq!(one.notify.mute_until, Some(1_900_000_000));
        assert_eq!(one.notify.silent, Some(true));
        assert_eq!(one.notify.show_previews, Some(false));
        assert!(one.muted, "far in the future");
        let two = loaded.iter().find(|c| c.id == 2).unwrap();
        assert_eq!(two.notify, NotifySettings::default());
        assert!(!two.muted);
        // set_notify updates in place; an expired mute loads as unmuted.
        s.set_notify(
            1,
            &NotifySettings {
                mute_until: Some(10),
                ..NotifySettings::default()
            },
        )
        .unwrap();
        let again = s.load_chats().unwrap();
        let one = again.iter().find(|c| c.id == 1).unwrap();
        assert_eq!(one.notify.mute_until, Some(10));
        assert!(!one.muted);
        assert_eq!(one.notify.silent, None);
    }

    #[test]
    fn notify_defaults_are_cached_and_bad_json_is_ignored() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.load_notify_defaults().unwrap(), NotifyDefaults::default());
        let d = NotifyDefaults {
            broadcasts: NotifySettings {
                mute_until: Some(crate::model::MUTE_FOREVER),
                silent: None,
                show_previews: Some(false),
            },
            ..NotifyDefaults::default()
        };
        s.save_notify_defaults(&d).unwrap();
        assert_eq!(s.load_notify_defaults().unwrap(), d);
        s.set_kv("notify_defaults", "{broken").unwrap();
        assert_eq!(s.load_notify_defaults().unwrap(), NotifyDefaults::default());
    }

    #[test]
    fn muted_chats_cached_before_migration_6_stay_muted() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("old.sqlite3");
        {
            let conn = Connection::open(&p).unwrap();
            for sql in &MIGRATIONS[..5] {
                conn.execute_batch(sql).unwrap();
            }
            conn.pragma_update(None, "user_version", 5).unwrap();
            conn.execute(
                "INSERT INTO chats (id, kind, title, muted) VALUES (7, 'group', 'G', 1), (8, 'user', 'U', 0)",
                [],
            )
            .unwrap();
        }
        let s = Store::open(&p).unwrap();
        let chats = s.load_chats().unwrap();
        assert!(chats.iter().find(|c| c.id == 7).unwrap().muted);
        assert!(!chats.iter().find(|c| c.id == 8).unwrap().muted);
    }

    #[test]
    fn delete_and_clear_chat_drop_messages() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_chats(&[chat(1, 5, false), chat(2, 4, false)])
            .unwrap();
        s.upsert_messages(&[
            Message::text(1, 1, false, 1, "a"),
            Message::text(2, 1, false, 1, "b"),
        ])
        .unwrap();
        s.clear_chat(1).unwrap();
        assert!(s.get_message(1, 1).unwrap().is_none());
        assert!(s.get_message(2, 1).unwrap().is_some());
        let c1 = s
            .load_chats()
            .unwrap()
            .into_iter()
            .find(|c| c.id == 1)
            .unwrap();
        assert!(
            c1.last_message.is_none() && c1.unread_count == 0,
            "chat stays, empty"
        );
        s.delete_chat(2).unwrap();
        assert!(s.load_chats().unwrap().iter().all(|c| c.id != 2));
        assert!(s.get_message(2, 1).unwrap().is_none());
    }

    #[test]
    fn photo_id_roundtrips_and_clears() {
        let s = Store::open_in_memory().unwrap();
        let mut c = chat(1, 5, false);
        c.photo_id = Some(1234567890123);
        s.upsert_chats(&[c.clone()]).unwrap();
        assert_eq!(s.load_chats().unwrap()[0].photo_id, Some(1234567890123));
        c.photo_id = None;
        s.upsert_chats(&[c]).unwrap();
        assert_eq!(s.load_chats().unwrap()[0].photo_id, None);
    }

    #[test]
    fn v2_database_migrates_forward_keeping_chats() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("old.sqlite3");
        {
            let conn = Connection::open(&p).unwrap();
            conn.execute_batch(MIGRATIONS[0]).unwrap();
            conn.execute_batch(MIGRATIONS[1]).unwrap();
            conn.execute(
                "INSERT INTO chats (id, kind, title) VALUES (7, 'user', 'Old chat')",
                [],
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 2).unwrap();
        }
        let s = Store::open(&p).unwrap();
        assert_eq!(s.schema_version().unwrap(), MIGRATIONS.len());
        let chats = s.load_chats().unwrap();
        assert_eq!(chats.len(), 1);
        assert_eq!(chats[0].title, "Old chat");
        assert_eq!(chats[0].photo_id, None);
    }

    #[test]
    fn contact_flag_and_folders_persist() {
        use crate::model::Folder;
        let s = Store::open_in_memory().unwrap();
        let mut c = chat(1, 5, false);
        c.contact = true;
        s.upsert_chats(&[c, chat(2, 4, false)]).unwrap();
        let loaded = s.load_chats().unwrap();
        assert!(loaded.iter().find(|c| c.id == 1).unwrap().contact);
        assert!(!loaded.iter().find(|c| c.id == 2).unwrap().contact);

        assert!(s.load_folders().unwrap().is_empty());
        let mut f = Folder::new(2, "Work");
        f.groups = true;
        f.include = vec![1, -1_000_000_000_005];
        s.save_folders(std::slice::from_ref(&f)).unwrap();
        assert_eq!(s.load_folders().unwrap(), vec![f]);
        s.set_kv("folders", "{not json").unwrap();
        assert!(s.load_folders().unwrap().is_empty(), "garbage is ignored");
        s.clear().unwrap();
        assert!(s.load_folders().unwrap().is_empty());
    }

    #[test]
    fn drafts_persist_through_chat_upserts_and_clear() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_chats(&[chat(1, 5, false), chat(2, 4, false)])
            .unwrap();
        assert!(s.load_chats().unwrap().iter().all(|c| c.draft.is_none()));
        let d = Draft {
            text: "half a thought".into(),
            reply_to: Some(77),
            date: 1234,
        };
        s.set_draft(1, Some(&d)).unwrap();
        let loaded = s.load_chats().unwrap();
        let c1 = loaded.iter().find(|c| c.id == 1).unwrap();
        assert_eq!(c1.draft, Some(d.clone()));
        assert!(loaded.iter().find(|c| c.id == 2).unwrap().draft.is_none());
        // The server's chat list is authoritative: an upsert without a draft clears it.
        s.upsert_chats(&[chat(1, 6, false)]).unwrap();
        assert!(s.load_chats().unwrap().iter().all(|c| c.draft.is_none()));
        // And one with a draft stores it.
        let mut with = chat(2, 4, false);
        with.draft = Some(d.clone());
        s.upsert_chats(&[with]).unwrap();
        assert_eq!(
            s.load_chats()
                .unwrap()
                .iter()
                .find(|c| c.id == 2)
                .unwrap()
                .draft,
            Some(d)
        );
        s.set_draft(2, None).unwrap();
        assert!(s.load_chats().unwrap().iter().all(|c| c.draft.is_none()));
    }

    #[test]
    fn migrations_set_user_version() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.schema_version().unwrap(), MIGRATIONS.len());
    }

    #[test]
    fn reopen_file_keeps_data_and_version() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.sqlite3");
        Store::open(&p)
            .unwrap()
            .upsert_chats(&[chat(1, 5, false)])
            .unwrap();
        let s = Store::open(&p).unwrap();
        assert_eq!(s.load_chats().unwrap().len(), 1);
        assert_eq!(s.schema_version().unwrap(), MIGRATIONS.len());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn chats_order_pinned_then_recent_with_last_message() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_chats(&[chat(1, 100, false), chat(2, 300, false), chat(3, 50, true)])
            .unwrap();
        let ids: Vec<_> = s.load_chats().unwrap().iter().map(|c| c.id).collect();
        assert_eq!(ids, vec![3, 2, 1]);
        let c = &s.load_chats().unwrap()[0];
        assert_eq!(c.last_message.as_ref().unwrap().text, "last");
    }

    #[test]
    fn history_pages_backwards_oldest_first() {
        let s = Store::open_in_memory().unwrap();
        let msgs: Vec<_> = (1..=50)
            .map(|i| Message::text(7, i, i % 2 == 0, i64::from(i), &format!("m{i}")))
            .collect();
        s.upsert_messages(&msgs).unwrap();
        let page = s.load_history(7, None, 20).unwrap();
        assert_eq!(page.first().unwrap().id, 31);
        assert_eq!(page.last().unwrap().id, 50);
        let older = s.load_history(7, Some(31), 20).unwrap();
        assert_eq!(older.first().unwrap().id, 11);
        assert_eq!(older.last().unwrap().id, 30);
        assert!(s.load_history(8, None, 20).unwrap().is_empty());
    }

    #[test]
    fn media_reactions_and_edits_roundtrip() {
        let s = Store::open_in_memory().unwrap();
        let mut m = Message::text(1, 1, true, 0, "cap");
        m.media = Some(Media::File {
            name: "a.pdf".into(),
            size: 10,
            mime: "application/pdf".into(),
        });
        m.reply_to = Some(9);
        s.upsert_messages(&[m.clone()]).unwrap();
        let r = vec![Reaction {
            emoji: "👍".into(),
            count: 2,
            chosen: true,
        }];
        s.set_reactions(1, 1, &r).unwrap();
        m.text = "edited".into();
        m.edited = true;
        m.reactions = r.clone();
        s.upsert_messages(&[m.clone()]).unwrap();
        assert_eq!(s.get_message(1, 1).unwrap().unwrap(), m);
    }

    #[test]
    fn range_math() {
        let page: Vec<_> = (901..=950)
            .map(|i| Message::text(1, i, false, 0, ""))
            .collect();
        assert_eq!(page_range(None, &page, 50), Some((901, 950)));
        assert_eq!(page_range(Some(951), &page, 50), Some((901, 950)));
        assert_eq!(page_range(Some(951), &page[..10], 50), Some((0, 950)));
        assert_eq!(page_range(Some(5), &[], 50), Some((0, 4)));
        assert_eq!(page_range(None, &[], 50), None);
        assert_eq!(merge_range(Some((901, 950)), (851, 900)), (851, 950));
        assert_eq!(merge_range(Some((901, 950)), (960, 990)), (960, 990));
        assert_eq!(merge_range(None, (1, 2)), (1, 2));
    }

    #[test]
    fn cache_gap_forces_network_fetch() {
        let s = Store::open_in_memory().unwrap();
        // Old cache 1..=50 (from an earlier session, never confirmed contiguous
        // with the present), then a fresh newest page 901..=950 from the network.
        let old: Vec<_> = (1..=50)
            .map(|i| Message::text(1, i, false, 0, ""))
            .collect();
        s.record_network_page(1, Some(51), &old, 50).unwrap();
        let newest: Vec<_> = (901..=950)
            .map(|i| Message::text(1, i, false, 0, ""))
            .collect();
        s.record_network_page(1, None, &newest, 50).unwrap();
        assert_eq!(s.history_range(1).unwrap(), Some((901, 950)));
        // Paging older than 901 must NOT be served from the cache (gap 51..=900).
        let (msgs, complete) = s.load_older_cached(1, 901, 50).unwrap();
        assert!(!complete && msgs.is_empty());
        // Once the network fills the gap down to 851, 851..=900 is trusted.
        let mid: Vec<_> = (851..=900)
            .map(|i| Message::text(1, i, false, 0, ""))
            .collect();
        s.record_network_page(1, Some(901), &mid, 50).unwrap();
        let (msgs, complete) = s.load_older_cached(1, 921, 50).unwrap();
        assert!(complete);
        assert_eq!((msgs[0].id, msgs.last().unwrap().id), (871, 920));
        // A short network page means "start of history": cache is complete.
        let tail: Vec<_> = (1..=50)
            .map(|i| Message::text(1, i, false, 0, ""))
            .collect();
        s.record_network_page(1, Some(851), &tail[..30], 50)
            .unwrap();
        assert_eq!(s.history_range(1).unwrap(), Some((0, 950)));
        assert!(s.load_older_cached(1, 10, 50).unwrap().1);
    }

    #[test]
    fn replace_chats_prunes_ghosts() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_chats(&[chat(1, 1, false), chat(2, 2, false)])
            .unwrap();
        s.record_network_page(2, None, &[Message::text(2, 10, false, 0, "x")], 50)
            .unwrap();
        assert_eq!(s.replace_chats(&[chat(1, 3, false)]).unwrap(), 1);
        assert_eq!(s.load_chats().unwrap().len(), 1);
        assert!(s.get_message(2, 10).unwrap().is_none());
        assert!(s.history_range(2).unwrap().is_none());
    }

    #[test]
    fn delete_without_chat_skips_channels() {
        let s = Store::open_in_memory().unwrap();
        let channel = -CHANNEL_OFFSET - 5;
        s.upsert_messages(&[
            Message::text(1, 3, false, 0, "a"),
            Message::text(-9, 3, false, 0, "b"),
            Message::text(channel, 3, false, 0, "c"),
        ])
        .unwrap();
        assert_eq!(s.delete_messages(None, &[3]).unwrap(), 2);
        assert!(s.get_message(channel, 3).unwrap().is_some());
        assert_eq!(s.delete_messages(Some(channel), &[3]).unwrap(), 1);
    }

    #[test]
    fn read_state_and_search() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_chats(&[chat(1, 1, false)]).unwrap();
        s.set_read_outbox(1, 8).unwrap();
        s.set_read_outbox(1, 4).unwrap();
        s.set_unread(1, 3, 2).unwrap();
        let c = &s.load_chats().unwrap()[0];
        assert_eq!((c.read_outbox_max_id, c.unread_count), (8, 3));
        s.upsert_messages(&[
            Message::text(1, 20, false, 0, "the 100% plan"),
            Message::text(1, 21, false, 0, "nothing"),
        ])
        .unwrap();
        assert_eq!(s.search_local(1, "100%", 10).unwrap().len(), 1);
        assert_eq!(s.search_local(1, "PLAN", 10).unwrap().len(), 1);
        s.set_kv("self_id", "42").unwrap();
        assert_eq!(s.get_kv("self_id").unwrap().as_deref(), Some("42"));
        s.clear().unwrap();
        assert!(s.load_chats().unwrap().is_empty());
    }
}
