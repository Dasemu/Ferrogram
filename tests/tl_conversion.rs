//! TL (wire) → domain conversion, against hand-built TL fixtures that are
//! round-tripped through the MTProto binary serialisation first, so the
//! parser path (`Deserializable`) is exercised as well.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use ferrogram::model::{CHANNEL_OFFSET, Folder, Media, SharedBody, SharedKind};
use ferrogram::net::Event;
use ferrogram::net::types::{
    dialog_info, event_from_update, event_from_update_as, folder_from_tl, folder_to_tl,
    folders_from_tl, largest_size, message_from_tl, peer_to_chat_id, rpc_error, shared_filter,
    shared_item_from_tl, shared_page_from_tl, utf16_slice,
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

fn draft_message(text: &str, reply: Option<i32>) -> tl::enums::DraftMessage {
    tl::enums::DraftMessage::Message(tl::types::DraftMessage {
        no_webpage: false,
        invert_media: false,
        reply_to: reply.map(|id| {
            tl::enums::InputReplyTo::Message(tl::types::InputReplyToMessage {
                reply_to_msg_id: id,
                top_msg_id: None,
                reply_to_peer_id: None,
                quote_text: None,
                quote_entities: None,
                quote_offset: None,
                monoforum_peer_id: None,
                todo_item_id: None,
                poll_option: None,
                quote_rich: None,
            })
        }),
        message: text.into(),
        entities: None,
        media: None,
        date: 1_795_000_100,
        effect: None,
        suggested_post: None,
        rich_message: None,
    })
}

#[test]
fn drafts_convert_from_updates_and_dialogs() {
    use ferrogram::model::Draft;
    use ferrogram::net::types::draft_from_tl;

    let d = draft_from_tl(&wire(draft_message("see you at 8", Some(41)))).unwrap();
    assert_eq!(
        d,
        Draft {
            text: "see you at 8".into(),
            reply_to: Some(41),
            date: 1_795_000_100
        }
    );
    // Empty text and the "empty" constructor both mean "no draft".
    assert!(draft_from_tl(&wire(draft_message("", None))).is_none());
    assert!(
        draft_from_tl(&tl::enums::DraftMessage::Empty(
            tl::types::DraftMessageEmpty { date: None }
        ))
        .is_none()
    );

    let upd = tl::enums::Update::DraftMessage(tl::types::UpdateDraftMessage {
        peer: channel(9),
        top_msg_id: None,
        saved_peer_id: None,
        draft: draft_message("wip", None),
    });
    match event_from_update(&wire(upd)) {
        Some(Event::Draft { chat, draft }) => {
            assert_eq!(chat, -CHANNEL_OFFSET - 9);
            assert_eq!(draft.unwrap().text, "wip");
        }
        other => panic!("{other:?}"),
    }
    // Cleared draft.
    let cleared = tl::enums::Update::DraftMessage(tl::types::UpdateDraftMessage {
        peer: user(5),
        top_msg_id: None,
        saved_peer_id: None,
        draft: tl::enums::DraftMessage::Empty(tl::types::DraftMessageEmpty { date: None }),
    });
    assert_eq!(
        event_from_update(&wire(cleared)),
        Some(Event::Draft {
            chat: 5,
            draft: None
        })
    );
    // Drafts of forum topics are not the chat's draft.
    let topic = tl::enums::Update::DraftMessage(tl::types::UpdateDraftMessage {
        peer: channel(9),
        top_msg_id: Some(2),
        saved_peer_id: None,
        draft: draft_message("topic", None),
    });
    assert_eq!(event_from_update(&wire(topic)), None);
}

// ------------------------------------------------------------------ folders

fn ip_user(id: i64) -> tl::enums::InputPeer {
    tl::enums::InputPeer::User(tl::types::InputPeerUser {
        user_id: id,
        access_hash: id * 7,
    })
}

fn ip_chat(id: i64) -> tl::enums::InputPeer {
    tl::enums::InputPeer::Chat(tl::types::InputPeerChat { chat_id: id })
}

fn ip_channel(id: i64) -> tl::enums::InputPeer {
    tl::enums::InputPeer::Channel(tl::types::InputPeerChannel {
        channel_id: id,
        access_hash: id * 3,
    })
}

fn title(t: &str) -> tl::enums::TextWithEntities {
    tl::enums::TextWithEntities::Entities(tl::types::TextWithEntities {
        text: t.into(),
        entities: Vec::new(),
    })
}

fn raw_filter() -> tl::types::DialogFilter {
    tl::types::DialogFilter {
        contacts: true,
        non_contacts: false,
        groups: true,
        broadcasts: false,
        bots: true,
        exclude_muted: true,
        exclude_read: false,
        exclude_archived: true,
        title_noanimate: false,
        id: 5,
        title: title("Work"),
        emoticon: Some("💼".into()),
        color: None,
        pinned_peers: vec![ip_user(11), tl::enums::InputPeer::PeerSelf],
        // 11 is listed twice (pinned and included): it must not repeat.
        include_peers: vec![ip_user(11), ip_chat(7), ip_channel(900)],
        exclude_peers: vec![ip_user(12)],
    }
}

#[test]
fn dialog_filter_converts_to_a_folder() {
    let wire = wire(tl::enums::DialogFilter::Filter(raw_filter()));
    let (f, peers) = folder_from_tl(&wire, 42).unwrap();
    assert_eq!((f.id, f.title.as_str()), (5, "Work"));
    assert_eq!(f.emoticon.as_deref(), Some("💼"));
    assert!(f.contacts && f.groups && f.bots);
    assert!(!f.non_contacts && !f.channels);
    assert!(f.exclude_muted && f.exclude_archived && !f.exclude_read);
    assert_eq!(
        f.pinned,
        vec![11, 42],
        "inputPeerSelf is Saved Messages (our id)"
    );
    assert_eq!(
        f.include,
        vec![-7, -CHANNEL_OFFSET - 900],
        "Bot API ids; the pinned chat is not repeated"
    );
    assert_eq!(f.exclude, vec![12]);
    assert!(!f.shared);
    // Access hashes are kept for saving the folder again.
    assert!(peers.iter().any(|(id, p)| *id == 11 && *p == ip_user(11)));
    assert!(
        peers
            .iter()
            .any(|(id, p)| *id == -CHANNEL_OFFSET - 900 && *p == ip_channel(900))
    );
}

#[test]
fn default_and_chatlist_filters() {
    assert!(folder_from_tl(&tl::enums::DialogFilter::Default, 1).is_none());
    let shared = tl::enums::DialogFilter::Chatlist(tl::types::DialogFilterChatlist {
        has_my_invites: true,
        title_noanimate: false,
        id: 9,
        title: title("Shared"),
        emoticon: None,
        color: None,
        pinned_peers: vec![],
        include_peers: vec![ip_channel(5)],
    });
    let (f, _) = folder_from_tl(&wire(shared), 1).unwrap();
    assert!(f.shared);
    assert_eq!(f.include, vec![-CHANNEL_OFFSET - 5]);
    assert!(
        !f.contacts && !f.groups,
        "shared folders have no type rules"
    );
}

#[test]
fn folder_converts_back_to_tl() {
    let (f, peers) = folder_from_tl(&tl::enums::DialogFilter::Filter(raw_filter()), 42).unwrap();
    let known: std::collections::HashMap<_, _> = peers.into_iter().collect();
    let resolve = |id: i64| known.get(&id).cloned();
    let back = folder_to_tl(&f, 42, &resolve).unwrap();
    let tl::enums::DialogFilter::Filter(d) = wire(back) else {
        panic!("a normal folder is a dialogFilter");
    };
    assert_eq!(d.id, 5);
    assert!(d.contacts && d.groups && d.bots && !d.broadcasts && !d.non_contacts);
    assert!(d.exclude_muted && d.exclude_archived && !d.exclude_read);
    assert_eq!(
        d.pinned_peers,
        vec![ip_user(11), tl::enums::InputPeer::PeerSelf]
    );
    assert_eq!(d.include_peers, vec![ip_chat(7), ip_channel(900)]);
    assert_eq!(d.exclude_peers, vec![ip_user(12)]);
    let tl::enums::TextWithEntities::Entities(t) = &d.title;
    assert_eq!(t.text, "Work");
}

#[test]
fn saving_a_folder_never_drops_unknown_chats_silently() {
    let mut f = Folder::new(3, "X");
    f.include = vec![5, 6];
    let only5 = |id: i64| (id == 5).then(|| ip_user(5));
    assert_eq!(folder_to_tl(&f, 1, &only5).unwrap_err(), 6);
    // A pinned chat is written once, in `pinned_peers`.
    let mut g = Folder::new(3, "X");
    g.pinned = vec![5];
    g.include = vec![5];
    let tl::enums::DialogFilter::Filter(d) = folder_to_tl(&g, 1, &only5).unwrap() else {
        panic!()
    };
    assert_eq!((d.pinned_peers.len(), d.include_peers.len()), (1, 0));
    // A shared folder goes back as a chat list, not as a filter.
    g.shared = true;
    assert!(matches!(
        folder_to_tl(&g, 1, &only5).unwrap(),
        tl::enums::DialogFilter::Chatlist(_)
    ));
}

#[test]
fn folder_list_skips_the_all_chats_placeholder() {
    let r = tl::enums::messages::DialogFilters::Filters(tl::types::messages::DialogFilters {
        tags_enabled: false,
        filters: vec![
            tl::enums::DialogFilter::Default,
            tl::enums::DialogFilter::Filter(raw_filter()),
        ],
    });
    let (folders, peers) = folders_from_tl(&wire(r), 42);
    assert_eq!(folders.len(), 1);
    assert_eq!(folders[0].id, 5);
    assert!(!peers.is_empty());
}

#[test]
fn folder_updates_map_to_events() {
    let changed = tl::enums::Update::DialogFilter(tl::types::UpdateDialogFilter {
        id: 5,
        filter: Some(tl::enums::DialogFilter::Filter(raw_filter())),
    });
    match event_from_update_as(&wire(changed), 42) {
        Some(Event::FolderChanged {
            id: 5,
            folder: Some(f),
        }) => {
            assert_eq!(f.title, "Work");
            assert!(f.pinned.contains(&42));
        }
        other => panic!("{other:?}"),
    }
    let deleted = tl::enums::Update::DialogFilter(tl::types::UpdateDialogFilter {
        id: 5,
        filter: None,
    });
    assert_eq!(
        event_from_update(&wire(deleted)),
        Some(Event::FolderChanged {
            id: 5,
            folder: None
        })
    );
    let order = tl::enums::Update::DialogFilterOrder(tl::types::UpdateDialogFilterOrder {
        order: vec![0, 7, 5],
    });
    assert_eq!(
        event_from_update(&wire(order)),
        Some(Event::FoldersOrder(vec![0, 7, 5]))
    );
    assert_eq!(
        event_from_update(&tl::enums::Update::DialogFilters),
        Some(Event::FoldersStale)
    );
}

// ------------------------------------------------------------ shared media

fn doc_media(
    attrs: Vec<tl::enums::DocumentAttribute>,
    mime: &str,
    size: i64,
) -> tl::enums::MessageMedia {
    tl::enums::MessageMedia::Document(tl::types::MessageMediaDocument {
        nopremium: false,
        spoiler: false,
        video: false,
        round: false,
        voice: false,
        document: Some(tl::enums::Document::Document(tl::types::Document {
            id: 1,
            access_hash: 2,
            file_reference: vec![],
            date: 0,
            mime_type: mime.into(),
            size,
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

fn video_attr(w: i32, h: i32, secs: f64, round: bool) -> tl::enums::DocumentAttribute {
    tl::enums::DocumentAttribute::Video(tl::types::DocumentAttributeVideo {
        round_message: round,
        supports_streaming: true,
        nosound: false,
        duration: secs,
        w,
        h,
        preload_prefix_size: None,
        video_start_ts: None,
        video_codec: None,
    })
}

fn audio_attr(voice: bool, secs: i32, title: Option<&str>) -> tl::enums::DocumentAttribute {
    tl::enums::DocumentAttribute::Audio(tl::types::DocumentAttributeAudio {
        voice,
        duration: secs,
        title: title.map(str::to_owned),
        performer: title.map(|_| "Band".to_owned()),
        waveform: None,
    })
}

fn msg_with(id: i32, media: Option<tl::enums::MessageMedia>) -> tl::enums::Message {
    let mut m = raw_message(id, user(1), false, "");
    m.media = media;
    wire(tl::enums::Message::Message(m))
}

#[test]
fn shared_filters_match_the_tabs() {
    use tl::enums::MessagesFilter as F;
    assert!(matches!(
        shared_filter(SharedKind::Media),
        F::InputMessagesFilterPhotoVideo
    ));
    assert!(matches!(
        shared_filter(SharedKind::Files),
        F::InputMessagesFilterDocument
    ));
    assert!(matches!(
        shared_filter(SharedKind::Links),
        F::InputMessagesFilterUrl
    ));
    assert!(matches!(
        shared_filter(SharedKind::Music),
        F::InputMessagesFilterMusic
    ));
    assert!(matches!(
        shared_filter(SharedKind::Voice),
        F::InputMessagesFilterVoice
    ));
    assert!(matches!(
        shared_filter(SharedKind::Gifs),
        F::InputMessagesFilterGif
    ));
}

#[test]
fn photos_videos_and_gifs_become_media_items() {
    let photo = tl::enums::MessageMedia::Photo(tl::types::MessageMediaPhoto {
        spoiler: false,
        live_photo: false,
        photo: Some(tl::enums::Photo::Photo(tl::types::Photo {
            has_stickers: false,
            id: 5,
            access_hash: 1,
            file_reference: vec![],
            date: 0,
            sizes: vec![tl::enums::PhotoSize::Size(tl::types::PhotoSize {
                r#type: "x".into(),
                w: 800,
                h: 600,
                size: 10,
            })],
            video_sizes: None,
            dc_id: 2,
        })),
        ttl_seconds: None,
        video: None,
    });
    let it = shared_item_from_tl(&msg_with(10, Some(photo)), SharedKind::Media).unwrap();
    assert_eq!(it.id, 10);
    assert_eq!(
        it.body,
        SharedBody::Photo {
            width: 800,
            height: 600
        }
    );
    assert!(it.has_thumb());

    let video = doc_media(vec![video_attr(1280, 720, 83.4, false)], "video/mp4", 1_000);
    let it = shared_item_from_tl(&msg_with(11, Some(video)), SharedKind::Media).unwrap();
    assert_eq!(
        it.body,
        SharedBody::Video {
            width: 1280,
            height: 720,
            seconds: 83
        }
    );

    let gif = doc_media(
        vec![
            tl::enums::DocumentAttribute::Animated,
            video_attr(480, 270, 2.0, false),
        ],
        "video/mp4",
        5,
    );
    let it = shared_item_from_tl(&msg_with(12, Some(gif)), SharedKind::Gifs).unwrap();
    assert_eq!(
        it.body,
        SharedBody::Gif {
            width: 480,
            height: 270
        }
    );

    // A photo does not belong to the GIF tab, nor a text-only message to Media.
    assert!(shared_item_from_tl(&msg_with(13, None), SharedKind::Media).is_none());
    let plain = doc_media(vec![], "application/pdf", 1);
    assert!(shared_item_from_tl(&msg_with(14, Some(plain)), SharedKind::Gifs).is_none());
}

#[test]
fn documents_audio_and_voice_land_in_their_tabs() {
    let file = doc_media(
        vec![tl::enums::DocumentAttribute::Filename(
            tl::types::DocumentAttributeFilename {
                file_name: "trail-map.pdf".into(),
            },
        )],
        "application/pdf",
        2_457_600,
    );
    let it = shared_item_from_tl(&msg_with(1, Some(file)), SharedKind::Files).unwrap();
    assert_eq!(
        it.body,
        SharedBody::File {
            name: "trail-map.pdf".into(),
            size: 2_457_600,
            mime: "application/pdf".into()
        }
    );

    let song = doc_media(
        vec![audio_attr(false, 215, Some("Mountain Air"))],
        "audio/mpeg",
        3,
    );
    let it = shared_item_from_tl(&msg_with(2, Some(song.clone())), SharedKind::Music).unwrap();
    assert_eq!(
        it.body,
        SharedBody::Audio {
            title: Some("Mountain Air".into()),
            performer: Some("Band".into()),
            name: None,
            seconds: 215
        }
    );
    assert!(shared_item_from_tl(&msg_with(2, Some(song)), SharedKind::Voice).is_none());

    let voice = doc_media(vec![audio_attr(true, 9, None)], "audio/ogg", 3);
    let it = shared_item_from_tl(&msg_with(3, Some(voice.clone())), SharedKind::Voice).unwrap();
    assert_eq!(it.body, SharedBody::Voice { seconds: 9 });
    assert!(
        shared_item_from_tl(&msg_with(3, Some(voice)), SharedKind::Music).is_none(),
        "voice messages are not music"
    );
}

#[test]
fn links_come_from_the_preview_or_the_entities() {
    // Web page preview wins and carries title/description.
    let mut m = raw_message(20, user(1), false, "look https://example.com/a");
    m.media = Some(tl::enums::MessageMedia::WebPage(
        tl::types::MessageMediaWebPage {
            force_large_media: false,
            force_small_media: false,
            manual: false,
            safe: false,
            webpage: tl::enums::WebPage::Page(tl::types::WebPage {
                has_large_media: false,
                video_cover_photo: false,
                id: 1,
                url: "https://example.com/a".into(),
                display_url: "example.com/a".into(),
                hash: 0,
                r#type: None,
                site_name: Some("Example".into()),
                title: Some("An example".into()),
                description: Some(String::new()),
                photo: None,
                embed_url: None,
                embed_type: None,
                embed_width: None,
                embed_height: None,
                duration: None,
                author: None,
                document: None,
                cached_page: None,
                attributes: None,
            }),
        },
    ));
    let it = shared_item_from_tl(&wire(tl::enums::Message::Message(m)), SharedKind::Links).unwrap();
    assert_eq!(
        it.body,
        SharedBody::Link {
            url: "https://example.com/a".into(),
            title: Some("An example".into()),
            description: None,
            site: Some("Example".into())
        }
    );

    // No preview: a plain url entity, sliced by UTF-16 offsets (the emoji is
    // two units), and a text-url entity.
    let mut m = raw_message(21, user(1), false, "😀 see example.org/x now");
    m.entities = Some(vec![tl::enums::MessageEntity::Url(
        tl::types::MessageEntityUrl {
            offset: 7,
            length: 13,
        },
    )]);
    let it = shared_item_from_tl(&wire(tl::enums::Message::Message(m)), SharedKind::Links).unwrap();
    assert!(matches!(it.body, SharedBody::Link { ref url, .. } if url == "example.org/x"));

    let mut m = raw_message(22, user(1), false, "click here");
    m.entities = Some(vec![tl::enums::MessageEntity::TextUrl(
        tl::types::MessageEntityTextUrl {
            offset: 0,
            length: 5,
            url: "https://t.example/q".into(),
        },
    )]);
    let it = shared_item_from_tl(&wire(tl::enums::Message::Message(m)), SharedKind::Links).unwrap();
    assert!(matches!(it.body, SharedBody::Link { ref url, .. } if url == "https://t.example/q"));

    // The filter can return a message whose link we cannot read: skipped.
    let m = raw_message(23, user(1), false, "no link");
    assert!(shared_item_from_tl(&tl::enums::Message::Message(m), SharedKind::Links).is_none());
}

#[test]
fn utf16_slicing_follows_telegram_offsets() {
    assert_eq!(utf16_slice("abc def", 4, 3).as_deref(), Some("def"));
    assert_eq!(utf16_slice("😀 x.com", 3, 5).as_deref(), Some("x.com"));
    assert_eq!(utf16_slice("😀x", 1, 1), None, "inside a surrogate pair");
    assert_eq!(utf16_slice("abc", 2, 5), None, "past the end");
    assert_eq!(utf16_slice("abc", -1, 2), None);
    assert_eq!(utf16_slice("abc", 0, 0), None);
}

#[test]
fn pages_know_where_the_next_one_starts() {
    let photo_doc = |id| msg_with(id, Some(doc_media(vec![], "application/pdf", 1)));
    // A full page: the cursor is the oldest raw id, even when some results
    // were not convertible for the tab.
    let msgs = vec![photo_doc(90), msg_with(80, None), photo_doc(70)];
    let page = shared_page_from_tl(&msgs, SharedKind::Files, 3);
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.next, Some(70));
    // A short page is the end.
    let page = shared_page_from_tl(&msgs, SharedKind::Files, 40);
    assert_eq!(page.next, None);
}

// ---------------------------------------------------------------- calls

mod calls {
    use super::wire;
    use ferrogram::net::Event;
    use ferrogram::net::calls::{CallRef, Connection, DiscardReason, PhoneCall, Protocol};
    use ferrogram::net::types::{
        call_label, discard_reason_to_tl, event_from_update, phone_call_from_tl, protocol_to_tl,
    };
    use grammers_client::tl;

    fn protocol() -> tl::enums::PhoneCallProtocol {
        protocol_to_tl(&Protocol::ours())
    }

    #[test]
    fn protocol_round_trips() {
        let p = wire(protocol());
        let tl::enums::PhoneCallProtocol::Protocol(p) = &p;
        assert!(p.udp_p2p && p.udp_reflector);
        assert_eq!((p.min_layer, p.max_layer), (65, 92));
        assert_eq!(p.library_versions, vec!["9.0.0", "8.0.0"]);
    }

    #[test]
    fn established_call_with_connections() {
        let raw = wire(tl::enums::PhoneCall::Call(tl::types::PhoneCall {
            p2p_allowed: true,
            video: false,
            conference_supported: false,
            id: 77,
            access_hash: -5,
            date: 1,
            admin_id: 10,
            participant_id: 20,
            g_a_or_b: vec![3; 256],
            key_fingerprint: -42,
            protocol: protocol(),
            connections: vec![
                tl::enums::PhoneConnection::Connection(tl::types::PhoneConnection {
                    tcp: false,
                    id: 9,
                    ip: "149.154.167.1".into(),
                    ipv6: String::new(),
                    port: 599,
                    peer_tag: vec![1; 16],
                }),
                tl::enums::PhoneConnection::Webrtc(tl::types::PhoneConnectionWebrtc {
                    turn: true,
                    stun: true,
                    id: 11,
                    ip: "1.2.3.4".into(),
                    ipv6: String::new(),
                    port: 3478,
                    username: "user".into(),
                    password: "secret-pass".into(),
                }),
            ],
            start_date: 2,
            custom_parameters: Some(tl::enums::DataJson::Json(tl::types::DataJson {
                data: r#"{"network_standalone_reflectors":true}"#.into(),
            })),
        }));
        let pc = phone_call_from_tl(&raw);
        let PhoneCall::Active {
            call,
            key_fingerprint,
            connections,
            p2p_allowed,
            custom_parameters,
            ..
        } = &pc
        else {
            panic!("{pc:?}")
        };
        assert_eq!(
            *call,
            CallRef {
                id: 77,
                access_hash: -5
            }
        );
        assert_eq!(*key_fingerprint, -42);
        assert!(*p2p_allowed);
        assert_eq!(connections.len(), 2);
        assert!(matches!(
            connections[0],
            Connection::Reflector {
                id: 9,
                port: 599,
                ..
            }
        ));
        assert!(custom_parameters.as_deref().unwrap().contains("standalone"));
        // Debug output never shows TURN passwords or peer tags.
        assert!(!format!("{pc:?}").contains("secret-pass"));
    }

    #[test]
    fn updates_become_events() {
        let up = wire(tl::enums::Update::PhoneCall(tl::types::UpdatePhoneCall {
            phone_call: tl::enums::PhoneCall::Discarded(tl::types::PhoneCallDiscarded {
                need_rating: true,
                need_debug: false,
                video: false,
                id: 5,
                reason: Some(discard_reason_to_tl(DiscardReason::Busy)),
                duration: None,
            }),
        }));
        assert_eq!(
            event_from_update(&up),
            Some(Event::PhoneCall(PhoneCall::Discarded {
                id: 5,
                reason: Some(DiscardReason::Busy),
                duration: None,
                need_rating: true,
            }))
        );
        let sig = wire(tl::enums::Update::PhoneCallSignalingData(
            tl::types::UpdatePhoneCallSignalingData {
                phone_call_id: 5,
                data: vec![1, 2, 3],
            },
        ));
        assert_eq!(
            event_from_update(&sig),
            Some(Event::CallSignaling {
                call_id: 5,
                data: vec![1, 2, 3]
            })
        );
        let req = wire(tl::enums::PhoneCall::Requested(
            tl::types::PhoneCallRequested {
                video: false,
                id: 6,
                access_hash: 7,
                date: 0,
                admin_id: 1,
                participant_id: 2,
                g_a_hash: vec![9; 32],
                protocol: protocol(),
            },
        ));
        assert!(matches!(
            phone_call_from_tl(&req),
            PhoneCall::Requested { admin_id: 1, .. }
        ));
    }

    #[test]
    fn history_labels() {
        assert_eq!(
            call_label(true, Some(DiscardReason::Hangup), Some(65)),
            "Outgoing call (1:05)"
        );
        assert_eq!(
            call_label(false, Some(DiscardReason::Hangup), Some(3725)),
            "Incoming call (1:02:05)"
        );
        assert_eq!(
            call_label(false, Some(DiscardReason::Missed), None),
            "Missed call"
        );
        assert_eq!(
            call_label(false, Some(DiscardReason::Busy), None),
            "Missed call"
        );
        assert_eq!(
            call_label(true, Some(DiscardReason::Missed), None),
            "Cancelled call"
        );
        assert_eq!(
            call_label(true, Some(DiscardReason::Busy), None),
            "Declined call"
        );
    }
}
