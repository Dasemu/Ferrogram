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

/// Original Ferrogram mark (also the app icon, `packaging/`): a speech
/// bubble with an iron-grey hexagon. Deliberately not Telegram's paper plane.
pub const LOGO: &[u8] = include_bytes!("../../packaging/dev.ferro.ferrogram.svg");
