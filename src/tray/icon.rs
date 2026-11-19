//! Tray icon pixmaps: the app's own SVG rendered to ARGB32 at 22/32/48 px,
//! with an optional unread badge for the "needs attention" state.
//!
//! `resvg` is already compiled in for iced's SVG widget, so this adds no new
//! code to the build.

use resvg::{tiny_skia, usvg};

/// Sizes panels ask for (the SNI host picks the closest).
pub const SIZES: [u32; 3] = [22, 32, 48];

/// A rendered icon in the layout StatusNotifierItem wants: straight
/// (non-premultiplied) ARGB32, one `A, R, G, B` byte quad per pixel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Argb {
    pub size: u32,
    pub data: Vec<u8>,
}

/// Render `svg` at `size` x `size`; `badge` adds the unread dot in the
/// bottom-right corner. `None` if the SVG is malformed.
pub fn render(svg: &str, size: u32, badge: bool) -> Option<Argb> {
    let tree = usvg::Tree::from_str(svg, &usvg::Options::default()).ok()?;
    let mut pixmap = tiny_skia::Pixmap::new(size, size)?;
    let src = tree.size();
    let transform = tiny_skia::Transform::from_scale(
        size as f32 / src.width().max(1.0),
        size as f32 / src.height().max(1.0),
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    if badge {
        draw_badge(&mut pixmap);
    }
    let mut data = Vec::with_capacity(pixmap.pixels().len() * 4);
    for px in pixmap.pixels() {
        let c = px.demultiply();
        data.extend_from_slice(&[c.alpha(), c.red(), c.green(), c.blue()]);
    }
    Some(Argb { size, data })
}

fn draw_badge(pixmap: &mut tiny_skia::Pixmap) {
    let s = pixmap.width() as f32;
    let r = s * 0.27;
    let (cx, cy) = (s - r - s * 0.02, s - r - s * 0.02);
    let mut fill = |radius: f32, rgba: (u8, u8, u8, u8)| {
        let mut paint = tiny_skia::Paint::default();
        paint.set_color_rgba8(rgba.0, rgba.1, rgba.2, rgba.3);
        paint.anti_alias = true;
        if let Some(path) = tiny_skia::PathBuilder::from_circle(cx, cy, radius) {
            pixmap.fill_path(
                &path,
                &paint,
                tiny_skia::FillRule::Winding,
                tiny_skia::Transform::identity(),
                None,
            );
        }
    };
    // A light ring keeps the dot readable on dark and light panels.
    fill(r + s * 0.05, (255, 255, 255, 255));
    fill(r, (0xec, 0x39, 0x42, 255));
}

#[cfg(test)]
mod tests {
    use super::*;

    const SVG: &str = include_str!("../../packaging/dev.ferro.ferrogram.svg");

    fn pixel(a: &Argb, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * a.size + x) * 4) as usize;
        [a.data[i], a.data[i + 1], a.data[i + 2], a.data[i + 3]]
    }

    #[test]
    fn renders_every_size_as_argb() {
        for size in SIZES {
            let icon = render(SVG, size, false).unwrap();
            assert_eq!(icon.size, size);
            assert_eq!(icon.data.len(), (size * size * 4) as usize);
            // The round icon leaves the corner transparent and the middle opaque.
            assert_eq!(pixel(&icon, 0, 0)[0], 0, "corner is transparent");
            let mid = pixel(&icon, size / 2, size / 2);
            assert_eq!(mid[0], 255, "centre is opaque");
        }
    }

    #[test]
    fn badge_is_a_red_dot_in_the_corner() {
        let plain = render(SVG, 48, false).unwrap();
        let badged = render(SVG, 48, true).unwrap();
        assert_ne!(plain, badged);
        let [a, r, g, b] = pixel(&badged, 47 - 12, 47 - 12);
        assert_eq!(a, 255);
        assert!(r > 200 && g < 100 && b < 100, "red dot, got {r},{g},{b}");
        // The rest of the icon is unchanged.
        assert_eq!(pixel(&plain, 24, 10), pixel(&badged, 24, 10));
    }

    #[test]
    fn malformed_svg_is_none() {
        assert!(render("<svg", 22, false).is_none());
        assert!(render(SVG, 0, false).is_none());
    }
}
