//! Right pane: header, history with bubbles, reply/edit bar, composer.

use iced::widget::{
    Column, Space, button, column, container, image, mouse_area, opaque, row, scrollable, stack,
    svg, text, text_input,
};
use iced::{Alignment, Color, ContentFit, Element, Length, Padding};

use super::format;
use super::icons::{Icon, tail_handle};
use super::theme::{self, FONT_SEMIBOLD, NAME_COLORS};
use super::widgets::{EMOJI_FONT, avatar_photo, emoji_plain, emoji_text, emoji_title, icon};
use crate::app::state::ComposeMode;
use crate::app::{App, CHAT_SEARCH_ID, COMPOSER_ID, HISTORY_ID, Message, Ui};
use crate::model::{Chat, ChatKind, Media, Message as ChatMessage, SendState, avatar_color_index};

pub const QUICK_REACTIONS: [&str; 8] = ["👍", "❤️", "🔥", "😂", "😮", "😢", "🎉", "🙏"];
pub const EMOJI_PANEL: [&str; 32] = [
    "😀", "😂", "🥲", "😊", "😍", "😘", "😎", "🤔", "😴", "😭", "😡", "🥳", "👍", "👎", "👏", "🙏",
    "💪", "🤝", "❤️", "🔥", "✨", "🎉", "💯", "✅", "☕", "🍕", "🍺", "🏔️", "🚀", "💻", "📎", "👀",
];

pub fn view(app: &App) -> Element<'_, Message> {
    let p = app.theme.palette;
    let (Some(chat_id), Some(conv)) = (app.selected, &app.conversation) else {
        return container(
            container(
                text("Select a chat to start messaging")
                    .size(app.theme.metrics.font_s)
                    .color(Color::WHITE),
            )
            .padding([4, 12])
            .style(theme::pill(p.service_bg, 12.0)),
        )
        .center(Length::Fill)
        .style(theme::fill(p.bg_panel))
        .into();
    };
    let chat = app.chats.get(chat_id);

    let mut col = Column::new().width(Length::Fill).push(header(app, chat));
    if let Some(s) = &app.chat_search {
        col = col.push(search_bar(app, s));
    }

    let history = history(app, chat, conv.messages(), conv.loading_older);
    let mut layers = stack![history].width(Length::Fill).height(Length::Fill);
    if !app.at_bottom || conv.detached {
        layers = layers.push(
            container(
                button(icon(Icon::ArrowDown, 22.0, p.text_secondary))
                    .padding(11)
                    .on_press(Message::Ui(Ui::ScrollToBottom))
                    .style(move |_, _| button::Style {
                        background: Some(p.bg_sidebar.into()),
                        border: iced::border::rounded(22.0),
                        ..button::Style::default()
                    }),
            )
            .align_right(Length::Fill)
            .align_bottom(Length::Fill)
            .padding(16),
        );
    }
    if let Some(s) = &app.chat_search
        && s.submitted
    {
        layers = layers.push(search_results(app, s));
    }
    if app.emoji_open {
        layers = layers.push(emoji_panel(app));
    }
    col = col.push(layers);
    if app.compose_mode != ComposeMode::Normal {
        col = col.push(compose_bar(app));
    }
    if let Some(f) = &app.pending_file {
        col = col.push(pending_file_bar(app, f));
    }
    col = col.push(composer(app));

    container(col)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::fill(p.bg_panel))
        .into()
}

fn header<'a>(app: &'a App, chat: Option<&'a Chat>) -> Element<'a, Message> {
    let t = &app.theme;
    let (p, m) = (t.palette, t.metrics);
    let title = chat.map_or("", |c| c.title.as_str());
    let typing = chat.and_then(|c| app.typing_status(c.id));
    let (mut status, mut live) = match chat.map(|c| c.kind) {
        Some(ChatKind::Bot) => ("bot".to_owned(), false),
        Some(ChatKind::Group) => ("group".to_owned(), false),
        Some(ChatKind::Channel) => ("channel".to_owned(), false),
        Some(ChatKind::SavedMessages) => (String::new(), false),
        Some(_) => format::presence_text(
            chat.map_or(crate::model::Presence::Unknown, |c| c.presence),
            app.now,
            app.tz_offset,
        ),
        None => (String::new(), false),
    };
    if let Some(t) = typing {
        status = t;
        live = true;
    }
    let ib = |i: Icon, msg: Option<Message>| {
        button(icon(i, 21.0, p.text_secondary))
            .padding(8)
            .on_press_maybe(msg)
            .style(theme::icon_button(p, true))
    };
    container(
        row![
            mouse_area(
                row![
                    avatar_photo(
                        chat.and_then(|c| app.avatar(c.id)),
                        chat.map_or(0, |c| c.id),
                        title,
                        chat.map(|c| c.kind),
                        m.avatar_header
                    ),
                    column![
                        emoji_title(title.to_string(), m.font_l - 0.5, p.text_primary),
                        text(status).size(m.font_s).color(if live {
                            p.link
                        } else {
                            p.text_secondary
                        }),
                    ]
                    .spacing(2),
                ]
                .spacing(10)
                .align_y(Alignment::Center)
            )
            .on_press(Message::Ui(Ui::ToggleInfo))
            .interaction(iced::mouse::Interaction::Pointer),
            Space::new().width(Length::Fill),
            ib(Icon::Search, Some(Message::Ui(Ui::ToggleChatSearch))),
            ib(Icon::Phone, None),
            ib(Icon::More, None),
        ]
        .spacing(10)
        .align_y(Alignment::Center),
    )
    .padding([0, 14])
    .width(Length::Fill)
    .height(m.header_height)
    .align_y(Alignment::Center)
    .style(theme::fill(p.bg_sidebar))
    .into()
}

fn search_bar<'a>(app: &'a App, s: &'a crate::app::ChatSearch) -> Element<'a, Message> {
    let t = &app.theme;
    let (p, m) = (t.palette, t.metrics);
    let count = if s.searching {
        "Searching…".to_owned()
    } else if s.submitted {
        format!("{} results", s.results.len())
    } else {
        String::new()
    };
    container(
        row![
            icon(Icon::Search, 16.0, p.text_muted),
            text_input("Search messages in this chat", &s.query)
                .id(CHAT_SEARCH_ID)
                .on_input(|q| Message::Ui(Ui::ChatSearchInput(q)))
                .on_submit(Message::Ui(Ui::ChatSearchSubmit))
                .padding([6, 4])
                .size(m.font_m)
                .style(theme::input(p, p.bg_sidebar, 0.0, false)),
            text(count).size(m.font_s).color(p.text_secondary),
            button(icon(Icon::X, 18.0, p.text_secondary))
                .padding(6)
                .on_press(Message::Ui(Ui::ToggleChatSearch))
                .style(theme::icon_button(p, true)),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .padding([4, 16])
    .width(Length::Fill)
    .style(move |_| container::Style {
        background: Some(p.bg_sidebar.into()),
        border: iced::Border {
            width: 0.0,
            ..Default::default()
        },
        shadow: iced::Shadow {
            color: p.border,
            offset: iced::Vector::new(0.0, -1.0),
            blur_radius: 0.0,
        },
        ..Default::default()
    })
    .into()
}

fn search_results<'a>(app: &'a App, s: &'a crate::app::ChatSearch) -> Element<'a, Message> {
    let t = &app.theme;
    let (p, m) = (t.palette, t.metrics);
    let mut list = Column::new();
    for msg in s.results.iter().take(30) {
        let who = if msg.outgoing {
            "You".to_owned()
        } else {
            msg.sender_name
                .clone()
                .or_else(|| app.chats.get(msg.chat_id).map(|c| c.title.clone()))
                .unwrap_or_default()
        };
        list = list.push(
            button(
                column![
                    row![
                        emoji_title(who.to_string(), m.font_s, p.text_primary),
                        Space::new().width(Length::Fill),
                        text(format::list_time(msg.date, app.now, app.tz_offset))
                            .size(m.font_xs)
                            .color(p.text_secondary),
                    ],
                    emoji_text(
                        format::ellipsize(&msg.preview(), 60),
                        m.font_s,
                        p.text_secondary
                    ),
                ]
                .spacing(2),
            )
            .width(Length::Fill)
            .padding([8, 12])
            .on_press(Message::Ui(Ui::OpenResult(msg.chat_id, msg.id)))
            .style(theme::menu_item(p, false)),
        );
    }
    if s.results.is_empty() && !s.searching {
        list = list.push(
            container(text("No messages found").size(m.font_s).color(p.text_muted)).padding(12),
        );
    }
    container(opaque(
        container(scrollable(list).style(theme::scroll(p)))
            .width(360)
            .max_height(300)
            .padding(4)
            .style(theme::elevated(p)),
    ))
    .align_right(Length::Fill)
    .padding(Padding {
        top: 6.0,
        right: 12.0,
        bottom: 0.0,
        left: 0.0,
    })
    .into()
}

/// Whether `next` continues the same visual group as `m` (same sender, same
/// day, within 5 minutes): no tail and no avatar on `m` then.
fn same_group(m: &ChatMessage, next: Option<&ChatMessage>, offset: i32) -> bool {
    next.is_some_and(|n| {
        !n.service
            && n.outgoing == m.outgoing
            && n.sender_id == m.sender_id
            && n.date - m.date < 300
            && format::day_key(n.date, offset) == format::day_key(m.date, offset)
    })
}

fn history<'a>(
    app: &'a App,
    chat: Option<&'a Chat>,
    msgs: &'a [ChatMessage],
    loading: bool,
) -> Element<'a, Message> {
    let t = &app.theme;
    let (p, m) = (t.palette, t.metrics);
    let mut col = Column::new().spacing(2).padding(Padding {
        top: 10.0,
        right: 16.0,
        bottom: 10.0,
        left: 16.0,
    });
    if loading {
        col = col.push(
            container(pill(app, "Loading…".into()))
                .width(Length::Fill)
                .align_x(Alignment::Center),
        );
    }
    let is_group = chat.is_some_and(|c| c.kind == ChatKind::Group);
    let mut last_day = None;
    for (i, msg) in msgs.iter().enumerate() {
        let day = format::day_key(msg.date, app.tz_offset);
        if last_day != Some(day) {
            last_day = Some(day);
            col = col.push(
                container(pill(
                    app,
                    format::day_separator(msg.date, app.now, app.tz_offset),
                ))
                .width(Length::Fill)
                .align_x(Alignment::Center)
                .padding([8, 0]),
            );
        }
        if msg.service {
            col = col.push(
                container(pill(app, msg.text.clone()))
                    .width(Length::Fill)
                    .align_x(Alignment::Center)
                    .padding([4, 0]),
            );
            continue;
        }
        let tail = !same_group(msg, msgs.get(i + 1), app.tz_offset);
        let first = i == 0 || !same_group(&msgs[i - 1], Some(msg), app.tz_offset);
        col = col.push(message_row(app, chat, msg, tail, first && is_group));
        if tail {
            col = col.push(Space::new().height(6));
        }
        if app.menu_for == Some(msg.id) {
            col = col.push(message_menu(app, msg));
        }
    }
    let _ = m;
    scrollable(col)
        .id(HISTORY_ID)
        .anchor_bottom()
        .on_scroll(|v| Message::Ui(Ui::HistoryScrolled(v)))
        .height(Length::Fill)
        .width(Length::Fill)
        .style(theme::scroll(p))
        .into()
}

fn pill<'a>(app: &'a App, label: String) -> Element<'a, Message> {
    let t = &app.theme;
    container(
        text(label)
            .size(t.metrics.font_s)
            .font(FONT_SEMIBOLD)
            .color(Color::WHITE),
    )
    .padding([3, 10])
    .style(theme::pill(t.palette.service_bg, 12.0))
    .into()
}

fn sender_color(id: Option<i64>) -> Color {
    NAME_COLORS[avatar_color_index(id.unwrap_or(0))]
}

fn message_row<'a>(
    app: &'a App,
    chat: Option<&'a Chat>,
    msg: &'a ChatMessage,
    tail: bool,
    show_name: bool,
) -> Element<'a, Message> {
    let t = &app.theme;
    let (p, m) = (t.palette, t.metrics);
    let out = msg.outgoing;
    let is_group = chat.is_some_and(|c| c.kind == ChatKind::Group) && !out;

    let mut body = Column::new().spacing(4);
    if show_name
        && !out
        && let Some(name) = &msg.sender_name
    {
        body = body.push(emoji_title(
            name.clone(),
            m.font_s,
            sender_color(msg.sender_id),
        ));
    }
    if let Some(reply_id) = msg.reply_to {
        body = body.push(reply_quote(app, chat, reply_id, out));
    }
    let mut has_media_block = false;
    if let Some(media) = &msg.media {
        has_media_block = true;
        body = body.push(media_view(app, msg, media));
    }

    let time_col = if out { p.time_out } else { p.time_in };
    let mut meta = row![].spacing(3).align_y(Alignment::Center);
    if msg.edited {
        meta = meta.push(text("edited").size(m.font_xs).color(time_col));
    }
    meta = meta.push(
        text(format::clock(msg.date, app.tz_offset))
            .size(m.font_xs)
            .color(time_col),
    );
    if out {
        let read = msg.id > 0 && chat.is_some_and(|c| msg.id <= c.read_outbox_max_id);
        let el: Element<'a, Message> = match msg.state {
            SendState::Sending => icon(
                Icon::Check,
                14.0,
                Color {
                    a: 0.4,
                    ..p.check_out
                },
            ),
            SendState::Failed => text("!").size(m.font_s).color(p.danger).into(),
            SendState::Sent if read => icon(Icon::CheckCheck, 15.0, p.check_out),
            SendState::Sent => icon(Icon::Check, 15.0, p.check_out),
        };
        meta = meta.push(el);
    }

    let mut meta = Some(meta);
    let short = !msg.text.contains('\n')
        && msg.text.chars().count() < 34
        && msg.reactions.is_empty()
        && !msg.text.is_empty();
    if !msg.text.is_empty() {
        let txt = emoji_text(msg.text.clone(), m.font_m, p.text_primary);
        if let (true, Some(mt)) = (short, meta.take_if(|_| short)) {
            body = body.push(row![txt, Space::new().width(10), mt].align_y(Alignment::End));
        } else {
            body = body.push(txt);
        }
    }
    if !short {
        let mut bottom = row![].spacing(4).align_y(Alignment::Center);
        for r in &msg.reactions {
            bottom = bottom.push(
                button(
                    row![
                        text(r.emoji.clone()).size(m.font_s).font(EMOJI_FONT),
                        text(r.count.to_string())
                            .size(m.font_xs)
                            .font(FONT_SEMIBOLD),
                    ]
                    .spacing(4)
                    .align_y(Alignment::Center),
                )
                .padding([2, 8])
                .on_press(Message::Ui(Ui::React(msg.id, r.emoji.clone())))
                .style(theme::reaction_chip(p, out, r.chosen)),
            );
        }
        bottom = bottom.push(Space::new().width(Length::Fill));
        if let Some(mt) = meta.take() {
            bottom = bottom.push(mt);
        }
        body = body.push(bottom);
    }

    let photo_only = matches!(msg.media, Some(Media::Photo { .. })) && msg.text.is_empty();
    let pad = if has_media_block && photo_only {
        Padding::new(4.0)
    } else {
        Padding {
            top: 6.0,
            right: 10.0,
            bottom: 6.0,
            left: 11.0,
        }
    };
    let bubble = container(body)
        .padding(pad)
        .max_width(m.bubble_max_width)
        .style(theme::bubble(p, m, out, tail));
    // Messages still being sent (temporary negative ids) get no actions.
    let bubble = if msg.id > 0 {
        mouse_area(bubble)
            .on_right_press(Message::Ui(Ui::MessageMenu(Some(msg.id))))
            .on_double_click(Message::Ui(Ui::Reply(msg.id)))
    } else {
        mouse_area(bubble)
    };

    let tail_el: Element<'a, Message> = if tail {
        svg(tail_handle(out))
            .width(9)
            .height(17)
            .style(theme::tint(if out { p.bubble_out } else { p.bubble_in }))
            .into()
    } else {
        Space::new().width(9).into()
    };

    let highlighted = app.highlight == Some(msg.id);
    let mut r = row![].align_y(Alignment::End);
    if out {
        r = r
            .push(Space::new().width(Length::Fill))
            .push(bubble)
            .push(tail_el);
    } else {
        if is_group {
            let av: Element<'a, Message> = if tail {
                avatar_photo(
                    msg.sender_id.and_then(|id| app.avatar(id)),
                    msg.sender_id.unwrap_or(0),
                    msg.sender_name.as_deref().unwrap_or("?"),
                    None,
                    m.avatar_msg,
                )
            } else {
                Space::new().width(m.avatar_msg).into()
            };
            r = r.push(av).push(Space::new().width(6));
        }
        r = r
            .push(tail_el)
            .push(bubble)
            .push(Space::new().width(Length::Fill));
    }
    let el = container(r).width(Length::Fill);
    if highlighted {
        el.style(theme::fill(Color {
            a: 0.18,
            ..p.accent
        }))
        .into()
    } else {
        el.into()
    }
}

fn reply_quote<'a>(
    app: &'a App,
    chat: Option<&'a Chat>,
    reply_id: i32,
    out: bool,
) -> Element<'a, Message> {
    let t = &app.theme;
    let (p, m) = (t.palette, t.metrics);
    let target = app.conversation.as_ref().and_then(|c| c.get(reply_id));
    let (name, color) = match target {
        Some(r) if r.outgoing => (
            app.me.as_ref().map_or("You".to_owned(), |u| u.name.clone()),
            if out { Color::WHITE } else { p.link },
        ),
        Some(r) => (
            r.sender_name
                .clone()
                .or_else(|| chat.map(|c| c.title.clone()))
                .unwrap_or_default(),
            if out {
                Color::WHITE
            } else {
                sender_color(r.sender_id)
            },
        ),
        None => ("Message".into(), p.link),
    };
    let preview = target.map_or_else(|| "Deleted message".to_owned(), |r| r.preview());
    let bar = if out { Color::WHITE } else { color };
    container(
        row![
            container(Space::new().width(3).height(34)).style(theme::pill(bar, 2.0)),
            column![
                emoji_title(name.to_string(), m.font_s, color),
                emoji_text(format::ellipsize(&preview, 40), m.font_s, p.text_primary),
            ]
            .spacing(1),
        ]
        .spacing(7)
        .align_y(Alignment::Center),
    )
    .padding(Padding {
        top: 2.0,
        right: 8.0,
        bottom: 2.0,
        left: 0.0,
    })
    .style(theme::pill(Color { a: 0.12, ..bar }, 4.0))
    .into()
}

fn media_view<'a>(app: &'a App, msg: &'a ChatMessage, media: &'a Media) -> Element<'a, Message> {
    let t = &app.theme;
    let (p, m) = (t.palette, t.metrics);
    let out = msg.outgoing;
    match media {
        Media::Photo { width, height, .. } => {
            let w = 360.0_f32;
            let ratio = if *width > 0 {
                *height as f32 / *width as f32
            } else {
                0.66
            };
            let h = (w * ratio).clamp(120.0, 420.0);
            let inner: Element<'a, Message> = match app.photos.peek(&(msg.chat_id, msg.id)) {
                Some(handle) => image(handle.clone())
                    .width(w)
                    .height(h)
                    .content_fit(ContentFit::Cover)
                    .border_radius(m.bubble_radius - 3.0)
                    .into(),
                None => container(icon(Icon::Image, 40.0, p.text_muted))
                    .width(w)
                    .height(h)
                    .center(Length::Fixed(w))
                    .style(theme::pill(p.bg_hover, m.bubble_radius - 3.0))
                    .into(),
            };
            mouse_area(inner)
                .on_press(Message::Ui(Ui::OpenMedia(msg.chat_id, msg.id)))
                .into()
        }
        Media::File { name, size, .. } => {
            let circle = container(icon(Icon::File, 22.0, Color::WHITE))
                .width(46)
                .height(46)
                .center(46)
                .style(theme::pill(if out { p.time_out } else { p.accent }, 23.0));
            let circle = button(circle)
                .padding(0)
                .on_press_maybe(
                    (msg.id > 0).then_some(Message::Ui(Ui::OpenFile(msg.chat_id, msg.id))),
                )
                .style(theme::link_button(p));
            row![
                circle,
                column![
                    emoji_title(format::ellipsize(name, 32), m.font_m, p.text_primary),
                    text(format::file_size(*size)).size(m.font_s).color(if out {
                        p.time_out
                    } else {
                        p.text_secondary
                    }),
                ]
                .spacing(3),
            ]
            .spacing(12)
            .align_y(Alignment::Center)
            .padding([2, 0])
            .into()
        }
        Media::Sticker { emoji } => text(emoji.clone()).size(72).font(EMOJI_FONT).into(),
        Media::Other { label } => text(label.clone())
            .size(m.font_m)
            .color(if out { p.time_out } else { p.link })
            .into(),
    }
}

fn message_menu<'a>(app: &'a App, msg: &'a ChatMessage) -> Element<'a, Message> {
    let t = &app.theme;
    let (p, m) = (t.palette, t.metrics);
    let mut reactions = row![].spacing(2);
    for e in QUICK_REACTIONS {
        reactions = reactions.push(
            button(text(e).size(20).font(EMOJI_FONT))
                .padding([4, 6])
                .on_press(Message::Ui(Ui::React(msg.id, e.to_owned())))
                .style(theme::icon_button(p, true)),
        );
    }
    let item = |i: Icon, label: &'static str, msg: Message, danger: bool| {
        button(
            row![
                icon(i, 17.0, if danger { p.danger } else { p.text_secondary }),
                text(label).size(m.font_m),
            ]
            .spacing(14)
            .align_y(Alignment::Center),
        )
        .width(Length::Fill)
        .padding([8, 12])
        .on_press(msg)
        .style(theme::menu_item(p, danger))
    };
    let mut items = column![
        container(reactions).padding([4, 4]),
        item(Icon::Reply, "Reply", Message::Ui(Ui::Reply(msg.id)), false),
    ]
    .spacing(0);
    if msg.outgoing && msg.media.is_none() {
        items = items.push(item(
            Icon::Pencil,
            "Edit",
            Message::Ui(Ui::Edit(msg.id)),
            false,
        ));
    }
    if !msg.text.is_empty() {
        items = items.push(item(
            Icon::Copy,
            "Copy Text",
            Message::Ui(Ui::Copy(msg.id)),
            false,
        ));
    }
    if matches!(msg.media, Some(Media::Photo { .. })) {
        items = items.push(item(
            Icon::Copy,
            "Copy Image",
            Message::Ui(Ui::CopyImage(msg.chat_id, msg.id)),
            false,
        ));
    }
    if matches!(msg.media, Some(Media::File { .. } | Media::Photo { .. })) {
        items = items.push(item(
            Icon::Download,
            "Save As…",
            Message::Ui(Ui::SaveMedia(msg.chat_id, msg.id)),
            false,
        ));
    }
    items = items.push(item(
        Icon::Trash,
        "Delete",
        Message::Ui(Ui::Delete(msg.id)),
        true,
    ));
    let card = container(items)
        .width(300)
        .padding(4)
        .style(theme::elevated(p));
    let r = if msg.outgoing {
        row![
            Space::new().width(Length::Fill),
            card,
            Space::new().width(9)
        ]
    } else {
        row![
            Space::new().width(9),
            card,
            Space::new().width(Length::Fill)
        ]
    };
    container(r).padding([4, 0]).into()
}

fn emoji_panel(app: &App) -> Element<'_, Message> {
    let p = app.theme.palette;
    let mut grid = Column::new().spacing(2);
    for chunk in EMOJI_PANEL.chunks(8) {
        let mut r = row![].spacing(2);
        for e in chunk {
            r = r.push(
                button(text(*e).size(22).font(EMOJI_FONT))
                    .padding([4, 6])
                    .on_press(Message::Ui(Ui::InsertEmoji(e)))
                    .style(theme::icon_button(p, false)),
            );
        }
        grid = grid.push(r);
    }
    container(opaque(container(grid).padding(8).style(theme::elevated(p))))
        .align_right(Length::Fill)
        .align_bottom(Length::Fill)
        .padding(10)
        .into()
}

fn compose_bar(app: &App) -> Element<'_, Message> {
    let t = &app.theme;
    let (p, m) = (t.palette, t.metrics);
    let conv = app.conversation.as_ref();
    let (ic, title, preview) = match app.compose_mode {
        ComposeMode::Reply(id) => {
            let target = conv.and_then(|c| c.get(id));
            let who = match target {
                Some(r) if r.outgoing => "yourself".to_owned(),
                Some(r) => r
                    .sender_name
                    .clone()
                    .or_else(|| {
                        app.selected
                            .and_then(|c| app.chats.get(c))
                            .map(|c| c.title.clone())
                    })
                    .unwrap_or_default(),
                None => String::new(),
            };
            (
                Icon::Reply,
                format!("Reply to {who}"),
                target.map(|r| r.preview()).unwrap_or_default(),
            )
        }
        ComposeMode::Edit(id) => (
            Icon::Pencil,
            "Edit message".to_owned(),
            conv.and_then(|c| c.get(id))
                .map(|r| r.preview())
                .unwrap_or_default(),
        ),
        ComposeMode::Normal => (Icon::Reply, String::new(), String::new()),
    };
    container(
        row![
            icon(ic, 22.0, p.link),
            container(Space::new().width(2).height(34)).style(theme::pill(p.link, 1.0)),
            column![
                emoji_title(title.to_string(), m.font_s, p.link),
                emoji_text(format::ellipsize(&preview, 70), m.font_s, p.text_primary),
            ]
            .spacing(1)
            .width(Length::Fill),
            button(icon(Icon::X, 20.0, p.text_secondary))
                .padding(6)
                .on_press(Message::Ui(Ui::CancelCompose))
                .style(theme::icon_button(p, true)),
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    )
    .padding([6, 16])
    .width(Length::Fill)
    .style(theme::fill(p.bg_sidebar))
    .into()
}

fn pending_file_bar<'a>(app: &'a App, f: &'a crate::app::PendingFile) -> Element<'a, Message> {
    let t = &app.theme;
    let (p, m) = (t.palette, t.metrics);
    let kind = match f.kind {
        crate::net::UploadKind::Photo => "Send as photo",
        crate::net::UploadKind::Document => "Send as file",
    };
    container(
        row![
            icon(
                if f.kind == crate::net::UploadKind::Photo {
                    Icon::Image
                } else {
                    Icon::File
                },
                22.0,
                p.link
            ),
            column![
                text(format!("{kind} · {}", format::file_size(f.size)))
                    .size(m.font_s)
                    .font(FONT_SEMIBOLD)
                    .color(p.link),
                emoji_plain(format::ellipsize(&f.name, 70), m.font_s, p.text_primary),
            ]
            .spacing(1)
            .width(Length::Fill),
            button(text("Send").size(m.font_s).font(FONT_SEMIBOLD))
                .padding([6, 14])
                .on_press(Message::Ui(Ui::SendPendingFile))
                .style(theme::primary_button(p)),
            button(icon(Icon::X, 20.0, p.text_secondary))
                .padding(6)
                .on_press(Message::Ui(Ui::CancelPendingFile))
                .style(theme::icon_button(p, true)),
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    )
    .padding([6, 16])
    .width(Length::Fill)
    .style(theme::fill(p.bg_sidebar))
    .into()
}

fn composer(app: &App) -> Element<'_, Message> {
    let t = &app.theme;
    let (p, m) = (t.palette, t.metrics);
    let has_text = !app.composer.trim().is_empty();
    let ib = |i: Icon, color: Color, msg: Message| {
        button(icon(i, 24.0, color))
            .padding(10)
            .on_press(msg)
            .style(theme::icon_button(p, true))
    };
    container(
        row![
            ib(Icon::Paperclip, p.text_muted, Message::Ui(Ui::Attach)),
            text_input("Write a message…", &app.composer)
                .id(COMPOSER_ID)
                .on_input(|s| Message::Ui(Ui::ComposerChanged(s)))
                .on_paste(|s| Message::Ui(Ui::TextPasted(s)))
                .on_submit(Message::Ui(Ui::Send))
                .padding([10, 2])
                .size(m.font_m)
                .style(theme::input(p, p.bg_sidebar, 0.0, false)),
            ib(
                Icon::Smile,
                if app.emoji_open { p.link } else { p.text_muted },
                Message::Ui(Ui::ToggleEmoji)
            ),
            ib(
                Icon::Send,
                if has_text { p.link } else { p.text_muted },
                Message::Ui(Ui::Send)
            ),
        ]
        .spacing(2)
        .align_y(Alignment::Center),
    )
    .padding([2, 6])
    .width(Length::Fill)
    .height(m.composer_height)
    .align_y(Alignment::Center)
    .style(theme::fill(p.bg_sidebar))
    .into()
}

/// Full-window photo viewer overlay.
pub fn media_viewer(app: &App) -> Option<Element<'_, Message>> {
    let key = app.viewer?;
    let p = app.theme.palette;
    let content: Element<'_, Message> = match app.photos.peek(&key) {
        Some(h) => image(h.clone())
            .content_fit(ContentFit::Contain)
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        None => icon(Icon::Image, 64.0, p.text_muted),
    };
    let close = button(icon(Icon::X, 26.0, Color::WHITE))
        .padding(10)
        .on_press(Message::Ui(Ui::CloseMedia))
        .style(theme::icon_button(p, true));
    Some(
        mouse_area(
            container(column![
                container(close).align_right(Length::Fill).padding(10),
                container(content).padding([10, 80]).center(Length::Fill),
            ])
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::fill(Color::from_rgba(0.0, 0.0, 0.0, 0.88))),
        )
        .on_press(Message::Ui(Ui::CloseMedia))
        .into(),
    )
}
