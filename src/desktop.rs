//! Desktop integration: notifications via `notify-rust` (D-Bus on Linux).
//!
//! One worker thread drains a small bounded queue. Notifications that pile
//! up within a short window are coalesced into a single "N new messages"
//! popup, and when the queue is full new ones are dropped instead of
//! spawning a thread per message.

use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::time::Duration;

use crate::config::{APP_ID, APP_NAME};

const QUEUE: usize = 16;
const COALESCE_WINDOW: Duration = Duration::from_millis(800);

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

/// What to show for a batch of queued notifications.
pub fn coalesce(batch: &[(String, String)]) -> Option<(String, String)> {
    match batch {
        [] => None,
        [one] => Some(one.clone()),
        many => {
            let mut chats: Vec<&str> = many.iter().map(|(t, _)| t.as_str()).collect();
            chats.dedup();
            let title = if chats.len() == 1 {
                chats[0].to_owned()
            } else {
                APP_NAME.to_owned()
            };
            Some((
                title,
                format!(
                    "{} new messages{}",
                    many.len(),
                    if chats.len() > 1 {
                        format!(" in {} chats", chats.len())
                    } else {
                        String::new()
                    }
                ),
            ))
        }
    }
}

fn sender() -> &'static SyncSender<(String, String)> {
    static TX: OnceLock<SyncSender<(String, String)>> = OnceLock::new();
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

fn worker(rx: Receiver<(String, String)>) {
    while let Ok(first) = rx.recv() {
        let mut batch = vec![first];
        std::thread::sleep(COALESCE_WINDOW);
        while let Ok(more) = rx.try_recv() {
            batch.push(more);
        }
        if let Some((title, body)) = coalesce(&batch) {
            show(&title, &body);
        }
    }
}

fn show(title: &str, body: &str) {
    let mut n = notify_rust::Notification::new();
    n.appname(APP_NAME)
        .summary(&escape_markup(title))
        .body(&escape_markup(body))
        .icon("mail-message-new");
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        n.hint(notify_rust::Hint::DesktopEntry(APP_ID.into()))
            .hint(notify_rust::Hint::Category("im.received".into()));
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    let _ = APP_ID;
    if n.show().is_err() {
        tracing::debug!("notification daemon unavailable");
    }
}

/// Queue a notification; never blocks the UI thread.
pub fn notify(title: String, body: String) {
    match sender().try_send((title, body)) {
        Ok(()) | Err(TrySendError::Full(_)) => {}
        Err(TrySendError::Disconnected(_)) => tracing::debug!("notification worker gone"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(coalesce(&[]).is_none());
        let one = vec![("Alice".to_owned(), "hi".to_owned())];
        assert_eq!(coalesce(&one).unwrap().1, "hi");
        let same = vec![("Alice".into(), "a".into()), ("Alice".into(), "b".into())];
        assert_eq!(
            coalesce(&same).unwrap(),
            ("Alice".into(), "2 new messages".into())
        );
        let mixed = vec![
            ("Alice".into(), "a".into()),
            ("Bob".into(), "b".into()),
            ("Bob".into(), "c".into()),
        ];
        assert_eq!(coalesce(&mixed).unwrap().1, "3 new messages in 2 chats");
    }
}
