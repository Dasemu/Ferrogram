//! Small shared view helpers (avatars, icons, badges).

use iced::widget::{container, svg, text};
use iced::{Alignment, Color, Element, Length};

use super::icons::Icon;
use super::theme::{self, FONT_SEMIBOLD, FerroTheme};
use crate::app::Message;
use crate::model::{ChatKind, avatar_color_index, initials};

pub fn icon<'a>(i: Icon, size: f32, color: Color) -> Element<'a, Message> {
    svg(i.handle())
        .width(size)
        .height(size)
        .style(theme::tint(color))
        .into()
}

pub fn avatar<'a>(id: i64, title: &str, kind: Option<ChatKind>, size: f32) -> Element<'a, Message> {
    let inner: Element<'a, Message> = if kind == Some(ChatKind::SavedMessages) {
        icon(Icon::Bookmark, size * 0.45, Color::WHITE)
    } else {
        text(initials(title))
            .size(size * 0.38)
            .font(FONT_SEMIBOLD)
            .color(Color::WHITE)
            .into()
    };
    let idx = if kind == Some(ChatKind::SavedMessages) {
        5
    } else {
        avatar_color_index(id)
    };
    container(inner)
        .width(size)
        .height(size)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .style(theme::avatar(idx, size))
        .into()
}

pub fn badge<'a>(t: &FerroTheme, label: String, muted: bool) -> Element<'a, Message> {
    let p = t.palette;
    container(
        text(label)
            .size(t.metrics.font_xs)
            .font(FONT_SEMIBOLD)
            .color(Color::WHITE),
    )
    .padding([1, 7])
    .height(20)
    .align_y(Alignment::Center)
    .align_x(Alignment::Center)
    .width(Length::Shrink)
    .style(theme::pill(
        if muted { p.badge_muted } else { p.badge },
        10.0,
    ))
    .into()
}
