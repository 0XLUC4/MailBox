//! Signatures dialog: per account, one signature for new messages and one for replies and
//! forwards (compose markup, see `mailbox_core::compose::markup`). Compose inserts them.

use gpui::component::WindowExt as _;
use gpui::component::input::{Textarea, TextareaState};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::compose::{self, Signature};
use mailbox_core::{Account, Command};

use crate::app_state::AppState;
use crate::components::{Button, IconName};
use crate::dialogs::field;
use crate::settings::tr;
use crate::theme::{DIALOG_LG, SPACE_2, SPACE_3, TEXT_XS, theme};

/// Opens the dialog on the current account (the selected folder's, else the first one).
pub fn open(state: Entity<AppState>, window: &mut Window, cx: &mut App) {
    let accounts = state.read(cx).store().list_accounts().unwrap_or_default();
    if accounts.is_empty() {
        return state.update(cx, |state, cx| {
            state.notify_info(tr(cx, "Add an account first"), cx)
        });
    }
    let current = crate::actions::current_account(&state, cx);
    let index = accounts
        .iter()
        .position(|account| Some(account.id) == current)
        .unwrap_or(0);
    let form = cx.new(|cx| SignatureForm::new(state, accounts, index, window, cx));
    window.open_dialog(cx, move |dialog, _, cx| {
        dialog
            .w(DIALOG_LG)
            .title(tr(cx, "Signatures"))
            .child(form.clone())
    });
}

struct SignatureForm {
    state: Entity<AppState>,
    accounts: Vec<Account>,
    index: usize,
    new_message: Entity<TextareaState>,
    reply: Entity<TextareaState>,
}

impl SignatureForm {
    fn new(
        state: Entity<AppState>,
        accounts: Vec<Account>,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let textarea = |placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(3, 8)
                    .placeholder(tr(cx, placeholder))
            })
        };
        let mut form = Self {
            new_message: textarea("-- \nYour name", window, cx),
            reply: textarea("Optional", window, cx),
            state,
            accounts,
            index,
        };
        form.load(window, cx);
        form
    }

    fn account(&self) -> Option<&Account> {
        self.accounts.get(self.index)
    }

    /// Fills the fields with the stored signature of the current account.
    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let stored = self
            .account()
            .and_then(|account| compose::signature(self.state.read(cx).store(), account.id).ok())
            .flatten()
            .unwrap_or_default();
        self.new_message.update(cx, |input, cx| {
            input.set_value(stored.new_message.clone(), window, cx)
        });
        self.reply.update(cx, |input, cx| {
            input.set_value(stored.reply.clone(), window, cx)
        });
    }

    /// Keeps the edits of the current account, then shows another one.
    fn switch(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index == self.index || !self.store(cx) {
            return;
        }
        self.index = index;
        self.load(window, cx);
        cx.notify();
    }

    /// Saves the fields of the current account when they changed; false when saving failed.
    fn store(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(account_id) = self.account().map(|account| account.id) else {
            return false;
        };
        let new_message = self.new_message.read(cx).value().trim_end().to_owned();
        let reply = self.reply.read(cx).value().trim_end().to_owned();
        let stored = compose::signature(self.state.read(cx).store(), account_id)
            .ok()
            .flatten()
            .unwrap_or_default();
        if stored.new_message == new_message && stored.reply == reply {
            return true;
        }
        let signature = Signature {
            id: 0,
            account_id,
            new_message,
            reply,
        };
        self.state.update(cx, |state, cx| {
            state.run(Command::SaveSignature(Box::new(signature)), cx)
        })
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.store(cx) {
            self.state.update(cx, |state, cx| {
                state.notify_info(tr(cx, "Signature saved"), cx)
            });
            window.close_dialog(cx);
        }
    }
}

impl Render for SignatureForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let tabs = (self.accounts.len() > 1).then(|| {
            div().flex().flex_row().flex_wrap().gap(SPACE_2).children(
                self.accounts.iter().enumerate().map(|(index, account)| {
                    let button = Button::new(("signature-account", index))
                        .label(account.name.clone())
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.switch(index, window, cx)),
                        );
                    if index == self.index {
                        button.primary()
                    } else {
                        button
                    }
                }),
            )
        });
        div()
            .flex()
            .flex_col()
            .gap(SPACE_3)
            .when_some(tabs, |el, tabs| el.child(tabs))
            .child(div().text_size(TEXT_XS).text_color(t.text_subtle).child(tr(
                cx,
                "Added at the end of the message. Formatting: **bold**, *italic*, \
                     [text](https://link).",
            )))
            .child(field(
                tr(cx, "New messages"),
                Textarea::new(&self.new_message),
                cx,
            ))
            .child(field(
                tr(cx, "Replies and forwards"),
                Textarea::new(&self.reply),
                cx,
            ))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(SPACE_2)
                    .child(
                        Button::new("signature-cancel")
                            .label(tr(cx, "Cancel"))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("signature-save")
                            .primary()
                            .icon(IconName::Check)
                            .label(tr(cx, "Save"))
                            .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                    ),
            )
    }
}
