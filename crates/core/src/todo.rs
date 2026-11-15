//! Tasks beyond the base [`Task`] row: custom lists, My Day, reminders, repeat, steps and the
//! link to the flagged message a task came from, plus smart-list membership, sorting and the
//! [`TaskCommand`]s that change them. Run them as `Command::Todo(..)` for undo/redo.
//!
//! Details live in `task_details`, one row per task with the same id (deleted with the task).
//! A task without a details row reads as empty details (in the default "Tasks" list).

use std::cmp::Ordering;
use std::collections::HashMap;

use chrono::{DateTime, Datelike, Days, Local, Months, NaiveDate, SecondsFormat, Utc, Weekday};
use rusqlite::Row;
use rusqlite::types::Value;
use serde::{Deserialize, Serialize};

use crate::command::Command;
use crate::error::{Error, Result};
use crate::model::{Color, Id, Importance, Message, Task};
use crate::store::{Record, Store};

/// Schema for this module, appended to `store::MIGRATIONS`.
pub(crate) const MIGRATION: &str = r#"
CREATE TABLE task_lists (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    color INTEGER NOT NULL,
    position INTEGER NOT NULL
);
CREATE TABLE task_details (
    id INTEGER PRIMARY KEY REFERENCES tasks(id) ON DELETE CASCADE,
    list_id INTEGER,
    my_day TEXT,
    reminder TEXT,
    repeat TEXT,
    steps_json TEXT NOT NULL,
    message_id INTEGER,
    completed_at TEXT
);
"#;

/// Manual order of tasks (drag & drop), appended to `store::MIGRATIONS`.
pub(crate) const POSITION_MIGRATION: &str =
    "ALTER TABLE task_details ADD COLUMN position INTEGER NOT NULL DEFAULT 0;";

/// How a task recurs; completing it creates the next occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Repeat {
    Daily,
    Weekdays,
    Weekly,
    Monthly,
    Yearly,
}

impl Repeat {
    pub const ALL: [Repeat; 5] = [
        Self::Daily,
        Self::Weekdays,
        Self::Weekly,
        Self::Monthly,
        Self::Yearly,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Weekdays => "weekdays",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
            Self::Yearly => "yearly",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|repeat| repeat.as_str() == value)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Daily => "Daily",
            Self::Weekdays => "Weekdays",
            Self::Weekly => "Weekly",
            Self::Monthly => "Monthly",
            Self::Yearly => "Yearly",
        }
    }

    /// The next occurrence after `date` (month ends are clamped: Jan 31 -> Feb 28).
    pub fn next(self, date: NaiveDate) -> Option<NaiveDate> {
        match self {
            Self::Daily => date.checked_add_days(Days::new(1)),
            Self::Weekdays => {
                let skip = match date.weekday() {
                    Weekday::Fri => 3,
                    Weekday::Sat => 2,
                    _ => 1,
                };
                date.checked_add_days(Days::new(skip))
            }
            Self::Weekly => date.checked_add_days(Days::new(7)),
            Self::Monthly => date.checked_add_months(Months::new(1)),
            Self::Yearly => date.checked_add_months(Months::new(12)),
        }
    }
}

/// A sub-task.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Step {
    pub title: String,
    pub done: bool,
}

/// A user-created task list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskList {
    pub id: Id,
    pub name: String,
    pub color: Color,
    /// Display order.
    pub position: i64,
}

/// Extra task fields, keyed by the task id.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TaskDetails {
    /// Same id as the task.
    pub id: Id,
    /// Custom list; `None` is the default "Tasks" list.
    pub list_id: Option<Id>,
    /// Day the task was added to My Day (it shows there only on that day).
    pub my_day: Option<NaiveDate>,
    pub reminder: Option<DateTime<Utc>>,
    pub repeat: Option<Repeat>,
    pub steps: Vec<Step>,
    /// The flagged message this task was created from.
    pub message_id: Option<Id>,
    pub completed_at: Option<DateTime<Utc>>,
    /// Manual order (drag & drop) used by the default sort; lower first.
    #[serde(default)]
    pub position: i64,
}

/// A task with its details: what the Tasks module reads and writes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskCard {
    pub task: Task,
    pub details: TaskDetails,
}

impl TaskCard {
    /// An unsaved, open task.
    pub fn new(account_id: Id, title: impl Into<String>, created: DateTime<Utc>) -> Self {
        Self {
            task: Task {
                id: 0,
                account_id,
                title: title.into(),
                notes: String::new(),
                due: None,
                completed: false,
                importance: Importance::Normal,
                categories: Vec::new(),
                created,
            },
            details: TaskDetails::default(),
        }
    }

    pub fn id(&self) -> Id {
        self.task.id
    }

    pub fn is_important(&self) -> bool {
        self.task.importance == Importance::High
    }

    pub fn in_my_day(&self, today: NaiveDate) -> bool {
        self.details.my_day == Some(today)
    }

    pub fn is_overdue(&self, today: NaiveDate) -> bool {
        !self.task.completed && self.task.due.is_some_and(|due| due < today)
    }

    /// Whether the task belongs to `list` on `today`.
    pub fn is_in(&self, list: SmartList, today: NaiveDate) -> bool {
        match list {
            SmartList::MyDay => self.in_my_day(today),
            SmartList::Important => self.is_important(),
            SmartList::Planned => self.task.due.is_some() || self.details.reminder.is_some(),
            SmartList::Flagged => self.details.message_id.is_some(),
            SmartList::Tasks => self.details.list_id.is_none(),
            SmartList::List(id) => self.details.list_id == Some(id),
        }
    }

    /// Steps done / total, `None` without steps.
    pub fn step_progress(&self) -> Option<(usize, usize)> {
        let steps = &self.details.steps;
        (!steps.is_empty()).then(|| (steps.iter().filter(|step| step.done).count(), steps.len()))
    }

    /// The occurrence that follows this repeating task, unsaved and open.
    pub fn next_occurrence(&self, today: NaiveDate, now: DateTime<Utc>) -> Option<TaskCard> {
        let repeat = self.details.repeat?;
        let base = self.task.due.unwrap_or(today);
        let next = repeat.next(base)?;
        let days = Days::new(u64::try_from((next - base).num_days()).ok()?);
        let mut card = self.clone();
        card.task.id = 0;
        card.task.completed = false;
        card.task.due = Some(next);
        card.task.created = now;
        card.details.id = 0;
        card.details.my_day = None;
        card.details.completed_at = None;
        // Only the first occurrence comes from the flagged email.
        card.details.message_id = None;
        // Same local clock time on the new day, across daylight-saving changes.
        card.details.reminder = self.details.reminder.and_then(|reminder| {
            let local = reminder
                .with_timezone(&Local)
                .naive_local()
                .checked_add_days(days)?;
            Some(
                local
                    .and_local_timezone(Local)
                    .earliest()?
                    .with_timezone(&Utc),
            )
        });
        for step in &mut card.details.steps {
            step.done = false;
        }
        Some(card)
    }
}

/// Lists shown in the Tasks sidebar. `Flagged` also shows flagged messages without a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SmartList {
    MyDay,
    Important,
    Planned,
    Flagged,
    Tasks,
    List(Id),
}

impl SmartList {
    /// Built-in lists in sidebar order.
    pub const BUILT_IN: [SmartList; 5] = [
        Self::MyDay,
        Self::Important,
        Self::Planned,
        Self::Flagged,
        Self::Tasks,
    ];

    /// Name of a built-in list (custom lists use their own name).
    pub fn label(self) -> &'static str {
        match self {
            Self::MyDay => "My Day",
            Self::Important => "Important",
            Self::Planned => "Planned",
            Self::Flagged => "Flagged email",
            Self::Tasks => "Tasks",
            Self::List(_) => "List",
        }
    }

    /// A new task added while viewing this list, with the list's implied fields set.
    pub fn new_task(
        self,
        account_id: Id,
        title: &str,
        today: NaiveDate,
        now: DateTime<Utc>,
    ) -> TaskCard {
        let mut card = TaskCard::new(account_id, title.trim(), now);
        match self {
            Self::MyDay => card.details.my_day = Some(today),
            Self::Important => card.task.importance = Importance::High,
            Self::Planned => card.task.due = Some(today),
            Self::List(id) => card.details.list_id = Some(id),
            Self::Flagged | Self::Tasks => {}
        }
        card
    }
}

/// Sort orders offered by the list header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TaskSort {
    /// Manual order (drag & drop), then oldest first.
    #[default]
    Created,
    DueDate,
    Importance,
    Alphabetical,
}

impl TaskSort {
    pub const ALL: [TaskSort; 4] = [
        Self::Created,
        Self::DueDate,
        Self::Importance,
        Self::Alphabetical,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Created => "Creation date",
            Self::DueDate => "Due date",
            Self::Importance => "Importance",
            Self::Alphabetical => "Alphabetically",
        }
    }
}

/// Stable sort; ties keep creation order.
pub fn sort_tasks(cards: &mut [TaskCard], sort: TaskSort) {
    let by_created = |a: &TaskCard, b: &TaskCard| {
        a.task
            .created
            .cmp(&b.task.created)
            .then(a.id().cmp(&b.id()))
    };
    let by_position = |a: &TaskCard, b: &TaskCard| {
        a.details
            .position
            .cmp(&b.details.position)
            .then(by_created(a, b))
    };
    let by_due = |a: &TaskCard, b: &TaskCard| match (a.task.due, b.task.due) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    };
    match sort {
        TaskSort::Created => cards.sort_by(by_position),
        TaskSort::DueDate => cards.sort_by(|a, b| by_due(a, b).then(by_created(a, b))),
        TaskSort::Importance => cards.sort_by(|a, b| {
            b.is_important()
                .cmp(&a.is_important())
                .then(by_due(a, b))
                .then(by_created(a, b))
        }),
        TaskSort::Alphabetical => cards.sort_by(|a, b| {
            a.task
                .title
                .to_lowercase()
                .cmp(&b.task.title.to_lowercase())
                .then(by_created(a, b))
        }),
    }
}

/// Due-date buckets for "group by due date".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DueBucket {
    Overdue,
    Today,
    Tomorrow,
    ThisWeek,
    Later,
    NoDate,
}

impl DueBucket {
    pub fn of(due: Option<NaiveDate>, today: NaiveDate) -> Self {
        let Some(due) = due else {
            return Self::NoDate;
        };
        match (due - today).num_days() {
            days if days < 0 => Self::Overdue,
            0 => Self::Today,
            1 => Self::Tomorrow,
            2..=6 => Self::ThisWeek,
            _ => Self::Later,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Overdue => "Overdue",
            Self::Today => "Today",
            Self::Tomorrow => "Tomorrow",
            Self::ThisWeek => "This week",
            Self::Later => "Later",
            Self::NoDate => "No date",
        }
    }
}

/// A task for a message ("Flagged email" -> real task linked to the message).
pub fn task_from_message(message: &Message, now: DateTime<Utc>) -> TaskCard {
    let subject = message.subject.trim();
    let title = if subject.is_empty() {
        "(no subject)"
    } else {
        subject
    };
    // Kept on this computer (like other tasks) so removing the mail account keeps it.
    let mut card = TaskCard::new(crate::calendar::LOCAL_ACCOUNT, title, now);
    card.task.notes = format!("From {}", message.from.display());
    card.task.categories = message.categories.clone();
    card.details.message_id = Some(message.id);
    card
}

// ---------- queries ----------

/// Every task with its details, in store order (open first, then by due date).
pub fn list_task_cards(store: &Store) -> Result<Vec<TaskCard>> {
    let mut details: HashMap<Id, TaskDetails> = store
        .query::<TaskDetails>("ORDER BY id", [])?
        .into_iter()
        .map(|details| (details.id, details))
        .collect();
    Ok(store
        .list_tasks()?
        .into_iter()
        .map(|task| {
            let details = details.remove(&task.id).unwrap_or(TaskDetails {
                id: task.id,
                ..Default::default()
            });
            TaskCard { task, details }
        })
        .collect())
}

pub fn task_card(store: &Store, id: Id) -> Result<Option<TaskCard>> {
    let Some(task) = store.get::<Task>(id)? else {
        return Ok(None);
    };
    let details = store.get::<TaskDetails>(id)?.unwrap_or(TaskDetails {
        id,
        ..Default::default()
    });
    Ok(Some(TaskCard { task, details }))
}

fn require_task(store: &Store, id: Id) -> Result<TaskCard> {
    task_card(store, id)?.ok_or(Error::NotFound { kind: "task", id })
}

pub fn list_task_lists(store: &Store) -> Result<Vec<TaskList>> {
    store.query("ORDER BY position, id", [])
}

/// Flagged messages, newest first (the "Flagged email" list).
pub fn flagged_messages(store: &Store) -> Result<Vec<Message>> {
    store.query("WHERE is_flagged = 1 ORDER BY date DESC, id DESC", [])
}

// ---------- commands ----------

/// Task mutations. Wrap in `Command::Todo` and run through `History::run`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "args", rename_all = "snake_case")]
pub enum TaskCommand {
    /// Creates (id 0) or replaces a task and its details.
    SaveTask(Box<TaskCard>),
    DeleteTasks(Vec<Id>),
    /// Completing a repeating task also creates its next occurrence.
    SetCompleted {
        id: Id,
        completed: bool,
    },
    SaveList(Box<TaskList>),
    /// Deletes the list and its tasks.
    DeleteList(Id),
}

impl TaskCommand {
    pub fn label(&self) -> String {
        match self {
            Self::SaveTask(card) if card.task.id == 0 => "Add task".into(),
            Self::SaveTask(_) => "Edit task".into(),
            Self::DeleteTasks(ids) if ids.len() == 1 => "Delete task".into(),
            Self::DeleteTasks(ids) => format!("Delete {} tasks", ids.len()),
            Self::SetCompleted {
                completed: true, ..
            } => "Complete task".into(),
            Self::SetCompleted {
                completed: false, ..
            } => "Reopen task".into(),
            Self::SaveList(list) if list.id == 0 => "Create list".into(),
            Self::SaveList(_) => "Edit list".into(),
            Self::DeleteList(_) => "Delete list".into(),
        }
    }
}

fn wrap(command: TaskCommand) -> Command {
    Command::Todo(command)
}

/// Applies `command` inside the caller's transaction and returns its inverse.
pub(crate) fn apply(store: &Store, command: TaskCommand) -> Result<Command> {
    match command {
        TaskCommand::SaveTask(card) => {
            let mut card = *card;
            let previous = if card.id() == 0 {
                None
            } else {
                task_card(store, card.id())?
            };
            let id = write_task(store, &mut card)?;
            Ok(wrap(match previous {
                Some(old) => TaskCommand::SaveTask(Box::new(old)),
                None => TaskCommand::DeleteTasks(vec![id]),
            }))
        }
        TaskCommand::DeleteTasks(ids) => {
            let inverses = ids
                .iter()
                .map(|&id| {
                    let card = require_task(store, id)?;
                    store.delete::<Task>(id)?;
                    Ok(wrap(TaskCommand::SaveTask(Box::new(card))))
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(Command::Batch(inverses))
        }
        TaskCommand::SetCompleted { id, completed } => {
            let mut card = require_task(store, id)?;
            if card.task.completed == completed {
                return Ok(Command::Batch(Vec::new()));
            }
            let previous = card.clone();
            let now = Utc::now();
            card.task.completed = completed;
            card.details.completed_at = completed.then_some(now);
            write_task(store, &mut card)?;
            let mut inverses = Vec::new();
            let next = completed
                .then(|| previous.next_occurrence(Local::now().date_naive(), now))
                .flatten();
            // Complete, reopen, complete: the occurrence created the first time is reused.
            let next = match next {
                Some(next) => {
                    let exists = list_task_cards(store)?.iter().any(|card| {
                        !card.task.completed
                            && card.task.id != id
                            && card.task.title == next.task.title
                            && card.task.due == next.task.due
                            && card.details.list_id == next.details.list_id
                    });
                    (!exists).then_some(next)
                }
                None => None,
            };
            if let Some(mut next) = next {
                let next_id = write_task(store, &mut next)?;
                inverses.push(wrap(TaskCommand::DeleteTasks(vec![next_id])));
            }
            inverses.push(wrap(TaskCommand::SaveTask(Box::new(previous))));
            Ok(Command::Batch(inverses))
        }
        TaskCommand::SaveList(list) => {
            let mut list = *list;
            list.name = list.name.trim().to_owned();
            if list.name.is_empty() {
                return Err(Error::Invalid("a list needs a name".into()));
            }
            let previous = if list.id == 0 {
                None
            } else {
                store.get::<TaskList>(list.id)?
            };
            if list.id == 0 && list.position == 0 {
                list.position = list_task_lists(store)?
                    .iter()
                    .map(|list| list.position + 1)
                    .max()
                    .unwrap_or(0);
            }
            let id = store.save(&mut list)?;
            Ok(wrap(match previous {
                Some(old) => TaskCommand::SaveList(Box::new(old)),
                None => TaskCommand::DeleteList(id),
            }))
        }
        TaskCommand::DeleteList(id) => {
            let list = store.require::<TaskList>(id)?;
            let mut inverses = vec![wrap(TaskCommand::SaveList(Box::new(list)))];
            for card in list_task_cards(store)? {
                if card.details.list_id == Some(id) {
                    store.delete::<Task>(card.id())?;
                    inverses.push(wrap(TaskCommand::SaveTask(Box::new(card))));
                }
            }
            store.delete::<TaskList>(id)?;
            Ok(Command::Batch(inverses))
        }
    }
}

/// Saves task + details (trimmed title required). Returns the task id.
fn write_task(store: &Store, card: &mut TaskCard) -> Result<Id> {
    card.task.title = card.task.title.trim().to_owned();
    if card.task.title.is_empty() {
        return Err(Error::Invalid("a task needs a title".into()));
    }
    card.details
        .steps
        .retain(|step| !step.title.trim().is_empty());
    let id = store.save(&mut card.task)?;
    card.details.id = id;
    store.save(&mut card.details)?;
    Ok(id)
}

// ---------- persistence ----------

fn conversion_error(
    index: usize,
    err: impl std::error::Error + Send + Sync + 'static,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(err))
}

fn date_value(date: Option<NaiveDate>) -> Value {
    date.map_or(Value::Null, |date| {
        Value::Text(date.format("%Y-%m-%d").to_string())
    })
}

fn time_value(time: Option<DateTime<Utc>>) -> Value {
    time.map_or(Value::Null, |time| {
        Value::Text(time.to_rfc3339_opts(SecondsFormat::Secs, true))
    })
}

fn get_date(row: &Row, index: usize) -> rusqlite::Result<Option<NaiveDate>> {
    let raw: Option<String> = row.get(index)?;
    raw.map(|raw| NaiveDate::parse_from_str(&raw, "%Y-%m-%d"))
        .transpose()
        .map_err(|err| conversion_error(index, err))
}

fn get_time(row: &Row, index: usize) -> rusqlite::Result<Option<DateTime<Utc>>> {
    let raw: Option<String> = row.get(index)?;
    raw.map(|raw| DateTime::parse_from_rfc3339(&raw).map(|time| time.with_timezone(&Utc)))
        .transpose()
        .map_err(|err| conversion_error(index, err))
}

impl Record for TaskList {
    const KIND: &'static str = "task list";
    const TABLE: &'static str = "task_lists";
    const COLUMNS: &'static [&'static str] = &["name", "color", "position"];

    fn id(&self) -> Id {
        self.id
    }
    fn set_id(&mut self, id: Id) {
        self.id = id;
    }
    fn values(&self) -> Result<Vec<Value>> {
        Ok(vec![
            Value::Text(self.name.clone()),
            Value::Integer(i64::from(self.color)),
            Value::Integer(self.position),
        ])
    }
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            name: row.get(1)?,
            color: row.get(2)?,
            position: row.get(3)?,
        })
    }
}

impl Record for TaskDetails {
    const KIND: &'static str = "task details";
    const TABLE: &'static str = "task_details";
    const COLUMNS: &'static [&'static str] = &[
        "list_id",
        "my_day",
        "reminder",
        "repeat",
        "steps_json",
        "message_id",
        "completed_at",
        "position",
    ];

    fn id(&self) -> Id {
        self.id
    }
    fn set_id(&mut self, id: Id) {
        self.id = id;
    }
    fn values(&self) -> Result<Vec<Value>> {
        Ok(vec![
            self.list_id.map_or(Value::Null, Value::Integer),
            date_value(self.my_day),
            time_value(self.reminder),
            self.repeat.map_or(Value::Null, |repeat| {
                Value::Text(repeat.as_str().to_owned())
            }),
            Value::Text(serde_json::to_string(&self.steps)?),
            self.message_id.map_or(Value::Null, Value::Integer),
            time_value(self.completed_at),
            Value::Integer(self.position),
        ])
    }
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        let repeat: Option<String> = row.get(4)?;
        let steps: String = row.get(5)?;
        Ok(Self {
            id: row.get(0)?,
            list_id: row.get(1)?,
            my_day: get_date(row, 2)?,
            reminder: get_time(row, 3)?,
            repeat: repeat.as_deref().and_then(Repeat::parse),
            steps: serde_json::from_str(&steps).map_err(|err| conversion_error(5, err))?,
            message_id: row.get(6)?,
            completed_at: get_time(row, 7)?,
            position: row.get(8)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::History;
    use crate::model::{Account, FolderRole};
    use crate::store::tests::{account, folder, message, store};
    use chrono::TimeZone;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 14, 9, 0, 0).unwrap()
    }

    fn run(store: &Store, history: &mut History, command: TaskCommand) {
        history.run(store, Command::Todo(command)).unwrap();
    }

    fn add(store: &Store, history: &mut History, card: TaskCard) -> TaskCard {
        run(store, history, TaskCommand::SaveTask(Box::new(card)));
        list_task_cards(store)
            .unwrap()
            .into_iter()
            .max_by_key(TaskCard::id)
            .unwrap()
    }

    fn rich_task(account: &Account) -> TaskCard {
        let mut card = TaskCard::new(account.id, "  Write report ", now());
        card.task.due = Some(date(2026, 11, 20));
        card.task.importance = Importance::High;
        card.details = TaskDetails {
            id: 0,
            list_id: None,
            my_day: Some(date(2026, 11, 14)),
            reminder: Some(Utc.with_ymd_and_hms(2026, 11, 20, 8, 0, 0).unwrap()),
            repeat: Some(Repeat::Weekly),
            steps: vec![
                Step {
                    title: "Outline".into(),
                    done: true,
                },
                Step {
                    title: "  ".into(),
                    done: false,
                },
            ],
            message_id: None,
            completed_at: None,
            position: 0,
        };
        card
    }

    #[test]
    fn save_edit_delete_undo_redo() {
        let store = store();
        let account = account(&store);
        let mut history = History::new();
        let created = add(&store, &mut history, rich_task(&account));
        assert_eq!(created.task.title, "Write report");
        assert_eq!(created.details.steps.len(), 1, "blank steps are dropped");
        assert_eq!(created.details.id, created.id());
        assert_eq!(history.undo_label(), Some("Add task"));

        let mut edited = created.clone();
        edited.details.steps.push(Step {
            title: "Send".into(),
            done: false,
        });
        edited.task.notes = "Q4 numbers".into();
        run(
            &store,
            &mut history,
            TaskCommand::SaveTask(Box::new(edited.clone())),
        );
        assert_eq!(task_card(&store, created.id()).unwrap(), Some(edited));

        run(
            &store,
            &mut history,
            TaskCommand::DeleteTasks(vec![created.id()]),
        );
        assert!(task_card(&store, created.id()).unwrap().is_none());
        assert!(store.get::<TaskDetails>(created.id()).unwrap().is_none());

        history.undo(&store).unwrap();
        history.undo(&store).unwrap();
        assert_eq!(
            task_card(&store, created.id()).unwrap(),
            Some(created.clone())
        );
        history.undo(&store).unwrap();
        assert!(list_task_cards(&store).unwrap().is_empty());
        history.redo(&store).unwrap();
        assert_eq!(task_card(&store, created.id()).unwrap(), Some(created));
    }

    #[test]
    fn blank_title_is_rejected() {
        let store = store();
        let account = account(&store);
        let mut history = History::new();
        let card = TaskCard::new(account.id, "   ", now());
        assert!(
            history
                .run(&store, Command::Todo(TaskCommand::SaveTask(Box::new(card))))
                .is_err()
        );
    }

    #[test]
    fn completing_a_repeating_task_spawns_the_next_one() {
        let store = store();
        let account = account(&store);
        let mut history = History::new();
        let created = add(&store, &mut history, rich_task(&account));

        run(
            &store,
            &mut history,
            TaskCommand::SetCompleted {
                id: created.id(),
                completed: true,
            },
        );
        let cards = list_task_cards(&store).unwrap();
        assert_eq!(cards.len(), 2);
        let done = cards.iter().find(|card| card.id() == created.id()).unwrap();
        assert!(done.task.completed);
        assert!(done.details.completed_at.is_some());
        let next = cards.iter().find(|card| card.id() != created.id()).unwrap();
        assert!(!next.task.completed);
        assert_eq!(next.task.due, Some(date(2026, 11, 27)));
        assert_eq!(
            next.details.reminder,
            Some(Utc.with_ymd_and_hms(2026, 11, 27, 8, 0, 0).unwrap())
        );
        assert!(next.details.steps.iter().all(|step| !step.done));
        assert_eq!(next.details.my_day, None);

        history.undo(&store).unwrap();
        assert_eq!(list_task_cards(&store).unwrap(), vec![created.clone()]);

        // Complete, reopen, complete: still one next occurrence.
        for completed in [true, false, true] {
            run(
                &store,
                &mut history,
                TaskCommand::SetCompleted {
                    id: created.id(),
                    completed,
                },
            );
        }
        assert_eq!(list_task_cards(&store).unwrap().len(), 2);
        for _ in 0..3 {
            history.undo(&store).unwrap();
        }
        assert_eq!(list_task_cards(&store).unwrap(), vec![created.clone()]);

        // Reopening a plain task does not spawn anything.
        let mut plain = created.clone();
        plain.details.repeat = None;
        run(&store, &mut history, TaskCommand::SaveTask(Box::new(plain)));
        for completed in [true, false] {
            run(
                &store,
                &mut history,
                TaskCommand::SetCompleted {
                    id: created.id(),
                    completed,
                },
            );
        }
        let cards = list_task_cards(&store).unwrap();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].details.completed_at, None);
    }

    #[test]
    fn deleting_a_list_deletes_its_tasks_and_undo_restores() {
        let store = store();
        let account = account(&store);
        let mut history = History::new();
        for name in ["Groceries", "Trips"] {
            run(
                &store,
                &mut history,
                TaskCommand::SaveList(Box::new(TaskList {
                    id: 0,
                    name: name.into(),
                    color: 0x22c55e,
                    position: 0,
                })),
            );
        }
        let lists = list_task_lists(&store).unwrap();
        assert_eq!(
            lists
                .iter()
                .map(|l| (l.name.as_str(), l.position))
                .collect::<Vec<_>>(),
            vec![("Groceries", 0), ("Trips", 1)]
        );
        let groceries = lists[0].id;
        let milk =
            SmartList::List(groceries).new_task(account.id, "Milk", date(2026, 11, 14), now());
        let milk = add(&store, &mut history, milk);
        let other = add(
            &store,
            &mut history,
            TaskCard::new(account.id, "Other", now()),
        );

        run(&store, &mut history, TaskCommand::DeleteList(groceries));
        assert_eq!(list_task_lists(&store).unwrap().len(), 1);
        assert_eq!(list_task_cards(&store).unwrap(), vec![other.clone()]);

        history.undo(&store).unwrap();
        assert_eq!(list_task_lists(&store).unwrap(), lists);
        let mut cards = list_task_cards(&store).unwrap();
        cards.sort_by_key(TaskCard::id);
        assert_eq!(cards, vec![milk, other]);
    }

    #[test]
    fn smart_lists_membership_and_new_tasks() {
        let today = date(2026, 11, 14);
        let mut card = TaskCard::new(1, "A", now());
        assert!(card.is_in(SmartList::Tasks, today));
        assert!(!card.is_in(SmartList::MyDay, today));
        card.details.my_day = Some(today);
        assert!(card.is_in(SmartList::MyDay, today));
        assert!(
            !card.is_in(SmartList::MyDay, date(2026, 11, 15)),
            "My Day resets daily"
        );
        card.task.due = Some(date(2026, 11, 13));
        assert!(card.is_in(SmartList::Planned, today));
        assert!(card.is_overdue(today));
        card.details.list_id = Some(4);
        assert!(card.is_in(SmartList::List(4), today));
        assert!(!card.is_in(SmartList::Tasks, today));

        let important = SmartList::Important.new_task(1, " Call ", today, now());
        assert!(important.is_important());
        assert_eq!(important.task.title, "Call");
        assert_eq!(
            SmartList::Planned.new_task(1, "x", today, now()).task.due,
            Some(today)
        );
        assert!(
            SmartList::MyDay
                .new_task(1, "x", today, now())
                .in_my_day(today)
        );
    }

    #[test]
    fn repeat_rules() {
        assert_eq!(
            Repeat::Daily.next(date(2026, 12, 31)),
            Some(date(2027, 1, 1))
        );
        // 2026-11-13 is a Friday.
        assert_eq!(
            Repeat::Weekdays.next(date(2026, 11, 13)),
            Some(date(2026, 11, 16))
        );
        assert_eq!(
            Repeat::Weekdays.next(date(2026, 11, 14)),
            Some(date(2026, 11, 16))
        );
        assert_eq!(
            Repeat::Weekdays.next(date(2026, 11, 16)),
            Some(date(2026, 11, 17))
        );
        assert_eq!(
            Repeat::Monthly.next(date(2026, 1, 31)),
            Some(date(2026, 2, 28))
        );
        assert_eq!(
            Repeat::Yearly.next(date(2028, 2, 29)),
            Some(date(2029, 2, 28))
        );
        for repeat in Repeat::ALL {
            assert_eq!(Repeat::parse(repeat.as_str()), Some(repeat));
        }
    }

    #[test]
    fn sorting_and_buckets() {
        let today = date(2026, 11, 14);
        let make = |id: Id, title: &str, due: Option<NaiveDate>, important: bool| {
            let mut card = TaskCard::new(1, title, now() + chrono::Duration::minutes(id));
            card.task.id = id;
            card.task.due = due;
            if important {
                card.task.importance = Importance::High;
            }
            card
        };
        let mut cards = vec![
            make(1, "banana", None, false),
            make(2, "Apple", Some(date(2026, 11, 20)), false),
            make(3, "cherry", Some(date(2026, 11, 15)), true),
            make(4, "date", None, true),
        ];
        let ids = |cards: &[TaskCard]| cards.iter().map(TaskCard::id).collect::<Vec<_>>();
        sort_tasks(&mut cards, TaskSort::DueDate);
        assert_eq!(ids(&cards), vec![3, 2, 1, 4]);
        sort_tasks(&mut cards, TaskSort::Importance);
        assert_eq!(ids(&cards), vec![3, 4, 2, 1]);
        sort_tasks(&mut cards, TaskSort::Alphabetical);
        assert_eq!(ids(&cards), vec![2, 1, 3, 4]);
        sort_tasks(&mut cards, TaskSort::Created);
        assert_eq!(ids(&cards), vec![1, 2, 3, 4]);

        assert_eq!(DueBucket::of(None, today), DueBucket::NoDate);
        assert_eq!(
            DueBucket::of(Some(date(2026, 11, 1)), today),
            DueBucket::Overdue
        );
        assert_eq!(DueBucket::of(Some(today), today), DueBucket::Today);
        assert_eq!(
            DueBucket::of(Some(date(2026, 11, 15)), today),
            DueBucket::Tomorrow
        );
        assert_eq!(
            DueBucket::of(Some(date(2026, 11, 20)), today),
            DueBucket::ThisWeek
        );
        assert_eq!(
            DueBucket::of(Some(date(2026, 12, 20)), today),
            DueBucket::Later
        );
    }

    #[test]
    fn flagged_messages_become_linked_tasks() {
        let store = store();
        let account = account(&store);
        let inbox = folder(&store, account.id, FolderRole::Inbox);
        let mut flagged = message(account.id, inbox.id);
        flagged.flags.flagged = true;
        store.save(&mut flagged).unwrap();
        store.save(&mut message(account.id, inbox.id)).unwrap();

        let found = flagged_messages(&store).unwrap();
        assert_eq!(found, vec![flagged.clone()]);

        let mut history = History::new();
        let task = add(&store, &mut history, task_from_message(&flagged, now()));
        assert_eq!(task.task.title, "Hello");
        assert_eq!(task.task.notes, "From Ada");
        assert_eq!(task.details.message_id, Some(flagged.id));
        assert!(task.is_in(SmartList::Flagged, date(2026, 11, 14)));
    }
}
