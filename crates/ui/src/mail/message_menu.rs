//! Right-click menu for one or more messages (also the reading pane's "More" menu).

use std::rc::Rc;

use gpui::component::Icon;
use gpui::component::menu::{PopupMenu, PopupMenuItem};
use gpui::*;
use mailbox_core::mail_ops::FlagPreset;
use mailbox_core::{Folder, FolderRole, Id, Message};

use super::folder_pane::{folder_icon, folder_name, folder_tree};
use super::{message_window, ops};
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
fn destinations(state: &Entity<AppState>, cx: &App) -> Rc<Vec<(String, Vec<(usize, Folder)>)>> {
    let store = state.read(cx).store();
    Rc::new(
        store
            .list_accounts()
            .unwrap_or_default()
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

/// Builds the message menu for `ids` (the selection, or the right-clicked row).
pub fn build(
    menu: PopupMenu,
    state: &Entity<AppState>,
    ids: Vec<Id>,
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
                .all(|m| m.categories.iter().any(|c| *c == category.name));
            (category.name, category.color, all_have)
        })
        .collect();
    let single = (ids.len() == 1).then(|| ids[0]);
    let targets = destinations(state, cx);

    let mut menu = menu.min_w(px(220.));
    if let Some(id) = single {
        let s = state.clone();
        menu = menu
            .item(
                PopupMenuItem::new("Open in new window")
                    .icon(IconName::SquareArrowOutUpRight)
                    .on_click(move |_, _, cx| message_window::open(s.clone(), id, cx)),
            )
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
            sub.separator().item(action(
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
