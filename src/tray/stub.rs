//! No tray on this platform yet (Windows and macOS are not implemented):
//! `spawn` always fails, so the window's close button quits as it did
//! before.

use std::time::Duration;

use super::{Sink, TrayModel};

#[derive(Debug, Clone)]
pub struct TrayHandle;

impl TrayHandle {
    pub async fn set_model(&self, _model: TrayModel) {}

    pub async fn shutdown(&self) {}
}

pub async fn spawn(
    _model: TrayModel,
    _sink: Sink,
    _attempts: u32,
    _delay: Duration,
) -> Result<TrayHandle, String> {
    Err("the tray is not implemented on this platform".into())
}
