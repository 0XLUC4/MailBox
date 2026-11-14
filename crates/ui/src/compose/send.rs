//! Undo send: the draft is already saved; after [`UNDO_SEND_DELAY`] it is built into an RFC 5322
//! message and queued with `Command::QueueSend`. "Undo" in the toast re-opens the draft instead.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use chrono::Utc;
use gpui::component::WindowExt as _;
use gpui::component::button::Button;
use gpui::component::notification::Notification;
use gpui::*;
use mailbox_core::compose::{self, OutboxItem, OutgoingAttachment};
use mailbox_core::{Command, Id};

use super::{ComposeRequest, dock, open_compose};
use crate::app_state::AppState;

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

/// Builds the message from the saved draft and queues it (draft moves to Sent).
fn queue(state: &Entity<AppState>, message_id: Id, cx: &mut App) {
    let item = build_item(state.read(cx), message_id);
    state.update(cx, |state, cx| match item {
        Ok(item) => {
            if state.run(Command::QueueSend(Box::new(item)), cx) {
                state.notify_info("Message sent to the outbox", cx);
            }
        }
        Err(err) => state.report_error(format!("Not sent: {err}. It is still in Drafts."), cx),
    });
}

fn build_item(state: &AppState, message_id: Id) -> Result<OutboxItem, String> {
    let draft = compose::load_draft(state.store(), message_id)
        .map_err(|err| err.to_string())?
        .ok_or("the draft was deleted")?;
    let mut files = Vec::with_capacity(draft.meta.attachments.len());
    for attachment in &draft.meta.attachments {
        let data = std::fs::read(&attachment.path)
            .map_err(|err| format!("cannot read {}: {err}", attachment.name))?;
        files.push(OutgoingAttachment {
            name: attachment.name.clone(),
            mime: attachment.mime.clone(),
            data,
        });
    }
    let now = Utc::now();
    let outgoing = compose::outgoing(&draft, files, now);
    Ok(OutboxItem {
        id: 0,
        account_id: draft.message.account_id,
        message_id: Some(message_id),
        envelope: outgoing.envelope(),
        raw: compose::build_message(&outgoing),
        queued_at: now,
    })
}
