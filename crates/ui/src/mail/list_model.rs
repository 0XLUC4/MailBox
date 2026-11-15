//! Pure message-list model: filter, conversations, pinned-first, sort and grouping into rows.
//! No GPUI here so it is unit-tested directly.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use chrono::{Datelike, Duration, FixedOffset, NaiveDate};
use mailbox_core::{Id, Importance, Message};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    All,
    Unread,
    Flagged,
    Mentions,
    Attachments,
}

impl Filter {
    pub const ALL: [Filter; 5] = [
        Self::All,
        Self::Unread,
        Self::Flagged,
        Self::Mentions,
        Self::Attachments,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Unread => "Unread",
            Self::Flagged => "Flagged",
            Self::Mentions => "Mentions",
            Self::Attachments => "Attachments",
        }
    }

    /// `mentioned`: ids of the messages that mention the user (see [`mentions`]).
    pub fn matches(self, message: &Message, mentioned: &HashSet<Id>) -> bool {
        match self {
            Self::All => true,
            Self::Unread => !message.flags.read,
            Self::Flagged => message.flags.flagged,
            Self::Attachments => !message.attachments.is_empty(),
            Self::Mentions => mentioned.contains(&message.id),
        }
    }
}

/// Whether `body` mentions one of `tokens` (lowercase names) as `@name`.
pub fn mentions(body: &str, tokens: &[String]) -> bool {
    if tokens.is_empty() || !body.contains('@') {
        return false;
    }
    let body = body.to_lowercase();
    tokens
        .iter()
        .any(|token| body.contains(&format!("@{token}")))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    #[default]
    Date,
    Sender,
    Subject,
    Importance,
}

impl SortKey {
    pub const ALL: [SortKey; 4] = [Self::Date, Self::Sender, Self::Subject, Self::Importance];

    pub fn label(self) -> &'static str {
        match self {
            Self::Date => "Date",
            Self::Sender => "From",
            Self::Subject => "Subject",
            Self::Importance => "Importance",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Grouping {
    #[default]
    Date,
    Sender,
    Importance,
    None,
}

impl Grouping {
    pub const ALL: [Grouping; 4] = [Self::Date, Self::Sender, Self::Importance, Self::None];

    pub fn label(self) -> &'static str {
        match self {
            Self::Date => "Date",
            Self::Sender => "Sender",
            Self::Importance => "Importance",
            Self::None => "No grouping",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListOptions {
    pub filter: Filter,
    pub sort: SortKey,
    pub descending: bool,
    pub grouping: Grouping,
    /// Show conversations (one row per thread, expandable).
    pub threaded: bool,
}

impl Default for ListOptions {
    fn default() -> Self {
        Self {
            filter: Filter::All,
            sort: SortKey::Date,
            descending: true,
            grouping: Grouping::Date,
            threaded: true,
        }
    }
}

/// Inputs besides the messages and options.
pub struct BuildContext<'a> {
    pub today: NaiveDate,
    /// Offset used to turn message timestamps into local days.
    pub offset: FixedOffset,
    /// Messages mentioning the user, for [`Filter::Mentions`].
    pub mentioned: &'a HashSet<Id>,
    /// Thread ids the user expanded.
    pub expanded: &'a HashSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Header {
        label: String,
        count: usize,
    },
    Message {
        /// Index into the message slice.
        index: usize,
        /// A message shown under its expanded conversation.
        child: bool,
        /// Messages in the conversation (1 when not threaded).
        thread_len: usize,
        /// Unread messages in the conversation.
        thread_unread: usize,
        expanded: bool,
    },
}

/// Date bucket and its order: Today, Yesterday, This week, Last week, Earlier.
pub fn date_bucket(date: NaiveDate, today: NaiveDate) -> (u8, &'static str) {
    let days = (today - date).num_days();
    let week_start = today - Duration::days(i64::from(today.weekday().num_days_from_monday()));
    if days <= 0 {
        (0, "Today")
    } else if days == 1 {
        (1, "Yesterday")
    } else if date >= week_start {
        (2, "This week")
    } else if date >= week_start - Duration::days(7) {
        (3, "Last week")
    } else {
        (4, "Earlier")
    }
}

/// Subject without reply/forward prefixes, lowercase (sorting).
pub fn base_subject(subject: &str) -> String {
    let mut subject = subject.trim();
    loop {
        let lower = subject.to_ascii_lowercase();
        let prefix = ["re:", "fw:", "fwd:", "tr:"]
            .iter()
            .find(|prefix| lower.starts_with(*prefix));
        match prefix {
            Some(prefix) => subject = subject[prefix.len()..].trim_start(),
            None => return subject.to_lowercase(),
        }
    }
}

fn importance_rank(importance: Importance) -> u8 {
    match importance {
        Importance::High => 0,
        Importance::Normal => 1,
        Importance::Low => 2,
    }
}

/// A conversation (or a single message): newest first.
struct Unit {
    head: usize,
    members: Vec<usize>,
    pinned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct GroupKey {
    rank: u8,
    sort: String,
}

/// Group identity and its display label (the first member's spelling).
fn group_key(
    message: &Message,
    options: &ListOptions,
    ctx: &BuildContext,
) -> Option<(GroupKey, String)> {
    match options.grouping {
        Grouping::None => None,
        Grouping::Date => {
            let date = message.date.with_timezone(&ctx.offset).date_naive();
            let (rank, label) = date_bucket(date, ctx.today);
            let ascending = options.sort == SortKey::Date && !options.descending;
            Some((
                GroupKey {
                    rank: if ascending { 4 - rank } else { rank },
                    sort: String::new(),
                },
                label.to_owned(),
            ))
        }
        Grouping::Sender => Some((
            GroupKey {
                rank: 0,
                sort: message.from.display().to_lowercase(),
            },
            message.from.display().to_owned(),
        )),
        Grouping::Importance => {
            let rank = importance_rank(message.importance);
            let label = match message.importance {
                Importance::High => "High importance",
                Importance::Normal => "Normal importance",
                Importance::Low => "Low importance",
            };
            Some((
                GroupKey {
                    rank,
                    sort: String::new(),
                },
                label.to_owned(),
            ))
        }
    }
}

fn compare(a: &Message, b: &Message, options: &ListOptions) -> Ordering {
    let primary = match options.sort {
        SortKey::Date => a.date.cmp(&b.date),
        SortKey::Sender => a
            .from
            .display()
            .to_lowercase()
            .cmp(&b.from.display().to_lowercase()),
        SortKey::Subject => base_subject(&a.subject).cmp(&base_subject(&b.subject)),
        SortKey::Importance => importance_rank(b.importance).cmp(&importance_rank(a.importance)),
    };
    let primary = if options.descending {
        primary.reverse()
    } else {
        primary
    };
    // Newest first breaks ties.
    primary
        .then_with(|| b.date.cmp(&a.date))
        .then_with(|| b.id.cmp(&a.id))
}

/// Builds the rows shown by the list.
pub fn build_rows(messages: &[Message], options: &ListOptions, ctx: &BuildContext) -> Vec<Row> {
    let newest_first = |a: &usize, b: &usize| {
        messages[*b]
            .date
            .cmp(&messages[*a].date)
            .then_with(|| messages[*b].id.cmp(&messages[*a].id))
    };

    // 1. Filter, then gather conversations (order of first appearance).
    let mut units: Vec<Unit> = Vec::new();
    let mut by_thread: HashMap<&str, usize> = HashMap::new();
    for (index, message) in messages.iter().enumerate() {
        if !options.filter.matches(message, ctx.mentioned) {
            continue;
        }
        let existing = options
            .threaded
            .then(|| by_thread.get(message.thread_id.as_str()).copied())
            .flatten();
        match existing {
            Some(unit) => units[unit].members.push(index),
            None => {
                by_thread.insert(message.thread_id.as_str(), units.len());
                units.push(Unit {
                    head: index,
                    members: vec![index],
                    pinned: false,
                });
            }
        }
    }
    for unit in &mut units {
        unit.members.sort_by(newest_first);
        unit.head = unit.members[0];
        unit.pinned = unit.members.iter().any(|&i| messages[i].flags.pinned);
    }

    // 2. Pinned first, then group, then the chosen sort.
    let keys: Vec<Option<(GroupKey, String)>> = units
        .iter()
        .map(|unit| group_key(&messages[unit.head], options, ctx))
        .collect();
    let mut order: Vec<usize> = (0..units.len()).collect();
    order.sort_by(|&a, &b| {
        units[b]
            .pinned
            .cmp(&units[a].pinned)
            .then_with(|| match units[a].pinned {
                true => Ordering::Equal,
                false => keys[a]
                    .as_ref()
                    .map(|(key, _)| key)
                    .cmp(&keys[b].as_ref().map(|(key, _)| key)),
            })
            .then_with(|| compare(&messages[units[a].head], &messages[units[b].head], options))
    });

    // 3. Emit headers and rows.
    let mut rows = Vec::with_capacity(messages.len() + 8);
    // Identity of the current group: `Some(None)` is "Pinned", `None` is ungrouped.
    let mut current: Option<Option<Option<&GroupKey>>> = None;
    let mut header_index = 0;
    for &unit_index in &order {
        let unit = &units[unit_index];
        let group: Option<(Option<&GroupKey>, &str)> = if unit.pinned {
            Some((None, "Pinned"))
        } else {
            keys[unit_index]
                .as_ref()
                .map(|(key, label)| (Some(key), label.as_str()))
        };
        let identity = group.map(|(key, _)| key);
        if current != Some(identity) {
            if let Some((_, label)) = group {
                header_index = rows.len();
                rows.push(Row::Header {
                    label: label.to_owned(),
                    count: 0,
                });
            }
            current = Some(identity);
        }
        if group.is_some()
            && let Some(Row::Header { count, .. }) = rows.get_mut(header_index)
        {
            *count += 1;
        }
        let thread_id = &messages[unit.head].thread_id;
        let expanded =
            options.threaded && unit.members.len() > 1 && ctx.expanded.contains(thread_id);
        let thread_unread = unit
            .members
            .iter()
            .filter(|&&i| !messages[i].flags.read)
            .count();
        rows.push(Row::Message {
            index: unit.head,
            child: false,
            thread_len: unit.members.len(),
            thread_unread,
            expanded,
        });
        if expanded {
            rows.extend(unit.members.iter().skip(1).map(|&index| Row::Message {
                index,
                child: true,
                thread_len: 1,
                thread_unread: usize::from(!messages[index].flags.read),
                expanded: false,
            }));
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use mailbox_core::{Address, Attachment, MessageFlags};

    fn msg(id: i64, thread: &str, from: &str, day: u32, hour: u32) -> Message {
        Message {
            id,
            account_id: 1,
            folder_id: 1,
            thread_id: thread.into(),
            from: Address::new(from, format!("{}@example.com", from.to_lowercase())),
            to: vec![],
            cc: vec![],
            subject: format!("Subject {id}"),
            date: Utc.with_ymd_and_hms(2026, 11, day, hour, 0, 0).unwrap(),
            preview: String::new(),
            body_text: String::new(),
            body_html: None,
            flags: MessageFlags {
                read: true,
                flagged: false,
                pinned: false,
            },
            importance: Importance::Normal,
            categories: vec![],
            attachments: vec![],
        }
    }

    fn ctx<'a>(expanded: &'a HashSet<String>, mentioned: &'a HashSet<Id>) -> BuildContext<'a> {
        BuildContext {
            // Saturday 14 Nov 2026.
            today: NaiveDate::from_ymd_opt(2026, 11, 14).unwrap(),
            offset: FixedOffset::east_opt(0).unwrap(),
            mentioned,
            expanded,
        }
    }

    fn summary(rows: &[Row], messages: &[Message]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                Row::Header { label, count } => format!("# {label} ({count})"),
                Row::Message {
                    index,
                    child,
                    thread_len,
                    ..
                } => format!(
                    "{}{}x{thread_len}",
                    if *child { "  " } else { "" },
                    messages[*index].id
                ),
            })
            .collect()
    }

    #[test]
    fn date_buckets() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 14).unwrap(); // Saturday
        let day = |d| NaiveDate::from_ymd_opt(2026, 11, d).unwrap();
        assert_eq!(date_bucket(day(14), today).1, "Today");
        assert_eq!(date_bucket(day(13), today).1, "Yesterday");
        assert_eq!(date_bucket(day(9), today).1, "This week");
        assert_eq!(date_bucket(day(2), today).1, "Last week");
        assert_eq!(date_bucket(day(1), today).1, "Earlier");
    }

    #[test]
    fn threads_group_and_pin() {
        let mut messages = vec![
            msg(1, "a", "Ada", 14, 9),
            msg(2, "b", "Bob", 13, 9),
            msg(3, "a", "Ada", 12, 9),
            msg(4, "c", "Cy", 2, 9),
            msg(5, "d", "Dee", 1, 9),
        ];
        messages[4].flags.pinned = true;
        messages[2].flags.read = false;
        let expanded = HashSet::from(["a".to_owned()]);
        let rows = build_rows(
            &messages,
            &ListOptions::default(),
            &ctx(&expanded, &HashSet::new()),
        );
        assert_eq!(
            summary(&rows, &messages),
            vec![
                "# Pinned (1)",
                "5x1",
                "# Today (1)",
                "1x2",
                "  3x1",
                "# Yesterday (1)",
                "2x1",
                "# Last week (1)",
                "4x1",
            ]
        );
        assert!(matches!(
            rows[3],
            Row::Message {
                thread_unread: 1,
                expanded: true,
                ..
            }
        ));

        let flat = ListOptions {
            threaded: false,
            grouping: Grouping::None,
            descending: false,
            ..ListOptions::default()
        };
        let rows = build_rows(&messages, &flat, &ctx(&expanded, &HashSet::new()));
        assert_eq!(
            summary(&rows, &messages),
            vec!["# Pinned (1)", "5x1", "4x1", "3x1", "2x1", "1x1"]
        );
    }

    #[test]
    fn filters_and_sender_grouping() {
        let mut messages = vec![
            msg(1, "a", "bob", 14, 9),
            msg(2, "b", "Ada", 13, 9),
            msg(3, "c", "Bob", 12, 9),
        ];
        messages[0].body_text = "Thanks @alex for the notes".into();
        messages[1].attachments.push(Attachment {
            name: "a.pdf".into(),
            mime: "application/pdf".into(),
            size: 1,
        });
        messages[2].flags.flagged = true;
        let none = HashSet::new();
        let tokens = vec!["alex".to_owned()];
        let mentioned: HashSet<Id> = messages
            .iter()
            .filter(|message| mentions(&message.body_text, &tokens))
            .map(|message| message.id)
            .collect();
        let only = |filter| {
            let options = ListOptions {
                filter,
                grouping: Grouping::None,
                ..ListOptions::default()
            };
            summary(
                &build_rows(&messages, &options, &ctx(&none, &mentioned)),
                &messages,
            )
        };
        assert_eq!(only(Filter::Mentions), vec!["1x1"]);
        assert_eq!(only(Filter::Attachments), vec!["2x1"]);
        assert_eq!(only(Filter::Flagged), vec!["3x1"]);
        assert_eq!(only(Filter::Unread), Vec::<String>::new());

        let by_sender = ListOptions {
            grouping: Grouping::Sender,
            ..ListOptions::default()
        };
        let rows = build_rows(&messages, &by_sender, &ctx(&none, &mentioned));
        assert_eq!(
            summary(&rows, &messages),
            vec!["# Ada (1)", "2x1", "# bob (2)", "1x1", "3x1"]
        );
    }

    #[test]
    fn mentions_need_the_at_sign() {
        let tokens = vec!["alex".to_owned()];
        assert!(mentions("Thanks @Alex!", &tokens));
        assert!(!mentions("Thanks Alex", &tokens));
        assert!(!mentions("@alex", &[]));
    }

    #[test]
    fn subject_prefixes_are_ignored() {
        assert_eq!(base_subject("RE: Fwd: Budget"), "budget");
        assert_eq!(base_subject("Budget"), "budget");
    }
}
