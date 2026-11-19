//! Background mode through the real `App::update`: close to tray vs quit,
//! showing again, `--minimized`, mute and the tray model. Tasks returned by
//! `update` are not run here (no window, no D-Bus); the state they leave
//! behind is what is checked.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use ferrogram::app::{App, Done, Message, Ui};
use ferrogram::config::Config;
use ferrogram::mock::{self, ALICE, FakeOptions, FakeTelegram};
use ferrogram::net::ApiHandle;
use ferrogram::prefs::MuteState;
use ferrogram::remote::{MuteRequest, Remote};

fn app_with(args: &[&str]) -> App {
    let mut all = vec!["ferrogram".to_owned(), "--mock".to_owned()];
    all.extend(args.iter().map(|a| (*a).to_owned()));
    let mut cfg = Config::from_env(&all);
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

fn ui(app: &mut App, u: Ui) {
    let _ = app.update(Message::Ui(u));
}

fn remote(app: &mut App, r: Remote) {
    ui(app, Ui::Remote(r));
}

/// An app whose tray host is known to exist (what `TrayReady(Ok)` records).
fn app_with_tray() -> App {
    let mut app = app_with(&[]);
    app.window.tray_ready(true);
    app
}

#[test]
fn the_window_opens_at_startup_by_default() {
    let app = app_with(&[]);
    assert!(app.window_id.is_some() && app.window.open);
    assert!(app.prefs.close_to_tray, "close to tray is the default");
    assert!(!app.prefs.start_minimized);
}

#[test]
fn close_button_hides_to_the_tray_and_frees_the_images() {
    let mut app = app_with_tray();
    ui(&mut app, Ui::SelectChat(ALICE));
    app.focused = true;
    ui(&mut app, Ui::ToggleMainMenu);
    ui(&mut app, Ui::CloseRequested);
    assert!(app.window_id.is_none() && !app.window.open, "window closed");
    assert!(!app.focused);
    assert!(!app.main_menu, "transient overlays are reset");
    assert_eq!(app.photos.len(), 0);
    assert_eq!(app.avatars.len(), 0);
    // The app itself keeps its state and connection.
    assert_eq!(app.selected, Some(ALICE));
    assert!(app.api.is_some() && app.auth.is_signed_in());
    assert!(!app.chats.all().is_empty());
}

#[test]
fn close_button_quits_when_there_is_no_tray_host() {
    let mut app = app_with(&[]);
    // Registration failed: nothing to hide into.
    let _ = app.update(Message::Done(Done::TrayReady(Err("no watcher".into()))));
    ui(&mut app, Ui::CloseRequested);
    assert!(
        app.window_id.is_some(),
        "window is not hidden; the app exits"
    );
    assert!(app.window.open);
}

#[test]
fn close_button_quits_when_close_to_tray_is_off() {
    let mut app = app_with_tray();
    ui(&mut app, Ui::SetCloseToTray(false));
    assert!(!app.prefs.close_to_tray && !app.window.close_to_tray);
    ui(&mut app, Ui::CloseRequested);
    assert!(
        app.window_id.is_some(),
        "not hidden: the quit task runs instead"
    );
    ui(&mut app, Ui::SetCloseToTray(true));
    ui(&mut app, Ui::CloseRequested);
    assert!(app.window_id.is_none());
}

#[test]
fn tray_toggle_and_notification_click_bring_the_chat_back() {
    let mut app = app_with_tray();
    remote(&mut app, Remote::Toggle);
    assert!(app.window_id.is_none(), "left click hides");
    remote(&mut app, Remote::Toggle);
    assert!(app.window_id.is_some(), "left click shows");
    remote(&mut app, Remote::Hide);
    assert!(app.window_id.is_none());
    // A notification click: show the window and open that chat.
    remote(&mut app, Remote::Show { chat: Some(ALICE) });
    assert!(app.window_id.is_some() && app.window.open);
    assert_eq!(app.selected, Some(ALICE));
    // A chat we do not know is ignored; the window still shows.
    remote(&mut app, Remote::Hide);
    remote(
        &mut app,
        Remote::Show {
            chat: Some(987_654_321),
        },
    );
    assert!(app.window_id.is_some());
    assert_eq!(app.selected, Some(ALICE));
}

#[test]
fn second_launch_while_visible_just_focuses() {
    let mut app = app_with_tray();
    let id = app.window_id;
    remote(&mut app, Remote::Show { chat: None });
    assert_eq!(app.window_id, id, "same window, not a second one");
}

#[test]
fn hiding_without_a_tray_is_refused() {
    let mut app = app_with(&[]);
    let _ = app.update(Message::Done(Done::TrayReady(Err("none".into()))));
    remote(&mut app, Remote::Hide);
    remote(&mut app, Remote::Toggle);
    assert!(app.window_id.is_some());
}

#[test]
fn minimized_start_has_no_window_until_the_tray_fails() {
    let mut app = app_with(&["--minimized"]);
    assert!(app.window_id.is_none() && !app.window.open, "tray only");
    // The tray registered: still no window.
    app.window.tray_ready(true);
    assert!(app.window_id.is_none());
    // But with no tray host the process would be unreachable: show it.
    let mut app = app_with(&["--minimized"]);
    let _ = app.update(Message::Done(Done::TrayReady(Err("no watcher".into()))));
    assert!(app.window_id.is_some() && app.window.open);
}

#[test]
fn losing_the_tray_while_hidden_reopens_the_window() {
    let mut app = app_with_tray();
    remote(&mut app, Remote::Hide);
    assert!(app.window_id.is_none());
    remote(&mut app, Remote::TrayLost);
    assert!(app.window_id.is_some(), "never leave a ghost process");
    remote(&mut app, Remote::TrayBack);
    remote(&mut app, Remote::Hide);
    assert!(
        app.window_id.is_none(),
        "hiding works again when the tray is back"
    );
}

#[test]
fn start_minimized_toggle_updates_the_preference_and_the_tray() {
    let mut app = app_with(&[]);
    remote(&mut app, Remote::ToggleStartMinimized);
    assert!(app.prefs.start_minimized);
    assert!(app.tray_model().start_minimized);
    ui(&mut app, Ui::SetStartMinimized(false));
    assert!(!app.prefs.start_minimized);
}

#[test]
fn tray_model_counts_unread_in_unmuted_chats_only() {
    let mut app = app_with(&[]);
    let model = app.tray_model();
    let expected: u32 = app
        .chats
        .all()
        .iter()
        .filter(|c| !c.muted)
        .map(|c| c.unread_count)
        .sum();
    assert!(expected > 0, "the mock data has unread messages");
    assert!(
        app.chats
            .all()
            .iter()
            .any(|c| c.muted && c.unread_count > 0)
    );
    assert_eq!(model.unread, expected);
    assert!(model.needs_attention());
    assert_eq!(
        model.tooltip_title(),
        format!("Ferrogram — {expected} unread")
    );
    assert!(model.window_open);
    // Reading the open chat lowers it.
    ui(&mut app, Ui::SelectChat(ALICE));
    assert!(app.tray_model().unread < expected);
}

#[test]
fn tray_model_is_quiet_when_signed_out() {
    let mut app = app_with(&[]);
    let _ = app.update(Message::Done(Done::LoggedOut));
    assert_eq!(app.tray_model().unread, 0);
    assert!(!app.tray_model().needs_attention());
}

#[test]
fn mute_from_the_tray_and_back() {
    let mut app = app_with(&[]);
    assert!(!app.muted() && app.tray_model().muted.is_none());
    remote(&mut app, Remote::Mute(MuteRequest::OneHour));
    assert!(matches!(app.prefs.mute, MuteState::Until(_)));
    assert!(app.muted());
    let text = app.tray_model().muted.expect("muted text");
    assert!(text.starts_with("until "), "{text}");
    remote(&mut app, Remote::Mute(MuteRequest::UntilEnabled));
    assert_eq!(app.tray_model().muted.as_deref(), Some("until re-enabled"));
    remote(&mut app, Remote::Mute(MuteRequest::Off));
    assert!(!app.muted() && app.tray_model().muted.is_none());
}

#[test]
fn mock_mode_never_writes_preferences_or_autostart() {
    let mut app = app_with(&[]);
    let prefs_before = app.config.prefs_path().exists();
    let autostart_before = ferrogram::autostart::system_is_enabled();
    ui(&mut app, Ui::SetAutostart(!autostart_before));
    assert_eq!(
        app.autostart, !autostart_before,
        "the toggle shows the demo state"
    );
    ui(&mut app, Ui::SetCloseToTray(false));
    ui(&mut app, Ui::SetStartMinimized(true));
    remote(&mut app, Remote::Mute(MuteRequest::UntilEnabled));
    assert_eq!(ferrogram::autostart::system_is_enabled(), autostart_before);
    assert_eq!(app.config.prefs_path().exists(), prefs_before);
}

#[test]
fn settings_open_and_escape_closes_them_first() {
    let mut app = app_with(&[]);
    ui(&mut app, Ui::ToggleMainMenu);
    ui(&mut app, Ui::OpenSettings);
    assert!(app.settings_open && !app.main_menu);
    ui(&mut app, Ui::Escape);
    assert!(!app.settings_open);
}

#[test]
fn window_geometry_is_remembered_for_the_next_show() {
    let mut app = app_with_tray();
    ui(&mut app, Ui::WindowResized(iced::Size::new(1400.0, 900.0)));
    ui(&mut app, Ui::WindowMoved(iced::Point::new(120.0, 80.0)));
    ui(&mut app, Ui::WindowResized(iced::Size::new(0.0, 0.0)));
    remote(&mut app, Remote::Hide);
    assert_eq!(app.geometry.size, (1400.0, 900.0));
    assert_eq!(app.geometry.position, Some((120.0, 80.0)));
}
