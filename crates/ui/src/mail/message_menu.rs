//! Right-click menu for one or more messages (also the reading pane's "More" menu).

use std::rc::Rc;

use gpui::component::Icon;
use gpui::component::menu::{PopupMenu, PopupMenuItem};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::mail_ops::FlagPreset;
use mailbox_core::{Folder, FolderRole, Id, Message};

use super::folder_pane::{folder_icon, folder_name, folder_tree};
use super::{ComposeKind, message_window, ops};
use crate::app_state::AppState;
use crate::components::IconName;

/// Menu item running `action` on the captured state and ids.
fn action(
    label: impl Into<SharedString>,
    icon: IconName,
    state: &Entity<AppState>,
    ids: &[Id],
    run: impl Fn(&Entity<AppState>, Vec<Id>, &mut Window, &mut App) + 'static,
) -> PopupMenuItem {
    let state = state.clone();
    let ids = ids.to_vec();
    PopupMenuItem::new(label)
        .icon(icon)
        .on_click(move |_, window, cx| run(&state, ids.clone(), window, cx))
}

/// Destination folders, grouped by account: `(account name, [(depth, folder)])`.
type Destinations = Rc<Vec<(String, Vec<(usize, Folder)>)>>;

fn destinations(state: &Entity<AppState>, cx: &App) -> Destinations {
    let store = state.read(cx).store();
    Rc::new(
        crate::settings::ordered_accounts(store)
            .into_iter()
            .map(|account| {
                let folders = store.list_folders(account.id).unwrap_or_default();
                let tree = folder_tree(&folders)
                    .into_iter()
                    .map(|(depth, folder)| (depth, folder.clone()))
                    .collect();
                (account.name, tree)
            })
            .collect(),
    )
}

fn folder_submenu(
    menu: PopupMenu,
    state: &Entity<AppState>,
    ids: &[Id],
    targets: &[(String, Vec<(usize, Folder)>)],
    copy: bool,
) -> PopupMenu {
    let multi_account = targets.len() > 1;
    let mut menu = menu.max_h(px(420.)).scrollable(true);
    for (account, folders) in targets {
        if multi_account {
            menu = menu.label(account.clone());
        }
        for (depth, folder) in folders {
            let folder_id = folder.id;
            let label = format!("{}{}", "    ".repeat(*depth), folder_name(folder));
            menu = menu.item(action(
                label,
                folder_icon(folder),
                state,
                ids,
                move |s, ids, _, cx| ops::move_to(s, ids, folder_id, copy, cx),
            ));
        }
    }
    menu
}

/// Builds the message menu for `ids` (the selection, or the right-clicked row with its
/// conversation). `single` is the message shown when one row is targeted: it gets the
/// open / respond / print entries.
pub fn build(
    menu: PopupMenu,
    state: &Entity<AppState>,
    ids: Vec<Id>,
    single: Option<Id>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let store = state.read(cx).store();
    let messages: Vec<Message> = ids
        .iter()
        .filter_map(|&id| store.get::<Message>(id).ok().flatten())
        .collect();
    if messages.is_empty() {
        return menu;
    }
    let any_unread = messages.iter().any(|m| !m.flags.read);
    let any_unpinned = messages.iter().any(|m| !m.flags.pinned);
    let any_flagged = messages.iter().any(|m| m.flags.flagged);
    let role_of = |message: &Message| {
        store
            .get::<Folder>(message.folder_id)
            .ok()
            .flatten()
            .map(|folder| folder.role)
    };
    let all_in = |role: FolderRole| messages.iter().all(|m| role_of(m) == Some(role));
    let in_trash = all_in(FolderRole::Trash);
    let in_junk = all_in(FolderRole::Junk);
    let in_archive = all_in(FolderRole::Archive);
    let categories: Vec<(String, u32, bool)> = store
        .list_categories()
        .unwrap_or_default()
        .into_iter()
        .map(|category| {
            let all_have = messages
                .iter()
                .all(|m| m.categories.contains(&category.name));
            (category.name, category.color, all_have)
        })
        .collect();
    let targets = destinations(state, cx);
    let muted = single
        .and_then(|id| messages.iter().find(|m| m.id == id))
        .is_some_and(|m| crate::later::is_muted(state, &m.thread_id, cx));

    let mut menu = menu.min_w(px(220.));
    if let Some(id) = single {
        let s = state.clone();
        let respond = |label: &'static str, icon: IconName, kind: ComposeKind| {
            let s = state.clone();
            PopupMenuItem::new(label)
                .icon(icon)
                .on_click(move |_, window, cx| ops::compose_from(&s, id, kind, window, cx))
        };
        menu = menu
            .item(
                PopupMenuItem::new("Open in new window")
                    .icon(IconName::SquareArrowOutUpRight)
                    .on_click(move |_, _, cx| message_window::open(s.clone(), id, cx)),
            )
            .separator()
            .item(respond("Reply", IconName::Reply, ComposeKind::Reply))
            .item(respond(
                "Reply all",
                IconName::ReplyAll,
                ComposeKind::ReplyAll,
            ))
            .item(respond("Forward", IconName::Forward, ComposeKind::Forward))
            .separator();
    }

    menu = menu
        .item(if any_unread {
            action(
                "Mark as read",
                IconName::MailOpen,
                state,
                &ids,
                |s, ids, _, cx| ops::set_read(s, ids, true, cx),
            )
        } else {
            action(
                "Mark as unread",
                IconName::Mail,
                state,
                &ids,
                |s, ids, _, cx| ops::set_read(s, ids, false, cx),
            )
        })
        .item(if any_unpinned {
            action("Pin", IconName::Pin, state, &ids, |s, ids, _, cx| {
                ops::set_pinned(s, ids, true, cx)
            })
        } else {
            action("Unpin", IconName::PinOff, state, &ids, |s, ids, _, cx| {
                ops::set_pinned(s, ids, false, cx)
            })
        });

    menu = menu.item(action(
        "Snooze…",
        IconName::AlarmClock,
        state,
        &ids,
        |s, ids, window, cx| crate::later::open_snooze_picker_for(s.clone(), ids, window, cx),
    ));
    if let Some(id) = single {
        let s = state.clone();
        menu = menu.item(
            PopupMenuItem::new(if muted {
                "Unmute conversation"
            } else {
                "Mute conversation"
            })
            .icon(if muted {
                IconName::Bell
            } else {
                IconName::BellOff
            })
            .on_click(move |_, _, cx| crate::later::toggle_mute(&s, id, cx)),
        );
    }

    let (s, i) = (state.clone(), ids.clone());
    menu = menu.submenu_with_icon(
        Some(Icon::new(IconName::Flag)),
        "Follow up",
        window,
        cx,
        move |sub, _, _| {
            let mut sub = sub;
            for preset in FlagPreset::ALL {
                sub = sub.item(action(
                    preset.label(),
                    if preset == FlagPreset::NoDate {
                        IconName::Flag
                    } else {
                        IconName::CalendarClock
                    },
                    &s,
                    &i,
                    move |s, ids, _, cx| ops::flag_with(s, ids, preset, cx),
                ));
            }
            sub.separator().item(
                action("Clear flag", IconName::FlagOff, &s, &i, |s, ids, _, cx| {
                    ops::set_flag(s, ids, false, cx)
                })
                .disabled(!any_flagged),
            )
        },
    );

    let (s, i) = (state.clone(), ids.clone());
    let category_list = Rc::new(categories);
    menu = menu.submenu_with_icon(
        Some(Icon::new(IconName::Tag)),
        "Categorize",
        window,
        cx,
        move |sub, _, _| {
            let mut sub = sub;
            for (name, color, checked) in category_list.iter() {
                let label = name.clone();
                let color = *color;
                sub = sub.item(
                    PopupMenuItem::element({
                        let label = label.clone();
                        move |_, _| {
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.))
                                .child(
                                    div()
                                        .size(px(10.))
                                        .rounded(px(3.))
                                        .bg(crate::theme::hex(color)),
                                )
                                .child(label.clone())
                        }
                    })
                    .checked(*checked)
                    .on_click({
                        let (s, i) = (s.clone(), i.clone());
                        move |_, _, cx| ops::toggle_category(&s, i.clone(), &label, cx)
                    }),
                );
            }
            sub.separator()
                .item(action(
                    "New category…",
                    IconName::Plus,
                    &s,
                    &i,
                    |s, ids, window, cx| open_category_dialog(s.clone(), ids, window, cx),
                ))
                .item(action(
                    "Clear all categories",
                    IconName::X,
                    &s,
                    &i,
                    |s, ids, _, cx| ops::clear_categories(s, ids, cx),
                ))
        },
    );

    menu = menu.separator();
    let (s, i, t) = (state.clone(), ids.clone(), targets.clone());
    menu = menu.submenu_with_icon(
        Some(Icon::new(IconName::FolderInput)),
        "Move to",
        window,
        cx,
        move |sub, _, _| folder_submenu(sub, &s, &i, &t, false),
    );
    let (s, i, t) = (state.clone(), ids.clone(), targets);
    menu = menu.submenu_with_icon(
        Some(Icon::new(IconName::Copy)),
        "Copy to",
        window,
        cx,
        move |sub, _, _| folder_submenu(sub, &s, &i, &t, true),
    );
    if in_trash {
        menu = menu.item(action(
            "Restore to Inbox",
            IconName::Undo2,
            state,
            &ids,
            |s, ids, _, cx| ops::move_to_role(s, ids, FolderRole::Inbox, cx),
        ));
    }
    if !in_archive {
        menu = menu.item(action(
            "Archive",
            IconName::Archive,
            state,
            &ids,
            |s, ids, _, cx| ops::move_to_role(s, ids, FolderRole::Archive, cx),
        ));
    }
    menu = menu.item(if in_junk {
        action(
            "Not junk",
            IconName::ShieldCheck,
            state,
            &ids,
            |s, ids, _, cx| ops::move_to_role(s, ids, FolderRole::Inbox, cx),
        )
    } else {
        action(
            "Move to Junk",
            IconName::OctagonAlert,
            state,
            &ids,
            |s, ids, _, cx| ops::move_to_role(s, ids, FolderRole::Junk, cx),
        )
    });

    if let Some(id) = single {
        let (s1, s2) = (state.clone(), state.clone());
        menu = menu
            .separator()
            .item(
                PopupMenuItem::new("Print")
                    .icon(IconName::Printer)
                    .on_click(move |_, _, cx| crate::print::print_message(&s1, id, cx)),
            )
            .item(
                PopupMenuItem::new("Save as .eml")
                    .icon(IconName::FileDown)
                    .on_click(move |_, _, cx| crate::print::save_as_eml(&s2, id, cx)),
            );
    }

    menu.separator().item(action(
        if in_trash {
            "Delete permanently"
        } else {
            "Delete"
        },
        IconName::Trash,
        state,
        &ids,
        ops::delete,
    ))
}

// ---------- pickers (ribbon / palette "Move to folder" and "Categorize") ----------

/// A selectable dialog row: leading icon or dot, label, check mark.
fn picker_row(
    id: impl Into<ElementId>,
    leading: AnyElement,
    label: SharedString,
    checked: bool,
    cx: &App,
) -> Stateful<Div> {
    let t = crate::theme::theme(cx);
    let (hover, accent) = (t.hover, t.accent);
    div()
        .id(id)
        .flex()
        .flex_row()
        .flex_shrink_0()
        .items_center()
        .gap(px(8.))
        .h(px(30.))
        .px(px(8.))
        .rounded(crate::theme::RADIUS_MD)
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .child(leading)
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .truncate()
                .text_size(crate::theme::TEXT_SM)
                .child(label),
        )
        .when(checked, |row| {
            row.child(
                gpui::component::Icon::new(IconName::Check)
                    .size(crate::theme::ICON_SIZE_SM)
                    .text_color(accent),
            )
        })
}

/// "Move to folder" for `ids`: every folder of every account, as a tree.
pub fn open_move_dialog(state: Entity<AppState>, ids: Vec<Id>, window: &mut Window, cx: &mut App) {
    use gpui::component::WindowExt as _;
    if ids.is_empty() {
        return;
    }
    let current: Vec<Id> = {
        let store = state.read(cx).store();
        ids.iter()
            .filter_map(|&id| store.get::<Message>(id).ok().flatten())
            .map(|m| m.folder_id)
            .collect()
    };
    let targets = destinations(&state, cx);
    let title: SharedString = if ids.len() == 1 {
        "Move to folder".into()
    } else {
        format!("Move {} messages to", ids.len()).into()
    };
    window.open_dialog(cx, move |dialog, _, cx| {
        let t = crate::theme::theme(cx).clone();
        let multi_account = targets.len() > 1;
        let mut list = div().flex().flex_col().gap(px(2.));
        for (account, folders) in targets.iter() {
            if multi_account {
                list = list.child(
                    div()
                        .pt(px(8.))
                        .pb(px(2.))
                        .px(px(8.))
                        .text_size(crate::theme::TEXT_XS)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.text_subtle)
                        .child(account.to_uppercase()),
                );
            }
            for (depth, folder) in folders {
                let folder_id = folder.id;
                let (state, ids) = (state.clone(), ids.clone());
                let icon = div()
                    .pl(px(*depth as f32 * 16.))
                    .child(
                        gpui::component::Icon::new(folder_icon(folder))
                            .size(crate::theme::ICON_SIZE_SM)
                            .text_color(t.text_muted),
                    )
                    .into_any_element();
                let here = !current.is_empty() && current.iter().all(|&f| f == folder_id);
                list = list.child(
                    picker_row(
                        ("move-folder", folder_id as u64),
                        icon,
                        folder_name(folder).into(),
                        here,
                        cx,
                    )
                    .on_click(move |_, window, cx| {
                        ops::move_to(&state, ids.clone(), folder_id, false, cx);
                        window.close_dialog(cx);
                    }),
                );
            }
        }
        dialog.w(px(360.)).title(title.clone()).child(
            div()
                .id("move-picker")
                .max_h(px(420.))
                .overflow_y_scroll()
                .child(list),
        )
    });
}

/// "Categorize" for `ids`: toggles each category on all of them.
pub fn open_category_dialog(
    state: Entity<AppState>,
    ids: Vec<Id>,
    window: &mut Window,
    cx: &mut App,
) {
    use gpui::component::WindowExt as _;
    if ids.is_empty() {
        return;
    }
    use gpui::component::input::{Input, InputEvent, InputState};
    let input = cx.new(|cx| InputState::new(window, cx).placeholder("New category, then Enter"));
    let (s, i) = (state.clone(), ids.clone());
    window
        .subscribe(&input, cx, move |input, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                let name = input.read(cx).value().trim().to_owned();
                if !name.is_empty() && create_category(&s, &i, &name, cx) {
                    input.update(cx, |input, cx| input.set_value("", window, cx));
                }
            }
        })
        .detach();
    window.open_dialog(cx, move |dialog, _, cx| {
        // Re-read on each render so the list and checks follow the changes.
        let store = state.read(cx).store();
        let categories = store.list_categories().unwrap_or_default();
        let messages: Vec<Message> = ids
            .iter()
            .filter_map(|&id| store.get::<Message>(id).ok().flatten())
            .collect();
        let rows: Vec<_> = categories
            .iter()
            .map(|category| {
                let checked = !messages.is_empty()
                    && messages
                        .iter()
                        .all(|m| m.categories.contains(&category.name));
                let dot = div()
                    .size(px(10.))
                    .rounded(px(3.))
                    .bg(crate::theme::hex(category.color))
                    .into_any_element();
                let (state, ids, name) = (state.clone(), ids.clone(), category.name.clone());
                picker_row(
                    ("category", category.id as u64),
                    dot,
                    category.name.clone().into(),
                    checked,
                    cx,
                )
                .on_click(move |_, _, cx| ops::toggle_category(&state, ids.clone(), &name, cx))
            })
            .collect();
        dialog
            .w(px(320.))
            .title("Categorize")
            .child(
                div()
                    .id("category-picker")
                    .max_h(px(380.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .children(rows),
            )
            .child(div().pt(px(8.)).child(Input::new(&input)))
    });
}

/// Colours given to new categories, in turn.
const CATEGORY_COLORS: [u32; 8] = [
    0x3b82f6, 0x22c55e, 0xf59e0b, 0xef4444, 0xa855f7, 0x14b8a6, 0xec4899, 0x64748b,
];

/// Creates category `name` (unless it exists, in any case) and adds it to `ids`.
fn create_category(state: &Entity<AppState>, ids: &[Id], name: &str, cx: &mut App) -> bool {
    let existing = state.read(cx).store().list_categories().unwrap_or_default();
    let name = match existing.iter().find(|c| c.name.eq_ignore_ascii_case(name)) {
        Some(category) => category.name.clone(),
        None => {
            let category = mailbox_core::Category {
                id: 0,
                name: name.to_owned(),
                color: CATEGORY_COLORS[existing.len() % CATEGORY_COLORS.len()],
            };
            if !ops::run(
                state,
                Ok(mailbox_core::Command::SaveCategory(Box::new(category))),
                cx,
            ) {
                return false;
            }
            name.to_owned()
        }
    };
    // Toggling would remove a category every message already has.
    if !all_have_category(state, ids, &name, cx) {
        ops::toggle_category(state, ids.to_vec(), &name, cx);
    }
    true
}

/// Whether every message of `ids` has category `name`.
fn all_have_category(state: &Entity<AppState>, ids: &[Id], name: &str, cx: &App) -> bool {
    let store = state.read(cx).store();
    ids.iter()
        .filter_map(|&id| store.get::<Message>(id).ok().flatten())
        .all(|message| message.categories.iter().any(|c| c == name))
}
