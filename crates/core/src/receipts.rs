//! Read receipts (incoming `Disposition-Notification-To`): requests are remembered per message
//! when it arrives; opening the message asks (or follows the policy) and sends a plain-text
//! receipt through the normal send path.
//!
//! State is one JSON blob in the settings table ([`KEY`]).

use std::collections::BTreeMap;

use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};

use crate::auto_reply::simple_draft;
use crate::compose::Draft;
use crate::settings::{get_json, set_json_command};
use crate::{Account, Address, Command, Id, Message, Result, Store};

/// Settings key of the [`Receipts`] blob.
pub const KEY: &str = "mail.read_receipts";

/// What to do when an opened message asks for a read receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptPolicy {
    #[default]
    Ask,
    Always,
    Never,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Receipts {
    pub policy: ReceiptPolicy,
    /// Message id → address the receipt goes to.
    pub pending: BTreeMap<Id, Address>,
}

impl Receipts {
    pub fn load(store: &Store) -> Result<Self> {
        Ok(get_json(store, KEY)?.unwrap_or_default())
    }

    pub fn command(&self) -> Result<Command> {
        set_json_command(KEY, self)
    }
}

/// A plain-text receipt for `original` (RFC 8098 asks for multipart/report; most clients
/// display this simple form just as well).
pub fn receipt_draft(
    account: &Account,
    original: &Message,
    to: Address,
    drafts_folder: Id,
    now: DateTime<Utc>,
) -> Draft {
    let body = format!(
        "Your message\n\n  To: {}\n  Subject: {}\n  Sent: {}\n\nwas read on {}.",
        account.email,
        original.subject,
        original.date.with_timezone(&Local).format("%Y-%m-%d %H:%M"),
        now.with_timezone(&Local).format("%Y-%m-%d %H:%M"),
    );
    simple_draft(
        account,
        drafts_folder,
        to,
        format!("Read: {}", original.subject),
        &body,
        Some(original),
        now,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Importance, MessageFlags};

    #[test]
    fn pending_requests_persist() {
        let store = Store::open_in_memory().unwrap();
        let mut receipts = Receipts::default();
        receipts.pending.insert(4, Address::new("", "a@b.co"));
        receipts.policy = ReceiptPolicy::Always;
        crate::command::apply(&store, receipts.command().unwrap()).unwrap();
        assert_eq!(Receipts::load(&store).unwrap(), receipts);
    }

    #[test]
    fn receipt_mentions_the_original() {
        let account = Account {
            id: 1,
            name: "Me".into(),
            email: "me@x.org".into(),
            color: 0,
        };
        let original = Message {
            id: 4,
            account_id: 1,
            folder_id: 2,
            thread_id: "id@x".into(),
            from: Address::new("", "a@b.co"),
            to: vec![],
            cc: vec![],
            subject: "Budget".into(),
            date: Utc::now(),
            preview: String::new(),
            body_text: String::new(),
            body_html: None,
            flags: MessageFlags::default(),
            importance: Importance::Normal,
            categories: vec![],
            attachments: vec![],
        };
        let draft = receipt_draft(&account, &original, original.from.clone(), 9, Utc::now());
        assert_eq!(draft.message.subject, "Read: Budget");
        assert!(draft.message.body_text.contains("Subject: Budget"));
        assert_eq!(draft.message.to[0].email, "a@b.co");
    }
}
