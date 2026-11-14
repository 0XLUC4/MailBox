//! Domain model. Plain data, serializable, no I/O.
//!
//! Ids are SQLite rowids. An id of `0` means "not saved yet": saving it inserts a new row.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

pub type Id = i64;

/// An RGB colour packed as `0xRRGGBB`.
pub type Color = u32;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Account {
    pub id: Id,
    pub name: String,
    pub email: String,
    pub color: Color,
}

/// Well-known folder purpose; `Custom` for user folders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FolderRole {
    Inbox,
    Sent,
    Drafts,
    Archive,
    Trash,
    Junk,
    Custom,
}

impl FolderRole {
    pub const ALL: [FolderRole; 7] = [
        Self::Inbox,
        Self::Sent,
        Self::Drafts,
        Self::Archive,
        Self::Trash,
        Self::Junk,
        Self::Custom,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inbox => "inbox",
            Self::Sent => "sent",
            Self::Drafts => "drafts",
            Self::Archive => "archive",
            Self::Trash => "trash",
            Self::Junk => "junk",
            Self::Custom => "custom",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|role| role.as_str() == value)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Folder {
    pub id: Id,
    pub account_id: Id,
    /// Parent folder for nesting; `None` at the account root.
    pub parent_id: Option<Id>,
    pub name: String,
    pub role: FolderRole,
    /// Display order among siblings.
    pub position: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Address {
    pub name: String,
    pub email: String,
}

impl Address {
    pub fn new(name: impl Into<String>, email: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            email: email.into(),
        }
    }

    /// Name if present, otherwise the email.
    pub fn display(&self) -> &str {
        if self.name.is_empty() {
            &self.email
        } else {
            &self.name
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Importance {
    Low,
    #[default]
    Normal,
    High,
}

impl Importance {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Normal => "normal",
            Self::High => "high",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        [Self::Low, Self::Normal, Self::High]
            .into_iter()
            .find(|i| i.as_str() == value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MessageFlags {
    pub read: bool,
    pub flagged: bool,
    pub pinned: bool,
}

/// Attachment metadata only; content is fetched/stored elsewhere later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub name: String,
    pub mime: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: Id,
    pub account_id: Id,
    pub folder_id: Id,
    /// Messages of a conversation share the same thread id.
    pub thread_id: String,
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub subject: String,
    pub date: DateTime<Utc>,
    /// First ~120 chars of the body, for the message list.
    pub preview: String,
    pub body_text: String,
    pub body_html: Option<String>,
    pub flags: MessageFlags,
    pub importance: Importance,
    /// Category names (see [`Category`]).
    pub categories: Vec<String>,
    pub attachments: Vec<Attachment>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Calendar {
    pub id: Id,
    pub account_id: Id,
    pub name: String,
    pub color: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseStatus {
    #[default]
    NeedsAction,
    Accepted,
    Tentative,
    Declined,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attendee {
    pub address: Address,
    pub status: ResponseStatus,
    pub optional: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalendarEvent {
    pub id: Id,
    pub calendar_id: Id,
    pub title: String,
    pub location: String,
    pub description: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub all_day: bool,
    /// iCalendar RRULE value (e.g. `FREQ=WEEKLY;BYDAY=MO`), `None` for one-off events.
    pub recurrence: Option<String>,
    pub attendees: Vec<Attendee>,
    /// Reminder offsets in minutes before `start`.
    pub reminders: Vec<i64>,
    pub categories: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Contact {
    pub id: Id,
    pub account_id: Id,
    pub display_name: String,
    pub emails: Vec<String>,
    pub phones: Vec<String>,
    pub company: String,
    pub job_title: String,
    pub notes: String,
    pub favorite: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: Id,
    pub account_id: Id,
    pub title: String,
    pub notes: String,
    pub due: Option<NaiveDate>,
    pub completed: bool,
    pub importance: Importance,
    pub categories: Vec<String>,
    pub created: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Category {
    pub id: Id,
    pub name: String,
    pub color: Color,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleCondition {
    FromContains(String),
    SubjectContains(String),
    ToContains(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleAction {
    MoveTo(Id),
    AddCategory(String),
    MarkRead,
    Flag,
}

/// Mail rule: when every condition matches, run every action.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub id: Id,
    pub account_id: Id,
    pub name: String,
    pub enabled: bool,
    pub conditions: Vec<RuleCondition>,
    pub actions: Vec<RuleAction>,
}

impl Rule {
    /// True when the rule is enabled and every condition matches (case-insensitive).
    pub fn matches(&self, message: &Message) -> bool {
        let contains =
            |haystack: &str, needle: &str| haystack.to_lowercase().contains(&needle.to_lowercase());
        self.enabled
            && !self.conditions.is_empty()
            && self.conditions.iter().all(|condition| match condition {
                RuleCondition::FromContains(text) => {
                    contains(&message.from.email, text) || contains(&message.from.name, text)
                }
                RuleCondition::SubjectContains(text) => contains(&message.subject, text),
                RuleCondition::ToContains(text) => {
                    message.to.iter().any(|to| contains(&to.email, text))
                }
            })
    }
}
