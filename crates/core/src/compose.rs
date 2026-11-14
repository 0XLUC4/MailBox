//! Compose domain: drafts (+ compose metadata), signatures, recipient suggestions,
//! reply/forward helpers. Body markup lives in [`markup`].
//!
//! Sending: `Command::QueueSend` moves the draft into the account's local "Outbox" folder
//! (`sync_bridge::outbox_folder`); the sync engine builds the RFC 5322 message from it,
//! delivers it and moves it to Sent.

pub mod address;
pub mod markup;

pub use address::{
    Mailto, display_recipient, is_valid_email, parse_address, parse_mailto, parse_recipients,
};

use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Local, Utc};
use rusqlite::Row;
use rusqlite::types::{Type, Value};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::command::Command;
use crate::error::Result;
use crate::model::*;
use crate::store::{Record, Store};

/// Tables owned by compose; appended to the store migrations.
pub const SCHEMA: &str = r#"
CREATE TABLE compose_drafts (
    id INTEGER PRIMARY KEY,
    message_id INTEGER NOT NULL UNIQUE REFERENCES messages(id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    bcc_json TEXT NOT NULL,
    read_receipt INTEGER NOT NULL,
    reply_to INTEGER,
    in_reply_to TEXT,
    references_json TEXT NOT NULL,
    attachments_json TEXT NOT NULL,
    markup TEXT NOT NULL
);
CREATE INDEX compose_drafts_key ON compose_drafts(key);
CREATE TABLE signatures (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL UNIQUE REFERENCES accounts(id),
    new_message TEXT NOT NULL,
    reply TEXT NOT NULL
);
"#;

/// A file attached to a draft (read from `path` when sending).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftAttachment {
    pub path: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
}

/// Compose state that does not fit [`Message`]: Bcc, markup source, reply links, files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftMeta {
    pub id: Id,
    /// The draft message row (set when saving).
    pub message_id: Id,
    /// RFC 5322 `Message-ID` of the outgoing message, stable across autosaves.
    pub key: String,
    pub bcc: Vec<Address>,
    pub read_receipt: bool,
    /// Message being replied to / forwarded.
    pub reply_to: Option<Id>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub attachments: Vec<DraftAttachment>,
    /// Body source (see [`markup`]).
    pub markup: String,
}

/// A draft message plus its compose metadata, saved atomically by `Command::SaveDraft`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Draft {
    pub message: Message,
    pub meta: DraftMeta,
}

/// Per-account signatures (markup): one for new messages, one for replies/forwards.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Signature {
    pub id: Id,
    pub account_id: Id,
    pub new_message: String,
    pub reply: String,
}

// ---------- queries ----------

pub fn draft_meta(store: &Store, message_id: Id) -> Result<Option<DraftMeta>> {
    let rows: Vec<DraftMeta> = store.query("WHERE message_id = ?1", [message_id])?;
    Ok(rows.into_iter().next())
}

/// Finds a draft by its `Message-ID` (used to learn the id assigned on first save).
pub fn draft_by_key(store: &Store, key: &str) -> Result<Option<DraftMeta>> {
    let rows: Vec<DraftMeta> = store.query("WHERE key = ?1 ORDER BY id LIMIT 1", [key])?;
    Ok(rows.into_iter().next())
}

/// A draft with its metadata; messages saved without metadata get a default one.
pub fn load_draft(store: &Store, message_id: Id) -> Result<Option<Draft>> {
    let Some(message) = store.get::<Message>(message_id)? else {
        return Ok(None);
    };
    let meta = match draft_meta(store, message_id)? {
        Some(meta) => meta,
        None => DraftMeta {
            id: 0,
            message_id,
            key: new_message_id(&message.from.email, Utc::now()),
            bcc: vec![],
            read_receipt: false,
            reply_to: None,
            in_reply_to: None,
            references: vec![],
            attachments: vec![],
            markup: markup::escape_markup(&message.body_text),
        },
    };
    Ok(Some(Draft { message, meta }))
}

pub fn signature(store: &Store, account_id: Id) -> Result<Option<Signature>> {
    let rows: Vec<Signature> = store.query("WHERE account_id = ?1", [account_id])?;
    Ok(rows.into_iter().next())
}

/// Autocomplete: contacts first, then past correspondents (newest first); prefix matches
/// rank first within each group. Case-insensitive on name and email.
pub fn suggest_recipients(store: &Store, query: &str, limit: usize) -> Result<Vec<Address>> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() || limit == 0 {
        return Ok(vec![]);
    }
    let mut seen: Vec<String> = Vec::new();
    let mut take = |address: Address, group: &mut Vec<Address>| {
        let email = address.email.to_lowercase();
        let hit = email.contains(&needle) || address.name.to_lowercase().contains(&needle);
        if hit && !email.is_empty() && !seen.contains(&email) {
            seen.push(email);
            group.push(address);
        }
    };

    let mut contacts = Vec::new();
    // Cards, not bare rows: a contact saved with only first/last name still has a name.
    for card in crate::people::list_contact_cards(store)? {
        let name = card.name();
        for email in &card.contact.emails {
            take(Address::new(name.clone(), email.clone()), &mut contacts);
        }
    }
    let mut correspondents = Vec::new();
    let recent: Vec<Message> = store.query("ORDER BY date DESC LIMIT 300", [])?;
    for message in recent {
        let addresses = std::iter::once(message.from)
            .chain(message.to)
            .chain(message.cc);
        for address in addresses {
            take(address, &mut correspondents);
        }
    }

    let is_prefix = |address: &Address| {
        address.email.to_lowercase().starts_with(&needle)
            || address
                .name
                .to_lowercase()
                .split_whitespace()
                .any(|word| word.starts_with(&needle))
    };
    contacts.sort_by_key(|address| !is_prefix(address));
    correspondents.sort_by_key(|address| !is_prefix(address));
    contacts.extend(correspondents);
    contacts.truncate(limit);
    Ok(contacts)
}

/// Result of "Check Names" for one recipient entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Resolved(Address),
    Ambiguous(Vec<Address>),
    NotFound,
}

/// Resolves a typed entry ("ada", "Ada Lovelace", "ada@x.org") against contacts and history.
pub fn resolve_recipient(store: &Store, entry: &Address) -> Result<Resolution> {
    if is_valid_email(&entry.email) {
        if entry.name.is_empty() {
            let known = suggest_recipients(store, &entry.email, 5)?
                .into_iter()
                .find(|address| address.email.eq_ignore_ascii_case(&entry.email));
            if let Some(known) = known {
                return Ok(Resolution::Resolved(known));
            }
        }
        return Ok(Resolution::Resolved(entry.clone()));
    }
    let text = if entry.name.is_empty() {
        &entry.email
    } else {
        &entry.name
    };
    let candidates = suggest_recipients(store, text, 8)?;
    let mut exact = candidates
        .iter()
        .filter(|address| address.name.eq_ignore_ascii_case(text.trim()));
    let single_exact = match (exact.next(), exact.next()) {
        (Some(address), None) => Some(address.clone()),
        _ => None,
    };
    Ok(match (candidates.len(), single_exact) {
        (0, _) => Resolution::NotFound,
        (1, _) => Resolution::Resolved(candidates[0].clone()),
        (_, Some(address)) => Resolution::Resolved(address),
        _ => Resolution::Ambiguous(candidates),
    })
}

// ---------- replies and forwards ----------

const REPLY_PREFIXES: [&str; 6] = ["re", "aw", "sv", "antw", "réf", "rif"];
const FORWARD_PREFIXES: [&str; 5] = ["fwd", "fw", "tr", "wg", "rv"];

/// Strips leading `Re:`-style prefixes from the given set (repeated, case-insensitive).
fn strip_prefixes<'a>(mut subject: &'a str, prefixes: &[&str]) -> &'a str {
    loop {
        let trimmed = subject.trim_start();
        let Some((head, rest)) = trimmed.split_once(':') else {
            return trimmed;
        };
        if prefixes.contains(&head.trim().to_lowercase().as_str()) {
            subject = rest;
        } else {
            return trimmed;
        }
    }
}

/// `Re: subject` (no `Re: Re:` pile-up).
pub fn reply_subject(subject: &str) -> String {
    format!("Re: {}", strip_prefixes(subject, &REPLY_PREFIXES))
}

/// Subject without any `Re:` / `Fwd:` prefixes, lowercased: groups messages that carry no
/// threading headers (imported files) into conversations.
pub fn conversation_subject(subject: &str) -> String {
    let all: Vec<&str> = REPLY_PREFIXES
        .iter()
        .chain(&FORWARD_PREFIXES)
        .copied()
        .collect();
    strip_prefixes(subject, &all).trim().to_lowercase()
}

/// `Fwd: subject`.
pub fn forward_subject(subject: &str) -> String {
    format!("Fwd: {}", strip_prefixes(subject, &FORWARD_PREFIXES))
}

/// To/Cc of a reply. `my_emails` are the user's addresses, never replied to.
pub fn reply_recipients(
    original: &Message,
    my_emails: &[String],
    reply_all: bool,
) -> (Vec<Address>, Vec<Address>) {
    let is_me = |address: &Address| {
        my_emails
            .iter()
            .any(|email| email.eq_ignore_ascii_case(&address.email))
    };
    let contains = |list: &[Address], address: &Address| {
        list.iter()
            .any(|other| other.email.eq_ignore_ascii_case(&address.email))
    };
    let mut to: Vec<Address> = Vec::new();
    let mut cc: Vec<Address> = Vec::new();
    let primary = if is_me(&original.from) {
        original.to.clone()
    } else {
        vec![original.from.clone()]
    };
    let extra_to = if reply_all {
        original.to.clone()
    } else {
        vec![]
    };
    for address in primary.into_iter().chain(extra_to) {
        if !is_me(&address) && !contains(&to, &address) {
            to.push(address);
        }
    }
    if reply_all {
        for address in &original.cc {
            if !is_me(address) && !contains(&to, address) && !contains(&cc, address) {
                cc.push(address.clone());
            }
        }
    }
    (to, cc)
}

fn local_date(date: &DateTime<Utc>) -> String {
    date.with_timezone(&Local)
        .format("%a, %b %-d, %Y at %H:%M")
        .to_string()
}

/// Original quoted below a reply: "On <date>, <sender> wrote:" then `> ` lines.
pub fn quote_for_reply(original: &Message) -> String {
    let header = format!(
        "On {}, {} wrote:",
        local_date(&original.date),
        markup::escape_markup(&display_recipient(&original.from))
    );
    let body: Vec<String> = original
        .body_text
        .lines()
        .map(|line| {
            if line.trim().is_empty() {
                ">".to_owned()
            } else {
                format!("> {}", markup::escape_markup(line))
            }
        })
        .collect();
    format!("{header}\n{}", body.join("\n"))
}

/// Original below a forward, with a bold header block.
pub fn quote_for_forward(original: &Message) -> String {
    let list = |addresses: &[Address]| {
        addresses
            .iter()
            .map(|address| markup::escape_markup(&display_recipient(address)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut lines = vec![
        "---------- Forwarded message ----------".to_owned(),
        format!(
            "**From:** {}",
            markup::escape_markup(&display_recipient(&original.from))
        ),
        format!("**Date:** {}", local_date(&original.date)),
        format!("**Subject:** {}", markup::escape_markup(&original.subject)),
        format!("**To:** {}", list(&original.to)),
    ];
    if !original.cc.is_empty() {
        lines.push(format!("**Cc:** {}", list(&original.cc)));
    }
    lines.push(String::new());
    lines.push(markup::escape_markup(&original.body_text));
    lines.join("\n")
}

/// `In-Reply-To` for a reply when the thread id is a Message-ID.
pub fn parent_message_id(original: &Message) -> Option<String> {
    let id = original.thread_id.trim();
    if !id.contains('@') {
        return None;
    }
    Some(if id.starts_with('<') {
        id.to_owned()
    } else {
        format!("<{id}>")
    })
}

/// First ~120 characters of a body, whitespace collapsed (for `Message::preview`).
pub fn preview_text(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(120).collect()
}

/// A fresh, unique `Message-ID` on the sender's domain.
pub fn new_message_id(from_email: &str, now: DateTime<Utc>) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let domain = from_email
        .rsplit_once('@')
        .map(|(_, domain)| domain)
        .filter(|domain| !domain.is_empty())
        .unwrap_or("mailbox.local");
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = now.timestamp_nanos_opt().unwrap_or_default();
    format!("<{nanos:x}.{count}.{:x}@{domain}>", std::process::id())
}

/// MIME type from a file extension (common types; `application/octet-stream` otherwise).
pub fn guess_mime(name: &str) -> &'static str {
    let extension = name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "json" => "application/json",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "ics" => "text/calendar",
        "vcf" => "text/vcard",
        "txt" | "log" | "md" => "text/plain",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    }
}

// ---------- command handlers (called from `command::apply_inner`) ----------

/// Moves the draft into `folder_id` (the Outbox) and drops its compose metadata.
/// The inverse restores the draft exactly.
pub(crate) fn apply_queue_send(store: &Store, message_id: Id, folder_id: Id) -> Result<Command> {
    let message = store.require::<Message>(message_id)?;
    let folder = store.require::<Folder>(folder_id)?;
    let meta = draft_meta(store, message_id)?;
    if let Some(meta) = &meta {
        store.delete::<DraftMeta>(meta.id)?;
    }
    let mut queued = message.clone();
    queued.folder_id = folder.id;
    queued.account_id = folder.account_id;
    queued.flags.read = true;
    store.save(&mut queued)?;
    Ok(match meta {
        Some(meta) => Command::SaveDraft(Box::new(Draft { message, meta })),
        None => Command::SaveMessage(Box::new(message)),
    })
}

pub(crate) fn apply_save_draft(store: &Store, draft: Draft) -> Result<Command> {
    let Draft {
        mut message,
        mut meta,
    } = draft;
    let (previous_message, previous_meta) = if message.id == 0 {
        (None, None)
    } else {
        (
            store.get::<Message>(message.id)?,
            draft_meta(store, message.id)?,
        )
    };
    store.save(&mut message)?;
    meta.message_id = message.id;
    meta.id = previous_meta.as_ref().map_or(0, |previous| previous.id);
    store.save(&mut meta)?;
    Ok(match (previous_message, previous_meta) {
        (Some(message), Some(meta)) => Command::SaveDraft(Box::new(Draft { message, meta })),
        (Some(previous), None) => Command::Batch(vec![
            Command::DeleteDraft(previous.id),
            Command::SaveMessage(Box::new(previous)),
        ]),
        (None, _) => Command::DeleteDraft(message.id),
    })
}

pub(crate) fn apply_delete_draft(store: &Store, message_id: Id) -> Result<Command> {
    let message = store.require::<Message>(message_id)?;
    let meta = draft_meta(store, message_id)?;
    if let Some(meta) = &meta {
        store.delete::<DraftMeta>(meta.id)?;
    }
    store.delete::<Message>(message_id)?;
    Ok(match meta {
        Some(meta) => Command::SaveDraft(Box::new(Draft { message, meta })),
        None => Command::SaveMessage(Box::new(message)),
    })
}

pub(crate) fn apply_save_signature(store: &Store, mut signature: Signature) -> Result<Command> {
    let previous = self::signature(store, signature.account_id)?;
    signature.id = previous.as_ref().map_or(0, |previous| previous.id);
    store.save(&mut signature)?;
    let restore = previous.unwrap_or(Signature {
        id: signature.id,
        account_id: signature.account_id,
        ..Signature::default()
    });
    Ok(Command::SaveSignature(Box::new(restore)))
}

// ---------- persistence ----------

fn json<T: Serialize>(value: &T) -> Result<Value> {
    Ok(Value::Text(serde_json::to_string(value)?))
}

fn get_json<T: DeserializeOwned>(row: &Row, index: usize) -> rusqlite::Result<T> {
    let raw: String = row.get(index)?;
    serde_json::from_str(&raw)
        .map_err(|err| rusqlite::Error::FromSqlConversionFailure(index, Type::Text, Box::new(err)))
}

fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

impl Record for DraftMeta {
    const KIND: &'static str = "draft";
    const TABLE: &'static str = "compose_drafts";
    const COLUMNS: &'static [&'static str] = &[
        "message_id",
        "key",
        "bcc_json",
        "read_receipt",
        "reply_to",
        "in_reply_to",
        "references_json",
        "attachments_json",
        "markup",
    ];

    fn id(&self) -> Id {
        self.id
    }
    fn set_id(&mut self, id: Id) {
        self.id = id;
    }
    fn values(&self) -> Result<Vec<Value>> {
        Ok(vec![
            Value::Integer(self.message_id),
            text(&self.key),
            json(&self.bcc)?,
            Value::Integer(i64::from(self.read_receipt)),
            self.reply_to.map_or(Value::Null, Value::Integer),
            self.in_reply_to.as_deref().map_or(Value::Null, text),
            json(&self.references)?,
            json(&self.attachments)?,
            text(&self.markup),
        ])
    }
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            message_id: row.get(1)?,
            key: row.get(2)?,
            bcc: get_json(row, 3)?,
            read_receipt: row.get(4)?,
            reply_to: row.get(5)?,
            in_reply_to: row.get(6)?,
            references: get_json(row, 7)?,
            attachments: get_json(row, 8)?,
            markup: row.get(9)?,
        })
    }
}

impl Record for Signature {
    const KIND: &'static str = "signature";
    const TABLE: &'static str = "signatures";
    const COLUMNS: &'static [&'static str] = &["account_id", "new_message", "reply"];

    fn id(&self) -> Id {
        self.id
    }
    fn set_id(&mut self, id: Id) {
        self.id = id;
    }
    fn values(&self) -> Result<Vec<Value>> {
        Ok(vec![
            Value::Integer(self.account_id),
            text(&self.new_message),
            text(&self.reply),
        ])
    }
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            account_id: row.get(1)?,
            new_message: row.get(2)?,
            reply: row.get(3)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::History;
    use crate::store::tests::{account, folder, message, store};

    fn draft(account: &Account, drafts: &Folder) -> Draft {
        let mut message = message(account.id, drafts.id);
        message.attachments.clear();
        message.from = Address::new(&account.name, &account.email);
        message.to = vec![Address::new("Bob", "bob@x.org")];
        Draft {
            message,
            meta: DraftMeta {
                id: 0,
                message_id: 0,
                key: "<k1@example.com>".into(),
                bcc: vec![Address::new("", "hidden@y.org")],
                read_receipt: true,
                reply_to: None,
                in_reply_to: None,
                references: vec![],
                attachments: vec![],
                markup: "Hello **Bob**".into(),
            },
        }
    }

    #[test]
    fn subjects_get_single_prefixes() {
        assert_eq!(reply_subject("Hello"), "Re: Hello");
        assert_eq!(reply_subject("RE: re:Hello"), "Re: Hello");
        assert_eq!(reply_subject("Fwd: Hello"), "Re: Fwd: Hello");
        assert_eq!(forward_subject("FW: Hello"), "Fwd: Hello");
        assert_eq!(forward_subject("Re: Hello"), "Fwd: Re: Hello");
        assert_eq!(reply_subject("Note: x"), "Re: Note: x");
        assert_eq!(conversation_subject("RE: Fwd: re: Q4 Budget "), "q4 budget");
        assert_eq!(conversation_subject("Note: x"), "note: x");
    }

    #[test]
    fn reply_recipients_skip_me_and_duplicates() {
        let store = store();
        let me = account(&store);
        let inbox = folder(&store, me.id, FolderRole::Inbox);
        let mut original = message(me.id, inbox.id);
        original.to = vec![
            Address::new("Me", "ME@example.com"),
            Address::new("Carl", "carl@x.org"),
        ];
        original.cc = vec![
            Address::new("Ada again", "ada@example.com"),
            Address::new("Dee", "dee@x.org"),
        ];
        let mine = vec![me.email.clone()];
        let (to, cc) = reply_recipients(&original, &mine, false);
        assert_eq!(to, vec![Address::new("Ada", "ada@example.com")]);
        assert!(cc.is_empty());
        let (to, cc) = reply_recipients(&original, &mine, true);
        assert_eq!(
            to,
            vec![
                Address::new("Ada", "ada@example.com"),
                Address::new("Carl", "carl@x.org")
            ]
        );
        assert_eq!(cc, vec![Address::new("Dee", "dee@x.org")]);
    }

    #[test]
    fn quotes_reply_and_forward() {
        let store = store();
        let me = account(&store);
        let inbox = folder(&store, me.id, FolderRole::Inbox);
        let mut original = message(me.id, inbox.id);
        original.body_text = "Hi *there*\n\nBye".into();
        let quote = quote_for_reply(&original);
        assert!(quote.starts_with("On "));
        assert!(quote.contains("Ada <ada@example.com> wrote:\n> Hi \\*there\\*\n>\n> Bye"));
        let blocks = markup::parse(&quote);
        assert_eq!(blocks[1].quote, 1);
        assert_eq!(markup::to_plain(&blocks[1..2]), "> Hi *there*");

        let forward = quote_for_forward(&original);
        assert!(forward.starts_with("---------- Forwarded message ----------\n**From:** Ada"));
        assert!(forward.contains("**Subject:** Hello\n**To:** Me <me@example.com>\n\nHi"));
        assert_eq!(parent_message_id(&original), None);
        original.thread_id = "abc@x.org".into();
        assert_eq!(parent_message_id(&original).as_deref(), Some("<abc@x.org>"));
    }

    #[test]
    fn draft_save_update_and_discard_are_undoable() {
        let store = store();
        let me = account(&store);
        let drafts = folder(&store, me.id, FolderRole::Drafts);
        let mut history = History::new();
        history
            .run(&store, Command::SaveDraft(Box::new(draft(&me, &drafts))))
            .unwrap();
        let meta = draft_by_key(&store, "<k1@example.com>").unwrap().unwrap();
        let saved = load_draft(&store, meta.message_id).unwrap().unwrap();
        assert_eq!(saved.meta.markup, "Hello **Bob**");
        assert_eq!(saved.meta.bcc.len(), 1);

        let mut edited = saved.clone();
        edited.meta.markup = "Changed".into();
        edited.meta.id = 0;
        history
            .run(&store, Command::SaveDraft(Box::new(edited)))
            .unwrap();
        assert_eq!(
            draft_meta(&store, saved.message.id)
                .unwrap()
                .unwrap()
                .markup,
            "Changed"
        );
        history.undo(&store).unwrap();
        assert_eq!(
            load_draft(&store, saved.message.id).unwrap().unwrap(),
            saved
        );

        history
            .run(&store, Command::DeleteDraft(saved.message.id))
            .unwrap();
        assert!(load_draft(&store, saved.message.id).unwrap().is_none());
        history.undo(&store).unwrap();
        assert_eq!(
            load_draft(&store, saved.message.id)
                .unwrap()
                .unwrap()
                .meta
                .markup,
            "Hello **Bob**"
        );
        history.undo(&store).unwrap();
        assert!(store.list_messages(drafts.id).unwrap().is_empty());
    }

    #[test]
    fn queue_send_moves_draft_to_outbox_and_undoes() {
        let store = store();
        let me = account(&store);
        let drafts = folder(&store, me.id, FolderRole::Drafts);
        let outbox = folder(&store, me.id, FolderRole::Custom);
        let mut history = History::new();
        history
            .run(&store, Command::SaveDraft(Box::new(draft(&me, &drafts))))
            .unwrap();
        let meta = draft_by_key(&store, "<k1@example.com>").unwrap().unwrap();
        let saved = load_draft(&store, meta.message_id).unwrap().unwrap();
        let queue = Command::QueueSend {
            message_id: saved.message.id,
            folder_id: outbox.id,
        };
        assert_eq!(queue.label(), "Send message");
        history.run(&store, queue).unwrap();
        assert!(store.list_messages(drafts.id).unwrap().is_empty());
        let queued = store.list_messages(outbox.id).unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].body_text, saved.message.body_text);
        assert!(draft_meta(&store, saved.message.id).unwrap().is_none());

        history.undo(&store).unwrap();
        assert!(store.list_messages(outbox.id).unwrap().is_empty());
        assert_eq!(
            load_draft(&store, saved.message.id).unwrap().unwrap(),
            saved
        );
        history.redo(&store).unwrap();
        assert_eq!(store.list_messages(outbox.id).unwrap().len(), 1);
    }

    #[test]
    fn guesses_mime_and_unique_ids() {
        assert_eq!(guess_mime("Report.PDF"), "application/pdf");
        assert_eq!(guess_mime("noext"), "application/octet-stream");
        assert!(new_message_id("a@b.co", Utc::now()).ends_with("@b.co>"));
        assert_ne!(
            new_message_id("a@b.co", Utc::now()),
            new_message_id("a@b.co", Utc::now())
        );
    }

    #[test]
    fn suggestions_rank_contacts_first_and_resolve_names() {
        let store = store();
        let me = account(&store);
        let inbox = folder(&store, me.id, FolderRole::Inbox);
        let mut contact = Contact {
            id: 0,
            account_id: me.id,
            display_name: "Adam Smith".into(),
            emails: vec!["adam@corp.com".into()],
            phones: vec![],
            company: String::new(),
            job_title: String::new(),
            notes: String::new(),
            favorite: false,
        };
        store.save(&mut contact).unwrap();
        let mut msg = message(me.id, inbox.id);
        store.save(&mut msg).unwrap();

        let found = suggest_recipients(&store, "ad", 10).unwrap();
        assert_eq!(
            found,
            vec![
                Address::new("Adam Smith", "adam@corp.com"),
                Address::new("Ada", "ada@example.com")
            ]
        );
        assert!(suggest_recipients(&store, "  ", 10).unwrap().is_empty());
        assert_eq!(
            resolve_recipient(&store, &Address::new("", "smith")).unwrap(),
            Resolution::Resolved(Address::new("Adam Smith", "adam@corp.com"))
        );
        assert_eq!(
            resolve_recipient(&store, &Address::new("", "ada@example.com")).unwrap(),
            Resolution::Resolved(Address::new("Ada", "ada@example.com"))
        );
        assert!(matches!(
            resolve_recipient(&store, &Address::new("", "ad")).unwrap(),
            Resolution::Ambiguous(list) if list.len() == 2
        ));
        assert_eq!(
            resolve_recipient(&store, &Address::new("", "zzz")).unwrap(),
            Resolution::NotFound
        );
    }

    #[test]
    fn signatures_upsert_per_account_and_undo() {
        let store = store();
        let me = account(&store);
        let mut history = History::new();
        let signature = |text: &str| Signature {
            id: 0,
            account_id: me.id,
            new_message: text.into(),
            reply: "-- reply".into(),
        };
        history
            .run(&store, Command::SaveSignature(Box::new(signature("-- v1"))))
            .unwrap();
        history
            .run(&store, Command::SaveSignature(Box::new(signature("-- v2"))))
            .unwrap();
        let rows: Vec<Signature> = store.query("", []).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].new_message, "-- v2");
        history.undo(&store).unwrap();
        assert_eq!(
            super::signature(&store, me.id)
                .unwrap()
                .unwrap()
                .new_message,
            "-- v1"
        );
        history.undo(&store).unwrap();
        assert_eq!(
            super::signature(&store, me.id)
                .unwrap()
                .unwrap()
                .new_message,
            ""
        );
    }
}
