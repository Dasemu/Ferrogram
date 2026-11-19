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
use ferrogram::media::AvatarImage;
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
        result: Ok(AvatarImage::decode(&png).unwrap()),
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
            result: Ok(AvatarImage::decode(&png).unwrap()),
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

// -------------------------------------------------------------- drafts

#[tokio::test]
async fn fake_backend_saves_and_clears_drafts() {
    let api = FakeTelegram::new(FakeOptions {
        logged_in: true,
        ..Default::default()
    });
    api.save_draft(ALICE, "later".into(), Some(3))
        .await
        .unwrap();
    let chats = api.dialogs(50).await.unwrap();
    let d = chats
        .iter()
        .find(|c| c.id == ALICE)
        .unwrap()
        .draft
        .clone()
        .unwrap();
    assert_eq!((d.text.as_str(), d.reply_to), ("later", Some(3)));
    api.save_draft(ALICE, String::new(), None).await.unwrap();
    let chats = api.dialogs(50).await.unwrap();
    assert!(
        chats
            .iter()
            .find(|c| c.id == ALICE)
            .unwrap()
            .draft
            .is_none()
    );
}

#[tokio::test]
async fn drafts_follow_the_user_between_chats() {
    let mut app = signed_in_app();
    // The mock account already has a draft in "Book Club" (-304).
    assert_eq!(
        app.chats.get(-304).unwrap().draft.as_ref().unwrap().text,
        "I'll bring the cookies"
    );
    let _ = app.update(Message::Ui(Ui::SelectChat(-304)));
    assert_eq!(app.composer, "I'll bring the cookies", "restored on open");

    // Type something in Alice's chat, leave, and come back.
    let _ = app.update(Message::Ui(Ui::SelectChat(ALICE)));
    assert_eq!(app.composer, "", "no draft there: a clean composer");
    let _ = app.update(Message::Ui(Ui::Reply(148)));
    let _ = app.update(Message::Ui(Ui::ComposerChanged("on my way".into())));
    let _ = app.update(Message::Ui(Ui::SelectChat(-304)));
    let d = app.chats.get(ALICE).unwrap().draft.clone().unwrap();
    assert_eq!(d.text, "on my way");
    assert_eq!(d.reply_to, Some(148));
    assert_eq!(app.composer, "I'll bring the cookies");
    let _ = app.update(Message::Ui(Ui::SelectChat(ALICE)));
    assert_eq!(app.composer, "on my way");
    assert_eq!(app.compose_mode, ComposeMode::Reply(148));

    // Emptying the box deletes the draft when leaving.
    let _ = app.update(Message::Ui(Ui::ComposerChanged(String::new())));
    let _ = app.update(Message::Ui(Ui::CancelCompose));
    let _ = app.update(Message::Ui(Ui::SelectChat(-304)));
    assert!(app.chats.get(ALICE).unwrap().draft.is_none());
}

#[tokio::test]
async fn draft_sync_waits_for_a_pause_in_typing() {
    let mut app = signed_in_app();
    let _ = app.update(Message::Ui(Ui::SelectChat(ALICE)));
    let _ = app.update(Message::Ui(Ui::ComposerChanged("a".into())));
    let first = app.draft_version;
    let _ = app.update(Message::Ui(Ui::ComposerChanged("ab".into())));
    assert!(app.draft_version > first);
    // The timer from the first keystroke is stale: nothing saved yet.
    let _ = app.update(Message::Ui(Ui::DraftDue(ALICE, first)));
    assert!(app.chats.get(ALICE).unwrap().draft.is_none());
    // The latest timer saves it.
    let _ = app.update(Message::Ui(Ui::DraftDue(ALICE, app.draft_version)));
    assert_eq!(
        app.chats.get(ALICE).unwrap().draft.as_ref().unwrap().text,
        "ab"
    );
    // A timer for a chat that is no longer open does nothing.
    let _ = app.update(Message::Ui(Ui::ComposerChanged("abc".into())));
    let v = app.draft_version;
    let _ = app.update(Message::Ui(Ui::SelectChat(-304)));
    let _ = app.update(Message::Ui(Ui::DraftDue(ALICE, v)));
    assert_eq!(app.composer, "I'll bring the cookies");
}

#[tokio::test]
async fn sending_consumes_the_draft_and_edits_keep_it() {
    let mut app = signed_in_app();
    let _ = app.update(Message::Ui(Ui::SelectChat(ALICE)));
    let _ = app.update(Message::Done(Done::NetHistory {
        chat: ALICE,
        before: None,
        result: Ok(mock::mock_history(ALICE)),
    }));
    let _ = app.update(Message::Ui(Ui::ComposerChanged("draft text".into())));
    let _ = app.update(Message::Ui(Ui::DraftDue(ALICE, app.draft_version)));
    assert!(app.chats.get(ALICE).unwrap().draft.is_some());

    // Editing a message stashes the draft and gets it back afterwards.
    let _ = app.update(Message::Ui(Ui::Edit(149)));
    assert_eq!(app.composer, "You're the best!");
    let _ = app.update(Message::Ui(Ui::ComposerChanged("You're the best!!".into())));
    let _ = app.update(Message::Ui(Ui::DraftDue(ALICE, app.draft_version)));
    assert_eq!(
        app.chats.get(ALICE).unwrap().draft.as_ref().unwrap().text,
        "draft text",
        "edit text is never saved as the draft"
    );
    let _ = app.update(Message::Ui(Ui::CancelCompose));
    assert_eq!(app.composer, "draft text");
    assert_eq!(app.compose_mode, ComposeMode::Normal);

    let _ = app.update(Message::Ui(Ui::Send));
    assert!(app.chats.get(ALICE).unwrap().draft.is_none());
    assert!(app.composer.is_empty());
}

#[tokio::test]
async fn drafts_from_other_devices_do_not_clobber_typing() {
    use ferrogram::model::Draft;
    let mut app = signed_in_app();
    let remote = |text: &str| {
        Message::Net(Event::Draft {
            chat: ALICE,
            draft: Some(Draft {
                text: text.into(),
                reply_to: None,
                date: 10,
            }),
        })
    };
    // Not the open chat: only the list changes.
    let _ = app.update(remote("from phone"));
    assert_eq!(
        app.chats.get(ALICE).unwrap().draft.as_ref().unwrap().text,
        "from phone"
    );
    // Open chat with an empty composer: the draft appears (like the official client).
    let _ = app.update(Message::Ui(Ui::SelectChat(ALICE)));
    assert_eq!(app.composer, "from phone");
    let _ = app.update(Message::Ui(Ui::ComposerChanged(String::new())));
    let _ = app.update(remote("newer from phone"));
    assert_eq!(app.composer, "newer from phone");
    // While the user is typing it is left alone.
    let _ = app.update(Message::Ui(Ui::ComposerChanged("mine".into())));
    let _ = app.update(remote("another"));
    assert_eq!(app.composer, "mine");
    // Cleared remotely.
    let _ = app.update(Message::Ui(Ui::ComposerChanged(String::new())));
    let _ = app.update(Message::Net(Event::Draft {
        chat: ALICE,
        draft: None,
    }));
    assert!(app.chats.get(ALICE).unwrap().draft.is_none());
}

fn wait_until_gone(path: &std::path::Path) -> bool {
    for _ in 0..100 {
        if !path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn pasted_image_gets_the_attach_preview_and_caption_flow() {
    use ferrogram::clipboard;
    use ferrogram::net::UploadKind;
    let dir = tempfile::tempdir().unwrap();
    let mut app = signed_in_app();
    app.config.cache_dir = dir.path().to_path_buf();
    let _ = app.update(Message::Ui(Ui::SelectChat(ALICE)));
    let png = mock::mock_photo_png(3, 32, 32);
    let path = clipboard::save_pasted(&app.config.paste_dir(), &png).unwrap();

    // Clipboard task finished: file is written, then stat'ed like a picked file.
    let _ = app.update(Message::Done(Done::PasteSaved(Ok(path.clone()))));
    let _ = app.update(Message::Done(Done::FileStat {
        path: path.clone(),
        result: Ok((png.len() as u64, false)),
    }));
    let f = app.pending_file.clone().expect("preview shown");
    assert!(f.temp, "pasted images are temporary files");
    assert_eq!(
        f.kind,
        UploadKind::Photo,
        "sent as a photo, like the picker"
    );

    // Caption flow: the composer text becomes the caption.
    let _ = app.update(Message::Ui(Ui::ComposerChanged("look at this".into())));
    let _ = app.update(Message::Ui(Ui::SendPendingFile));
    assert!(app.pending_file.is_none());
    assert!(app.composer.is_empty());
    let last = app
        .conversation
        .as_ref()
        .unwrap()
        .messages()
        .last()
        .unwrap()
        .clone();
    assert_eq!(last.text, "look at this");
    assert_eq!(last.state, SendState::Sending);

    // Cancelling the preview deletes the temp file.
    let path2 = clipboard::save_pasted(&app.config.paste_dir(), &png).unwrap();
    let _ = app.update(Message::Done(Done::FileStat {
        path: path2.clone(),
        result: Ok((png.len() as u64, false)),
    }));
    assert!(app.pending_file.is_some());
    let _ = app.update(Message::Ui(Ui::Escape));
    assert!(app.pending_file.is_none());
    assert!(wait_until_gone(&path2), "temp image removed on cancel");
}

#[test]
fn pasting_a_copied_file_removes_the_path_text_the_field_inserted() {
    use ferrogram::clipboard::Clip;
    let mut app = signed_in_app();
    let _ = app.update(Message::Ui(Ui::SelectChat(ALICE)));
    // The composer already took the text form of the clipboard.
    let _ = app.update(Message::Ui(Ui::ComposerChanged(
        "hi file:///tmp/cat.jpg".into(),
    )));
    let _ = app.update(Message::Ui(Ui::TextPasted("file:///tmp/cat.jpg".into())));
    let _ = app.update(Message::Done(Done::Pasted(Clip::Files(vec![
        "/tmp/cat.jpg".into(),
        "/tmp/dog.jpg".into(),
    ]))));
    assert_eq!(app.composer, "hi ");
    assert!(
        app.toast.as_deref().unwrap().contains("first"),
        "extra files are reported"
    );
}

#[test]
fn pasting_plain_text_or_without_a_chat_changes_nothing() {
    use ferrogram::clipboard::Clip;
    let mut app = signed_in_app();
    // No chat open: nothing to attach to.
    let _ = app.update(Message::Done(Done::Pasted(Clip::Image(vec![1, 2, 3]))));
    assert!(app.pending_file.is_none());
    let _ = app.update(Message::Ui(Ui::SelectChat(ALICE)));
    let _ = app.update(Message::Ui(Ui::ComposerChanged("keep".into())));
    let _ = app.update(Message::Ui(Ui::TextPasted("keep".into())));
    let _ = app.update(Message::Done(Done::Pasted(Clip::Nothing)));
    assert_eq!(app.composer, "keep", "text paste stays with the text field");
    assert!(app.pending_file.is_none());
}

#[test]
fn stale_text_paste_is_not_stripped_by_a_later_image_paste() {
    use ferrogram::clipboard::Clip;
    let mut app = signed_in_app();
    app.config.cache_dir = tempfile::tempdir().unwrap().keep();
    let _ = app.update(Message::Ui(Ui::SelectChat(ALICE)));
    let _ = app.update(Message::Ui(Ui::ComposerChanged("abc".into())));
    app.last_paste = Some(("abc".into(), 0)); // pasted long ago
    let _ = app.update(Message::Done(Done::Pasted(Clip::Image(
        mock::mock_photo_png(1, 8, 8),
    ))));
    assert_eq!(app.composer, "abc");
}

#[test]
fn copy_image_closes_the_menu() {
    let mut app = signed_in_app();
    let _ = app.update(Message::Ui(Ui::SelectChat(ALICE)));
    let _ = app.update(Message::Ui(Ui::MessageMenu(Some(143))));
    assert_eq!(app.menu_for, Some(143));
    let _ = app.update(Message::Ui(Ui::CopyImage(ALICE, 143)));
    assert_eq!(app.menu_for, None);
    let _ = app.update(Message::Done(Done::ImageCopied(Ok(()))));
    assert_eq!(app.toast.as_deref(), Some("Image copied."));
}

// ------------------------------------------------------------------ folders

use ferrogram::app::folders::{EditorFlag, Picker};
use ferrogram::model::Folder;

fn app_with_folders() -> App {
    let mut app = signed_in_app();
    let _ = app.update(Message::Done(Done::Folders(Ok(mock::mock_folders()))));
    app
}

fn visible_ids(app: &App) -> Vec<i64> {
    app.visible_chats().iter().map(|c| c.id).collect()
}

#[test]
fn folder_tabs_filter_the_chat_list() {
    let mut app = app_with_folders();
    assert_eq!(app.folders.ids(), vec![2, 3, 4]);
    let all = visible_ids(&app);
    assert!(all.len() > 10, "All chats shows everything");

    // Work: groups + Echo Bot, without muted chats.
    let _ = app.update(Message::Ui(Ui::SelectFolder(Some(2))));
    let work = visible_ids(&app);
    assert!(work.contains(&ECHO_BOT) && work.contains(&mock::RUST_GROUP));
    assert!(
        !work.contains(&mock::DEV_CHANNEL),
        "channels are not in Work"
    );
    assert!(!work.contains(&ALICE));
    // Muted groups are excluded by the flag ("Weekend Hiking" is muted).
    assert!(
        app.chats
            .all()
            .iter()
            .any(|c| c.muted && c.kind == ferrogram::model::ChatKind::Group)
    );
    assert!(
        work.iter()
            .all(|id| !app.chats.get(*id).unwrap().muted || *id == ECHO_BOT)
    );

    // People: contacts and non-contacts, but not the Echo Bot (never list).
    let _ = app.update(Message::Ui(Ui::SelectFolder(Some(3))));
    let people = visible_ids(&app);
    assert!(people.contains(&ALICE));
    assert!(!people.contains(&ECHO_BOT));
    assert!(people.iter().all(|id| *id > 0));

    // Back to everything; unknown folder ids fall back to All chats.
    let _ = app.update(Message::Ui(Ui::SelectFolder(None)));
    assert_eq!(visible_ids(&app), all);
    let _ = app.update(Message::Ui(Ui::SelectFolder(Some(99))));
    assert_eq!(app.folders.selected, None);
}

#[test]
fn folder_updates_from_other_devices_are_applied() {
    let mut app = app_with_folders();
    let _ = app.update(Message::Ui(Ui::SelectFolder(Some(4))));
    let mut f = Folder::new(6, "Fresh");
    f.bots = true;
    let _ = app.update(Message::Net(Event::FolderChanged {
        id: 6,
        folder: Some(f),
    }));
    assert_eq!(app.folders.ids(), vec![2, 3, 4, 6]);
    let _ = app.update(Message::Net(Event::FoldersOrder(vec![0, 6, 2, 3, 4])));
    assert_eq!(app.folders.ids(), vec![6, 2, 3, 4]);
    // The open folder is deleted elsewhere: back to All chats.
    let _ = app.update(Message::Net(Event::FolderChanged {
        id: 4,
        folder: None,
    }));
    assert_eq!(app.folders.selected, None);
    assert_eq!(app.folders.ids(), vec![6, 2, 3]);
    // A stale marker asks for a refetch but changes nothing by itself.
    let _ = app.update(Message::Net(Event::FoldersStale));
    assert_eq!(app.folders.ids(), vec![6, 2, 3]);
}

#[test]
fn add_to_folder_is_optimistic_and_rolls_back_on_refusal() {
    let mut app = app_with_folders();
    let _ = app.update(Message::Ui(Ui::ChatMenu(Some(ALICE))));
    assert_eq!(app.folders.chat_menu, Some(ALICE));
    let before = app.folders.list.clone();
    // Alice is a contact, not in Work: toggling includes her.
    let _ = app.update(Message::Ui(Ui::ChatInFolder(ALICE, 2)));
    assert_eq!(app.folders.chat_menu, None, "menu closes");
    assert!(app.folders.get(2).unwrap().include.contains(&ALICE));
    // The server said no: the list goes back and the user is told.
    let _ = app.update(Message::Done(Done::FolderOp {
        before: before.clone(),
        result: Err(ferrogram::error::Error::Protocol(
            "400 DIALOG_FILTERS_TOO_MUCH".into(),
        )),
    }));
    assert_eq!(app.folders.list, before);
    assert!(
        app.toast
            .as_deref()
            .unwrap()
            .contains("maximum number of folders")
    );
    // Toggling again from the menu of a shown chat removes it.
    let _ = app.update(Message::Ui(Ui::ChatInFolder(ECHO_BOT, 2)));
    assert!(!app.folders.get(2).unwrap().include.contains(&ECHO_BOT));
}

#[test]
fn deleting_and_reordering_folders() {
    let mut app = app_with_folders();
    let _ = app.update(Message::Ui(Ui::OpenFolders));
    assert!(app.folders.manager_open);
    let _ = app.update(Message::Ui(Ui::FolderMove(4, -1)));
    assert_eq!(app.folders.ids(), vec![2, 4, 3]);
    let _ = app.update(Message::Ui(Ui::FolderMove(2, -1)));
    assert_eq!(app.folders.ids(), vec![2, 4, 3], "first folder stays");
    // Delete asks first.
    let _ = app.update(Message::Ui(Ui::FolderDeleteAsk(4)));
    assert_eq!(app.folders.confirm_delete, Some(4));
    assert_eq!(app.folders.ids(), vec![2, 4, 3]);
    let _ = app.update(Message::Ui(Ui::FolderDeleteCancel));
    assert_eq!(app.folders.confirm_delete, None);
    let _ = app.update(Message::Ui(Ui::FolderDeleteConfirm(4)));
    assert_eq!(app.folders.ids(), vec![2, 3]);
    // A refused delete brings the folder back.
    let before = vec![Folder::new(2, "a"), Folder::new(4, "b")];
    let _ = app.update(Message::Done(Done::FolderOp {
        before: before.clone(),
        result: Err(ferrogram::error::Error::Net("x".into())),
    }));
    assert_eq!(app.folders.list, before);
    // Esc closes the manager.
    let _ = app.update(Message::Ui(Ui::Escape));
    assert!(!app.folders.manager_open);
}

#[test]
fn folder_editor_create_validate_save() {
    let mut app = app_with_folders();
    let _ = app.update(Message::Ui(Ui::FolderNew(Some(ALICE))));
    let ed = app.folder_editor.as_ref().unwrap();
    assert!(ed.is_new);
    assert_eq!(ed.folder.id, 5, "lowest free id");
    assert!(ed.contains(Picker::Include, ALICE), "from the chat menu");

    // A name is required.
    let _ = app.update(Message::Ui(Ui::FolderSave));
    assert!(
        app.folder_editor
            .as_ref()
            .unwrap()
            .error
            .as_deref()
            .unwrap()
            .contains("name")
    );
    assert!(!app.folder_editor.as_ref().unwrap().saving);

    let _ = app.update(Message::Ui(Ui::FolderTitle("Friends and family".into())));
    assert_eq!(
        app.folder_editor
            .as_ref()
            .unwrap()
            .folder
            .title
            .chars()
            .count(),
        12
    );
    let _ = app.update(Message::Ui(Ui::FolderFlag(EditorFlag::Groups)));
    let _ = app.update(Message::Ui(Ui::FolderPick(Some(Picker::Exclude))));
    let _ = app.update(Message::Ui(Ui::FolderChatToggle(
        Picker::Exclude,
        mock::RUST_GROUP,
    )));
    let _ = app.update(Message::Ui(Ui::FolderPick(None)));
    // Esc inside the picker closes only the picker.
    let _ = app.update(Message::Ui(Ui::FolderPick(Some(Picker::Include))));
    let _ = app.update(Message::Ui(Ui::Escape));
    assert!(app.folder_editor.as_ref().unwrap().picker.is_none());
    assert!(app.folder_editor.is_some());

    let _ = app.update(Message::Ui(Ui::FolderSave));
    assert!(
        app.folder_editor.as_ref().unwrap().saving,
        "waits for the server"
    );
    let saved = app.folder_editor.as_ref().unwrap().to_save();
    assert!(saved.exclude.contains(&mock::RUST_GROUP));
    // The server refuses (too many folders): the editor stays open with the reason.
    let _ = app.update(Message::Done(Done::FolderSaved {
        folder: saved.clone(),
        result: Err(ferrogram::error::Error::Protocol(
            "400 DIALOG_FILTERS_TOO_MUCH".into(),
        )),
    }));
    let ed = app.folder_editor.as_ref().unwrap();
    assert!(!ed.saving);
    assert!(
        ed.error
            .as_deref()
            .unwrap()
            .contains("maximum number of folders")
    );
    assert_eq!(app.folders.ids(), vec![2, 3, 4]);
    // Retry succeeds: the folder is in the list and the editor closed.
    let _ = app.update(Message::Ui(Ui::FolderSave));
    let _ = app.update(Message::Done(Done::FolderSaved {
        folder: saved.clone(),
        result: Ok(()),
    }));
    assert!(app.folder_editor.is_none());
    assert_eq!(app.folders.ids(), vec![2, 3, 4, 5]);
    assert_eq!(app.folders.get(5).unwrap().title, "Friends and");

    // Editing an existing folder starts from its current values.
    let _ = app.update(Message::Ui(Ui::FolderEdit(5)));
    let ed = app.folder_editor.as_ref().unwrap();
    assert!(!ed.is_new);
    assert!(ed.flag(EditorFlag::Groups));
    let _ = app.update(Message::Ui(Ui::Escape));
    assert!(app.folder_editor.is_none());
}

#[test]
fn shared_folders_are_not_edited_here() {
    let mut app = signed_in_app();
    let mut shared = Folder::new(7, "Shared");
    shared.shared = true;
    shared.include = vec![mock::DEV_CHANNEL];
    let _ = app.update(Message::Done(Done::Folders(Ok(vec![shared]))));
    let _ = app.update(Message::Ui(Ui::FolderEdit(7)));
    assert!(app.folder_editor.is_none());
    assert!(app.toast.as_deref().unwrap().contains("official app"));
    let before = app.folders.list.clone();
    let _ = app.update(Message::Ui(Ui::ChatInFolder(ALICE, 7)));
    assert_eq!(app.folders.list, before);
}

#[tokio::test]
async fn fake_backend_folder_roundtrip_with_events() {
    use ferrogram::error::Error;
    let fake = FakeTelegram::new(FakeOptions {
        logged_in: true,
        live_simulation: false,
        ..Default::default()
    });
    let mut events = fake.events();
    let list = fake.dialog_filters().await.unwrap();
    assert_eq!(list.iter().map(|f| f.id).collect::<Vec<_>>(), vec![2, 3, 4]);

    let mut f = Folder::new(5, "Trips");
    f.include = vec![ALICE];
    fake.save_dialog_filter(f.clone()).await.unwrap();
    assert!(matches!(
        events.next().await,
        Some(Event::FolderChanged { id: 5, folder: Some(ref g) }) if g.title == "Trips"
    ));
    assert_eq!(fake.dialog_filters().await.unwrap().len(), 4);

    // The server validates like Telegram does.
    let empty = Folder::new(6, "Empty");
    assert!(matches!(
        fake.save_dialog_filter(empty).await,
        Err(Error::Protocol(e)) if e.contains("FILTER_INCLUDE_EMPTY")
    ));
    assert!(matches!(
        fake.save_dialog_filter(Folder::new(6, " ")).await,
        Err(Error::Protocol(e)) if e.contains("FILTER_TITLE_EMPTY")
    ));

    fake.reorder_dialog_filters(vec![5, 4, 3, 2]).await.unwrap();
    assert!(matches!(events.next().await, Some(Event::FoldersOrder(o)) if o == vec![5, 4, 3, 2]));
    let ids: Vec<_> = fake
        .dialog_filters()
        .await
        .unwrap()
        .iter()
        .map(|f| f.id)
        .collect();
    assert_eq!(ids, vec![5, 4, 3, 2]);

    fake.delete_dialog_filter(3).await.unwrap();
    assert!(matches!(
        events.next().await,
        Some(Event::FolderChanged {
            id: 3,
            folder: None
        })
    ));
    assert_eq!(fake.dialog_filters().await.unwrap().len(), 3);

    // The folder limit.
    for i in 0..10 {
        let mut g = Folder::new(20 + i, format!("F{i}"));
        g.bots = true;
        if fake.save_dialog_filter(g).await.is_err() {
            break;
        }
    }
    let mut over = Folder::new(99, "Over");
    over.bots = true;
    assert!(matches!(
        fake.save_dialog_filter(over).await,
        Err(Error::Protocol(e)) if e.contains("DIALOG_FILTERS_TOO_MUCH")
    ));
}

// -------------------------------------------------------------- info panel

use ferrogram::app::info::PAGE as INFO_PAGE;
use ferrogram::model::SharedKind;

fn app_with_info(chat: i64) -> App {
    let mut app = signed_in_app();
    let _ = app.update(Message::Ui(Ui::SelectChat(chat)));
    let _ = app.update(Message::Ui(Ui::ToggleInfo));
    app
}

fn shared_page(chat: i64, kind: SharedKind, offset: i32) -> ferrogram::model::SharedPage {
    let all = mock::mock_shared(chat, kind);
    let older: Vec<_> = all
        .into_iter()
        .filter(|i| offset <= 0 || i.id < offset)
        .collect();
    let items: Vec<_> = older.iter().take(INFO_PAGE).cloned().collect();
    let next = (older.len() > INFO_PAGE)
        .then(|| items.last().map(|i| i.id))
        .flatten();
    ferrogram::model::SharedPage { items, next }
}

#[test]
fn info_panel_opens_for_the_open_chat_and_closes_with_esc() {
    let mut app = signed_in_app();
    let _ = app.update(Message::Ui(Ui::ToggleInfo));
    assert!(app.info.is_none(), "needs an open chat");
    let _ = app.update(Message::Ui(Ui::SelectChat(ALICE)));
    let _ = app.update(Message::Ui(Ui::ToggleInfo));
    let p = app.info.as_ref().unwrap();
    assert_eq!((p.chat, p.tab), (ALICE, SharedKind::Media));
    assert!(
        p.current().unwrap().loading(),
        "first page requested at once"
    );
    let _ = app.update(Message::Ui(Ui::Escape));
    assert!(app.info.is_none());
    let _ = app.update(Message::Ui(Ui::ToggleInfo));
    assert!(app.info.is_some());
    let _ = app.update(Message::Ui(Ui::ToggleInfo));
    assert!(app.info.is_none(), "header click toggles");
}

#[test]
fn profile_info_arrives_or_fails_quietly() {
    let mut app = app_with_info(ALICE);
    let _ = app.update(Message::Done(Done::PeerInfo {
        chat: ALICE,
        result: Ok(mock::mock_peer_info(ALICE)),
    }));
    let info = app.info.as_ref().unwrap().info.clone().unwrap();
    assert_eq!(info.username.as_deref(), Some("alice_j"));
    assert!(info.phone.is_some());
    // An answer for another chat is dropped.
    let _ = app.update(Message::Done(Done::PeerInfo {
        chat: mock::RUST_GROUP,
        result: Ok(mock::mock_peer_info(mock::RUST_GROUP)),
    }));
    assert_eq!(
        app.info.as_ref().unwrap().info.as_ref().unwrap().members,
        None
    );
    // A failure keeps the panel and shows no toast.
    let mut app = app_with_info(ALICE);
    let _ = app.update(Message::Done(Done::PeerInfo {
        chat: ALICE,
        result: Err(ferrogram::error::Error::Net("x".into())),
    }));
    assert!(app.info.as_ref().unwrap().info_failed);
    assert!(app.toast.is_none());
}

#[test]
fn shared_tabs_page_lazily_and_queue_bounded_thumbnails() {
    let mut app = app_with_info(ALICE);
    let first = shared_page(ALICE, SharedKind::Media, 0);
    assert_eq!(first.items.len(), INFO_PAGE);
    let _ = app.update(Message::Done(Done::SharedPage {
        chat: ALICE,
        kind: SharedKind::Media,
        offset: 0,
        result: Ok(first.clone()),
    }));
    let tab = app.info.as_ref().unwrap().tab(SharedKind::Media);
    assert_eq!(tab.items.len(), INFO_PAGE);
    assert!(tab.next.is_some() && !tab.loading());
    // Thumbnails: only a few at a time, the rest wait in the queue.
    assert_eq!(app.thumb_q.pending(), INFO_PAGE);

    // Near the end of the panel the next page is requested (same path as scrolling).
    let _ = app.update(Message::Ui(Ui::InfoTab(SharedKind::Media)));
    assert!(app.info.as_ref().unwrap().tab(SharedKind::Media).loading());
    // Asking again while it is in flight does not duplicate the request.
    let _ = app.update(Message::Ui(Ui::InfoTab(SharedKind::Media)));
    let offset = first.next.unwrap();
    let second = shared_page(ALICE, SharedKind::Media, offset);
    let _ = app.update(Message::Done(Done::SharedPage {
        chat: ALICE,
        kind: SharedKind::Media,
        offset,
        result: Ok(second),
    }));
    let tab = app.info.as_ref().unwrap().tab(SharedKind::Media);
    assert_eq!(tab.items.len(), 64);
    assert!(tab.ended());

    // Another tab loads independently; an empty tab says so.
    let _ = app.update(Message::Ui(Ui::InfoTab(SharedKind::Voice)));
    assert!(app.info.as_ref().unwrap().tab(SharedKind::Voice).loading());
    let page = shared_page(ALICE, SharedKind::Voice, 0);
    assert_eq!(page.items.len(), 7);
    let _ = app.update(Message::Done(Done::SharedPage {
        chat: ALICE,
        kind: SharedKind::Voice,
        offset: 0,
        result: Ok(page),
    }));
    assert!(app.info.as_ref().unwrap().tab(SharedKind::Voice).ended());
}

#[test]
fn failed_pages_show_retry_and_stale_pages_are_ignored() {
    let mut app = app_with_info(ALICE);
    let _ = app.update(Message::Done(Done::SharedPage {
        chat: ALICE,
        kind: SharedKind::Media,
        offset: 0,
        result: Err(ferrogram::error::Error::Net("x".into())),
    }));
    let t = app.info.as_ref().unwrap().tab(SharedKind::Media);
    assert!(t.failed && !t.loading());
    assert!(app.toast.is_none());
    // Selecting the tab again retries.
    let _ = app.update(Message::Ui(Ui::InfoTab(SharedKind::Media)));
    assert!(app.info.as_ref().unwrap().tab(SharedKind::Media).loading());
    // A page for a chat that is no longer shown is dropped.
    let _ = app.update(Message::Ui(Ui::SelectChat(mock::RUST_GROUP)));
    assert_eq!(
        app.info.as_ref().unwrap().chat,
        mock::RUST_GROUP,
        "panel follows the chat"
    );
    assert!(app.info.as_ref().unwrap().current().unwrap().loading());
    let _ = app.update(Message::Done(Done::SharedPage {
        chat: ALICE,
        kind: SharedKind::Media,
        offset: 0,
        result: Ok(shared_page(ALICE, SharedKind::Media, 0)),
    }));
    assert!(
        app.info
            .as_ref()
            .unwrap()
            .tab(SharedKind::Media)
            .items
            .is_empty()
    );
    assert_eq!(app.thumb_q.pending(), 0);
}

#[test]
fn thumbnail_cache_is_bounded_and_rejects_bad_images() {
    let mut app = app_with_info(ALICE);
    let png = mock::mock_photo_png(1, 160, 120);
    let _ = app.update(Message::Done(Done::Thumb {
        key: (ALICE, 5),
        result: Ok(png.clone()),
    }));
    assert!(app.thumbs.peek(&(ALICE, 5)).is_some());
    // Garbage and another chat's thumbnail are not cached.
    let _ = app.update(Message::Done(Done::Thumb {
        key: (ALICE, 6),
        result: Ok(vec![0u8; 3 * 1024 * 1024]),
    }));
    assert!(app.thumbs.peek(&(ALICE, 6)).is_none());
    let _ = app.update(Message::Done(Done::Thumb {
        key: (mock::RUST_GROUP, 7),
        result: Ok(png.clone()),
    }));
    assert!(app.thumbs.peek(&(mock::RUST_GROUP, 7)).is_none());
    // Many thumbnails never exceed the budget.
    for i in 0..3000 {
        let _ = app.update(Message::Done(Done::Thumb {
            key: (ALICE, 1000 + i),
            result: Ok(png.clone()),
        }));
    }
    assert!(app.thumbs.bytes() <= ferrogram::app::info::THUMB_BUDGET + png.len());
    // Closing the panel frees them.
    let _ = app.update(Message::Ui(Ui::ToggleInfo));
    assert!(app.thumbs.is_empty());
}

#[test]
fn notifications_switch_is_optimistic_and_rolls_back() {
    let mut app = app_with_info(ALICE);
    assert!(!app.chats.get(ALICE).unwrap().muted);
    let _ = app.update(Message::Ui(Ui::SetNotifications(false)));
    assert!(app.chats.get(ALICE).unwrap().muted);
    let _ = app.update(Message::Done(Done::MuteDone {
        chat: ALICE,
        previous: false,
        result: Err(ferrogram::error::Error::Net("x".into())),
    }));
    assert!(
        !app.chats.get(ALICE).unwrap().muted,
        "refused: back to unmuted"
    );
    assert!(app.toast.is_some());
    // Another device mutes: the switch follows.
    let _ = app.update(Message::Net(Event::Muted {
        chat: ALICE,
        muted: true,
    }));
    assert!(app.chats.get(ALICE).unwrap().muted);
    let _ = app.update(Message::Ui(Ui::SetNotifications(true)));
    assert!(!app.chats.get(ALICE).unwrap().muted);
}

#[test]
fn photos_open_the_viewer_and_link_failures_are_reported() {
    let mut app = app_with_info(ALICE);
    let _ = app.update(Message::Ui(Ui::OpenMedia(ALICE, 2_700)));
    assert_eq!(app.viewer, Some((ALICE, 2_700)));
    assert!(
        app.photo_loading.contains(&(ALICE, 2_700)),
        "viewer fetches the photo"
    );
    let _ = app.update(Message::Done(Done::LinkOpened(Err(
        ferrogram::error::Error::Protocol("not a web link".into()),
    ))));
    assert!(app.toast.as_deref().unwrap().contains("link"));
}

#[tokio::test]
async fn fake_backend_serves_shared_media_info_and_mute() {
    let fake = FakeTelegram::new(FakeOptions {
        logged_in: true,
        live_simulation: false,
        ..Default::default()
    });
    let mut events = fake.events();
    let p1 = fake
        .shared_media(ALICE, SharedKind::Media, 0, 40)
        .await
        .unwrap();
    assert_eq!(p1.items.len(), 40);
    let p2 = fake
        .shared_media(ALICE, SharedKind::Media, p1.next.unwrap(), 40)
        .await
        .unwrap();
    assert_eq!(p2.items.len(), 24);
    assert_eq!(p2.next, None);
    assert!(
        p1.items
            .iter()
            .all(|i| p2.items.iter().all(|j| j.id != i.id))
    );
    assert!(
        p1.items.windows(2).all(|w| w[0].id > w[1].id),
        "newest first"
    );
    let none = fake
        .shared_media(mock::ECHO_BOT, SharedKind::Files, 0, 40)
        .await
        .unwrap();
    assert!(none.items.is_empty() && none.next.is_none());

    let info = fake.peer_info(mock::RUST_GROUP).await.unwrap();
    assert_eq!(info.members, Some(4_721));
    assert!(
        fake.download_thumb(ALICE, p1.items[0].id)
            .await
            .unwrap()
            .len()
            > 100
    );

    fake.set_muted(ALICE, true).await.unwrap();
    assert!(matches!(
        events.next().await,
        Some(Event::Muted {
            chat: ALICE,
            muted: true
        })
    ));
    assert!(fake.calls().contains(&"mute".to_owned()));
}
