//! Conversion of raw TL types (grammers-tl-types) into domain types.
//!
//! Everything here is a pure function over `tl::enums::*` so it can be unit
//! tested with hand-built fixtures, without a connection or a `Client`.

use grammers_client::tl;

use super::Event;
use crate::model::{
    CHANNEL_OFFSET, ChatId, Media, Message, Presence, Reaction, SendState, TypingAction,
};

/// `tl::Peer` → Bot-API style chat id.
pub fn peer_to_chat_id(peer: &tl::enums::Peer) -> ChatId {
    match peer {
        tl::enums::Peer::User(u) => u.user_id,
        tl::enums::Peer::Chat(c) => -c.chat_id,
        tl::enums::Peer::Channel(c) => channel_chat_id(c.channel_id),
    }
}

pub fn channel_chat_id(channel_id: i64) -> ChatId {
    -CHANNEL_OFFSET - channel_id
}

/// Convert a raw message. `None` for `messageEmpty`.
pub fn message_from_tl(raw: &tl::enums::Message) -> Option<Message> {
    match raw {
        tl::enums::Message::Empty(_) => None,
        tl::enums::Message::Message(m) => {
            let chat_id = peer_to_chat_id(&m.peer_id);
            Some(Message {
                chat_id,
                id: m.id,
                sender_id: m.from_id.as_ref().map(peer_to_chat_id).or(if m.out {
                    None
                } else {
                    Some(chat_id).filter(|c| *c > 0)
                }),
                sender_name: m.post_author.clone(),
                outgoing: m.out,
                date: i64::from(m.date),
                text: m.message.clone(),
                media: m.media.as_ref().and_then(media_from_tl),
                reply_to: m.reply_to.as_ref().and_then(reply_to_id),
                edited: m.edit_date.is_some() && !m.edit_hide,
                reactions: m
                    .reactions
                    .as_ref()
                    .map(reactions_from_tl)
                    .unwrap_or_default(),
                service: false,
                state: SendState::Sent,
            })
        }
        tl::enums::Message::Service(m) => Some(Message {
            chat_id: peer_to_chat_id(&m.peer_id),
            id: m.id,
            sender_id: m.from_id.as_ref().map(peer_to_chat_id),
            sender_name: None,
            outgoing: m.out,
            date: i64::from(m.date),
            text: service_text(&m.action),
            media: None,
            reply_to: m.reply_to.as_ref().and_then(reply_to_id),
            edited: false,
            reactions: Vec::new(),
            service: true,
            state: SendState::Sent,
        }),
    }
}

fn reply_to_id(h: &tl::enums::MessageReplyHeader) -> Option<i32> {
    match h {
        tl::enums::MessageReplyHeader::Header(h) => h.reply_to_msg_id,
        _ => None,
    }
}

pub fn service_text(action: &tl::enums::MessageAction) -> String {
    use tl::enums::MessageAction as A;
    match action {
        A::ChatCreate(a) => format!("Group “{}” created", a.title),
        A::ChannelCreate(a) => format!("Channel “{}” created", a.title),
        A::ChatEditTitle(a) => format!("Title changed to “{}”", a.title),
        A::ChatEditPhoto(_) => "Group photo updated".into(),
        A::ChatAddUser(_) => "New member added".into(),
        A::ChatDeleteUser(_) => "A member left the group".into(),
        A::ChatJoinedByLink(_) | A::ChatJoinedByRequest => "A member joined via invite link".into(),
        A::PinMessage => "Message pinned".into(),
        A::PhoneCall(_) => "Call".into(),
        A::ContactSignUp => "Joined Telegram".into(),
        A::HistoryClear => "History cleared".into(),
        _ => "Service message".into(),
    }
}

pub fn media_from_tl(media: &tl::enums::MessageMedia) -> Option<Media> {
    use tl::enums::MessageMedia as M;
    match media {
        M::Empty | M::Unsupported => None,
        M::Photo(p) => match &p.photo {
            Some(tl::enums::Photo::Photo(photo)) => {
                let (width, height) = largest_size(&photo.sizes);
                Some(Media::Photo {
                    id: photo.id,
                    width,
                    height,
                })
            }
            _ => Some(Media::Other {
                label: "Photo (expired)".into(),
            }),
        },
        M::Document(d) => match &d.document {
            Some(tl::enums::Document::Document(doc)) => Some(document_media(doc, d.voice, d.round)),
            _ => Some(Media::Other {
                label: "File (expired)".into(),
            }),
        },
        M::Geo(_) | M::GeoLive(_) => Some(Media::Other {
            label: "Location".into(),
        }),
        M::Venue(v) => Some(Media::Other {
            label: format!("Venue: {}", v.title),
        }),
        M::Contact(_) => Some(Media::Other {
            label: "Contact".into(),
        }),
        M::Poll(_) => Some(Media::Other {
            label: "Poll".into(),
        }),
        M::Dice(d) => Some(Media::Other {
            label: format!("{} {}", d.emoticon, d.value),
        }),
        // Link previews are rendered as plain text links, like a message
        // without media.
        M::WebPage(_) => None,
        _ => Some(Media::Other {
            label: "Unsupported media".into(),
        }),
    }
}

fn document_media(doc: &tl::types::Document, voice: bool, round: bool) -> Media {
    let mut name = None;
    let mut sticker = None;
    let mut video = false;
    let mut audio = false;
    for attr in &doc.attributes {
        match attr {
            tl::enums::DocumentAttribute::Filename(f) => name = Some(f.file_name.clone()),
            tl::enums::DocumentAttribute::Sticker(s) => sticker = Some(s.alt.clone()),
            tl::enums::DocumentAttribute::Video(_) => video = true,
            tl::enums::DocumentAttribute::Audio(_) => audio = true,
            _ => {}
        }
    }
    if let Some(emoji) = sticker {
        return Media::Sticker { emoji };
    }
    if voice {
        return Media::Other {
            label: "Voice message".into(),
        };
    }
    if round {
        return Media::Other {
            label: "Video message".into(),
        };
    }
    let fallback = if video {
        "Video"
    } else if audio {
        "Audio"
    } else {
        "File"
    };
    Media::File {
        name: name.unwrap_or_else(|| fallback.into()),
        size: u64::try_from(doc.size).unwrap_or(0),
        mime: doc.mime_type.clone(),
    }
}

/// (w, h) of the largest available size of a photo.
pub fn largest_size(sizes: &[tl::enums::PhotoSize]) -> (u32, u32) {
    let mut best = (0i32, 0i32);
    for s in sizes {
        let (w, h) = match s {
            tl::enums::PhotoSize::Size(s) => (s.w, s.h),
            tl::enums::PhotoSize::PhotoCachedSize(s) => (s.w, s.h),
            tl::enums::PhotoSize::Progressive(s) => (s.w, s.h),
            _ => (0, 0),
        };
        if w * h > best.0 * best.1 {
            best = (w, h);
        }
    }
    (
        u32::try_from(best.0).unwrap_or(0),
        u32::try_from(best.1).unwrap_or(0),
    )
}

pub fn reactions_from_tl(r: &tl::enums::MessageReactions) -> Vec<Reaction> {
    let tl::enums::MessageReactions::Reactions(r) = r;
    r.results
        .iter()
        .filter_map(|rc| {
            let tl::enums::ReactionCount::Count(rc) = rc;
            let emoji = match &rc.reaction {
                tl::enums::Reaction::Emoji(e) => e.emoticon.clone(),
                // Custom emoji need their sticker set; show a generic star.
                tl::enums::Reaction::CustomEmoji(_) => "⭐".into(),
                _ => return None,
            };
            Some(Reaction {
                emoji,
                count: u32::try_from(rc.count).unwrap_or(0),
                chosen: rc.chosen_order.is_some(),
            })
        })
        .collect()
}

/// Map a raw update into a domain event. `None` for updates we ignore.
pub fn event_from_update(update: &tl::enums::Update) -> Option<Event> {
    use tl::enums::Update as U;
    match update {
        U::NewMessage(u) => message_from_tl(&u.message).map(Event::NewMessage),
        U::NewChannelMessage(u) => message_from_tl(&u.message).map(Event::NewMessage),
        U::EditMessage(u) => message_from_tl(&u.message).map(Event::MessageEdited),
        U::EditChannelMessage(u) => message_from_tl(&u.message).map(Event::MessageEdited),
        U::DeleteMessages(u) => Some(Event::MessagesDeleted {
            chat: None,
            ids: u.messages.clone(),
        }),
        U::DeleteChannelMessages(u) => Some(Event::MessagesDeleted {
            chat: Some(channel_chat_id(u.channel_id)),
            ids: u.messages.clone(),
        }),
        U::MessageReactions(u) => Some(Event::Reactions {
            chat: peer_to_chat_id(&u.peer),
            id: u.msg_id,
            reactions: reactions_from_tl(&u.reactions),
        }),
        U::ReadHistoryOutbox(u) => Some(Event::ReadOutbox {
            chat: peer_to_chat_id(&u.peer),
            max_id: u.max_id,
        }),
        U::ReadChannelOutbox(u) => Some(Event::ReadOutbox {
            chat: channel_chat_id(u.channel_id),
            max_id: u.max_id,
        }),
        U::ReadHistoryInbox(u) => Some(Event::ReadInbox {
            chat: peer_to_chat_id(&u.peer),
            max_id: u.max_id,
            still_unread: u32::try_from(u.still_unread_count).unwrap_or(0),
        }),
        U::ReadChannelInbox(u) => Some(Event::ReadInbox {
            chat: channel_chat_id(u.channel_id),
            max_id: u.max_id,
            still_unread: u32::try_from(u.still_unread_count).unwrap_or(0),
        }),
        U::UserTyping(u) => Some(typing_event(u.user_id, u.user_id, &u.action)),
        U::ChatUserTyping(u) => {
            user_of(&u.from_id).map(|user| typing_event(-u.chat_id, user, &u.action))
        }
        U::ChannelUserTyping(u) if u.top_msg_id.is_none() => user_of(&u.from_id)
            .map(|user| typing_event(channel_chat_id(u.channel_id), user, &u.action)),
        U::UserStatus(u) => Some(Event::UserStatus {
            user: u.user_id,
            presence: presence_from_tl(&u.status, chrono::Utc::now().timestamp()),
        }),
        _ => None,
    }
}

fn user_of(p: &tl::enums::Peer) -> Option<i64> {
    match p {
        tl::enums::Peer::User(u) => Some(u.user_id),
        _ => None,
    }
}

fn typing_event(chat: ChatId, user: i64, action: &tl::enums::SendMessageAction) -> Event {
    Event::Typing {
        chat,
        user,
        action: typing_action_from_tl(action),
    }
}

/// `None` means the action ended (cancel / stop).
pub fn typing_action_from_tl(a: &tl::enums::SendMessageAction) -> Option<TypingAction> {
    use tl::enums::SendMessageAction as A;
    Some(match a {
        A::SendMessageCancelAction | A::SendMessageStopDraftAction(_) => return None,
        A::SendMessageTypingAction => TypingAction::Typing,
        A::SendMessageRecordAudioAction => TypingAction::RecordingVoice,
        A::SendMessageRecordVideoAction | A::SendMessageRecordRoundAction => {
            TypingAction::RecordingVideo
        }
        A::SendMessageUploadPhotoAction(_) => TypingAction::UploadingPhoto,
        A::SendMessageUploadVideoAction(_) | A::SendMessageUploadRoundAction(_) => {
            TypingAction::UploadingVideo
        }
        A::SendMessageUploadDocumentAction(_) | A::SendMessageUploadAudioAction(_) => {
            TypingAction::UploadingFile
        }
        A::SendMessageChooseStickerAction => TypingAction::ChoosingSticker,
        _ => TypingAction::Other,
    })
}

/// Telegram `UserStatus` → domain presence. A stale "online" (its `expires`
/// already passed) becomes "last seen at `expires`".
pub fn presence_from_tl(s: &tl::enums::UserStatus, now: i64) -> Presence {
    use tl::enums::UserStatus as S;
    match s {
        S::Empty => Presence::Unknown,
        S::Online(o) => {
            let until = i64::from(o.expires);
            if until > now {
                Presence::Online { until }
            } else {
                Presence::Offline { was_online: until }
            }
        }
        S::Offline(o) => Presence::Offline {
            was_online: i64::from(o.was_online),
        },
        S::Recently(_) => Presence::Recently,
        S::LastWeek(_) => Presence::LastWeek,
        S::LastMonth(_) => Presence::LastMonth,
    }
}

/// Per-dialog counters from a raw `tl::Dialog`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DialogInfo {
    pub unread: u32,
    pub pinned: bool,
    pub muted: bool,
    pub read_inbox: i32,
    pub read_outbox: i32,
}

pub fn dialog_info(raw: &tl::enums::Dialog, now: i64) -> DialogInfo {
    match raw {
        tl::enums::Dialog::Dialog(d) => {
            let tl::enums::PeerNotifySettings::Settings(ns) = &d.notify_settings;
            DialogInfo {
                unread: u32::try_from(d.unread_count).unwrap_or(0),
                pinned: d.pinned,
                muted: ns.mute_until.is_some_and(|t| i64::from(t) > now),
                read_inbox: d.read_inbox_max_id,
                read_outbox: d.read_outbox_max_id,
            }
        }
        _ => DialogInfo::default(),
    }
}

/// Map an RPC error (code, NAME, value) to a crate error.
pub fn rpc_error(code: i32, name: &str, value: Option<u32>) -> crate::error::Error {
    use crate::error::{AuthError, Error};
    match name {
        "PHONE_NUMBER_INVALID" | "PHONE_NUMBER_BANNED" | "PHONE_NUMBER_FLOOD" => {
            Error::Auth(AuthError::InvalidPhone)
        }
        "PHONE_CODE_INVALID" | "PHONE_CODE_EXPIRED" | "PHONE_CODE_EMPTY" => {
            Error::Auth(AuthError::InvalidCode)
        }
        "PASSWORD_HASH_INVALID" => Error::Auth(AuthError::InvalidPassword),
        "FLOOD_WAIT" | "FLOOD_PREMIUM_WAIT" => {
            Error::Auth(AuthError::FloodWait(value.unwrap_or(0)))
        }
        "AUTH_KEY_UNREGISTERED"
        | "SESSION_REVOKED"
        | "SESSION_EXPIRED"
        | "USER_DEACTIVATED"
        | "USER_DEACTIVATED_BAN"
        | "AUTH_KEY_DUPLICATED" => Error::Auth(AuthError::SessionRevoked),
        _ if code == 401 => Error::Auth(AuthError::SessionRevoked),
        _ => Error::Protocol(format!("{code} {name}")),
    }
}
