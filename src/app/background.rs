//! The window / tray (background mode) state machine (pure, unit-tested without iced).
//!
//! The app is an `iced::daemon`: with the tray available, "closing" the
//! window really closes it (so the renderer surfaces are freed) while the
//! process, the connection and the in-memory state keep running; "showing"
//! opens a new window with the remembered size and position.
//!
//! The invariant behind every rule here: **never leave an invisible,
//! unreachable process**. The window may only be hidden while a tray host is
//! known to exist, and it comes back by itself when the tray disappears.

use crate::remote::Remote;

/// Whether a StatusNotifierWatcher (tray host) accepted our icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayStatus {
    /// Still registering (startup).
    Pending,
    Available,
    /// No host, registration failed, or the host went away.
    Unavailable,
}

/// What the app has to do to the window after a transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    /// Open the window (first start, or "show" from the tray).
    Open,
    /// Close the window but keep running (close to tray / "hide").
    CloseWindow,
    /// Already open: raise and focus it.
    Focus,
    /// Quit the program.
    Exit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowState {
    /// A window currently exists.
    pub open: bool,
    pub tray: TrayStatus,
    pub close_to_tray: bool,
}

impl WindowState {
    pub fn new(close_to_tray: bool) -> Self {
        Self {
            open: false,
            tray: TrayStatus::Pending,
            close_to_tray,
        }
    }

    /// Startup: open the window unless we start minimized to the tray.
    /// A minimized start without a tray would be invisible, so that case is
    /// resolved later by [`Self::tray_ready`].
    pub fn start(&mut self, minimized: bool) -> Action {
        if minimized {
            Action::None
        } else {
            self.open = true;
            Action::Open
        }
    }

    pub fn can_hide(&self) -> bool {
        self.tray == TrayStatus::Available
    }

    /// The tray registration finished.
    pub fn tray_ready(&mut self, ok: bool) -> Action {
        self.tray = if ok {
            TrayStatus::Available
        } else {
            TrayStatus::Unavailable
        };
        self.reveal_if_unreachable()
    }

    pub fn tray_lost(&mut self) -> Action {
        self.tray = TrayStatus::Unavailable;
        self.reveal_if_unreachable()
    }

    pub fn tray_back(&mut self) {
        self.tray = TrayStatus::Available;
    }

    fn reveal_if_unreachable(&mut self) -> Action {
        if self.tray == TrayStatus::Unavailable && !self.open {
            self.open = true;
            Action::Open
        } else {
            Action::None
        }
    }

    /// The window's close button: to the tray if we can, else quit as before.
    pub fn close_requested(&mut self) -> Action {
        if !self.open {
            return Action::None;
        }
        if self.close_to_tray && self.can_hide() {
            self.open = false;
            Action::CloseWindow
        } else {
            Action::Exit
        }
    }

    pub fn show(&mut self) -> Action {
        if self.open {
            Action::Focus
        } else {
            self.open = true;
            Action::Open
        }
    }

    pub fn hide(&mut self) -> Action {
        if self.open && self.can_hide() {
            self.open = false;
            Action::CloseWindow
        } else {
            Action::None
        }
    }

    pub fn toggle(&mut self) -> Action {
        if self.open { self.hide() } else { self.show() }
    }

    /// Map a request from the tray / second launch / notification.
    /// Requests that are not about the window (mute, start minimized) are
    /// handled by the app and give `Action::None` here.
    pub fn handle(&mut self, request: Remote) -> Action {
        match request {
            Remote::Toggle => self.toggle(),
            Remote::Show { .. } => self.show(),
            Remote::Hide => self.hide(),
            Remote::Quit => Action::Exit,
            Remote::TrayLost => self.tray_lost(),
            Remote::TrayBack => {
                self.tray_back();
                Action::None
            }
            Remote::Mute(_) | Remote::ToggleStartMinimized => Action::None,
        }
    }
}

/// Size and position to restore when the window is opened again.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Geometry {
    pub size: (f32, f32),
    /// `None` where the platform never tells us (Wayland).
    pub position: Option<(f32, f32)>,
}

pub const DEFAULT_SIZE: (f32, f32) = (1180.0, 760.0);
pub const MIN_SIZE: (f32, f32) = (720.0, 480.0);

impl Default for Geometry {
    fn default() -> Self {
        Self {
            size: DEFAULT_SIZE,
            position: None,
        }
    }
}

impl Geometry {
    /// Remember a resize. Bogus values (a minimized window reports 0x0) and
    /// absurd ones are ignored.
    pub fn resized(&mut self, w: f32, h: f32) {
        if w.is_finite()
            && h.is_finite()
            && w >= MIN_SIZE.0
            && h >= MIN_SIZE.1
            && w < 16_384.0
            && h < 16_384.0
        {
            self.size = (w, h);
        }
    }

    /// Remember a move. Off-screen sentinels (Windows minimizes to -32000)
    /// are ignored.
    pub fn moved(&mut self, x: f32, y: f32) {
        if x.is_finite()
            && y.is_finite()
            && x > -10_000.0
            && y > -10_000.0
            && x < 100_000.0
            && y < 100_000.0
        {
            self.position = Some((x, y));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::MuteRequest;

    fn with_tray(close_to_tray: bool) -> WindowState {
        let mut w = WindowState::new(close_to_tray);
        assert_eq!(w.start(false), Action::Open);
        assert_eq!(w.tray_ready(true), Action::None);
        w
    }

    #[test]
    fn visible_hidden_shown() {
        let mut w = with_tray(true);
        assert!(w.open);
        assert_eq!(w.hide(), Action::CloseWindow);
        assert!(!w.open);
        assert_eq!(w.show(), Action::Open);
        assert!(w.open);
        assert_eq!(w.show(), Action::Focus, "already open: just raise it");
        assert_eq!(w.toggle(), Action::CloseWindow);
        assert_eq!(w.toggle(), Action::Open);
    }

    #[test]
    fn close_button_goes_to_the_tray_when_there_is_one() {
        let mut w = with_tray(true);
        assert_eq!(w.close_requested(), Action::CloseWindow);
        assert!(!w.open);
        assert_eq!(w.close_requested(), Action::None, "nothing to close");
    }

    #[test]
    fn close_button_quits_without_a_tray_host() {
        let mut w = WindowState::new(true);
        w.start(false);
        assert_eq!(w.tray_ready(false), Action::None);
        assert_eq!(w.close_requested(), Action::Exit);
        // Still registering counts as "no tray yet".
        let mut p = WindowState::new(true);
        p.start(false);
        assert_eq!(p.close_requested(), Action::Exit);
    }

    #[test]
    fn close_button_quits_when_the_setting_is_off() {
        let mut w = with_tray(false);
        assert_eq!(w.close_requested(), Action::Exit);
        // The explicit tray "Hide" still works: it is not the close button.
        assert_eq!(w.hide(), Action::CloseWindow);
    }

    #[test]
    fn no_hiding_without_a_tray() {
        let mut w = WindowState::new(true);
        w.start(false);
        w.tray_ready(false);
        assert_eq!(w.hide(), Action::None);
        assert_eq!(w.toggle(), Action::None);
        assert!(w.open);
    }

    #[test]
    fn minimized_start_stays_hidden_while_the_tray_works() {
        let mut w = WindowState::new(true);
        assert_eq!(w.start(true), Action::None);
        assert!(!w.open);
        assert_eq!(w.tray_ready(true), Action::None);
        assert!(!w.open);
        assert_eq!(w.show(), Action::Open);
    }

    #[test]
    fn minimized_start_without_a_tray_opens_the_window() {
        let mut w = WindowState::new(true);
        w.start(true);
        assert_eq!(w.tray_ready(false), Action::Open);
        assert!(w.open);
    }

    #[test]
    fn losing_the_tray_while_hidden_brings_the_window_back() {
        let mut w = with_tray(true);
        w.hide();
        assert_eq!(w.handle(Remote::TrayLost), Action::Open);
        assert!(w.open);
        assert_eq!(w.close_requested(), Action::Exit, "no host: close quits");
        // The host returns: hiding works again.
        let mut w2 = with_tray(true);
        w2.handle(Remote::TrayLost);
        assert_eq!(w2.handle(Remote::TrayBack), Action::None);
        assert_eq!(w2.hide(), Action::CloseWindow);
    }

    #[test]
    fn losing_the_tray_while_visible_changes_nothing_visible() {
        let mut w = with_tray(true);
        assert_eq!(w.handle(Remote::TrayLost), Action::None);
        assert!(w.open);
    }

    #[test]
    fn remote_requests_map_to_actions() {
        let mut w = with_tray(true);
        assert_eq!(w.handle(Remote::Toggle), Action::CloseWindow);
        assert_eq!(w.handle(Remote::Show { chat: Some(5) }), Action::Open);
        assert_eq!(w.handle(Remote::Show { chat: None }), Action::Focus);
        assert_eq!(w.handle(Remote::Hide), Action::CloseWindow);
        assert_eq!(w.handle(Remote::Hide), Action::None);
        assert_eq!(w.handle(Remote::Quit), Action::Exit);
        assert_eq!(w.handle(Remote::Mute(MuteRequest::OneHour)), Action::None);
        assert_eq!(w.handle(Remote::ToggleStartMinimized), Action::None);
    }

    #[test]
    fn geometry_ignores_nonsense() {
        let mut g = Geometry::default();
        assert_eq!(g.size, DEFAULT_SIZE);
        g.resized(1400.0, 900.0);
        assert_eq!(g.size, (1400.0, 900.0));
        g.resized(0.0, 0.0);
        g.resized(f32::NAN, 900.0);
        g.resized(100_000.0, 900.0);
        g.resized(300.0, 300.0);
        assert_eq!(g.size, (1400.0, 900.0));
        assert_eq!(g.position, None);
        g.moved(120.0, 80.0);
        assert_eq!(g.position, Some((120.0, 80.0)));
        g.moved(-32_000.0, -32_000.0);
        g.moved(f32::INFINITY, 0.0);
        assert_eq!(g.position, Some((120.0, 80.0)));
    }
}
