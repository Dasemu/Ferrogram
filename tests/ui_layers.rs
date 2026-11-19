//! Layout-level regression tests for the flicker reports: the real views are
//! laid out with iced's `UserInterface` on the CPU renderer (no window) and
//! probed with a cursor. A modal layer must hide the cursor from every layer
//! below it, otherwise hover highlights and pointer shapes of the hidden chat
//! list change under the dim backdrop as the mouse moves.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use ferrogram::app::{App, Done, Message, Ui};
use ferrogram::config::Config;
use ferrogram::mock::{self, FakeOptions, FakeTelegram};
use ferrogram::net::ApiHandle;
use iced::advanced::clipboard;
use iced::advanced::mouse;
use iced::{Pixels, Point, Size};
use iced_runtime::user_interface::{Cache, State, UserInterface};

const WINDOW: Size = Size::new(1180.0, 760.0);

fn signed_in_app() -> App {
    let mut cfg = Config::from_env(&["ferrogram".into(), "--mock".into()]);
    cfg.notifications = false;
    let mut app = App::boot(cfg).0;
    let fake = FakeTelegram::new(FakeOptions {
        logged_in: true,
        live_simulation: false,
        ..Default::default()
    });
    let _ = app.update(Message::Done(Done::ApiReady(Ok(ApiHandle(fake)))));
    let _ = app.update(Message::Done(Done::Restored(Ok(Some(mock::me())))));
    let _ = app.update(Message::Done(Done::Dialogs(Ok(mock::mock_chats()))));
    app
}

fn renderer() -> iced::Renderer {
    iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
        iced::Font::DEFAULT,
        Pixels(16.0),
    ))
}

/// The pointer shape the real view asks for with the cursor at `at`.
fn interaction_at(app: &App, at: Point) -> mouse::Interaction {
    let mut renderer = renderer();
    let mut ui = UserInterface::build(
        ferrogram::ui::view(app),
        WINDOW,
        Cache::default(),
        &mut renderer,
    );
    let cursor = mouse::Cursor::Available(at);
    let mut messages: Vec<Message> = Vec::new();
    let (state, _) = ui.update(
        &[iced::Event::Mouse(mouse::Event::CursorMoved {
            position: at,
        })],
        cursor,
        &mut renderer,
        &mut clipboard::Null,
        &mut messages,
    );
    match state {
        State::Updated {
            mouse_interaction, ..
        } => mouse_interaction,
        State::Outdated => mouse::Interaction::None,
    }
}

/// Middle of the first chat row (a button: pointer when nothing covers it).
const ROW: Point = Point::new(150.0, 100.0);

#[test]
fn chat_row_shows_pointer_without_a_modal() {
    let app = signed_in_app();
    assert_eq!(interaction_at(&app, ROW), mouse::Interaction::Pointer);
}

#[test]
fn settings_backdrop_hides_the_cursor_from_the_chat_list() {
    let mut app = signed_in_app();
    let _ = app.update(Message::Ui(Ui::OpenSettings));
    assert!(app.settings_open);
    assert_ne!(
        interaction_at(&app, ROW),
        mouse::Interaction::Pointer,
        "rows below the dim backdrop must not react to the pointer"
    );
}

#[test]
fn view_builds_the_same_icon_handles_every_frame() {
    use ferrogram::ui::icons::Icon;
    let a = Icon::Search.handle();
    let b = Icon::Search.handle();
    assert_eq!(a.id(), b.id());
    assert_eq!(
        ferrogram::ui::icons::tail_handle(true).id(),
        ferrogram::ui::icons::tail_handle(true).id()
    );
    assert_ne!(Icon::Search.handle().id(), Icon::Menu.handle().id());
}
