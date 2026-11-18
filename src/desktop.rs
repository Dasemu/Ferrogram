//! Desktop integration: notifications via `notify-rust` (D-Bus on Linux).

use crate::config::{APP_ID, APP_NAME};

/// Fire-and-forget notification on a blocking thread (D-Bus is blocking).
pub fn notify(title: String, body: String) {
    std::thread::spawn(move || {
        let mut n = notify_rust::Notification::new();
        n.appname(APP_NAME)
            .summary(&title)
            .body(&body)
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
    });
}
