//! Mail actions shared by the list, the reading pane, context menus and drag & drop.
//! Each one builds a command (see `mailbox_core::mail_ops`) and runs it through `AppState`
//! so it is undoable and errors show as toasts.

use chrono::Local;
use gpui::component::WindowExt as _;
use gpui::component::button::ButtonVariant;
use gpui::*;
use mailbox_core::mail_ops::{self, Deletion, FlagPreset};
use mailbox_core::{Command, FolderRole, Id, Message};

use super::compose_request::{ComposeRequest, request_compose};
use crate::app_state::AppState;

/// Runs a built command, or reports the error that prevented building it.
pub fn run(state: &Entity<AppState>, command: mailbox_core::Result<Command>, cx: &mut App) -> bool {
    state.update(cx, |state, cx| match command {
        Ok(command) => state.run(command, cx),
        Err(err) => {
            state.report_error(err, cx);
            false
        }
    })
}

fn messages(state: &Entity<AppState>, ids: &[Id], cx: &App) -> Vec<Message> {
    let store = state.read(cx).store();
    ids.iter()
        .filter_map(|&id| store.get::<Message>(id).ok().flatten())
        .collect()
}

/// Clears the reading pane when it shows one of `ids` (after a move or delete).
fn deselect(state: &Entity<AppState>, ids: &[Id], cx: &mut App) {
    state.update(cx, |state, cx| {
        if state.selected_message().is_some_and(|id| ids.contains(&id)) {
            state.select_message(None, cx);
        }
    });
}

pub fn set_read(state: &Entity<AppState>, ids: Vec<Id>, read: bool, cx: &mut App) {
    run(
        state,
        Ok(Command::SetMessageFlags {
            ids,
            read: Some(read),
            flagged: None,
            pinned: None,
        }),
        cx,
    );
}

/// Read when any is unread, unread otherwise.
pub fn toggle_read(state: &Entity<AppState>, ids: Vec<Id>, cx: &mut App) {
    let any_unread = messages(state, &ids, cx).iter().any(|m| !m.flags.read);
    set_read(state, ids, any_unread, cx);
}

pub fn set_flag(state: &Entity<AppState>, ids: Vec<Id>, flagged: bool, cx: &mut App) {
    run(
        state,
        Ok(Command::FlagMessages {
            ids,
            flagged,
            due: None,
        }),
        cx,
    );
}

/// Flags when any is unflagged, clears the flags otherwise.
pub fn toggle_flag(state: &Entity<AppState>, ids: Vec<Id>, cx: &mut App) {
    let any_unflagged = messages(state, &ids, cx).iter().any(|m| !m.flags.flagged);
    set_flag(state, ids, any_unflagged, cx);
}

pub fn flag_with(state: &Entity<AppState>, ids: Vec<Id>, preset: FlagPreset, cx: &mut App) {
    run(
        state,
        Ok(Command::FlagMessages {
            ids,
            flagged: true,
            due: preset.due(Local::now().date_naive()),
        }),
        cx,
    );
}

pub fn set_pinned(state: &Entity<AppState>, ids: Vec<Id>, pinned: bool, cx: &mut App) {
    run(
        state,
        Ok(Command::SetMessageFlags {
            ids,
            read: None,
            flagged: None,
            pinned: Some(pinned),
        }),
        cx,
    );
}

pub fn toggle_category(state: &Entity<AppState>, ids: Vec<Id>, name: &str, cx: &mut App) {
    let command = mail_ops::toggle_category(state.read(cx).store(), &ids, name);
    run(state, command, cx);
}

pub fn clear_categories(state: &Entity<AppState>, ids: Vec<Id>, cx: &mut App) {
    run(
        state,
        Ok(Command::SetMessageCategories {
            ids,
            categories: Vec::new(),
        }),
        cx,
    );
}

/// Move (or copy) into a folder: drag & drop and the Move/Copy menus.
pub fn move_to(state: &Entity<AppState>, ids: Vec<Id>, folder_id: Id, copy: bool, cx: &mut App) {
    if copy {
        run(state, Ok(Command::CopyMessages { ids, folder_id }), cx);
        return;
    }
    // Skip messages already in the target folder.
    let ids: Vec<Id> = messages(state, &ids, cx)
        .into_iter()
        .filter(|m| m.folder_id != folder_id)
        .map(|m| m.id)
        .collect();
    if ids.is_empty() {
        return;
    }
    if run(
        state,
        Ok(Command::MoveMessages {
            ids: ids.clone(),
            folder_id,
        }),
        cx,
    ) {
        deselect(state, &ids, cx);
    }
}

/// Archive, Junk, "Not junk" (Inbox)...
pub fn move_to_role(state: &Entity<AppState>, ids: Vec<Id>, role: FolderRole, cx: &mut App) {
    let command = mail_ops::move_to_role(state.read(cx).store(), &ids, role);
    if run(state, command, cx) {
        deselect(state, &ids, cx);
    }
}

/// Two-step delete: to Deleted Items, or permanently (after confirmation) from there.
pub fn delete(state: &Entity<AppState>, ids: Vec<Id>, window: &mut Window, cx: &mut App) {
    if ids.is_empty() {
        return;
    }
    let deletion = mail_ops::delete_messages(state.read(cx).store(), &ids);
    match deletion {
        Err(err) => state.update(cx, |state, cx| state.report_error(err, cx)),
        Ok(Deletion::MoveToTrash(command)) => {
            if run(state, Ok(command), cx) {
                deselect(state, &ids, cx);
            }
        }
        Ok(Deletion::Permanent(command)) => {
            let count = ids.len();
            let state = state.clone();
            window.open_alert_dialog(cx, move |alert, _, _| {
                let state = state.clone();
                let command = command.clone();
                let ids = ids.clone();
                alert
                    .title("Delete permanently?")
                    .description(if count == 1 {
                        "This message will be removed from Deleted Items.".to_owned()
                    } else {
                        format!("These {count} messages will be removed from Deleted Items.")
                    })
                    .confirm()
                    .ok_text("Delete")
                    .ok_variant(ButtonVariant::Danger)
                    .on_ok(move |_, _, cx| {
                        if run(&state, Ok(command.clone()), cx) {
                            deselect(&state, &ids, cx);
                        }
                        true
                    })
            });
        }
    }
}

/// Opens compose for a reply / reply all / forward of `id`.
pub fn compose_from(
    state: &Entity<AppState>,
    id: Id,
    kind: super::ComposeKind,
    window: &mut Window,
    cx: &mut App,
) {
    let store = state.read(cx).store();
    let Some(message) = store.get::<Message>(id).ok().flatten() else {
        return;
    };
    let me = store
        .list_accounts()
        .unwrap_or_default()
        .into_iter()
        .find(|account| account.id == message.account_id)
        .map(|account| account.email)
        .unwrap_or_default();
    let request = match kind {
        super::ComposeKind::Reply => ComposeRequest::reply(&message, &me, false),
        super::ComposeKind::ReplyAll => ComposeRequest::reply(&message, &me, true),
        super::ComposeKind::Forward => ComposeRequest::forward(&message),
        super::ComposeKind::New => ComposeRequest::new_to(message.from.clone()),
    };
    send_to_compose(state, request, window, cx);
}

pub fn send_to_compose(
    state: &Entity<AppState>,
    request: ComposeRequest,
    window: &mut Window,
    cx: &mut App,
) {
    if !request_compose(request, window, cx) {
        state.update(cx, |state, cx| {
            state.notify_info("Compose isn't available yet", cx)
        });
    }
}

/// Opens a sanitized link: web links in the browser, `mailto:` in compose.
pub fn open_link(state: &Entity<AppState>, href: &str, window: &mut Window, cx: &mut App) {
    if let Some(address) = href.strip_prefix("mailto:") {
        let email = address.split('?').next().unwrap_or(address).to_owned();
        let request = ComposeRequest::new_to(mailbox_core::Address::new("", email));
        send_to_compose(state, request, window, cx);
    } else {
        cx.open_url(href);
    }
}
