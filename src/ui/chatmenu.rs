//! Chat header widgets: the ⋮ menu, the confirmation dialog, the search row
//! (query, "from:", hit counter, arrows), the member picker and the
//! selection bar. State is in `app::chatmenu`.

use iced::widget::{
    Column, Space, button, checkbox, column, container, mouse_area, opaque, row, scrollable, text,
    text_input,
};
use iced::{Alignment, Element, Length, Padding};

use super::format;
use super::icons::Icon;
use super::theme::{self, FONT_SEMIBOLD};
use super::widgets::{emoji_plain, emoji_title, icon, modal};
use crate::app::chatmenu::{Dir, HeaderMenu, MenuItem, menu_items};
use crate::app::{App, CHAT_SEARCH_ID, Message, Ui};
use crate::model::{ChatKind, MuteFor};

const MENU_WIDTH: f32 = 250.0;

fn entry<'a>(app: &App, ic: Icon, label: String, msg: Ui, danger: bool) -> Element<'a, Message> {
    let (p, m) = (app.theme.palette, app.theme.metrics);
    let color = if danger { p.danger } else { p.text_secondary };
    button(
        row![
            container(icon(ic, 18.0, color)).width(22),
            text(label).size(m.font_m),
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding([8, 12])
    .on_press(Message::Ui(msg))
    .style(theme::menu_item(p, danger))
    .into()
}

fn back_row<'a>(app: &App, label: &'static str) -> Element<'a, Message> {
    let (p, m) = (app.theme.palette, app.theme.metrics);
    button(
        row![
            icon(Icon::ArrowLeft, 18.0, p.text_secondary),
            text(label).size(m.font_m).font(FONT_SEMIBOLD),
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding([8, 12])
    .on_press(Message::Ui(Ui::HeaderMenuPage(HeaderMenu::Main)))
    .style(theme::menu_item(p, false))
    .into()
}

/// The ⋮ menu as a full-window layer: a transparent backdrop that closes it
/// on any outside click, and the card under the header's right end.
pub fn header_menu(app: &App) -> Option<Element<'_, Message>> {
    let page = app.header_menu?;
    let chat = app.chats.get(app.selected?)?;
    let (p, m) = (app.theme.palette, app.theme.metrics);
    let blocked = app.blocked.get(&chat.id).copied();
    let info_open = app.info.is_some();
    let mut items = Column::new().spacing(0);
    match page {
        HeaderMenu::Main => {
            for item in menu_items(chat, info_open, blocked) {
                items = items.push(match item {
                    MenuItem::Info { open } => entry(
                        app,
                        Icon::Info,
                        match (chat.kind, open) {
                            (_, true) => "Hide info".to_owned(),
                            (ChatKind::Group, _) => "View group info".to_owned(),
                            (ChatKind::Channel, _) => "View channel info".to_owned(),
                            _ => "View profile".to_owned(),
                        },
                        Ui::ToggleInfo,
                        false,
                    ),
                    MenuItem::Mute => entry(
                        app,
                        Icon::BellOff,
                        "Mute…".into(),
                        Ui::HeaderMenuPage(HeaderMenu::Mute),
                        false,
                    ),
                    MenuItem::Unmute => entry(
                        app,
                        Icon::Bell,
                        "Unmute".into(),
                        Ui::SetMute(MuteFor::Off),
                        false,
                    ),
                    MenuItem::Search => entry(
                        app,
                        Icon::Search,
                        "Search".into(),
                        Ui::OpenChatSearch,
                        false,
                    ),
                    MenuItem::Select => entry(
                        app,
                        Icon::Check,
                        "Select messages".into(),
                        Ui::StartSelect,
                        false,
                    ),
                    MenuItem::AddToFolder => entry(
                        app,
                        Icon::Folder,
                        "Add to folder…".into(),
                        Ui::HeaderMenuPage(HeaderMenu::Folders),
                        false,
                    ),
                    MenuItem::ClearHistory => entry(
                        app,
                        Icon::Trash,
                        "Clear history".into(),
                        Ui::AskClearHistory,
                        true,
                    ),
                    MenuItem::DeleteChat => entry(
                        app,
                        Icon::Trash,
                        "Delete chat".into(),
                        Ui::AskDeleteChat,
                        true,
                    ),
                    MenuItem::LeaveGroup => entry(
                        app,
                        Icon::ArrowLeft,
                        "Leave group".into(),
                        Ui::AskLeave,
                        true,
                    ),
                    MenuItem::LeaveChannel => entry(
                        app,
                        Icon::ArrowLeft,
                        "Leave channel".into(),
                        Ui::AskLeave,
                        true,
                    ),
                    MenuItem::Block => {
                        entry(app, Icon::X, "Block user".into(), Ui::ToggleBlock, true)
                    }
                    MenuItem::Unblock => entry(
                        app,
                        Icon::Check,
                        "Unblock user".into(),
                        Ui::ToggleBlock,
                        false,
                    ),
                });
            }
        }
        HeaderMenu::Mute => {
            items = items.push(back_row(app, "Mute notifications"));
            for how in MuteFor::CHOICES {
                items = items.push(entry(
                    app,
                    Icon::BellOff,
                    how.label().to_owned(),
                    Ui::SetMute(how),
                    false,
                ));
            }
        }
        HeaderMenu::Folders => {
            items = items.push(back_row(app, "Add to folder"));
            for f in app.folders.list.iter().filter(|f| !f.shared) {
                let inside = f.matches(chat);
                items = items.push(
                    button(
                        row![
                            container(if inside {
                                icon(Icon::Check, 17.0, p.link)
                            } else {
                                Space::new().width(17).into()
                            })
                            .width(22),
                            emoji_plain(format::ellipsize(&f.title, 22), m.font_m, p.text_primary),
                        ]
                        .spacing(12)
                        .align_y(Alignment::Center),
                    )
                    .width(Length::Fill)
                    .padding([8, 12])
                    .on_press(Message::Ui(Ui::ChatInFolder(chat.id, f.id)))
                    .style(theme::menu_item(p, false)),
                );
            }
            items = items.push(
                button(
                    row![
                        Space::new().width(22),
                        text("New folder…").size(m.font_m).color(p.link)
                    ]
                    .spacing(12),
                )
                .width(Length::Fill)
                .padding([8, 12])
                .on_press(Message::Ui(Ui::FolderNew(Some(chat.id))))
                .style(theme::menu_item(p, false)),
            );
        }
    }
    let right = 10.0
        + if app.info.is_some() {
            super::info::PANEL_WIDTH + 1.0
        } else {
            0.0
        };
    let card = container(items)
        .padding(4)
        .width(MENU_WIDTH)
        .style(theme::elevated(p));
    Some(opaque(
        mouse_area(
            container(opaque(card))
                .width(Length::Fill)
                .height(Length::Fill)
                .align_right(Length::Fill)
                .padding(Padding {
                    top: m.header_height - 6.0,
                    right,
                    bottom: 0.0,
                    left: 0.0,
                }),
        )
        .on_press(Message::Ui(Ui::CloseHeaderMenu)),
    ))
}

/// The confirmation dialog of a destructive action.
pub fn confirm_dialog(app: &App) -> Option<Element<'_, Message>> {
    let confirm = app.confirm.as_ref()?;
    let (p, m) = (app.theme.palette, app.theme.metrics);
    let chat = app.chats.get(confirm.chat());
    let kind = chat.map_or(ChatKind::User, |c| c.kind);
    let name = chat.map_or("this chat", |c| c.title.as_str());
    let mut body = column![
        text(confirm.title(kind))
            .size(m.font_l)
            .font(FONT_SEMIBOLD)
            .color(p.text_primary),
        text(confirm.body(kind, name))
            .size(m.font_m)
            .color(p.text_secondary),
    ]
    .spacing(10);
    if let Some(revoke) = confirm.revoke()
        && matches!(kind, ChatKind::User | ChatKind::Bot)
    {
        body = body.push(
            checkbox(revoke)
                .label(format!("Also delete for {name}"))
                .on_toggle(|on| Message::Ui(Ui::ConfirmRevoke(on)))
                .size(18)
                .text_size(m.font_m),
        );
    }
    body = body.push(
        row![
            Space::new().width(Length::Fill),
            button(text("Cancel").size(m.font_m))
                .padding([8, 16])
                .on_press(Message::Ui(Ui::ConfirmCancel))
                .style(theme::menu_item(p, false)),
            button(text(confirm.accept_label(kind)).size(m.font_m))
                .padding([8, 16])
                .on_press(Message::Ui(Ui::ConfirmAccept))
                .style(theme::menu_item(p, true)),
        ]
        .spacing(8),
    );
    let card = container(body)
        .padding(20)
        .width(380)
        .style(theme::elevated(p));
    Some(modal(card, Message::Ui(Ui::ConfirmCancel)))
}

/// The header row while the in-chat search is open: it replaces the title.
pub fn search_row<'a>(app: &'a App, s: &'a crate::app::ChatSearch) -> Element<'a, Message> {
    let (p, m) = (app.theme.palette, app.theme.metrics);
    let is_group = app
        .selected
        .and_then(|id| app.chats.get(id))
        .is_some_and(|c| c.kind == ChatKind::Group);
    let ib = |i: Icon, msg: Option<Message>| {
        button(icon(i, 20.0, p.text_secondary))
            .padding(7)
            .on_press_maybe(msg)
            .style(theme::icon_button(p, true))
    };
    let mut r = row![
        icon(Icon::Search, 18.0, p.text_muted),
        text_input("Search in this chat", &s.query)
            .id(CHAT_SEARCH_ID)
            .on_input(|q| Message::Ui(Ui::ChatSearchInput(q)))
            .on_submit(Message::Ui(Ui::ChatSearchSubmit))
            .padding([6, 4])
            .size(m.font_m)
            .style(theme::input(p, p.bg_sidebar, 0.0, false)),
    ]
    .spacing(10)
    .align_y(Alignment::Center);
    if is_group {
        let label = match &s.from {
            Some(f) => format!("From: {}", format::ellipsize(&f.name, 18)),
            None => "From".to_owned(),
        };
        r = r.push(
            button(text(label).size(m.font_s))
                .padding([5, 10])
                .on_press(Message::Ui(Ui::ChatSearchFromPicker))
                .style(theme::menu_item(p, false)),
        );
        if s.from.is_some() {
            r = r.push(ib(Icon::X, Some(Message::Ui(Ui::ChatSearchFrom(None)))));
        }
    }
    r = r
        .push(text(s.counter()).size(m.font_s).color(p.text_secondary))
        .push(ib(
            Icon::ArrowUp,
            s.can_step(Dir::Older)
                .then_some(Message::Ui(Ui::ChatSearchStep(Dir::Older))),
        ))
        .push(ib(
            Icon::ArrowDown,
            s.can_step(Dir::Newer)
                .then_some(Message::Ui(Ui::ChatSearchStep(Dir::Newer))),
        ))
        .push(ib(Icon::X, Some(Message::Ui(Ui::ToggleChatSearch))));
    r.into()
}

/// The "from:" member list under the header (groups).
pub fn from_picker<'a>(
    app: &'a App,
    s: &'a crate::app::ChatSearch,
) -> Option<Element<'a, Message>> {
    if !s.from_open {
        return None;
    }
    let (p, m) = (app.theme.palette, app.theme.metrics);
    let mut list = Column::new();
    if s.members_loading {
        list =
            list.push(container(text("Loading…").size(m.font_s).color(p.text_muted)).padding(10));
    }
    for member in s.visible_members() {
        list = list.push(
            button(emoji_title(
                format::ellipsize(&member.name, 30),
                m.font_m,
                p.text_primary,
            ))
            .width(Length::Fill)
            .padding([8, 12])
            .on_press(Message::Ui(Ui::ChatSearchFrom(Some(member.clone()))))
            .style(theme::menu_item(p, false)),
        );
    }
    if !s.members_loading && s.members.as_ref().is_some_and(|v| v.is_empty()) {
        list = list.push(
            container(text("No members found").size(m.font_s).color(p.text_muted)).padding(10),
        );
    }
    let card = container(
        column![
            text_input("Search members", &s.from_query)
                .on_input(|q| Message::Ui(Ui::ChatSearchFromQuery(q)))
                .padding([6, 8])
                .size(m.font_m)
                .style(theme::input(p, p.bg_input, 6.0, false)),
            scrollable(list)
                .height(Length::Shrink)
                .style(theme::scroll(p)),
        ]
        .spacing(6),
    )
    .padding(6)
    .width(300)
    .max_height(340)
    .style(theme::elevated(p));
    Some(
        container(opaque(card))
            .width(Length::Fill)
            .align_right(Length::Fill)
            .padding(Padding {
                top: 2.0,
                right: 12.0,
                bottom: 0.0,
                left: 0.0,
            })
            .into(),
    )
}

/// Replaces the composer in "Select messages" mode.
pub fn select_bar(app: &App) -> Option<Element<'_, Message>> {
    let sel = app.selection.as_ref()?;
    let (p, m) = (app.theme.palette, app.theme.metrics);
    let some = !sel.is_empty();
    let action = |ic: Icon, label: &'static str, msg: Ui, danger: bool| {
        button(
            row![
                icon(ic, 18.0, if danger { p.danger } else { p.text_secondary }),
                text(label).size(m.font_m),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .padding([8, 14])
        .on_press_maybe(some.then_some(Message::Ui(msg)))
        .style(theme::menu_item(p, danger))
    };
    Some(
        container(
            row![
                button(icon(Icon::X, 20.0, p.text_secondary))
                    .padding(8)
                    .on_press(Message::Ui(Ui::CancelSelect))
                    .style(theme::icon_button(p, true)),
                text(if some {
                    format!("{} selected", sel.len())
                } else {
                    "Select messages".to_owned()
                })
                .size(m.font_m)
                .font(FONT_SEMIBOLD)
                .color(p.text_primary),
                Space::new().width(Length::Fill),
                action(Icon::Copy, "Copy", Ui::CopySelected, false),
                action(Icon::Trash, "Delete", Ui::AskDeleteSelected, true),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        )
        .padding([2, 12])
        .width(Length::Fill)
        .height(m.composer_height)
        .align_y(Alignment::Center)
        .style(theme::fill(p.bg_sidebar))
        .into(),
    )
}
