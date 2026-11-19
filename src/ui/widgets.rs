//! Small shared view helpers (avatars, icons, badges).

use iced::widget::{container, image, svg, text};
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
    avatar_photo(None, id, title, kind, size)
}

/// Avatar with the downloaded profile photo when there is one; the initials
/// on a gradient circle otherwise (no photo, still loading, or failed).
pub fn avatar_photo<'a>(
    photo: Option<&image::Handle>,
    id: i64,
    title: &str,
    kind: Option<ChatKind>,
    size: f32,
) -> Element<'a, Message> {
    if let Some(h) = photo {
        return container(
            image(h.clone())
                .width(size)
                .height(size)
                .content_fit(iced::ContentFit::Cover)
                .border_radius(size / 2.0),
        )
        .width(size)
        .height(size)
        .into();
    }
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

/// System colour emoji font (Noto Color Emoji on Linux, used if installed).
pub const EMOJI_FONT: iced::Font = iced::Font::with_name("Noto Color Emoji");

pub fn is_emoji(c: char) -> bool {
    matches!(c as u32,
        0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2B00..=0x2BFF | 0x2300..=0x23FF
        | 0xFE0F | 0x200D | 0x20E3 | 0xE0020..=0xE007F)
}

/// Split into (is_emoji, run) segments.
pub fn emoji_runs(s: &str) -> Vec<(bool, String)> {
    let mut out: Vec<(bool, String)> = Vec::new();
    for c in s.chars() {
        let e = is_emoji(c);
        match out.last_mut() {
            Some((k, run)) if *k == e => run.push(c),
            _ => out.push((e, c.to_string())),
        }
    }
    out
}

/// Text that renders emoji with the colour emoji font.
pub fn emoji_text<'a>(s: String, size: f32, color: Color) -> Element<'a, Message> {
    emoji_text_wrapped(s, size, color, true)
}

/// Single-line variant (chat list previews).
pub fn emoji_line<'a>(s: String, size: f32, color: Color) -> Element<'a, Message> {
    emoji_text_wrapped(s, size, color, false)
}

fn emoji_text_wrapped<'a>(s: String, size: f32, color: Color, wrap: bool) -> Element<'a, Message> {
    use iced::widget::text::Wrapping;
    let wrapping = if wrap { Wrapping::Word } else { Wrapping::None };
    if !s.chars().any(is_emoji) {
        return text(s).size(size).color(color).wrapping(wrapping).into();
    }
    let spans: Vec<iced::widget::text::Span<'a, (), iced::Font>> = emoji_runs(&s)
        .into_iter()
        .map(|(e, run)| {
            let sp = iced::widget::span(run).color(color);
            if e { sp.font(EMOJI_FONT) } else { sp }
        })
        .collect();
    iced::widget::rich_text::<(), Message, _, _>(spans)
        .size(size)
        .wrapping(wrapping)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emoji_runs_split() {
        let r = emoji_runs("hi 👍🏽 ok ❤️");
        assert_eq!(r.len(), 4);
        assert!(r[1].0 && !r[2].0 && r[3].0);
        assert_eq!(r[3].1, "❤️");
        assert!(!is_emoji('a'));
    }
}
