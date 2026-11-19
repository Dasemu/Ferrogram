//! Small shared view helpers (avatars, icons, badges).

use iced::widget::{center, container, image, mouse_area, opaque, svg, text};
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

/// A modal layer: a dimmed backdrop (click = `on_dismiss`) with `card` in the
/// middle. The whole layer is `opaque`: iced's `stack` only hides the cursor
/// from the layers below when the top layer reports a mouse interaction, and a
/// plain `mouse_area` reports none outside its content. Without it every row
/// under the dim backdrop kept its hover highlight and pointer shape, so the
/// chat list flashed under the modal as the mouse moved over it.
pub fn modal<'a>(
    card: impl Into<Element<'a, Message>>,
    on_dismiss: Message,
) -> Element<'a, Message> {
    opaque(mouse_area(center(opaque(card)).style(theme::fill(theme::SCRIM))).on_press(on_dismiss))
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
                .content_fit(iced::ContentFit::Cover),
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

/// Colour emoji font family. Noto Color Emoji is bundled in the binary
/// (`theme::NOTO_COLOR_EMOJI`, loaded at startup) and a system copy of the same
/// family, when installed, is interchangeable with it.
pub const EMOJI_FONT: iced::Font = iced::Font::with_name("Noto Color Emoji");

/// Directories where a system copy of the emoji font may live (fontdb scans
/// the same places on Linux).
fn font_dirs() -> Vec<std::path::PathBuf> {
    use std::path::PathBuf;
    let mut v = vec![
        PathBuf::from("/usr/share/fonts"),
        PathBuf::from("/usr/local/share/fonts"),
    ];
    if let Some(h) = std::env::var_os("HOME") {
        v.push(PathBuf::from(&h).join(".fonts"));
        v.push(PathBuf::from(&h).join(".local/share/fonts"));
    }
    if let Some(x) = std::env::var_os("XDG_DATA_HOME") {
        v.push(PathBuf::from(x).join("fonts"));
    }
    if let Some(x) = std::env::var_os("XDG_DATA_DIRS") {
        v.extend(std::env::split_paths(&x).map(|d| d.join("fonts")));
    }
    v.sort();
    v.dedup();
    v
}

/// Whether `dirs` (searched a few levels deep) hold a Noto Color Emoji file.
pub fn has_font_file(dirs: &[std::path::PathBuf]) -> bool {
    fn walk(dir: &std::path::Path, depth: u8) -> bool {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return false;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_ascii_lowercase();
            let Ok(ty) = e.file_type() else { continue };
            if ty.is_dir() {
                if depth > 0 && walk(&e.path(), depth - 1) {
                    return true;
                }
            } else if name.starts_with("notocoloremoji")
                && (name.ends_with(".ttf") || name.ends_with(".otf"))
            {
                return true;
            }
        }
        false
    }
    dirs.iter().any(|d| walk(d, 5))
}

/// The bundled font is only loaded when the system has no Noto Color Emoji:
/// both carry the same family name, and loading ~10 MB of bitmaps for nothing
/// costs ~22 MB RSS. `FERROGRAM_BUNDLED_EMOJI=1` forces the bundled copy.
pub fn needs_bundled_emoji_font() -> bool {
    if std::env::var("FERROGRAM_BUNDLED_EMOJI").is_ok_and(|v| v == "1") {
        return true;
    }
    !has_font_file(&font_dirs())
}

const VS16: char = '\u{FE0F}';
const KEYCAP: char = '\u{20E3}';

/// Single code points that are drawn as emoji even without a variation
/// selector: the pictograph planes, the BMP symbol blocks and the stray
/// emoji-presentation characters outside them (Unicode `Emoji_Presentation`
/// plus the classic Telegram set). Text-default marks such as (c), (R) and
/// TM only become emoji with U+FE0F, which `is_emoji_cluster` checks.
pub fn is_emoji(c: char) -> bool {
    matches!(c as u32,
        0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2B00..=0x2BFF | 0x2300..=0x23FF
        | 0x25FB..=0x25FE | 0x203C | 0x2049 | 0x3030 | 0x303D | 0x3297 | 0x3299
        | 0xFE0F | 0x20E3)
}

/// Whether a grapheme cluster is an emoji sequence: a keycap, a flag, a
/// variation-selector-16 presentation sequence, a skin-toned or ZWJ sequence,
/// or a pictograph. Plain digits, `#`, `*` and bare (c)/(R)/TM stay text.
pub fn is_emoji_cluster(g: &str) -> bool {
    g.chars().any(|c| {
        c == VS16
            || c == KEYCAP
            || (c != '\u{200D}' && is_emoji(c))
            || matches!(c as u32, 0x1F3FB..=0x1F3FF)
    })
}

/// Split into (is_emoji, run) segments, by grapheme cluster so that a keycap,
/// flag or ZWJ sequence is never cut apart.
pub fn emoji_runs(s: &str) -> Vec<(bool, String)> {
    use unicode_segmentation::UnicodeSegmentation;
    let mut out: Vec<(bool, String)> = Vec::new();
    for g in s.graphemes(true) {
        let e = is_emoji_cluster(g);
        match out.last_mut() {
            Some((k, run)) if *k == e => run.push_str(g),
            _ => out.push((e, g.to_string())),
        }
    }
    out
}

/// True when the text has at least one emoji cluster.
pub fn has_emoji(s: &str) -> bool {
    use unicode_segmentation::UnicodeSegmentation;
    s.graphemes(true).any(is_emoji_cluster)
}

/// Text that renders emoji with the colour emoji font.
pub fn emoji_text<'a>(s: String, size: f32, color: Color) -> Element<'a, Message> {
    emoji_text_wrapped(s, size, color, true)
}

/// Single-line variant (chat list previews).
pub fn emoji_line<'a>(s: String, size: f32, color: Color) -> Element<'a, Message> {
    emoji_text_wrapped(s, size, color, false)
}

/// Semibold single-line variant for names and titles.
pub fn emoji_title<'a>(s: String, size: f32, color: Color) -> Element<'a, Message> {
    emoji_rich(s, size, color, false, Some(FONT_SEMIBOLD))
}

/// Regular single-line variant for names and file names.
pub fn emoji_plain<'a>(s: String, size: f32, color: Color) -> Element<'a, Message> {
    emoji_rich(s, size, color, false, None)
}

fn emoji_text_wrapped<'a>(s: String, size: f32, color: Color, wrap: bool) -> Element<'a, Message> {
    emoji_rich(s, size, color, wrap, None)
}

fn emoji_rich<'a>(
    s: String,
    size: f32,
    color: Color,
    wrap: bool,
    font: Option<iced::Font>,
) -> Element<'a, Message> {
    use iced::widget::text::Wrapping;
    let wrapping = if wrap { Wrapping::Word } else { Wrapping::None };
    if !has_emoji(&s) {
        let t = text(s).size(size).color(color).wrapping(wrapping);
        return match font {
            Some(f) => t.font(f).into(),
            None => t.into(),
        };
    }
    let spans: Vec<iced::widget::text::Span<'a, (), iced::Font>> = emoji_runs(&s)
        .into_iter()
        .map(|(e, run)| {
            let sp = iced::widget::span(run).color(color);
            if e { sp.font(EMOJI_FONT) } else { sp }
        })
        .collect();
    let r = iced::widget::rich_text::<(), Message, _, _>(spans)
        .size(size)
        .wrapping(wrapping);
    match font {
        Some(f) => r.font(f).into(),
        None => r.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_system_font_file() {
        let d = tempfile::tempdir().unwrap();
        let dirs = vec![d.path().to_path_buf()];
        assert!(!has_font_file(&dirs));
        let sub = d.path().join("truetype/noto");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("NotoSans-Regular.ttf"), b"x").unwrap();
        assert!(!has_font_file(&dirs));
        std::fs::write(sub.join("NotoColorEmoji.ttf"), b"x").unwrap();
        assert!(has_font_file(&dirs));
        assert!(!has_font_file(&[d.path().join("missing")]));
    }

    fn kinds(s: &str) -> Vec<(bool, String)> {
        emoji_runs(s)
    }

    #[test]
    fn emoji_runs_split() {
        let r = emoji_runs("hi \u{1F44D}\u{1F3FD} ok \u{2764}\u{FE0F}");
        assert_eq!(r.len(), 4);
        assert!(r[1].0 && !r[2].0 && r[3].0);
        assert_eq!(r[3].1, "\u{2764}\u{FE0F}");
        assert!(!is_emoji('a'));
    }

    #[test]
    fn keycaps_stay_whole() {
        for k in [
            "1\u{FE0F}\u{20E3}",
            "#\u{FE0F}\u{20E3}",
            "*\u{FE0F}\u{20E3}",
            "0\u{20E3}",
        ] {
            assert_eq!(kinds(k), vec![(true, k.to_string())], "{k:?}");
        }
        assert_eq!(kinds("a1#"), vec![(false, "a1#".to_string())]);
        assert_eq!(
            kinds("call 112 #1"),
            vec![(false, "call 112 #1".to_string())]
        );
        assert_eq!(
            kinds("x 1\u{FE0F}\u{20E3} y"),
            vec![
                (false, "x ".into()),
                (true, "1\u{FE0F}\u{20E3}".into()),
                (false, " y".into())
            ]
        );
    }

    #[test]
    fn flags() {
        let es = "\u{1F1EA}\u{1F1F8}";
        let fr = "\u{1F1EB}\u{1F1F7}";
        assert_eq!(
            kinds(&format!("{es}{fr}")),
            vec![(true, format!("{es}{fr}"))]
        );
        // England: black flag + tag sequence.
        let eng = "\u{1F3F4}\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}";
        assert_eq!(kinds(eng), vec![(true, eng.to_string())]);
    }

    #[test]
    fn zwj_families_and_professions() {
        let fam = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
        assert_eq!(kinds(fam), vec![(true, fam.to_string())]);
        let heart = "\u{1F3F3}\u{FE0F}\u{200D}\u{1F308}";
        assert_eq!(kinds(heart), vec![(true, heart.to_string())]);
        // A ZWJ sequence whose tail is outside the pictograph blocks.
        let med = "\u{1F9D1}\u{1F3FD}\u{200D}\u{2695}\u{FE0F}";
        assert_eq!(kinds(med), vec![(true, med.to_string())]);
        let walk = "\u{1F6B6}\u{200D}\u{2640}\u{FE0F}";
        assert_eq!(kinds(walk), vec![(true, walk.to_string())]);
    }

    #[test]
    fn skin_tones() {
        let t = "\u{1F44B}\u{1F3FF}";
        assert_eq!(kinds(t), vec![(true, t.to_string())]);
        assert_eq!(kinds("hi\u{1F44B}\u{1F3FB}!").len(), 3);
    }

    #[test]
    fn symbols_with_and_without_vs16() {
        for c in ['\u{A9}', '\u{AE}', '\u{2122}', '\u{2139}', '\u{2194}'] {
            let bare = c.to_string();
            let vs = format!("{c}\u{FE0F}");
            assert_eq!(kinds(&bare), vec![(false, bare.clone())], "bare {c:?}");
            assert_eq!(kinds(&vs), vec![(true, vs.clone())], "vs16 {c:?}");
        }
        for c in [
            '\u{203C}', '\u{2049}', '\u{3030}', '\u{303D}', '\u{3297}', '\u{3299}',
        ] {
            assert!(has_emoji(&c.to_string()), "{c:?}");
            assert!(has_emoji(&format!("{c}\u{FE0F}")), "{c:?}");
        }
    }

    #[test]
    fn mixed_text() {
        let r = kinds(
            "caf\u{E9} \u{2615} \u{1F600}\u{1F600} \u{43F}\u{440}\u{438}\u{432}\u{435}\u{442} \u{1F1EA}\u{1F1F8}",
        );
        let emoji: Vec<&str> = r
            .iter()
            .filter(|(e, _)| *e)
            .map(|(_, s)| s.as_str())
            .collect();
        assert_eq!(
            emoji,
            vec!["\u{2615}", "\u{1F600}\u{1F600}", "\u{1F1EA}\u{1F1F8}"]
        );
        let text: String = r
            .iter()
            .filter(|(e, _)| !*e)
            .map(|(_, s)| s.as_str())
            .collect();
        assert_eq!(
            text,
            "caf\u{E9}   \u{43F}\u{440}\u{438}\u{432}\u{435}\u{442} "
        );
        assert!(kinds("").is_empty());
        assert!(!has_emoji("plain 123 #"));
    }
}
