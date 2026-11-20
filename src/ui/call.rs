//! Call UI: the call panel (ringing, connecting, in call) and the in-call bar
//! above the chat when the panel is minimised.

use iced::widget::{Space, button, column, container, row, scrollable, text, tooltip};
use iced::{Alignment, Element, Length};

use super::icons::Icon;
use super::theme::{self, FONT_SEMIBOLD};
use super::widgets::{avatar_photo, emoji_text, emoji_title, icon, modal};
use crate::app::call::{CallSession, Direction, Phase};
use crate::app::phone::DeviceKind;
use crate::app::{App, Message, Ui};
use crate::model::ChatKind;

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn peer_name(app: &App, s: &CallSession) -> String {
    app.chats
        .get(s.peer)
        .map(|c| c.title.clone())
        .or_else(|| app.user_names.get(&s.peer).cloned())
        .unwrap_or_else(|| "Unknown".into())
}

/// Status line; the screenshot mode pins the clock so images are stable.
fn status(app: &App, s: &CallSession) -> String {
    let t = if app.config.screenshot.is_some() {
        match s.phase {
            Phase::Active { since, .. } | Phase::Reconnecting { since } => since + 83,
            _ => now(),
        }
    } else {
        now()
    };
    s.status_text(t)
}

fn round<'a>(
    i: Icon,
    bg: iced::Color,
    fg: iced::Color,
    label: &'a str,
    msg: Message,
) -> Element<'a, Message> {
    let p = theme::NIGHT;
    column![
        button(
            container(icon(i, theme::CALL_BUTTON_ICON, fg))
                .center_x(theme::CALL_BUTTON)
                .center_y(theme::CALL_BUTTON),
        )
        .padding(0)
        .on_press(msg)
        .style(theme::round_button(bg)),
        text(label)
            .size(theme::METRICS.font_xs)
            .color(p.text_primary),
    ]
    .spacing(6)
    .align_x(Alignment::Center)
    .width(76)
    .into()
}

/// Four signal bars, `q` of them lit.
fn signal<'a>(q: u8) -> Element<'a, Message> {
    let bars = (1..=4u8).map(|i| {
        let lit = i <= q;
        container(Space::new())
            .width(theme::CALL_SIGNAL_BAR)
            .height(theme::CALL_SIGNAL_BAR * f32::from(i))
            .style(theme::pill(
                if lit {
                    iced::Color::WHITE
                } else {
                    theme::CALL_BUTTON_BG
                },
                1.0,
            ))
            .into()
    });
    row(bars).spacing(2).align_y(Alignment::End).into()
}

fn device_section<'a>(app: &'a App, kind: DeviceKind) -> Element<'a, Message> {
    let p = app.theme.palette;
    let m = app.theme.metrics;
    let (list, current, title) = match kind {
        DeviceKind::Input => (
            &app.call_ui.inputs,
            app.call_ui.input.as_deref(),
            "Microphone",
        ),
        DeviceKind::Output => (
            &app.call_ui.outputs,
            app.call_ui.output.as_deref(),
            "Speakers",
        ),
    };
    let mut items = column![
        text(title)
            .size(m.font_s)
            .font(FONT_SEMIBOLD)
            .color(p.text_secondary)
    ]
    .spacing(2);
    if list.is_empty() {
        items = items.push(
            text("System default")
                .size(m.font_s)
                .color(p.text_secondary),
        );
    }
    for name in list {
        let selected = current == Some(name.as_str());
        items = items.push(
            button(
                row![
                    text(name.clone()).size(m.font_s),
                    Space::new().width(Length::Fill),
                    if selected {
                        icon(Icon::Check, 16.0, p.link)
                    } else {
                        Space::new().width(16).into()
                    },
                ]
                .align_y(Alignment::Center),
            )
            .width(Length::Fill)
            .padding([6, 8])
            .on_press(Message::Ui(Ui::CallPickDevice(kind, name.clone())))
            .style(theme::menu_item(p, false)),
        );
    }
    items.into()
}

/// Output and input device lists.
fn device_menu(app: &App) -> Element<'_, Message> {
    let p = app.theme.palette;
    container(
        scrollable(
            column![
                device_section(app, DeviceKind::Output),
                device_section(app, DeviceKind::Input)
            ]
            .spacing(app.theme.metrics.space_m),
        )
        .height(Length::Shrink),
    )
    .max_height(200)
    .padding(8)
    .style(theme::elevated(p))
    .into()
}

/// The full call panel (modal), or `None` when there is no call or it is
/// minimised.
pub fn panel(app: &App) -> Option<Element<'_, Message>> {
    let s = app.calls.session.as_ref()?;
    if !app.call_ui.expanded {
        return None;
    }
    let p = app.theme.palette;
    let m = app.theme.metrics;
    let name = peer_name(app, s);
    let ringing_in = s.direction == Direction::Incoming && s.phase == Phase::Ringing;
    let ended = s.is_ended();

    let top = row![
        if let (Some(e), false) = (s.emoji, ended) {
            tooltip(
                emoji_text(e.join(""), theme::CALL_EMOJI * 0.7, p.text_primary),
                container(
                    text(format!(
                        "If these emoji are the same on {name}'s screen, this call is end-to-end encrypted."
                    ))
                    .size(m.font_s)
                    .color(p.text_primary),
                )
                .max_width(240)
                .padding(8)
                .style(theme::elevated(p)),
                tooltip::Position::Bottom,
            )
            .into()
        } else {
            Element::from(Space::new())
        },
        Space::new().width(Length::Fill),
        if ringing_in || ended {
            Element::from(Space::new().height(36))
        } else {
            button(icon(Icon::ChevronDown, 20.0, p.text_secondary))
                .padding(8)
                .on_press(Message::Ui(Ui::CallExpand(false)))
                .style(theme::icon_button(p, true))
                .into()
        },
    ]
    .align_y(Alignment::Center);

    let mut info = column![
        avatar_photo(
            app.avatar(s.peer),
            s.peer,
            &name,
            Some(ChatKind::User),
            theme::CALL_AVATAR
        ),
        Space::new().height(m.space_l),
        emoji_title(name.clone(), m.font_xl, p.text_primary),
        row![text(status(app, s)).size(m.font_m).color(p.text_secondary),]
            .spacing(8)
            .align_y(Alignment::Center),
    ]
    .spacing(4)
    .align_x(Alignment::Center);
    if matches!(s.phase, Phase::Active { .. } | Phase::Reconnecting { .. }) {
        let route = match s.phase {
            Phase::Active { relay: true, .. } => "via Telegram server",
            Phase::Active { .. } => "direct connection",
            _ => "",
        };
        info = info.push(
            row![
                signal(s.quality),
                text(route).size(m.font_xs).color(p.text_secondary)
            ]
            .spacing(8)
            .align_y(Alignment::End),
        );
    }

    let buttons: Element<'_, Message> = if ringing_in {
        row![
            round(
                Icon::PhoneOff,
                p.danger,
                iced::Color::WHITE,
                "Decline",
                Message::Ui(Ui::CallHangUp)
            ),
            Space::new().width(40),
            round(
                Icon::Phone,
                p.success,
                iced::Color::WHITE,
                "Accept",
                Message::Ui(Ui::CallAccept)
            ),
        ]
        .align_y(Alignment::Start)
        .into()
    } else if ended {
        row![round(
            Icon::X,
            theme::CALL_BUTTON_BG,
            iced::Color::WHITE,
            "Close",
            Message::Ui(Ui::CallHangUp)
        )]
        .into()
    } else {
        let menu = Message::Ui(Ui::CallDevices(
            app.call_ui.menu.is_none().then_some(DeviceKind::Output),
        ));
        let (mic_icon, mic_bg, mic_fg, mic_label) = if s.muted {
            (
                Icon::MicOff,
                theme::CALL_BUTTON_ON,
                theme::CALL_BG,
                "Unmute",
            )
        } else {
            (Icon::Mic, theme::CALL_BUTTON_BG, iced::Color::WHITE, "Mute")
        };
        row![
            round(
                Icon::Speaker,
                theme::CALL_BUTTON_BG,
                iced::Color::WHITE,
                "Audio",
                menu
            ),
            round(
                mic_icon,
                mic_bg,
                mic_fg,
                mic_label,
                Message::Ui(Ui::CallToggleMute)
            ),
            round(
                Icon::PhoneOff,
                p.danger,
                iced::Color::WHITE,
                "End call",
                Message::Ui(Ui::CallHangUp)
            ),
        ]
        .spacing(8)
        .into()
    };

    let mut body = column![
        top,
        Space::new().height(m.space_m),
        info,
        Space::new().height(Length::Fill)
    ]
    .align_x(Alignment::Center)
    .width(Length::Fill)
    .height(Length::Fill);
    if app.call_ui.menu.is_some() && !ringing_in && !ended {
        body = body.push(device_menu(app));
        body = body.push(Space::new().height(m.space_m));
    }
    body = body.push(buttons);

    let card = container(body)
        .width(theme::CALL_PANEL_WIDTH)
        .height(theme::CALL_PANEL_HEIGHT)
        .padding([m.space_l, m.space_l])
        .style(theme::call_card(p));
    // Clicking outside minimises an ongoing call; a ringing call stays.
    let dismiss = if ringing_in || ended {
        Message::Ui(Ui::CallTick)
    } else {
        Message::Ui(Ui::CallExpand(false))
    };
    Some(modal(card, dismiss))
}

/// The bar above the chat while a minimised call runs.
pub fn bar(app: &App) -> Option<Element<'_, Message>> {
    let s = app.calls.session.as_ref()?;
    if app.call_ui.expanded {
        return None;
    }
    let m = app.theme.metrics;
    let white = iced::Color::WHITE;
    let name = peer_name(app, s);
    let small = |i: Icon, msg: Message| {
        button(icon(i, 18.0, white))
            .padding(6)
            .on_press(msg)
            .style(theme::round_button(theme::CALL_BUTTON_BG))
    };
    let content = row![
        small(
            if s.muted { Icon::MicOff } else { Icon::Mic },
            Message::Ui(Ui::CallToggleMute)
        ),
        Space::new().width(Length::Fill),
        emoji_title(name, m.font_m, white),
        text(status(app, s)).size(m.font_s).color(white),
        Space::new().width(Length::Fill),
        small(Icon::PhoneOff, Message::Ui(Ui::CallHangUp)),
    ]
    .spacing(m.space_m)
    .align_y(Alignment::Center);
    Some(
        button(content)
            .width(Length::Fill)
            .height(theme::CALL_BAR_HEIGHT)
            .padding([0, 10])
            .on_press(Message::Ui(Ui::CallExpand(true)))
            .style(theme::call_bar(app.theme.palette))
            .into(),
    )
}
