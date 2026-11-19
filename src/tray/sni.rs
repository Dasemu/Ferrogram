//! The StatusNotifierItem service, via `ksni` (pure Rust over zbus).
//!
//! Left click sends `Remote::Toggle`; the menu is built from
//! [`TrayModel::menu`]. When the panel's StatusNotifierWatcher disappears
//! (plasmashell restarting or crashing) the app is told, so a hidden window
//! can come back instead of leaving an unreachable process.

use std::time::Duration;

use ksni::{Category, Icon, MenuItem, OfflineReason, Status, ToolTip, TrayMethods, menu};

use super::icon::{self, Argb};
use super::{Entry, Sink, TrayModel};
use crate::config::{APP_ID, APP_NAME};
use crate::remote::Remote;

const APP_SVG: &str = crate::desktop::APP_ICON_SVG;

struct Icons {
    normal: Vec<Icon>,
    attention: Vec<Icon>,
}

fn to_icon(a: Argb) -> Icon {
    // `size` is at most 48, far below i32::MAX.
    let side = i32::try_from(a.size).unwrap_or(0);
    Icon {
        width: side,
        height: side,
        data: a.data,
    }
}

impl Icons {
    fn render() -> Self {
        let set = |badge| {
            icon::SIZES
                .iter()
                .filter_map(|s| icon::render(APP_SVG, *s, badge))
                .map(to_icon)
                .collect::<Vec<_>>()
        };
        Self {
            normal: set(false),
            attention: set(true),
        }
    }
}

struct FerroTray {
    model: TrayModel,
    sink: Sink,
    icons: std::sync::Arc<Icons>,
}

impl ksni::Tray for FerroTray {
    fn id(&self) -> String {
        APP_ID.to_owned()
    }

    fn category(&self) -> Category {
        Category::Communications
    }

    fn title(&self) -> String {
        APP_NAME.to_owned()
    }

    fn status(&self) -> Status {
        if self.model.needs_attention() {
            Status::NeedsAttention
        } else {
            Status::Active
        }
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        self.icons.normal.clone()
    }

    fn attention_icon_pixmap(&self) -> Vec<Icon> {
        self.icons.attention.clone()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            icon_name: String::new(),
            icon_pixmap: self.icons.normal.clone(),
            title: self.model.tooltip_title(),
            description: self.model.tooltip_text(),
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        (self.sink)(Remote::Toggle);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        self.model
            .menu()
            .into_iter()
            .map(|entry| match entry {
                Entry::Separator => MenuItem::Separator,
                Entry::Item { label, request } => menu::StandardItem {
                    label,
                    activate: Box::new(move |t: &mut Self| (t.sink)(request)),
                    ..Default::default()
                }
                .into(),
                Entry::Check {
                    label,
                    checked,
                    request,
                } => menu::CheckmarkItem {
                    label,
                    checked,
                    activate: Box::new(move |t: &mut Self| (t.sink)(request)),
                    ..Default::default()
                }
                .into(),
            })
            .collect()
    }

    fn watcher_offline(&self, reason: OfflineReason) -> bool {
        tracing::warn!(?reason, "tray host went away");
        (self.sink)(Remote::TrayLost);
        // Keep the service: ksni re-registers when the watcher is back.
        true
    }

    fn watcher_online(&self) {
        tracing::info!("tray host is back");
        (self.sink)(Remote::TrayBack);
    }
}

/// Handle to a running tray; clones share the service.
#[derive(Clone)]
pub struct TrayHandle(ksni::Handle<FerroTray>);

impl std::fmt::Debug for TrayHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TrayHandle")
    }
}

impl TrayHandle {
    /// Replace what the icon shows. The panel is only notified by ksni.
    pub async fn set_model(&self, model: TrayModel) {
        self.0.update(move |t| t.model = model).await;
    }

    pub async fn shutdown(&self) {
        self.0.shutdown().await;
    }
}

/// Register with the panel. `attempts` tries `delay` apart: at login the
/// panel may start after us, so a minimized autostart keeps trying for a
/// while before the caller gives up and shows the window instead.
pub async fn spawn(
    model: TrayModel,
    sink: Sink,
    attempts: u32,
    delay: Duration,
) -> Result<TrayHandle, String> {
    let icons = tokio::task::spawn_blocking(Icons::render)
        .await
        .map_err(|e| format!("icon rendering failed: {e}"))?;
    let icons = std::sync::Arc::new(icons);
    let attempts = attempts.max(1);
    let mut last = String::new();
    for attempt in 1..=attempts {
        let tray = FerroTray {
            model: model.clone(),
            sink: sink.clone(),
            icons: icons.clone(),
        };
        match tray.spawn().await {
            Ok(handle) => return Ok(TrayHandle(handle)),
            Err(e) => {
                last = e.to_string();
                tracing::debug!(attempt, error = %last, "tray registration failed");
                if attempt < attempts {
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
    Err(last)
}
