//! Read receipts for incoming mail. The sync bridge records `Disposition-Notification-To`
//! requests ([`note_request`]); when such a message is opened the policy decides: ask
//! (Send / Ignore, with "always" / "never" shortcuts), always send, or never send.

use std::cell::Cell;
use std::rc::Rc;

use chrono::Utc;
use gpui::component::WindowExt as _;
use gpui::*;
use mailbox_core::receipts::{ReceiptPolicy, Receipts, receipt_draft};
use mailbox_core::{Address, Id, Message};

use crate::app_state::AppState;
use crate::auto_reply::{account_and_drafts, queue_plain_draft};
use crate::components::{Button, IconName};
use crate::settings::tr;
use crate::theme::{DIALOG_MD, DIALOG_SM, SPACE_2, SPACE_3, TEXT_SM, theme};

fn load(state: &AppState) -> Receipts {
    Receipts::load(state.store()).unwrap_or_default()
}

fn save(state: &Entity<AppState>, receipts: &Receipts, cx: &mut App) {
    match receipts.command() {
        Ok(command) => {
            state.update(cx, |state, cx| state.run_untracked(command, cx));
        }
        Err(err) => state.update(cx, |state, cx| state.report_error(err, cx)),
    }
}

/// Remembers that message `id` asks for a read receipt to `to` (sync bridge hook).
pub fn note_request(state: &Entity<AppState>, id: Id, to: Address, cx: &mut App) {
    let mut receipts = load(state.read(cx));
    if receipts.policy == ReceiptPolicy::Never || receipts.pending.contains_key(&id) {
        return;
    }
    receipts.pending.insert(id, to);
    save(state, &receipts, cx);
}

/// Watches the selection: opening a message with a pending request applies the policy.
pub fn init(state: Entity<AppState>, main_window: AnyWindowHandle, cx: &mut App) {
    let last = Cell::new(None::<Id>);
    cx.observe(&state, move |state, cx| {
        let selected = state.read(cx).selected_message();
        if selected == last.replace(selected) {
            return;
        }
        let Some(id) = selected else {
            return;
        };
        let receipts = load(state.read(cx));
        let Some(to) = receipts.pending.get(&id).cloned() else {
            return;
        };
        let policy = receipts.policy;
        // Outside the notification: sending goes through the sync bridge and dialogs.
        cx.defer(move |cx| match policy {
            ReceiptPolicy::Always => resolve(&state, id, Some(to), cx),
            ReceiptPolicy::Never => resolve(&state, id, None, cx),
            ReceiptPolicy::Ask => {
                main_window
                    .update(cx, |_, window, cx| ask(state, id, to, window, cx))
                    .ok();
            }
        });
    })
    .detach();
}

/// Drops the pending request; sends the receipt when `to` is given.
fn resolve(state: &Entity<AppState>, id: Id, to: Option<Address>, cx: &mut App) {
    let mut receipts = load(state.read(cx));
    if receipts.pending.remove(&id).is_none() {
        return;
    }
    save(state, &receipts, cx);
    let Some(to) = to else {
        return;
    };
    let draft = {
        let state = state.read(cx);
        let original = state.store().get::<Message>(id).ok().flatten();
        original.and_then(|original| {
            let (account, drafts) = account_and_drafts(state, original.account_id)?;
            Some(receipt_draft(&account, &original, to, drafts, Utc::now()))
        })
    };
    if let Some(draft) = draft {
        queue_plain_draft(state, draft, cx);
    }
}

fn set_policy(state: &Entity<AppState>, policy: ReceiptPolicy, cx: &mut App) {
    let mut receipts = load(state.read(cx));
    receipts.policy = policy;
    save(state, &receipts, cx);
}

fn ask(state: Entity<AppState>, id: Id, to: Address, window: &mut Window, cx: &mut App) {
    if window.has_active_dialog(cx) {
        return; // Asked again next time the message is opened.
    }
    let to = Rc::new(to);
    window.open_dialog(cx, move |dialog, _, cx| {
        let t = theme(cx).clone();
        let choice = |label: &'static str, policy: Option<ReceiptPolicy>, send: bool| {
            let (state, to) = (state.clone(), to.clone());
            Button::new(label)
                .label(tr(cx, label))
                .on_click(move |_, window, cx| {
                    if let Some(policy) = policy {
                        set_policy(&state, policy, cx);
                    }
                    resolve(&state, id, send.then(|| (*to).clone()), cx);
                    window.close_dialog(cx);
                })
        };
        dialog
            .w(DIALOG_MD)
            .title(tr(cx, "Read receipt requested"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(SPACE_3)
                    .child(
                        div().text_size(TEXT_SM).text_color(t.text_muted).child(
                            tr(cx, "{} asked to be told when you read this message.")
                                .replace("{}", &to.email),
                        ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .justify_end()
                            .gap(SPACE_2)
                            .child(choice("Never send", Some(ReceiptPolicy::Never), false))
                            .child(choice("Always send", Some(ReceiptPolicy::Always), true))
                            .child(choice("Ignore", None, false))
                            .child(
                                choice("Send receipt", None, true)
                                    .primary()
                                    .icon(IconName::MailCheck),
                            ),
                    ),
            )
    });
}

/// Palette / settings entry: choose the policy.
pub fn open_policy(state: Entity<AppState>, window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, move |dialog, _, cx| {
        let current = load(state.read(cx)).policy;
        let option = |label: &'static str, policy: ReceiptPolicy| {
            let state = state.clone();
            let button = Button::new(label)
                .label(tr(cx, label))
                .on_click(move |_, window, cx| {
                    set_policy(&state, policy, cx);
                    window.close_dialog(cx);
                });
            if policy == current {
                button.primary().icon(IconName::Check)
            } else {
                button
            }
        };
        dialog.w(DIALOG_SM).title(tr(cx, "Read receipts")).child(
            div()
                .flex()
                .flex_col()
                .gap(SPACE_2)
                .child(
                    div()
                        .text_size(TEXT_SM)
                        .child(tr(cx, "When a message you open asks for a read receipt:")),
                )
                .child(option("Ask me every time", ReceiptPolicy::Ask))
                .child(option("Always send a receipt", ReceiptPolicy::Always))
                .child(option("Never send a receipt", ReceiptPolicy::Never)),
        )
    });
}
