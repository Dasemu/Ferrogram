//! Desktop integration: notifications via `notify-rust` (D-Bus on Linux).
//!
//! One worker thread drains a small bounded queue. Notifications that pile
//! up within a short window are coalesced into a single "N new messages"
//! popup, and when the queue is full new ones are dropped instead of
//! spawning a thread per message.

use std::sync::OnceLock;
#[cfg(all(unix, not(target_os = "macos")))]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::time::Duration;

use crate::config::{APP_ID, APP_NAME};
#[cfg(all(unix, not(target_os = "macos")))]
use crate::remote::Remote;

const QUEUE: usize = 16;
const COALESCE_WINDOW: Duration = Duration::from_millis(800);
/// Notifications waiting for a click at once; beyond this they are shown
/// without a click action instead of parking more threads.
#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
const MAX_CLICK_WAITERS: usize = 4;

/// One desktop notification: text, and the chat a click should open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub title: String,
    pub body: String,
    pub chat: Option<i64>,
}

/// Escape text for the freedesktop notification body (a subset of HTML
/// markup is interpreted by many daemons, including Plasma's).
pub fn escape_markup(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// What to show for a batch of queued notifications. A click opens the chat
/// only when the whole batch is from one chat.
pub fn coalesce(batch: &[Toast]) -> Option<Toast> {
    match batch {
        [] => None,
        [one] => Some(one.clone()),
        many => {
            let mut chats: Vec<&str> = many.iter().map(|t| t.title.as_str()).collect();
            chats.dedup();
            let title = if chats.len() == 1 {
                chats[0].to_owned()
            } else {
                APP_NAME.to_owned()
            };
            let chat = many[0]
                .chat
                .filter(|c| many.iter().all(|t| t.chat == Some(*c)));
            Some(Toast {
                title,
                body: format!(
                    "{} new messages{}",
                    many.len(),
                    if chats.len() > 1 {
                        format!(" in {} chats", chats.len())
                    } else {
                        String::new()
                    }
                ),
                chat,
            })
        }
    }
}

fn sender() -> &'static SyncSender<Toast> {
    static TX: OnceLock<SyncSender<Toast>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = sync_channel(QUEUE);
        std::thread::Builder::new()
            .name("notify".into())
            .spawn(move || worker(rx))
            .map_err(|_| tracing::debug!("notification worker unavailable"))
            .ok();
        tx
    })
}

fn worker(rx: Receiver<Toast>) {
    while let Ok(first) = rx.recv() {
        let mut batch = vec![first];
        std::thread::sleep(COALESCE_WINDOW);
        while let Ok(more) = rx.try_recv() {
            batch.push(more);
        }
        if let Some(toast) = coalesce(&batch) {
            show(&toast);
        }
    }
}

fn show(toast: &Toast) {
    let mut n = notify_rust::Notification::new();
    n.appname(APP_NAME)
        .summary(&escape_markup(&toast.title))
        .body(&escape_markup(&toast.body))
        .icon("mail-message-new");
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        n.hint(notify_rust::Hint::DesktopEntry(APP_ID.into()))
            .hint(notify_rust::Hint::Category("im.received".into()));
        show_clickable(n, toast.chat);
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    {
        let _ = APP_ID;
        if n.show().is_err() {
            tracing::debug!("notification daemon unavailable");
        }
    }
}

/// Show with a default "click" action: clicking the popup brings the window
/// back (it may be in the tray) and opens the chat. Waiting for the click
/// parks a thread until the popup closes, so only a few are allowed at once;
/// beyond that the popup is shown without the click action.
#[cfg(all(unix, not(target_os = "macos")))]
fn show_clickable(mut n: notify_rust::Notification, chat: Option<i64>) {
    let slots = click_slots();
    let waiting = slots.fetch_add(1, Ordering::AcqRel) < MAX_CLICK_WAITERS;
    if waiting {
        n.action("default", "Open");
    } else {
        slots.fetch_sub(1, Ordering::AcqRel);
    }
    let handle = match n.show() {
        Ok(h) => h,
        Err(_) => {
            tracing::debug!("notification daemon unavailable");
            if waiting {
                slots.fetch_sub(1, Ordering::AcqRel);
            }
            return;
        }
    };
    if !waiting {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("notify-click".into())
        .spawn(move || {
            handle.wait_for_action(|action| {
                if action == "default" {
                    crate::remote::send(Remote::Show { chat });
                }
            });
            click_slots().fetch_sub(1, Ordering::AcqRel);
        });
    if spawned.is_err() {
        slots.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn click_slots() -> &'static AtomicUsize {
    static SLOTS: AtomicUsize = AtomicUsize::new(0);
    &SLOTS
}

/// Queue a notification; never blocks the UI thread.
pub fn notify(toast: Toast) {
    match sender().try_send(toast) {
        Ok(()) | Err(TrySendError::Full(_)) => {}
        Err(TrySendError::Disconnected(_)) => tracing::debug!("notification worker gone"),
    }
}

pub const DESKTOP_ENTRY: &str = include_str!("../packaging/dev.ferro.ferrogram.desktop");
pub const APP_ICON_SVG: &str = include_str!("../packaging/dev.ferro.ferrogram.svg");

/// The desktop entry with `Exec` pointing at `exec` (quoted per the spec).
pub fn desktop_entry_for(exec: &std::path::Path) -> String {
    let quoted = format!(
        "\"{}\"",
        exec.display()
            .to_string()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    );
    DESKTOP_ENTRY
        .lines()
        .map(|l| {
            if l.starts_with("Exec=") {
                format!("Exec={quoted}")
            } else {
                l.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

/// `--install-desktop-entry`: write the .desktop file and the SVG icon under
/// `data_home` (normally `~/.local/share`). Returns the two paths.
pub fn install_desktop_entry(
    data_home: &std::path::Path,
    exec: &std::path::Path,
) -> std::io::Result<(std::path::PathBuf, std::path::PathBuf)> {
    let apps = data_home.join("applications");
    let icons = data_home.join("icons/hicolor/scalable/apps");
    std::fs::create_dir_all(&apps)?;
    std::fs::create_dir_all(&icons)?;
    let entry = apps.join(format!("{APP_ID}.desktop"));
    let icon = icons.join(format!("{APP_ID}.svg"));
    std::fs::write(&entry, desktop_entry_for(exec))?;
    std::fs::write(&icon, APP_ICON_SVG)?;
    Ok((entry, icon))
}

/// Open a downloaded file with the system's default handler (`xdg-open`,
/// `open`, `ShellExecute`) through the `open` crate: the path is passed as a
/// single argument, never through a shell. Refuses save-only (executable)
/// names even if a caller forgot to check.
pub fn open_file(path: &std::path::Path) -> crate::error::Result<()> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if crate::media::is_save_only(&name) {
        return Err(crate::error::Error::Protocol("save-only file type".into()));
    }
    open::that_detached(path).map_err(|e| crate::error::Error::Io(format!("open: {}", e.kind())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_entry_install() {
        let dir = tempfile::tempdir().unwrap();
        let exe = std::path::Path::new("/opt/Ferro gram/ferrogram");
        let (entry, icon) = install_desktop_entry(dir.path(), exe).unwrap();
        let text = std::fs::read_to_string(&entry).unwrap();
        assert!(
            text.contains("Exec=\"/opt/Ferro gram/ferrogram\""),
            "{text}"
        );
        assert!(text.contains("Icon=dev.ferro.ferrogram"));
        assert!(text.contains("StartupWMClass=dev.ferro.ferrogram"));
        assert!(entry.ends_with("applications/dev.ferro.ferrogram.desktop"));
        assert!(icon.ends_with("icons/hicolor/scalable/apps/dev.ferro.ferrogram.svg"));
        assert!(std::fs::read_to_string(icon).unwrap().contains("<svg"));
    }

    #[test]
    fn chat_titles_are_escaped_too() {
        // A chat called "<b>Evil</b> & co" must not inject markup.
        assert_eq!(
            escape_markup("<b>Evil</b> & co"),
            "&lt;b&gt;Evil&lt;/b&gt; &amp; co"
        );
    }

    #[test]
    fn markup_is_escaped() {
        assert_eq!(
            escape_markup("<b>hi</b> & \"x\""),
            "&lt;b&gt;hi&lt;/b&gt; &amp; &quot;x&quot;"
        );
    }

    #[test]
    fn batches_are_coalesced() {
        let toast = |t: &str, b: &str, chat| Toast {
            title: t.into(),
            body: b.into(),
            chat: Some(chat),
        };
        assert!(coalesce(&[]).is_none());
        let one = vec![toast("Alice", "hi", 1)];
        assert_eq!(coalesce(&one).unwrap(), toast("Alice", "hi", 1));
        let same = vec![toast("Alice", "a", 1), toast("Alice", "b", 1)];
        assert_eq!(
            coalesce(&same).unwrap(),
            toast("Alice", "2 new messages", 1)
        );
        let mixed = vec![
            toast("Alice", "a", 1),
            toast("Bob", "b", 2),
            toast("Bob", "c", 2),
        ];
        let c = coalesce(&mixed).unwrap();
        assert_eq!(c.body, "3 new messages in 2 chats");
        assert_eq!(c.chat, None, "a click cannot pick one of several chats");
    }
}
