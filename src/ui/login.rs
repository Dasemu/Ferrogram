//! Login screens: phone, code, 2FA password (Telegram Desktop layout).

use iced::widget::{Space, button, column, container, svg, text, text_input};
use iced::{Alignment, Element, Length};

use super::icons::LOGO;
use super::theme::{self, FONT_SEMIBOLD};
use crate::app::{AUTH_INPUT_ID, App, Message, Ui};
use crate::auth::AuthState;

pub fn view(app: &App) -> Element<'_, Message> {
    let t = &app.theme;
    let p = t.palette;
    let m = t.metrics;

    let (title, subtitle, placeholder, value, secure, error, busy, hint, can_back) = match &app.auth
    {
        AuthState::Restoring => (
            "Ferrogram".to_owned(),
            "Connecting…".to_owned(),
            "",
            String::new(),
            false,
            None,
            true,
            None,
            false,
        ),
        AuthState::Phone { input, error, busy } => (
            "Your Phone Number".to_owned(),
            "Please confirm your country code\nand enter your phone number.".to_owned(),
            "+1 234 567 8900",
            input.clone(),
            false,
            error.clone(),
            *busy,
            None,
            false,
        ),
        AuthState::Code {
            phone,
            input,
            error,
            busy,
        } => (
            // The number is shown to its owner on screen (never logged).
            phone.expose().to_owned(),
            "We've sent the code to the Telegram app\non your other device.".to_owned(),
            "Code",
            input.clone(),
            false,
            error.clone(),
            *busy,
            None,
            true,
        ),
        AuthState::Password {
            hint,
            input,
            error,
            busy,
        } => (
            "Cloud password check".to_owned(),
            "Please enter your cloud password.".to_owned(),
            "Your cloud password",
            input.clone(),
            true,
            error.clone(),
            *busy,
            hint.clone(),
            true,
        ),
        AuthState::SignedIn(_) => (
            String::new(),
            String::new(),
            "",
            String::new(),
            false,
            None,
            true,
            None,
            false,
        ),
    };

    let show_input = !matches!(app.auth, AuthState::Restoring);
    let mut form = column![
        svg(svg::Handle::from_memory(LOGO)).width(110).height(110),
        Space::new().height(22),
        text(title)
            .size(m.font_xl)
            .font(FONT_SEMIBOLD)
            .color(p.text_primary),
        Space::new().height(8),
        text(subtitle)
            .size(m.font_m)
            .color(p.text_secondary)
            .align_x(Alignment::Center),
        Space::new().height(30),
    ]
    .align_x(Alignment::Center)
    .width(340);

    if show_input {
        let input = text_input(placeholder, &value)
            .id(AUTH_INPUT_ID)
            .secure(secure)
            .on_input(|s| Message::Ui(Ui::AuthInput(s)))
            .on_submit(Message::Ui(Ui::AuthSubmit))
            .padding([12, 14])
            .size(m.font_l)
            .style(theme::input(p, p.bg_input, 8.0, true));
        form = form.push(input);
        if let Some(h) = hint {
            form = form.push(
                container(
                    text(format!("Hint: {h}"))
                        .size(m.font_s)
                        .color(p.text_secondary),
                )
                .width(Length::Fill)
                .padding([6, 4]),
            );
        }
        form = form.push(
            container(
                text(error.unwrap_or_default())
                    .size(m.font_s)
                    .color(p.danger),
            )
            .width(Length::Fill)
            .padding([6, 4])
            .height(30),
        );
        let label = if busy { "PLEASE WAIT…" } else { "NEXT" };
        form = form.push(
            button(
                container(text(label).size(m.font_m).font(FONT_SEMIBOLD))
                    .width(Length::Fill)
                    .align_x(Alignment::Center),
            )
            .width(Length::Fill)
            .padding([12, 0])
            .on_press_maybe((!busy).then_some(Message::Ui(Ui::AuthSubmit)))
            .style(theme::primary_button(p)),
        );
        if can_back {
            form = form.push(Space::new().height(14)).push(
                button(text("Use a different phone number").size(m.font_s))
                    .on_press(Message::Ui(Ui::AuthBack))
                    .style(theme::link_button(p)),
            );
        }
    }

    let footer = text("Unofficial client · not affiliated with Telegram")
        .size(m.font_xs)
        .color(p.text_muted);

    container(
        column![
            Space::new().height(Length::Fill),
            form,
            Space::new().height(Length::Fill),
            footer,
            Space::new().height(16),
        ]
        .align_x(Alignment::Center)
        .width(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(theme::fill(p.bg_sidebar))
    .into()
}
