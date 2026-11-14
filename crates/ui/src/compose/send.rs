//! Undo send: the draft is already saved; after [`UNDO_SEND_DELAY`] its attachments are copied
//! where the sync engine reads them and `Command::QueueSend` moves it into the account's Outbox
//! folder, then Send/Receive starts. "Undo" in the toast re-opens the draft instead.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui::component::WindowExt as _;
use gpui::component::button::Button;
use gpui::component::notification::Notification;
use gpui::*;
use mailbox_core::compose;
use mailbox_core::{Command, Id};

use super::{ComposeRequest, dock, open_compose};
use crate::app_state::AppState;
use crate::sync_bridge;

pub const UNDO_SEND_DELAY: Duration = Duration::from_secs(5);

/// Notification id type for the undo-send toast.
struct UndoSendToast;

/// Shows the "Sending…" toast and queues draft `message_id` once the delay has passed.
pub fn schedule(state: Entity<AppState>, message_id: Id, subject: String, cx: &mut App) {
    let cancelled = Rc::new(Cell::new(false));
    let key = SharedString::from(format!("undo-send-{message_id}"));
    let main = dock::main_window(cx);

    if let Some(main) = main {
        let (cancel, toast_key, toast_state) = (cancelled.clone(), key.clone(), state.clone());
        let subject = if subject.trim().is_empty() {
            "(no subject)".to_owned()
        } else {
            subject
        };
        let notification = Notification::info(format!("“{subject}”"))
            .id1::<UndoSendToast>(key.clone())
            .title("Sending…")
            .action(move |_, _, _| {
                let (cancel, key, state) = (cancel.clone(), toast_key.clone(), toast_state.clone());
                Button::new("undo-send")
                    .label("Undo")
                    .on_click(move |_, window, cx| {
                        if cancel.replace(true) {
                            return;
                        }
                        window.remove_notification1::<UndoSendToast>(key.clone(), cx);
                        open_compose(&state, ComposeRequest::EditDraft(message_id), window, cx);
                    })
            });
        // Deferred: the caller may be updating that very window right now.
        cx.defer(move |cx| {
            main.update(cx, |_, window, cx| {
                window.push_notification(notification, cx)
            })
            .ok();
        });
    }

    cx.spawn(async move |cx| {
        cx.background_executor().timer(UNDO_SEND_DELAY).await;
        cx.update(|cx| {
            if cancelled.replace(true) {
                return;
            }
            if let Some(main) = main {
                main.update(cx, |_, window, cx| {
                    window.remove_notification1::<UndoSendToast>(key, cx);
                })
                .ok();
            }
            queue(&state, message_id, cx);
        });
    })
    .detach();
}

/// Moves the saved draft into the Outbox (the sync engine sends it and files it in Sent).
pub(super) fn queue(state: &Entity<AppState>, message_id: Id, cx: &mut App) {
    let account_id = match prepare(state.read(cx), message_id) {
        Ok(account_id) => account_id,
        Err(err) => return fail(state, err, cx),
    };
    let Some(folder_id) = sync_bridge::outbox_folder(account_id, cx) else {
        return fail(state, "the Outbox folder could not be created".into(), cx);
    };
    let queued = state.update(cx, |state, cx| {
        state.run_untracked(
            Command::QueueSend {
                message_id,
                folder_id,
            },
            cx,
        )
    });
    if queued {
        state.update(cx, |state, cx| {
            state.notify_info("Message moved to the Outbox", cx)
        });
        sync_bridge::send_receive(cx);
    }
}

/// Copies attachments where the sync engine reads them. Returns the draft's account.
fn prepare(state: &AppState, message_id: Id) -> Result<Id, String> {
    let draft = compose::load_draft(state.store(), message_id)
        .map_err(|err| err.to_string())?
        .ok_or("the draft was deleted")?;
    for attachment in &draft.meta.attachments {
        let target = sync_bridge::attachment_path(message_id, &attachment.name);
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir).map_err(|err| err.to_string())?;
        }
        std::fs::copy(&attachment.path, &target)
            .map_err(|err| format!("cannot read {}: {err}", attachment.name))?;
    }
    Ok(draft.message.account_id)
}

fn fail(state: &Entity<AppState>, err: String, cx: &mut App) {
    state.update(cx, |state, cx| {
        state.report_error(format!("Not sent: {err}. It is still in Drafts."), cx)
    });
}
