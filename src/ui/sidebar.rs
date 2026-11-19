//! Left column: menu, search field, chat list (and search results).

use iced::widget::{
    Column, Space, button, column, container, mouse_area, row, scrollable, text, text_input,
};
use iced::{Alignment, Element, Length};

use super::format;
use super::icons::Icon;
use super::theme::{self, FONT_SEMIBOLD};
use super::widgets::{avatar, avatar_photo, badge, emoji_line, emoji_plain, emoji_title, icon};
use crate::app::{App, Message, Ui};
use crate::model::{Chat, ChatKind, ConnState, GlobalHit, Message as ChatMessage, SendState};
use iced::widget::text::Wrapping;

pub fn view(app: &App) -> Element<'_, Message> {
    let t = &app.theme;
    let p = t.palette;
    let m = t.metrics;

    let search = container(
        row![
            icon(Icon::Search, 16.0, p.text_muted),
            text_input("Search", &app.sidebar_query)
                .on_input(|s| Message::Ui(Ui::SidebarSearch(s)))
                .padding([6, 4])
                .size(m.font_m)
                .style(theme::input(p, p.bg_input, 0.0, false)),
        ]
        .spacing(6)
        .align_y(Alignment::Center),
    )
    .padding([2, 12])
    .height(36)
    .align_y(Alignment::Center)
    .style(theme::pill(p.bg_input, 18.0));

    let close_or_menu: Element<'_, Message> = if app.sidebar_query.is_empty() {
        button(icon(Icon::Menu, 22.0, p.text_secondary))
            .padding(8)
            .on_press(Message::Ui(Ui::ToggleMainMenu))
            .style(theme::icon_button(p, true))
            .into()
    } else {
        button(icon(Icon::X, 22.0, p.text_secondary))
            .padding(8)
            .on_press(Message::Ui(Ui::ClearSidebarSearch))
            .style(theme::icon_button(p, true))
            .into()
    };

    let top = container(
        row![close_or_menu, search]
            .spacing(10)
            .align_y(Alignment::Center),
    )
    .padding([9, 10])
    .height(m.header_height);

    let mut list = Column::new();
    if app.sidebar_query.trim().is_empty() {
        let visible = app.visible_chats();
        let none = visible.is_empty();
        for c in visible {
            list = list.push(chat_entry(app, c));
        }
        if app.folders.current().is_some() && none && !app.dialogs_loading {
            list = list.push(
                container(
                    text("No chats in this folder")
                        .size(m.font_s)
                        .color(p.text_muted),
                )
                .width(Length::Fill)
                .align_x(Alignment::Center)
                .padding([16, 16]),
            );
        }
    } else {
        let chats = app.chats.filter(&app.sidebar_query);
        list = list.push(section(app, "Chats"));
        if chats.is_empty() {
            list = list.push(
                container(text("No chats found").size(m.font_s).color(p.text_muted))
                    .padding([10, 16]),
            );
        }
        for c in chats {
            list = list.push(chat_row(app, c));
        }
        if !app.sidebar_messages.is_empty() {
            list = list.push(section(app, "Messages"));
            for msg in &app.sidebar_messages {
                if let Some(c) = app.chats.get(msg.chat_id) {
                    list = list.push(message_result_row(app, c, msg));
                }
            }
        }
        if !app.sidebar_global.is_empty() {
            list = list.push(section(app, "Global search results"));
            for hit in &app.sidebar_global {
                list = list.push(global_result_row(app, hit));
            }
        } else if app.global_searching {
            list = list.push(
                container(text("Searching…").size(m.font_s).color(p.text_muted)).padding([10, 16]),
            );
        }
    }

    if app.dialogs_loading && app.sidebar_query.trim().is_empty() {
        list = list.push(
            container(text("Loading…").size(m.font_s).color(p.text_muted))
                .width(Length::Fill)
                .align_x(Alignment::Center)
                .padding([10, 16]),
        );
    }

    let banner: Element<'_, Message> = match app.conn {
        ConnState::Connected => Space::new().height(0).into(),
        c => container(
            text(match c {
                ConnState::Connecting => "Connecting…",
                ConnState::Reconnecting { .. } => "Waiting for network…",
                _ => "Offline — showing cached chats",
            })
            .size(m.font_s)
            .color(p.text_secondary),
        )
        .width(Length::Fill)
        .padding([6, 16])
        .style(theme::fill(p.bg_elevated))
        .into(),
    };

    let menu: Element<'_, Message> = if app.main_menu {
        let name = app.me.as_ref().map(|u| u.name.clone()).unwrap_or_default();
        container(
            column![
                row![
                    avatar(app.me.as_ref().map_or(0, |u| u.id), &name, None, 40.0),
                    emoji_title(name, m.font_m, p.text_primary),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
                button(text("Chat folders").size(m.font_m))
                    .width(Length::Fill)
                    .padding([8, 10])
                    .on_press(Message::Ui(Ui::OpenFolders))
                    .style(theme::menu_item(p, false)),
                button(text("Settings").size(m.font_m))
                    .width(Length::Fill)
                    .padding([8, 10])
                    .on_press(Message::Ui(Ui::OpenSettings))
                    .style(theme::menu_item(p, false)),
                button(text("Log out").size(m.font_m))
                    .width(Length::Fill)
                    .padding([8, 10])
                    .on_press(Message::Ui(Ui::LogOut))
                    .style(theme::menu_item(p, true)),
            ]
            .spacing(8),
        )
        .padding(10)
        .width(Length::Fill)
        .style(theme::elevated(p))
        .into()
    } else {
        Space::new().height(0).into()
    };

    let strip = if app.sidebar_query.trim().is_empty() {
        folder_strip(app)
    } else {
        Space::new().height(0).into()
    };
    container(column![
        top,
        menu,
        strip,
        banner,
        scrollable(list)
            .on_scroll(|vp| Message::Ui(Ui::SidebarScrolled(vp)))
            .height(Length::Fill)
            .style(theme::scroll(p))
    ])
    .width(m.sidebar_width)
    .height(Length::Fill)
    .style(theme::fill(p.bg_sidebar))
    .into()
}

/// Folder tabs under the search box (only when the account has folders):
/// "All chats" plus one per folder, with the unread count of its unmuted
/// chats. Right click opens the tab's menu.
fn folder_strip(app: &App) -> Element<'_, Message> {
    let t = &app.theme;
    let (p, m) = (t.palette, t.metrics);
    if app.folders.list.is_empty() {
        return Space::new().height(0).into();
    }
    let tab = |id: Option<i32>, title: String, unread: u32| -> Element<'_, Message> {
        let selected = app.folders.selected == id;
        let mut label = row![emoji_plain(
            format::ellipsize(&title, 14),
            m.font_m,
            if selected { p.link } else { p.text_secondary }
        )]
        .spacing(6)
        .align_y(Alignment::Center);
        if unread > 0 {
            label = label.push(badge(t, format::badge_count(unread), false));
        }
        let b = button(label)
            .padding([6, 8])
            .on_press(Message::Ui(Ui::SelectFolder(id)))
            .style(theme::folder_tab(p, selected));
        let underline =
            container(Space::new().height(2).width(Length::Fill)).style(theme::fill(if selected {
                p.link
            } else {
                iced::Color::TRANSPARENT
            }));
        let col = column![b, underline].width(Length::Shrink);
        match id {
            Some(fid) => mouse_area(col)
                .on_right_press(Message::Ui(Ui::FolderTabMenu(Some(fid))))
                .into(),
            None => col.into(),
        }
    };
    let all_unread: u32 = app
        .chats
        .all()
        .iter()
        .filter(|c| !c.muted)
        .map(|c| c.unread_count)
        .sum();
    let mut tabs = row![tab(None, "All chats".into(), all_unread)]
        .spacing(2)
        .align_y(Alignment::Center);
    for f in &app.folders.list {
        let unread = crate::app::folders::FolderState::unread_in(f, app.chats.all());
        tabs = tabs.push(tab(Some(f.id), f.title.clone(), unread));
    }
    let strip = scrollable(container(tabs).padding(iced::Padding {
        bottom: 5.0,
        ..iced::Padding::ZERO
    }))
    .direction(scrollable::Direction::Horizontal(
        scrollable::Scrollbar::new()
            .width(3)
            .scroller_width(3)
            .margin(1),
    ))
    .width(Length::Fill);
    let mut col = column![container(strip).padding([0, 6])];
    if let Some(id) = app.folders.tab_menu
        && let Some(f) = app.folders.get(id)
    {
        let item = |label: &'static str, msg: Ui, danger: bool| {
            button(text(label).size(m.font_m))
                .width(Length::Fill)
                .padding([8, 12])
                .on_press(Message::Ui(msg))
                .style(theme::menu_item(p, danger))
        };
        col = col.push(
            container(
                column![
                    item("Edit folder", Ui::FolderEdit(f.id), false),
                    item("Delete folder…", Ui::FolderDeleteAsk(f.id), true),
                ]
                .spacing(0),
            )
            .padding(4)
            .width(Length::Fill)
            .style(theme::elevated(p)),
        );
    }
    col.into()
}

/// A chat row with its right-click menu ("Add to folder") under it.
fn chat_entry<'a>(app: &'a App, c: &'a Chat) -> Element<'a, Message> {
    let row_el = mouse_area(chat_row(app, c)).on_right_press(Message::Ui(Ui::ChatMenu(Some(c.id))));
    if app.folders.chat_menu != Some(c.id) {
        return row_el.into();
    }
    let t = &app.theme;
    let (p, m) = (t.palette, t.metrics);
    let mut items = column![
        container(
            text("Add to folder")
                .size(m.font_s)
                .font(FONT_SEMIBOLD)
                .color(p.text_secondary)
        )
        .padding([6, 12])
    ]
    .spacing(0);
    for f in &app.folders.list {
        if f.shared {
            continue;
        }
        let inside = f.matches(c);
        items = items.push(
            button(
                row![
                    container(if inside {
                        icon(Icon::Check, 17.0, p.link)
                    } else {
                        Space::new().width(17).into()
                    })
                    .width(17),
                    emoji_plain(format::ellipsize(&f.title, 24), m.font_m, p.text_primary),
                ]
                .spacing(12)
                .align_y(Alignment::Center),
            )
            .width(Length::Fill)
            .padding([8, 12])
            .on_press(Message::Ui(Ui::ChatInFolder(c.id, f.id)))
            .style(theme::menu_item(p, false)),
        );
    }
    items = items.push(
        button(
            row![
                Space::new().width(17),
                text("New folder…").size(m.font_m).color(p.link)
            ]
            .spacing(12),
        )
        .width(Length::Fill)
        .padding([8, 12])
        .on_press(Message::Ui(Ui::FolderNew(Some(c.id))))
        .style(theme::menu_item(p, false)),
    );
    column![
        row_el,
        container(items)
            .padding(4)
            .width(Length::Fill)
            .style(theme::elevated(p))
    ]
    .into()
}

fn section<'a>(app: &'a App, title: &'a str) -> Element<'a, Message> {
    let t = &app.theme;
    container(
        text(title)
            .size(t.metrics.font_s)
            .font(FONT_SEMIBOLD)
            .color(t.palette.text_secondary),
    )
    .width(Length::Fill)
    .padding([8, 16])
    .style(theme::fill(t.palette.bg_app))
    .into()
}

fn chat_row<'a>(app: &'a App, c: &'a Chat) -> Element<'a, Message> {
    let t = &app.theme;
    let p = t.palette;
    let m = t.metrics;
    let selected = app.selected == Some(c.id);
    let fg = if selected {
        p.selected_text
    } else {
        p.text_primary
    };
    let sec = if selected {
        p.selected_text
    } else {
        p.text_secondary
    };

    let mut title_row = row![].spacing(4).align_y(Alignment::Center);
    match c.kind {
        ChatKind::Group => title_row = title_row.push(icon(Icon::Users, 15.0, fg)),
        ChatKind::Channel => title_row = title_row.push(icon(Icon::Megaphone, 15.0, fg)),
        ChatKind::Bot => title_row = title_row.push(icon(Icon::Bot, 15.0, fg)),
        _ => {}
    }
    title_row = title_row.push(emoji_title(format::ellipsize(&c.title, 24), m.font_m, fg));
    if c.muted {
        title_row = title_row.push(icon(
            Icon::Muted,
            14.0,
            if selected { fg } else { p.text_muted },
        ));
    }
    title_row = title_row.push(Space::new().width(Length::Fill));
    if let Some(last) = &c.last_message {
        if last.outgoing && c.kind != ChatKind::SavedMessages {
            let read = last.id > 0 && last.id <= c.read_outbox_max_id;
            let col = if selected { fg } else { p.link };
            let i = match (last.state, read) {
                (SendState::Sending, _) | (SendState::Failed, _) => Icon::Check,
                (_, true) => Icon::CheckCheck,
                (_, false) => Icon::Check,
            };
            title_row = title_row.push(icon(i, 16.0, col));
        }
        title_row = title_row.push(
            text(format::list_time(c.last_date, app.now, app.tz_offset))
                .size(m.font_xs)
                .color(sec),
        );
    }

    let mut preview_row = row![].spacing(0).align_y(Alignment::Center);
    if let Some(status) = app.typing_status(c.id) {
        // Like Telegram Desktop: the preview becomes the live status.
        preview_row = preview_row.push(
            container(
                text(status)
                    .size(m.font_m)
                    .wrapping(Wrapping::None)
                    .color(if selected { fg } else { p.link }),
            )
            .width(Length::Fill)
            .clip(true),
        );
    } else if let Some(draft) = &c.draft {
        // Like Telegram Desktop: a red "Draft:" label before the text.
        preview_row = preview_row.push(text("Draft: ").size(m.font_m).color(if selected {
            fg
        } else {
            p.danger
        }));
        preview_row = preview_row.push(
            container(emoji_line(
                format::ellipsize(&draft.text, 30),
                m.font_m,
                if selected { fg } else { sec },
            ))
            .width(Length::Fill)
            .clip(true),
        );
    } else if let Some(last) = &c.last_message {
        let prefix =
            if last.outgoing && c.kind != ChatKind::SavedMessages && c.kind != ChatKind::Channel {
                Some("You".to_owned())
            } else if c.kind == ChatKind::Group {
                last.sender_name
                    .as_ref()
                    .map(|n| n.split_whitespace().next().unwrap_or(n).to_owned())
            } else {
                None
            };
        if let Some(pre) = prefix {
            preview_row = preview_row.push(emoji_plain(
                format!("{pre}: "),
                m.font_m,
                if selected { fg } else { p.text_primary },
            ));
        }
        let media_only = last.text.trim().is_empty() && last.media.is_some();
        preview_row = preview_row.push(
            container(emoji_line(
                format::ellipsize(
                    &last.preview(),
                    if c.kind == ChatKind::Group { 24 } else { 30 },
                ),
                m.font_m,
                if media_only && !selected { p.link } else { sec },
            ))
            .width(Length::Fill)
            .clip(true),
        );
    } else {
        preview_row = preview_row.push(Space::new().width(Length::Fill));
    }
    if c.unread_count > 0 {
        preview_row = preview_row.push(badge(t, format::badge_count(c.unread_count), c.muted));
    } else if c.pinned {
        preview_row = preview_row.push(icon(
            Icon::Pin,
            15.0,
            if selected { fg } else { p.text_muted },
        ));
    }

    let content = row![
        avatar_photo(
            app.avatar(c.id),
            c.id,
            &c.title,
            Some(c.kind),
            m.avatar_list
        ),
        column![title_row, preview_row]
            .spacing(4)
            .width(Length::Fill),
    ]
    .spacing(11)
    .align_y(Alignment::Center);

    button(content)
        .padding([6, 10])
        .height(m.chat_row_height)
        .width(Length::Fill)
        .on_press(Message::Ui(Ui::SelectChat(c.id)))
        .style(theme::chat_row(p, selected))
        .into()
}

fn global_result_row<'a>(app: &'a App, hit: &'a GlobalHit) -> Element<'a, Message> {
    let t = &app.theme;
    let p = t.palette;
    let m = t.metrics;
    let msg = &hit.message;
    let content = row![
        avatar_photo(
            app.avatar(msg.chat_id),
            msg.chat_id,
            &hit.chat_title,
            Some(hit.chat_kind),
            m.avatar_list
        ),
        column![
            row![
                emoji_title(
                    format::ellipsize(&hit.chat_title, 24),
                    m.font_m,
                    p.text_primary
                ),
                Space::new().width(Length::Fill),
                text(format::list_time(msg.date, app.now, app.tz_offset))
                    .size(m.font_xs)
                    .color(p.text_secondary),
            ],
            container(emoji_line(
                format::ellipsize(&msg.preview(), 36),
                m.font_m,
                p.text_secondary
            ))
            .width(Length::Fill)
            .clip(true),
        ]
        .spacing(4),
    ]
    .spacing(11)
    .align_y(Alignment::Center);
    button(content)
        .padding([6, 10])
        .height(m.chat_row_height)
        .width(Length::Fill)
        .on_press(Message::Ui(Ui::OpenResult(msg.chat_id, msg.id)))
        .style(theme::chat_row(p, false))
        .into()
}

fn message_result_row<'a>(app: &'a App, c: &'a Chat, msg: &'a ChatMessage) -> Element<'a, Message> {
    let t = &app.theme;
    let p = t.palette;
    let m = t.metrics;
    let content = row![
        avatar_photo(
            app.avatar(c.id),
            c.id,
            &c.title,
            Some(c.kind),
            m.avatar_list
        ),
        column![
            row![
                emoji_title(format::ellipsize(&c.title, 24), m.font_m, p.text_primary),
                Space::new().width(Length::Fill),
                text(format::list_time(msg.date, app.now, app.tz_offset))
                    .size(m.font_xs)
                    .color(p.text_secondary),
            ],
            container(emoji_line(
                format::ellipsize(&msg.preview(), 36),
                m.font_m,
                p.text_secondary
            ))
            .width(Length::Fill)
            .clip(true),
        ]
        .spacing(4),
    ]
    .spacing(11)
    .align_y(Alignment::Center);
    button(content)
        .padding([6, 10])
        .height(m.chat_row_height)
        .width(Length::Fill)
        .on_press(Message::Ui(Ui::OpenResult(c.id, msg.id)))
        .style(theme::chat_row(p, false))
        .into()
}
