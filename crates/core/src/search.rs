//! Mail search query language, parsed then executed in memory against the store.
//!
//! ```text
//! budget "q3 plan" from:ada to:bob subject:report body:invoice has:attachment
//! is:unread is:read is:flagged is:pinned is:important -draft -from:noreply
//! after:2026-10-01 before:2026-11-01 in:inbox in:all category:work importance:high folder:12
//! ```
//!
//! Every term must match (AND); `-` negates a term; quotes group words. Words are matched
//! case-insensitively as substrings. Unknown `key:value` pairs are searched as plain text and
//! incomplete ones (`from:`) are ignored, so a query being typed never errors.
//! Scope: all mailboxes by default; `folder:<id>` restricts to a folder, `in:<name>` to folders
//! whose name or role matches.

use std::collections::HashMap;

use chrono::{NaiveDate, NaiveTime};

use crate::error::Result;
use crate::model::{Id, Importance, Message};
use crate::store::Store;

/// Operators offered as suggestions: (syntax, description).
pub const OPERATORS: &[(&str, &str)] = &[
    ("from:", "Sender name or address"),
    ("to:", "Recipient (to or cc)"),
    ("subject:", "Words in the subject"),
    ("body:", "Words in the message body"),
    ("has:attachment", "Messages with attachments"),
    ("is:unread", "Unread messages"),
    ("is:read", "Read messages"),
    ("is:flagged", "Flagged messages"),
    ("is:pinned", "Pinned messages"),
    ("is:important", "High importance"),
    ("after:", "Received on or after a date (YYYY-MM-DD)"),
    ("before:", "Received before a date (YYYY-MM-DD)"),
    ("in:", "Folder name (inbox, sent, archive...)"),
    ("in:all", "Every folder of every account"),
    ("category:", "Category name"),
    ("importance:", "low, normal or high"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Unread,
    Read,
    Flagged,
    Pinned,
    Important,
}

/// One search criterion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    /// Free text: subject, sender, recipients, body, categories.
    Text(String),
    From(String),
    /// To or cc.
    To(String),
    Subject(String),
    Body(String),
    HasAttachment,
    Is(Status),
    /// On or after this day (UTC).
    After(NaiveDate),
    /// Strictly before this day (UTC).
    Before(NaiveDate),
    /// Folder name or role.
    In(String),
    /// Explicitly every folder (`in:all`); matches everything.
    AllFolders,
    FolderId(Id),
    Category(String),
    Importance(Importance),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term {
    pub negated: bool,
    pub filter: Filter,
}

/// A parsed query: every term must match.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchQuery {
    pub terms: Vec<Term>,
}

/// Splits on whitespace, keeping quoted groups together (quotes removed).
fn tokenize(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in input.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn parse_filter(token: &str) -> Option<Filter> {
    let text = |value: &str| value.to_lowercase();
    let Some((key, value)) = token.split_once(':') else {
        return Some(Filter::Text(text(token)));
    };
    let value = value.trim();
    let key = key.to_lowercase();
    let known = matches!(
        key.as_str(),
        "from"
            | "to"
            | "cc"
            | "subject"
            | "body"
            | "has"
            | "is"
            | "after"
            | "since"
            | "before"
            | "in"
            | "folder"
            | "category"
            | "cat"
            | "importance"
    );
    if !known {
        return Some(Filter::Text(text(token)));
    }
    if value.is_empty() {
        return None; // still being typed
    }
    let lower = text(value);
    let date = || NaiveDate::parse_from_str(value, "%Y-%m-%d").ok();
    match key.as_str() {
        "from" => Some(Filter::From(lower)),
        "to" | "cc" => Some(Filter::To(lower)),
        "subject" => Some(Filter::Subject(lower)),
        "body" => Some(Filter::Body(lower)),
        "has" => matches!(lower.as_str(), "attachment" | "attachments" | "attach")
            .then_some(Filter::HasAttachment),
        "is" => match lower.as_str() {
            "unread" => Some(Filter::Is(Status::Unread)),
            "read" => Some(Filter::Is(Status::Read)),
            "flagged" | "starred" => Some(Filter::Is(Status::Flagged)),
            "pinned" => Some(Filter::Is(Status::Pinned)),
            "important" => Some(Filter::Is(Status::Important)),
            _ => None,
        },
        "after" | "since" => date().map(Filter::After),
        "before" => date().map(Filter::Before),
        "in" => Some(match lower.as_str() {
            "all" | "*" | "anywhere" => Filter::AllFolders,
            _ => Filter::In(lower),
        }),
        "folder" => value.parse().ok().map(Filter::FolderId),
        "category" | "cat" => Some(Filter::Category(lower)),
        "importance" => Importance::parse(&lower).map(Filter::Importance),
        _ => None,
    }
}

impl SearchQuery {
    pub fn parse(input: &str) -> Self {
        let terms = tokenize(input)
            .iter()
            .filter_map(|token| {
                let (negated, body) = match token.strip_prefix('-') {
                    Some(rest) if !rest.is_empty() => (true, rest),
                    _ => (false, token.as_str()),
                };
                parse_filter(body).map(|filter| Term { negated, filter })
            })
            .collect();
        Self { terms }
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// True when the query already chooses folders (`in:` / `folder:`), so the UI scope
    /// must not add its own.
    pub fn has_folder_filter(&self) -> bool {
        self.terms.iter().any(|term| {
            matches!(
                term.filter,
                Filter::In(_) | Filter::AllFolders | Filter::FolderId(_)
            )
        })
    }

    /// Whether `message` matches every term. `folders` maps folder id to (lowercase name, role).
    pub fn matches(&self, message: &Message, folders: &FolderNames) -> bool {
        self.terms
            .iter()
            .all(|term| term_matches(&term.filter, message, folders) != term.negated)
    }
}

/// Folder id to (lowercase name, role string), for `in:`.
pub type FolderNames = HashMap<Id, (String, &'static str)>;

fn contains(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(needle)
}

fn term_matches(filter: &Filter, message: &Message, folders: &FolderNames) -> bool {
    let from = |needle: &str| {
        contains(&message.from.name, needle) || contains(&message.from.email, needle)
    };
    let to = |needle: &str| {
        message
            .to
            .iter()
            .chain(&message.cc)
            .any(|address| contains(&address.name, needle) || contains(&address.email, needle))
    };
    let start_of = |day: &NaiveDate| day.and_time(NaiveTime::MIN).and_utc();
    match filter {
        Filter::Text(needle) => {
            contains(&message.subject, needle)
                || from(needle)
                || to(needle)
                || contains(&message.preview, needle)
                || contains(&message.body_text, needle)
                || message
                    .categories
                    .iter()
                    .any(|category| contains(category, needle))
        }
        Filter::From(needle) => from(needle),
        Filter::To(needle) => to(needle),
        Filter::Subject(needle) => contains(&message.subject, needle),
        Filter::Body(needle) => {
            contains(&message.body_text, needle) || contains(&message.preview, needle)
        }
        Filter::HasAttachment => !message.attachments.is_empty(),
        Filter::Is(Status::Unread) => !message.flags.read,
        Filter::Is(Status::Read) => message.flags.read,
        Filter::Is(Status::Flagged) => message.flags.flagged,
        Filter::Is(Status::Pinned) => message.flags.pinned,
        Filter::Is(Status::Important) => message.importance == Importance::High,
        Filter::After(day) => message.date >= start_of(day),
        Filter::Before(day) => message.date < start_of(day),
        Filter::In(name) => folders
            .get(&message.folder_id)
            .is_some_and(|(folder, role)| folder.contains(name.as_str()) || *role == name.as_str()),
        Filter::AllFolders => true,
        Filter::FolderId(id) => message.folder_id == *id,
        Filter::Category(needle) => message
            .categories
            .iter()
            .any(|category| contains(category, needle)),
        Filter::Importance(importance) => message.importance == *importance,
    }
}

/// Every folder of every account, for `in:` matching.
pub fn folder_names(store: &Store) -> Result<FolderNames> {
    let mut names = HashMap::new();
    for account in store.list_accounts()? {
        for folder in store.list_folders(account.id)? {
            names.insert(
                folder.id,
                (folder.name.to_lowercase(), folder.role.as_str()),
            );
        }
    }
    Ok(names)
}

/// Runs `query`, newest first, at most `limit` results. An empty query returns nothing.
pub fn search(store: &Store, query: &SearchQuery, limit: usize) -> Result<Vec<Message>> {
    if query.is_empty() {
        return Ok(Vec::new());
    }
    // A single positive folder id narrows the SQL scan; everything else filters in memory.
    let folder = query.terms.iter().find_map(|term| match term.filter {
        Filter::FolderId(id) if !term.negated => Some(id),
        _ => None,
    });
    let candidates: Vec<Message> = match folder {
        Some(id) => store.list_messages(id)?,
        None => store.query("ORDER BY date DESC, id DESC", [])?,
    };
    let folders = folder_names(store)?;
    Ok(candidates
        .into_iter()
        .filter(|message| query.matches(message, &folders))
        .take(limit)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Address, FolderRole};
    use crate::store::tests::{account, folder, message, store};
    use chrono::{TimeZone, Utc};

    fn parse(input: &str) -> Vec<Term> {
        SearchQuery::parse(input).terms
    }

    fn term(filter: Filter) -> Term {
        Term {
            negated: false,
            filter,
        }
    }

    #[test]
    fn parses_every_operator() {
        let terms = parse(
            r#"Budget "Q3 plan" from:Ada to:bob subject:report body:invoice has:attachment is:unread is:flagged -draft after:2026-10-01 before:2026-11-01 in:Inbox category:work importance:high folder:7"#,
        );
        assert_eq!(
            terms,
            vec![
                term(Filter::Text("budget".into())),
                term(Filter::Text("q3 plan".into())),
                term(Filter::From("ada".into())),
                term(Filter::To("bob".into())),
                term(Filter::Subject("report".into())),
                term(Filter::Body("invoice".into())),
                term(Filter::HasAttachment),
                term(Filter::Is(Status::Unread)),
                term(Filter::Is(Status::Flagged)),
                Term {
                    negated: true,
                    filter: Filter::Text("draft".into())
                },
                term(Filter::After(NaiveDate::from_ymd_opt(2026, 10, 1).unwrap())),
                term(Filter::Before(
                    NaiveDate::from_ymd_opt(2026, 11, 1).unwrap()
                )),
                term(Filter::In("inbox".into())),
                term(Filter::Category("work".into())),
                term(Filter::Importance(Importance::High)),
                term(Filter::FolderId(7)),
            ]
        );
    }

    #[test]
    fn incomplete_and_unknown_terms_are_lenient() {
        assert!(SearchQuery::parse("from: to: after:2026-1 is:whatever").is_empty());
        assert_eq!(
            parse("http://x"),
            vec![term(Filter::Text("http://x".into()))]
        );
        assert_eq!(parse("-"), vec![term(Filter::Text("-".into()))]);
        assert_eq!(
            parse("subject:\"big news\""),
            vec![term(Filter::Subject("big news".into()))]
        );
        assert!(SearchQuery::parse("in:all").has_folder_filter());
        assert!(!SearchQuery::parse("ada").has_folder_filter());
    }

    #[test]
    fn executes_against_store() {
        let store = store();
        let account = account(&store);
        let inbox = folder(&store, account.id, FolderRole::Inbox);
        let archive = folder(&store, account.id, FolderRole::Archive);

        let mut first = message(account.id, inbox.id);
        store.save(&mut first).unwrap();

        let mut second = message(account.id, archive.id);
        second.subject = "Quarterly budget".into();
        second.from = Address::new("Bob", "bob@example.com");
        second.attachments.clear();
        second.flags.read = true;
        second.flags.flagged = true;
        second.categories = vec![];
        second.date = Utc.with_ymd_and_hms(2026, 10, 20, 8, 0, 0).unwrap();
        store.save(&mut second).unwrap();

        let ids = |query: &str| {
            search(&store, &SearchQuery::parse(query), 50)
                .unwrap()
                .into_iter()
                .map(|message| message.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids("budget"), vec![second.id]);
        assert_eq!(ids("from:ada"), vec![first.id]);
        assert_eq!(ids("-from:ada"), vec![second.id]);
        assert_eq!(ids("has:attachment"), vec![first.id]);
        assert_eq!(ids("is:unread"), vec![first.id]);
        assert_eq!(ids("is:flagged is:read"), vec![second.id]);
        assert_eq!(ids("after:2026-11-01"), vec![first.id]);
        assert_eq!(ids("before:2026-11-01"), vec![second.id]);
        assert_eq!(ids("in:archive"), vec![second.id]);
        assert_eq!(ids(&format!("folder:{}", inbox.id)), vec![first.id]);
        assert_eq!(ids("category:work"), vec![first.id]);
        assert_eq!(ids("to:me@example"), vec![first.id, second.id]);
        assert_eq!(ids("in:all"), vec![first.id, second.id]);
        assert_eq!(ids("importance:low"), Vec::<Id>::new());
        assert!(ids("").is_empty());
        // The plain store entry point uses the same language.
        assert_eq!(store.search_messages("from:bob", 10).unwrap().len(), 1);
    }
}
