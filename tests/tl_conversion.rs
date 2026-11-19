//! TL (wire) → domain conversion, against hand-built TL fixtures that are
//! round-tripped through the MTProto binary serialisation first, so the
//! parser path (`Deserializable`) is exercised as well.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use ferrogram::model::{CHANNEL_OFFSET, Media};
use ferrogram::net::Event;
use ferrogram::net::types::{
    dialog_info, event_from_update, largest_size, message_from_tl, peer_to_chat_id, rpc_error,
};
use grammers_client::tl;
use grammers_client::tl::{Deserializable, Serializable};

fn user(id: i64) -> tl::enums::Peer {
    tl::enums::Peer::User(tl::types::PeerUser { user_id: id })
}

fn channel(id: i64) -> tl::enums::Peer {
    tl::enums::Peer::Channel(tl::types::PeerChannel { channel_id: id })
}

fn raw_message(id: i32, peer: tl::enums::Peer, out: bool, text: &str) -> tl::types::Message {
    tl::types::Message {
        out,
        mentioned: false,
        media_unread: false,
        silent: false,
        post: false,
        from_scheduled: false,
        legacy: false,
        edit_hide: false,
        pinned: false,
        noforwards: false,
        invert_media: false,
        offline: false,
        video_processing_pending: false,
        paid_suggested_post_stars: false,
        paid_suggested_post_ton: false,
        id,
        from_id: None,
        from_boosts_applied: None,
        from_rank: None,
        peer_id: peer,
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        guestchat_via_from: None,
        reply_to: None,
        date: 1_795_000_000,
        message: text.into(),
        media: None,
        reply_markup: None,
        entities: None,
        views: None,
        forwards: None,
        replies: None,
        edit_date: None,
        post_author: None,
        grouped_id: None,
        reactions: None,
        restriction_reason: None,
        ttl_period: None,
        quick_reply_shortcut_id: None,
        effect: None,
        factcheck: None,
        report_delivery_until_date: None,
        paid_message_stars: None,
        suggested_post: None,
        schedule_repeat_period: None,
        summary_from_language: None,
        rich_message: None,
    }
}

/// Serialise to MTProto bytes and parse back (the "recorded" wire form).
fn wire<T: Serializable + Deserializable>(v: T) -> T {
    T::from_bytes(&v.to_bytes()).expect("valid TL")
}

fn reactions(items: &[(&str, i32, bool)]) -> tl::enums::MessageReactions {
    tl::enums::MessageReactions::Reactions(tl::types::MessageReactions {
        min: false,
        can_see_list: false,
        reactions_as_tags: false,
        results: items
            .iter()
            .map(|(e, c, chosen)| {
                tl::enums::ReactionCount::Count(tl::types::ReactionCount {
                    chosen_order: chosen.then_some(0),
                    reaction: tl::enums::Reaction::Emoji(tl::types::ReactionEmoji {
                        emoticon: (*e).into(),
                    }),
                    count: *c,
                })
            })
            .collect(),
        recent_reactions: None,
        top_reactors: None,
    })
}

#[test]
fn peer_ids_use_bot_api_encoding() {
    assert_eq!(peer_to_chat_id(&user(42)), 42);
    assert_eq!(
        peer_to_chat_id(&tl::enums::Peer::Chat(tl::types::PeerChat { chat_id: 7 })),
        -7
    );
    assert_eq!(peer_to_chat_id(&channel(5)), -CHANNEL_OFFSET - 5);
}

#[test]
fn text_message_with_reply_edit_and_reactions() {
    let mut m = raw_message(10, user(42), false, "hello there");
    m.reply_to = Some(tl::enums::MessageReplyHeader::Header(
        tl::types::MessageReplyHeader {
            reply_to_scheduled: false,
            forum_topic: false,
            quote: false,
            reply_to_ephemeral: false,
            reply_to_msg_id: Some(9),
            reply_to_peer_id: None,
            reply_from: None,
            reply_media: None,
            reply_to_top_id: None,
            quote_text: None,
            quote_entities: None,
            quote_offset: None,
            todo_item_id: None,
            poll_option: None,
            quote_rich: None,
        },
    ));
    m.edit_date = Some(1_795_000_100);
    m.reactions = Some(reactions(&[("👍", 3, true), ("🔥", 1, false)]));
    let raw = wire(tl::enums::Message::Message(m));
    let d = message_from_tl(&raw).unwrap();
    assert_eq!((d.chat_id, d.id, d.outgoing), (42, 10, false));
    assert_eq!(
        d.sender_id,
        Some(42),
        "private incoming: sender is the peer"
    );
    assert_eq!(d.text, "hello there");
    assert_eq!(d.reply_to, Some(9));
    assert!(d.edited);
    assert_eq!(d.reactions.len(), 2);
    assert!(d.reactions[0].chosen && d.reactions[0].count == 3);
}

#[test]
fn hidden_edits_are_not_marked_edited() {
    let mut m = raw_message(1, user(1), true, "x");
    m.edit_date = Some(5);
    m.edit_hide = true;
    let d = message_from_tl(&tl::enums::Message::Message(m)).unwrap();
    assert!(!d.edited);
    assert_eq!(d.sender_id, None);
}

#[test]
fn photo_media_uses_largest_size() {
    let sizes = vec![
        tl::enums::PhotoSize::Size(tl::types::PhotoSize {
            r#type: "m".into(),
            w: 320,
            h: 213,
            size: 1000,
        }),
        tl::enums::PhotoSize::Progressive(tl::types::PhotoSizeProgressive {
            r#type: "y".into(),
            w: 1280,
            h: 853,
            sizes: vec![1, 2, 3],
        }),
    ];
    assert_eq!(largest_size(&sizes), (1280, 853));
    let mut m = raw_message(3, user(1), false, "caption");
    m.media = Some(tl::enums::MessageMedia::Photo(
        tl::types::MessageMediaPhoto {
            spoiler: false,
            live_photo: false,
            photo: Some(tl::enums::Photo::Photo(tl::types::Photo {
                has_stickers: false,
                id: 77,
                access_hash: 1,
                file_reference: vec![],
                date: 0,
                sizes,
                video_sizes: None,
                dc_id: 2,
            })),
            ttl_seconds: None,
            video: None,
        },
    ));
    let d = message_from_tl(&wire(tl::enums::Message::Message(m))).unwrap();
    assert_eq!(
        d.media,
        Some(Media::Photo {
            id: 77,
            width: 1280,
            height: 853
        })
    );
    assert_eq!(d.preview(), "Photo, caption");
}

fn document(attrs: Vec<tl::enums::DocumentAttribute>, voice: bool) -> tl::enums::MessageMedia {
    tl::enums::MessageMedia::Document(tl::types::MessageMediaDocument {
        nopremium: false,
        spoiler: false,
        video: false,
        round: false,
        voice,
        document: Some(tl::enums::Document::Document(tl::types::Document {
            id: 1,
            access_hash: 1,
            file_reference: vec![],
            date: 0,
            mime_type: "application/pdf".into(),
            size: 2_457_600,
            thumbs: None,
            video_thumbs: None,
            dc_id: 2,
            attributes: attrs,
        })),
        alt_documents: None,
        video_cover: None,
        video_timestamp: None,
        ttl_seconds: None,
    })
}

#[test]
fn documents_become_files_or_labels() {
    let mut m = raw_message(4, user(1), false, "");
    m.media = Some(document(
        vec![tl::enums::DocumentAttribute::Filename(
            tl::types::DocumentAttributeFilename {
                file_name: "trail-map.pdf".into(),
            },
        )],
        false,
    ));
    let d = message_from_tl(&wire(tl::enums::Message::Message(m.clone()))).unwrap();
    assert_eq!(
        d.media,
        Some(Media::File {
            name: "trail-map.pdf".into(),
            size: 2_457_600,
            mime: "application/pdf".into()
        })
    );
    m.media = Some(document(vec![], true));
    let d = message_from_tl(&tl::enums::Message::Message(m)).unwrap();
    assert_eq!(d.preview(), "Voice message");
}

#[test]
fn service_and_empty_messages() {
    let s = tl::enums::Message::Service(tl::types::MessageService {
        out: false,
        mentioned: false,
        media_unread: false,
        reactions_are_possible: false,
        silent: false,
        post: false,
        legacy: false,
        id: 5,
        from_id: Some(user(3)),
        peer_id: channel(9),
        saved_peer_id: None,
        reply_to: None,
        date: 0,
        action: tl::enums::MessageAction::PinMessage,
        reactions: None,
        ttl_period: None,
    });
    let d = message_from_tl(&wire(s)).unwrap();
    assert!(d.service);
    assert_eq!(d.text, "Message pinned");
    assert_eq!(d.chat_id, -CHANNEL_OFFSET - 9);
    let empty = tl::enums::Message::Empty(tl::types::MessageEmpty {
        id: 1,
        peer_id: None,
    });
    assert!(message_from_tl(&empty).is_none());
}

#[test]
fn updates_map_to_events() {
    let new = tl::enums::Update::NewMessage(tl::types::UpdateNewMessage {
        message: tl::enums::Message::Message(raw_message(11, user(5), false, "yo")),
        pts: 1,
        pts_count: 1,
    });
    match event_from_update(&wire(new)) {
        Some(Event::NewMessage(m)) => assert_eq!((m.chat_id, m.id), (5, 11)),
        other => panic!("{other:?}"),
    }

    let edit = tl::enums::Update::EditChannelMessage(tl::types::UpdateEditChannelMessage {
        message: tl::enums::Message::Message(raw_message(2, channel(8), true, "edited")),
        pts: 1,
        pts_count: 1,
    });
    assert!(
        matches!(event_from_update(&edit), Some(Event::MessageEdited(m)) if m.text == "edited")
    );

    let del = tl::enums::Update::DeleteMessages(tl::types::UpdateDeleteMessages {
        messages: vec![1, 2],
        pts: 1,
        pts_count: 2,
    });
    assert_eq!(
        event_from_update(&wire(del)),
        Some(Event::MessagesDeleted {
            chat: None,
            ids: vec![1, 2]
        })
    );
    let cdel = tl::enums::Update::DeleteChannelMessages(tl::types::UpdateDeleteChannelMessages {
        channel_id: 8,
        messages: vec![3],
        pts: 1,
        pts_count: 1,
    });
    assert_eq!(
        event_from_update(&cdel),
        Some(Event::MessagesDeleted {
            chat: Some(-CHANNEL_OFFSET - 8),
            ids: vec![3]
        })
    );

    let react = tl::enums::Update::MessageReactions(tl::types::UpdateMessageReactions {
        peer: user(5),
        msg_id: 11,
        top_msg_id: None,
        saved_peer_id: None,
        reactions: reactions(&[("❤️", 2, false)]),
    });
    match event_from_update(&wire(react)) {
        Some(Event::Reactions {
            chat,
            id,
            reactions,
        }) => {
            assert_eq!((chat, id, reactions[0].emoji.as_str()), (5, 11, "❤️"));
        }
        other => panic!("{other:?}"),
    }

    let read = tl::enums::Update::ReadHistoryOutbox(tl::types::UpdateReadHistoryOutbox {
        peer: user(5),
        max_id: 11,
        pts: 1,
        pts_count: 1,
    });
    assert_eq!(
        event_from_update(&read),
        Some(Event::ReadOutbox {
            chat: 5,
            max_id: 11
        })
    );
    let inbox = tl::enums::Update::ReadHistoryInbox(tl::types::UpdateReadHistoryInbox {
        folder_id: None,
        peer: user(5),
        top_msg_id: None,
        max_id: 11,
        still_unread_count: 0,
        pts: 1,
        pts_count: 1,
    });
    assert_eq!(
        event_from_update(&inbox),
        Some(Event::ReadInbox {
            chat: 5,
            max_id: 11,
            still_unread: 0
        })
    );
    // Unrelated updates are ignored.
    let unrelated = tl::enums::Update::UserName(tl::types::UpdateUserName {
        user_id: 5,
        first_name: "A".into(),
        last_name: "B".into(),
        usernames: vec![],
    });
    assert!(event_from_update(&unrelated).is_none());
}

#[test]
fn dialog_counters_and_mute() {
    let settings = |mute_until| {
        tl::enums::PeerNotifySettings::Settings(tl::types::PeerNotifySettings {
            show_previews: None,
            silent: None,
            mute_until,
            ios_sound: None,
            android_sound: None,
            other_sound: None,
            stories_muted: None,
            stories_hide_sender: None,
            stories_ios_sound: None,
            stories_android_sound: None,
            stories_other_sound: None,
        })
    };
    let d = |mute_until| {
        tl::enums::Dialog::Dialog(tl::types::Dialog {
            pinned: true,
            unread_mark: false,
            view_forum_as_messages: false,
            peer: user(1),
            top_message: 50,
            read_inbox_max_id: 45,
            read_outbox_max_id: 49,
            unread_count: 5,
            unread_mentions_count: 0,
            unread_reactions_count: 0,
            unread_poll_votes_count: 0,
            notify_settings: settings(mute_until),
            pts: None,
            draft: None,
            folder_id: None,
            ttl_period: None,
        })
    };
    let info = dialog_info(&wire(d(Some(2_000_000_000))), 1_795_000_000);
    assert!(info.pinned && info.muted);
    assert_eq!(
        (info.unread, info.read_inbox, info.read_outbox),
        (5, 45, 49)
    );
    assert!(!dialog_info(&d(Some(10)), 1_795_000_000).muted);
    assert!(!dialog_info(&d(None), 1_795_000_000).muted);
}

#[test]
fn rpc_errors_map_to_auth_errors() {
    use ferrogram::error::{AuthError, Error};
    assert!(matches!(
        rpc_error(400, "PHONE_CODE_INVALID", None),
        Error::Auth(AuthError::InvalidCode)
    ));
    assert!(matches!(
        rpc_error(400, "PASSWORD_HASH_INVALID", None),
        Error::Auth(AuthError::InvalidPassword)
    ));
    assert!(matches!(
        rpc_error(420, "FLOOD_WAIT", Some(30)),
        Error::Auth(AuthError::FloodWait(30))
    ));
    assert!(rpc_error(401, "AUTH_KEY_UNREGISTERED", None).is_session_revoked());
    assert!(rpc_error(401, "SOMETHING_NEW", None).is_session_revoked());
    assert!(matches!(
        rpc_error(400, "PEER_ID_INVALID", None),
        Error::Protocol(_)
    ));
}

#[test]
fn typing_updates_map_to_events() {
    use ferrogram::model::TypingAction;
    use tl::enums::SendMessageAction as A;
    let action = |a: A| wire(a);

    // Private chat: chat id == user id.
    let u = tl::enums::Update::UserTyping(tl::types::UpdateUserTyping {
        user_id: 42,
        top_msg_id: None,
        action: action(A::SendMessageTypingAction),
    });
    assert_eq!(
        event_from_update(&wire(u)),
        Some(Event::Typing {
            chat: 42,
            user: 42,
            action: Some(TypingAction::Typing)
        })
    );

    // Basic group: negative chat id, the typist comes from `from_id`.
    let g = tl::enums::Update::ChatUserTyping(tl::types::UpdateChatUserTyping {
        chat_id: 7,
        from_id: user(42),
        action: action(A::SendMessageRecordAudioAction),
    });
    assert_eq!(
        event_from_update(&wire(g)),
        Some(Event::Typing {
            chat: -7,
            user: 42,
            action: Some(TypingAction::RecordingVoice)
        })
    );

    // Channel / supergroup, and a cancel action ends the status.
    let c = tl::enums::Update::ChannelUserTyping(tl::types::UpdateChannelUserTyping {
        channel_id: 9,
        top_msg_id: None,
        from_id: user(42),
        action: action(A::SendMessageCancelAction),
    });
    assert_eq!(
        event_from_update(&wire(c)),
        Some(Event::Typing {
            chat: -CHANNEL_OFFSET - 9,
            user: 42,
            action: None
        })
    );

    // Typing by a channel/chat peer (anonymous admin) and in forum topics is ignored.
    let anon = tl::enums::Update::ChannelUserTyping(tl::types::UpdateChannelUserTyping {
        channel_id: 9,
        top_msg_id: None,
        from_id: channel(9),
        action: action(A::SendMessageTypingAction),
    });
    assert_eq!(event_from_update(&wire(anon)), None);
    let topic = tl::enums::Update::ChannelUserTyping(tl::types::UpdateChannelUserTyping {
        channel_id: 9,
        top_msg_id: Some(3),
        from_id: user(42),
        action: action(A::SendMessageTypingAction),
    });
    assert_eq!(event_from_update(&wire(topic)), None);

    // Upload actions carry progress; unknown ones degrade to `Other`.
    for (a, want) in [
        (
            A::SendMessageUploadPhotoAction(tl::types::SendMessageUploadPhotoAction {
                progress: 50,
            }),
            TypingAction::UploadingPhoto,
        ),
        (
            A::SendMessageChooseStickerAction,
            TypingAction::ChoosingSticker,
        ),
        (A::SendMessageGamePlayAction, TypingAction::Other),
    ] {
        assert_eq!(
            ferrogram::net::types::typing_action_from_tl(&wire(a)),
            Some(want)
        );
    }
}

#[test]
fn user_status_maps_to_presence() {
    use ferrogram::model::Presence;
    use ferrogram::net::types::presence_from_tl;
    use tl::enums::UserStatus as S;
    let now = 1_795_000_000;
    let online = |expires| S::Online(tl::types::UserStatusOnline { expires });
    assert_eq!(
        presence_from_tl(&wire(online(now as i32 + 30)), now),
        Presence::Online { until: now + 30 }
    );
    // A stale "online" becomes "last seen at <expires>".
    assert_eq!(
        presence_from_tl(&wire(online(now as i32 - 30)), now),
        Presence::Offline {
            was_online: now - 30
        }
    );
    assert_eq!(
        presence_from_tl(
            &wire(S::Offline(tl::types::UserStatusOffline {
                was_online: 1_794_999_000
            })),
            now
        ),
        Presence::Offline {
            was_online: 1_794_999_000
        }
    );
    assert_eq!(presence_from_tl(&wire(S::Empty), now), Presence::Unknown);
    assert_eq!(
        presence_from_tl(
            &wire(S::Recently(tl::types::UserStatusRecently { by_me: false })),
            now
        ),
        Presence::Recently
    );
    assert_eq!(
        presence_from_tl(
            &wire(S::LastWeek(tl::types::UserStatusLastWeek { by_me: false })),
            now
        ),
        Presence::LastWeek
    );
    assert_eq!(
        presence_from_tl(
            &wire(S::LastMonth(tl::types::UserStatusLastMonth {
                by_me: false
            })),
            now
        ),
        Presence::LastMonth
    );

    let ev = tl::enums::Update::UserStatus(tl::types::UpdateUserStatus {
        user_id: 5,
        status: S::Recently(tl::types::UserStatusRecently { by_me: false }),
    });
    assert_eq!(
        event_from_update(&wire(ev)),
        Some(Event::UserStatus {
            user: 5,
            presence: Presence::Recently
        })
    );
}
