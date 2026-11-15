//! Meeting invitations by email (iMIP, RFC 6047).
//!
//! - Sending: the event editor calls `dav_bridge::send_invitation`, which queues a
//!   `METHOD:REQUEST` .ics mail in the Outbox through [`queue_calendar_mail`].
//! - Receiving (hook for the mail reading pane): [`invitation_bar`] renders Accept /
//!   Tentative / Decline for a message carrying a `METHOD:REQUEST` .ics ("Update calendar"
//!   for a REPLY, "Remove from calendar" for a CANCEL); `None` for other messages.
//!   [`respond_to_invitation`] does the same without UI. Answering imports the event into
//!   the account's calendar with the answer (one undoable step) and queues a
//!   `METHOD:REPLY` to the organizer.

use chrono::Local;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::calendar::{
    self as cal, CalendarCommand, EventDetails, EventSnapshot, Method, ShowAs,
};
use mailbox_core::{
    Account, Address, Attachment, Calendar, CalendarEvent, Command, Id, Importance, Message,
    MessageFlags, ResponseStatus, Store, command, ics,
};

use crate::app_state::AppState;
use crate::components::{Button, ButtonVariant, Icon, IconName};
use crate::settings::tr;
use crate::sync_bridge;
use crate::theme::{ICON_SIZE, RADIUS_LG, SPACE_1, SPACE_2, SPACE_3, TEXT_MD, TEXT_SM, theme};

/// A calendar file found in a message.
pub struct IncomingInvitation {
    pub method: Method,
    pub events: Vec<EventSnapshot>,
}

impl IncomingInvitation {
    /// The series (or single) event the invitation is about.
    fn main(&self) -> Option<&EventSnapshot> {
        self.events
            .iter()
            .find(|s| s.details.as_ref().is_none_or(|d| d.recurrence_id.is_none()))
            .or(self.events.first())
    }
}

fn is_calendar_attachment(attachment: &Attachment) -> bool {
    attachment
        .mime
        .to_ascii_lowercase()
        .starts_with("text/calendar")
        || attachment.name.to_ascii_lowercase().ends_with(".ics")
}

/// The iTIP content of `message` (its first .ics attachment), if any.
pub fn incoming_invitation(message: &Message) -> Option<IncomingInvitation> {
    message
        .attachments
        .iter()
        .filter(|attachment| is_calendar_attachment(attachment))
        .find_map(|attachment| {
            let path = sync_bridge::attachment_path(message.id, &attachment.name);
            let text = std::fs::read_to_string(path).ok()?;
            let parsed = ics::parse(&text).ok()?;
            let method = parsed.method?;
            (!parsed.events.is_empty()).then_some(IncomingInvitation {
                method,
                events: parsed.events,
            })
        })
}

/// The account that sends mail for a calendar: its own account, else the first one
/// (local calendars have no account).
pub fn sending_account(store: &Store, account_id: Id) -> Option<Account> {
    let accounts = store.list_accounts().ok()?;
    accounts
        .iter()
        .find(|account| account.id == account_id)
        .or(accounts.first())
        .cloned()
}

/// Puts a mail with a calendar attachment in the account's Outbox; the mail sync sends it.
pub fn queue_calendar_mail(
    state: &Entity<AppState>,
    account: &Account,
    to: Vec<Address>,
    subject: String,
    body: String,
    attachment: (String, String, String),
    cx: &mut App,
) -> Result<(), String> {
    let (name, mime, content) = attachment;
    if to.is_empty() {
        return Err(tr(cx, "Nobody to send the invitation to").into());
    }
    let outbox = sync_bridge::outbox_folder(account.id, cx)
        .ok_or(tr(cx, "The mail Outbox is not available"))?;
    let message = Message {
        id: 0,
        account_id: account.id,
        folder_id: outbox,
        thread_id: cal::new_uid(),
        from: Address::new(account.name.clone(), account.email.clone()),
        to,
        cc: Vec::new(),
        subject,
        date: chrono::Utc::now(),
        // One line in the message list.
        preview: body
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(120)
            .collect(),
        body_text: body,
        body_html: None,
        flags: MessageFlags {
            read: true,
            ..MessageFlags::default()
        },
        importance: Importance::Normal,
        categories: Vec::new(),
        attachments: vec![Attachment {
            name: name.clone(),
            mime,
            size: content.len() as u64,
        }],
    };
    let store = state.read(cx).store();
    let inverse = command::apply(store, Command::SaveMessage(Box::new(message)))
        .map_err(|err| err.to_string())?;
    let Command::DeleteMessages { ids } = inverse else {
        return Err(tr(cx, "Cannot queue the message").into());
    };
    let id = ids
        .first()
        .copied()
        .ok_or(tr(cx, "Cannot queue the message"))?;
    // Written before the mail bridge picks the message up (same thread).
    let path = sync_bridge::attachment_path(id, &name);
    let written = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&path, content));
    if let Err(err) = written {
        let _ = command::apply(store, Command::DeleteMessages { ids });
        return Err(tr(cx, "Cannot attach the calendar file: {}").replace("{}", &err.to_string()));
    }
    state.update(cx, |_, cx| cx.notify());
    Ok(())
}

/// One line describing when an event happens, in local time.
pub(crate) fn when_text(snapshot: &EventSnapshot, cx: &App) -> String {
    let start = snapshot.event.start.with_timezone(&Local);
    if snapshot.event.all_day {
        format!("{} ({})", start.format("%A %e %B %Y"), tr(cx, "All day"))
    } else {
        let end = snapshot.event.end.with_timezone(&Local);
        format!(
            "{} – {}",
            start.format("%A %e %B %Y, %H:%M"),
            end.format("%H:%M")
        )
    }
}

fn status_label(status: ResponseStatus, cx: &App) -> &'static str {
    tr(
        cx,
        match status {
            ResponseStatus::Accepted => "accepted",
            ResponseStatus::Tentative => "tentative",
            ResponseStatus::Declined => "declined",
            ResponseStatus::NeedsAction => "no answer yet",
        },
    )
}

/// Calendar that receives an invitation sent to `account_id`.
fn target_calendar(store: &Store, account_id: Id) -> Option<Calendar> {
    let calendars = store.list_calendars().ok()?;
    calendars
        .iter()
        .find(|calendar| calendar.account_id == account_id)
        .or(calendars.first())
        .cloned()
}

/// My current answer to the invitation, once it is in the calendar.
fn current_answer(
    store: &Store,
    invitation: &IncomingInvitation,
    email: &str,
) -> Option<ResponseStatus> {
    let uid = invitation.main()?.details.as_ref()?.uid.clone();
    let row = cal::details_by_uid(store, &uid)
        .ok()?
        .into_iter()
        .find(|d| d.recurrence_id.is_none())?;
    let event = store
        .get::<mailbox_core::CalendarEvent>(row.event_id)
        .ok()??;
    event
        .attendees
        .iter()
        .find(|a| a.address.email.eq_ignore_ascii_case(email))
        .map(|a| a.status)
}

/// Answers the invitation in `message_id`: adds it to the calendar with the answer and
/// queues the reply to the organizer. Returns `false` (with a toast) on failure.
pub fn respond_to_invitation(
    state: &Entity<AppState>,
    message_id: Id,
    status: ResponseStatus,
    cx: &mut App,
) -> bool {
    let result = (|| {
        let store = state.read(cx).store();
        let message = store
            .get::<Message>(message_id)
            .ok()
            .flatten()
            .ok_or(tr(cx, "The message is gone"))?;
        let invitation =
            incoming_invitation(&message).ok_or(tr(cx, "No invitation in this message"))?;
        let account =
            sending_account(store, message.account_id).ok_or(tr(cx, "No mail account"))?;
        let calendar = target_calendar(store, message.account_id).ok_or(tr(cx, "No calendar"))?;
        let mut events = invitation.events.clone();
        for snapshot in &mut events {
            let attendee = snapshot
                .event
                .attendees
                .iter_mut()
                .find(|a| a.address.email.eq_ignore_ascii_case(&account.email));
            match attendee {
                Some(attendee) => attendee.status = status,
                None => snapshot.event.attendees.push(mailbox_core::Attendee {
                    address: Address::new(account.name.clone(), account.email.clone()),
                    status,
                    optional: false,
                }),
            }
            if let Some(details) = snapshot.details.as_mut() {
                details.show_as = match status {
                    ResponseStatus::Tentative => ShowAs::Tentative,
                    ResponseStatus::Declined => ShowAs::Free,
                    _ => details.show_as,
                };
            }
        }
        let main = invitation.main().ok_or(tr(cx, "Empty invitation"))?.clone();
        let answered = events
            .iter()
            .find(|s| s.details.as_ref().map(|d| &d.uid) == main.details.as_ref().map(|d| &d.uid))
            .cloned()
            .unwrap_or(main);
        let import = CalendarCommand::Import {
            calendar_id: calendar.id,
            method: Some(Method::Request),
            events,
        };
        Ok::<_, String>((account, answered, import))
    })();
    let (account, answered, import) = match result {
        Ok(parts) => parts,
        Err(err) => {
            state.update(cx, |state, cx| state.report_error(err, cx));
            return false;
        }
    };
    if !state.update(cx, |state, cx| state.run(import.into_command(), cx)) {
        return false;
    }
    let details = answered
        .details
        .clone()
        .unwrap_or_else(|| EventDetails::default_for(0));
    let verb = reply_verb(status).0;
    let queued = queue_reply(state, &account, &answered.event, &details, status, cx);
    state.update(cx, |state, cx| match queued {
        Ok(()) => state.notify_info(
            format!(
                "{} - {}",
                tr(cx, verb),
                tr(cx, "reply queued in the Outbox")
            ),
            cx,
        ),
        // The answer is saved; only the reply could not be queued.
        Err(err) => state.report_error(
            tr(cx, "Answer saved, reply not sent: {}").replace("{}", &err),
            cx,
        ),
    });
    true
}

fn reply_verb(status: ResponseStatus) -> (&'static str, &'static str) {
    match status {
        ResponseStatus::Accepted => ("Accepted", "has accepted"),
        ResponseStatus::Tentative => ("Tentative", "has tentatively accepted"),
        ResponseStatus::Declined => ("Declined", "has declined"),
        ResponseStatus::NeedsAction => ("Reply", "has not answered"),
    }
}

/// Queues an iTIP REPLY (my answer) to the organizer of `event`.
pub(crate) fn queue_reply(
    state: &Entity<AppState>,
    account: &Account,
    event: &CalendarEvent,
    details: &EventDetails,
    status: ResponseStatus,
    cx: &mut App,
) -> Result<(), String> {
    let organizer: Vec<Address> = details.organizer.clone().into_iter().collect();
    if organizer.is_empty() {
        return Err(tr(cx, "The invitation has no organizer").into());
    }
    let (verb, phrase) = reply_verb(status);
    queue_calendar_mail(
        state,
        account,
        organizer,
        format!("{verb}: {}", event.title),
        format!("{} {phrase} this invitation.", account.name),
        (
            "reply.ics".into(),
            "text/calendar; method=REPLY; charset=utf-8".into(),
            ics::reply(event, details, &account.email, status),
        ),
        cx,
    )
}

/// Queues an iTIP CANCEL of a meeting I organize to its attendees.
pub(crate) fn queue_cancel(
    state: &Entity<AppState>,
    account: &Account,
    event: &CalendarEvent,
    details: &EventDetails,
    cx: &mut App,
) -> Result<(), String> {
    let to: Vec<Address> = event
        .attendees
        .iter()
        .map(|attendee| attendee.address.clone())
        .filter(|address| !address.email.eq_ignore_ascii_case(&account.email))
        .collect();
    queue_calendar_mail(
        state,
        account,
        to,
        format!("Canceled: {}", event.title),
        format!("{} has canceled this meeting.", account.name),
        (
            "cancel.ics".into(),
            "text/calendar; method=CANCEL; charset=utf-8".into(),
            ics::export(&[(event.clone(), details.clone())], Some(Method::Cancel)),
        ),
        cx,
    )
}

/// Applies an incoming REPLY or CANCEL to the calendar.
fn apply_update(state: &Entity<AppState>, message_id: Id, cx: &mut App) {
    let store = state.read(cx).store();
    let Some(message) = store.get::<Message>(message_id).ok().flatten() else {
        return;
    };
    let Some(invitation) = incoming_invitation(&message) else {
        return;
    };
    let Some(calendar) = target_calendar(store, message.account_id) else {
        return;
    };
    let command = CalendarCommand::Import {
        calendar_id: calendar.id,
        method: Some(invitation.method),
        events: invitation.events,
    };
    state.update(cx, |state, cx| {
        if state.run(command.into_command(), cx) {
            state.notify_info(tr(cx, "Calendar updated"), cx);
        }
    });
}

/// Invitation banner for the reading pane, or `None` when `message_id` carries no
/// calendar invitation. Call it from the message view's render.
pub fn invitation_bar(
    state: &Entity<AppState>,
    message_id: Id,
    cx: &mut App,
) -> Option<AnyElement> {
    let t = theme(cx).clone();
    let store = state.read(cx).store();
    let message = store.get::<Message>(message_id).ok().flatten()?;
    let my_email = sending_account(store, message.account_id)
        .map(|account| account.email)
        .unwrap_or_default();
    // Invitations and replies the user sent need no answer.
    if message.from.email.eq_ignore_ascii_case(&my_email) {
        return None;
    }
    let invitation = incoming_invitation(&message)?;
    let main = invitation.main()?.clone();
    let answer = current_answer(store, &invitation, &my_email);
    let (icon, heading) = match invitation.method {
        Method::Request => (IconName::CalendarPlus, "Meeting invitation"),
        Method::Reply => (IconName::CalendarCheck, "Meeting response"),
        Method::Cancel => (IconName::CalendarX, "Meeting canceled"),
        _ => (IconName::Calendar, "Calendar"),
    };
    let heading = tr(cx, heading);
    let organizer = main
        .details
        .as_ref()
        .and_then(|d| d.organizer.clone())
        .map(|o| o.display().to_owned())
        .unwrap_or_else(|| message.from.display().to_owned());
    let answer_button = |id: &'static str, label: &'static str, status: ResponseStatus| {
        let state = state.clone();
        Button::new(id)
            .label(tr(cx, label))
            .when(answer == Some(status), |button| button.primary())
            .on_click(move |_, _, cx| {
                respond_to_invitation(&state, message_id, status, cx);
            })
    };
    let actions: AnyElement = match invitation.method {
        Method::Request => div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(SPACE_2)
            .child(answer_button(
                "invite-accept",
                "Accept",
                ResponseStatus::Accepted,
            ))
            .child(answer_button(
                "invite-tentative",
                "Tentative",
                ResponseStatus::Tentative,
            ))
            .child(
                answer_button("invite-decline", "Decline", ResponseStatus::Declined)
                    .when(answer != Some(ResponseStatus::Declined), |b| {
                        b.variant(ButtonVariant::Ghost)
                    }),
            )
            .into_any_element(),
        Method::Reply | Method::Cancel => {
            let state = state.clone();
            let button = Button::new("invite-apply")
                .label(tr(
                    cx,
                    if invitation.method == Method::Cancel {
                        "Remove from calendar"
                    } else {
                        "Update calendar"
                    },
                ))
                .on_click(move |_, _, cx| apply_update(&state, message_id, cx));
            div().flex().flex_row().child(button).into_any_element()
        }
        _ => div().into_any_element(),
    };
    let reply_line = (invitation.method == Method::Reply).then(|| {
        main.event
            .attendees
            .iter()
            .map(|a| format!("{} — {}", a.address.display(), status_label(a.status, cx)))
            .collect::<Vec<_>>()
            .join(", ")
    });
    Some(
        div()
            .id("invitation-bar")
            .flex()
            .flex_col()
            .gap(SPACE_2)
            .p(SPACE_3)
            .rounded(RADIUS_LG)
            .border_1()
            .border_color(t.border)
            .bg(t.surface)
            .text_color(t.text)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(SPACE_2)
                    .child(Icon::new(icon).size(ICON_SIZE).text_color(t.accent))
                    .child(
                        div()
                            .text_size(TEXT_SM)
                            .text_color(t.text_muted)
                            .child(heading),
                    ),
            )
            .child(
                div()
                    .min_w(px(0.))
                    .truncate()
                    .text_size(TEXT_MD)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(main.event.title.clone()),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(SPACE_1)
                    .text_size(TEXT_SM)
                    .text_color(t.text_muted)
                    .child(when_text(&main, cx))
                    .when(!main.event.location.is_empty(), |el| {
                        el.child(main.event.location.clone())
                    })
                    .child(
                        div()
                            .min_w(px(0.))
                            .truncate()
                            .child(tr(cx, "Organizer: {}").replace("{}", &organizer)),
                    )
                    .when_some(reply_line, |el, line| el.child(line))
                    .when_some(answer, |el, answer| {
                        el.child(tr(cx, "Your answer: {}").replace("{}", status_label(answer, cx)))
                    }),
            )
            .child(actions)
            .into_any_element(),
    )
}
