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
