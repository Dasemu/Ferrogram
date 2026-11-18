//! Local cache: SQLite via rusqlite (architecture §8).
//!
//! The store is a cache: it can be deleted at any time and is rebuilt from
//! the network. It never contains credentials.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{Error, Result};
use crate::model::{
    CHANNEL_OFFSET, Chat, ChatId, ChatKind, Media, Message, MessageId, Reaction, SendState,
};

const MIGRATIONS: &[&str] = &[include_str!("schema.sql")];

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
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn)
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
                "INSERT INTO chats (id, kind, title, unread, muted, pinned, read_outbox, read_inbox, last_date, last_message_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(id) DO UPDATE SET kind=excluded.kind, title=excluded.title, unread=excluded.unread,
                   muted=excluded.muted, pinned=excluded.pinned, read_outbox=excluded.read_outbox,
                   read_inbox=excluded.read_inbox, last_date=excluded.last_date, last_message_id=excluded.last_message_id",
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
                ],
            )?;
            if let Some(m) = &c.last_message {
                insert_message(&tx, m)?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Chats ordered like the official client: pinned first, then by activity.
    pub fn load_chats(&self) -> Result<Vec<Chat>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, kind, title, unread, muted, pinned, read_outbox, read_inbox, last_date, last_message_id
             FROM chats ORDER BY pinned DESC, last_date DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            let mut c = Chat::new(
                r.get(0)?,
                ChatKind::parse(&r.get::<_, String>(1)?),
                r.get::<_, String>(2)?,
            );
            c.unread_count = r.get(3)?;
            c.muted = r.get(4)?;
            c.pinned = r.get(5)?;
            c.read_outbox_max_id = r.get(6)?;
            c.read_inbox_max_id = r.get(7)?;
            c.last_date = r.get(8)?;
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

    /// Drop everything (logout).
    pub fn clear(&self) -> Result<()> {
        self.conn()?
            .execute_batch("DELETE FROM messages; DELETE FROM chats; DELETE FROM kv;")?;
        Ok(())
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
        assert_eq!(s.schema_version().unwrap(), 1);
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
