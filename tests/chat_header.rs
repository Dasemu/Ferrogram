//! Chat header (round 4): in-chat search, the ⋮ menu actions and muted
//! chats, driven through the real `App::update` and the `FakeTelegram`
//! backend. Tasks are executed (not just returned), so the fake server's
//! state proves what was sent.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use ferrogram::app::chatmenu::{Confirm, Dir, HeaderMenu};
use ferrogram::app::{App, Done, Message, Ui};
use ferrogram::config::Config;
use ferrogram::mock::{self, ALICE, DEV_CHANNEL, FakeOptions, FakeTelegram, RUST_GROUP};
use ferrogram::model::{
    MUTE_FOREVER, MuteFor, NotifyDefaults, NotifyScope, NotifySettings, NotifyTarget,
};
use ferrogram::net::{ApiHandle, Event};
use futures::StreamExt;
use iced_runtime::Action;

/// Feed `first` to the app and keep feeding it the messages its tasks
/// produce, until nothing more comes out (bounded).
async fn run(app: &mut App, first: Message) {
    let mut queue = vec![first];
    let mut steps = 0;
    while let Some(m) = queue.pop() {
        steps += 1;
        assert!(steps < 400, "message loop does not settle");
        let task = app.update(m);
        let Some(mut stream) = iced_runtime::task::into_stream(task) else {
            continue;
        };
        while let Ok(Some(action)) =
            tokio::time::timeout(Duration::from_secs(2), stream.next()).await
        {
            if let Action::Output(m) = action {
                queue.push(m);
            }
        }
    }
}

async fn ui(app: &mut App, u: Ui) {
    run(app, Message::Ui(u)).await;
}

async fn signed_in() -> (App, Arc<FakeTelegram>) {
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
    (app, fake)
}

async fn open(app: &mut App, chat: i64) {
    ui(app, Ui::SelectChat(chat)).await;
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

// ------------------------------------------------------------------ header

#[tokio::test]
async fn header_menu_opens_closes_and_pages() {
    let (mut app, _fake) = signed_in().await;
    ui(&mut app, Ui::ToggleHeaderMenu).await;
    assert!(app.header_menu.is_none(), "needs an open chat");
    open(&mut app, ALICE).await;
    ui(&mut app, Ui::ToggleHeaderMenu).await;
    assert_eq!(app.header_menu, Some(HeaderMenu::Main));
    ui(&mut app, Ui::HeaderMenuPage(HeaderMenu::Mute)).await;
    assert_eq!(app.header_menu, Some(HeaderMenu::Mute));
    ui(&mut app, Ui::Escape).await;
    assert!(app.header_menu.is_none(), "Esc closes the menu first");
    ui(&mut app, Ui::ToggleHeaderMenu).await;
    ui(&mut app, Ui::ToggleHeaderMenu).await;
    assert!(app.header_menu.is_none(), "the button toggles it");
    // Switching chats closes it.
    ui(&mut app, Ui::ToggleHeaderMenu).await;
    open(&mut app, RUST_GROUP).await;
    assert!(app.header_menu.is_none());
}

#[tokio::test]
async fn view_profile_toggles_the_info_panel() {
    let (mut app, _fake) = signed_in().await;
    open(&mut app, ALICE).await;
    ui(&mut app, Ui::ToggleHeaderMenu).await;
    ui(&mut app, Ui::ToggleInfo).await;
    assert!(app.info.is_some());
    assert!(
        app.header_menu.is_none(),
        "choosing an entry closes the menu"
    );
    ui(&mut app, Ui::ToggleHeaderMenu).await;
    ui(&mut app, Ui::ToggleInfo).await;
    assert!(app.info.is_none());
}

// -------------------------------------------------------------------- mute

#[tokio::test]
async fn mute_durations_sync_to_the_server_and_unmute_clears() {
    let (mut app, fake) = signed_in().await;
    open(&mut app, ALICE).await;
    assert!(!app.chats.get(ALICE).unwrap().muted);

    let before = now();
    ui(&mut app, Ui::SetMute(MuteFor::Hour)).await;
    let until = fake.notify_of(ALICE).unwrap().mute_until.unwrap();
    assert!((before + 3600..=now() + 3600).contains(&until), "1 hour");
    assert!(app.chats.get(ALICE).unwrap().muted);
    assert_eq!(app.chats.get(ALICE).unwrap().notify.mute_until, Some(until));

    ui(&mut app, Ui::SetMute(MuteFor::Week)).await;
    let until = fake.notify_of(ALICE).unwrap().mute_until.unwrap();
    assert!(until >= before + 7 * 86_400);

    ui(&mut app, Ui::SetMute(MuteFor::Forever)).await;
    assert_eq!(
        fake.notify_of(ALICE).unwrap().mute_until,
        Some(MUTE_FOREVER)
    );

    ui(&mut app, Ui::SetMute(MuteFor::Off)).await;
    assert_eq!(fake.notify_of(ALICE).unwrap().mute_until, Some(0));
    assert!(!app.chats.get(ALICE).unwrap().muted);
    assert!(fake.calls().contains(&"unmute".to_owned()));
}

#[tokio::test]
async fn a_refused_mute_is_put_back() {
    let (mut app, _fake) = signed_in().await;
    open(&mut app, ALICE).await;
    let prev = app.chats.get(ALICE).unwrap().notify;
    let _ = app.update(Message::Ui(Ui::SetMute(MuteFor::Forever)));
    assert!(app.chats.get(ALICE).unwrap().muted, "optimistic");
    let _ = app.update(Message::Done(Done::MuteDone {
        chat: ALICE,
        previous: prev,
        result: Err(ferrogram::error::Error::Net("x".into())),
    }));
    assert!(!app.chats.get(ALICE).unwrap().muted);
    assert_eq!(app.chats.get(ALICE).unwrap().notify, prev);
    assert!(app.toast.is_some());
}

#[tokio::test]
async fn timed_mutes_expire_and_other_devices_can_unmute() {
    let (mut app, _fake) = signed_in().await;
    open(&mut app, ALICE).await;
    // A mute that ended a second ago is not a mute.
    let _ = app.update(Message::Net(Event::Notify {
        target: NotifyTarget::Chat(ALICE),
        settings: NotifySettings {
            mute_until: Some(now() - 1),
            ..Default::default()
        },
    }));
    assert!(!app.chats.get(ALICE).unwrap().muted);
    // Muted for an hour on the phone ...
    let _ = app.update(Message::Net(Event::Notify {
        target: NotifyTarget::Chat(ALICE),
        settings: NotifySettings {
            mute_until: Some(now() + 3600),
            ..Default::default()
        },
    }));
    assert!(app.chats.get(ALICE).unwrap().muted);
    // ... and the tick re-evaluates when it runs out.
    app.chats.set_notify(
        ALICE,
        NotifySettings {
            mute_until: Some(now() - 5),
            ..Default::default()
        },
        now() - 10,
    );
    assert!(
        app.chats.get(ALICE).unwrap().muted,
        "was muted at that time"
    );
    let _ = app.update(Message::Ui(Ui::Tick));
    assert!(!app.chats.get(ALICE).unwrap().muted);
}

#[tokio::test]
async fn account_defaults_mute_all_channels_and_explicit_unmute_wins() {
    let mut cfg = Config::from_env(&["ferrogram".into(), "--mock".into()]);
    cfg.notifications = false;
    let mut app = App::boot(cfg).0;
    let fake = FakeTelegram::new(FakeOptions {
        logged_in: true,
        live_simulation: false,
        ..Default::default()
    });
    fake.set_notify_defaults(NotifyDefaults {
        broadcasts: NotifySettings {
            mute_until: Some(MUTE_FOREVER),
            silent: Some(true),
            show_previews: Some(false),
        },
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
    assert!(fake.calls().contains(&"notify_defaults".to_owned()));

    // Every channel is muted although none has its own setting.
    let channels: Vec<_> = app
        .chats
        .all()
        .iter()
        .filter(|c| c.kind == ferrogram::model::ChatKind::Channel)
        .collect();
    assert!(!channels.is_empty());
    assert!(channels.iter().all(|c| c.muted));
    assert!(
        channels.iter().any(|c| c.notify.mute_until.is_none()),
        "muted by the default alone"
    );
    // Private chats and groups are not.
    assert!(!app.chats.get(ALICE).unwrap().muted);
    let eff = app.chats.effective(DEV_CHANNEL, now()).unwrap();
    assert!(eff.muted && eff.silent && !eff.show_previews);

    // The tray count leaves them out.
    let tray_unread = app.tray_model().unread;
    let expected: u32 = app
        .chats
        .all()
        .iter()
        .filter(|c| !c.muted)
        .map(|c| c.unread_count)
        .sum();
    assert_eq!(tray_unread, expected);

    // Unmuting one channel sends an explicit 0, which beats the default.
    open(&mut app, DEV_CHANNEL).await;
    ui(&mut app, Ui::SetMute(MuteFor::Off)).await;
    assert_eq!(fake.notify_of(DEV_CHANNEL).unwrap().mute_until, Some(0));
    assert!(!app.chats.get(DEV_CHANNEL).unwrap().muted);
    // And the defaults are cached for the next start.
    let cached = app.store.load_notify_defaults().unwrap();
    assert_eq!(cached.broadcasts.mute_until, Some(MUTE_FOREVER));

    // The account default changing on another device is followed live.
    let _ = app.update(Message::Net(Event::Notify {
        target: NotifyTarget::Scope(NotifyScope::Chats),
        settings: NotifySettings {
            mute_until: Some(MUTE_FOREVER),
            ..Default::default()
        },
    }));
    assert!(app.chats.get(RUST_GROUP).unwrap().muted);
}

#[tokio::test]
async fn muted_chats_do_not_notify() {
    let (mut app, _fake) = signed_in().await;
    let m = ferrogram::model::Message::text(ALICE, 99_000, false, now(), "hi");
    assert!(ferrogram::app::state::should_notify(
        app.chats.get(ALICE),
        &m,
        None,
        false,
        now()
    ));
    let _ = app.update(Message::Net(Event::Notify {
        target: NotifyTarget::Chat(ALICE),
        settings: NotifySettings {
            mute_until: Some(MUTE_FOREVER),
            ..Default::default()
        },
    }));
    assert!(!ferrogram::app::state::should_notify(
        app.chats.get(ALICE),
        &m,
        None,
        false,
        now()
    ));
    // A message in a muted chat adds to its unread but not to the tray.
    let before = app.tray_model().unread;
    let _ = app.update(Message::Net(Event::NewMessage(m)));
    assert_eq!(app.tray_model().unread, before);
    assert!(app.chats.get(ALICE).unwrap().unread_count > 0);
}

// -------------------------------------------------------- clear / delete

#[tokio::test]
async fn clear_history_asks_first_then_calls_the_server() {
    let (mut app, fake) = signed_in().await;
    open(&mut app, ALICE).await;
    assert!(fake.history_len(ALICE) > 0);
    ui(&mut app, Ui::AskClearHistory).await;
    assert!(matches!(
        app.confirm,
        Some(Confirm::ClearHistory { revoke: false, .. })
    ));
    ui(&mut app, Ui::ConfirmCancel).await;
    assert!(app.confirm.is_none() && fake.history_len(ALICE) > 0);
    assert!(!fake.calls().iter().any(|c| c.starts_with("clear_history")));

    ui(&mut app, Ui::AskClearHistory).await;
    ui(&mut app, Ui::ConfirmRevoke(true)).await;
    ui(&mut app, Ui::ConfirmAccept).await;
    assert_eq!(fake.history_len(ALICE), 0);
    assert!(fake.calls().contains(&"clear_history+revoke".to_owned()));
    assert!(app.chats.get(ALICE).unwrap().last_message.is_none());
    assert!(app.conversation.as_ref().unwrap().messages().is_empty());
    assert!(app.confirm.is_none());
}

#[tokio::test]
async fn delete_chat_removes_it_everywhere() {
    let (mut app, fake) = signed_in().await;
    open(&mut app, ALICE).await;
    app.store.upsert_chats(app.chats.all()).unwrap();
    ui(&mut app, Ui::AskDeleteChat).await;
    ui(&mut app, Ui::ConfirmAccept).await;
    assert!(fake.calls().contains(&"delete_chat".to_owned()));
    assert!(!fake.has_chat(ALICE));
    assert!(app.chats.get(ALICE).is_none());
    assert!(app.selected.is_none() && app.conversation.is_none());
    assert!(
        app.store
            .load_chats()
            .unwrap()
            .iter()
            .all(|c| c.id != ALICE),
        "gone from the cache too"
    );
}

#[tokio::test]
async fn leave_group_and_channel_use_their_own_calls() {
    let (mut app, fake) = signed_in().await;
    open(&mut app, RUST_GROUP).await;
    ui(&mut app, Ui::AskLeave).await;
    assert!(matches!(app.confirm, Some(Confirm::Leave { .. })));
    ui(&mut app, Ui::ConfirmAccept).await;
    assert!(fake.calls().contains(&"leave_group".to_owned()));
    assert!(app.chats.get(RUST_GROUP).is_none());

    open(&mut app, DEV_CHANNEL).await;
    ui(&mut app, Ui::AskLeave).await;
    ui(&mut app, Ui::ConfirmAccept).await;
    assert!(fake.calls().contains(&"leave_channel".to_owned()));
    assert!(!fake.has_chat(DEV_CHANNEL));
}

#[tokio::test]
async fn escape_cancels_a_confirmation_without_acting() {
    let (mut app, fake) = signed_in().await;
    open(&mut app, ALICE).await;
    ui(&mut app, Ui::AskDeleteChat).await;
    ui(&mut app, Ui::Escape).await;
    assert!(app.confirm.is_none());
    assert!(fake.has_chat(ALICE));
}

// ------------------------------------------------------------------- block

#[tokio::test]
async fn block_and_unblock_round_trip_with_the_server() {
    let (mut app, fake) = signed_in().await;
    open(&mut app, ALICE).await;
    ui(&mut app, Ui::ToggleHeaderMenu).await;
    assert_eq!(
        app.blocked.get(&ALICE),
        Some(&false),
        "opening the menu learns the state"
    );
    ui(&mut app, Ui::ToggleBlock).await;
    assert!(fake.is_blocked(ALICE));
    assert_eq!(app.blocked.get(&ALICE), Some(&true));
    assert!(app.header_menu.is_none());
    // The menu now offers Unblock (state came from the server).
    let items = ferrogram::app::chatmenu::menu_items(
        app.chats.get(ALICE).unwrap(),
        false,
        app.blocked.get(&ALICE).copied(),
    );
    assert!(items.contains(&ferrogram::app::chatmenu::MenuItem::Unblock));
    ui(&mut app, Ui::ToggleBlock).await;
    assert!(!fake.is_blocked(ALICE));
    assert!(fake.calls().contains(&"unblock".to_owned()));
    // A refusal reverts the optimistic flag.
    let _ = app.update(Message::Done(Done::BlockDone {
        chat: ALICE,
        blocked: true,
        result: Err(ferrogram::error::Error::Net("x".into())),
    }));
    assert_eq!(app.blocked.get(&ALICE), Some(&false));
}

// ------------------------------------------------------------------ search

#[tokio::test]
async fn search_as_you_type_selects_and_highlights_the_newest_hit() {
    let (mut app, fake) = signed_in().await;
    open(&mut app, ALICE).await;
    ui(&mut app, Ui::OpenChatSearch).await;
    assert!(app.chat_search.is_some());
    ui(&mut app, Ui::ChatSearchInput("trail".into())).await;
    assert!(
        fake.calls().contains(&"search".to_owned()),
        "debounce fired"
    );
    let s = app.chat_search.as_ref().unwrap();
    assert!(!s.searching && s.searched && !s.results.is_empty());
    assert_eq!(s.index, Some(0));
    let newest = s.results[0].id;
    assert!(s.results.iter().all(|m| m.id <= newest), "newest first");
    assert_eq!(app.highlight, Some(newest));
    assert!(s.counter().starts_with("1 of "));
}

#[tokio::test]
async fn a_stale_debounce_does_not_search() {
    let (mut app, fake) = signed_in().await;
    open(&mut app, ALICE).await;
    ui(&mut app, Ui::OpenChatSearch).await;
    let _ = app.update(Message::Ui(Ui::ChatSearchInput("tr".into())));
    let stale = app.chat_search.as_ref().unwrap().version;
    let _ = app.update(Message::Ui(Ui::ChatSearchInput("trail".into())));
    ui(&mut app, Ui::ChatSearchDue(stale)).await;
    assert!(!fake.calls().contains(&"search".to_owned()));
}

#[tokio::test]
async fn up_and_down_walk_the_hits_and_jump_to_each() {
    let (mut app, _fake) = signed_in().await;
    open(&mut app, ALICE).await;
    ui(&mut app, Ui::OpenChatSearch).await;
    ui(&mut app, Ui::ChatSearchInput("the".into())).await;
    let ids: Vec<_> = app
        .chat_search
        .as_ref()
        .unwrap()
        .results
        .iter()
        .map(|m| m.id)
        .collect();
    assert!(ids.len() >= 3, "fixture has several hits");
    ui(&mut app, Ui::ChatSearchStep(Dir::Older)).await;
    assert_eq!(app.highlight, Some(ids[1]));
    ui(&mut app, Ui::ChatSearchStep(Dir::Older)).await;
    assert_eq!(app.highlight, Some(ids[2]));
    ui(&mut app, Ui::ChatSearchStep(Dir::Newer)).await;
    assert_eq!(app.highlight, Some(ids[1]));
    assert_eq!(app.chat_search.as_ref().unwrap().index, Some(1));
    // Enter after a finished search is "next older".
    ui(&mut app, Ui::ChatSearchSubmit).await;
    assert_eq!(app.highlight, Some(ids[2]));
    // The search survives the jumps (same chat) and Esc closes it.
    assert!(app.chat_search.is_some());
    ui(&mut app, Ui::Escape).await;
    assert!(app.chat_search.is_none());
    assert!(
        app.highlight.is_none(),
        "closing the search clears the highlight"
    );
}

#[tokio::test]
async fn stepping_past_a_full_page_fetches_the_next_one() {
    let (mut app, fake) = signed_in().await;
    open(&mut app, ALICE).await;
    ui(&mut app, Ui::OpenChatSearch).await;
    // Almost every filler line has an "e": more than one page of hits.
    ui(&mut app, Ui::ChatSearchInput("e".into())).await;
    let s = app.chat_search.as_ref().unwrap();
    assert_eq!(s.results.len(), ferrogram::app::chatmenu::SEARCH_PAGE);
    assert!(!s.exhausted);
    let searches = fake.calls().iter().filter(|c| *c == "search").count();
    for _ in 0..ferrogram::app::chatmenu::SEARCH_PAGE {
        ui(&mut app, Ui::ChatSearchStep(Dir::Older)).await;
    }
    let s = app.chat_search.as_ref().unwrap();
    assert!(s.results.len() > ferrogram::app::chatmenu::SEARCH_PAGE);
    assert_eq!(
        fake.calls().iter().filter(|c| *c == "search").count(),
        searches + 1
    );
    assert_eq!(s.index, Some(ferrogram::app::chatmenu::SEARCH_PAGE));
    let ids: Vec<_> = s.results.iter().map(|m| m.id).collect();
    assert!(
        ids.windows(2).all(|w| w[0] > w[1]),
        "no duplicates, newest first"
    );
}

#[tokio::test]
async fn from_filter_limits_a_group_search_to_one_member() {
    let (mut app, fake) = signed_in().await;
    open(&mut app, RUST_GROUP).await;
    ui(&mut app, Ui::OpenChatSearch).await;
    ui(&mut app, Ui::ChatSearchFromPicker).await;
    let members = app
        .chat_search
        .as_ref()
        .unwrap()
        .members
        .clone()
        .expect("members loaded");
    assert!(fake.calls().contains(&"members".to_owned()));
    assert!(members.len() >= 2);
    let who = members
        .iter()
        .find(|m| m.id != mock::SELF_ID)
        .unwrap()
        .clone();
    ui(&mut app, Ui::ChatSearchFrom(Some(who.clone()))).await;
    let s = app.chat_search.as_ref().unwrap();
    assert!(!s.from_open);
    assert!(
        !s.results.is_empty(),
        "an empty query with a member lists their messages"
    );
    assert!(s.results.iter().all(|m| m.sender_id == Some(who.id)));
    // Clearing the filter with no text ends the search.
    ui(&mut app, Ui::ChatSearchFrom(None)).await;
    let s = app.chat_search.as_ref().unwrap();
    assert!(s.results.is_empty() && s.from.is_none());
}

#[tokio::test]
async fn closing_the_search_via_the_button_and_switching_chats() {
    let (mut app, _fake) = signed_in().await;
    open(&mut app, ALICE).await;
    ui(&mut app, Ui::ToggleChatSearch).await;
    assert!(app.chat_search.is_some());
    ui(&mut app, Ui::ToggleChatSearch).await;
    assert!(app.chat_search.is_none());
    ui(&mut app, Ui::ToggleChatSearch).await;
    open(&mut app, RUST_GROUP).await;
    assert!(
        app.chat_search.is_none(),
        "another chat: the search is gone"
    );
}

// --------------------------------------------------------------- selection

#[tokio::test]
async fn select_messages_then_delete_or_copy() {
    let (mut app, fake) = signed_in().await;
    open(&mut app, ALICE).await;
    let ids: Vec<i32> = app
        .conversation
        .as_ref()
        .unwrap()
        .messages()
        .iter()
        .rev()
        .take(3)
        .map(|m| m.id)
        .collect();
    ui(&mut app, Ui::ToggleHeaderMenu).await;
    ui(&mut app, Ui::StartSelect).await;
    assert!(app.selection.is_some() && app.header_menu.is_none());
    for id in &ids {
        ui(&mut app, Ui::ToggleSelect(*id)).await;
    }
    ui(&mut app, Ui::ToggleSelect(ids[0])).await;
    assert_eq!(app.selection.as_ref().unwrap().len(), 2);

    let before = fake.history_len(ALICE);
    ui(&mut app, Ui::AskDeleteSelected).await;
    let Some(Confirm::DeleteMessages { ids: asked, .. }) = &app.confirm else {
        panic!("asks first");
    };
    assert_eq!(asked.len(), 2);
    assert_eq!(fake.history_len(ALICE), before, "nothing deleted yet");
    ui(&mut app, Ui::ConfirmAccept).await;
    assert_eq!(fake.history_len(ALICE), before - 2);
    assert!(app.selection.is_none());
    let conv = app.conversation.as_ref().unwrap();
    assert!(conv.get(ids[1]).is_none() && conv.get(ids[2]).is_none());

    // Esc leaves selection mode; the composer is back.
    ui(&mut app, Ui::StartSelect).await;
    ui(&mut app, Ui::Escape).await;
    assert!(app.selection.is_none());
}

#[tokio::test]
async fn a_failed_bulk_delete_restores_the_messages() {
    let (mut app, _fake) = signed_in().await;
    open(&mut app, ALICE).await;
    let old: Vec<_> = app
        .conversation
        .as_ref()
        .unwrap()
        .messages()
        .iter()
        .rev()
        .take(2)
        .cloned()
        .collect();
    for m in &old {
        app.conversation.as_mut().unwrap().remove_local(m.id);
    }
    let _ = app.update(Message::Done(Done::RollbackMany {
        old: old.clone(),
        error: ferrogram::error::Error::Net("x".into()),
    }));
    let conv = app.conversation.as_ref().unwrap();
    assert!(old.iter().all(|m| conv.get(m.id).is_some()));
    assert!(app.toast.is_some());
}

// ------------------------------------------------------------------ folder

#[tokio::test]
async fn add_to_folder_from_the_header_menu() {
    let (mut app, fake) = signed_in().await;
    open(&mut app, ALICE).await;
    assert!(!app.folders.list.is_empty(), "mock account has folders");
    let folder = app
        .folders
        .list
        .iter()
        .find(|f| !f.shared && !f.matches(app.chats.get(ALICE).unwrap()))
        .or_else(|| app.folders.list.iter().find(|f| !f.shared))
        .unwrap()
        .id;
    ui(&mut app, Ui::ToggleHeaderMenu).await;
    ui(&mut app, Ui::HeaderMenuPage(HeaderMenu::Folders)).await;
    ui(&mut app, Ui::ChatInFolder(ALICE, folder)).await;
    assert!(app.header_menu.is_none());
    assert!(fake.calls().contains(&"save_dialog_filter".to_owned()));
}
