//! Chat folders: the list/selection state and the editor, as pure reducers
//! (unit-tested without iced). Talking to Telegram lives in `app/mod.rs`.

use crate::model::{Chat, ChatId, FOLDER_CHATS_MAX, FOLDER_TITLE_MAX, Folder};

/// Folder list, which one is shown, and the open menus.
#[derive(Debug, Default, Clone)]
pub struct FolderState {
    pub list: Vec<Folder>,
    /// `None` = "All chats".
    pub selected: Option<i32>,
    /// Right-clicked folder tab (its small menu is open).
    pub tab_menu: Option<i32>,
    /// The "Chat folders" manager dialog.
    pub manager_open: bool,
    /// Folder whose delete button was pressed once (asks to confirm).
    pub confirm_delete: Option<i32>,
    /// Right-clicked chat row (its menu with "Add to folder" is open).
    pub chat_menu: Option<ChatId>,
}

impl FolderState {
    pub fn get(&self, id: i32) -> Option<&Folder> {
        self.list.iter().find(|f| f.id == id)
    }

    /// The selected folder, if it still exists.
    pub fn current(&self) -> Option<&Folder> {
        self.selected.and_then(|id| self.get(id))
    }

    pub fn ids(&self) -> Vec<i32> {
        self.list.iter().map(|f| f.id).collect()
    }

    /// Replace the whole list (server answer or cache).
    pub fn set_list(&mut self, list: Vec<Folder>) {
        self.list = list;
        self.fix_selection();
    }

    fn fix_selection(&mut self) {
        if self.selected.is_some_and(|id| self.get(id).is_none()) {
            self.selected = None;
        }
        if self.tab_menu.is_some_and(|id| self.get(id).is_none()) {
            self.tab_menu = None;
        }
        if self.confirm_delete.is_some_and(|id| self.get(id).is_none()) {
            self.confirm_delete = None;
        }
    }

    /// `updateDialogFilter`: create/replace (`Some`) or delete (`None`).
    pub fn apply_changed(&mut self, id: i32, folder: Option<Folder>) {
        match folder {
            Some(f) => self.upsert(f),
            None => {
                self.list.retain(|f| f.id != id);
            }
        }
        self.fix_selection();
    }

    /// Replace a folder in place, or append it when new.
    pub fn upsert(&mut self, folder: Folder) {
        match self.list.iter_mut().find(|f| f.id == folder.id) {
            Some(slot) => *slot = folder,
            None => self.list.push(folder),
        }
    }

    /// Remove a folder, remembering where it was (for a rollback).
    pub fn remove(&mut self, id: i32) -> Option<(usize, Folder)> {
        let i = self.list.iter().position(|f| f.id == id)?;
        let f = self.list.remove(i);
        self.fix_selection();
        Some((i, f))
    }

    pub fn restore(&mut self, index: usize, folder: Folder) {
        let i = index.min(self.list.len());
        self.list.insert(i, folder);
    }

    /// `updateDialogFilterOrder`: known ids in the given order; folders the
    /// update does not mention keep their relative order at the end.
    pub fn apply_order(&mut self, order: &[i32]) {
        let mut rest = std::mem::take(&mut self.list);
        let mut out = Vec::with_capacity(rest.len());
        for id in order {
            if let Some(i) = rest.iter().position(|f| f.id == *id) {
                out.push(rest.remove(i));
            }
        }
        out.extend(rest);
        self.list = out;
    }

    /// Move a folder one step (`-1` up, `+1` down). The new order, or `None`
    /// when it cannot move.
    pub fn step(&mut self, id: i32, delta: isize) -> Option<Vec<i32>> {
        let i = self.list.iter().position(|f| f.id == id)?;
        let j = i.checked_add_signed(delta)?;
        if j >= self.list.len() {
            return None;
        }
        self.list.swap(i, j);
        Some(self.ids())
    }

    /// An id no folder uses: the lowest from 2 up (0 and 1 are reserved:
    /// "All chats" and the archive), like the official client picks.
    pub fn next_id(&self) -> i32 {
        (2..).find(|id| self.get(*id).is_none()).unwrap_or(2)
    }

    /// Chats of `folder`, in the order the folder shows them: its own pins
    /// first, then by recent activity.
    pub fn chats_in<'a>(folder: &Folder, chats: &'a [Chat]) -> Vec<&'a Chat> {
        let mut out: Vec<&Chat> = chats.iter().filter(|c| folder.matches(c)).collect();
        out.sort_by(|a, b| {
            let pa = folder.pinned.iter().position(|c| *c == a.id);
            let pb = folder.pinned.iter().position(|c| *c == b.id);
            match (pa, pb) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => b.last_date.cmp(&a.last_date).then(a.id.cmp(&b.id)),
            }
        });
        out
    }

    /// Unread messages in the folder's unmuted chats (the tab badge).
    pub fn unread_in(folder: &Folder, chats: &[Chat]) -> u32 {
        chats
            .iter()
            .filter(|c| !c.muted && folder.matches(c))
            .map(|c| c.unread_count)
            .sum()
    }
}

// ------------------------------------------------------------------- editor

/// Which chat list of the folder the picker edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Picker {
    Include,
    Exclude,
}

/// The toggles of the editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorFlag {
    Contacts,
    NonContacts,
    Groups,
    Channels,
    Bots,
    ExcludeMuted,
    ExcludeRead,
    ExcludeArchived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderError {
    EmptyTitle,
    /// No included chats and no chat types: the folder would be empty.
    NoChats,
    TooManyChats,
}

impl FolderError {
    pub fn message(self) -> String {
        match self {
            Self::EmptyTitle => "Give the folder a name.".into(),
            Self::NoChats => "Add at least one chat or chat type.".into(),
            Self::TooManyChats => {
                format!("A folder can hold at most {FOLDER_CHATS_MAX} chats.")
            }
        }
    }
}

/// The folder being created or edited (a working copy until "Save").
#[derive(Debug, Clone, PartialEq)]
pub struct FolderEditor {
    pub folder: Folder,
    pub is_new: bool,
    /// The chat picker, when open.
    pub picker: Option<Picker>,
    pub query: String,
    pub error: Option<String>,
    pub saving: bool,
}

impl FolderEditor {
    pub fn create(id: i32) -> Self {
        Self::with(Folder::new(id, ""), true)
    }

    pub fn edit(folder: Folder) -> Self {
        Self::with(folder, false)
    }

    fn with(folder: Folder, is_new: bool) -> Self {
        Self {
            folder,
            is_new,
            picker: None,
            query: String::new(),
            error: None,
            saving: false,
        }
    }

    /// The name, cut to the server's limit (characters, not bytes).
    pub fn set_title(&mut self, title: &str) {
        self.folder.title = title.chars().take(FOLDER_TITLE_MAX).collect();
        self.error = None;
    }

    pub fn toggle(&mut self, flag: EditorFlag) {
        let f = &mut self.folder;
        let slot = match flag {
            EditorFlag::Contacts => &mut f.contacts,
            EditorFlag::NonContacts => &mut f.non_contacts,
            EditorFlag::Groups => &mut f.groups,
            EditorFlag::Channels => &mut f.channels,
            EditorFlag::Bots => &mut f.bots,
            EditorFlag::ExcludeMuted => &mut f.exclude_muted,
            EditorFlag::ExcludeRead => &mut f.exclude_read,
            EditorFlag::ExcludeArchived => &mut f.exclude_archived,
        };
        *slot = !*slot;
        self.error = None;
    }

    pub fn flag(&self, flag: EditorFlag) -> bool {
        let f = &self.folder;
        match flag {
            EditorFlag::Contacts => f.contacts,
            EditorFlag::NonContacts => f.non_contacts,
            EditorFlag::Groups => f.groups,
            EditorFlag::Channels => f.channels,
            EditorFlag::Bots => f.bots,
            EditorFlag::ExcludeMuted => f.exclude_muted,
            EditorFlag::ExcludeRead => f.exclude_read,
            EditorFlag::ExcludeArchived => f.exclude_archived,
        }
    }

    /// Chats of one list (pins first for the included list).
    pub fn chats(&self, which: Picker) -> Vec<ChatId> {
        match which {
            Picker::Include => self
                .folder
                .pinned
                .iter()
                .chain(&self.folder.include)
                .copied()
                .collect(),
            Picker::Exclude => self.folder.exclude.clone(),
        }
    }

    pub fn contains(&self, which: Picker, id: ChatId) -> bool {
        match which {
            Picker::Include => self.folder.is_explicit(id),
            Picker::Exclude => self.folder.exclude.contains(&id),
        }
    }

    /// Check/uncheck a chat in one list. A chat is never in both lists.
    pub fn toggle_chat(&mut self, which: Picker, id: ChatId) {
        if self.contains(which, id) {
            self.remove_chat(which, id);
        } else {
            let f = &mut self.folder;
            match which {
                Picker::Include => {
                    f.exclude.retain(|c| *c != id);
                    f.include.push(id);
                }
                Picker::Exclude => {
                    f.pinned.retain(|c| *c != id);
                    f.include.retain(|c| *c != id);
                    f.exclude.push(id);
                }
            }
        }
        self.error = None;
    }

    pub fn remove_chat(&mut self, which: Picker, id: ChatId) {
        let f = &mut self.folder;
        match which {
            Picker::Include => {
                f.pinned.retain(|c| *c != id);
                f.include.retain(|c| *c != id);
            }
            Picker::Exclude => f.exclude.retain(|c| *c != id),
        }
        self.error = None;
    }

    pub fn validate(&self) -> Result<(), FolderError> {
        if self.folder.title.trim().is_empty() {
            return Err(FolderError::EmptyTitle);
        }
        if !self.folder.has_rules() {
            return Err(FolderError::NoChats);
        }
        if self.folder.explicit_len() > FOLDER_CHATS_MAX
            || self.folder.exclude.len() > FOLDER_CHATS_MAX
        {
            return Err(FolderError::TooManyChats);
        }
        Ok(())
    }

    /// The folder to send, with the name trimmed.
    pub fn to_save(&self) -> Folder {
        let mut f = self.folder.clone();
        f.title = f.title.trim().to_owned();
        f
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ChatKind;

    fn chat(id: ChatId, kind: ChatKind) -> Chat {
        let mut c = Chat::new(id, kind, format!("c{id}"));
        c.last_date = id.abs();
        c
    }

    fn state(ids: &[i32]) -> FolderState {
        let mut s = FolderState::default();
        s.set_list(
            ids.iter()
                .map(|i| Folder::new(*i, format!("F{i}")))
                .collect(),
        );
        s
    }

    #[test]
    fn matching_follows_the_official_rules() {
        let mut f = Folder::new(2, "Work");
        f.groups = true;
        f.exclude_muted = true;
        let group = chat(-5, ChatKind::Group);
        let mut muted = chat(-6, ChatKind::Group);
        muted.muted = true;
        let user = chat(7, ChatKind::User);
        assert!(f.matches(&group));
        assert!(!f.matches(&muted), "muted excluded by flag");
        assert!(!f.matches(&user), "type not included");
        f.include.push(-6);
        assert!(f.matches(&muted), "an explicit chat beats the exclusions");
        f.exclude.push(-5);
        assert!(!f.matches(&group), "the never list always wins");

        let mut g = Folder::new(3, "People");
        g.contacts = true;
        g.exclude_read = true;
        let mut contact = chat(10, ChatKind::User);
        contact.contact = true;
        assert!(!g.matches(&contact), "read chat excluded");
        contact.unread_count = 2;
        assert!(g.matches(&contact));
        let stranger = chat(11, ChatKind::User);
        assert!(!g.matches(&stranger), "non-contacts not included");
        g.non_contacts = true;
        g.exclude_read = false;
        assert!(g.matches(&stranger));
        let bot = chat(12, ChatKind::Bot);
        assert!(!g.matches(&bot));
        g.bots = true;
        assert!(g.matches(&bot));
        let saved = chat(13, ChatKind::SavedMessages);
        assert!(g.matches(&saved), "Saved Messages counts as a contact");
    }

    #[test]
    fn toggling_a_chat_adds_removes_and_excludes() {
        let mut f = Folder::new(2, "F");
        let by_flag = chat(-5, ChatKind::Group);
        let plain = chat(7, ChatKind::User);
        // Not shown: include it.
        f.toggle_chat(&plain);
        assert_eq!(f.include, vec![7]);
        assert!(f.matches(&plain));
        // Shown through the explicit list: remove it again.
        f.toggle_chat(&plain);
        assert!(f.include.is_empty() && f.exclude.is_empty());
        // Shown through a type rule: removing puts it on the never list.
        f.groups = true;
        assert!(f.matches(&by_flag));
        f.toggle_chat(&by_flag);
        assert_eq!(f.exclude, vec![-5]);
        assert!(!f.matches(&by_flag));
        // And adding it back clears the never list.
        f.toggle_chat(&by_flag);
        assert!(f.exclude.is_empty());
        assert!(f.matches(&by_flag));
        // Pinned chats leave the pin list when removed.
        f.pinned.push(7);
        f.toggle_chat(&plain);
        assert!(f.pinned.is_empty());
    }

    #[test]
    fn summary_lists_chats_and_types() {
        let mut f = Folder::new(2, "F");
        assert_eq!(f.summary(), "No chats");
        f.include = vec![1, 2, 3];
        f.groups = true;
        f.bots = true;
        assert_eq!(f.summary(), "3 chats, Groups, Bots");
        f.include = vec![1];
        f.groups = false;
        f.bots = false;
        assert_eq!(f.summary(), "1 chat");
    }

    #[test]
    fn server_updates_create_replace_and_delete() {
        let mut s = state(&[2, 3]);
        s.selected = Some(3);
        let mut renamed = Folder::new(2, "New name");
        renamed.bots = true;
        s.apply_changed(2, Some(renamed.clone()));
        assert_eq!(s.get(2), Some(&renamed));
        assert_eq!(s.ids(), vec![2, 3], "replaced in place");
        s.apply_changed(9, Some(Folder::new(9, "Fresh")));
        assert_eq!(s.ids(), vec![2, 3, 9], "new folders go last");
        s.apply_changed(3, None);
        assert_eq!(s.ids(), vec![2, 9]);
        assert_eq!(s.selected, None, "deleted folder is deselected");
    }

    #[test]
    fn order_update_keeps_unknown_and_unmentioned_folders_sane() {
        let mut s = state(&[2, 3, 4, 5]);
        s.apply_order(&[0, 4, 2, 99, 5]);
        assert_eq!(s.ids(), vec![4, 2, 5, 3], "0 and 99 ignored, 3 kept last");
    }

    #[test]
    fn stepping_reorders_within_bounds() {
        let mut s = state(&[2, 3, 4]);
        assert_eq!(s.step(2, -1), None, "first cannot move up");
        assert_eq!(s.step(4, 1), None, "last cannot move down");
        assert_eq!(s.step(3, -1), Some(vec![3, 2, 4]));
        assert_eq!(s.step(2, 1), Some(vec![3, 4, 2]));
        assert_eq!(s.step(77, 1), None);
    }

    #[test]
    fn next_id_reuses_gaps_and_skips_reserved() {
        assert_eq!(FolderState::default().next_id(), 2);
        assert_eq!(state(&[2, 3]).next_id(), 4);
        assert_eq!(state(&[3, 4]).next_id(), 2);
    }

    #[test]
    fn remove_and_restore_for_rollback() {
        let mut s = state(&[2, 3, 4]);
        let (i, f) = s.remove(3).unwrap();
        assert_eq!(s.ids(), vec![2, 4]);
        s.restore(i, f);
        assert_eq!(s.ids(), vec![2, 3, 4]);
        assert!(s.remove(99).is_none());
    }

    #[test]
    fn folder_view_orders_pins_first_and_counts_unmuted_unread() {
        let mut f = Folder::new(2, "F");
        f.groups = true;
        f.pinned = vec![-7];
        let mut a = chat(-5, ChatKind::Group);
        a.last_date = 500;
        a.unread_count = 3;
        let mut b = chat(-6, ChatKind::Group);
        b.last_date = 900;
        b.unread_count = 4;
        b.muted = true;
        let mut c = chat(-7, ChatKind::Group);
        c.last_date = 1;
        c.unread_count = 1;
        let user = chat(1, ChatKind::User);
        let chats = vec![a, b, c, user];
        let ids: Vec<_> = FolderState::chats_in(&f, &chats)
            .iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(ids, vec![-7, -6, -5]);
        assert_eq!(
            FolderState::unread_in(&f, &chats),
            4,
            "muted chat not counted"
        );
    }

    #[test]
    fn editor_validates_like_the_server() {
        let mut e = FolderEditor::create(2);
        assert_eq!(e.validate(), Err(FolderError::EmptyTitle));
        e.set_title("  ");
        assert_eq!(e.validate(), Err(FolderError::EmptyTitle));
        e.set_title("Work");
        assert_eq!(e.validate(), Err(FolderError::NoChats));
        e.toggle(EditorFlag::Groups);
        assert_eq!(e.validate(), Ok(()));
        e.toggle(EditorFlag::Groups);
        e.toggle_chat(Picker::Include, 5);
        assert_eq!(e.validate(), Ok(()));
        for i in 0..FOLDER_CHATS_MAX as i64 {
            e.toggle_chat(Picker::Include, 1000 + i);
        }
        assert_eq!(e.validate(), Err(FolderError::TooManyChats));
        assert!(e.validate().unwrap_err().message().contains("100"));
    }

    #[test]
    fn editor_title_is_cut_by_characters() {
        let mut e = FolderEditor::create(2);
        e.set_title("ñandúñandúñandúñandú");
        assert_eq!(e.folder.title.chars().count(), FOLDER_TITLE_MAX);
        e.set_title(" Trim ");
        assert_eq!(e.to_save().title, "Trim");
    }

    #[test]
    fn editor_lists_are_exclusive_and_togglable() {
        let mut e = FolderEditor::edit(Folder::new(2, "F"));
        e.toggle_chat(Picker::Include, 5);
        assert!(e.contains(Picker::Include, 5));
        e.toggle_chat(Picker::Exclude, 5);
        assert!(!e.contains(Picker::Include, 5), "moved to the other list");
        assert!(e.contains(Picker::Exclude, 5));
        e.toggle_chat(Picker::Include, 5);
        assert!(!e.contains(Picker::Exclude, 5));
        e.toggle_chat(Picker::Include, 5);
        assert!(e.chats(Picker::Include).is_empty());
        e.folder.pinned = vec![8];
        e.folder.include = vec![9];
        assert_eq!(e.chats(Picker::Include), vec![8, 9], "pins first");
        e.remove_chat(Picker::Include, 8);
        assert_eq!(e.chats(Picker::Include), vec![9]);
        assert!(!e.flag(EditorFlag::ExcludeMuted));
        e.toggle(EditorFlag::ExcludeMuted);
        assert!(e.flag(EditorFlag::ExcludeMuted));
    }
}
