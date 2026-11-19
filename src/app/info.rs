//! The chat info panel: profile data and the shared-content tabs (Media,
//! Files, Links, Music, Voice, GIFs) as pure reducers, plus the bounded
//! thumbnail queue. Unit-tested without iced.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::model::{
    ChatId, MessageId, PeerInfo, SharedItem, SharedKind, SharedPage,
};

/// Items requested per page.
pub const PAGE: usize = 40;
/// Most items kept per tab (older ones are not loaded; keeps memory bounded).
pub const MAX_ITEMS: usize = 600;
/// Thumbnails downloaded at the same time.
pub const THUMB_CONCURRENCY: usize = 4;
/// Budget of the in-memory thumbnail cache (encoded bytes).
pub const THUMB_BUDGET: usize = 12 * 1024 * 1024;
/// Thumbnails waiting for a download slot (older ones are dropped).
pub const THUMB_QUEUE_MAX: usize = 240;

/// One tab's list and paging state.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct SharedTab {
    pub items: Vec<SharedItem>,
    /// Offset of the next page; `None` once the end was reached.
    pub next: Option<MessageId>,
    /// The first page arrived.
    pub loaded: bool,
    /// The offset of the request in flight.
    pub requested: Option<MessageId>,
    pub failed: bool,
    /// `MAX_ITEMS` reached: no further pages are fetched.
    pub capped: bool,
}

impl SharedTab {
    pub fn loading(&self) -> bool {
        self.requested.is_some()
    }

    /// Everything the server has was loaded (or the cap was hit).
    pub fn ended(&self) -> bool {
        self.capped || (self.loaded && self.next.is_none())
    }

    /// The offset to request next (0 = newest) when a request makes sense now.
    pub fn want(&self) -> Option<MessageId> {
        if self.loading() || self.ended() || self.failed {
            return None;
        }
        Some(if self.loaded { self.next? } else { 0 })
    }

    /// Mark the request for `offset` as sent.
    pub fn start(&mut self, offset: MessageId) {
        self.requested = Some(offset);
        self.failed = false;
    }

    /// A page arrived. Returns the new items' ids that have a thumbnail. A
    /// page that is not the one asked for (stale) is ignored.
    pub fn apply(&mut self, offset: MessageId, page: SharedPage) -> Vec<MessageId> {
        if self.requested != Some(offset) {
            return Vec::new();
        }
        self.requested = None;
        self.loaded = true;
        self.failed = false;
        let have: HashSet<MessageId> = self.items.iter().map(|i| i.id).collect();
        let mut new = Vec::new();
        for item in page.items {
            if have.contains(&item.id) || self.items.len() >= MAX_ITEMS {
                continue;
            }
            if item.has_thumb() {
                new.push(item.id);
            }
            self.items.push(item);
        }
        // A cursor that did not move would loop forever.
        let progressed = page.next.is_some_and(|n| offset == 0 || n < offset);
        self.next = page.next.filter(|_| progressed);
        self.capped = self.items.len() >= MAX_ITEMS && self.next.is_some();
        new
    }

    pub fn fail(&mut self, offset: MessageId) {
        if self.requested == Some(offset) {
            self.requested = None;
            self.failed = true;
        }
    }
}

/// The open panel.
#[derive(Debug, Clone, PartialEq)]
pub struct InfoPanel {
    pub chat: ChatId,
    pub info: Option<PeerInfo>,
    pub info_failed: bool,
    pub tab: SharedKind,
    tabs: HashMap<SharedKind, SharedTab>,
}

impl InfoPanel {
    pub fn new(chat: ChatId) -> Self {
        Self {
            chat,
            info: None,
            info_failed: false,
            tab: SharedKind::Media,
            tabs: HashMap::new(),
        }
    }

    pub fn tab(&self, kind: SharedKind) -> SharedTab {
        self.tabs.get(&kind).cloned().unwrap_or_default()
    }

    pub fn tab_ref(&self, kind: SharedKind) -> Option<&SharedTab> {
        self.tabs.get(&kind)
    }

    pub fn tab_mut(&mut self, kind: SharedKind) -> &mut SharedTab {
        self.tabs.entry(kind).or_default()
    }

    pub fn current(&self) -> Option<&SharedTab> {
        self.tabs.get(&self.tab)
    }

    /// Switch tab; a failed tab is retried by selecting it again.
    pub fn select(&mut self, kind: SharedKind) {
        self.tab = kind;
        if let Some(t) = self.tabs.get_mut(&kind) {
            t.failed = false;
        }
    }
}

/// Thumbnails to download: bounded queue, a few at a time, never twice.
#[derive(Debug, Default)]
pub struct ThumbQueue {
    queue: VecDeque<(ChatId, MessageId)>,
    inflight: HashSet<(ChatId, MessageId)>,
    failed: HashSet<(ChatId, MessageId)>,
}

impl ThumbQueue {
    /// Queue thumbnails (newest first = the order given). Keys already
    /// queued, loading, failed or satisfying `have` are skipped; the queue
    /// keeps only the most recent `THUMB_QUEUE_MAX`.
    pub fn enqueue(
        &mut self,
        keys: impl IntoIterator<Item = (ChatId, MessageId)>,
        have: impl Fn(&(ChatId, MessageId)) -> bool,
    ) {
        for k in keys {
            if have(&k)
                || self.inflight.contains(&k)
                || self.failed.contains(&k)
                || self.queue.contains(&k)
            {
                continue;
            }
            if self.queue.len() >= THUMB_QUEUE_MAX {
                self.queue.pop_front();
            }
            self.queue.push_back(k);
        }
    }

    /// Start as many downloads as free slots allow.
    pub fn take_ready(&mut self) -> Vec<(ChatId, MessageId)> {
        let mut out = Vec::new();
        while self.inflight.len() < THUMB_CONCURRENCY {
            let Some(k) = self.queue.pop_front() else {
                break;
            };
            self.inflight.insert(k);
            out.push(k);
        }
        out
    }

    pub fn finish(&mut self, key: (ChatId, MessageId), ok: bool) {
        self.inflight.remove(&key);
        if !ok {
            if self.failed.len() >= 4096 {
                self.failed.clear();
            }
            self.failed.insert(key);
        }
    }

    pub fn clear(&mut self) {
        self.queue.clear();
        self.inflight.clear();
        self.failed.clear();
    }

    pub fn pending(&self) -> usize {
        self.queue.len() + self.inflight.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SharedBody;

    fn photo(id: i32) -> SharedItem {
        SharedItem {
            id,
            date: 0,
            body: SharedBody::Photo {
                width: 1,
                height: 1,
            },
        }
    }

    fn file(id: i32) -> SharedItem {
        SharedItem {
            id,
            date: 0,
            body: SharedBody::File {
                name: "a".into(),
                size: 1,
                mime: String::new(),
            },
        }
    }

    fn page(ids: &[i32], next: Option<i32>) -> SharedPage {
        SharedPage {
            items: ids.iter().map(|i| photo(*i)).collect(),
            next,
        }
    }

    #[test]
    fn pages_append_in_order_and_stop_at_the_end() {
        let mut t = SharedTab::default();
        assert_eq!(t.want(), Some(0), "first page starts from the newest");
        t.start(0);
        assert_eq!(t.want(), None, "one request at a time");
        let new = t.apply(0, page(&[90, 80, 70], Some(70)));
        assert_eq!(new, vec![90, 80, 70]);
        assert!(t.loaded && !t.ended());
        assert_eq!(t.want(), Some(70));
        t.start(70);
        t.apply(70, page(&[60, 50], None));
        assert!(t.ended());
        assert_eq!(t.want(), None);
        let ids: Vec<_> = t.items.iter().map(|i| i.id).collect();
        assert_eq!(ids, vec![90, 80, 70, 60, 50]);
    }

    #[test]
    fn stale_duplicate_and_stuck_pages_are_harmless() {
        let mut t = SharedTab::default();
        t.start(0);
        // An answer for another offset is ignored.
        assert!(t.apply(55, page(&[1], None)).is_empty());
        assert!(t.items.is_empty() && t.loading());
        t.apply(0, page(&[90, 80], Some(80)));
        t.start(80);
        // Overlapping page: duplicates dropped; a cursor that does not move
        // ends the list instead of looping.
        let new = t.apply(80, page(&[80, 79], Some(80)));
        assert_eq!(new, vec![79]);
        assert!(t.ended(), "stuck cursor stops paging");
        assert_eq!(t.items.len(), 3);
    }

    #[test]
    fn only_items_with_previews_are_listed_for_thumbnails() {
        let mut t = SharedTab::default();
        t.start(0);
        let mut p = page(&[5], None);
        p.items.push(file(4));
        assert_eq!(t.apply(0, p), vec![5]);
    }

    #[test]
    fn failures_can_be_retried_by_selecting_the_tab_again() {
        let mut t = SharedTab::default();
        t.start(0);
        t.fail(0);
        assert!(t.failed && !t.loading());
        assert_eq!(t.want(), None, "no automatic retry loop");
        let mut p = InfoPanel::new(5);
        *p.tab_mut(SharedKind::Files) = t;
        p.select(SharedKind::Files);
        assert_eq!(p.current().unwrap().want(), Some(0));
        // A failure for another request does not clear a running one.
        let mut u = SharedTab::default();
        u.start(0);
        u.fail(77);
        assert!(u.loading());
    }

    #[test]
    fn a_tab_is_capped() {
        let mut t = SharedTab::default();
        let mut offset = 0;
        let mut id = 100_000;
        loop {
            t.start(offset);
            let ids: Vec<i32> = (0..PAGE as i32).map(|i| id - i).collect();
            id -= PAGE as i32;
            t.apply(offset, page(&ids, Some(id + 1)));
            offset = id + 1;
            if t.ended() {
                break;
            }
        }
        assert_eq!(t.items.len(), MAX_ITEMS);
        assert!(t.capped);
        assert_eq!(t.want(), None);
    }

    #[test]
    fn panel_remembers_each_tab_separately() {
        let mut p = InfoPanel::new(9);
        assert_eq!(p.tab, SharedKind::Media);
        p.tab_mut(SharedKind::Links).start(0);
        p.select(SharedKind::Links);
        assert!(p.current().unwrap().loading());
        p.select(SharedKind::Media);
        assert!(p.current().is_none(), "media not started yet");
        assert!(p.tab(SharedKind::Links).loading());
    }

    #[test]
    fn thumb_queue_is_bounded_deduplicated_and_rate_limited() {
        let mut q = ThumbQueue::default();
        q.enqueue((1..=10).map(|i| (7, i)), |_| false);
        let first = q.take_ready();
        assert_eq!(first.len(), THUMB_CONCURRENCY);
        assert!(q.take_ready().is_empty(), "slots are full");
        // Already loading / queued keys are not queued twice.
        q.enqueue([(7, 1), (7, 5)], |_| false);
        assert_eq!(q.pending(), 10);
        // Finishing frees a slot; failures are never retried.
        q.finish(first[0], false);
        assert_eq!(q.take_ready().len(), 1);
        q.enqueue([first[0]], |_| false);
        assert_eq!(q.pending(), 10 - 1, "failed key stays out");
        // Cached thumbnails are skipped.
        q.enqueue([(7, 99)], |k| k.1 == 99);
        assert_eq!(q.pending(), 9);
        // The queue never grows past its bound: old entries make room.
        let mut big = ThumbQueue::default();
        big.enqueue((0..1000).map(|i| (1, i)), |_| false);
        assert_eq!(big.pending(), THUMB_QUEUE_MAX);
        big.clear();
        assert_eq!(big.pending(), 0);
    }
}
