//! Pure state types and reducers (unit-tested without iced).

use std::collections::{HashMap, VecDeque};

use crate::model::{Chat, ChatId, ChatKind, Message, MessageId, Reaction, SendState};
use crate::net::Event;

/// Max messages kept in memory per open conversation (architecture §3).
pub const MAX_WINDOW: usize = 200;
/// Page size for history loads.
pub const PAGE: usize = 50;

// ------------------------------------------------------------------ chat list

#[derive(Debug, Default, Clone)]
pub struct ChatList {
    chats: Vec<Chat>,
}

impl ChatList {
    pub fn new(mut chats: Vec<Chat>) -> Self {
        sort_chats(&mut chats);
        Self { chats }
    }

    pub fn all(&self) -> &[Chat] {
        &self.chats
    }

    pub fn get(&self, id: ChatId) -> Option<&Chat> {
        self.chats.iter().find(|c| c.id == id)
    }

    fn get_mut(&mut self, id: ChatId) -> Option<&mut Chat> {
        self.chats.iter_mut().find(|c| c.id == id)
    }

    pub fn replace(&mut self, chats: Vec<Chat>) {
        *self = Self::new(chats);
    }

    pub fn total_unread(&self) -> u32 {
        self.chats
            .iter()
            .filter(|c| !c.muted)
            .map(|c| c.unread_count)
            .sum()
    }

    /// Case-insensitive title filter for the sidebar search.
    pub fn filter(&self, query: &str) -> Vec<&Chat> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return self.chats.iter().collect();
        }
        self.chats
            .iter()
            .filter(|c| c.title.to_lowercase().contains(&q))
            .collect()
    }

    pub fn mark_read(&mut self, id: ChatId) {
        if let Some(c) = self.get_mut(id) {
            c.unread_count = 0;
            if let Some(m) = &c.last_message {
                c.read_inbox_max_id = c.read_inbox_max_id.max(m.id);
            }
        }
    }

    /// Apply a network event. `open` is the chat currently on screen (and
    /// focused): new incoming messages there do not bump the unread counter.
    pub fn apply_event(&mut self, ev: &Event, open: Option<ChatId>) {
        match ev {
            Event::NewMessage(m) => {
                if let Some(c) = self.get_mut(m.chat_id) {
                    if c.last_message.as_ref().is_none_or(|l| l.id <= m.id) {
                        c.last_message = Some(m.clone());
                        c.last_date = m.date;
                    }
                    if !m.outgoing && open != Some(m.chat_id) {
                        c.unread_count += 1;
                    }
                } else {
                    // A chat we have not seen yet: show it with a placeholder title.
                    let mut c = Chat::new(
                        m.chat_id,
                        if m.chat_id > 0 {
                            ChatKind::User
                        } else {
                            ChatKind::Group
                        },
                        m.sender_name.clone().unwrap_or_else(|| "New chat".into()),
                    );
                    c.last_date = m.date;
                    c.unread_count = u32::from(!m.outgoing && open != Some(m.chat_id));
                    c.last_message = Some(m.clone());
                    self.chats.push(c);
                }
                sort_chats(&mut self.chats);
            }
            Event::MessageEdited(m) => {
                if let Some(c) = self.get_mut(m.chat_id)
                    && c.last_message.as_ref().is_some_and(|l| l.id == m.id)
                {
                    c.last_message = Some(m.clone());
                }
            }
            Event::MessagesDeleted { chat, ids } => {
                for c in &mut self.chats {
                    if chat.is_some_and(|id| id != c.id) {
                        continue;
                    }
                    if chat.is_none() && c.id < -crate::model::CHANNEL_OFFSET {
                        continue;
                    }
                    if c.last_message.as_ref().is_some_and(|l| ids.contains(&l.id)) {
                        c.last_message = None;
                    }
                }
            }
            Event::ReadOutbox { chat, max_id } => {
                if let Some(c) = self.get_mut(*chat) {
                    c.read_outbox_max_id = c.read_outbox_max_id.max(*max_id);
                }
            }
            Event::ReadInbox {
                chat,
                max_id,
                still_unread,
            } => {
                if let Some(c) = self.get_mut(*chat) {
                    c.read_inbox_max_id = c.read_inbox_max_id.max(*max_id);
                    c.unread_count = *still_unread;
                }
            }
            Event::Reactions { .. } | Event::Connection(_) | Event::SessionRevoked => {}
        }
    }

    /// Replace a chat's last message after a local send.
    pub fn set_last(&mut self, m: &Message) {
        if let Some(c) = self.get_mut(m.chat_id) {
            c.last_message = Some(m.clone());
            c.last_date = m.date;
        }
        sort_chats(&mut self.chats);
    }
}

/// Pinned first, then most recent activity first.
pub fn sort_chats(chats: &mut [Chat]) {
    chats.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then(b.last_date.cmp(&a.last_date))
            .then(a.id.cmp(&b.id))
    });
}

// --------------------------------------------------------------- conversation

/// The open chat's in-memory message window.
#[derive(Debug, Clone)]
pub struct Conversation {
    pub chat_id: ChatId,
    /// Sorted ascending by id; local pending messages have negative ids and
    /// sort last.
    messages: Vec<Message>,
    pub loading_older: bool,
    /// The network said there is nothing older.
    pub reached_start: bool,
    next_temp_id: MessageId,
}

impl Conversation {
    pub fn new(chat_id: ChatId) -> Self {
        Self {
            chat_id,
            messages: Vec::new(),
            loading_older: false,
            reached_start: false,
            next_temp_id: -1,
        }
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn get(&self, id: MessageId) -> Option<&Message> {
        self.messages.iter().find(|m| m.id == id)
    }

    /// Oldest confirmed message id (cursor for the next older page).
    pub fn oldest_id(&self) -> Option<MessageId> {
        self.messages
            .iter()
            .filter(|m| m.id > 0)
            .map(|m| m.id)
            .min()
    }

    fn sort(&mut self) {
        self.messages
            .sort_by_key(|m| if m.id < 0 { (1, -m.id) } else { (0, m.id) });
    }

    /// Merge a page (from cache or network). Existing messages are updated,
    /// duplicates dropped. Returns how many new messages were added.
    pub fn merge(&mut self, page: Vec<Message>) -> usize {
        let mut added = 0;
        for m in page {
            if m.chat_id != self.chat_id {
                continue;
            }
            match self.messages.iter_mut().find(|x| x.id == m.id) {
                Some(x) => *x = m,
                None => {
                    self.messages.push(m);
                    added += 1;
                }
            }
        }
        self.sort();
        added
    }

    /// Drop the oldest messages beyond [`MAX_WINDOW`] (only while the user is
    /// looking at the bottom, so we never yank content from under them).
    pub fn trim(&mut self) {
        let confirmed = self.messages.iter().filter(|m| m.id > 0).count();
        if confirmed > MAX_WINDOW {
            let drop = confirmed - MAX_WINDOW;
            self.messages.drain(..drop);
            self.reached_start = false;
        }
    }

    /// Add an optimistic outgoing message; returns its temporary id.
    pub fn push_pending(&mut self, mut m: Message) -> MessageId {
        let id = self.next_temp_id;
        self.next_temp_id -= 1;
        m.id = id;
        m.state = SendState::Sending;
        self.messages.push(m);
        self.sort();
        id
    }

    /// Replace a pending message with the server's copy (or mark it failed).
    pub fn resolve_pending(&mut self, temp: MessageId, result: Option<Message>) {
        match result {
            Some(real) => {
                self.messages.retain(|m| m.id != temp && m.id != real.id);
                self.messages.push(real);
                self.sort();
            }
            None => {
                if let Some(m) = self.messages.iter_mut().find(|m| m.id == temp) {
                    m.state = SendState::Failed;
                }
            }
        }
    }

    pub fn apply_event(&mut self, ev: &Event) {
        match ev {
            Event::NewMessage(m) if m.chat_id == self.chat_id => {
                self.merge(vec![m.clone()]);
            }
            Event::MessageEdited(m) if m.chat_id == self.chat_id => {
                if let Some(x) = self.messages.iter_mut().find(|x| x.id == m.id) {
                    let name = x.sender_name.take();
                    *x = m.clone();
                    if x.sender_name.is_none() {
                        x.sender_name = name;
                    }
                }
            }
            Event::MessagesDeleted { chat, ids } => {
                let applies = match chat {
                    Some(c) => *c == self.chat_id,
                    None => self.chat_id > -crate::model::CHANNEL_OFFSET,
                };
                if applies {
                    self.messages.retain(|m| !ids.contains(&m.id));
                }
            }
            Event::Reactions {
                chat,
                id,
                reactions,
            } if *chat == self.chat_id => {
                if let Some(m) = self.messages.iter_mut().find(|m| m.id == *id) {
                    m.reactions = reactions.clone();
                }
            }
            _ => {}
        }
    }

    /// Locally apply a reaction toggle (optimistic).
    pub fn toggle_reaction(&mut self, id: MessageId, emoji: &str) -> Option<Option<String>> {
        let m = self.messages.iter_mut().find(|m| m.id == id)?;
        let already = m.reactions.iter().any(|r| r.chosen && r.emoji == emoji);
        let target = if already { None } else { Some(emoji) };
        toggle_reaction(&mut m.reactions, target);
        Some(target.map(str::to_owned))
    }

    pub fn edit_local(&mut self, id: MessageId, text: &str) {
        if let Some(m) = self.messages.iter_mut().find(|m| m.id == id) {
            m.text = text.to_owned();
            m.edited = true;
        }
    }

    pub fn remove_local(&mut self, id: MessageId) {
        self.messages.retain(|m| m.id != id);
    }
}

/// Set our reaction to `emoji` (or remove it with `None`), Telegram style:
/// one reaction per user for non-premium accounts.
pub fn toggle_reaction(reactions: &mut Vec<Reaction>, emoji: Option<&str>) {
    for r in reactions.iter_mut() {
        if r.chosen {
            r.chosen = false;
            r.count = r.count.saturating_sub(1);
        }
    }
    if let Some(e) = emoji {
        match reactions.iter_mut().find(|r| r.emoji == e) {
            Some(r) => {
                r.chosen = true;
                r.count += 1;
            }
            None => reactions.push(Reaction {
                emoji: e.to_owned(),
                count: 1,
                chosen: true,
            }),
        }
    }
    reactions.retain(|r| r.count > 0);
}

// ---------------------------------------------------------------- composer

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposeMode {
    Normal,
    Reply(MessageId),
    Edit(MessageId),
}

// ----------------------------------------------------------- notifications

/// Whether a new message should raise a desktop notification.
/// Messages older than this when they arrive are catch-up, not "new".
pub const NOTIFY_MAX_AGE_SECS: i64 = 120;

/// Whether a new message should raise a desktop notification. `now` is the
/// wall clock (unix seconds): catch-up messages after a reconnect are skipped.
pub fn should_notify(
    chat: Option<&Chat>,
    m: &Message,
    open: Option<ChatId>,
    focused: bool,
    now: i64,
) -> bool {
    if m.outgoing || m.service || now - m.date > NOTIFY_MAX_AGE_SECS {
        return false;
    }
    if focused && open == Some(m.chat_id) {
        return false;
    }
    !chat.is_some_and(|c| c.muted)
}

/// Notification (title, body) following the privacy setting.
pub fn notification_text(chat: Option<&Chat>, m: &Message, preview: bool) -> (String, String) {
    let title = chat
        .map(|c| c.title.clone())
        .or_else(|| m.sender_name.clone())
        .unwrap_or_else(|| "New message".into());
    let body = if !preview {
        "New message".to_owned()
    } else {
        match (&m.sender_name, chat.map(|c| c.kind)) {
            (Some(s), Some(ChatKind::Group)) => format!("{s}: {}", m.preview()),
            _ => m.preview(),
        }
    };
    (title, body)
}

// ------------------------------------------------------------- image cache

/// Byte-budgeted LRU (architecture §3: decoded images, default 48 MB).
#[derive(Debug)]
pub struct Lru<K, V> {
    map: HashMap<K, (V, usize)>,
    order: VecDeque<K>,
    bytes: usize,
    budget: usize,
}

impl<K: std::hash::Hash + Eq + Clone, V> Lru<K, V> {
    pub fn new(budget: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
            budget,
        }
    }

    pub fn get(&mut self, k: &K) -> Option<&V> {
        if self.map.contains_key(k) {
            self.order.retain(|x| x != k);
            self.order.push_back(k.clone());
        }
        self.map.get(k).map(|(v, _)| v)
    }

    pub fn peek(&self, k: &K) -> Option<&V> {
        self.map.get(k).map(|(v, _)| v)
    }

    pub fn insert(&mut self, k: K, v: V, size: usize) {
        if let Some((_, old)) = self.map.remove(&k) {
            self.bytes -= old;
            self.order.retain(|x| x != &k);
        }
        self.map.insert(k.clone(), (v, size));
        self.order.push_back(k);
        self.bytes += size;
        while self.bytes > self.budget && self.order.len() > 1 {
            if let Some(old) = self.order.pop_front()
                && let Some((_, s)) = self.map.remove(&old)
            {
                self.bytes -= s;
            }
        }
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

// ----------------------------------------------------------- image limits

/// Hard cap on downloaded photo bytes before we even try to decode.
pub use crate::net::MAX_PHOTO_BYTES;
/// Hard cap on decoded pixels (≈ 160 MB of RGBA at 40 MP).
pub const MAX_PHOTO_PIXELS: u64 = 40_000_000;

/// Width/height from a PNG or JPEG header without decoding the image.
pub fn image_dimensions(b: &[u8]) -> Option<(u32, u32)> {
    if b.len() >= 24 && b.starts_with(b"\x89PNG\r\n\x1a\n") && &b[12..16] == b"IHDR" {
        let w = u32::from_be_bytes([b[16], b[17], b[18], b[19]]);
        let h = u32::from_be_bytes([b[20], b[21], b[22], b[23]]);
        return Some((w, h));
    }
    if b.len() >= 4 && b[0] == 0xFF && b[1] == 0xD8 {
        let mut i = 2;
        while i + 9 < b.len() {
            if b[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = b[i + 1];
            if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
                i += 2;
                continue;
            }
            let len = usize::from(u16::from_be_bytes([b[i + 2], b[i + 3]]));
            // SOF0..SOF15 except DHT (C4), JPG (C8), DAC (CC).
            if (0xC0..=0xCF).contains(&marker) && ![0xC4, 0xC8, 0xCC].contains(&marker) {
                let h = u32::from(u16::from_be_bytes([b[i + 5], b[i + 6]]));
                let w = u32::from(u16::from_be_bytes([b[i + 7], b[i + 8]]));
                return Some((w, h));
            }
            i += 2 + len;
        }
    }
    None
}

/// Whether downloaded photo bytes are safe to hand to the image decoder.
pub fn photo_is_acceptable(b: &[u8]) -> bool {
    if b.len() > MAX_PHOTO_BYTES {
        return false;
    }
    match image_dimensions(b) {
        Some((w, h)) => w > 0 && h > 0 && u64::from(w) * u64::from(h) <= MAX_PHOTO_PIXELS,
        // Unknown format (e.g. WebP): only small blobs are allowed.
        None => b.len() <= 2 * 1024 * 1024,
    }
}

// --------------------------------------------------------------- scrolling

/// With a bottom-anchored scrollable, how far (px) the viewport is from the
/// oldest (top) content.
pub fn distance_from_top(offset_from_bottom: f32, content_h: f32, viewport_h: f32) -> f32 {
    ((content_h - viewport_h).max(0.0) - offset_from_bottom).max(0.0)
}

/// Load the next older page when within this many px of the top.
pub const LOAD_THRESHOLD: f32 = 400.0;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Message as M;

    fn chat(id: ChatId, date: i64) -> Chat {
        let mut c = Chat::new(id, ChatKind::User, format!("c{id}"));
        c.last_date = date;
        c
    }

    #[test]
    fn new_message_bumps_unread_and_reorders() {
        let mut l = ChatList::new(vec![chat(1, 10), chat(2, 20)]);
        assert_eq!(l.all()[0].id, 2);
        l.apply_event(&Event::NewMessage(M::text(1, 5, false, 30, "hi")), None);
        assert_eq!(l.all()[0].id, 1);
        assert_eq!(l.get(1).unwrap().unread_count, 1);
        // Not counted when the chat is open, nor for outgoing.
        l.apply_event(&Event::NewMessage(M::text(1, 6, false, 31, "x")), Some(1));
        l.apply_event(&Event::NewMessage(M::text(1, 7, true, 32, "y")), None);
        assert_eq!(l.get(1).unwrap().unread_count, 1);
        assert_eq!(l.total_unread(), 1);
    }

    #[test]
    fn pinned_chats_stay_on_top() {
        let mut p = chat(3, 1);
        p.pinned = true;
        let mut l = ChatList::new(vec![chat(1, 10), p]);
        l.apply_event(&Event::NewMessage(M::text(1, 1, false, 99, "a")), None);
        assert_eq!(l.all()[0].id, 3);
    }

    #[test]
    fn unknown_chat_is_created() {
        let mut l = ChatList::new(vec![]);
        l.apply_event(&Event::NewMessage(M::text(9, 1, false, 1, "a")), None);
        assert_eq!(l.all().len(), 1);
        assert_eq!(l.get(9).unwrap().unread_count, 1);
    }

    #[test]
    fn read_events_update_counters() {
        let mut l = ChatList::new(vec![chat(1, 1)]);
        l.apply_event(&Event::ReadOutbox { chat: 1, max_id: 7 }, None);
        l.apply_event(&Event::ReadOutbox { chat: 1, max_id: 3 }, None);
        l.apply_event(
            &Event::ReadInbox {
                chat: 1,
                max_id: 9,
                still_unread: 2,
            },
            None,
        );
        let c = l.get(1).unwrap();
        assert_eq!(
            (c.read_outbox_max_id, c.read_inbox_max_id, c.unread_count),
            (7, 9, 2)
        );
        l.mark_read(1);
        assert_eq!(l.get(1).unwrap().unread_count, 0);
    }

    #[test]
    fn filter_is_case_insensitive() {
        let mut a = chat(1, 1);
        a.title = "Alice Johnson".into();
        let l = ChatList::new(vec![a, chat(2, 2)]);
        assert_eq!(l.filter("alice").len(), 1);
        assert_eq!(l.filter("  ").len(), 2);
        assert!(l.filter("zzz").is_empty());
    }

    #[test]
    fn conversation_merges_pages_in_order_without_duplicates() {
        let mut c = Conversation::new(1);
        assert_eq!(
            c.merge((11..=20).map(|i| M::text(1, i, false, 0, "")).collect()),
            10
        );
        assert_eq!(
            c.merge((5..=12).map(|i| M::text(1, i, false, 0, "")).collect()),
            6
        );
        let ids: Vec<_> = c.messages().iter().map(|m| m.id).collect();
        assert_eq!(ids, (5..=20).collect::<Vec<_>>());
        assert_eq!(c.oldest_id(), Some(5));
        // Messages for other chats are ignored.
        assert_eq!(c.merge(vec![M::text(2, 1, false, 0, "")]), 0);
    }

    #[test]
    fn pending_message_lifecycle() {
        let mut c = Conversation::new(1);
        c.merge(vec![M::text(1, 1, false, 0, "a")]);
        let t = c.push_pending(M::text(1, 0, true, 0, "b"));
        assert!(t < 0);
        assert_eq!(c.messages().last().unwrap().state, SendState::Sending);
        c.resolve_pending(t, Some(M::text(1, 2, true, 0, "b")));
        assert_eq!(
            c.messages().iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![1, 2]
        );
        let t2 = c.push_pending(M::text(1, 0, true, 0, "c"));
        c.resolve_pending(t2, None);
        assert_eq!(c.get(t2).unwrap().state, SendState::Failed);
    }

    #[test]
    fn events_edit_delete_react() {
        let mut c = Conversation::new(-5);
        let mut m = M::text(-5, 1, false, 0, "old");
        m.sender_name = Some("Ana".into());
        c.merge(vec![m, M::text(-5, 2, false, 0, "x")]);
        c.apply_event(&Event::MessageEdited(M::text(-5, 1, false, 0, "new")));
        assert_eq!(c.get(1).unwrap().text, "new");
        assert_eq!(c.get(1).unwrap().sender_name.as_deref(), Some("Ana"));
        c.apply_event(&Event::Reactions {
            chat: -5,
            id: 1,
            reactions: vec![Reaction {
                emoji: "🔥".into(),
                count: 3,
                chosen: false,
            }],
        });
        assert_eq!(c.get(1).unwrap().reactions[0].count, 3);
        c.apply_event(&Event::MessagesDeleted {
            chat: None,
            ids: vec![2],
        });
        assert!(c.get(2).is_none());
        // A channel deletion with another id does not apply.
        c.apply_event(&Event::MessagesDeleted {
            chat: Some(-100),
            ids: vec![1],
        });
        assert!(c.get(1).is_some());
    }

    #[test]
    fn trim_keeps_newest_window() {
        let mut c = Conversation::new(1);
        c.merge((1..=250).map(|i| M::text(1, i, false, 0, "")).collect());
        c.reached_start = true;
        c.trim();
        assert_eq!(c.messages().len(), MAX_WINDOW);
        assert_eq!(c.oldest_id(), Some(51));
        assert!(!c.reached_start);
    }

    #[test]
    fn reaction_toggle_semantics() {
        let mut r = vec![Reaction {
            emoji: "👍".into(),
            count: 2,
            chosen: false,
        }];
        toggle_reaction(&mut r, Some("👍"));
        assert_eq!((r[0].count, r[0].chosen), (3, true));
        toggle_reaction(&mut r, Some("❤️"));
        assert_eq!(r.len(), 2);
        assert_eq!((r[0].count, r[0].chosen), (2, false));
        toggle_reaction(&mut r, None);
        assert_eq!(r.len(), 1);

        let mut c = Conversation::new(1);
        c.merge(vec![M::text(1, 1, false, 0, "")]);
        assert_eq!(c.toggle_reaction(1, "😂"), Some(Some("😂".into())));
        assert_eq!(c.toggle_reaction(1, "😂"), Some(None));
        assert!(c.get(1).unwrap().reactions.is_empty());
    }

    #[test]
    fn notification_rules() {
        let mut ch = chat(1, 0);
        let m = M::text(1, 1, false, 0, "hello");
        assert!(should_notify(Some(&ch), &m, None, true, 5));
        assert!(should_notify(Some(&ch), &m, Some(1), false, 5));
        assert!(!should_notify(Some(&ch), &m, Some(1), true, 5));
        assert!(!should_notify(
            Some(&ch),
            &M::text(1, 1, true, 0, "x"),
            None,
            false,
            5
        ));
        // Catch-up after a reconnect: old messages do not notify.
        assert!(!should_notify(Some(&ch), &m, None, false, 10_000));
        ch.muted = true;
        assert!(!should_notify(Some(&ch), &m, None, false, 5));
        let (_, body) = notification_text(None, &m, false);
        assert_eq!(body, "New message");
        let (t, body) = notification_text(Some(&chat(1, 0)), &m, true);
        assert_eq!((t.as_str(), body.as_str()), ("c1", "hello"));
    }

    #[test]
    fn lru_respects_budget() {
        let mut l = Lru::new(100);
        l.insert(1, "a", 40);
        l.insert(2, "b", 40);
        assert!(l.get(&1).is_some()); // 1 is now most recent
        l.insert(3, "c", 40);
        assert!(l.peek(&2).is_none());
        assert!(l.peek(&1).is_some());
        assert_eq!(l.bytes(), 80);
        l.insert(4, "huge", 500); // over budget alone: kept, others evicted
        assert_eq!(l.len(), 1);
    }

    #[test]
    fn photo_limits() {
        let png = crate::mock::mock_photo_png(1, 64, 32);
        assert_eq!(image_dimensions(&png), Some((64, 32)));
        assert!(photo_is_acceptable(&png));
        // A forged PNG header claiming 100k x 100k pixels is rejected.
        let mut bomb = png.clone();
        bomb[16..20].copy_from_slice(&100_000u32.to_be_bytes());
        bomb[20..24].copy_from_slice(&100_000u32.to_be_bytes());
        assert!(!photo_is_acceptable(&bomb));
        // Minimal JPEG: SOI, APP0 (len 16), SOF0 with 480x640.
        let mut jpg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        jpg.extend_from_slice(&[0; 14]);
        jpg.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08, 0x01, 0xE0, 0x02, 0x80, 0x03]);
        jpg.extend_from_slice(&[0; 12]);
        assert_eq!(image_dimensions(&jpg), Some((640, 480)));
        assert!(!photo_is_acceptable(&vec![0u8; MAX_PHOTO_BYTES + 1]));
    }

    #[test]
    fn scroll_distance() {
        assert_eq!(distance_from_top(0.0, 2000.0, 500.0), 1500.0);
        assert_eq!(distance_from_top(1500.0, 2000.0, 500.0), 0.0);
        assert_eq!(distance_from_top(0.0, 100.0, 500.0), 0.0);
    }
}
