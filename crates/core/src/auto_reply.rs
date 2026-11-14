//! Automatic replies (out of office), local rule-based fallback: while active, each sender
//! gets one reply per period. Replies are plain drafts queued through the normal send path.
//!
//! State is one JSON blob in the settings table ([`KEY`]); persist with [`AutoReply::command`].

use std::collections::BTreeSet;

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::compose::{self, Draft, DraftMeta, markup};
use crate::settings::{get_json, set_json_command};
use crate::{Account, Address, Command, Id, Importance, Message, MessageFlags, Result, Store};

/// Settings key of the [`AutoReply`] blob.
pub const KEY: &str = "mail.auto_reply";

/// Sender local parts that never get an automatic reply (robots, lists, bounces).
const AUTOMATED_SENDERS: [&str; 9] = [
    "noreply",
    "no-reply",
    "no_reply",
    "donotreply",
    "do-not-reply",
    "mailer-daemon",
    "postmaster",
    "bounce",
    "notifications",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoReply {
    pub enabled: bool,
    /// First day (inclusive); `None` = from now.
    pub start: Option<NaiveDate>,
    /// Last day (inclusive); `None` = until turned off.
    pub end: Option<NaiveDate>,
    pub subject: String,
    pub body: String,
    /// Lower-cased addresses already answered in this period.
    pub replied: BTreeSet<String>,
}

impl Default for AutoReply {
    fn default() -> Self {
        Self {
            enabled: false,
            start: None,
            end: None,
            subject: "Automatic reply".into(),
            body: "Thanks for your message. I'm away and will reply when I'm back.".into(),
            replied: BTreeSet::new(),
        }
    }
}

impl AutoReply {
    /// Stored settings, defaults (off) when never saved.
    pub fn load(store: &Store) -> Result<Self> {
        Ok(get_json(store, KEY)?.unwrap_or_default())
    }

    /// Command that persists these settings.
    pub fn command(&self) -> Result<Command> {
        set_json_command(KEY, self)
    }

    /// Enabled and `today` within the date range.
    pub fn is_active(&self, today: NaiveDate) -> bool {
        self.enabled
            && self.start.is_none_or(|start| start <= today)
            && self.end.is_none_or(|end| today <= end)
    }

    /// Whether `message` (just received) should get an automatic reply.
    /// `own_emails` are the user's addresses (never answered).
    pub fn should_reply(&self, message: &Message, own_emails: &[String], today: NaiveDate) -> bool {
        let email = message.from.email.trim().to_lowercase();
        let local_part = email.split('@').next().unwrap_or_default();
        self.is_active(today)
            && email.contains('@')
            && !own_emails
                .iter()
                .any(|own| own.eq_ignore_ascii_case(&email))
            && !self.replied.contains(&email)
            && !AUTOMATED_SENDERS
                .iter()
                .any(|robot| local_part.contains(robot))
    }

    pub fn mark_replied(&mut self, email: &str) {
        self.replied.insert(email.trim().to_lowercase());
    }

    /// The reply draft for `original`, to save in `drafts_folder` and queue.
    pub fn reply_draft(
        &self,
        account: &Account,
        original: &Message,
        drafts_folder: Id,
        now: DateTime<Utc>,
    ) -> Draft {
        let subject = if self.subject.trim().is_empty() {
            compose::reply_subject(&original.subject)
        } else {
            format!("{}: {}", self.subject.trim(), original.subject)
        };
        simple_draft(
            account,
            drafts_folder,
            original.from.clone(),
            subject,
            &self.body,
            Some(original),
            now,
        )
    }
}

/// A plain-text draft from `account` to `to`, threaded under `original` when given.
pub fn simple_draft(
    account: &Account,
    drafts_folder: Id,
    to: Address,
    subject: String,
    body: &str,
    original: Option<&Message>,
    now: DateTime<Utc>,
) -> Draft {
    let in_reply_to = original.and_then(compose::parent_message_id);
    let message = Message {
        id: 0,
        account_id: account.id,
        folder_id: drafts_folder,
        thread_id: original.map(|o| o.thread_id.clone()).unwrap_or_default(),
        from: Address::new(account.name.clone(), account.email.clone()),
        to: vec![to],
        cc: Vec::new(),
        subject,
        date: now,
        preview: compose::preview_text(body),
        body_text: body.to_owned(),
        body_html: None,
        flags: MessageFlags {
            read: true,
            ..MessageFlags::default()
        },
        importance: Importance::Normal,
        categories: Vec::new(),
        attachments: Vec::new(),
    };
    let meta = DraftMeta {
        id: 0,
        message_id: 0,
        key: compose::new_message_id(&account.email, now),
        bcc: Vec::new(),
        read_receipt: false,
        reply_to: original.map(|o| o.id),
        references: in_reply_to.iter().cloned().collect(),
        in_reply_to,
        attachments: Vec::new(),
        markup: markup::escape_markup(body),
    };
    Draft { message, meta }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(text: &str) -> NaiveDate {
        text.parse().unwrap()
    }

    fn message(from: &str) -> Message {
        Message {
            id: 3,
            account_id: 1,
            folder_id: 2,
            thread_id: "abc@x.org".into(),
            from: Address::new("Ada", from),
            to: vec![Address::new("", "me@x.org")],
            cc: vec![],
            subject: "Lunch?".into(),
            date: Utc::now(),
            preview: String::new(),
            body_text: String::new(),
            body_html: None,
            flags: MessageFlags::default(),
            importance: Importance::Normal,
            categories: vec![],
            attachments: vec![],
        }
    }

    fn active() -> AutoReply {
        AutoReply {
            enabled: true,
            start: Some(day("2026-07-01")),
            end: Some(day("2026-07-15")),
            ..AutoReply::default()
        }
    }

    #[test]
    fn replies_once_per_sender_within_range() {
        let mut rule = active();
        let own = vec!["me@x.org".to_owned()];
        let today = day("2026-07-03");
        let incoming = message("Ada@Example.com");
        assert!(rule.should_reply(&incoming, &own, today));
        rule.mark_replied(&incoming.from.email);
        assert!(!rule.should_reply(&incoming, &own, today));
        assert!(rule.should_reply(&message("bob@example.com"), &own, today));
        assert!(!rule.should_reply(&message("bob@example.com"), &own, day("2026-07-16")));
        assert!(!rule.should_reply(&message("bob@example.com"), &own, day("2026-06-30")));
    }

    #[test]
    fn skips_robots_self_and_disabled() {
        let rule = active();
        let own = vec!["me@x.org".to_owned()];
        let today = day("2026-07-03");
        assert!(!rule.should_reply(&message("no-reply@shop.com"), &own, today));
        assert!(!rule.should_reply(&message("MAILER-DAEMON@x.org"), &own, today));
        assert!(!rule.should_reply(&message("me@x.org"), &own, today));
        assert!(!rule.should_reply(&message(""), &own, today));
        let off = AutoReply {
            enabled: false,
            ..active()
        };
        assert!(!off.should_reply(&message("ada@example.com"), &own, today));
    }

    #[test]
    fn builds_a_threaded_reply() {
        let account = Account {
            id: 1,
            name: "Me".into(),
            email: "me@x.org".into(),
            color: 0,
        };
        let draft = active().reply_draft(&account, &message("ada@example.com"), 9, Utc::now());
        assert_eq!(draft.message.folder_id, 9);
        assert_eq!(draft.message.to[0].email, "ada@example.com");
        assert_eq!(draft.message.subject, "Automatic reply: Lunch?");
        assert_eq!(draft.meta.in_reply_to.as_deref(), Some("<abc@x.org>"));
        assert_eq!(draft.meta.reply_to, Some(3));
    }
}
