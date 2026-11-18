//! Lucide icons (ISC licence, see assets/icons/LICENSE.txt), embedded.

use iced::widget::svg;

macro_rules! icons {
    ($($name:ident => $file:literal),* $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Icon { $($name),* }

        impl Icon {
            pub fn bytes(self) -> &'static [u8] {
                match self {
                    $(Self::$name => include_bytes!(concat!("../../assets/icons/", $file))),*
                }
            }
        }
    };
}

icons! {
    Search => "search.svg",
    Menu => "menu.svg",
    Paperclip => "paperclip.svg",
    Smile => "smile.svg",
    Send => "send.svg",
    Check => "check.svg",
    CheckCheck => "check-check.svg",
    X => "x.svg",
    Reply => "reply.svg",
    Pencil => "pencil.svg",
    Trash => "trash-2.svg",
    Phone => "phone.svg",
    More => "ellipsis-vertical.svg",
    File => "file.svg",
    Image => "image.svg",
    Lock => "lock.svg",
    ArrowLeft => "arrow-left.svg",
    Users => "users.svg",
    Megaphone => "megaphone.svg",
    Bookmark => "bookmark.svg",
    SmilePlus => "smile-plus.svg",
    Copy => "copy.svg",
    Muted => "volume-x.svg",
    Pin => "pin.svg",
    Bot => "bot.svg",
    Download => "download.svg",
    ArrowDown => "arrow-down.svg",
}

impl Icon {
    pub fn handle(self) -> svg::Handle {
        svg::Handle::from_memory(self.bytes())
    }
}

/// Speech-bubble tail (original artwork) drawn next to the last bubble of a
/// group. White, tinted with the bubble colour at render time.
pub const TAIL_IN: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="9" height="17" viewBox="0 0 9 17"><path d="M9 0 V17 H1.2 C0.2 17 -0.3 15.9 0.4 15.2 C4.5 11.2 9 7.5 9 0 Z" fill="#fff"/></svg>"##;
pub const TAIL_OUT: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="9" height="17" viewBox="0 0 9 17"><path d="M0 0 V17 H7.8 C8.8 17 9.3 15.9 8.6 15.2 C4.5 11.2 0 7.5 0 0 Z" fill="#fff"/></svg>"##;

/// Original Ferrogram mark for the login screen (a speech bubble with an
/// iron-grey hexagon). Deliberately not Telegram's paper plane.
pub const LOGO: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="120" viewBox="0 0 120 120"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#6aa8dc"/><stop offset="1" stop-color="#3b6e9e"/></linearGradient></defs><circle cx="60" cy="60" r="60" fill="url(#g)"/><path d="M30 40 Q30 30 40 30 H80 Q90 30 90 40 V70 Q90 80 80 80 H52 L38 92 L40 80 Q30 80 30 70 Z" fill="#fff"/><path d="M60 41 L72 48 V62 L60 69 L48 62 V48 Z" fill="#5d6b78"/></svg>"##;
