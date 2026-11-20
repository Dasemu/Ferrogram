//! Shared-media scroll cost (round 4). The Media tab used to build, lay out
//! and draw every loaded item (up to 600) on each scroll event. These tests
//! lay out and rasterise the real info panel on the CPU renderer and compare
//! 60 against 600 items: the frame cost must not grow with the list.
//!
//! Numbers are printed (`cargo test --test media_scroll -- --nocapture`);
//! they come from the debug profile and only the ratio is asserted.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use ferrogram::app::{App, Done, Message, Ui};
use ferrogram::config::Config;
use ferrogram::mock::{self, ALICE, FakeOptions, FakeTelegram};
use ferrogram::model::{SharedBody, SharedItem, SharedKind};
use ferrogram::net::ApiHandle;
use futures::StreamExt;
use iced::advanced::clipboard;
use iced::advanced::mouse;
use iced::{Pixels, Point, Rectangle, Size};
use iced_runtime::Action;
use iced_runtime::user_interface::{Cache, UserInterface};

const WINDOW: Size = Size::new(1180.0, 760.0);

async fn run(app: &mut App, first: Message) {
    let mut queue = vec![first];
    let mut steps = 0;
    while let Some(m) = queue.pop() {
        steps += 1;
        assert!(steps < 5_000, "message loop does not settle");
        let task = app.update(m);
        let Some(mut stream) = iced_runtime::task::into_stream(task) else {
            continue;
        };
        while let Ok(Some(action)) =
            tokio::time::timeout(Duration::from_secs(5), stream.next()).await
        {
            if let Action::Output(m) = action {
                queue.push(m);
            }
        }
    }
}

/// An app with the info panel open on Alice and `n` photos in its Media tab,
/// every thumbnail delivered the way the network does it.
async fn app_with_media(n: usize) -> App {
    let mut cfg = Config::from_env(&["ferrogram".into(), "--mock".into()]);
    cfg.notifications = false;
    let mut app = App::boot(cfg).0;
    let fake = FakeTelegram::new(FakeOptions {
        logged_in: true,
        live_simulation: false,
        ..Default::default()
    });
    run(&mut app, Message::Done(Done::ApiReady(Ok(ApiHandle(fake))))).await;
    run(
        &mut app,
        Message::Done(Done::Restored(Ok(Some(mock::me())))),
    )
    .await;
    run(&mut app, Message::Ui(Ui::SelectChat(ALICE))).await;
    run(&mut app, Message::Ui(Ui::ToggleInfo)).await;
    // The benchmark measures drawing, not eviction: keep every thumbnail.
    app.thumbs = ferrogram::app::state::Lru::new(usize::MAX / 2);
    let panel = app.info.as_mut().unwrap();
    let tab = panel.tab_mut(SharedKind::Media);
    tab.items = (0..n)
        .map(|i| SharedItem {
            id: 100_000 - i as i32,
            date: 1_795_000_000 - i as i64,
            body: SharedBody::Photo {
                width: 320,
                height: 320,
            },
        })
        .collect();
    tab.loaded = true;
    tab.next = None;
    // Telegram's previews are ~320 px JPEGs: use eight distinct 320 px images.
    let pngs: Vec<Vec<u8>> = (0..8).map(|s| jpeg(s, 320)).collect();
    for i in 0..n {
        run(
            &mut app,
            Message::Done(Done::Thumb {
                key: (ALICE, 100_000 - i as i32),
                result: Ok(pngs[i % 8].clone()),
            }),
        )
        .await;
    }
    app
}

/// A `size` px JPEG preview, like the ones Telegram sends for shared media.
fn jpeg(seed: i64, size: u32) -> Vec<u8> {
    let png = mock::mock_photo_png(seed, size, size);
    let img = image::load_from_memory(&png).unwrap().to_rgb8();
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80)
        .encode_image(&img)
        .unwrap();
    out
}

struct Bench {
    renderer: iced::Renderer,
    pixels: Vec<u8>,
    mask: tiny_skia::Mask,
    cache: Cache,
}

/// The info panel's rectangle: tiny-skia only re-rasterises damaged areas,
/// and scrolling it damages exactly this.
fn panel_rect() -> Rectangle {
    Rectangle {
        x: WINDOW.width - 341.0,
        y: 0.0,
        width: 341.0,
        height: WINDOW.height,
    }
}

/// Times of one frame, in milliseconds.
#[derive(Debug, Clone, Copy, Default)]
struct Frame {
    view: f64,
    layout: f64,
    update: f64,
    draw: f64,
}

impl Frame {
    fn total(&self) -> f64 {
        self.view + self.layout + self.update + self.draw
    }
}

impl Bench {
    fn new() -> Self {
        Self {
            renderer: iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
                iced::Font::DEFAULT,
                Pixels(16.0),
            )),
            pixels: vec![0; (WINDOW.width * WINDOW.height) as usize * 4],
            mask: tiny_skia::Mask::new(WINDOW.width as u32, WINDOW.height as u32).unwrap(),
            cache: Cache::default(),
        }
    }

    /// One frame like the runtime does it: build the view, lay it out (with
    /// the widget state of the previous frame), feed it the events, draw and
    /// rasterise the damaged panel. The messages the widgets emitted
    /// (`on_scroll`) are returned for the caller to deliver.
    fn frame(&mut self, app: &App, events: &[iced::Event]) -> (Frame, Vec<Message>) {
        let t0 = Instant::now();
        let element = ferrogram::ui::view(app);
        let t1 = Instant::now();
        let cache = std::mem::take(&mut self.cache);
        let mut ui = UserInterface::build(element, WINDOW, cache, &mut self.renderer);
        let t2 = Instant::now();
        let at = Point::new(WINDOW.width - 170.0, 500.0);
        let mut messages: Vec<Message> = Vec::new();
        let cursor = mouse::Cursor::Available(at);
        let _ = ui.update(
            events,
            cursor,
            &mut self.renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        let t3 = Instant::now();
        ui.draw(
            &mut self.renderer,
            &iced::Theme::Dark,
            &iced::advanced::renderer::Style {
                text_color: iced::Color::WHITE,
            },
            cursor,
        );
        if let iced::Renderer::Secondary(r) = &mut self.renderer {
            let viewport = iced_tiny_skia::graphics::Viewport::with_physical_size(
                Size::new(WINDOW.width as u32, WINDOW.height as u32),
                1.0,
            );
            let mut pm = tiny_skia::PixmapMut::from_bytes(
                &mut self.pixels,
                WINDOW.width as u32,
                WINDOW.height as u32,
            )
            .unwrap();
            r.draw(
                &mut pm,
                &mut self.mask,
                &viewport,
                &[panel_rect()],
                iced::Color::BLACK,
            );
        }
        let t4 = Instant::now();
        self.cache = ui.into_cache();
        let ms = |a: Instant, b: Instant| (b - a).as_secs_f64() * 1e3;
        (
            Frame {
                view: ms(t0, t1),
                layout: ms(t1, t2),
                update: ms(t2, t3),
                draw: ms(t3, t4),
            },
            messages,
        )
    }

    /// Scroll the panel down by `step` px per frame for `frames` frames,
    /// delivering the scroll messages to the app between frames.
    async fn scroll(&mut self, app: &mut App, frames: usize, step: f32) -> Vec<Frame> {
        let at = Point::new(WINDOW.width - 170.0, 500.0);
        let mut out = Vec::new();
        for _ in 0..frames {
            let events = [
                iced::Event::Mouse(mouse::Event::CursorMoved { position: at }),
                iced::Event::Mouse(mouse::Event::WheelScrolled {
                    delta: mouse::ScrollDelta::Pixels { x: 0.0, y: -step },
                }),
            ];
            let (frame, messages) = self.frame(app, &events);
            out.push(frame);
            for m in messages {
                run(app, m).await;
            }
        }
        out
    }
}

fn mean_of(frames: &[Frame], f: fn(&Frame) -> f64) -> f64 {
    frames.iter().map(f).sum::<f64>() / frames.len() as f64
}

fn stats(frames: &[Frame]) -> (f64, f64, f64, f64) {
    let mut t: Vec<f64> = frames.iter().map(Frame::total).collect();
    t.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = t.iter().sum::<f64>() / t.len() as f64;
    (
        mean,
        t[t.len() * 95 / 100],
        *t.last().unwrap(),
        mean_of(frames, |f| f.view),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scrolling_cost_does_not_grow_with_the_number_of_items() {
    let mut bench = Bench::new();
    let mut table = Vec::new();
    for n in [60usize, 600] {
        let mut app = app_with_media(n).await;
        assert!(app.thumbs.len() >= n, "every thumbnail is cached");
        // Warm up: first layout, glyphs, then scroll back to the top.
        let _ = bench.scroll(&mut app, 2, 40.0).await;
        let _ = bench.scroll(&mut app, 2, -40.0).await;
        let frames = bench.scroll(&mut app, 24, 45.0).await;
        // The same frame again and again: what remains when no new row
        // comes into view (text, the visible tiles).
        let still = bench.scroll(&mut app, 4, 0.0).await;
        let (mean, p95, worst, view) = stats(&frames);
        let rows = app.info.as_ref().unwrap().window(SharedKind::Media);
        table.push((n, mean, p95, worst, view, rows.len()));
        eprintln!(
            "{n:4} items scrolling: view {:.2} layout {:.2} update {:.2} draw {:.2} | still frame total {:.2}",
            mean_of(&frames, |f| f.view),
            mean_of(&frames, |f| f.layout),
            mean_of(&frames, |f| f.update),
            mean_of(&frames, |f| f.draw),
            mean_of(&still, Frame::total),
        );
    }
    eprintln!(
        "items | mean frame ms | p95 ms | worst ms | view() ms | rows built  (debug build, panel damage only)"
    );
    for (n, mean, p95, worst, view, rows) in &table {
        eprintln!("{n:5} | {mean:13.2} | {p95:6.2} | {worst:8.2} | {view:9.2} | {rows}");
    }
    let (small, big) = (table[0].1, table[1].1);
    assert!(
        big < small * 2.0 + 3.0,
        "600 items cost {big:.1} ms per scroll frame against {small:.1} ms for 60"
    );
}

// ------------------------------------------------------------- behaviour

async fn plain_app_with_info() -> (App, std::sync::Arc<FakeTelegram>) {
    let mut cfg = Config::from_env(&["ferrogram".into(), "--mock".into()]);
    cfg.notifications = false;
    let mut app = App::boot(cfg).0;
    let fake = FakeTelegram::new(FakeOptions {
        logged_in: true,
        live_simulation: false,
        ..Default::default()
    });
    run(
        &mut app,
        Message::Done(Done::ApiReady(Ok(ApiHandle(fake.clone())))),
    )
    .await;
    run(
        &mut app,
        Message::Done(Done::Restored(Ok(Some(mock::me())))),
    )
    .await;
    run(&mut app, Message::Ui(Ui::SelectChat(ALICE))).await;
    run(&mut app, Message::Ui(Ui::ToggleInfo)).await;
    (app, fake)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn downloaded_previews_become_fixed_tiles_off_the_ui_thread() {
    let (mut app, _fake) = plain_app_with_info().await;
    // The fake serves 160x120 previews; the panel opened and fetched the
    // visible ones. Every cached tile is the same square, whatever its source.
    let keys: Vec<_> = app
        .info
        .as_ref()
        .unwrap()
        .wanted_thumbs()
        .into_iter()
        .map(|id| (ALICE, id))
        .collect();
    assert!(!keys.is_empty());
    for k in &keys {
        assert!(app.thumbs.peek(k).is_some(), "{k:?} fetched and decoded");
    }
    // Garbage is rejected without caching and is not retried.
    let bad = (ALICE, 424_242);
    run(
        &mut app,
        Message::Done(Done::Thumb {
            key: bad,
            result: Ok(vec![7u8; 4096]),
        }),
    )
    .await;
    assert!(app.thumbs.peek(&bad).is_none());
    assert_eq!(app.thumb_q.pending(), 0, "the slot was released");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_visible_rows_are_fetched_and_scrolled_away_ones_are_evicted() {
    let (mut app, fake) = plain_app_with_info().await;
    // 600 items in the tab, a small cache so eviction is visible.
    app.thumbs = ferrogram::app::state::Lru::new(60 * 104 * 104 * 4);
    {
        let tab = app.info.as_mut().unwrap().tab_mut(SharedKind::Media);
        tab.items = (0..600)
            .map(|i| SharedItem {
                id: 100_000 - i,
                date: 0,
                body: SharedBody::Photo {
                    width: 320,
                    height: 320,
                },
            })
            .collect();
        tab.loaded = true;
        tab.next = None;
    }
    let before = fake
        .calls()
        .iter()
        .filter(|c| *c == "download_thumb")
        .count();
    app.info.as_mut().unwrap().set_view(420.0, 700.0, 420.0);
    run(&mut app, Message::Ui(Ui::InfoTab(SharedKind::Media))).await;
    let wanted = app.info.as_ref().unwrap().wanted_thumbs();
    let first_downloads = fake
        .calls()
        .iter()
        .filter(|c| *c == "download_thumb")
        .count()
        - before;
    assert_eq!(first_downloads, wanted.len(), "exactly the window, not 600");
    assert!(wanted.len() <= 15 * ferrogram::app::info::COLUMNS);
    assert!(app.thumbs.peek(&(ALICE, wanted[0])).is_some());

    // Scroll far down: the new rows load, memory stays bounded.
    for step in 1..=20 {
        let y = 420.0 + step as f32 * 600.0;
        app.info.as_mut().unwrap().set_view(y, 700.0, 420.0);
        run(&mut app, Message::Ui(Ui::InfoTab(SharedKind::Media))).await;
        for id in app.info.as_ref().unwrap().wanted_thumbs() {
            assert!(
                app.thumbs.peek(&(ALICE, id)).is_some(),
                "visible tile {id} is there after step {step}"
            );
        }
        assert!(app.thumbs.bytes() <= 60 * 104 * 104 * 4 + 104 * 104 * 4);
    }
    assert!(
        app.thumbs.peek(&(ALICE, wanted[0])).is_none(),
        "the first rows were evicted after scrolling away"
    );
    // The scrolled-away rows were never downloaded: far fewer than 600.
    let total = fake
        .calls()
        .iter()
        .filter(|c| *c == "download_thumb")
        .count()
        - before;
    assert!(
        total < 400,
        "{total} downloads for 20 screens of a 600-item tab"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn view_reuses_one_handle_per_thumbnail() {
    let app = app_with_media(30).await;
    let key = (ALICE, 100_000);
    let id = |app: &App| app.thumbs.peek(&key).unwrap().id();
    let first = id(&app);
    let mut bench = Bench::new();
    for _ in 0..3 {
        let _ = bench.frame(&app, &[]);
    }
    assert_eq!(id(&app), first, "building views never recreates the handle");
    // And the cache hands out clones that share that id.
    assert_eq!(app.thumbs.peek(&key).unwrap().clone().id(), first);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_scale_factor_redoes_the_tiles_at_the_drawn_size() {
    let (mut app, _fake) = plain_app_with_info().await;
    let key = (ALICE, app.info.as_ref().unwrap().wanted_thumbs()[0]);
    let size_of = |app: &App| match app.thumbs.peek(&key).unwrap() {
        iced::advanced::image::Handle::Rgba { width, .. } => *width,
        other => panic!("expected decoded pixels, got {other:?}"),
    };
    assert_eq!(size_of(&app), 104);
    run(&mut app, Message::Ui(Ui::Rescaled(1.5))).await;
    assert_eq!(size_of(&app), 156, "tiles follow the window's scale");
    run(&mut app, Message::Ui(Ui::Rescaled(f32::NAN))).await;
    assert_eq!(size_of(&app), 104, "nonsense falls back to 1.0");
}
