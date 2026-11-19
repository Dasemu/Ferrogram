//! Requests that reach the app from outside its window: the tray icon, a
//! second launch of the program, a clicked notification.
//!
//! Those sources live on their own threads or D-Bus tasks and know nothing
//! about iced. They all call [`send`]; the app drains the queue through the
//! [`stream`] subscription. The queue is process-wide and survives a
//! subscription restart (the receiver is shared, never taken).

use std::sync::{Arc, OnceLock};

use futures::Stream;
use tokio::sync::Mutex;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// How long the "mute notifications" tray action silences them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuteRequest {
    OneHour,
    UntilEnabled,
    Off,
}

/// Something asked the app to do something without touching the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remote {
    /// Tray left click: show the window, or hide it when it is visible.
    Toggle,
    /// Show (and focus) the window, optionally opening a chat: a clicked
    /// notification or a second launch.
    Show {
        chat: Option<i64>,
    },
    Hide,
    Mute(MuteRequest),
    ToggleStartMinimized,
    Quit,
    /// The StatusNotifierWatcher disappeared (panel crashed or restarted).
    TrayLost,
    /// The StatusNotifierWatcher is back.
    TrayBack,
}

type Shared = (
    UnboundedSender<Remote>,
    Arc<Mutex<UnboundedReceiver<Remote>>>,
);

fn bus() -> &'static Shared {
    static BUS: OnceLock<Shared> = OnceLock::new();
    BUS.get_or_init(|| {
        let (tx, rx) = unbounded_channel();
        (tx, Arc::new(Mutex::new(rx)))
    })
}

/// Queue a request for the app. Never blocks; safe from any thread.
pub fn send(request: Remote) {
    // The receiver is owned by the static, so this can only fail at exit.
    let _ = bus().0.send(request);
}

/// The queue as a stream, for `Subscription::run`.
pub fn stream() -> impl Stream<Item = Remote> {
    futures::stream::unfold(bus().1.clone(), |rx| async move {
        let next = rx.lock().await.recv().await;
        next.map(|r| (r, rx))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[tokio::test]
    async fn requests_arrive_in_order_and_survive_a_restart() {
        send(Remote::Hide);
        send(Remote::Show { chat: Some(7) });
        let mut first = Box::pin(stream());
        assert_eq!(first.next().await, Some(Remote::Hide));
        drop(first);
        // A second stream (subscription restart) continues the same queue.
        let mut second = Box::pin(stream());
        assert_eq!(second.next().await, Some(Remote::Show { chat: Some(7) }));
    }
}
