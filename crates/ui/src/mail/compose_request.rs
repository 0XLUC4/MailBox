//! Hook from mail to the compose module. Reply / Reply all / Forward / mailto links build a
//! [`ComposeRequest`] and hand it to the handler the compose module registered with
//! [`set_compose_handler`] (mail never depends on compose's internals).

use std::rc::Rc;

use chrono::Local;
use gpui::{App, Global, Window};
use mailbox_core::{Address, Attachment, Id, Message};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposeKind {
    New,
    Reply,
    ReplyAll,
    Forward,
}

/// Everything compose needs to pre-fill a draft.
#[derive(Debug, Clone, PartialEq)]
pub struct ComposeRequest {
    pub kind: ComposeKind,
    /// Account to send from (the source message's account).
    pub account_id: Option<Id>,
    /// Message replied to or forwarded.
    pub source_id: Option<Id>,
    /// Keep replies in the same conversation.
    pub thread_id: Option<String>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub subject: String,
    /// Original message as plain text with a header block, to place under the reply.
    pub quoted_body: String,
    /// Attachments to carry over (forward only).
    pub attachments: Vec<Attachment>,
}

fn prefixed(prefix: &str, subject: &str) -> String {
    let lower = subject.trim().to_ascii_lowercase();
    if lower.starts_with(&prefix.to_ascii_lowercase()) {
        subject.trim().to_owned()
    } else {
        format!("{prefix} {}", subject.trim())
    }
}

fn join(addresses: &[Address]) -> String {
    addresses
        .iter()
        .map(|a| {
            if a.name.is_empty() {
                a.email.clone()
            } else {
                format!("{} <{}>", a.name, a.email)
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn quote(message: &Message) -> String {
    let mut header = format!(
        "\n\n-------- Original message --------\nFrom: {}\nSent: {}\nTo: {}\n",
        join(std::slice::from_ref(&message.from)),
        message
            .date
            .with_timezone(&Local)
            .format("%A %d %B %Y %H:%M"),
        join(&message.to),
    );
    if !message.cc.is_empty() {
        header.push_str(&format!("Cc: {}\n", join(&message.cc)));
    }
    header.push_str(&format!("Subject: {}\n\n", message.subject));
    header.push_str(&message.body_text);
    header
}

/// Adds addresses not already present (case-insensitive) and not `me`.
fn push_unique(list: &mut Vec<Address>, candidates: &[Address], me: &str) {
    for candidate in candidates {
        let email = candidate.email.to_lowercase();
        if email.is_empty()
            || email == me.to_lowercase()
            || list.iter().any(|a| a.email.to_lowercase() == email)
        {
            continue;
        }
        list.push(candidate.clone());
    }
}

impl ComposeRequest {
    /// Blank message to someone (mailto links, contact cards).
    pub fn new_to(to: Address) -> Self {
        Self {
            kind: ComposeKind::New,
            account_id: None,
            source_id: None,
            thread_id: None,
            to: vec![to],
            cc: Vec::new(),
            subject: String::new(),
            quoted_body: String::new(),
            attachments: Vec::new(),
        }
    }

    /// Reply (or reply all) to `message`; `me` is the account's address, left out of recipients.
    pub fn reply(message: &Message, me: &str, all: bool) -> Self {
        let mut to = Vec::new();
        let mut cc = Vec::new();
        // Replying to our own sent message goes back to its recipients.
        if message.from.email.eq_ignore_ascii_case(me) {
            push_unique(&mut to, &message.to, me);
        } else {
            push_unique(&mut to, std::slice::from_ref(&message.from), "");
        }
        if all {
            push_unique(&mut to, &message.to, me);
            push_unique(&mut cc, &message.cc, me);
            cc.retain(|c| !to.iter().any(|t| t.email.eq_ignore_ascii_case(&c.email)));
        }
        Self {
            kind: if all {
                ComposeKind::ReplyAll
            } else {
                ComposeKind::Reply
            },
            account_id: Some(message.account_id),
            source_id: Some(message.id),
            thread_id: Some(message.thread_id.clone()),
            to,
            cc,
            subject: prefixed("Re:", &message.subject),
            quoted_body: quote(message),
            attachments: Vec::new(),
        }
    }

    pub fn forward(message: &Message) -> Self {
        Self {
            kind: ComposeKind::Forward,
            account_id: Some(message.account_id),
            source_id: Some(message.id),
            thread_id: Some(message.thread_id.clone()),
            to: Vec::new(),
            cc: Vec::new(),
            subject: prefixed("Fw:", &message.subject),
            quoted_body: quote(message),
            attachments: message.attachments.clone(),
        }
    }
}

type Handler = Rc<dyn Fn(ComposeRequest, &mut Window, &mut App)>;

struct ComposeHandler(Handler);

impl Global for ComposeHandler {}

/// Registers the compose module's entry point (call once at startup).
pub fn set_compose_handler(
    cx: &mut App,
    handler: impl Fn(ComposeRequest, &mut Window, &mut App) + 'static,
) {
    cx.set_global(ComposeHandler(Rc::new(handler)));
}

/// Hands the request to compose. Returns `false` when no handler is registered yet.
pub fn request_compose(request: ComposeRequest, window: &mut Window, cx: &mut App) -> bool {
    let Some(handler) = cx
        .try_global::<ComposeHandler>()
        .map(|global| global.0.clone())
    else {
        return false;
    };
    handler(request, window, cx);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use mailbox_core::{Importance, MessageFlags};

    fn message() -> Message {
        Message {
            id: 7,
            account_id: 1,
            folder_id: 1,
            thread_id: "t".into(),
            from: Address::new("Ada", "ada@example.com"),
            to: vec![
                Address::new("Me", "me@example.com"),
                Address::new("Bob", "bob@example.com"),
            ],
            cc: vec![
                Address::new("Cy", "cy@example.com"),
                Address::new("Bob again", "BOB@example.com"),
            ],
            subject: "Budget".into(),
            date: Utc::now(),
            preview: String::new(),
            body_text: "Numbers attached.".into(),
            body_html: None,
            flags: MessageFlags::default(),
            importance: Importance::Normal,
            categories: vec![],
            attachments: vec![],
        }
    }

    #[test]
    fn reply_all_dedups_and_skips_me() {
        let request = ComposeRequest::reply(&message(), "me@example.com", true);
        let emails = |list: &[Address]| list.iter().map(|a| a.email.clone()).collect::<Vec<_>>();
        assert_eq!(
            emails(&request.to),
            vec!["ada@example.com", "bob@example.com"]
        );
        assert_eq!(emails(&request.cc), vec!["cy@example.com"]);
        assert_eq!(request.subject, "Re: Budget");
        assert!(request.quoted_body.contains("Numbers attached."));

        let reply = ComposeRequest::reply(&message(), "me@example.com", false);
        assert_eq!(emails(&reply.to), vec!["ada@example.com"]);
        assert!(reply.cc.is_empty());

        let mut again = message();
        again.subject = "RE: Budget".into();
        assert_eq!(
            ComposeRequest::reply(&again, "", false).subject,
            "RE: Budget"
        );
        assert_eq!(ComposeRequest::forward(&message()).subject, "Fw: Budget");
    }
}
