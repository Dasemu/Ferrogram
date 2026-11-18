//! Views: pure functions `&App -> Element<Message>`.

pub mod chat;
pub mod format;
pub mod icons;
pub mod login;
pub mod sidebar;
pub mod theme;
pub mod widgets;

use iced::widget::{container, opaque, row, stack, text};
use iced::{Element, Length};

use crate::app::{App, Message};

pub fn view(app: &App) -> Element<'_, Message> {
    if !app.auth.is_signed_in() {
        return login::view(app);
    }
    let p = app.theme.palette;
    let divider = container(iced::widget::Space::new().width(1).height(Length::Fill))
        .style(theme::fill(p.border));
    let mut layers = stack![row![sidebar::view(app), divider, chat::view(app)]]
        .width(Length::Fill)
        .height(Length::Fill);
    if let Some(toast) = &app.toast {
        layers = layers.push(
            container(
                container(text(toast.clone()).size(13).color(p.text_primary))
                    .padding([8, 14])
                    .style(theme::elevated(p)),
            )
            .align_bottom(Length::Fill)
            .center_x(Length::Fill)
            .padding(80),
        );
    }
    if let Some(viewer) = chat::media_viewer(app) {
        layers = layers.push(opaque(viewer));
    }
    layers.into()
}
