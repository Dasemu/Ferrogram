//! Right-hand info panel: profile, notifications and the shared-content
//! tabs (Media, Files, Links, Music, Voice, GIFs).

use iced::widget::{
    Column, Space, button, column, container, image, row, scrollable, text, toggler,
};
use iced::{Alignment, Color, ContentFit, Element, Length};

use super::format;
use super::icons::Icon;
use super::theme::{self, FONT_SEMIBOLD};
use super::widgets::{avatar_photo, emoji_plain, emoji_title, icon};
use crate::app::{App, Message, Ui};
use crate::model::{ChatKind, SharedBody, SharedItem, SharedKind};

use crate::app::info::{CELL, COLUMNS, FOOTER_H, GAP, GRID_PAD, ROW_H, grid_rows};

pub const PANEL_WIDTH: f32 = 340.0;

pub fn view<'a>(app: &'a App) -> Option<Element<'a, Message>> {
    let panel = app.info.as_ref()?;
    let chat = app.chats.get(panel.chat)?;
    let (p, m) = (app.theme.palette, app.theme.metrics);

    let top = container(
        row![
            text("Info")
                .size(m.font_l)
                .font(FONT_SEMIBOLD)
                .color(p.text_primary)
                .width(Length::Fill),
            button(icon(Icon::X, 20.0, p.text_secondary))
                .padding(6)
                .on_press(Message::Ui(Ui::ToggleInfo))
                .style(theme::icon_button(p, true)),
        ]
        .align_y(Alignment::Center),
    )
    .padding([0, 14])
    .height(m.header_height)
    .align_y(Alignment::Center)
    .width(Length::Fill)
    .style(theme::fill(p.bg_sidebar));

    // Who: avatar, name, short status.
    let status = match (chat.kind, panel.info.as_ref().and_then(|i| i.members)) {
        (ChatKind::Group, Some(n)) => format!("{} members", format::badge_count_full(n)),
        (ChatKind::Channel, Some(n)) => format!("{} subscribers", format::badge_count_full(n)),
        (ChatKind::Group, None) => "group".to_owned(),
        (ChatKind::Channel, None) => "channel".to_owned(),
        (ChatKind::Bot, _) => "bot".to_owned(),
        (ChatKind::SavedMessages, _) => String::new(),
        _ => format::presence_text(chat.presence, app.now, app.tz_offset).0,
    };
    let mut body = Column::new().spacing(0).width(Length::Fill);
    body = body.push(
        container(
            column![
                avatar_photo(
                    app.avatar(chat.id),
                    chat.id,
                    &chat.title,
                    Some(chat.kind),
                    96.0
                ),
                emoji_title(chat.title.clone(), m.font_l, p.text_primary),
                text(status).size(m.font_s).color(p.text_secondary),
                call_action(app, chat),
            ]
            .spacing(6)
            .align_x(Alignment::Center),
        )
        .width(Length::Fill)
        .align_x(Alignment::Center)
        .padding([18, 14]),
    );

    // Details: only what Telegram told us (phone only when visible).
    let detail =
        |ic: Icon, value: Element<'a, Message>, label: &'static str| -> Element<'a, Message> {
            row![
                container(icon(ic, 20.0, p.text_muted)).width(28),
                column![value, text(label).size(m.font_xs).color(p.text_secondary)].spacing(2),
            ]
            .spacing(10)
            .padding([8, 16])
            .into()
        };
    if let Some(info) = &panel.info {
        if let Some(phone) = &info.phone {
            body = body.push(detail(
                Icon::Phone,
                text(phone.clone())
                    .size(m.font_m)
                    .color(p.text_primary)
                    .into(),
                "Phone",
            ));
        }
        if let Some(u) = &info.username {
            body = body.push(detail(
                Icon::AtSign,
                text(format!("@{u}"))
                    .size(m.font_m)
                    .color(p.text_primary)
                    .into(),
                "Username",
            ));
        }
        if let Some(about) = &info.about {
            body = body.push(detail(
                Icon::Info,
                emoji_plain(about.clone(), m.font_m, p.text_primary),
                if chat.kind == ChatKind::User {
                    "Bio"
                } else {
                    "Description"
                },
            ));
        }
    }
    if chat.kind != ChatKind::SavedMessages {
        body = body.push(
            container(
                row![
                    container(icon(
                        if chat.muted {
                            Icon::BellOff
                        } else {
                            Icon::Bell
                        },
                        20.0,
                        p.text_muted
                    ))
                    .width(28),
                    text("Notifications")
                        .size(m.font_m)
                        .color(p.text_primary)
                        .width(Length::Fill),
                    toggler(!chat.muted)
                        .on_toggle(|on| Message::Ui(Ui::SetNotifications(on)))
                        .size(20)
                        .style(theme::switch(p)),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            )
            .padding([8, 16]),
        );
    }
    body = body.push(
        container(Space::new().height(8))
            .style(theme::fill(p.bg_app))
            .width(Length::Fill),
    );

    // Tabs.
    let mut tabs = row![].spacing(2);
    for kind in SharedKind::ALL {
        let on = panel.tab == kind;
        tabs = tabs.push(column![
            button(text(kind.label()).size(m.font_m))
                .padding([8, 6])
                .on_press(Message::Ui(Ui::InfoTab(kind)))
                .style(theme::folder_tab(p, on)),
            container(Space::new().height(2).width(Length::Fill)).style(theme::fill(if on {
                p.link
            } else {
                Color::TRANSPARENT
            })),
        ]);
    }
    body = body.push(
        scrollable(container(tabs).padding([0, 6]))
            .direction(scrollable::Direction::Horizontal(
                scrollable::Scrollbar::new()
                    .width(3)
                    .scroller_width(3)
                    .margin(1),
            ))
            .width(Length::Fill),
    );
    body = body.push(tab_content(app, panel.chat, panel.tab));

    Some(
        container(column![
            top,
            scrollable(body)
                .on_scroll(|vp| Message::Ui(Ui::InfoScrolled(vp)))
                .height(Length::Fill)
                .style(theme::scroll(p)),
        ])
        .width(PANEL_WIDTH)
        .height(Length::Fill)
        .style(theme::fill(p.bg_sidebar))
        .into(),
    )
}

fn tab_content<'a>(app: &'a App, chat: i64, kind: SharedKind) -> Element<'a, Message> {
    let (p, m) = (app.theme.palette, app.theme.metrics);
    let Some(panel) = app.info.as_ref() else {
        return Space::new().into();
    };
    static EMPTY: std::sync::OnceLock<crate::app::info::SharedTab> = std::sync::OnceLock::new();
    let tab = panel
        .tab_ref(kind)
        .unwrap_or_else(|| EMPTY.get_or_init(crate::app::info::SharedTab::default));
    let mut col = Column::new().width(Length::Fill);
    if kind.has_thumbs() {
        // Rows have a fixed height, so the ones outside the viewport (and a
        // small margin) are replaced by two spacers of the exact height:
        // layout, drawing and event handling stay constant however many
        // items are loaded.
        let total = grid_rows(tab.items.len());
        let rows = panel.window(kind);
        let mut grid = Column::new().width(Length::Fill);
        grid = grid.push(Space::new().height(GRID_PAD + rows.start as f32 * ROW_H));
        for chunk in tab.items.chunks(COLUMNS).skip(rows.start).take(rows.len()) {
            let mut r = row![].spacing(GAP);
            for item in chunk {
                r = r.push(thumb_cell(app, chat, item));
            }
            grid = grid.push(container(r).height(ROW_H).padding(iced::Padding {
                left: GAP,
                bottom: GAP,
                ..iced::Padding::ZERO
            }));
        }
        grid = grid.push(Space::new().height((total - rows.end) as f32 * ROW_H));
        col = col.push(grid);
    } else {
        for item in &tab.items {
            col = col.push(list_row(app, chat, item));
        }
    }
    let mut footer = Column::new().width(Length::Fill);
    let note = if tab.loading() {
        Some("Loading…".to_owned())
    } else if tab.failed {
        None
    } else if tab.loaded && tab.items.is_empty() {
        Some(format!("No {} yet", kind.label().to_lowercase()))
    } else {
        None
    };
    if let Some(n) = note {
        footer = footer.push(
            container(text(n).size(m.font_s).color(p.text_muted))
                .width(Length::Fill)
                .align_x(Alignment::Center)
                .padding(16),
        );
    }
    if tab.failed {
        footer = footer.push(
            container(
                button(
                    text("Could not load. Try again")
                        .size(m.font_s)
                        .color(p.link),
                )
                .padding([6, 12])
                .on_press(Message::Ui(Ui::InfoTab(kind)))
                .style(theme::menu_item(p, false)),
            )
            .width(Length::Fill)
            .align_x(Alignment::Center)
            .padding(8),
        );
    }
    // Under a virtualised grid the footer has a fixed height, so the content
    // height is exactly header + grid + footer (see `grid_top_from_content`).
    col = col.push(if kind.has_thumbs() {
        Element::from(container(footer).width(Length::Fill).height(FOOTER_H))
    } else {
        footer.into()
    });
    col.into()
}

fn open_message(chat: i64, item: &SharedItem) -> Message {
    match item.body {
        // Photos open the viewer; everything else jumps to its message.
        SharedBody::Photo { .. } => Message::Ui(Ui::OpenMedia(chat, item.id)),
        _ => Message::Ui(Ui::OpenResult(chat, item.id)),
    }
}

fn thumb_cell<'a>(app: &'a App, chat: i64, item: &'a SharedItem) -> Element<'a, Message> {
    let p = app.theme.palette;
    let inner: Element<'a, Message> = match app.thumbs.peek(&(chat, item.id)) {
        Some(h) => image(h.clone())
            .width(CELL)
            .height(CELL)
            .content_fit(ContentFit::Cover)
            .into(),
        None => container(icon(Icon::Image, 28.0, p.text_muted))
            .width(CELL)
            .height(CELL)
            .center(CELL)
            .style(theme::fill(p.bg_hover))
            .into(),
    };
    let mut layers = iced::widget::stack![inner].width(CELL).height(CELL);
    let badge = match item.body {
        SharedBody::Video { seconds, .. } => Some(format::duration(seconds)),
        SharedBody::Gif { .. } => Some("GIF".to_owned()),
        _ => None,
    };
    if let Some(b) = badge {
        layers = layers.push(
            container(
                container(text(b).size(app.theme.metrics.font_xs).color(Color::WHITE))
                    .padding([1, 6])
                    .style(theme::pill(Color::from_rgba(0.0, 0.0, 0.0, 0.55), 8.0)),
            )
            .padding(4)
            .width(CELL)
            .height(CELL),
        );
    }
    button(layers)
        .padding(0)
        .on_press(open_message(chat, item))
        .style(theme::link_button(p))
        .into()
}

fn list_row<'a>(app: &'a App, chat: i64, item: &'a SharedItem) -> Element<'a, Message> {
    let (p, m) = (app.theme.palette, app.theme.metrics);
    let circle = |ic: Icon| {
        container(icon(ic, 20.0, Color::WHITE))
            .width(40)
            .height(40)
            .center(40)
            .style(theme::pill(p.accent, 20.0))
    };
    let when = format::list_time(item.date, app.now, app.tz_offset);
    let (lead, title, sub, msg): (Element<'a, Message>, String, String, Message) = match &item.body
    {
        SharedBody::File { name, size, .. } => (
            circle(Icon::File).into(),
            format::ellipsize(name, 30),
            format!("{} · {when}", format::file_size(*size)),
            open_message(chat, item),
        ),
        SharedBody::Audio {
            title,
            performer,
            name,
            seconds,
        } => (
            circle(Icon::Music).into(),
            format::ellipsize(title.as_deref().or(name.as_deref()).unwrap_or("Audio"), 30),
            format!(
                "{}{} · {when}",
                performer
                    .as_deref()
                    .map(|a| format!("{a} · "))
                    .unwrap_or_default(),
                format::duration(*seconds)
            ),
            open_message(chat, item),
        ),
        SharedBody::Voice { seconds } => (
            circle(Icon::Mic).into(),
            "Voice message".to_owned(),
            format!("{} · {when}", format::duration(*seconds)),
            open_message(chat, item),
        ),
        SharedBody::Link {
            url,
            title,
            description,
            site,
        } => {
            let head = title
                .clone()
                .or_else(|| site.clone())
                .unwrap_or_else(|| format::ellipsize(url, 40));
            let sub = description.clone().unwrap_or_else(|| when.clone());
            (
                circle(Icon::Link).into(),
                format::ellipsize(&head, 36),
                format::ellipsize(&sub, 44),
                // Links open in the browser, not in the chat.
                Message::Ui(Ui::OpenLink(url.clone())),
            )
        }
        _ => return Space::new().into(),
    };
    let mut details = column![
        emoji_plain(title, m.font_m, p.text_primary),
        emoji_plain(sub, m.font_xs, p.text_secondary),
    ]
    .spacing(2)
    .width(Length::Fill);
    if let SharedBody::Link { url, .. } = &item.body {
        details = details.push(
            text(format::ellipsize(url, 44))
                .size(m.font_xs)
                .color(p.link),
        );
    }
    button(row![lead, details].spacing(12).align_y(Alignment::Center))
        .width(Length::Fill)
        .padding([8, 14])
        .on_press(msg)
        .style(theme::menu_item(p, false))
        .into()
}

/// "Call" under the name of a user, like the official profile actions.
fn call_action<'a>(app: &'a App, chat: &'a crate::model::Chat) -> Element<'a, Message> {
    if chat.kind != ChatKind::User {
        return Space::new().into();
    }
    let p = app.theme.palette;
    button(
        row![
            icon(Icon::Phone, 18.0, p.link),
            text("Call").size(app.theme.metrics.font_m)
        ]
        .spacing(6)
        .align_y(Alignment::Center),
    )
    .padding([6, 12])
    .on_press(Message::Ui(Ui::CallStart(chat.id)))
    .style(theme::link_button(p))
    .into()
}
