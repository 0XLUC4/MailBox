//! Calendar domain on top of [`CalendarEvent`]: per-event details (time zone, show-as,
//! privacy, organizer, recurrence exceptions, invitations), occurrence expansion, free/busy
//! and the undoable [`CalendarCommand`]s used by the calendar UI.
//!
//! Details live in the `event_details` table, keyed by the event id. Events without a row get
//! [`EventDetails::default_for`]. A modified occurrence of a series is a normal event whose
//! details point at the series (`master_id`) and the original start (`recurrence_id`); the
//! series lists that original start in its `exdates` so it is not shown twice.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Duration, Local, NaiveDate, SecondsFormat, TimeZone, Utc};
use chrono_tz::Tz;
use rusqlite::Row;
use rusqlite::types::{Type, Value};
use serde::{Deserialize, Serialize};

use crate::command::Command;
use crate::error::{Error, Result};
use crate::model::*;
use crate::recurrence::RRule;
use crate::store::{Record, Store};

/// Schema for [`EventDetails`], appended to `store::MIGRATIONS`.
pub const MIGRATION: &str = r#"
CREATE TABLE event_details (
    id INTEGER PRIMARY KEY REFERENCES events(id) ON DELETE CASCADE,
    master_id INTEGER,
    uid TEXT NOT NULL,
    data_json TEXT NOT NULL
);
CREATE INDEX event_details_master ON event_details(master_id);
CREATE INDEX event_details_uid ON event_details(uid);
"#;

/// Local calendars and tasks (no account, `account_id = 0`): rebuilds `calendars` and
/// `tasks` without the foreign key to `accounts`. Appended to `store::MIGRATIONS`, which
/// runs with foreign keys off so the tables can be swapped.
pub const LOCAL_MIGRATION: &str = r#"
CREATE TABLE calendars_new (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL,
    name TEXT NOT NULL,
    color INTEGER NOT NULL
);
INSERT INTO calendars_new (id, account_id, name, color)
    SELECT id, account_id, name, color FROM calendars;
DROP TABLE calendars;
ALTER TABLE calendars_new RENAME TO calendars;
CREATE TABLE tasks_new (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL,
    title TEXT NOT NULL,
    notes TEXT NOT NULL,
    due TEXT,
    completed INTEGER NOT NULL,
    importance TEXT NOT NULL,
    categories_json TEXT NOT NULL,
    created TEXT NOT NULL
);
INSERT INTO tasks_new (id, account_id, title, notes, due, completed, importance, categories_json, created)
    SELECT id, account_id, title, notes, due, completed, importance, categories_json, created FROM tasks;
DROP TABLE tasks;
ALTER TABLE tasks_new RENAME TO tasks;
"#;

/// Account id of calendars and tasks kept on this computer only.
pub const LOCAL_ACCOUNT: Id = 0;

/// How an event shows on free/busy (Outlook "Show as").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShowAs {
    Free,
    Tentative,
    #[default]
    Busy,
    OutOfOffice,
    WorkingElsewhere,
}

impl ShowAs {
    pub const ALL: [ShowAs; 5] = [
        Self::Free,
        Self::Tentative,
        Self::Busy,
        Self::OutOfOffice,
        Self::WorkingElsewhere,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Free => "Free",
            Self::Tentative => "Tentative",
            Self::Busy => "Busy",
            Self::OutOfOffice => "Out of office",
            Self::WorkingElsewhere => "Working elsewhere",
        }
    }

    /// Whether the slot blocks scheduling.
    pub fn is_busy(self) -> bool {
        !matches!(self, Self::Free | Self::WorkingElsewhere)
    }
}

/// iTIP method of an invitation message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Method {
    Publish,
    Request,
    Reply,
    Cancel,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Publish => "PUBLISH",
            Self::Request => "REQUEST",
            Self::Reply => "REPLY",
            Self::Cancel => "CANCEL",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value.trim().to_ascii_uppercase().as_str() {
            "PUBLISH" => Self::Publish,
            "REQUEST" => Self::Request,
            "REPLY" => Self::Reply,
            "CANCEL" => Self::Cancel,
            _ => return None,
        })
    }
}

/// An invitation received from someone else (`METHOD:REQUEST`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invitation {
    pub method: Method,
    /// iCalendar `SEQUENCE`: a higher one supersedes an older invitation.
    pub sequence: u32,
}

/// Extra event properties stored next to [`CalendarEvent`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventDetails {
    /// Same as the event id.
    pub event_id: Id,
    /// iCalendar UID, shared by a series and its modified occurrences.
    pub uid: String,
    /// IANA zone (`Europe/Paris`) the event was created in; `None` = the system zone.
    pub time_zone: Option<String>,
    pub show_as: ShowAs,
    pub private: bool,
    pub organizer: Option<Address>,
    /// Original starts of skipped (deleted or modified) occurrences of a series.
    pub exdates: Vec<DateTime<Utc>>,
    /// For a modified occurrence: the series event.
    pub master_id: Option<Id>,
    /// For a modified occurrence: the original start it replaces.
    pub recurrence_id: Option<DateTime<Utc>>,
    pub invitation: Option<Invitation>,
}

impl EventDetails {
    /// Details assumed for an event without a stored row.
    pub fn default_for(event_id: Id) -> Self {
        Self {
            event_id,
            uid: if event_id == 0 {
                new_uid()
            } else {
                format!("mailbox-event-{event_id}@mailbox")
            },
            time_zone: None,
            show_as: ShowAs::Busy,
            private: false,
            organizer: None,
            exdates: Vec::new(),
            master_id: None,
            recurrence_id: None,
            invitation: None,
        }
    }
}

impl Record for EventDetails {
    const KIND: &'static str = "event details";
    const TABLE: &'static str = "event_details";
    const COLUMNS: &'static [&'static str] = &["master_id", "uid", "data_json"];

    fn id(&self) -> Id {
        self.event_id
    }
    fn set_id(&mut self, id: Id) {
        self.event_id = id;
    }
    fn values(&self) -> Result<Vec<Value>> {
        Ok(vec![
            self.master_id.map_or(Value::Null, Value::Integer),
            Value::Text(self.uid.clone()),
            Value::Text(serde_json::to_string(self)?),
        ])
    }
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        let raw: String = row.get(3)?;
        let mut details: Self = serde_json::from_str(&raw).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(3, Type::Text, Box::new(err))
        })?;
        details.event_id = row.get(0)?;
        details.master_id = row.get(1)?;
        Ok(details)
    }
}

/// A new globally unique id for an iCalendar UID.
pub fn new_uid() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = Utc::now().timestamp_nanos_opt().unwrap_or_default();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:x}-{n:x}@mailbox")
}

// ---------- time zones ----------

/// Parses an IANA zone name.
pub fn parse_tz(name: &str) -> Option<Tz> {
    name.trim().parse::<Tz>().ok()
}

/// Every IANA zone name, sorted.
pub fn time_zone_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = chrono_tz::TZ_VARIANTS.iter().map(|tz| tz.name()).collect();
    names.sort_unstable();
    names
}

/// UTC offset label of `zone` at `at`, e.g. "UTC+01:00".
pub fn utc_offset_label(zone: &str, at: DateTime<Utc>) -> String {
    let seconds = parse_tz(zone).map_or(0, |tz| {
        use chrono::Offset as _;
        at.with_timezone(&tz).offset().fix().local_minus_utc()
    });
    let sign = if seconds < 0 { '-' } else { '+' };
    let seconds = seconds.abs();
    format!("UTC{sign}{:02}:{:02}", seconds / 3600, seconds % 3600 / 60)
}

// ---------- reads ----------

/// Stored details of an event, or the defaults.
pub fn details(store: &Store, event_id: Id) -> Result<EventDetails> {
    Ok(store
        .get::<EventDetails>(event_id)?
        .unwrap_or_else(|| EventDetails::default_for(event_id)))
}

/// Modified occurrences of a series.
pub fn exceptions_of(store: &Store, master_id: Id) -> Result<Vec<EventDetails>> {
    store.query("WHERE master_id = ?1 ORDER BY id", [master_id])
}

/// Details rows with this UID (a series and its exceptions).
pub fn details_by_uid(store: &Store, uid: &str) -> Result<Vec<EventDetails>> {
    store.query("WHERE uid = ?1 ORDER BY id", [uid])
}

/// One displayed instance of an event.
#[derive(Debug, Clone, PartialEq)]
pub struct Occurrence {
    /// The event with `start`/`end` set to this instance.
    pub event: CalendarEvent,
    pub details: EventDetails,
    /// Original start of this instance when it belongs to a recurring series.
    pub series_start: Option<DateTime<Utc>>,
}

impl Occurrence {
    /// True for an instance generated from a recurring event.
    pub fn is_recurring_instance(&self) -> bool {
        self.event.recurrence.is_some()
    }

    /// True for a modified occurrence stored as its own event.
    pub fn is_exception(&self) -> bool {
        self.details.master_id.is_some()
    }

    /// True when edits should ask "this occurrence / entire series".
    pub fn is_part_of_series(&self) -> bool {
        self.is_recurring_instance() || self.is_exception()
    }

    /// The series event id (itself for a plain event).
    pub fn series_id(&self) -> Id {
        self.details.master_id.unwrap_or(self.event.id)
    }

    /// Stable identity of this instance (event id + original start).
    pub fn key(&self) -> (Id, i64) {
        (
            self.event.id,
            self.series_start.unwrap_or(self.event.start).timestamp(),
        )
    }

    pub fn duration(&self) -> Duration {
        self.event.end - self.event.start
    }
}

/// Runs `f` with the zone the event's recurrence is computed in.
fn with_event_tz<R>(
    event: &CalendarEvent,
    details: &EventDetails,
    f: impl FnOnce(&dyn ExpandIn) -> R,
) -> R {
    if event.all_day {
        return f(&Utc);
    }
    match details.time_zone.as_deref().and_then(parse_tz) {
        Some(tz) => f(&tz),
        None => f(&Local),
    }
}

/// Object-safe wrapper over [`RRule::occurrences`] for any zone.
trait ExpandIn {
    fn occurrences(
        &self,
        rule: &RRule,
        event: &CalendarEvent,
        exdates: &[DateTime<Utc>],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Vec<DateTime<Utc>>;
}

impl<Z: TimeZone> ExpandIn for Z {
    fn occurrences(
        &self,
        rule: &RRule,
        event: &CalendarEvent,
        exdates: &[DateTime<Utc>],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Vec<DateTime<Utc>> {
        rule.occurrences(
            event.start,
            event.end - event.start,
            self,
            exdates,
            from,
            to,
        )
    }
}

/// Expands one event into its instances overlapping `[from, to)`.
pub fn expand_event(
    event: &CalendarEvent,
    details: &EventDetails,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Vec<Occurrence> {
    let rule = event
        .recurrence
        .as_deref()
        .and_then(|value| RRule::parse(value).ok());
    let Some(rule) = rule else {
        if event.start < to && (event.end > from || event.start >= from) {
            return vec![Occurrence {
                event: event.clone(),
                details: details.clone(),
                series_start: None,
            }];
        }
        return Vec::new();
    };
    let duration = event.end - event.start;
    let starts = with_event_tz(event, details, |tz| {
        tz.occurrences(&rule, event, &details.exdates, from, to)
    });
    starts
        .into_iter()
        .map(|start| {
            let mut instance = event.clone();
            instance.start = start;
            instance.end = start + duration;
            Occurrence {
                event: instance,
                details: details.clone(),
                series_start: Some(start),
            }
        })
        .collect()
}

/// All instances overlapping `[from, to)`, sorted by start (all-day first on ties).
pub fn occurrences(
    store: &Store,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<Occurrence>> {
    let events = store.list_events_between(from, to)?;
    let stored: Vec<EventDetails> = store.query(
        "WHERE id IN (SELECT id FROM events WHERE start_at < ?2 AND (end_at > ?1 OR recurrence IS NOT NULL))",
        rusqlite::params![timestamp(&from), timestamp(&to)],
    )?;
    let mut found: Vec<Occurrence> = events
        .iter()
        .flat_map(|event| {
            let details = stored
                .iter()
                .find(|details| details.event_id == event.id)
                .cloned()
                .unwrap_or_else(|| EventDetails::default_for(event.id));
            expand_event(event, &details, from, to)
        })
        .collect();
    sort_occurrences(&mut found);
    Ok(found)
}

/// Sorts by start, all-day first, longer first.
pub fn sort_occurrences(list: &mut [Occurrence]) {
    list.sort_by(|a, b| {
        a.event
            .start
            .cmp(&b.event.start)
            .then(b.event.all_day.cmp(&a.event.all_day))
            .then((b.event.end - b.event.start).cmp(&(a.event.end - a.event.start)))
            .then(a.event.id.cmp(&b.event.id))
    });
}

fn timestamp(value: &DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

// ---------- reminders ----------

/// A reminder that is due: `at` is when it fires.
#[derive(Debug, Clone, PartialEq)]
pub struct DueReminder {
    pub at: DateTime<Utc>,
    pub occurrence: Occurrence,
}

/// Reminders firing in `(after, now]` for instances that have not ended yet, oldest first.
pub fn due_reminders(
    store: &Store,
    after: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<Vec<DueReminder>> {
    // The longest usual reminder is a couple of weeks before the start.
    let list = occurrences(store, after, now + Duration::days(15))?;
    let mut due: Vec<DueReminder> = list
        .into_iter()
        .filter(|occurrence| occurrence.event.end > now)
        .flat_map(|occurrence| {
            occurrence
                .event
                .reminders
                .iter()
                .map(|minutes| occurrence.event.start - Duration::minutes(*minutes))
                .filter(|at| *at > after && *at <= now)
                .max()
                .map(|at| DueReminder { at, occurrence })
        })
        .collect();
    due.sort_by_key(|reminder| reminder.at);
    Ok(due)
}

// ---------- free/busy ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusySlot {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub show_as: ShowAs,
}

fn same_email(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// Busy slots of `email` among `list`. `my_emails` are the user's own addresses: every event
/// in the store is theirs, others are busy where they organize or attend (not declined).
pub fn free_busy(list: &[Occurrence], email: &str, my_emails: &[String]) -> Vec<BusySlot> {
    let is_me = my_emails.iter().any(|mine| same_email(mine, email));
    let mut slots: Vec<BusySlot> = list
        .iter()
        .filter(|occurrence| occurrence.details.show_as != ShowAs::Free)
        .filter(|occurrence| {
            let attendee = occurrence
                .event
                .attendees
                .iter()
                .find(|attendee| same_email(&attendee.address.email, email));
            if attendee.is_some_and(|a| a.status == ResponseStatus::Declined) {
                return false;
            }
            is_me
                || attendee.is_some()
                || occurrence
                    .details
                    .organizer
                    .as_ref()
                    .is_some_and(|organizer| same_email(&organizer.email, email))
        })
        .map(|occurrence| {
            let tentative = occurrence.event.attendees.iter().any(|a| {
                same_email(&a.address.email, email) && a.status == ResponseStatus::Tentative
            });
            BusySlot {
                start: occurrence.event.start,
                end: occurrence.event.end,
                show_as: if tentative && occurrence.details.show_as == ShowAs::Busy {
                    ShowAs::Tentative
                } else {
                    occurrence.details.show_as
                },
            }
        })
        .collect();
    slots.sort_by_key(|slot| slot.start);
    slots
}

/// Starts of free slots of `duration` for everyone, stepping by `step`, inside working hours
/// (`work_start_hour..work_end_hour` in `tz`, Monday to Friday), at most `limit` results.
#[allow(clippy::too_many_arguments)]
pub fn suggest_slots<Z: TimeZone>(
    busy: &[Vec<BusySlot>],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    duration: Duration,
    step: Duration,
    work_hours: (u32, u32),
    tz: &Z,
    limit: usize,
) -> Vec<DateTime<Utc>> {
    use chrono::{Datelike, Timelike, Weekday};
    let mut found = Vec::new();
    if step <= Duration::zero() || duration <= Duration::zero() {
        return found;
    }
    let mut at = from;
    while at + duration <= to && found.len() < limit {
        let local_start = at.with_timezone(tz);
        let local_end = (at + duration).with_timezone(tz);
        let in_hours = !matches!(local_start.weekday(), Weekday::Sat | Weekday::Sun)
            && local_start.date_naive() == local_end.date_naive()
            && local_start.hour() >= work_hours.0
            && (local_end.hour() < work_hours.1
                || (local_end.hour() == work_hours.1 && local_end.minute() == 0));
        let free = busy
            .iter()
            .flatten()
            .all(|slot| !slot.show_as.is_busy() || slot.end <= at || slot.start >= at + duration);
        if in_hours && free {
            found.push(at);
        }
        at += step;
    }
    found
}

// ---------- commands ----------

/// An event plus its details, as stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventSnapshot {
    pub event: CalendarEvent,
    pub details: Option<EventDetails>,
}

/// Undoable calendar mutations, wrapped in [`Command::Calendar`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum CalendarCommand {
    /// Creates (`event.id == 0`) or replaces an event and its details.
    Save {
        event: Box<CalendarEvent>,
        details: Box<EventDetails>,
    },
    /// Moves/resizes an event. For a series the whole series shifts (exceptions keep their
    /// identity: their `recurrence_id`s and the series `exdates` shift by the same amount).
    Move {
        id: Id,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    },
    /// Duplicates an event (as a one-off) at a new time.
    Copy {
        id: Id,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    },
    /// Deletes an event, and every modified occurrence when it is a series.
    Delete { id: Id },
    /// Deletes one instance of a series (adds an exdate).
    SkipOccurrence {
        series_id: Id,
        occurrence: DateTime<Utc>,
    },
    /// Edits one instance of a series: stores it as its own event linked to the series.
    /// `event.id == 0` creates the exception, otherwise updates it.
    SaveOccurrence {
        series_id: Id,
        occurrence: DateTime<Utc>,
        event: Box<CalendarEvent>,
        details: Box<EventDetails>,
    },
    /// Sets the user's answer to an invitation.
    Respond {
        id: Id,
        email: String,
        status: ResponseStatus,
    },
    /// Imports events (from .ics), merging with existing ones by UID. A `REPLY` updates
    /// attendee answers, a `CANCEL` deletes the matching events.
    Import {
        calendar_id: Id,
        method: Option<Method>,
        events: Vec<EventSnapshot>,
    },
    /// Puts snapshots back and removes created events (the inverse of every other op).
    Restore {
        put: Vec<EventSnapshot>,
        remove: Vec<Id>,
    },
}

impl CalendarCommand {
    pub fn label(&self) -> String {
        match self {
            Self::Save { event, .. } if event.id == 0 => "Create event".into(),
            Self::Save { .. } => "Edit event".into(),
            Self::Move { .. } => "Move event".into(),
            Self::Copy { .. } => "Copy event".into(),
            Self::Delete { .. } => "Delete event".into(),
            Self::SkipOccurrence { .. } => "Delete occurrence".into(),
            Self::SaveOccurrence { .. } => "Edit occurrence".into(),
            Self::Respond { status, .. } => match status {
                ResponseStatus::Accepted => "Accept invitation".into(),
                ResponseStatus::Tentative => "Tentatively accept invitation".into(),
                ResponseStatus::Declined => "Decline invitation".into(),
                ResponseStatus::NeedsAction => "Reset invitation answer".into(),
            },
            Self::Import { events, method, .. } => match method {
                Some(Method::Reply) => "Apply invitation reply".into(),
                Some(Method::Cancel) => "Apply cancellation".into(),
                _ if events.len() == 1 => "Import event".into(),
                _ => format!("Import {} events", events.len()),
            },
            Self::Restore { .. } => "Restore events".into(),
        }
    }

    /// Wraps into a [`Command`].
    pub fn into_command(self) -> Command {
        Command::Calendar(Box::new(self))
    }
}

/// Moves a series' EXDATEs and its exceptions' RECURRENCE-IDs by `delta` (the series start
/// moved), so they keep matching the occurrences they skip or replace.
fn shift_series(
    tx: &mut Tx<'_>,
    store: &Store,
    series_id: Id,
    details: &mut EventDetails,
    delta: Duration,
) -> Result<()> {
    if delta == Duration::zero() {
        return Ok(());
    }
    for exdate in &mut details.exdates {
        *exdate += delta;
    }
    for exception in exceptions_of(store, series_id)? {
        let (exception_event, mut exception_details) = tx.load(exception.event_id)?;
        exception_details.recurrence_id = exception_details.recurrence_id.map(|at| at + delta);
        tx.write(exception_event, Some(exception_details))?;
    }
    Ok(())
}

/// Applies a calendar command (inside the caller's transaction) and returns its inverse.
pub fn apply(store: &Store, command: CalendarCommand) -> Result<Command> {
    let mut tx = Tx::new(store);
    match command {
        CalendarCommand::Save { event, details } => {
            validate(&event)?;
            if event.id == 0 {
                tx.create(*event, *details)?;
            } else {
                // A series whose start moved: skipped/modified occurrences move with it.
                let (previous, _) = tx.load(event.id)?;
                let mut details = *details;
                if event.recurrence.is_some() {
                    let delta = event.start - previous.start;
                    shift_series(&mut tx, store, event.id, &mut details, delta)?;
                }
                tx.write(*event, Some(details))?;
            }
        }
        CalendarCommand::Move { id, start, end } => {
            let (mut event, details) = tx.load(id)?;
            let delta = start - event.start;
            event.start = start;
            event.end = end;
            validate(&event)?;
            let mut details = details;
            if event.recurrence.is_some() {
                shift_series(&mut tx, store, id, &mut details, delta)?;
            }
            tx.write(event, Some(details))?;
        }
        CalendarCommand::Copy { id, start, end } => {
            let (mut event, details) = tx.load(id)?;
            event.id = 0;
            event.start = start;
            event.end = end;
            event.recurrence = None;
            validate(&event)?;
            let copy = EventDetails {
                event_id: 0,
                uid: new_uid(),
                exdates: Vec::new(),
                master_id: None,
                recurrence_id: None,
                invitation: None,
                ..details
            };
            tx.create(event, copy)?;
        }
        CalendarCommand::Delete { id } => {
            for exception in exceptions_of(store, id)? {
                tx.delete(exception.event_id)?;
            }
            tx.delete(id)?;
        }
        CalendarCommand::SkipOccurrence {
            series_id,
            occurrence,
        } => {
            let (event, mut details) = tx.load(series_id)?;
            if event.recurrence.is_none() {
                return Err(Error::Invalid("event is not recurring".into()));
            }
            if !details.exdates.contains(&occurrence) {
                details.exdates.push(occurrence);
                details.exdates.sort();
            }
            tx.write(event, Some(details))?;
        }
        CalendarCommand::SaveOccurrence {
            series_id,
            occurrence,
            event,
            details,
        } => {
            let (master, mut master_details) = tx.load(series_id)?;
            let mut event = *event;
            event.recurrence = None;
            event.calendar_id = master.calendar_id;
            validate(&event)?;
            let details = EventDetails {
                uid: master_details.uid.clone(),
                master_id: Some(series_id),
                recurrence_id: Some(occurrence),
                ..*details
            };
            if master.recurrence.is_some() && !master_details.exdates.contains(&occurrence) {
                master_details.exdates.push(occurrence);
                master_details.exdates.sort();
                tx.write(master, Some(master_details))?;
            }
            if event.id == 0 {
                tx.create(event, details)?;
            } else {
                tx.write(event, Some(details))?;
            }
        }
        CalendarCommand::Respond { id, email, status } => {
            let (mut event, details) = tx.load(id)?;
            match event
                .attendees
                .iter_mut()
                .find(|attendee| same_email(&attendee.address.email, &email))
            {
                Some(attendee) => attendee.status = status,
                None => event.attendees.push(Attendee {
                    address: Address::new("", email),
                    status,
                    optional: false,
                }),
            }
            tx.write(event, Some(details))?;
        }
        CalendarCommand::Import {
            calendar_id,
            method,
            events,
        } => import(&mut tx, calendar_id, method, events)?,
        CalendarCommand::Restore { put, remove } => {
            for id in remove {
                if store.get::<CalendarEvent>(id)?.is_some() {
                    tx.delete(id)?;
                }
            }
            for snapshot in put {
                tx.write(snapshot.event, snapshot.details)?;
            }
        }
    }
    Ok(tx.inverse())
}

fn validate(event: &CalendarEvent) -> Result<()> {
    if event.end < event.start {
        return Err(Error::Invalid(
            "an event cannot end before it starts".into(),
        ));
    }
    if let Some(rule) = &event.recurrence {
        RRule::parse(rule)?;
    }
    Ok(())
}

fn import(
    tx: &mut Tx,
    calendar_id: Id,
    method: Option<Method>,
    events: Vec<EventSnapshot>,
) -> Result<()> {
    let store = tx.store;
    // Series first so exceptions can link to them.
    let (masters, exceptions): (Vec<_>, Vec<_>) = events.into_iter().partition(|snapshot| {
        snapshot
            .details
            .as_ref()
            .is_none_or(|details| details.recurrence_id.is_none())
    });
    for snapshot in masters.into_iter().chain(exceptions) {
        let EventSnapshot { mut event, details } = snapshot;
        let mut details = details.unwrap_or_else(|| EventDetails::default_for(0));
        let existing = details_by_uid(store, &details.uid)?
            .into_iter()
            .find(|row| row.recurrence_id == details.recurrence_id);
        match method {
            Some(Method::Cancel) => {
                if let Some(row) = existing {
                    for exception in exceptions_of(store, row.event_id)? {
                        tx.delete(exception.event_id)?;
                    }
                    tx.delete(row.event_id)?;
                } else if let Some(recurrence_id) = details.recurrence_id {
                    // Cancelling one occurrence of a known series.
                    if let Some(master) = details_by_uid(store, &details.uid)?
                        .into_iter()
                        .find(|row| row.recurrence_id.is_none())
                    {
                        let (master_event, mut master_details) = tx.load(master.event_id)?;
                        master_details.exdates.push(recurrence_id);
                        tx.write(master_event, Some(master_details))?;
                    }
                }
                continue;
            }
            Some(Method::Reply) => {
                // Only the attendee answers matter in a reply.
                if let Some(row) = existing {
                    let (mut target, target_details) = tx.load(row.event_id)?;
                    for answer in &event.attendees {
                        if let Some(attendee) = target
                            .attendees
                            .iter_mut()
                            .find(|a| same_email(&a.address.email, &answer.address.email))
                        {
                            attendee.status = answer.status;
                        }
                    }
                    tx.write(target, Some(target_details))?;
                }
                continue;
            }
            _ => {}
        }
        if let Some(recurrence_id) = details.recurrence_id
            && let Some(master) = details_by_uid(store, &details.uid)?
                .into_iter()
                .find(|row| row.recurrence_id.is_none())
        {
            details.master_id = Some(master.event_id);
            let (master_event, mut master_details) = tx.load(master.event_id)?;
            if !master_details.exdates.contains(&recurrence_id) {
                master_details.exdates.push(recurrence_id);
                master_details.exdates.sort();
                tx.write(master_event, Some(master_details))?;
            }
        }
        match existing {
            Some(row) => {
                let (current, current_details) = tx.load(row.event_id)?;
                let newer = details.invitation.as_ref().map_or(0, |i| i.sequence)
                    >= current_details
                        .invitation
                        .as_ref()
                        .map_or(0, |i| i.sequence);
                if newer {
                    event.id = current.id;
                    event.calendar_id = current.calendar_id;
                    details.event_id = current.id;
                    tx.write(event, Some(details))?;
                }
            }
            None => {
                event.id = 0;
                event.calendar_id = calendar_id;
                validate(&event)?;
                tx.create(event, details)?;
            }
        }
    }
    Ok(())
}

/// Records what a command touches so it can build the inverse.
struct Tx<'a> {
    store: &'a Store,
    seen: HashSet<Id>,
    before: Vec<EventSnapshot>,
    created: Vec<Id>,
}

impl<'a> Tx<'a> {
    fn new(store: &'a Store) -> Self {
        Self {
            store,
            seen: HashSet::new(),
            before: Vec::new(),
            created: Vec::new(),
        }
    }

    fn load(&self, id: Id) -> Result<(CalendarEvent, EventDetails)> {
        let event = self.store.require::<CalendarEvent>(id)?;
        let details = details(self.store, id)?;
        Ok((event, details))
    }

    /// Remembers the state of `id` before its first change.
    fn touch(&mut self, id: Id) -> Result<()> {
        if id == 0 || !self.seen.insert(id) {
            return Ok(());
        }
        match self.store.get::<CalendarEvent>(id)? {
            Some(event) => {
                let details = self.store.get::<EventDetails>(id)?;
                self.before.push(EventSnapshot { event, details });
            }
            None => self.created.push(id),
        }
        Ok(())
    }

    fn create(&mut self, mut event: CalendarEvent, mut details: EventDetails) -> Result<Id> {
        event.id = 0;
        let id = self.store.save(&mut event)?;
        self.seen.insert(id);
        self.created.push(id);
        details.event_id = id;
        self.store.save(&mut details)?;
        Ok(id)
    }

    fn write(&mut self, mut event: CalendarEvent, details: Option<EventDetails>) -> Result<()> {
        self.touch(event.id)?;
        self.store.save(&mut event)?;
        match details {
            Some(mut details) => {
                details.event_id = event.id;
                self.store.save(&mut details)?;
            }
            None => {
                self.store.delete::<EventDetails>(event.id)?;
            }
        }
        Ok(())
    }

    fn delete(&mut self, id: Id) -> Result<()> {
        self.touch(id)?;
        self.store.delete::<EventDetails>(id)?;
        if !self.store.delete::<CalendarEvent>(id)? {
            return Err(Error::NotFound { kind: "event", id });
        }
        Ok(())
    }

    fn inverse(self) -> Command {
        // Ids created by this command must not be restored by the inverse.
        let created: HashSet<Id> = self.created.iter().copied().collect();
        CalendarCommand::Restore {
            put: self
                .before
                .into_iter()
                .filter(|snapshot| !created.contains(&snapshot.event.id))
                .collect(),
            remove: self.created,
        }
        .into_command()
    }
}

/// Midnight UTC of a date: how all-day events store their start.
pub fn all_day_start(date: NaiveDate) -> DateTime<Utc> {
    date.and_hms_opt(0, 0, 0).unwrap_or_default().and_utc()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::History;
    use crate::store::tests::{account, store};

    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    fn calendar(store: &Store) -> Calendar {
        let account = account(store);
        let mut calendar = Calendar {
            id: 0,
            account_id: account.id,
            name: "Work".into(),
            color: 0x3b82f6,
        };
        store.save(&mut calendar).unwrap();
        calendar
    }

    #[test]
    fn local_calendar_and_task_need_no_account() {
        let store = store();
        let mut local = Calendar {
            id: 0,
            account_id: LOCAL_ACCOUNT,
            name: "Calendar".into(),
            color: 0x3b82f6,
        };
        store.save(&mut local).unwrap();
        let mut history = History::new();
        let mut ev = event(local.id, utc(2026, 11, 16, 9, 0), None);
        ev.attendees.clear();
        history
            .run(
                &store,
                CalendarCommand::Save {
                    event: Box::new(ev),
                    details: Box::new(EventDetails::default_for(0)),
                }
                .into_command(),
            )
            .unwrap();
        assert_eq!(store.query::<CalendarEvent>("", []).unwrap().len(), 1);
        let mut task = crate::todo::TaskCard::new(LOCAL_ACCOUNT, "Local task", Utc::now());
        history
            .run(
                &store,
                Command::Todo(crate::todo::TaskCommand::SaveTask(Box::new(task.clone()))),
            )
            .unwrap();
        task = crate::todo::list_task_cards(&store).unwrap().remove(0);
        assert_eq!(task.task.account_id, LOCAL_ACCOUNT);
        // Events still reference their calendar.
        assert!(store.delete::<Calendar>(local.id).is_err());
    }

    fn event(calendar_id: Id, start: DateTime<Utc>, recurrence: Option<&str>) -> CalendarEvent {
        CalendarEvent {
            id: 0,
            calendar_id,
            title: "Standup".into(),
            location: String::new(),
            description: String::new(),
            start,
            end: start + Duration::minutes(30),
            all_day: false,
            recurrence: recurrence.map(str::to_owned),
            attendees: vec![Attendee {
                address: Address::new("Ada", "ada@example.com"),
                status: ResponseStatus::NeedsAction,
                optional: false,
            }],
            reminders: vec![10],
            categories: vec![],
        }
    }

    fn utc_details() -> EventDetails {
        EventDetails {
            time_zone: Some("UTC".into()),
            ..EventDetails::default_for(0)
        }
    }

    fn run(store: &Store, history: &mut History, command: CalendarCommand) {
        history.run(store, command.into_command()).unwrap();
    }

    fn dump(store: &Store) -> Vec<EventSnapshot> {
        store
            .query::<CalendarEvent>("ORDER BY id", [])
            .unwrap()
            .into_iter()
            .map(|event| EventSnapshot {
                details: store.get::<EventDetails>(event.id).unwrap(),
                event,
            })
            .collect()
    }

    fn create(store: &Store, history: &mut History, event: CalendarEvent) -> Id {
        run(
            store,
            history,
            CalendarCommand::Save {
                event: Box::new(event),
                details: Box::new(utc_details()),
            },
        );
        store
            .query::<CalendarEvent>("ORDER BY id DESC", [])
            .unwrap()[0]
            .id
    }

    #[test]
    fn create_update_undo_redo() {
        let store = store();
        let cal = calendar(&store);
        let mut history = History::new();
        let id = create(
            &store,
            &mut history,
            event(cal.id, utc(2026, 11, 16, 9, 0), None),
        );
        assert_eq!(history.undo_label(), Some("Create event"));
        let created = dump(&store);
        assert_eq!(created.len(), 1);
        assert!(created[0].details.is_some());

        let mut edited = store.require::<CalendarEvent>(id).unwrap();
        edited.title = "Renamed".into();
        run(
            &store,
            &mut history,
            CalendarCommand::Save {
                event: Box::new(edited),
                details: Box::new(EventDetails {
                    private: true,
                    ..details(&store, id).unwrap()
                }),
            },
        );
        assert!(details(&store, id).unwrap().private);

        history.undo(&store).unwrap();
        assert_eq!(dump(&store), created);
        history.undo(&store).unwrap();
        assert!(dump(&store).is_empty());
        history.redo(&store).unwrap();
        assert_eq!(dump(&store), created);
    }

    #[test]
    fn saving_a_series_at_a_new_time_keeps_skipped_occurrences_skipped() {
        let store = store();
        let cal = calendar(&store);
        let mut history = History::new();
        let start = utc(2026, 11, 16, 9, 0);
        let id = create(
            &store,
            &mut history,
            event(cal.id, start, Some("FREQ=DAILY;COUNT=5")),
        );
        run(
            &store,
            &mut history,
            CalendarCommand::SkipOccurrence {
                series_id: id,
                occurrence: start + Duration::days(1),
            },
        );
        let series = store.require::<CalendarEvent>(id).unwrap();
        run(
            &store,
            &mut history,
            CalendarCommand::Save {
                event: Box::new(CalendarEvent {
                    start: series.start + Duration::hours(1),
                    end: series.end + Duration::hours(1),
                    ..series
                }),
                details: Box::new(details(&store, id).unwrap()),
            },
        );
        assert_eq!(
            details(&store, id).unwrap().exdates,
            vec![start + Duration::days(1) + Duration::hours(1)]
        );
    }

    #[test]
    fn move_and_copy_are_undoable() {
        let store = store();
        let cal = calendar(&store);
        let mut history = History::new();
        let id = create(
            &store,
            &mut history,
            event(cal.id, utc(2026, 11, 16, 9, 0), None),
        );
        let before = dump(&store);

        let start = utc(2026, 11, 17, 14, 0);
        run(
            &store,
            &mut history,
            CalendarCommand::Move {
                id,
                start,
                end: start + Duration::hours(1),
            },
        );
        let moved = store.require::<CalendarEvent>(id).unwrap();
        assert_eq!(
            (moved.start, moved.end),
            (start, start + Duration::hours(1))
        );
        history.undo(&store).unwrap();
        assert_eq!(dump(&store), before);

        run(
            &store,
            &mut history,
            CalendarCommand::Copy {
                id,
                start,
                end: start + Duration::minutes(30),
            },
        );
        let all = dump(&store);
        assert_eq!(all.len(), 2);
        assert_ne!(
            all[0].details.as_ref().unwrap().uid,
            all[1].details.as_ref().unwrap().uid
        );
        history.undo(&store).unwrap();
        assert_eq!(dump(&store), before);

        let invalid = CalendarCommand::Move {
            id,
            start,
            end: start - Duration::hours(1),
        };
        assert!(history.run(&store, invalid.into_command()).is_err());
    }

    #[test]
    fn occurrence_exceptions_and_series_delete() {
        let store = store();
        let cal = calendar(&store);
        let mut history = History::new();
        let start = utc(2026, 11, 16, 9, 0);
        let series = create(
            &store,
            &mut history,
            event(cal.id, start, Some("FREQ=DAILY;COUNT=5")),
        );
        let original = dump(&store);
        let week = |store: &Store| occurrences(store, start, start + Duration::days(7)).unwrap();
        assert_eq!(week(&store).len(), 5);

        // Skip the 2nd occurrence.
        let second = start + Duration::days(1);
        run(
            &store,
            &mut history,
            CalendarCommand::SkipOccurrence {
                series_id: series,
                occurrence: second,
            },
        );
        assert_eq!(week(&store).len(), 4);
        assert!(week(&store).iter().all(|o| o.event.start != second));

        // Move the 3rd occurrence to the afternoon.
        let third = start + Duration::days(2);
        let mut moved = store.require::<CalendarEvent>(series).unwrap();
        moved.id = 0;
        moved.start = third + Duration::hours(5);
        moved.end = moved.start + Duration::minutes(30);
        run(
            &store,
            &mut history,
            CalendarCommand::SaveOccurrence {
                series_id: series,
                occurrence: third,
                event: Box::new(moved),
                details: Box::new(utc_details()),
            },
        );
        let list = week(&store);
        assert_eq!(list.len(), 4);
        let exception = list
            .iter()
            .find(|o| o.event.start == third + Duration::hours(5))
            .unwrap();
        assert!(exception.is_exception());
        assert_eq!(exception.series_id(), series);
        assert_eq!(exception.details.recurrence_id, Some(third));
        assert!(!exception.is_recurring_instance());
        assert!(list.iter().all(|o| o.event.start != third));

        // Deleting the series removes the exception too; one undo restores everything.
        let before_delete = dump(&store);
        run(&store, &mut history, CalendarCommand::Delete { id: series });
        assert!(dump(&store).is_empty());
        history.undo(&store).unwrap();
        assert_eq!(dump(&store), before_delete);

        history.undo(&store).unwrap();
        history.undo(&store).unwrap();
        assert_eq!(dump(&store), original);
    }

    #[test]
    fn moving_a_series_shifts_exceptions() {
        let store = store();
        let cal = calendar(&store);
        let mut history = History::new();
        let start = utc(2026, 11, 16, 9, 0);
        let series = create(
            &store,
            &mut history,
            event(cal.id, start, Some("FREQ=DAILY;COUNT=3")),
        );
        let second = start + Duration::days(1);
        run(
            &store,
            &mut history,
            CalendarCommand::SkipOccurrence {
                series_id: series,
                occurrence: second,
            },
        );
        let later = start + Duration::hours(2);
        run(
            &store,
            &mut history,
            CalendarCommand::Move {
                id: series,
                start: later,
                end: later + Duration::minutes(30),
            },
        );
        let list = occurrences(&store, start, start + Duration::days(7)).unwrap();
        let starts: Vec<_> = list.iter().map(|o| o.event.start).collect();
        assert_eq!(starts, [later, later + Duration::days(2)]);
    }

    #[test]
    fn respond_to_invitation() {
        let store = store();
        let cal = calendar(&store);
        let mut history = History::new();
        let id = create(
            &store,
            &mut history,
            event(cal.id, utc(2026, 11, 16, 9, 0), None),
        );
        run(
            &store,
            &mut history,
            CalendarCommand::Respond {
                id,
                email: "ADA@example.com".into(),
                status: ResponseStatus::Tentative,
            },
        );
        let event = store.require::<CalendarEvent>(id).unwrap();
        assert_eq!(event.attendees.len(), 1);
        assert_eq!(event.attendees[0].status, ResponseStatus::Tentative);
        assert_eq!(history.undo_label(), Some("Tentatively accept invitation"));
        history.undo(&store).unwrap();
        assert_eq!(
            store.require::<CalendarEvent>(id).unwrap().attendees[0].status,
            ResponseStatus::NeedsAction
        );
    }

    #[test]
    fn import_merges_by_uid_and_handles_reply_and_cancel() {
        let store = store();
        let cal = calendar(&store);
        let mut history = History::new();
        let start = utc(2026, 11, 16, 9, 0);
        let incoming = |title: &str, sequence: u32| EventSnapshot {
            event: CalendarEvent {
                title: title.into(),
                ..event(0, start, None)
            },
            details: Some(EventDetails {
                uid: "abc@example.com".into(),
                invitation: Some(Invitation {
                    method: Method::Request,
                    sequence,
                }),
                ..utc_details()
            }),
        };
        let import = |method, events| CalendarCommand::Import {
            calendar_id: cal.id,
            method,
            events,
        };
        run(
            &store,
            &mut history,
            import(Some(Method::Request), vec![incoming("Plan", 0)]),
        );
        run(
            &store,
            &mut history,
            import(Some(Method::Request), vec![incoming("Plan v2", 1)]),
        );
        let all = dump(&store);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].event.title, "Plan v2");
        assert_eq!(all[0].event.calendar_id, cal.id);

        // An older sequence does not overwrite.
        run(
            &store,
            &mut history,
            import(Some(Method::Request), vec![incoming("Old", 0)]),
        );
        assert_eq!(dump(&store)[0].event.title, "Plan v2");

        let mut reply = incoming("ignored", 1);
        reply.event.attendees[0].status = ResponseStatus::Accepted;
        run(
            &store,
            &mut history,
            import(Some(Method::Reply), vec![reply]),
        );
        let all = dump(&store);
        assert_eq!(all[0].event.title, "Plan v2");
        assert_eq!(all[0].event.attendees[0].status, ResponseStatus::Accepted);

        run(
            &store,
            &mut history,
            import(Some(Method::Cancel), vec![incoming("", 2)]),
        );
        assert!(dump(&store).is_empty());
        history.undo(&store).unwrap();
        assert_eq!(dump(&store), all);
    }

    #[test]
    fn free_busy_and_suggestions() {
        let store = store();
        let cal = calendar(&store);
        let mut history = History::new();
        // Monday 16 Nov 2026, 09:00-09:30 with Ada.
        let start = utc(2026, 11, 16, 9, 0);
        create(&store, &mut history, event(cal.id, start, None));
        let mut solo = event(cal.id, start + Duration::hours(1), None);
        solo.attendees.clear();
        create(&store, &mut history, solo);

        let day = occurrences(
            &store,
            start - Duration::hours(9),
            start + Duration::hours(15),
        )
        .unwrap();
        let me = vec!["me@example.com".to_owned()];
        let mine = free_busy(&day, "me@example.com", &me);
        let ada = free_busy(&day, "ada@example.com", &me);
        let bob = free_busy(&day, "bob@example.com", &me);
        assert_eq!(mine.len(), 2);
        assert_eq!(ada.len(), 1);
        assert!(bob.is_empty());

        let slots = suggest_slots(
            &[mine, ada],
            start,
            start + Duration::hours(4),
            Duration::minutes(30),
            Duration::minutes(30),
            (9, 17),
            &Utc,
            10,
        );
        assert_eq!(
            slots,
            [
                start + Duration::minutes(30),
                start + Duration::minutes(90),
                start + Duration::minutes(120),
                start + Duration::minutes(150),
                start + Duration::minutes(180),
                start + Duration::minutes(210),
            ]
        );
    }

    #[test]
    fn reminders_fire_once_in_window() {
        let store = store();
        let cal = calendar(&store);
        let mut history = History::new();
        let start = utc(2026, 11, 16, 9, 0);
        create(&store, &mut history, event(cal.id, start, None));
        let fire = start - Duration::minutes(10);
        assert!(
            due_reminders(
                &store,
                fire - Duration::minutes(5),
                fire - Duration::minutes(1)
            )
            .unwrap()
            .is_empty()
        );
        let due = due_reminders(&store, fire - Duration::minutes(1), fire).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].at, fire);
        assert!(
            due_reminders(&store, fire, fire + Duration::minutes(1))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn time_zone_helpers() {
        assert!(parse_tz("Europe/Paris").is_some());
        assert!(parse_tz("Mars/Olympus").is_none());
        assert!(time_zone_names().contains(&"America/New_York"));
        assert_eq!(
            utc_offset_label("Europe/Paris", utc(2026, 1, 1, 0, 0)),
            "UTC+01:00"
        );
        assert_eq!(
            utc_offset_label("America/New_York", utc(2026, 7, 1, 0, 0)),
            "UTC-04:00"
        );
    }

    #[test]
    fn calendar_commands_serialize() {
        let command = CalendarCommand::SkipOccurrence {
            series_id: 3,
            occurrence: utc(2026, 11, 16, 9, 0),
        }
        .into_command();
        let json = serde_json::to_string(&command).unwrap();
        assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), command);
    }
}
