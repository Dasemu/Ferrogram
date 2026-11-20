//! State of the chat header: the ⋮ menu, the confirmation dialogs, message
//! selection and the in-chat search, as pure reducers (unit-tested without
//! iced). Talking to Telegram lives in `app/chat_actions.rs`.

use std::collections::BTreeSet;

use crate::model::{CHANNEL_OFFSET, Chat, ChatId, ChatKind, Member, Message, MessageId};

/// Results requested per `messages.search` page.
pub const SEARCH_PAGE: usize = 50;
/// Most results kept for one search (older ones are not loaded).
pub const SEARCH_MAX: usize = 500;
/// Pause after the last keystroke before the search runs.
pub const SEARCH_DEBOUNCE_MS: u64 = 350;
/// Most group members offered by the "from:" filter.
pub const MEMBERS_MAX: usize = 200;

// ------------------------------------------------------------------ ⋮ menu

/// Which page of the header menu is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderMenu {
    Main,
    /// "Mute for…" durations.
    Mute,
    /// "Add to folder" list.
    Folders,
}

/// One entry of the main page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItem {
    /// Toggle the info panel (`open` = it is showing now).
    Info {
        open: bool,
    },
    /// Opens the durations page.
    Mute,
    Unmute,
    Search,
    Select,
    /// Opens the folders page.
    AddToFolder,
    ClearHistory,
    DeleteChat,
    LeaveGroup,
    LeaveChannel,
    Block,
    Unblock,
}

/// Basic groups have small ids; supergroups and channels are `-100…`.
pub fn is_basic_group(id: ChatId) -> bool {
    id < 0 && id > -CHANNEL_OFFSET
}

/// The entries the official client offers for this kind of chat. `blocked`
/// is `None` until we know (it is fetched when the menu opens).
pub fn menu_items(chat: &Chat, info_open: bool, blocked: Option<bool>) -> Vec<MenuItem> {
    use MenuItem as M;
    let mut v = Vec::new();
    if chat.kind != ChatKind::SavedMessages {
        v.push(M::Info { open: info_open });
        v.push(if chat.muted { M::Unmute } else { M::Mute });
    }
    v.push(M::Search);
    v.push(M::Select);
    v.push(M::AddToFolder);
    match chat.kind {
        ChatKind::User | ChatKind::Bot => {
            v.push(M::ClearHistory);
            v.push(M::DeleteChat);
            v.push(if blocked == Some(true) {
                M::Unblock
            } else {
                M::Block
            });
        }
        ChatKind::SavedMessages => v.push(M::ClearHistory),
        ChatKind::Group => {
            // Only admins can clear a supergroup, and we do not know our
            // rights: offer it for basic groups only.
            if is_basic_group(chat.id) {
                v.push(M::ClearHistory);
            }
            v.push(M::LeaveGroup);
        }
        ChatKind::Channel => v.push(M::LeaveChannel),
    }
    v
}

// ------------------------------------------------------------- confirmations

/// A destructive action waiting for the user's "yes".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    ClearHistory {
        chat: ChatId,
        revoke: bool,
    },
    /// Delete a private chat.
    DeleteChat {
        chat: ChatId,
        revoke: bool,
    },
    /// Leave a group or channel.
    Leave {
        chat: ChatId,
    },
    DeleteMessages {
        chat: ChatId,
        ids: Vec<MessageId>,
    },
}

impl Confirm {
    pub fn chat(&self) -> ChatId {
        match self {
            Self::ClearHistory { chat, .. }
            | Self::DeleteChat { chat, .. }
            | Self::Leave { chat }
            | Self::DeleteMessages { chat, .. } => *chat,
        }
    }

    /// The "also delete for the other side" checkbox state, when the dialog
    /// has one (private chats only).
    pub fn revoke(&self) -> Option<bool> {
        match self {
            Self::ClearHistory { revoke, .. } | Self::DeleteChat { revoke, .. } => Some(*revoke),
            _ => None,
        }
    }

    pub fn set_revoke(&mut self, value: bool) {
        if let Self::ClearHistory { revoke, .. } | Self::DeleteChat { revoke, .. } = self {
            *revoke = value;
        }
    }

    pub fn title(&self, kind: ChatKind) -> String {
        match self {
            Self::ClearHistory { .. } => "Clear history".into(),
            Self::DeleteChat { .. } => "Delete chat".into(),
            Self::Leave { .. } if kind == ChatKind::Channel => "Leave channel".into(),
            Self::Leave { .. } => "Leave group".into(),
            Self::DeleteMessages { ids, .. } if ids.len() == 1 => "Delete message".into(),
            Self::DeleteMessages { .. } => "Delete messages".into(),
        }
    }

    pub fn body(&self, kind: ChatKind, title: &str) -> String {
        match self {
            Self::ClearHistory { .. } => {
                format!("Delete all messages in the chat with {title}? This can't be undone.")
            }
            Self::DeleteChat { .. } => {
                format!(
                    "Delete the chat with {title}? The messages are removed from this chat list."
                )
            }
            Self::Leave { .. } if kind == ChatKind::Channel => {
                format!("Leave the channel {title}?")
            }
            Self::Leave { .. } => format!("Leave the group {title}?"),
            Self::DeleteMessages { ids, .. } if ids.len() == 1 => {
                "Delete this message? This can't be undone.".into()
            }
            Self::DeleteMessages { ids, .. } => {
                format!("Delete {} messages? This can't be undone.", ids.len())
            }
        }
    }

    pub fn accept_label(&self, kind: ChatKind) -> &'static str {
        match self {
            Self::ClearHistory { .. } => "Clear history",
            Self::DeleteChat { .. } => "Delete chat",
            Self::Leave { .. } if kind == ChatKind::Channel => "Leave channel",
            Self::Leave { .. } => "Leave group",
            Self::DeleteMessages { .. } => "Delete",
        }
    }
}

// ----------------------------------------------------------------- selection

/// "Select messages" mode: the ticked messages of the open chat.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    pub ids: BTreeSet<MessageId>,
}

impl Selection {
    /// Tick or untick. Messages still being sent (ids <= 0) cannot be picked.
    pub fn toggle(&mut self, id: MessageId) {
        if id <= 0 {
            return;
        }
        if !self.ids.remove(&id) {
            self.ids.insert(id);
        }
    }

    pub fn contains(&self, id: MessageId) -> bool {
        self.ids.contains(&id)
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

// ------------------------------------------------------------------- search

/// Which way a search step goes. Results run newest first, so "older" is
/// the up arrow (towards the top of the chat).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Older,
    Newer,
}

/// What a step asks the app to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Jump(MessageId),
    /// Past the last loaded result and the server may have more.
    NeedMore,
    Nothing,
}

/// A page that matches the request in flight, applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// Not the answer to the current query (changed meanwhile).
    Stale,
    Page {
        /// The message to show now (the first hit, or the one the user
        /// stepped towards while the page was loading).
        jump: Option<MessageId>,
    },
}

/// The in-chat search of the open chat.
#[derive(Debug, Clone, Default)]
pub struct ChatSearch {
    pub query: String,
    /// Only messages of this member (groups).
    pub from: Option<Member>,
    /// Newest first, as `messages.search` returns them.
    pub results: Vec<Message>,
    /// The hit on screen.
    pub index: Option<usize>,
    pub searching: bool,
    /// The first page of the current query arrived.
    pub searched: bool,
    /// The server has nothing older than the last result.
    pub exhausted: bool,
    /// Bumped whenever the query or filter changes, so late answers for an
    /// old query are ignored.
    pub version: u64,
    /// The user stepped past the end: show the next hit when its page lands.
    pub advance: bool,
    /// Members for the "from:" picker, once fetched.
    pub members: Option<Vec<Member>>,
    pub members_loading: bool,
    pub from_open: bool,
    pub from_query: String,
}

impl ChatSearch {
    /// Nothing to search for (no text, no member filter).
    pub fn is_blank(&self) -> bool {
        self.query.trim().is_empty() && self.from.is_none()
    }

    /// The query or filter changed: drop the results and invalidate every
    /// request in flight. Returns the new version.
    pub fn reset(&mut self) -> u64 {
        self.version += 1;
        self.results.clear();
        self.index = None;
        self.searching = false;
        self.searched = false;
        self.exhausted = false;
        self.advance = false;
        self.version
    }

    /// The `offset_id` of the next page (0 = the newest).
    pub fn next_offset(&self) -> MessageId {
        self.results.last().map_or(0, |m| m.id)
    }

    /// Mark a request as started.
    pub fn start(&mut self) {
        self.searching = true;
    }

    /// A page for (`version`, `offset`) arrived.
    pub fn apply_page(&mut self, version: u64, offset: MessageId, page: Vec<Message>) -> Applied {
        if version != self.version || offset != self.next_offset() || !self.searching {
            return Applied::Stale;
        }
        self.searching = false;
        self.searched = true;
        self.exhausted = page.len() < SEARCH_PAGE;
        let first_new = self.results.len();
        for m in page {
            if self.results.len() >= SEARCH_MAX {
                self.exhausted = true;
                break;
            }
            if !self.results.iter().any(|r| r.id == m.id) {
                self.results.push(m);
            }
        }
        if self.results.len() == first_new {
            // An empty or all-duplicate page ends the list.
            self.exhausted = true;
        }
        let target = if first_new == 0 {
            (!self.results.is_empty()).then_some(0)
        } else if self.advance {
            (self.results.len() > first_new).then_some(first_new)
        } else {
            None
        };
        self.advance = false;
        let jump = target.map(|i| {
            self.index = Some(i);
            self.results[i].id
        });
        Applied::Page { jump }
    }

    /// The request failed: allow another try.
    pub fn fail(&mut self, version: u64) {
        if version == self.version {
            self.searching = false;
            self.advance = false;
        }
    }

    /// Move to the next hit in `dir`.
    pub fn step(&mut self, dir: Dir) -> Step {
        let len = self.results.len();
        if len == 0 {
            return Step::Nothing;
        }
        let target = match (dir, self.index) {
            (_, None) => Some(0),
            (Dir::Older, Some(i)) if i + 1 < len => Some(i + 1),
            (Dir::Newer, Some(i)) if i > 0 => Some(i - 1),
            (Dir::Older, Some(_)) => {
                return if self.exhausted || self.searching {
                    Step::Nothing
                } else {
                    self.advance = true;
                    Step::NeedMore
                };
            }
            (Dir::Newer, Some(_)) => None,
        };
        match target {
            Some(i) => {
                self.index = Some(i);
                Step::Jump(self.results[i].id)
            }
            None => Step::Nothing,
        }
    }

    /// Can the arrows do something right now?
    pub fn can_step(&self, dir: Dir) -> bool {
        match (dir, self.index) {
            _ if self.results.is_empty() => false,
            (_, None) => true,
            (Dir::Older, Some(i)) => i + 1 < self.results.len() || !self.exhausted,
            (Dir::Newer, Some(i)) => i > 0,
        }
    }

    /// "3 of 12" (a "+" when there may be more), "Searching…", "No results".
    pub fn counter(&self) -> String {
        if self.is_blank() {
            return String::new();
        }
        match self.index {
            Some(i) => format!(
                "{} of {}{}",
                i + 1,
                self.results.len(),
                if self.exhausted { "" } else { "+" }
            ),
            None if self.searching => "Searching…".to_owned(),
            None if self.searched => "No results".to_owned(),
            None => String::new(),
        }
    }

    /// Members matching the text typed in the picker.
    pub fn visible_members(&self) -> Vec<&Member> {
        let q = self.from_query.trim().to_lowercase();
        self.members
            .iter()
            .flatten()
            .filter(|m| q.is_empty() || m.name.to_lowercase().contains(&q))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(id: ChatId, kind: ChatKind) -> Chat {
        Chat::new(id, kind, "T")
    }

    fn msgs(ids: impl IntoIterator<Item = i32>) -> Vec<Message> {
        ids.into_iter()
            .map(|i| Message::text(1, i, false, 0, "x"))
            .collect()
    }

    fn started(s: &mut ChatSearch) -> u64 {
        let v = s.reset();
        s.start();
        v
    }

    #[test]
    fn menu_per_chat_kind() {
        use MenuItem as M;
        let user = menu_items(&chat(5, ChatKind::User), false, None);
        assert_eq!(
            user,
            vec![
                M::Info { open: false },
                M::Mute,
                M::Search,
                M::Select,
                M::AddToFolder,
                M::ClearHistory,
                M::DeleteChat,
                M::Block
            ]
        );
        assert!(menu_items(&chat(5, ChatKind::User), false, Some(true)).contains(&M::Unblock));
        let mut muted = chat(5, ChatKind::Bot);
        muted.muted = true;
        let bot = menu_items(&muted, true, Some(false));
        assert!(bot.contains(&M::Unmute) && !bot.contains(&M::Mute));
        assert!(bot.contains(&M::Info { open: true }) && bot.contains(&M::Block));

        let basic = menu_items(&chat(-7, ChatKind::Group), false, None);
        assert!(basic.contains(&M::ClearHistory) && basic.contains(&M::LeaveGroup));
        assert!(!basic.contains(&M::Block) && !basic.contains(&M::DeleteChat));
        let mega = menu_items(&chat(-CHANNEL_OFFSET - 7, ChatKind::Group), false, None);
        assert!(!mega.contains(&M::ClearHistory), "admins only");
        assert!(mega.contains(&M::LeaveGroup));
        let chan = menu_items(&chat(-CHANNEL_OFFSET - 8, ChatKind::Channel), false, None);
        assert!(chan.contains(&M::LeaveChannel) && !chan.contains(&M::LeaveGroup));
        assert!(!chan.contains(&M::ClearHistory));
        let saved = menu_items(&chat(9, ChatKind::SavedMessages), false, None);
        assert_eq!(
            saved,
            vec![M::Search, M::Select, M::AddToFolder, M::ClearHistory]
        );
    }

    #[test]
    fn confirm_texts_and_revoke() {
        let mut c = Confirm::ClearHistory {
            chat: 5,
            revoke: false,
        };
        assert_eq!(c.revoke(), Some(false));
        c.set_revoke(true);
        assert_eq!(c.revoke(), Some(true));
        assert_eq!(c.title(ChatKind::User), "Clear history");
        let leave = Confirm::Leave { chat: -7 };
        assert_eq!(leave.revoke(), None);
        assert_eq!(leave.title(ChatKind::Channel), "Leave channel");
        assert_eq!(leave.title(ChatKind::Group), "Leave group");
        assert!(leave.body(ChatKind::Group, "Rust").contains("Rust"));
        let del = Confirm::DeleteMessages {
            chat: 5,
            ids: vec![1, 2, 3],
        };
        assert_eq!(del.title(ChatKind::User), "Delete messages");
        assert!(del.body(ChatKind::User, "A").contains('3'));
        assert_eq!(del.chat(), 5);
    }

    #[test]
    fn selection_toggles_and_ignores_unsent() {
        let mut s = Selection::default();
        s.toggle(4);
        s.toggle(2);
        s.toggle(-1);
        s.toggle(0);
        assert_eq!(s.len(), 2);
        assert_eq!(s.ids.iter().copied().collect::<Vec<_>>(), vec![2, 4]);
        s.toggle(4);
        assert!(!s.contains(4) && s.contains(2));
        s.toggle(2);
        assert!(s.is_empty());
    }

    #[test]
    fn first_page_selects_the_newest_hit() {
        let mut s = ChatSearch {
            query: "hi".into(),
            ..ChatSearch::default()
        };
        let v = started(&mut s);
        let page = msgs([90, 80, 70]);
        assert_eq!(s.apply_page(v, 0, page), Applied::Page { jump: Some(90) });
        assert_eq!(s.index, Some(0));
        assert!(s.exhausted, "a short page is the end");
        assert_eq!(s.counter(), "1 of 3");
    }

    #[test]
    fn stale_pages_are_ignored() {
        let mut s = ChatSearch::default();
        let old = started(&mut s);
        let new = started(&mut s);
        assert_eq!(s.apply_page(old, 0, msgs([5])), Applied::Stale);
        assert!(s.results.is_empty() && s.searching);
        // Wrong offset (not the page after the last result).
        assert_eq!(
            s.apply_page(new, 77, msgs([5])),
            Applied::Stale,
            "offset must match"
        );
        assert!(matches!(
            s.apply_page(new, 0, msgs([5])),
            Applied::Page { .. }
        ));
        // Nothing in flight any more: a duplicate answer is dropped.
        assert_eq!(s.apply_page(new, 5, msgs([4])), Applied::Stale);
    }

    #[test]
    fn stepping_walks_older_and_newer_and_clamps() {
        let mut s = ChatSearch::default();
        let v = started(&mut s);
        s.apply_page(v, 0, msgs([30, 20, 10]));
        assert_eq!(s.step(Dir::Newer), Step::Nothing, "already the newest");
        assert_eq!(s.step(Dir::Older), Step::Jump(20));
        assert_eq!(s.step(Dir::Older), Step::Jump(10));
        assert_eq!(
            s.step(Dir::Older),
            Step::Nothing,
            "exhausted: nothing older"
        );
        assert_eq!(s.step(Dir::Newer), Step::Jump(20));
        assert_eq!(s.index, Some(1));
        assert!(s.can_step(Dir::Older) && s.can_step(Dir::Newer));
        s.index = Some(0);
        assert!(!s.can_step(Dir::Newer));
    }

    #[test]
    fn stepping_past_a_full_page_loads_the_next_one() {
        let mut s = ChatSearch {
            query: "x".into(),
            ..ChatSearch::default()
        };
        let v = started(&mut s);
        let first: Vec<i32> = (0..SEARCH_PAGE as i32).map(|i| 1000 - i).collect();
        s.apply_page(v, 0, msgs(first.clone()));
        assert!(!s.exhausted);
        assert!(s.counter().ends_with('+'));
        s.index = Some(SEARCH_PAGE - 1);
        assert_eq!(s.step(Dir::Older), Step::NeedMore);
        assert!(s.advance);
        s.start();
        let offset = s.next_offset();
        assert_eq!(offset, 1000 - SEARCH_PAGE as i32 + 1);
        let r = s.apply_page(v, offset, msgs([offset - 1, offset - 2]));
        assert_eq!(
            r,
            Applied::Page {
                jump: Some(offset - 1)
            },
            "the step the user asked for happens when the page lands"
        );
        assert_eq!(s.index, Some(SEARCH_PAGE));
        assert!(s.exhausted);
    }

    #[test]
    fn empty_and_duplicate_pages_end_the_list() {
        let mut s = ChatSearch {
            query: "zz".into(),
            ..ChatSearch::default()
        };
        let v = started(&mut s);
        s.apply_page(v, 0, Vec::new());
        assert_eq!(s.counter(), "No results");
        assert_eq!(s.step(Dir::Older), Step::Nothing);
        assert!(!s.can_step(Dir::Older));

        let mut s = ChatSearch::default();
        let v = started(&mut s);
        let full: Vec<i32> = (0..SEARCH_PAGE as i32).map(|i| 500 - i).collect();
        s.apply_page(v, 0, msgs(full.clone()));
        s.start();
        let off = s.next_offset();
        // The server repeats a message: no new result, so no endless paging.
        s.apply_page(v, off, msgs([full[0]; SEARCH_PAGE]));
        assert!(s.exhausted);
    }

    #[test]
    fn results_are_capped() {
        let mut s = ChatSearch::default();
        let v = started(&mut s);
        let mut next_id = 100_000;
        let mut offset = 0;
        for _ in 0..(SEARCH_MAX / SEARCH_PAGE + 2) {
            let page: Vec<i32> = (0..SEARCH_PAGE as i32)
                .map(|_| {
                    next_id -= 1;
                    next_id
                })
                .collect();
            if !matches!(s.apply_page(v, offset, msgs(page)), Applied::Page { .. }) {
                break;
            }
            offset = s.next_offset();
            if s.exhausted {
                break;
            }
            s.start();
        }
        assert_eq!(s.results.len(), SEARCH_MAX);
        assert!(s.exhausted);
    }

    #[test]
    fn reset_invalidates_and_blank_has_no_counter() {
        let mut s = ChatSearch {
            query: "a".into(),
            ..ChatSearch::default()
        };
        let v = started(&mut s);
        s.apply_page(v, 0, msgs([3, 2]));
        assert_eq!(s.counter(), "1 of 2");
        let v2 = s.reset();
        assert!(v2 > v && s.results.is_empty() && s.index.is_none());
        s.query.clear();
        assert!(s.is_blank());
        assert_eq!(s.counter(), "");
        s.from = Some(Member {
            id: 7,
            name: "Ana".into(),
        });
        assert!(!s.is_blank(), "a member filter alone is a search");
    }

    #[test]
    fn failure_allows_another_try() {
        let mut s = ChatSearch::default();
        let v = started(&mut s);
        s.fail(v);
        assert!(!s.searching);
        let v2 = started(&mut s);
        s.fail(v); // an old failure must not cancel the new request
        assert!(s.searching);
        s.fail(v2);
        assert!(!s.searching);
    }

    #[test]
    fn member_picker_filters_by_name() {
        let mut s = ChatSearch {
            members: Some(vec![
                Member {
                    id: 1,
                    name: "Ana Ruiz".into(),
                },
                Member {
                    id: 2,
                    name: "Bruno".into(),
                },
            ]),
            ..ChatSearch::default()
        };
        assert_eq!(s.visible_members().len(), 2);
        s.from_query = " ru ".into();
        let names: Vec<_> = s.visible_members().iter().map(|m| m.id).collect();
        assert_eq!(names, vec![1, 2], "case-insensitive substring");
        s.from_query = "bru".into();
        assert_eq!(s.visible_members().len(), 1);
        assert!(ChatSearch::default().visible_members().is_empty());
    }
}
