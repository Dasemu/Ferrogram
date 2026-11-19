//! System clipboard access for images and files (`arboard`).
//!
//! iced only reads and writes clipboard *text*. Telegram Desktop also pastes
//! copied files and screenshots into the composer ("send as photo / file"
//! preview) and copies a photo from the context menu, so this module covers
//! those. It talks to X11 and, through data-control, to Wayland compositors
//! (arboard tries Wayland first when `WAYLAND_DISPLAY` is set and falls back
//! to X11/XWayland, which is what KDE offers when it lacks data-control).
//!
//! All functions here block: call them from `spawn_blocking`. The pure helpers
//! (`encode_png`, `decode_photo`, `strip_pasted`, `temp_name`) are unit tested;
//! the arboard calls are thin wrappers that cannot run in CI (no display).

use std::path::{Path, PathBuf};

/// Hard caps for anything that comes from, or goes to, the clipboard.
pub const MAX_PIXELS: u64 = 40_000_000;
pub const MAX_PNG_BYTES: usize = 20 * 1024 * 1024;

/// What was on the clipboard when the user pressed Ctrl+V.
#[derive(Clone, PartialEq, Eq)]
pub enum Clip {
    /// Copied files (a file manager's "Copy"). Only the first can be sent.
    Files(Vec<PathBuf>),
    /// A copied image (screenshot, "Copy image" in a browser), as PNG.
    Image(Vec<u8>),
    /// Text or nothing usable: the focused text field handles text itself.
    Nothing,
}

impl std::fmt::Debug for Clip {
    // File names and pixels are private: print sizes only.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Files(v) => write!(f, "Clip::Files(n={})", v.len()),
            Self::Image(b) => write!(f, "Clip::Image(bytes={})", b.len()),
            Self::Nothing => f.write_str("Clip::Nothing"),
        }
    }
}

/// RGBA pixels as PNG; `None` for empty/oversized images (a decompression
/// bomb on the clipboard) or a result above `max_bytes`.
pub fn encode_png(width: usize, height: usize, rgba: &[u8], max_bytes: usize) -> Option<Vec<u8>> {
    let (w, h) = (u32::try_from(width).ok()?, u32::try_from(height).ok()?);
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_PIXELS {
        return None;
    }
    if rgba.len() != width.checked_mul(height)?.checked_mul(4)? {
        return None;
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().ok()?;
        writer.write_image_data(rgba).ok()?;
    }
    (out.len() <= max_bytes).then_some(out)
}

/// A decoded photo ready for the clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pixels {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

/// Decode downloaded photo bytes (JPEG/PNG) after the usual header checks.
pub fn decode_photo(bytes: &[u8]) -> Option<Pixels> {
    if !crate::app::state::photo_is_acceptable(bytes) {
        return None;
    }
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    (w > 0 && h > 0 && u64::from(w) * u64::from(h) <= MAX_PIXELS).then(|| Pixels {
        width: w as usize,
        height: h as usize,
        rgba: img.into_raw(),
    })
}

/// File name for a pasted image: unique, no user data in it.
pub fn temp_name(n: u64) -> String {
    format!("paste-{n:016x}.png")
}

/// Remove `pasted` (what the text field just inserted for Ctrl+V) from the
/// end of the composer. File managers put the file *paths* on the clipboard
/// as text too, and the text field has already pasted them by the time we
/// know a file was copied: the file wins, like in the official client.
pub fn strip_pasted(composer: &str, pasted: &str) -> String {
    if pasted.is_empty() {
        return composer.to_owned();
    }
    match composer.rfind(pasted) {
        Some(i) => format!("{}{}", &composer[..i], &composer[i + pasted.len()..]),
        None => composer.to_owned(),
    }
}

/// Write a pasted image to a private (0600) new file in `dir`.
pub fn save_pasted(dir: &Path, bytes: &[u8]) -> std::io::Result<PathBuf> {
    use std::io::Write;
    crate::config::ensure_private_dir(dir)?;
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64)
        ^ u64::from(std::process::id()).rotate_left(40);
    let path = dir.join(temp_name(n));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(&path)?.write_all(bytes)?;
    Ok(path)
}

/// One long-lived handle. On X11 the process that copied something must stay
/// alive to serve it, and dropping the last `arboard::Clipboard` ends that
/// service, so "Copy image" followed by a paste elsewhere would get nothing.
fn with_clipboard<T>(f: impl FnOnce(&mut arboard::Clipboard) -> T) -> Option<T> {
    static CB: std::sync::Mutex<Option<arboard::Clipboard>> = std::sync::Mutex::new(None);
    let mut guard = CB.lock().ok()?;
    if guard.is_none() {
        *guard = arboard::Clipboard::new().ok();
    }
    guard.as_mut().map(f)
}

/// Read files or an image from the system clipboard. Blocking.
pub fn read_blocking() -> Clip {
    with_clipboard(|cb| {
        if let Ok(files) = cb.get().file_list()
            && !files.is_empty()
        {
            return Clip::Files(files);
        }
        if let Ok(img) = cb.get().image()
            && let Some(png) = encode_png(img.width, img.height, &img.bytes, MAX_PNG_BYTES)
        {
            return Clip::Image(png);
        }
        Clip::Nothing
    })
    .unwrap_or(Clip::Nothing)
}

/// Put an image on the system clipboard. Blocking.
pub fn write_image_blocking(px: Pixels) -> Result<(), String> {
    with_clipboard(|cb| {
        cb.set_image(arboard::ImageData {
            width: px.width,
            height: px.height,
            bytes: std::borrow::Cow::Owned(px.rgba),
        })
        .map_err(|e| e.to_string())
    })
    .unwrap_or_else(|| Err("no clipboard".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_roundtrip_and_caps() {
        let px = vec![200u8; 4 * 4 * 4];
        let png = encode_png(4, 4, &px, 10_000).unwrap();
        assert_eq!(&png[1..4], b"PNG");
        assert!(encode_png(4, 4, &px, 10).is_none(), "byte cap");
        assert!(encode_png(0, 4, &px, 10_000).is_none());
        assert!(
            encode_png(4, 4, &px[..10], 10_000).is_none(),
            "short buffer"
        );
        assert!(
            encode_png(100_000, 100_000, &px, 10_000).is_none(),
            "pixel cap"
        );
        let back = decode_photo(&png).unwrap();
        assert_eq!((back.width, back.height), (4, 4));
        assert_eq!(back.rgba.len(), 64);
    }

    #[test]
    fn decode_refuses_garbage_and_bombs() {
        assert!(decode_photo(b"not an image").is_none());
        let mut png = crate::mock::mock_photo_png(1, 8, 8);
        png[16..20].copy_from_slice(&100_000u32.to_be_bytes());
        png[20..24].copy_from_slice(&100_000u32.to_be_bytes());
        assert!(decode_photo(&png).is_none());
    }

    #[test]
    fn strips_the_text_the_field_pasted() {
        assert_eq!(strip_pasted("hi file:///a/b.png", "file:///a/b.png"), "hi ");
        assert_eq!(strip_pasted("hi", "zzz"), "hi");
        assert_eq!(strip_pasted("abab", "ab"), "ab", "only the last occurrence");
        assert_eq!(strip_pasted("x", ""), "x");
    }

    #[test]
    fn pasted_images_are_private_and_unique() {
        let dir = tempfile::tempdir().unwrap();
        let a = save_pasted(dir.path(), b"one").unwrap();
        let b = save_pasted(dir.path(), b"two").unwrap();
        assert_ne!(a, b);
        assert_eq!(std::fs::read(&a).unwrap(), b"one");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&a).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(
            a.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("paste-")
        );
    }

    #[test]
    fn debug_never_prints_paths() {
        let c = Clip::Files(vec![PathBuf::from("/home/me/secret-plans.pdf")]);
        assert!(!format!("{c:?}").contains("secret"));
    }
}
