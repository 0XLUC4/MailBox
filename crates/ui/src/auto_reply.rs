//! Automatic replies (out of office): the settings dialog and the new-mail hook. Rules live in
//! `mailbox_core::auto_reply`; replies go through the normal send path (Drafts → Outbox).

use chrono::{Local, NaiveDate, Utc};
use gpui::component::WindowExt as _;
use gpui::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui::component::switch::Switch;
use gpui::*;
use mailbox_core::auto_reply::AutoReply;
use mailbox_core::compose::{self, Draft};
use mailbox_core::{Account, Command, FolderRole, Id, Message};

use crate::app_state::AppState;
use crate::components::{Button, IconName};
use crate::dialogs::field;
use crate::settings::tr;
use crate::theme::{DIALOG_MD, SPACE_2, SPACE_3, TEXT_SM, TEXT_XS, theme};

/// Saves `draft` in Drafts (untracked) and queues it for sending. Used for automatic replies
/// and read receipts.
pub fn queue_plain_draft(state: &Entity<AppState>, draft: Draft, cx: &mut App) {
    let key = draft.meta.key.clone();
    let saved = state.update(cx, |state, cx| {
        state.run_untracked(Command::SaveDraft(Box::new(draft)), cx)
    });
    if !saved {
        return;
    }
    let id = compose::draft_by_key(state.read(cx).store(), &key)
        .ok()
        .flatten()
        .map(|meta| meta.message_id);
    if let Some(id) = id {
        crate::compose::queue_draft(state, id, cx);
    }
}

/// The account of `message` and its Drafts folder.
pub fn account_and_drafts(state: &AppState, account_id: Id) -> Option<(Account, Id)> {
    let store = state.store();
    let account = store.get::<Account>(account_id).ok().flatten()?;
    let drafts = store
        .folder_by_role(account_id, FolderRole::Drafts)
        .ok()
        .flatten()?;
    Some((account, drafts.id))
}

/// New-mail hook: one automatic reply per sender while the rule is active.
pub fn process(state: &Entity<AppState>, ids: &[Id], cx: &mut App) {
    let mut rule = match AutoReply::load(state.read(cx).store()) {
        Ok(rule) => rule,
        Err(_) => return,
    };
    let today = Local::now().date_naive();
    if !rule.is_active(today) {
        return;
    }
    let (own, messages): (Vec<String>, Vec<Message>) = {
        let store = state.read(cx).store();
        let own = store
            .list_accounts()
            .unwrap_or_default()
            .into_iter()
            .map(|account| account.email)
            .collect();
        let messages = ids
            .iter()
            .filter_map(|id| store.get::<Message>(*id).ok().flatten())
            .collect();
        (own, messages)
    };
    let mut drafts = Vec::new();
    for message in &messages {
        if !rule.should_reply(message, &own, today) {
            continue;
        }
        let Some((account, drafts_folder)) = account_and_drafts(state.read(cx), message.account_id)
        else {
            continue;
        };
        rule.mark_replied(&message.from.email);
        drafts.push(rule.reply_draft(&account, message, drafts_folder, Utc::now()));
    }
    if drafts.is_empty() {
        return;
    }
    // Remember who was answered before sending, so a crash never answers twice.
    let saved = match rule.command() {
        Ok(command) => state.update(cx, |state, cx| state.run_untracked(command, cx)),
        Err(_) => false,
    };
    if saved {
        for draft in drafts {
            queue_plain_draft(state, draft, cx);
        }
    }
}

/// The "Automatic replies" dialog.
pub fn open(state: Entity<AppState>, window: &mut Window, cx: &mut App) {
    let form = cx.new(|cx| AutoReplyForm::new(state, window, cx));
    window.open_dialog(cx, move |dialog, _, cx| {
        dialog
            .w(DIALOG_MD)
            .title(tr(cx, "Automatic replies"))
            .child(form.clone())
    });
}

struct AutoReplyForm {
    state: Entity<AppState>,
    rule: AutoReply,
    start: Entity<InputState>,
    end: Entity<InputState>,
    subject: Entity<InputState>,
    body: Entity<TextareaState>,
}

fn date_text(date: Option<NaiveDate>) -> String {
    date.map(|date| date.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// Empty → `None`; otherwise a `YYYY-MM-DD` date.
fn parse_date(text: &str) -> Result<Option<NaiveDate>, chrono::ParseError> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d").map(Some)
}

impl AutoReplyForm {
    fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let rule = AutoReply::load(state.read(cx).store()).unwrap_or_default();
        let input = |placeholder: &'static str,
                     value: String,
                     window: &mut Window,
                     cx: &mut Context<Self>| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(tr(cx, placeholder))
                    .default_value(value)
            })
        };
        let start = input("YYYY-MM-DD (optional)", date_text(rule.start), window, cx);
        let end = input("YYYY-MM-DD (optional)", date_text(rule.end), window, cx);
        let subject = input("Automatic reply", rule.subject.clone(), window, cx);
        // Re-render on date edits so Save follows their validity.
        for date in [&start, &end] {
            cx.subscribe(date, |_, _, _: &InputEvent, cx| cx.notify())
                .detach();
        }
        let body = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(4, 10)
                .placeholder(tr(cx, "Your message"))
                .default_value(rule.body.clone())
        });
        Self {
            state,
            rule,
            start,
            end,
            subject,
            body,
        }
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let invalid = |what: &'static str| {
            tr(cx, "{}: use the format YYYY-MM-DD").replace("{}", tr(cx, what))
        };
        let start = parse_date(&self.start.read(cx).value()).map_err(|_| invalid("Start date"));
        let end = parse_date(&self.end.read(cx).value()).map_err(|_| invalid("End date"));
        let dates = start.and_then(|start| Ok((start, end?)));
        let body = self.body.read(cx).value().to_string();
        let checked = dates.and_then(|(start, end)| {
            if let (Some(start), Some(end)) = (start, end)
                && end < start
            {
                return Err(tr(cx, "The end date is before the start date").to_owned());
            }
            if self.rule.enabled && body.trim().is_empty() {
                return Err(tr(cx, "Write the message to send").to_owned());
            }
            Ok((start, end))
        });
        let (start, end) = match checked {
            Ok(dates) => dates,
            Err(err) => {
                return self
                    .state
                    .update(cx, |state, cx| state.report_error(err, cx));
            }
        };
        let mut rule = AutoReply {
            enabled: self.rule.enabled,
            start,
            end,
            subject: self.subject.read(cx).value().trim().to_owned(),
            body,
            replied: self.rule.replied.clone(),
        };
        // A new period (or switching on again) answers everyone once more.
        let stored = AutoReply::load(self.state.read(cx).store()).unwrap_or_default();
        if rule.start != stored.start || rule.end != stored.end || (rule.enabled && !stored.enabled)
        {
            rule.replied.clear();
        }
        let saved = match rule.command() {
            Ok(command) => self.state.update(cx, |state, cx| state.run(command, cx)),
            Err(err) => {
                self.state
                    .update(cx, |state, cx| state.report_error(err, cx));
                false
            }
        };
        if saved {
            let message = tr(
                cx,
                if rule.enabled {
                    "Automatic replies are on"
                } else {
                    "Automatic replies are off"
                },
            );
            self.state
                .update(cx, |state, cx| state.notify_info(message, cx));
            window.close_dialog(cx);
        }
    }
}

impl Render for AutoReplyForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let dates_valid = parse_date(&self.start.read(cx).value()).is_ok()
            && parse_date(&self.end.read(cx).value()).is_ok();
        div()
            .flex()
            .flex_col()
            .gap(SPACE_3)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(SPACE_2)
                    .child(
                        Switch::new("auto-reply-enabled")
                            .checked(self.rule.enabled)
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.rule.enabled = *checked;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .text_size(TEXT_SM)
                            .child(tr(cx, "Send automatic replies")),
                    ),
            )
            .child(div().text_size(TEXT_XS).text_color(t.text_subtle).child(tr(
                cx,
                "Each sender gets one reply per period. Mailing lists and no-reply \
                         senders are skipped. Replies are sent by Mailbox, so it must be running.",
            )))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(SPACE_2)
                    .child(div().flex_1().child(field(
                        tr(cx, "Start date"),
                        Input::new(&self.start),
                        cx,
                    )))
                    .child(div().flex_1().child(field(
                        tr(cx, "End date"),
                        Input::new(&self.end),
                        cx,
                    ))),
            )
            .child(field(
                tr(cx, "Subject prefix"),
                Input::new(&self.subject),
                cx,
            ))
            .child(field(tr(cx, "Message"), Textarea::new(&self.body), cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(SPACE_2)
                    .child(
                        Button::new("auto-reply-cancel")
                            .label(tr(cx, "Cancel"))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("auto-reply-save")
                            .primary()
                            .icon(IconName::Check)
                            .label(tr(cx, "Save"))
                            .disabled(!dates_valid)
                            .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::parse_date;

    #[test]
    fn parses_optional_dates() {
        assert_eq!(parse_date("  "), Ok(None));
        assert_eq!(
            parse_date("2026-07-01"),
            Ok(Some("2026-07-01".parse().unwrap()))
        );
        assert!(parse_date("01/07/2026").is_err());
    }
}
