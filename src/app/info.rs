//! The chat info panel: profile data and the shared-content tabs (Media,
//! Files, Links, Music, Voice, GIFs) as pure reducers, plus the bounded
//! thumbnail queue. Unit-tested without iced.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;

use crate::model::{ChatId, MessageId, PeerInfo, SharedItem, SharedKind, SharedPage};

/// Items requested per page.
pub const PAGE: usize = 40;
/// Most items kept per tab (older ones are not loaded; keeps memory bounded).
pub const MAX_ITEMS: usize = 600;
/// Thumbnails downloaded at the same time.
pub const THUMB_CONCURRENCY: usize = 4;
/// Side in physical pixels of the square thumbnails kept in memory: exactly
/// the cell as it is drawn (`CELL` logical px times the window's scale
/// factor). The CPU renderer (tiny-skia, the default) blits an image whose
/// size matches its destination, but resamples every other one with a
/// bilinear filter on each frame; the grid draws ~20 of them per frame.
pub fn thumb_side(scale: f32) -> u32 {
    let scale = if scale.is_finite() {
        scale.clamp(0.5, 4.0)
    } else {
        1.0
    };
    (CELL * scale).round() as u32
}
/// Budget of the in-memory thumbnail cache (decoded RGBA: about 100 KB each,
/// so ~160 thumbnails, several screens of the grid).
pub const THUMB_BUDGET: usize = 16 * 1024 * 1024;

// ---- the thumbnail grid: fixed geometry, so only visible rows are built
/// Side of one grid cell (px).
pub const CELL: f32 = 104.0;
/// Gap between cells.
pub const GAP: f32 = 2.0;
pub const COLUMNS: usize = 3;
/// Height of one grid row including its gap: rows are laid out at fixed
/// positions, which is what makes skipping the invisible ones exact.
pub const ROW_H: f32 = CELL + GAP;
/// Space above the first row.
pub const GRID_PAD: f32 = 2.0;
/// Fixed-height area under the grid ("Loading…", retry).
pub const FOOTER_H: f32 = 56.0;
/// Rows built beyond the visible ones on each side.
pub const OVERSCAN_ROWS: usize = 3;

/// Rows needed for `items` thumbnails.
pub fn grid_rows(items: usize) -> usize {
    items.div_ceil(COLUMNS)
}

/// Height of the whole grid (all rows, built or not).
pub fn grid_height(items: usize) -> f32 {
    GRID_PAD + grid_rows(items) as f32 * ROW_H
}

/// What the scrollable last reported (scroll offset and size) and where
/// the grid starts inside it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewState {
    pub scroll_y: f32,
    pub viewport_h: f32,
    /// Distance from the top of the scrolled content to the first row.
    pub grid_top: f32,
}

impl Default for ViewState {
    /// Before the first scroll event: the top of the panel in a tall window.
    fn default() -> Self {
        Self {
            scroll_y: 0.0,
            viewport_h: 800.0,
            grid_top: 420.0,
        }
    }
}

/// Rows `first..last` that intersect the viewport, extended by `overscan`
/// rows on both sides, clamped to `total_rows`. Never panics on odd floats.
pub fn row_window(total_rows: usize, v: &ViewState, overscan: usize) -> Range<usize> {
    let finite = |x: f32| if x.is_finite() { x } else { 0.0 };
    let top = finite(v.scroll_y) - finite(v.grid_top) - GRID_PAD;
    let bottom = top + finite(v.viewport_h).max(0.0);
    let first = (top / ROW_H).floor().max(0.0) as usize;
    let last = (bottom / ROW_H).ceil().max(0.0) as usize;
    let start = first.saturating_sub(overscan).min(total_rows);
    let end = last.saturating_add(overscan).min(total_rows).max(start);
    start..end
}
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
    /// Scroll position, for building only the visible grid rows.
    pub view: ViewState,
}

impl InfoPanel {
    pub fn new(chat: ChatId) -> Self {
        Self {
            chat,
            info: None,
            info_failed: false,
            tab: SharedKind::Media,
            tabs: HashMap::new(),
            view: ViewState::default(),
        }
    }

    /// Record the scrollable's state. `grid_top` is where the first row
    /// starts inside the scrolled content.
    pub fn set_view(&mut self, scroll_y: f32, viewport_h: f32, grid_top: f32) {
        self.view = ViewState {
            scroll_y,
            viewport_h,
            grid_top,
        };
    }

    /// Where the grid starts, derived from the scrollable's content height:
    /// everything that is not the (fixed-height) grid and footer is header.
    pub fn grid_top_from_content(&self, content_h: f32) -> f32 {
        let items = self.current().map_or(0, |t| t.items.len());
        (content_h - grid_height(items) - FOOTER_H).max(0.0)
    }

    /// Grid rows to build for the current tab.
    pub fn window(&self, kind: SharedKind) -> Range<usize> {
        let total = grid_rows(self.tab_ref(kind).map_or(0, |t| t.items.len()));
        row_window(total, &self.view, OVERSCAN_ROWS)
    }

    /// Message ids whose thumbnails the current window needs, the visible
    /// ones first, then the rows around them.
    pub fn wanted_thumbs(&self) -> Vec<MessageId> {
        let kind = self.tab;
        let Some(tab) = self.tab_ref(kind).filter(|_| kind.has_thumbs()) else {
            return Vec::new();
        };
        let total = grid_rows(tab.items.len());
        let visible = row_window(total, &self.view, 0);
        let all = row_window(total, &self.view, OVERSCAN_ROWS);
        let ids = |rows: Range<usize>| -> Vec<MessageId> {
            tab.items
                .iter()
                .skip(rows.start * COLUMNS)
                .take(rows.len() * COLUMNS)
                .filter(|i| i.has_thumb())
                .map(|i| i.id)
                .collect()
        };
        let mut out = ids(visible.clone());
        out.extend(ids(visible.end..all.end));
        out.extend(ids(all.start..visible.start).into_iter().rev());
        out
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

    /// Replace the waiting keys with exactly `keys` (in priority order),
    /// minus those cached, loading or failed: rows scrolled away from stop
    /// being wanted.
    pub fn set_wanted(
        &mut self,
        keys: impl IntoIterator<Item = (ChatId, MessageId)>,
        have: impl Fn(&(ChatId, MessageId)) -> bool,
    ) {
        self.queue.clear();
        for k in keys {
            if have(&k) || self.inflight.contains(&k) || self.failed.contains(&k) {
                continue;
            }
            if self.queue.len() >= THUMB_QUEUE_MAX {
                break;
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

    fn panel_with(n: i32) -> InfoPanel {
        let mut p = InfoPanel::new(1);
        let t = p.tab_mut(SharedKind::Media);
        t.items = (0..n).map(|i| photo(10_000 - i)).collect();
        t.loaded = true;
        p
    }

    #[test]
    fn thumbnails_match_the_drawn_cell_at_every_scale() {
        assert_eq!(thumb_side(1.0), 104);
        assert_eq!(thumb_side(1.5), 156);
        assert_eq!(thumb_side(2.0), 208);
        assert_eq!(thumb_side(f32::NAN), 104);
        assert_eq!(thumb_side(0.0), 52);
        assert_eq!(thumb_side(100.0), 416);
    }

    #[test]
    fn window_covers_the_viewport_plus_overscan_and_nothing_else() {
        let v = ViewState {
            scroll_y: 0.0,
            viewport_h: 700.0,
            grid_top: 400.0,
        };
        // Only the part of the viewport below the header shows rows.
        let w = row_window(200, &v, 0);
        assert_eq!(w, 0..3, "300 px of grid visible = 3 rows");
        assert_eq!(row_window(200, &v, OVERSCAN_ROWS), 0..6);
        // Scrolled well into a 200-row grid.
        let v = ViewState {
            scroll_y: 400.0 + 50.0 * ROW_H,
            viewport_h: 700.0,
            grid_top: 400.0,
        };
        let w = row_window(200, &v, OVERSCAN_ROWS);
        assert!(w.start <= 50 && w.end >= 57, "{w:?}");
        assert!(
            w.len() <= 7 + 2 * OVERSCAN_ROWS + 1,
            "bounded by the viewport: {w:?}"
        );
        // Past the end / short lists / odd numbers never panic.
        assert!(row_window(5, &v, OVERSCAN_ROWS).end <= 5);
        assert_eq!(row_window(0, &v, 3), 0..0);
        for bad in [f32::NAN, f32::INFINITY, -1e9, 1e30] {
            let v = ViewState {
                scroll_y: bad,
                viewport_h: bad,
                grid_top: bad,
            };
            let w = row_window(10, &v, 2);
            assert!(w.start <= w.end && w.end <= 10);
        }
    }

    #[test]
    fn window_size_does_not_depend_on_the_item_count() {
        let v = ViewState {
            scroll_y: 3000.0,
            viewport_h: 700.0,
            grid_top: 420.0,
        };
        let small = panel_with(300);
        let big = panel_with(600);
        let (mut a, mut b) = (small, big);
        a.view = v;
        b.view = v;
        assert_eq!(a.window(SharedKind::Media), b.window(SharedKind::Media));
        assert!(b.window(SharedKind::Media).len() < 15, "of 200 rows");
    }

    #[test]
    fn grid_top_is_recovered_from_the_content_height() {
        let p = panel_with(90);
        let header = 377.0;
        let content = header + grid_height(90) + FOOTER_H;
        assert!((p.grid_top_from_content(content) - header).abs() < 0.01);
        assert_eq!(p.grid_top_from_content(10.0), 0.0, "never negative");
    }

    #[test]
    fn wanted_thumbnails_are_the_visible_rows_first_then_the_margin() {
        let mut p = panel_with(120);
        p.set_view(420.0 + 10.0 * ROW_H, 4.0 * ROW_H, 420.0);
        let wanted = p.wanted_thumbs();
        let visible = row_window(40, &p.view, 0);
        let all = row_window(40, &p.view, OVERSCAN_ROWS);
        assert_eq!(wanted.len(), all.len() * COLUMNS);
        // The first ones are exactly the visible rows, top to bottom.
        let first_visible = 10_000 - (visible.start * COLUMNS) as i32;
        assert_eq!(wanted[0], first_visible);
        assert!(wanted.len() < 40, "never the whole list");
        // No duplicates.
        let set: HashSet<_> = wanted.iter().collect();
        assert_eq!(set.len(), wanted.len());
        // Tabs without thumbnails want none.
        p.tab = SharedKind::Files;
        assert!(p.wanted_thumbs().is_empty());
    }

    #[test]
    fn set_wanted_drops_rows_scrolled_away_from() {
        let mut q = ThumbQueue::default();
        q.set_wanted([(1, 1), (1, 2), (1, 3)], |_| false);
        let started = q.take_ready();
        assert_eq!(started.len(), 3);
        q.set_wanted([(1, 4), (1, 5), (1, 2), (1, 6)], |k| k.1 == 6);
        // 2 is in flight, 6 is cached: only 4 and 5 wait.
        q.finish((1, 2), false);
        let next = q.take_ready();
        assert_eq!(next, vec![(1, 4), (1, 5)]);
        // A failed one is not asked for again.
        q.set_wanted([(1, 2)], |_| false);
        assert!(q.take_ready().is_empty());
        q.set_wanted((0..1000).map(|i| (1, i)), |_| false);
        assert!(q.pending() <= THUMB_QUEUE_MAX + THUMB_CONCURRENCY);
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
