//! Snooze, send later and mute conversation (state and presets in `mailbox_core::later`).
//!
//! - Snooze moves messages to the account's "Snoozed" folder (undoable) and remembers where
//!   they came from; the scheduler moves them back, unread, when the time comes.
//! - Send later keeps the saved draft in Drafts; at the chosen time it is queued through the
//!   normal send path (Outbox → Send/Receive). Editing or sending it earlier is harmless.
//! - Muted conversations: new messages are marked read and archived on arrival.

use std::rc::Rc;
use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use gpui::component::WindowExt as _;
use gpui::*;
use mailbox_core::later::{Later, LaterPreset, SNOOZED_FOLDER};
use mailbox_core::{Command, Folder, FolderRole, Id, Message, mail_ops};

use crate::app_state::AppState;
use crate::components::{Icon, IconName};
use crate::notifications::{self, Activation};
use crate::theme::{ICON_SIZE_SM, RADIUS_MD, SPACE_1, SPACE_2, TEXT_SM, TEXT_XS, theme};

/// Called with the moment picked in [`open_time_picker`].
pub type OnPick = Rc<dyn Fn(DateTime<Utc>, &mut Window, &mut App)>;

/// How often snoozes and scheduled sends are checked.
const TICK: Duration = Duration::from_secs(30);

/// Starts the scheduler. Call once.
pub fn init(state: Entity<AppState>, cx: &mut App) {
    cx.spawn(async move |cx| {
        let mut delay = Duration::from_secs(3);
        loop {
            cx.background_executor().timer(delay).await;
            delay = TICK;
            cx.update(|cx| tick(&state, cx));
        }
    })
    .detach();
}

fn load(state: &Entity<AppState>, cx: &App) -> Later {
    Later::load(state.read(cx).store()).unwrap_or_default()
}

/// Persists `later` outside the undo history.
fn save(state: &Entity<AppState>, later: &Later, cx: &mut App) -> bool {
    match later.command() {
        Ok(command) => state.update(cx, |state, cx| state.run_untracked(command, cx)),
        Err(err) => {
            state.update(cx, |state, cx| state.report_error(err, cx));
            false
        }
    }
}

fn tick(state: &Entity<AppState>, cx: &mut App) {
    let mut later = load(state, cx);
    let now = Utc::now();
    let snoozes = later.take_due_snoozes(now);
    let sends = later.take_due_sends(now);
    if snoozes.is_empty() && sends.is_empty() {
        return;
    }
    if !save(state, &later, cx) {
        return;
    }
    for snoozed in snoozes {
        wake(state, snoozed.message_id, snoozed.return_folder, cx);
    }
    for scheduled in sends {
        let in_drafts = {
            let store = state.read(cx).store();
            store
                .get::<Message>(scheduled.message_id)
                .ok()
                .flatten()
                .and_then(|message| store.get::<Folder>(message.folder_id).ok().flatten())
                .is_some_and(|folder| folder.role == FolderRole::Drafts)
        };
        // Sent, deleted or moved in the meantime: nothing to do.
        if in_drafts {
            crate::compose::queue_draft(state, scheduled.message_id, cx);
        }
    }
}

/// Brings a snoozed message back (unread) unless the user already moved it elsewhere.
fn wake(state: &Entity<AppState>, id: Id, return_folder: Id, cx: &mut App) {
    let (message, target) = {
        let store = state.read(cx).store();
        let Some(message) = store.get::<Message>(id).ok().flatten() else {
            return;
        };
        let still_snoozed = store
            .get::<Folder>(message.folder_id)
            .ok()
            .flatten()
            .is_some_and(|folder| folder.name == SNOOZED_FOLDER);
        if !still_snoozed {
            return;
        }
        let target = match store.get::<Folder>(return_folder).ok().flatten() {
            Some(folder) => Some(folder.id),
            None => store
                .folder_by_role(message.account_id, FolderRole::Inbox)
                .ok()
                .flatten()
                .map(|folder| folder.id),
        };
        (message, target)
    };
    let Some(target) = target else {
        return;
    };
    state.update(cx, |state, cx| {
        state.run_untracked(
            Command::MoveMessages {
                ids: vec![id],
                folder_id: target,
            },
            cx,
        );
        state.run_untracked(
            Command::SetMessageFlags {
                ids: vec![id],
                read: Some(false),
                flagged: None,
                pinned: None,
            },
            cx,
        );
    });
    notifications::show(
        &format!("Snoozed: {}", message.from.display()),
        &message.subject,
        Activation::Message(id),
        cx,
    );
}

/// The account's "Snoozed" folder, created when missing.
fn snoozed_folder(state: &Entity<AppState>, account_id: Id, cx: &mut App) -> Option<Id> {
    let find = |state: &AppState| {
        state
            .store()
            .list_folders(account_id)
            .unwrap_or_default()
            .into_iter()
            .find(|folder| folder.parent_id.is_none() && folder.name == SNOOZED_FOLDER)
            .map(|folder| folder.id)
    };
    if let Some(id) = find(state.read(cx)) {
        return Some(id);
    }
    state.update(cx, |state, cx| {
        match mail_ops::new_folder(state.store(), account_id, None, SNOOZED_FOLDER) {
            Ok(command) => {
                state.run_untracked(command, cx);
            }
            Err(err) => state.report_error(err, cx),
        }
        find(state)
    })
}

/// Snoozes messages until `until` (the move is undoable; the wake-up is not).
pub fn snooze(state: &Entity<AppState>, ids: &[Id], until: DateTime<Utc>, cx: &mut App) {
    let messages: Vec<Message> = {
        let store = state.read(cx).store();
        ids.iter()
            .filter_map(|id| store.get::<Message>(*id).ok().flatten())
            .collect()
    };
    let Some(first) = messages.first() else {
        return;
    };
    let Some(folder_id) = snoozed_folder(state, first.account_id, cx) else {
        return;
    };
    let mut later = load(state, cx);
    let moving: Vec<Id> = messages
        .iter()
        .filter(|message| message.account_id == first.account_id && message.folder_id != folder_id)
        .map(|message| {
            later.snooze(message.id, until, message.folder_id);
            message.id
        })
        .collect();
    if moving.is_empty() || !save(state, &later, cx) {
        return;
    }
    let when = until.with_timezone(&Local).format("%a %d %b, %H:%M");
    state.update(cx, |state, cx| {
        if state.run(
            Command::MoveMessages {
                ids: moving,
                folder_id,
            },
            cx,
        ) {
            state.select_message(None, cx);
            state.notify_info(format!("Snoozed until {when}"), cx);
        }
    });
}

/// Preset picker for snoozing the selected message.
pub fn open_snooze_picker(state: Entity<AppState>, window: &mut Window, cx: &mut App) {
    let Some(id) = state.read(cx).selected_message() else {
        return state.update(cx, |state, cx| {
            state.notify_info("Select a message first", cx)
        });
    };
    let on_pick: OnPick = Rc::new(move |until, _, cx| snooze(&state, &[id], until, cx));
    open_time_picker("Snooze until", on_pick, window, cx);
}

/// A dialog listing [`LaterPreset`]s; `on_pick` gets the chosen moment.
pub fn open_time_picker(title: &'static str, on_pick: OnPick, window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, move |dialog, _, cx| {
        let t = theme(cx).clone();
        let now = Local::now();
        let rows = LaterPreset::ALL
            .into_iter()
            .enumerate()
            .map(|(index, preset)| {
                let at = preset.at(now);
                let on_pick = on_pick.clone();
                div()
                    .id(("later-preset", index))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(SPACE_2)
                    .h(px(36.))
                    .px(SPACE_2)
                    .rounded(RADIUS_MD)
                    .cursor_pointer()
                    .hover(|style| style.bg(t.hover))
                    .child(
                        Icon::new(IconName::AlarmClock)
                            .size(ICON_SIZE_SM)
                            .text_color(t.text_muted),
                    )
                    .child(div().flex_1().text_size(TEXT_SM).child(preset.label()))
                    .child(
                        div().text_size(TEXT_XS).text_color(t.text_subtle).child(
                            at.with_timezone(&Local)
                                .format("%a %d %b, %H:%M")
                                .to_string(),
                        ),
                    )
                    .on_click(move |_, window, cx| {
                        window.close_dialog(cx);
                        on_pick(at, window, cx);
                    })
            });
        dialog
            .w(px(360.))
            .title(title)
            .child(div().flex().flex_col().gap(SPACE_1).children(rows))
    });
}

/// Records a draft to send at `at` (the draft stays in Drafts until then).
pub fn schedule_send(state: &Entity<AppState>, message_id: Id, at: DateTime<Utc>, cx: &mut App) {
    let mut later = load(state, cx);
    later.schedule(message_id, at);
    if save(state, &later, cx) {
        let when = at.with_timezone(&Local).format("%a %d %b, %H:%M");
        state.update(cx, |state, cx| {
            state.notify_info(
                format!("Will be sent {when} (kept in Drafts until then)"),
                cx,
            )
        });
    }
}

/// Mutes or unmutes the selected message's conversation.
pub fn toggle_mute_selected(state: &Entity<AppState>, cx: &mut App) {
    let message = {
        let state = state.read(cx);
        state
            .selected_message()
            .and_then(|id| state.store().get::<Message>(id).ok().flatten())
    };
    let Some(message) = message.filter(|message| !message.thread_id.is_empty()) else {
        return state.update(cx, |state, cx| {
            state.notify_info("Select a message first", cx)
        });
    };
    let mut later = load(state, cx);
    let muted = later.toggle_mute(&message.thread_id);
    if !save(state, &later, cx) {
        return;
    }
    state.update(cx, |state, cx| {
        if muted {
            let unread: Vec<Id> = state
                .store()
                .list_thread(&message.thread_id)
                .unwrap_or_default()
                .into_iter()
                .filter(|message| !message.flags.read)
                .map(|message| message.id)
                .collect();
            if !unread.is_empty() {
                state.run(
                    Command::SetMessageFlags {
                        ids: unread,
                        read: Some(true),
                        flagged: None,
                        pinned: None,
                    },
                    cx,
                );
            }
            state.notify_info("Conversation muted: new replies skip the inbox", cx);
        } else {
            state.notify_info("Conversation unmuted", cx);
        }
    });
}

/// Marks read and archives new messages of muted conversations. Returns the other ids.
pub fn archive_muted(state: &Entity<AppState>, ids: Vec<Id>, cx: &mut App) -> Vec<Id> {
    let later = load(state, cx);
    if later.muted_threads.is_empty() {
        return ids;
    }
    let (muted, rest): (Vec<Message>, Vec<Message>) = {
        let store = state.read(cx).store();
        ids.iter()
            .filter_map(|id| store.get::<Message>(*id).ok().flatten())
            .partition(|message| later.is_muted(&message.thread_id))
    };
    for message in &muted {
        state.update(cx, |state, cx| {
            state.run_untracked(
                Command::SetMessageFlags {
                    ids: vec![message.id],
                    read: Some(true),
                    flagged: None,
                    pinned: None,
                },
                cx,
            );
            let archive = mail_ops::move_to_role(state.store(), &[message.id], FolderRole::Archive);
            if let Ok(command) = archive {
                state.run_untracked(command, cx);
            }
        });
    }
    rest.into_iter().map(|message| message.id).collect()
}
