//! Chat folder dialogs: the manager (list, reorder, delete) and the editor
//! (name, included chats and types, excluded chats and flags).

use iced::widget::{
    Column, Space, button, column, container, row, scrollable, text, text_input, toggler,
};
use iced::{Alignment, Element, Length};

use super::format;
use super::icons::Icon;
use super::theme::{self, FONT_SEMIBOLD};
use super::widgets::{avatar_photo, emoji_plain, icon, modal};
use crate::app::folders::{EditorFlag, FolderEditor, Picker};
use crate::app::{App, FOLDER_TITLE_ID, Message, Ui};
use crate::model::ChatId;

fn title<'a>(app: &App, s: &'a str) -> Element<'a, Message> {
    text(s)
        .size(app.theme.metrics.font_l)
        .font(FONT_SEMIBOLD)
        .color(app.theme.palette.text_primary)
        .into()
}

/// "Chat folders": every folder with reorder, edit and delete.
pub fn manager(app: &App) -> Element<'_, Message> {
    let (p, m) = (app.theme.palette, app.theme.metrics);
    let mut list = Column::new().spacing(2);
    if app.folders.list.is_empty() {
        list = list.push(
            container(
                text(
                    "Create folders for different groups of chats and quickly switch between them.",
                )
                .size(m.font_s)
                .color(p.text_secondary),
            )
            .padding([8, 4]),
        );
    }
    let last = app.folders.list.len().saturating_sub(1);
    for (i, f) in app.folders.list.iter().enumerate() {
        let ib = |ic: Icon, msg: Option<Ui>| {
            button(icon(ic, 18.0, p.text_secondary))
                .padding(6)
                .on_press_maybe(msg.map(Message::Ui))
                .style(theme::icon_button(p, true))
        };
        let right: Element<'_, Message> = if app.folders.confirm_delete == Some(f.id) {
            row![
                text("Delete?").size(m.font_s).color(p.danger),
                button(text("Delete").size(m.font_s))
                    .padding([5, 10])
                    .on_press(Message::Ui(Ui::FolderDeleteConfirm(f.id)))
                    .style(theme::menu_item(p, true)),
                button(text("Cancel").size(m.font_s))
                    .padding([5, 10])
                    .on_press(Message::Ui(Ui::FolderDeleteCancel))
                    .style(theme::menu_item(p, false)),
            ]
            .spacing(4)
            .align_y(Alignment::Center)
            .into()
        } else {
            row![
                ib(Icon::ArrowUp, (i > 0).then_some(Ui::FolderMove(f.id, -1))),
                ib(
                    Icon::ArrowDown,
                    (i < last).then_some(Ui::FolderMove(f.id, 1))
                ),
                ib(Icon::Pencil, Some(Ui::FolderEdit(f.id))),
                ib(Icon::Trash, Some(Ui::FolderDeleteAsk(f.id))),
            ]
            .spacing(0)
            .into()
        };
        list = list.push(
            container(
                row![
                    column![
                        emoji_plain(format::ellipsize(&f.title, 24), m.font_m, p.text_primary),
                        text(if f.shared {
                            "Shared folder".to_owned()
                        } else {
                            f.summary()
                        })
                        .size(m.font_xs)
                        .color(p.text_secondary),
                    ]
                    .spacing(2)
                    .width(Length::Fill),
                    right,
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            )
            .padding([6, 8])
            .style(theme::pill(p.bg_hover, 8.0)),
        );
    }
    let card = container(
        column![
            title(app, "Chat folders"),
            scrollable(list)
                .height(Length::Shrink)
                .style(theme::scroll(p)),
            row![
                button(text("Create new folder").size(m.font_m))
                    .padding([8, 14])
                    .on_press(Message::Ui(Ui::FolderNew(None)))
                    .style(theme::menu_item(p, false)),
                Space::new().width(Length::Fill),
                button(text("Done").size(m.font_m))
                    .padding([8, 22])
                    .on_press(Message::Ui(Ui::CloseFolders))
                    .style(theme::primary_button(p)),
            ]
            .align_y(Alignment::Center),
        ]
        .spacing(14),
    )
    .padding(20)
    .width(460)
    .max_height(620)
    .style(theme::elevated(p));
    modal(card, Message::Ui(Ui::CloseFolders))
}

fn flag_row<'a>(
    app: &App,
    ed: &FolderEditor,
    label: &'a str,
    flag: EditorFlag,
) -> Element<'a, Message> {
    let (p, m) = (app.theme.palette, app.theme.metrics);
    row![
        text(label)
            .size(m.font_m)
            .color(p.text_primary)
            .width(Length::Fill),
        toggler(ed.flag(flag))
            .on_toggle(move |_| Message::Ui(Ui::FolderFlag(flag)))
            .size(20)
            .style(theme::switch(p)),
    ]
    .spacing(12)
    .align_y(Alignment::Center)
    .into()
}

fn chat_title(app: &App, id: ChatId) -> String {
    app.chats
        .get(id)
        .map_or_else(|| "Chat".to_owned(), |c| c.title.clone())
}

/// The chats of one list as removable lines plus an "Add chats" button.
fn chat_list<'a>(app: &'a App, ed: &FolderEditor, which: Picker) -> Element<'a, Message> {
    let (p, m) = (app.theme.palette, app.theme.metrics);
    let mut col = Column::new().spacing(2);
    for id in ed.chats(which) {
        let c = app.chats.get(id);
        col = col.push(
            container(
                row![
                    avatar_photo(
                        app.avatar(id),
                        id,
                        &chat_title(app, id),
                        c.map(|c| c.kind),
                        28.0
                    ),
                    emoji_plain(
                        format::ellipsize(&chat_title(app, id), 28),
                        m.font_m,
                        p.text_primary
                    ),
                    Space::new().width(Length::Fill),
                    button(icon(Icon::X, 16.0, p.text_secondary))
                        .padding(4)
                        .on_press(Message::Ui(Ui::FolderChatRemove(which, id)))
                        .style(theme::icon_button(p, true)),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            )
            .padding([2, 6])
            .style(theme::pill(p.bg_hover, 8.0)),
        );
    }
    col.push(
        button(
            row![
                icon(Icon::Plus, 17.0, p.link),
                text("Add chats").size(m.font_m).color(p.link)
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        )
        .padding([6, 8])
        .on_press(Message::Ui(Ui::FolderPick(Some(which))))
        .style(theme::menu_item(p, false)),
    )
    .into()
}

fn section<'a>(app: &App, label: &'a str) -> Element<'a, Message> {
    text(label)
        .size(app.theme.metrics.font_s)
        .font(FONT_SEMIBOLD)
        .color(app.theme.palette.link)
        .into()
}

/// Create / edit dialog; while the chat picker is open it shows the picker.
pub fn editor(app: &App) -> Option<Element<'_, Message>> {
    let ed = app.folder_editor.as_ref()?;
    let (p, m) = (app.theme.palette, app.theme.metrics);
    if let Some(which) = ed.picker {
        return Some(picker(app, ed, which));
    }
    let body = column![
        text_input("Folder name", &ed.folder.title)
            .id(FOLDER_TITLE_ID)
            .on_input(|s| Message::Ui(Ui::FolderTitle(s)))
            .on_submit(Message::Ui(Ui::FolderSave))
            .padding([8, 10])
            .size(m.font_m)
            .style(theme::input(p, p.bg_input, 8.0, true)),
        section(app, "Included chats"),
        chat_list(app, ed, Picker::Include),
        flag_row(app, ed, "Contacts", EditorFlag::Contacts),
        flag_row(app, ed, "Non-contacts", EditorFlag::NonContacts),
        flag_row(app, ed, "Groups", EditorFlag::Groups),
        flag_row(app, ed, "Channels", EditorFlag::Channels),
        flag_row(app, ed, "Bots", EditorFlag::Bots),
        section(app, "Excluded chats"),
        chat_list(app, ed, Picker::Exclude),
        flag_row(app, ed, "Exclude muted", EditorFlag::ExcludeMuted),
        flag_row(app, ed, "Exclude read", EditorFlag::ExcludeRead),
        flag_row(app, ed, "Exclude archived", EditorFlag::ExcludeArchived),
    ]
    .spacing(10)
    .padding([0, 8]);
    let error: Element<'_, Message> = match &ed.error {
        Some(e) => text(e.clone()).size(m.font_s).color(p.danger).into(),
        None => Space::new().height(0).into(),
    };
    let card = container(
        column![
            title(
                app,
                if ed.is_new {
                    "New folder"
                } else {
                    "Edit folder"
                }
            ),
            scrollable(body)
                .height(Length::Shrink)
                .style(theme::scroll(p)),
            error,
            row![
                Space::new().width(Length::Fill),
                button(text("Cancel").size(m.font_m))
                    .padding([8, 16])
                    .on_press(Message::Ui(Ui::FolderEditorClose))
                    .style(theme::menu_item(p, false)),
                button(
                    text(if ed.saving {
                        "Saving…"
                    } else if ed.is_new {
                        "Create"
                    } else {
                        "Save"
                    })
                    .size(m.font_m)
                )
                .padding([8, 22])
                .on_press_maybe((!ed.saving).then_some(Message::Ui(Ui::FolderSave)))
                .style(theme::primary_button(p)),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        ]
        .spacing(12),
    )
    .padding(20)
    .width(480)
    .max_height(700)
    .style(theme::elevated(p));
    Some(modal(card, Message::Ui(Ui::FolderEditorClose)))
}

/// Pick chats for one list: search box and every loaded chat with a check.
fn picker<'a>(app: &'a App, ed: &'a FolderEditor, which: Picker) -> Element<'a, Message> {
    let (p, m) = (app.theme.palette, app.theme.metrics);
    let q = ed.query.trim().to_lowercase();
    let mut list = Column::new().spacing(1);
    for c in app.chats.all() {
        if !q.is_empty() && !c.title.to_lowercase().contains(&q) {
            continue;
        }
        let on = ed.contains(which, c.id);
        list = list.push(
            button(
                row![
                    avatar_photo(app.avatar(c.id), c.id, &c.title, Some(c.kind), 34.0),
                    emoji_plain(format::ellipsize(&c.title, 28), m.font_m, p.text_primary),
                    Space::new().width(Length::Fill),
                    if on {
                        icon(Icon::Check, 20.0, p.link)
                    } else {
                        Space::new().width(20).into()
                    },
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            )
            .width(Length::Fill)
            .padding([5, 8])
            .on_press(Message::Ui(Ui::FolderChatToggle(which, c.id)))
            .style(theme::menu_item(p, false)),
        );
    }
    let heading = match which {
        Picker::Include => "Add chats to the folder",
        Picker::Exclude => "Choose chats to exclude",
    };
    let card = container(
        column![
            title(app, heading),
            text_input("Search", &ed.query)
                .on_input(|s| Message::Ui(Ui::FolderPickQuery(s)))
                .padding([8, 10])
                .size(m.font_m)
                .style(theme::input(p, p.bg_input, 8.0, true)),
            scrollable(list)
                .height(Length::Fixed(380.0))
                .style(theme::scroll(p)),
            row![
                Space::new().width(Length::Fill),
                button(text("Done").size(m.font_m))
                    .padding([8, 22])
                    .on_press(Message::Ui(Ui::FolderPick(None)))
                    .style(theme::primary_button(p)),
            ],
        ]
        .spacing(12),
    )
    .padding(20)
    .width(480)
    .style(theme::elevated(p));
    modal(card, Message::Ui(Ui::FolderPick(None)))
}
