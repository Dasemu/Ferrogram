//! Needs a display (X11 or Wayland), so it is ignored in CI:
//! `xvfb-run -a cargo test --test clipboard_display -- --ignored`
#![allow(clippy::unwrap_used, clippy::expect_used)]

use ferrogram::clipboard::{self, Clip, Pixels};

#[test]
#[ignore = "needs a display server"]
fn image_written_to_the_clipboard_is_read_back_as_png() {
    let px = Pixels {
        width: 3,
        height: 2,
        rgba: (0..24).map(|i| (i * 10) as u8).collect(),
    };
    clipboard::write_image_blocking(px.clone()).unwrap();
    match clipboard::read_blocking() {
        Clip::Image(png) => {
            let back = clipboard::decode_photo(&png).unwrap();
            assert_eq!((back.width, back.height), (3, 2));
        }
        other => panic!("expected an image, got {other:?}"),
    }
}

#[test]
#[ignore = "needs a display server"]
fn file_list_on_the_clipboard_is_read_back() {
    std::fs::write("/tmp/ferrogram-a.txt", "x").unwrap();
    let mut cb = arboard::Clipboard::new().unwrap();
    cb.set().file_list(&["/tmp/ferrogram-a.txt"]).unwrap();
    match clipboard::read_blocking() {
        Clip::Files(f) => assert_eq!(f, vec![std::path::PathBuf::from("/tmp/ferrogram-a.txt")]),
        other => panic!("expected files, got {other:?}"),
    }
}
