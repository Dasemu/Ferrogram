//! Integration tests: drive the real `App::update` and the `FakeTelegram`
//! backend through login, chat list, history paging, sending, edits,
//! deletes, reactions, search and the update stream. No network.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use ferrogram::app::state::{ComposeMode, PAGE};
use ferrogram::app::{App, Done, Message, Ui};
use ferrogram::auth::{self, AuthCommand, AuthEvent, AuthState, SignInOutcome};
use ferrogram::config::Config;
use ferrogram::mock::{self, ALICE, CODE_2FA, ECHO_BOT, FakeOptions, FakeTelegram, PASSWORD};
use ferrogram::model::{Message as ChatMessage, SendState};
use ferrogram::net::{ApiHandle, Event, TelegramApi};
use futures::StreamExt;

fn mock_app() -> App {
    let mut cfg = Config::from_env(&["ferrogram".into(), "--mock".into()]);
    cfg.notifications = false;
    App::boot(cfg).0
}

fn signed_in_app() -> App {
    let mut app = mock_app();
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

#[test]
fn boot_renders_from_cache_and_waits_for_restore() {
    let app = mock_app();
    assert_eq!(app.auth, AuthState::Restoring);
    assert!(app.chats.all().is_empty());
    assert_eq!(app.title(), "Ferrogram");
}

#[test]
fn restored_session_shows_chat_list_with_unread_badges() {
    let app = signed_in_app();
    assert!(app.auth.is_signed_in());
    assert_eq!(app.chats.all()[0].title, "Saved Messages", "pinned first");
    assert!(app.chats.total_unread() > 0);
    assert!(app.title().starts_with("Ferrogram ("));
}

#[test]
fn open_chat_merges_cache_then_network_and_pages_older() {
    let mut app = signed_in_app();
    let _ = app.update(Message::Ui(Ui::SelectChat(ALICE)));
    assert_eq!(app.selected, Some(ALICE));
    assert_eq!(app.chats.get(ALICE).unwrap().unread_count, 0);

    let history = mock::mock_history(ALICE);
    let newest: Vec<_> = history[history.len() - PAGE..].to_vec();
    let _ = app.update(Message::Done(Done::CacheHistory {
        chat: ALICE,
        before: None,
        complete: false,
        result: Ok(newest[PAGE - 10..].to_vec()),
    }));
    assert_eq!(app.conversation.as_ref().unwrap().messages().len(), 10);
    let _ = app.update(Message::Done(Done::NetHistory {
        chat: ALICE,
        before: None,
        result: Ok(newest.clone()),
    }));
    let conv = app.conversation.as_ref().unwrap();
    assert_eq!(conv.messages().len(), PAGE);
    assert!(!conv.reached_start);
    let oldest = conv.oldest_id().unwrap();

    // Older page outside the confirmed range -> network fills it.
    let older: Vec<_> = history.iter().filter(|m| m.id < oldest).cloned().collect();
    app.conversation.as_mut().unwrap().loading_older = true; // as load_older() does
    let _ = app.update(Message::Done(Done::CacheHistory {
        chat: ALICE,
        before: Some(oldest),
        complete: false,
        result: Ok(Vec::new()),
    }));
    assert!(
        app.conversation.as_ref().unwrap().loading_older,
        "network fetch must still be pending"
    );
    let _ = app.update(Message::Done(Done::NetHistory {
        chat: ALICE,
        before: Some(oldest),
        result: Ok(older[older.len() - PAGE..].to_vec()),
    }));
    let conv = app.conversation.as_ref().unwrap();
    assert_eq!(conv.messages().len(), 2 * PAGE);
    assert!(!conv.loading_older);
    let ids: Vec<_> = conv.messages().iter().map(|m| m.id).collect();
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "sorted ascending");

    // Results for another chat are ignored.
    let _ = app.update(Message::Done(Done::NetHistory {
        chat: 999,
        before: None,
        result: Ok(vec![ChatMessage::text(999, 1, false, 0, "x")]),
    }));
    assert_eq!(
        app.conversation.as_ref().unwrap().messages().len(),
        2 * PAGE
    );
}

#[test]
fn send_reply_resolves_pending_and_updates_list() {
    let mut app = signed_in_app();
    let _ = app.update(Message::Ui(Ui::SelectChat(ALICE)));
    let _ = app.update(Message::Done(Done::NetHistory {
        chat: ALICE,
        before: None,
        result: Ok(mock::mock_history(ALICE)),
    }));
    let _ = app.update(Message::Ui(Ui::Reply(148)));
    assert_eq!(app.compose_mode, ComposeMode::Reply(148));
    let _ = app.update(Message::Ui(Ui::ComposerChanged("  see you  ".into())));
    let _ = app.update(Message::Ui(Ui::Send));
    assert!(app.composer.is_empty());
    assert_eq!(app.compose_mode, ComposeMode::Normal);
    let pending = app
        .conversation
        .as_ref()
        .unwrap()
        .messages()
        .last()
        .unwrap()
        .clone();
    assert_eq!(pending.state, SendState::Sending);
    assert_eq!(pending.reply_to, Some(148));
    assert_eq!(pending.text, "see you");

    let mut real = pending.clone();
    real.id = 500;
    real.state = SendState::Sent;
    let _ = app.update(Message::Done(Done::Sent {
        chat: ALICE,
        temp: pending.id,
        result: Ok(real),
    }));
    let conv = app.conversation.as_ref().unwrap();
    assert!(conv.get(pending.id).is_none());
    assert_eq!(conv.get(500).unwrap().state, SendState::Sent);
    assert_eq!(
        app.chats
            .all()
            .iter()
            .find(|c| c.id == ALICE)
            .unwrap()
            .last_message
            .as_ref()
            .unwrap()
            .id,
        500
    );
}

#[test]
fn edit_delete_react_and_events() {
    let mut app = signed_in_app();
    let _ = app.update(Message::Ui(Ui::SelectChat(ALICE)));
    let _ = app.update(Message::Done(Done::NetHistory {
        chat: ALICE,
        before: None,
        result: Ok(mock::mock_history(ALICE)),
    }));
    // Edit an outgoing message.
    let _ = app.update(Message::Ui(Ui::Edit(149)));
    assert_eq!(app.composer, "You're the best!");
    let _ = app.update(Message::Ui(Ui::ComposerChanged("You're the best!!".into())));
    let _ = app.update(Message::Ui(Ui::Send));
    let m = app.conversation.as_ref().unwrap().get(149).unwrap();
    assert!(m.edited && m.text.ends_with("!!"));

    // React, then un-react.
    let _ = app.update(Message::Ui(Ui::React(150, "🎉".into())));
    assert!(
        app.conversation
            .as_ref()
            .unwrap()
            .get(150)
            .unwrap()
            .reactions[0]
            .chosen
    );
    let _ = app.update(Message::Ui(Ui::React(150, "🎉".into())));
    assert!(
        app.conversation
            .as_ref()
            .unwrap()
            .get(150)
            .unwrap()
            .reactions
            .is_empty()
    );

    // Delete locally.
    let _ = app.update(Message::Ui(Ui::Delete(150)));
    assert!(app.conversation.as_ref().unwrap().get(150).is_none());

    // Incoming message for an open, focused chat: no unread bump.
    let incoming = ChatMessage::text(ALICE, 600, false, 1, "live!");
    let _ = app.update(Message::Net(Event::NewMessage(incoming)));
    assert_eq!(app.chats.get(ALICE).unwrap().unread_count, 0);
    assert!(app.conversation.as_ref().unwrap().get(600).is_some());

    // Window unfocused: counts as unread.
    let _ = app.update(Message::Ui(Ui::Focus(false)));
    let _ = app.update(Message::Net(Event::NewMessage(ChatMessage::text(
        ALICE, 601, false, 2, "again",
    ))));
    assert_eq!(app.chats.get(ALICE).unwrap().unread_count, 1);
    let _ = app.update(Message::Ui(Ui::Focus(true)));
    assert_eq!(app.chats.get(ALICE).unwrap().unread_count, 0);

    // Server-side edit / delete / read receipts.
    let mut edited = ChatMessage::text(ALICE, 600, false, 1, "live (edited)");
    edited.edited = true;
    let _ = app.update(Message::Net(Event::MessageEdited(edited)));
    assert_eq!(
        app.conversation.as_ref().unwrap().get(600).unwrap().text,
        "live (edited)"
    );
    let _ = app.update(Message::Net(Event::MessagesDeleted {
        chat: None,
        ids: vec![600],
    }));
    assert!(app.conversation.as_ref().unwrap().get(600).is_none());
    let _ = app.update(Message::Net(Event::ReadOutbox {
        chat: ALICE,
        max_id: 9999,
    }));
    assert_eq!(app.chats.get(ALICE).unwrap().read_outbox_max_id, 9999);
}

#[test]
fn search_sidebar_and_in_chat() {
    let mut app = signed_in_app();
    let _ = app.update(Message::Ui(Ui::SidebarSearch("ali".into())));
    assert_eq!(app.chats.filter(&app.sidebar_query).len(), 1);
    let _ = app.update(Message::Ui(Ui::SelectChat(ALICE)));
    let _ = app.update(Message::Ui(Ui::ToggleChatSearch));
    let _ = app.update(Message::Ui(Ui::ChatSearchInput("trail".into())));
    let _ = app.update(Message::Ui(Ui::ChatSearchSubmit));
    assert!(app.chat_search.as_ref().unwrap().searching);
    let hit = ChatMessage::text(ALICE, 146, false, 0, "Here's the trail map");
    let _ = app.update(Message::Done(Done::SearchResults {
        chat: ALICE,
        query: "trail".into(),
        result: Ok(vec![hit]),
    }));
    let s = app.chat_search.as_ref().unwrap();
    assert!(!s.searching);
    assert_eq!(s.results.len(), 1);
    let _ = app.update(Message::Ui(Ui::Escape));
    assert!(app.chat_search.is_none());
}

#[test]
fn session_revoked_returns_to_login() {
    let mut app = signed_in_app();
    let _ = app.update(Message::Ui(Ui::SelectChat(ALICE)));
    let _ = app.update(Message::Net(Event::SessionRevoked));
    assert!(matches!(app.auth, AuthState::Phone { error: Some(_), .. }));
    assert!(app.chats.all().is_empty());
    assert!(app.selected.is_none());
}

#[test]
fn ui_login_flow_with_2fa() {
    let mut app = mock_app();
    let fake = FakeTelegram::new(FakeOptions::default());
    let _ = app.update(Message::Done(Done::ApiReady(Ok(ApiHandle(fake)))));
    let _ = app.update(Message::Done(Done::Restored(Ok(None))));
    let _ = app.update(Message::Ui(Ui::AuthInput("+34 600 123 412".into())));
    let _ = app.update(Message::Ui(Ui::AuthSubmit));
    assert!(app.auth.is_busy());
    let _ = app.update(Message::Done(Done::CodeRequested(Ok(()))));
    let _ = app.update(Message::Ui(Ui::AuthInput(CODE_2FA.into())));
    let _ = app.update(Message::Ui(Ui::AuthSubmit));
    let _ = app.update(Message::Done(Done::SignIn(Ok(
        SignInOutcome::PasswordRequired {
            hint: Some("h".into()),
        },
    ))));
    assert!(matches!(app.auth, AuthState::Password { .. }));
    let _ = app.update(Message::Ui(Ui::AuthInput(PASSWORD.into())));
    let _ = app.update(Message::Ui(Ui::AuthSubmit));
    let _ = app.update(Message::Done(Done::SignIn(Ok(SignInOutcome::SignedIn(
        mock::me(),
    )))));
    assert!(app.auth.is_signed_in());
}

// ----------------------------------------------- fake backend end-to-end

/// Run the pure auth reducer against the fake backend, executing commands.
async fn login(api: &FakeTelegram, phone: &str, code: &str, password: Option<&str>) -> AuthState {
    let mut s = AuthState::Restoring;
    let restored = api.restore().await;
    let mut pending = auth::reduce(&mut s, AuthEvent::Restored(restored));
    let inputs = [
        phone.to_owned(),
        code.to_owned(),
        password.unwrap_or("").to_owned(),
    ];
    let mut step = 0;
    loop {
        while let Some(cmd) = pending.take() {
            let ev = match cmd {
                AuthCommand::RequestCode(p) => AuthEvent::CodeRequested(api.request_code(p).await),
                AuthCommand::SignIn { code } => AuthEvent::SignIn(api.sign_in(code).await),
                AuthCommand::CheckPassword(pw) => AuthEvent::SignIn(api.check_password(pw).await),
                AuthCommand::PersistSession => {
                    api.persist_session().await.unwrap();
                    return s;
                }
            };
            pending = auth::reduce(&mut s, ev);
        }
        if step >= inputs.len() || s.is_signed_in() {
            return s;
        }
        auth::reduce(&mut s, AuthEvent::Input(inputs[step].clone().into()));
        step += 1;
        pending = auth::reduce(&mut s, AuthEvent::Submit);
        if pending.is_none() {
            return s;
        }
    }
}

#[tokio::test]
async fn fake_login_with_and_without_2fa() {
    let api = FakeTelegram::new(FakeOptions::default());
    let s = login(&api, "+34600123412", CODE_2FA, Some(PASSWORD)).await;
    assert!(s.is_signed_in(), "{s:?}");
    assert!(api.calls().contains(&"check_password".to_owned()));
    assert!(api.calls().contains(&"persist_session".to_owned()));

    let api = FakeTelegram::new(FakeOptions::default());
    let s = login(&api, "+34600123412", mock::CODE_PLAIN, None).await;
    assert!(s.is_signed_in());

    let api = FakeTelegram::new(FakeOptions::default());
    let s = login(&api, "+34600123412", CODE_2FA, Some("wrong")).await;
    assert!(matches!(s, AuthState::Password { error: Some(_), .. }));
}

#[tokio::test]
async fn fake_history_paging_reaches_start() {
    let api = FakeTelegram::new(FakeOptions {
        logged_in: true,
        ..Default::default()
    });
    let mut before = None;
    let mut total = 0;
    loop {
        let page = api.history(ALICE, before, PAGE).await.unwrap();
        total += page.len();
        if page.len() < PAGE {
            break;
        }
        before = page.first().map(|m| m.id);
    }
    assert_eq!(total, mock::mock_history(ALICE).len());
}

#[tokio::test]
async fn fake_update_stream_delivers_echo_and_read_receipts() {
    let api: Arc<FakeTelegram> = FakeTelegram::new(FakeOptions {
        logged_in: true,
        live_simulation: true,
        ..Default::default()
    });
    let mut events = api.events();
    let sent = api.send_text(ECHO_BOT, "ping".into(), None).await.unwrap();
    let mut got_read = false;
    let mut got_echo = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !(got_read && got_echo) && tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(3), events.next()).await {
            Ok(Some(Event::ReadOutbox { max_id, .. })) => got_read = max_id == sent.id,
            Ok(Some(Event::NewMessage(m))) => {
                got_echo = m.text == "ping" && m.reply_to == Some(sent.id);
            }
            _ => {}
        }
    }
    assert!(got_read && got_echo);
    // A new subscriber takes over; the old stream ends instead of
    // swallowing events.
    let mut fresh = api.events();
    assert!(events.next().await.is_none());
    api.push_event(Event::ReadOutbox {
        chat: ECHO_BOT,
        max_id: 1,
    });
    assert!(matches!(fresh.next().await, Some(Event::ReadOutbox { .. })));
}

#[tokio::test]
async fn login_logout_login_still_receives_events() {
    let api = FakeTelegram::new(FakeOptions {
        live_simulation: false,
        ..Default::default()
    });
    assert!(
        login(&api, "+34600123412", mock::CODE_PLAIN, None)
            .await
            .is_signed_in()
    );
    let _first = api.events();
    api.log_out().await.unwrap();
    assert!(api.dialogs(10).await.is_err());
    assert!(
        login(&api, "+34600123412", mock::CODE_PLAIN, None)
            .await
            .is_signed_in()
    );
    // The app subscribes again after re-login (new subscription generation).
    let mut second = api.events();
    api.edit_message(ALICE, 149, "after relogin".into())
        .await
        .unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(2), second.next())
        .await
        .expect("event after re-login");
    assert!(matches!(ev, Some(Event::MessageEdited(m)) if m.text == "after relogin"));
}

#[test]
fn logout_bumps_subscription_generation() {
    let mut app = signed_in_app();
    let gen_before = app.api_gen;
    let _ = app.update(Message::Done(Done::LoggedOut));
    assert!(matches!(app.auth, AuthState::Phone { .. }));
    assert!(app.api_gen > gen_before, "event subscription must restart");
}

#[test]
fn offline_restore_keeps_cache_and_retries() {
    let mut app = mock_app();
    // Cached chats from a previous run.
    app.chats.replace(mock::mock_chats());
    let fake = FakeTelegram::new(FakeOptions {
        logged_in: true,
        live_simulation: false,
        ..Default::default()
    });
    let _ = app.update(Message::Done(Done::ApiReady(Ok(ApiHandle(fake)))));
    let _ = app.update(Message::Done(Done::Restored(Err(
        ferrogram::error::Error::Net("offline".into()),
    ))));
    assert_eq!(
        app.auth,
        AuthState::Restoring,
        "never drop to the phone screen"
    );
    assert!(app.shows_main(), "cached chats stay visible");
    assert_eq!(app.restore_attempt, 1);
    assert!(matches!(
        app.conn,
        ferrogram::model::ConnState::Reconnecting { attempt: 1 }
    ));
    let _ = app.update(Message::Done(Done::Restored(Err(
        ferrogram::error::Error::Net("still offline".into()),
    ))));
    assert_eq!(app.restore_attempt, 2);
    // Network comes back.
    let _ = app.update(Message::Done(Done::Restored(Ok(Some(mock::me())))));
    assert!(app.auth.is_signed_in());
    assert_eq!(app.restore_attempt, 0);

    // A definite "not authorized" does go to the phone screen.
    let mut app = mock_app();
    let _ = app.update(Message::Done(Done::Restored(Ok(None))));
    assert!(matches!(app.auth, AuthState::Phone { .. }));
}

#[test]
fn pending_messages_have_no_actions_and_failures_roll_back() {
    let mut app = signed_in_app();
    let _ = app.update(Message::Ui(Ui::SelectChat(ALICE)));
    let _ = app.update(Message::Done(Done::NetHistory {
        chat: ALICE,
        before: None,
        result: Ok(mock::mock_history(ALICE)),
    }));
    let _ = app.update(Message::Ui(Ui::ComposerChanged("pending".into())));
    let _ = app.update(Message::Ui(Ui::Send));
    let temp = app
        .conversation
        .as_ref()
        .unwrap()
        .messages()
        .last()
        .unwrap()
        .id;
    assert!(temp < 0);
    let _ = app.update(Message::Ui(Ui::MessageMenu(Some(temp))));
    assert!(app.menu_for.is_none());
    let _ = app.update(Message::Ui(Ui::Delete(temp)));
    assert!(app.conversation.as_ref().unwrap().get(temp).is_some());

    // Optimistic delete that fails on the server is restored.
    let old = app.conversation.as_ref().unwrap().get(150).unwrap().clone();
    let _ = app.update(Message::Ui(Ui::Delete(150)));
    assert!(app.conversation.as_ref().unwrap().get(150).is_none());
    let _ = app.update(Message::Done(Done::Rollback {
        old: old.clone(),
        error: ferrogram::error::Error::Protocol("MESSAGE_DELETE_FORBIDDEN".into()),
    }));
    assert_eq!(app.conversation.as_ref().unwrap().get(150), Some(&old));
    assert!(app.toast.is_some());
}

#[test]
fn dropped_files_need_confirmation_and_folders_are_rejected() {
    let mut app = signed_in_app();
    let _ = app.update(Message::Ui(Ui::SelectChat(ALICE)));
    let _ = app.update(Message::Done(Done::FileStat {
        path: "/tmp/some-dir".into(),
        result: Ok((0, true)),
    }));
    assert!(app.pending_file.is_none());
    assert!(app.toast.as_deref().unwrap().contains("Folders"));
    let _ = app.update(Message::Done(Done::FileStat {
        path: "/tmp/cat.jpg".into(),
        result: Ok((1234, false)),
    }));
    let f = app.pending_file.clone().unwrap();
    assert_eq!((f.name.as_str(), f.size), ("cat.jpg", 1234));
    let before = app.conversation.as_ref().unwrap().messages().len();
    let _ = app.update(Message::Ui(Ui::Escape));
    assert!(app.pending_file.is_none(), "Esc cancels the preview");
    assert_eq!(app.conversation.as_ref().unwrap().messages().len(), before);
    let _ = app.update(Message::Done(Done::FileStat {
        path: "/tmp/cat.jpg".into(),
        result: Ok((1234, false)),
    }));
    let _ = app.update(Message::Ui(Ui::SendPendingFile));
    assert!(app.pending_file.is_none());
    assert_eq!(
        app.conversation.as_ref().unwrap().messages().len(),
        before + 1
    );
}

#[test]
fn stale_sidebar_results_are_dropped_and_no_api_never_hangs() {
    let mut app = signed_in_app();
    let _ = app.update(Message::Ui(Ui::SidebarSearch("hik".into())));
    let _ = app.update(Message::Ui(Ui::SidebarSearch("hike".into())));
    let _ = app.update(Message::Done(Done::SidebarResults {
        query: "hik".into(),
        result: Ok(vec![ChatMessage::text(ALICE, 1, false, 0, "stale")]),
    }));
    assert!(app.sidebar_messages.is_empty());
    let _ = app.update(Message::Done(Done::SidebarResults {
        query: "hike".into(),
        result: Ok(vec![ChatMessage::text(ALICE, 1, false, 0, "fresh")]),
    }));
    assert_eq!(app.sidebar_messages.len(), 1);

    // Login form without a client: the step fails instead of spinning.
    let mut app = mock_app();
    let _ = app.update(Message::Done(Done::Restored(Ok(None))));
    let _ = app.update(Message::Ui(Ui::AuthInput("+34 600 123 412".into())));
    let _ = app.update(Message::Ui(Ui::AuthSubmit));
    assert!(!app.auth.is_busy());
}

#[tokio::test]
async fn received_files_can_be_saved() {
    let api = FakeTelegram::new(FakeOptions {
        logged_in: true,
        ..Default::default()
    });
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("trail-map.pdf");
    api.download_media(ALICE, 145, out.clone()).await.unwrap();
    assert!(
        std::fs::read_to_string(&out)
            .unwrap()
            .contains("trail-map.pdf")
    );
    assert!(
        api.download_media(ALICE, 141, out).await.is_err(),
        "text-only message"
    );
}

#[test]
fn ui_messages_do_not_leak_typed_secrets() {
    let ui = Ui::AuthInput("hunter2".into());
    let setup = Ui::SetupApiHash("0123456789abcdef0123456789abcdef".into());
    let dbg = format!("{ui:?} {setup:?}");
    assert!(!dbg.contains("hunter2") && !dbg.contains("0123456789abcdef"));
}

#[tokio::test]
async fn fake_edit_delete_react_emit_events() {
    let api = FakeTelegram::new(FakeOptions {
        logged_in: true,
        live_simulation: false,
        ..Default::default()
    });
    let mut events = api.events();
    api.edit_message(ALICE, 149, "changed".into())
        .await
        .unwrap();
    api.react(ALICE, 150, Some("👍".into())).await.unwrap();
    api.delete_messages(ALICE, vec![150]).await.unwrap();
    let a = events.next().await.unwrap();
    let b = events.next().await.unwrap();
    let c = events.next().await.unwrap();
    assert!(matches!(a, Event::MessageEdited(m) if m.text == "changed" && m.edited));
    assert!(matches!(b, Event::Reactions { reactions, .. } if reactions[0].chosen));
    assert!(matches!(c, Event::MessagesDeleted { chat: None, ids } if ids == vec![150]));
    let found = api.search(ALICE, "TRAIL".into()).await.unwrap();
    assert!(!found.is_empty());
    let photo = api.download_photo(ALICE, 143).await.unwrap();
    assert_eq!(&photo[1..4], b"PNG");
}

#[tokio::test]
async fn store_roundtrip_of_fake_dialogs() {
    let api = FakeTelegram::new(FakeOptions {
        logged_in: true,
        ..Default::default()
    });
    let chats = api.dialogs(100).await.unwrap();
    let store = ferrogram::store::Store::open_in_memory().unwrap();
    let to_store = chats.clone();
    store
        .call(move |s| s.upsert_chats(&to_store))
        .await
        .unwrap();
    let loaded = store.load_chats().unwrap();
    assert_eq!(loaded.len(), chats.len());
    assert_eq!(loaded[0].id, chats.iter().find(|c| c.pinned).unwrap().id);
    let unauth = FakeTelegram::new(FakeOptions::default());
    assert!(unauth.dialogs(10).await.unwrap_err().is_session_revoked());
}

#[test]
fn advanced_api_setup_is_optional_and_cancellable() {
    let mut app = mock_app();
    let _ = app.update(Message::Done(Done::Restored(Ok(None))));
    assert!(
        app.setup.is_none(),
        "phone screen first, like the official client"
    );
    let _ = app.update(Message::Ui(Ui::OpenApiSetup));
    assert!(app.setup.as_ref().unwrap().advanced);
    let _ = app.update(Message::Ui(Ui::SetupApiId("12x34".into())));
    assert_eq!(app.setup.as_ref().unwrap().api_id, "1234");
    // Malformed hash is rejected locally.
    let _ = app.update(Message::Ui(Ui::SetupApiHash("short".into())));
    let _ = app.update(Message::Ui(Ui::SetupSubmit));
    assert!(app.setup.as_ref().unwrap().error.is_some());
    let _ = app.update(Message::Ui(Ui::Escape));
    assert!(
        app.setup.is_none(),
        "Esc/Cancel returns to the phone screen"
    );
}

#[test]
fn missing_credentials_force_setup_without_cancel() {
    let mut app = mock_app();
    let _ = app.update(Message::Done(Done::ApiReady(Err(
        ferrogram::error::Error::Auth(ferrogram::error::AuthError::MissingApiCredentials),
    ))));
    let f = app.setup.clone().unwrap();
    assert!(!f.advanced);
    let _ = app.update(Message::Ui(Ui::CancelSetup));
    assert!(
        app.setup.is_some(),
        "a dev build without credentials cannot skip setup"
    );
}

#[test]
fn api_setup_flag_opens_the_form() {
    let mut cfg = Config::from_env(&["ferrogram".into(), "--mock".into(), "--api-setup".into()]);
    cfg.notifications = false;
    assert!(cfg.api_setup);
}

// ------------------------------------------------ avatars, typing, files

use ferrogram::app::load_avatar;
use ferrogram::model::{Media, Presence, TypingAction};
use ferrogram::store::avatars::AvatarDisk;

#[tokio::test]
async fn avatar_loads_from_network_then_from_disk_cache() {
    let dir = tempfile::tempdir().unwrap();
    let disk = AvatarDisk::new(dir.path().join("av"));
    let fake = FakeTelegram::new(FakeOptions {
        logged_in: true,
        ..Default::default()
    });
    let api = ApiHandle(fake.clone());
    let alice = mock::mock_chats()
        .into_iter()
        .find(|c| c.id == ALICE)
        .unwrap();
    let pid = alice.photo_id.expect("mock Alice has a profile photo");

    // Miss on disk -> downloaded and written.
    let bytes = load_avatar(Some(api.clone()), Some(disk.clone()), (ALICE, pid))
        .await
        .unwrap();
    assert!(ferrogram::app::state::avatar_is_acceptable(&bytes));
    assert_eq!(disk.read(ALICE, pid).unwrap(), bytes);
    assert_eq!(
        fake.calls()
            .iter()
            .filter(|c| *c == "download_avatar")
            .count(),
        1
    );

    // Hit on disk -> no network call, and it works offline (no API).
    let again = load_avatar(None, Some(disk.clone()), (ALICE, pid))
        .await
        .unwrap();
    assert_eq!(again, bytes);
    let again = load_avatar(Some(api.clone()), Some(disk.clone()), (ALICE, pid))
        .await
        .unwrap();
    assert_eq!(again, bytes);
    assert_eq!(
        fake.calls()
            .iter()
            .filter(|c| *c == "download_avatar")
            .count(),
        1
    );

    // Offline with an empty cache is "try later", not a permanent failure.
    let miss = load_avatar(None, Some(disk.clone()), (ALICE, pid + 1)).await;
    assert!(matches!(miss, Err(ferrogram::error::Error::Cancelled)));

    // A corrupt cache file is ignored and replaced from the network.
    std::fs::write(
        dir.path()
            .join("av")
            .join(AvatarDisk::file_name(ALICE, pid)),
        b"junk",
    )
    .unwrap();
    let healed = load_avatar(Some(api), Some(disk.clone()), (ALICE, pid))
        .await
        .unwrap();
    assert_eq!(healed, bytes);
}

#[test]
fn avatar_state_follows_photo_id_and_chat_list() {
    let mut app = signed_in_app();
    let alice = app.chats.get(ALICE).unwrap().clone();
    let pid = alice.photo_id.unwrap();
    assert!(app.avatar(ALICE).is_none(), "nothing loaded yet");
    // A chat without a photo id never has an avatar (initials fallback).
    assert!(app.chats.get(ECHO_BOT).unwrap().photo_id.is_none());

    let png = mock::mock_avatar_png(pid, 160).unwrap();
    let _ = app.update(Message::Done(Done::Avatar {
        chat: ALICE,
        photo_id: pid,
        result: Ok(png.clone()),
    }));
    assert!(app.avatar(ALICE).is_some());

    // The photo changed on the server: the stale image is not drawn.
    let mut changed = mock::mock_chats();
    for c in &mut changed {
        if c.id == ALICE {
            c.photo_id = Some(pid + 1);
        }
    }
    let _ = app.update(Message::Done(Done::Dialogs(Ok(changed))));
    assert!(app.avatar(ALICE).is_none());

    // Failures are remembered and not retried; "not now" is retried.
    let _ = app.update(Message::Done(Done::Avatar {
        chat: ALICE,
        photo_id: pid + 1,
        result: Err(ferrogram::error::Error::Protocol("x".into())),
    }));
    assert!(app.avatar_failed.contains(&(ALICE, pid + 1)));
    let _ = app.update(Message::Done(Done::Avatar {
        chat: -301,
        photo_id: 1,
        result: Err(ferrogram::error::Error::Cancelled),
    }));
    assert!(!app.avatar_failed.contains(&(-301, 1)));
    assert!(app.avatar_loading.len() <= ferrogram::app::AVATAR_CONCURRENCY);
}

#[test]
fn avatar_cache_is_bounded_and_cleared_on_logout() {
    let mut app = signed_in_app();
    let png = mock::mock_avatar_png(1, 160).unwrap();
    for chat in 0..500 {
        let _ = app.update(Message::Done(Done::Avatar {
            chat,
            photo_id: 1,
            result: Ok(png.clone()),
        }));
    }
    assert!(app.avatars.bytes() <= ferrogram::app::AVATAR_BUDGET);
    assert!(app.avatars.len() < 500);
    let _ = app.update(Message::Net(Event::SessionRevoked));
    assert!(app.avatars.is_empty());
}

#[test]
fn typing_and_presence_events_drive_the_ui_state() {
    let mut app = signed_in_app();
    let _ = app.update(Message::Ui(Ui::SelectChat(ALICE)));
    let typing = |chat, user, action| Message::Net(Event::Typing { chat, user, action });
    let _ = app.update(typing(ALICE, ALICE, Some(TypingAction::Typing)));
    assert_eq!(app.typing_status(ALICE).as_deref(), Some("typing…"));
    // Our own typing (from another device) is not shown.
    let _ = app.update(typing(ALICE, mock::SELF_ID, Some(TypingAction::Typing)));
    assert_eq!(app.typing.in_chat(ALICE).len(), 1);
    // The peer sends the message: status ends.
    let mut m = ChatMessage::text(ALICE, 700, false, 1, "done typing");
    m.sender_id = Some(ALICE);
    let _ = app.update(Message::Net(Event::NewMessage(m)));
    assert_eq!(app.typing_status(ALICE), None);

    // Group typing resolves the name from the group's last message.
    let _ = app.update(typing(mock::RUST_GROUP, 311, Some(TypingAction::Typing)));
    assert_eq!(
        app.typing_status(mock::RUST_GROUP).as_deref(),
        Some("Ana is typing…")
    );
    let _ = app.update(typing(mock::RUST_GROUP, 311, None));
    assert_eq!(app.typing_status(mock::RUST_GROUP), None);

    // Expiry.
    let _ = app.update(typing(ALICE, ALICE, Some(TypingAction::Typing)));
    assert!(app.typing_status(ALICE).is_some());
    app.typing.expire(i64::MAX);
    assert!(app.typing_status(ALICE).is_none());

    // Presence.
    let _ = app.update(Message::Net(Event::UserStatus {
        user: ALICE,
        presence: Presence::Recently,
    }));
    assert_eq!(app.chats.get(ALICE).unwrap().presence, Presence::Recently);
}

#[tokio::test]
async fn composing_sends_a_throttled_typing_action() {
    let mut app = signed_in_app();
    let _ = app.update(Message::Ui(Ui::SelectChat(ALICE)));
    assert!(app.typing_throttle.chat.is_none());
    let _ = app.update(Message::Ui(Ui::ComposerChanged("h".into())));
    assert_eq!(app.typing_throttle.chat, Some(ALICE));
    let _ = app.update(Message::Ui(Ui::ComposerChanged("he".into())));
    assert_eq!(app.typing_throttle.chat, Some(ALICE));
    // Emptying the box ends our typing action.
    let _ = app.update(Message::Ui(Ui::ComposerChanged(String::new())));
    assert!(app.typing_throttle.chat.is_none());
    // Sending a message resets it too.
    let _ = app.update(Message::Ui(Ui::ComposerChanged("hello".into())));
    assert!(app.typing_throttle.chat.is_some());
    let _ = app.update(Message::Ui(Ui::Send));
    assert!(app.typing_throttle.chat.is_none());
    // No typing in Saved Messages or while editing.
    let _ = app.update(Message::Ui(Ui::SelectChat(mock::SELF_ID)));
    let _ = app.update(Message::Ui(Ui::ComposerChanged("note".into())));
    assert!(app.typing_throttle.chat.is_none());
    let _ = app.update(Message::Ui(Ui::SelectChat(ALICE)));
    let _ = app.update(Message::Done(Done::NetHistory {
        chat: ALICE,
        before: None,
        result: Ok(mock::mock_history(ALICE)),
    }));
    let _ = app.update(Message::Ui(Ui::Edit(149)));
    let _ = app.update(Message::Ui(Ui::ComposerChanged("edited".into())));
    assert!(app.typing_throttle.chat.is_none());
}

#[tokio::test]
async fn fake_backend_records_typing_calls() {
    let api = FakeTelegram::new(FakeOptions {
        logged_in: true,
        ..Default::default()
    });
    api.set_typing(ALICE, true).await.unwrap();
    api.set_typing(ALICE, false).await.unwrap();
    let calls = api.calls();
    assert!(calls.contains(&"set_typing:on".to_owned()));
    assert!(calls.contains(&"set_typing:off".to_owned()));
}

#[test]
fn executable_files_are_save_only_and_never_opened() {
    let mut app = signed_in_app();
    let _ = app.update(Message::Ui(Ui::SelectChat(ALICE)));
    let mut exe = ChatMessage::text(ALICE, 800, false, 1, "");
    exe.media = Some(Media::File {
        name: "invoice.pdf.exe".into(),
        size: 10,
        mime: "application/octet-stream".into(),
    });
    let mut pdf = ChatMessage::text(ALICE, 801, false, 2, "");
    pdf.media = Some(Media::File {
        name: "trail-map.pdf".into(),
        size: 10,
        mime: "application/pdf".into(),
    });
    let mut huge = ChatMessage::text(ALICE, 802, false, 3, "");
    huge.media = Some(Media::File {
        name: "disk.iso".into(),
        size: ferrogram::media::MAX_OPEN_BYTES + 1,
        mime: String::new(),
    });
    let _ = app.update(Message::Net(Event::NewMessage(exe)));
    let _ = app.update(Message::Net(Event::NewMessage(pdf)));
    let _ = app.update(Message::Net(Event::NewMessage(huge)));

    let _ = app.update(Message::Ui(Ui::OpenFile(ALICE, 800)));
    assert!(
        app.toast
            .as_deref()
            .unwrap_or_default()
            .contains("not opened automatically"),
        "{:?}",
        app.toast
    );
    app.toast = None;
    // A safe type starts the download silently.
    let _ = app.update(Message::Ui(Ui::OpenFile(ALICE, 801)));
    assert!(app.toast.is_none());
    // Oversized files are refused with an explanation.
    let _ = app.update(Message::Ui(Ui::OpenFile(ALICE, 802)));
    assert!(
        app.toast
            .as_deref()
            .unwrap_or_default()
            .contains("too large")
    );
}

#[test]
fn desktop_open_refuses_save_only_names() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("run.sh");
    std::fs::write(&p, "#!/bin/sh\n").unwrap();
    assert!(ferrogram::desktop::open_file(&p).is_err());
    let p = d.path().join("setup.EXE");
    std::fs::write(&p, "x").unwrap();
    assert!(ferrogram::desktop::open_file(&p).is_err());
}

#[tokio::test]
async fn fake_dialog_paging_walks_the_whole_list() {
    let api = FakeTelegram::new(FakeOptions {
        logged_in: true,
        extra_chats: 250,
        ..Default::default()
    });
    let first = api.dialogs(100).await.unwrap();
    assert_eq!(first.len(), 100);
    let mut all = first;
    loop {
        let page = api.more_dialogs(100).await.unwrap();
        let n = page.len();
        all.extend(page);
        if n < 100 {
            break;
        }
    }
    let mut ids: Vec<_> = all.iter().map(|c| c.id).collect();
    let total = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), total, "no duplicates across pages");
    assert_eq!(total, mock::mock_chats().len() + 250);
    // Restarting begins again from the top.
    assert_eq!(api.dialogs(5).await.unwrap().len(), 5);
}

#[test]
fn chat_list_pages_in_on_demand_and_prunes_only_when_complete() {
    use ferrogram::app::DIALOGS_PAGE;
    let mut app = signed_in_app();
    // The mock list is shorter than a page: exhausted, nothing more to load.
    assert!(app.dialogs_exhausted);

    // A full first page: more is available.
    let page: Vec<_> = (0..DIALOGS_PAGE as i64)
        .map(|i| {
            let mut c = ferrogram::model::Chat::new(
                5_000 + i,
                ferrogram::model::ChatKind::User,
                format!("c{i}"),
            );
            c.last_date = 10_000 - i;
            c
        })
        .collect();
    let _ = app.update(Message::Done(Done::Dialogs(Ok(page))));
    assert!(!app.dialogs_exhausted);
    assert_eq!(app.chats.all().len(), DIALOGS_PAGE);

    // The next, short page is appended and ends paging.
    let more: Vec<_> = (0..7_i64)
        .map(|i| {
            let mut c = ferrogram::model::Chat::new(
                9_000 + i,
                ferrogram::model::ChatKind::User,
                format!("old{i}"),
            );
            c.last_date = 100 - i;
            c
        })
        .collect();
    let _ = app.update(Message::Done(Done::MoreDialogs(Ok(more))));
    assert!(app.dialogs_exhausted);
    assert_eq!(app.chats.all().len(), DIALOGS_PAGE + 7);
    assert_eq!(
        app.chats.all().last().unwrap().id,
        9_006,
        "older chats last"
    );

    // A failed page load leaves the list alone and allows a retry.
    let _ = app.update(Message::Done(Done::MoreDialogs(Err(
        ferrogram::error::Error::Net("x".into()),
    ))));
    assert!(!app.dialogs_loading);
    assert_eq!(app.chats.all().len(), DIALOGS_PAGE + 7);
}

// ------------------------------------------------- global search + jump

#[tokio::test]
async fn fake_global_search_spans_chats_newest_first() {
    let api = FakeTelegram::new(FakeOptions {
        logged_in: true,
        ..Default::default()
    });
    let hits = api.search_global("hike".into(), 30).await.unwrap();
    assert!(!hits.is_empty());
    assert!(
        hits.iter()
            .all(|h| h.message.text.to_lowercase().contains("hike"))
    );
    assert!(
        hits.windows(2)
            .all(|w| w[0].message.date >= w[1].message.date)
    );
    assert!(hits.iter().any(|h| h.chat_title == "Alice Johnson"));
    assert!(
        api.search_global("zzzz-no-match".into(), 30)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(api.search_global("hike".into(), 1).await.unwrap().len() <= 1);
}

#[tokio::test]
async fn sidebar_global_search_debounces_dedupes_and_ignores_stale_answers() {
    use ferrogram::model::GlobalHit;
    let mut app = signed_in_app();
    let hit = |chat, id, text: &str| GlobalHit {
        message: ChatMessage::text(chat, id, false, 50, text),
        chat_title: "Far Away".into(),
        chat_kind: ferrogram::model::ChatKind::Group,
        photo_id: None,
    };
    let _ = app.update(Message::Ui(Ui::SidebarSearch("hik".into())));
    // The debounce fires for the settled query only.
    let _ = app.update(Message::Ui(Ui::GlobalSearchDue("hi".into())));
    assert!(!app.global_searching, "stale debounce ignored");
    let _ = app.update(Message::Ui(Ui::GlobalSearchDue("hik".into())));
    assert!(app.global_searching);

    // An answer for an older query is dropped.
    let _ = app.update(Message::Done(Done::GlobalResults {
        query: "hi".into(),
        result: Ok(vec![hit(-777, 5, "old")]),
    }));
    assert!(app.sidebar_global.is_empty());

    // A local (cache) match is not repeated in the global section.
    app.sidebar_messages = vec![ChatMessage::text(ALICE, 144, false, 1, "hike")];
    let _ = app.update(Message::Done(Done::GlobalResults {
        query: "hik".into(),
        result: Ok(vec![hit(ALICE, 144, "hike"), hit(-777, 5, "hike far away")]),
    }));
    assert!(!app.global_searching);
    assert_eq!(app.sidebar_global.len(), 1);
    assert_eq!(app.sidebar_global[0].message.chat_id, -777);

    // Failures keep the local results and show no toast.
    let _ = app.update(Message::Ui(Ui::SidebarSearch("hike".into())));
    let _ = app.update(Message::Done(Done::GlobalResults {
        query: "hike".into(),
        result: Err(ferrogram::error::Error::Net("x".into())),
    }));
    assert!(app.toast.is_none());

    // Clearing the field clears everything.
    let _ = app.update(Message::Ui(Ui::ClearSidebarSearch));
    assert!(app.sidebar_global.is_empty() && !app.global_searching);
}

#[tokio::test]
async fn opening_a_search_hit_loads_its_window_and_detaches() {
    use ferrogram::model::GlobalHit;
    let mut app = signed_in_app();
    // A hit in a chat that is not in the loaded list.
    let far = -777;
    let target = 40;
    let hit = GlobalHit {
        message: ChatMessage::text(far, target, false, 5, "needle"),
        chat_title: "Far Away".into(),
        chat_kind: ferrogram::model::ChatKind::Group,
        photo_id: Some(3),
    };
    app.sidebar_global = vec![hit];
    assert!(app.chats.get(far).is_none());
    let _ = app.update(Message::Ui(Ui::OpenResult(far, target)));
    assert_eq!(app.selected, Some(far));
    assert_eq!(app.chats.get(far).unwrap().title, "Far Away");
    assert_eq!(app.highlight, Some(target));
    let conv = app.conversation.as_ref().unwrap();
    assert!(conv.detached && conv.jump_to == Some(target));

    // A late "newest page" answer (before = None) must not pollute the window.
    let _ = app.update(Message::Done(Done::NetHistory {
        chat: far,
        before: None,
        result: Ok(vec![ChatMessage::text(far, 500, false, 9, "newest")]),
    }));
    assert!(app.conversation.as_ref().unwrap().get(500).is_none());

    // The window around the hit arrives: the jump target is consumed.
    let window: Vec<_> = (20..=65)
        .map(|i| ChatMessage::text(far, i, false, i64::from(i), "m"))
        .collect();
    let _ = app.update(Message::Done(Done::NetHistory {
        chat: far,
        before: Some(target + ferrogram::app::state::JUMP_NEWER),
        result: Ok(window),
    }));
    let conv = app.conversation.as_ref().unwrap();
    assert!(conv.jump_to.is_none() && conv.detached);
    assert!(conv.get(target).is_some());
    assert!(!app.at_bottom);

    // Live messages do not append to the detached window...
    let _ = app.update(Message::Net(Event::NewMessage(ChatMessage::text(
        far, 900, false, 99, "live",
    ))));
    assert!(app.conversation.as_ref().unwrap().get(900).is_none());
    // ...and "jump to latest" reattaches to the newest history.
    let _ = app.update(Message::Ui(Ui::ScrollToBottom));
    let conv = app.conversation.as_ref().unwrap();
    assert!(!conv.detached);
    assert!(app.at_bottom);

    // A hit that vanished server-side falls back to the latest messages.
    let _ = app.update(Message::Ui(Ui::OpenResult(far, 41)));
    assert!(app.conversation.as_ref().unwrap().detached);
    let _ = app.update(Message::Done(Done::NetHistory {
        chat: far,
        before: Some(41 + ferrogram::app::state::JUMP_NEWER),
        result: Ok(vec![ChatMessage::text(far, 10, false, 1, "other")]),
    }));
    assert!(!app.conversation.as_ref().unwrap().detached);
    assert!(
        app.toast
            .as_deref()
            .unwrap_or_default()
            .contains("no longer available")
    );
}

#[tokio::test]
async fn jumping_inside_the_loaded_window_does_not_refetch() {
    let mut app = signed_in_app();
    let _ = app.update(Message::Ui(Ui::SelectChat(ALICE)));
    let _ = app.update(Message::Done(Done::NetHistory {
        chat: ALICE,
        before: None,
        result: Ok(mock::mock_history(ALICE)),
    }));
    let _ = app.update(Message::Ui(Ui::OpenResult(ALICE, 148)));
    let conv = app.conversation.as_ref().unwrap();
    assert!(
        !conv.detached,
        "target already loaded: stay on the live window"
    );
    assert_eq!(app.highlight, Some(148));
}
