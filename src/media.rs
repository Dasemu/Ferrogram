//! Safe handling of received files: file-name sanitising, the save-only
//! (executable) rule and the cache path used before the system handler
//! opens a file. Pure logic, unit-tested; the actual `open` call lives in
//! [`crate::desktop`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::model::{ChatId, MessageId};

/// Extensions that are never handed to the system handler: executables,
/// installers, scripts and launchers. Such files can only be saved through
/// the "Save as…" dialog.
const SAVE_ONLY: &[&str] = &[
    // Windows
    "exe",
    "com",
    "bat",
    "cmd",
    "msi",
    "msp",
    "scr",
    "pif",
    "cpl",
    "dll",
    "sys",
    "vbs",
    "vbe",
    "js",
    "jse",
    "wsf",
    "wsh",
    "ws",
    "wsc",
    "ps1",
    "ps1xml",
    "ps2",
    "psc1",
    "psm1",
    "lnk",
    "url",
    "inf",
    "reg",
    "hta",
    "chm",
    "application",
    "gadget",
    "msc",
    // Unix shells and launchers
    "sh",
    "bash",
    "zsh",
    "csh",
    "ksh",
    "fish",
    "run",
    "bin",
    "elf",
    "out",
    "appimage",
    "desktop",
    "service",
    "flatpakref",
    "flatpak",
    "snap",
    // macOS
    "app",
    "command",
    "pkg",
    "dmg",
    "workflow",
    "action",
    "scpt",
    "scptd",
    "terminal",
    // Packages and other scripting
    "deb",
    "rpm",
    "apk",
    "ipa",
    "jar",
    "py",
    "pyw",
    "pl",
    "rb",
    "php",
];

/// Largest file we will fetch just to open it (the "Save as…" path has the
/// protocol maximum instead).
pub const MAX_OPEN_BYTES: u64 = 512 * 1024 * 1024;

/// The extension as the operating system would see it: lower-case, after the
/// last dot, ignoring trailing dots and spaces (Windows strips them).
fn extension(name: &str) -> Option<String> {
    let trimmed = name.trim_end_matches(['.', ' ']);
    let (_, ext) = trimmed.rsplit_once('.')?;
    (!ext.is_empty()).then(|| ext.to_lowercase())
}

/// True when the file must be saved, never opened.
pub fn is_save_only(name: &str) -> bool {
    extension(name).is_some_and(|e| SAVE_ONLY.contains(&e.as_str()))
}

/// A file name that is safe to create inside a directory we control: no
/// path separators, no control or bidi-override characters, no leading
/// dots, bounded length (keeping the extension).
pub fn safe_file_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = base
        .chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(*c as u32, 0x202A..=0x202E | 0x2066..=0x2069 | 0x200E | 0x200F)
                && !matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
        .collect();
    let cleaned = cleaned.trim_matches(|c: char| c == '.' || c.is_whitespace());
    if cleaned.is_empty() {
        return "file".into();
    }
    const MAX: usize = 100;
    if cleaned.chars().count() <= MAX {
        return cleaned.to_owned();
    }
    match cleaned.rsplit_once('.') {
        Some((stem, ext)) if ext.chars().count() <= 12 => {
            let keep = MAX - ext.chars().count() - 1;
            format!("{}.{ext}", stem.chars().take(keep).collect::<String>())
        }
        _ => cleaned.chars().take(MAX).collect(),
    }
}

/// `<dir>/<chat>_<message>/<safe name>`: a per-message directory keeps two
/// files with the same name apart without renaming them.
pub fn open_path(dir: &Path, chat: ChatId, id: MessageId, name: &str) -> PathBuf {
    dir.join(format!("{chat}_{id}")).join(safe_file_name(name))
}

/// A decoded profile photo, already cut to a circle (alpha mask), so every
/// renderer draws it round. Pixels are shared, not copied, between messages.
#[derive(Clone)]
pub struct AvatarImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<Vec<u8>>,
}

impl std::fmt::Debug for AvatarImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AvatarImage({}x{})", self.width, self.height)
    }
}

impl AvatarImage {
    /// Decode a JPEG/PNG avatar. Dimensions are validated from the header
    /// first (see [`crate::app::state::avatar_is_acceptable`]), so a hostile
    /// file cannot make the decoder allocate more than a few megabytes.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if !crate::app::state::avatar_is_acceptable(bytes) {
            return None;
        }
        let mut img = image::load_from_memory(bytes).ok()?.to_rgba8();
        let (w, h) = img.dimensions();
        if w == 0 || h == 0 {
            return None;
        }
        let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
        let r = cx.min(cy);
        for (x, y, px) in img.enumerate_pixels_mut() {
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            // One pixel of anti-aliasing on the edge of the circle.
            let cover = (r - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0);
            px.0[3] = (f32::from(px.0[3]) * cover) as u8;
        }
        Some(Self {
            width: w,
            height: h,
            rgba: Arc::new(img.into_raw()),
        })
    }
}

/// A shared-media preview, decoded and cropped to a fixed square off the UI
/// thread, so the grid only ever draws small, equally sized RGBA tiles.
#[derive(Clone)]
pub struct ThumbImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for ThumbImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ThumbImage({}x{})", self.width, self.height)
    }
}

impl ThumbImage {
    /// Decoded size in memory (what the cache budget counts).
    pub fn cost(&self) -> usize {
        self.rgba.len().max(1)
    }

    /// Decode, then scale and centre-crop to `side` x `side` (the grid draws
    /// cells with `ContentFit::Cover`, so this is what it showed before,
    /// minus the per-frame scaling). The header is validated first
    /// ([`crate::app::state::photo_is_acceptable`]), so a hostile preview
    /// cannot make the decoder allocate more than the pixel cap allows.
    pub fn decode(bytes: &[u8], side: u32) -> Option<Self> {
        if side == 0 || !crate::app::state::photo_is_acceptable(bytes) {
            return None;
        }
        let img = image::load_from_memory(bytes).ok()?;
        if img.width() == 0 || img.height() == 0 {
            return None;
        }
        let tile = img
            .resize_to_fill(side, side, image::imageops::FilterType::Triangle)
            .to_rgba8();
        Some(Self {
            width: tile.width(),
            height: tile.height(),
            rgba: tile.into_raw(),
        })
    }
}

/// Mark a downloaded file owner-read/write only: no execute bit, whatever
/// the umask says.
pub fn make_non_executable(path: &Path) {
    crate::config::restrict_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avatars_decode_to_a_circle() {
        let png = crate::mock::mock_avatar_png(3, 64).unwrap();
        let a = AvatarImage::decode(&png).unwrap();
        assert_eq!((a.width, a.height), (64, 64));
        assert_eq!(a.rgba.len(), 64 * 64 * 4);
        let alpha = |x: usize, y: usize| a.rgba[(y * 64 + x) * 4 + 3];
        assert_eq!(alpha(0, 0), 0, "corners are transparent");
        assert_eq!(alpha(63, 63), 0);
        assert_eq!(alpha(32, 32), 255, "centre is opaque");
        assert!(alpha(32, 1) > 0, "edge midpoints are inside the circle");
    }

    #[test]
    fn thumbnails_become_fixed_size_tiles_whatever_the_source_shape() {
        for (w, h) in [(320, 320), (90, 160), (400, 120), (32, 32)] {
            let png = crate::mock::mock_photo_png(1, w, h);
            let t = ThumbImage::decode(&png, 64).unwrap();
            assert_eq!((t.width, t.height), (64, 64), "{w}x{h}");
            assert_eq!(t.rgba.len(), 64 * 64 * 4);
            assert_eq!(t.cost(), 64 * 64 * 4);
        }
    }

    #[test]
    fn thumbnail_crop_is_centred() {
        // Left half red, right half blue: a wide source keeps both halves
        // visible in the cropped square, not just the left.
        let (w, h) = (40u32, 10u32);
        let mut px = Vec::new();
        for _ in 0..h {
            for x in 0..w {
                px.extend_from_slice(if x < w / 2 {
                    &[255, 0, 0, 255]
                } else {
                    &[0, 0, 255, 255]
                });
            }
        }
        let png = crate::mock::encode_png(w, h, &px).unwrap();
        let t = ThumbImage::decode(&png, 20).unwrap();
        let at = |x: usize, y: usize| &t.rgba[(y * 20 + x) * 4..(y * 20 + x) * 4 + 3];
        assert!(at(2, 10)[0] > 200 && at(2, 10)[2] < 60, "left is red");
        assert!(at(17, 10)[2] > 200 && at(17, 10)[0] < 60, "right is blue");
    }

    #[test]
    fn hostile_thumbnails_are_refused_not_decoded() {
        assert!(ThumbImage::decode(b"", 64).is_none());
        assert!(ThumbImage::decode(b"not an image at all", 64).is_none());
        assert!(ThumbImage::decode(&crate::mock::mock_photo_png(1, 8, 8), 0).is_none());
        let mut bomb = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bomb.extend_from_slice(&60_000u32.to_be_bytes());
        bomb.extend_from_slice(&60_000u32.to_be_bytes());
        bomb.extend_from_slice(&[8, 6, 0, 0, 0]);
        assert!(
            ThumbImage::decode(&bomb, 64).is_none(),
            "refused from the header"
        );
        let png = crate::mock::mock_photo_png(3, 64, 64);
        assert!(ThumbImage::decode(&png[..png.len() / 2], 64).is_none());
    }

    #[test]
    fn hostile_avatars_are_refused_not_decoded() {
        assert!(AvatarImage::decode(b"").is_none());
        assert!(AvatarImage::decode(b"not an image").is_none());
        // Valid PNG header claiming 40000 x 40000 pixels: refused from the header alone.
        let mut bomb = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bomb.extend_from_slice(&40_000u32.to_be_bytes());
        bomb.extend_from_slice(&40_000u32.to_be_bytes());
        bomb.extend_from_slice(&[8, 6, 0, 0, 0]);
        assert!(AvatarImage::decode(&bomb).is_none());
        // Truncated real image: decoder error, not a panic.
        let png = crate::mock::mock_avatar_png(3, 64).unwrap();
        assert!(AvatarImage::decode(&png[..png.len() / 2]).is_none());
    }

    #[test]
    fn executables_are_save_only() {
        for n in [
            "setup.exe",
            "SETUP.EXE",
            "run.sh",
            "x.AppImage",
            "photo.jpg.exe",
            "evil.exe ",
            "evil.exe...",
            "a.desktop",
            "m.scr",
            "t.PS1",
            "j.jar",
        ] {
            assert!(is_save_only(n), "{n}");
        }
    }

    #[test]
    fn documents_and_media_open_normally() {
        for n in [
            "trail-map.pdf",
            "photo.JPG",
            "movie.mp4",
            "archive.zip",
            "noext",
            "exe",
            ".exe.txt",
            "notes.txt",
            "trailing.dot.",
        ] {
            assert!(!is_save_only(n), "{n}");
        }
    }

    #[test]
    fn names_are_sanitised() {
        assert_eq!(safe_file_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_file_name("..\\..\\boot.ini"), "boot.ini");
        assert_eq!(safe_file_name("  .hidden "), "hidden");
        assert_eq!(safe_file_name("a\u{0}b\nc.txt"), "abc.txt");
        assert_eq!(safe_file_name(""), "file");
        assert_eq!(safe_file_name("..."), "file");
        // Right-to-left override cannot disguise an extension.
        assert_eq!(safe_file_name("fdp.\u{202E}exe.txt"), "fdp.exe.txt");
        assert_eq!(safe_file_name("a:b*c?.pdf"), "abc.pdf");
    }

    #[test]
    fn long_names_keep_their_extension() {
        let n = format!("{}.pdf", "x".repeat(300));
        let s = safe_file_name(&n);
        assert_eq!(s.chars().count(), 100);
        assert!(s.ends_with(".pdf"));
        assert_eq!(safe_file_name(&"y".repeat(300)).chars().count(), 100);
    }

    #[test]
    fn open_path_stays_inside_dir() {
        let p = open_path(Path::new("/c/open"), -5, 9, "../../x.pdf");
        assert_eq!(p, Path::new("/c/open/-5_9/x.pdf"));
        assert!(p.starts_with("/c/open"));
    }
}
