//! User actions of the chat header: in-chat search, the ⋮ menu (mute,
//! clear history, delete/leave, block), message selection. Pure state lives
//! in `chatmenu.rs`; this file turns it into `Task`s.

use std::time::Duration;

use iced::widget::{Id, operation};
use iced::{Task, clipboard};
use tracing::{info, warn};

use super::chatmenu::{
    ChatSearch, Confirm, Dir, HeaderMenu, MEMBERS_MAX, SEARCH_DEBOUNCE_MS, SEARCH_PAGE, Selection,
    Step,
};
use super::{App, CHAT_SEARCH_ID, Done, Message, Ui};
use crate::error::{Error, Redacted};
use crate::model::{
    ChatId, ChatKind, Member, Message as ChatMessage, MessageId, MuteFor, NotifySettings,
};

impl App {
    pub(super) fn fetch_notify_defaults(&self) -> Task<Message> {
        let Some(api) = self.api.clone() else {
            return Task::none();
        };
        Task::perform(async move { api.notify_defaults().await }, |r| {
            Message::Done(Done::NotifyDefaults(r))
        })
    }

    pub(super) fn chat_ui(&mut self, ui: Ui) -> Task<Message> {
        match ui {
            // ---------------------------------------------------- search
            Ui::ToggleChatSearch => {
                if self.chat_search.take().is_some() {
                    self.highlight = None;
                    Task::none()
                } else {
                    self.open_chat_search()
                }
            }
            Ui::OpenChatSearch => self.open_chat_search(),
            Ui::ChatSearchInput(q) => {
                let Some(s) = &mut self.chat_search else {
                    return Task::none();
                };
                s.query = q;
                s.from_open = false;
                let version = s.reset();
                if s.is_blank() {
                    self.highlight = None;
                    return Task::none();
                }
                // Search as you type, once the typing pauses.
                Task::perform(
                    async move {
                        tokio::time::sleep(Duration::from_millis(SEARCH_DEBOUNCE_MS)).await;
                        version
                    },
                    |v| Message::Ui(Ui::ChatSearchDue(v)),
                )
            }
            Ui::ChatSearchDue(version) => {
                if self
                    .chat_search
                    .as_ref()
                    .is_some_and(|s| s.version == version && !s.searching && !s.is_blank())
                {
                    self.run_search()
                } else {
                    Task::none()
                }
            }
            Ui::ChatSearchSubmit => {
                let Some(s) = &self.chat_search else {
                    return Task::none();
                };
                if s.is_blank() || s.searching {
                    Task::none()
                } else if s.searched {
                    self.chat_ui(Ui::ChatSearchStep(Dir::Older))
                } else {
                    self.run_search()
                }
            }
            Ui::ChatSearchStep(dir) => {
                let Some(s) = &mut self.chat_search else {
                    return Task::none();
                };
                match s.step(dir) {
                    Step::Jump(id) => self.jump_to_hit(id),
                    Step::NeedMore => self.run_search(),
                    Step::Nothing => Task::none(),
                }
            }
            Ui::ChatSearchFrom(member) => {
                let Some(s) = &mut self.chat_search else {
                    return Task::none();
                };
                s.from = member;
                s.from_open = false;
                s.from_query.clear();
                s.reset();
                if s.is_blank() {
                    self.highlight = None;
                    return Task::none();
                }
                self.run_search()
            }
            Ui::ChatSearchFromPicker => {
                let Some(chat) = self.selected else {
                    return Task::none();
                };
                let Some(s) = &mut self.chat_search else {
                    return Task::none();
                };
                s.from_open = !s.from_open;
                if s.from_open && s.members.is_none() && !s.members_loading {
                    s.members_loading = true;
                    let Some(api) = self.api.clone() else {
                        s.members_loading = false;
                        return Task::none();
                    };
                    return Task::perform(
                        async move { api.members(chat, MEMBERS_MAX).await },
                        move |result| Message::Done(Done::Members { chat, result }),
                    );
                }
                Task::none()
            }
            Ui::ChatSearchFromQuery(q) => {
                if let Some(s) = &mut self.chat_search {
                    s.from_query = q;
                }
                Task::none()
            }

            // ------------------------------------------------- ⋮ menu
            Ui::ToggleHeaderMenu => {
                if self.header_menu.take().is_some() {
                    return Task::none();
                }
                let Some(chat) = self.selected.and_then(|id| self.chats.get(id)) else {
                    return Task::none();
                };
                self.header_menu = Some(HeaderMenu::Main);
                // "Block" or "Unblock" depends on the user's state.
                let id = chat.id;
                if matches!(chat.kind, ChatKind::User | ChatKind::Bot)
                    && !self.blocked.contains_key(&id)
                {
                    return self.fetch_peer_info(id);
                }
                Task::none()
            }
            Ui::HeaderMenuPage(page) => {
                if self.header_menu.is_some() {
                    self.header_menu = Some(page);
                }
                Task::none()
            }
            Ui::CloseHeaderMenu => {
                self.header_menu = None;
                Task::none()
            }
            Ui::SetMute(how) => {
                self.header_menu = None;
                match self.selected {
                    Some(chat) => self.set_mute(chat, how),
                    None => Task::none(),
                }
            }
            Ui::AskClearHistory => {
                self.header_menu = None;
                if let Some(chat) = self.selected {
                    self.confirm = Some(Confirm::ClearHistory {
                        chat,
                        revoke: false,
                    });
                }
                Task::none()
            }
            Ui::AskDeleteChat => {
                self.header_menu = None;
                if let Some(chat) = self.selected {
                    self.confirm = Some(Confirm::DeleteChat {
                        chat,
                        revoke: false,
                    });
                }
                Task::none()
            }
            Ui::AskLeave => {
                self.header_menu = None;
                if let Some(chat) = self.selected {
                    self.confirm = Some(Confirm::Leave { chat });
                }
                Task::none()
            }
            Ui::ConfirmRevoke(on) => {
                if let Some(c) = &mut self.confirm {
                    c.set_revoke(on);
                }
                Task::none()
            }
            Ui::ConfirmCancel => {
                self.confirm = None;
                Task::none()
            }
            Ui::ConfirmAccept => match self.confirm.take() {
                Some(action) => self.run_confirmed(action),
                None => Task::none(),
            },
            Ui::ToggleBlock => {
                self.header_menu = None;
                let Some(chat) = self.selected else {
                    return Task::none();
                };
                let now_blocked = !self.blocked.get(&chat).copied().unwrap_or(false);
                self.blocked.insert(chat, now_blocked);
                let Some(api) = self.api.clone() else {
                    self.blocked.insert(chat, !now_blocked);
                    return Task::none();
                };
                Task::perform(
                    async move { api.set_blocked(chat, now_blocked).await },
                    move |result| {
                        Message::Done(Done::BlockDone {
                            chat,
                            blocked: now_blocked,
                            result,
                        })
                    },
                )
            }

            // -------------------------------------------------- selection
            Ui::StartSelect => {
                self.header_menu = None;
                self.menu_for = None;
                self.selection = Some(Selection::default());
                Task::none()
            }
            Ui::ToggleSelect(id) => {
                if let Some(s) = &mut self.selection {
                    s.toggle(id);
                }
                Task::none()
            }
            Ui::CancelSelect => {
                self.selection = None;
                Task::none()
            }
            Ui::AskDeleteSelected => {
                if let (Some(chat), Some(sel)) = (self.selected, &self.selection)
                    && !sel.is_empty()
                {
                    self.confirm = Some(Confirm::DeleteMessages {
                        chat,
                        ids: sel.ids.iter().copied().collect(),
                    });
                }
                Task::none()
            }
            Ui::CopySelected => {
                let (Some(sel), Some(conv)) = (self.selection.take(), &self.conversation) else {
                    return Task::none();
                };
                let text = conv
                    .messages()
                    .iter()
                    .filter(|m| sel.contains(m.id) && !m.text.is_empty())
                    .map(|m| m.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                if text.is_empty() {
                    return Task::none();
                }
                clipboard::write(text)
            }
            _ => Task::none(),
        }
    }

    // -------------------------------------------------------------- search

    fn open_chat_search(&mut self) -> Task<Message> {
        self.header_menu = None;
        if self.selected.is_none() {
            return Task::none();
        }
        if self.chat_search.is_none() {
            self.chat_search = Some(ChatSearch::default());
        }
        operation::focus(Id::new(CHAT_SEARCH_ID))
    }

    /// Scroll to / open the window around a hit; the bubble is highlighted.
    fn jump_to_hit(&mut self, id: MessageId) -> Task<Message> {
        match self.selected {
            Some(chat) => self.open_chat_at(chat, id),
            None => Task::none(),
        }
    }

    /// Ask the server for the next page of the current search.
    fn run_search(&mut self) -> Task<Message> {
        let (Some(chat), Some(s)) = (self.selected, &mut self.chat_search) else {
            return Task::none();
        };
        let Some(api) = self.api.clone() else {
            s.fail(s.version);
            return Task::none();
        };
        s.start();
        let (version, offset) = (s.version, s.next_offset());
        let (query, from) = (s.query.trim().to_owned(), s.from.as_ref().map(|m| m.id));
        info!(query = %Redacted(&query), offset, "in-chat search");
        Task::perform(
            async move { api.search(chat, query, from, offset, SEARCH_PAGE).await },
            move |result| {
                Message::Done(Done::SearchPage {
                    chat,
                    version,
                    offset,
                    result,
                })
            },
        )
    }

    pub(super) fn on_search_page(
        &mut self,
        chat: ChatId,
        version: u64,
        offset: MessageId,
        result: Result<Vec<ChatMessage>, Error>,
    ) -> Task<Message> {
        if self.selected != Some(chat) {
            return Task::none();
        }
        let Some(s) = &mut self.chat_search else {
            return Task::none();
        };
        match result {
            Ok(page) => match s.apply_page(version, offset, page) {
                super::chatmenu::Applied::Page { jump: Some(id) } => self.jump_to_hit(id),
                _ => Task::none(),
            },
            Err(e) => {
                s.fail(version);
                if e.is_session_revoked() {
                    return self.session_revoked();
                }
                self.on_error(e)
            }
        }
    }

    pub(super) fn on_members(
        &mut self,
        chat: ChatId,
        result: Result<Vec<Member>, Error>,
    ) -> Task<Message> {
        if self.selected != Some(chat) {
            return Task::none();
        }
        let Some(s) = &mut self.chat_search else {
            return Task::none();
        };
        s.members_loading = false;
        match result {
            Ok(list) => {
                s.members = Some(list);
                Task::none()
            }
            Err(e) => {
                warn!(error = %e, "could not load group members");
                // The picker closes; the user can open it again to retry.
                s.from_open = false;
                self.toast("Could not load the members.".into())
            }
        }
    }

    // ---------------------------------------------------------------- mute

    /// Mute for a while / forever / unmute: applied at once, saved, sent,
    /// and put back if Telegram refuses.
    pub(super) fn set_mute(&mut self, chat: ChatId, how: MuteFor) -> Task<Message> {
        let now = chrono::Utc::now().timestamp();
        let Some(previous) = self.chats.get(chat).map(|c| c.notify) else {
            return Task::none();
        };
        let until = how.until(now);
        let next = NotifySettings {
            mute_until: Some(until),
            ..previous
        };
        self.chats.set_notify(chat, next, now);
        let persist = self.store_bg(move |s| s.set_notify(chat, &next));
        let Some(api) = self.api.clone() else {
            return persist;
        };
        Task::batch([
            persist,
            Task::perform(
                async move { api.set_mute(chat, until).await },
                move |result| {
                    Message::Done(Done::MuteDone {
                        chat,
                        previous,
                        result,
                    })
                },
            ),
        ])
    }

    // ------------------------------------------------------ confirmations

    fn run_confirmed(&mut self, action: Confirm) -> Task<Message> {
        let Some(api) = self.api.clone() else {
            return self.toast("Not connected.".into());
        };
        match action.clone() {
            Confirm::ClearHistory { chat, revoke } => Task::perform(
                async move { api.clear_history(chat, revoke).await },
                move |result| {
                    Message::Done(Done::ChatActionDone {
                        action: action.clone(),
                        result,
                    })
                },
            ),
            Confirm::DeleteChat { chat, revoke } => Task::perform(
                async move { api.delete_chat(chat, revoke).await },
                move |result| {
                    Message::Done(Done::ChatActionDone {
                        action: action.clone(),
                        result,
                    })
                },
            ),
            Confirm::Leave { chat } => Task::perform(
                async move { api.delete_chat(chat, false).await },
                move |result| {
                    Message::Done(Done::ChatActionDone {
                        action: action.clone(),
                        result,
                    })
                },
            ),
            Confirm::DeleteMessages { chat, ids } => {
                self.selection = None;
                let mut old = Vec::new();
                if let Some(conv) = self.conversation.as_mut().filter(|c| c.chat_id == chat) {
                    for id in &ids {
                        if let Some(m) = conv.get(*id).cloned() {
                            old.push(m);
                        }
                        conv.remove_local(*id);
                    }
                }
                Task::perform(
                    async move { api.delete_messages(chat, ids).await },
                    move |r| match r {
                        Ok(()) => Message::Done(Done::Call(Ok(()))),
                        Err(error) => Message::Done(Done::RollbackMany {
                            old: old.clone(),
                            error,
                        }),
                    },
                )
            }
        }
    }

    pub(super) fn on_chat_action_done(
        &mut self,
        action: Confirm,
        result: Result<(), Error>,
    ) -> Task<Message> {
        if let Err(e) = result {
            if e.is_session_revoked() {
                return self.session_revoked();
            }
            return self.on_error(e);
        }
        let chat = action.chat();
        match action {
            Confirm::ClearHistory { .. } => {
                self.chats.cleared(chat);
                if self.selected == Some(chat) {
                    let mut conv = super::state::Conversation::new(chat);
                    conv.reached_start = true;
                    self.conversation = Some(conv);
                    self.highlight = None;
                    self.selection = None;
                    self.chat_search = None;
                }
                Task::batch([
                    self.store_bg(move |s| s.clear_chat(chat)),
                    self.toast("History cleared.".into()),
                ])
            }
            Confirm::DeleteChat { .. } | Confirm::Leave { .. } => {
                self.chats.remove(chat);
                if self.selected == Some(chat) {
                    self.selected = None;
                    self.conversation = None;
                    self.chat_search = None;
                    self.selection = None;
                    self.highlight = None;
                    self.close_info();
                }
                self.blocked.remove(&chat);
                self.store_bg(move |s| s.delete_chat(chat))
            }
            // Deleted messages were already removed optimistically.
            Confirm::DeleteMessages { .. } => Task::none(),
        }
    }

    pub(super) fn on_block_done(
        &mut self,
        chat: ChatId,
        blocked: bool,
        result: Result<(), Error>,
    ) -> Task<Message> {
        match result {
            Ok(()) => self.toast(
                if blocked {
                    "User blocked."
                } else {
                    "User unblocked."
                }
                .into(),
            ),
            Err(e) => {
                self.blocked.insert(chat, !blocked);
                self.on_error(e)
            }
        }
    }
}
