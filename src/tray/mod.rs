//! System tray icon (StatusNotifierItem on Linux).
//!
//! `TrayModel` and `Entry` are plain data: what the icon, tooltip and menu
//! should show, and which [`Remote`] request each menu entry sends. They are
//! unit-tested here; `sni.rs` only translates them to the D-Bus service.
//! Only Linux has an implementation; Windows and macOS compile a stub whose
//! `spawn` fails, so the app behaves as before there (closing quits).

use std::sync::Arc;

use crate::config::APP_NAME;
use crate::remote::{MuteRequest, Remote};

pub mod icon;
#[cfg(target_os = "linux")]
mod sni;
#[cfg(not(target_os = "linux"))]
mod stub;

#[cfg(target_os = "linux")]
pub use sni::{TrayHandle, spawn};
#[cfg(not(target_os = "linux"))]
pub use stub::{TrayHandle, spawn};

/// Where menu clicks and clicks on the icon go (normally `remote::send`).
pub type Sink = Arc<dyn Fn(Remote) + Send + Sync>;

/// Everything the tray shows. Cheap to compare so the app only talks to the
/// panel when something visible changed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TrayModel {
    /// Unread messages in unmuted chats.
    pub unread: u32,
    /// The window is currently shown.
    pub window_open: bool,
    /// An incoming call is ringing.
    pub ringing: bool,
    /// Notifications are muted globally; text for the tooltip ("until 15:30").
    pub muted: Option<String>,
    pub start_minimized: bool,
}

/// One row of the context menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Item {
        label: String,
        request: Remote,
    },
    Check {
        label: String,
        checked: bool,
        request: Remote,
    },
    Separator,
}

impl TrayModel {
    /// Unread messages in unmuted chats turn the icon to "needs attention".
    pub fn needs_attention(&self) -> bool {
        self.unread > 0 || self.ringing
    }

    /// `Ferrogram — 3 unread` (just `Ferrogram` when nothing is unread).
    pub fn tooltip_title(&self) -> String {
        if self.ringing {
            format!("{APP_NAME} — incoming call")
        } else if self.unread == 0 {
            APP_NAME.to_owned()
        } else {
            format!("{APP_NAME} — {} unread", self.unread)
        }
    }

    pub fn tooltip_text(&self) -> String {
        match &self.muted {
            Some(until) => format!("Notifications muted {until}"),
            None => String::new(),
        }
    }

    /// Show/Hide, mute, start minimized, Quit (what Telegram Desktop offers).
    pub fn menu(&self) -> Vec<Entry> {
        let item = |label: &str, request| Entry::Item {
            label: label.to_owned(),
            request,
        };
        let mut menu = vec![
            item(
                if self.window_open {
                    "Hide Ferrogram"
                } else {
                    "Show Ferrogram"
                },
                Remote::Toggle,
            ),
            Entry::Separator,
        ];
        if self.muted.is_some() {
            menu.push(item("Unmute notifications", Remote::Mute(MuteRequest::Off)));
        } else {
            menu.push(item(
                "Mute notifications for 1 hour",
                Remote::Mute(MuteRequest::OneHour),
            ));
            menu.push(item(
                "Mute notifications until re-enabled",
                Remote::Mute(MuteRequest::UntilEnabled),
            ));
        }
        menu.push(Entry::Check {
            label: "Start minimized to tray".into(),
            checked: self.start_minimized,
            request: Remote::ToggleStartMinimized,
        });
        menu.push(Entry::Separator);
        menu.push(item("Quit", Remote::Quit));
        menu
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ringing_call_needs_attention() {
        let m = TrayModel {
            ringing: true,
            ..model()
        };
        assert!(m.needs_attention());
        assert!(m.tooltip_title().contains("incoming call"));
    }

    fn model() -> TrayModel {
        TrayModel::default()
    }

    #[test]
    fn tooltip_counts_unread() {
        assert_eq!(model().tooltip_title(), "Ferrogram");
        let m = TrayModel {
            unread: 12,
            ..model()
        };
        assert_eq!(m.tooltip_title(), "Ferrogram — 12 unread");
        assert!(m.needs_attention());
        assert!(!model().needs_attention());
    }

    #[test]
    fn tooltip_mentions_mute() {
        assert_eq!(model().tooltip_text(), "");
        let m = TrayModel {
            muted: Some("until re-enabled".into()),
            ..model()
        };
        assert_eq!(m.tooltip_text(), "Notifications muted until re-enabled");
    }

    fn requests(menu: &[Entry]) -> Vec<Remote> {
        menu.iter()
            .filter_map(|e| match e {
                Entry::Item { request, .. } | Entry::Check { request, .. } => Some(*request),
                Entry::Separator => None,
            })
            .collect()
    }

    #[test]
    fn menu_maps_entries_to_requests() {
        let menu = model().menu();
        assert_eq!(
            requests(&menu),
            vec![
                Remote::Toggle,
                Remote::Mute(MuteRequest::OneHour),
                Remote::Mute(MuteRequest::UntilEnabled),
                Remote::ToggleStartMinimized,
                Remote::Quit,
            ]
        );
        assert!(matches!(&menu[0], Entry::Item { label, .. } if label == "Show Ferrogram"));
        assert_eq!(
            menu.last(),
            Some(&Entry::Item {
                label: "Quit".into(),
                request: Remote::Quit
            })
        );
    }

    #[test]
    fn menu_follows_the_state() {
        let open = TrayModel {
            window_open: true,
            start_minimized: true,
            muted: Some("for 1 hour".into()),
            ..model()
        };
        let menu = open.menu();
        assert!(matches!(&menu[0], Entry::Item { label, .. } if label == "Hide Ferrogram"));
        assert!(menu.iter().any(|e| matches!(
            e,
            Entry::Item { label, request: Remote::Mute(MuteRequest::Off) } if label == "Unmute notifications"
        )));
        assert!(!requests(&menu).contains(&Remote::Mute(MuteRequest::OneHour)));
        assert!(
            menu.iter()
                .any(|e| matches!(e, Entry::Check { checked: true, .. }))
        );
    }
}
