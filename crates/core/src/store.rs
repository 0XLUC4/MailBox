//! SQLite persistence: schema migrations, generic CRUD over [`Record`] and typed queries.
//!
//! Lists (addresses, categories, attendees...) are stored as JSON text columns,
//! timestamps as RFC 3339 UTC text (`2026-11-14T09:30:00Z`, sorts lexicographically).

use std::path::Path;

use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use rusqlite::types::{Type, Value};
use rusqlite::{Connection, OptionalExtension, Params, Row, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{Error, Result};
use crate::model::*;

/// Ordered schema migrations; index + 1 is the resulting `PRAGMA user_version`.
/// Never edit a shipped migration: append a new one.
const MIGRATIONS: &[&str] = &[
    r#"
CREATE TABLE accounts (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    email TEXT NOT NULL,
    color INTEGER NOT NULL
);
CREATE TABLE folders (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id),
    parent_id INTEGER REFERENCES folders(id),
    name TEXT NOT NULL,
    role TEXT NOT NULL,
    position INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE messages (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id),
    folder_id INTEGER NOT NULL REFERENCES folders(id),
    thread_id TEXT NOT NULL,
    from_name TEXT NOT NULL,
    from_email TEXT NOT NULL,
    to_json TEXT NOT NULL,
    cc_json TEXT NOT NULL,
    subject TEXT NOT NULL,
    date TEXT NOT NULL,
    preview TEXT NOT NULL,
    body_text TEXT NOT NULL,
    body_html TEXT,
    is_read INTEGER NOT NULL,
    is_flagged INTEGER NOT NULL,
    is_pinned INTEGER NOT NULL,
    importance TEXT NOT NULL,
    categories_json TEXT NOT NULL,
    attachments_json TEXT NOT NULL
);
CREATE INDEX messages_folder_date ON messages(folder_id, date DESC);
CREATE INDEX messages_thread ON messages(thread_id);
CREATE TABLE calendars (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id),
    name TEXT NOT NULL,
    color INTEGER NOT NULL
);
CREATE TABLE events (
    id INTEGER PRIMARY KEY,
    calendar_id INTEGER NOT NULL REFERENCES calendars(id),
    title TEXT NOT NULL,
    location TEXT NOT NULL,
    description TEXT NOT NULL,
    start_at TEXT NOT NULL,
    end_at TEXT NOT NULL,
    all_day INTEGER NOT NULL,
    recurrence TEXT,
    attendees_json TEXT NOT NULL,
    reminders_json TEXT NOT NULL,
    categories_json TEXT NOT NULL
);
CREATE INDEX events_start ON events(start_at);
CREATE TABLE contacts (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id),
    display_name TEXT NOT NULL,
    emails_json TEXT NOT NULL,
    phones_json TEXT NOT NULL,
    company TEXT NOT NULL,
    job_title TEXT NOT NULL,
    notes TEXT NOT NULL,
    favorite INTEGER NOT NULL
);
CREATE TABLE tasks (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id),
    title TEXT NOT NULL,
    notes TEXT NOT NULL,
    due TEXT,
    completed INTEGER NOT NULL,
    importance TEXT NOT NULL,
    categories_json TEXT NOT NULL,
    created TEXT NOT NULL
);
CREATE TABLE categories (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    color INTEGER NOT NULL
);
CREATE TABLE rules (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id),
    name TEXT NOT NULL,
    enabled INTEGER NOT NULL,
    conditions_json TEXT NOT NULL,
    actions_json TEXT NOT NULL
);
"#,
    r#"
CREATE TABLE message_flag_due (
    id INTEGER PRIMARY KEY,
    due TEXT NOT NULL
);
"#,
    crate::people::MIGRATION,
    crate::todo::MIGRATION,
    r#"
CREATE TABLE IF NOT EXISTS settings (
    id INTEGER PRIMARY KEY,
    key TEXT NOT NULL UNIQUE,
    value TEXT NOT NULL
);
"#,
    crate::compose::SCHEMA,
    crate::calendar::MIGRATION,
    crate::people::MIGRATION_LABELS,
    crate::calendar::LOCAL_MIGRATION,
    crate::todo::POSITION_MIGRATION,
    crate::remote::MIGRATION,
];

/// A model type persisted in one table. Row layout: `id` then [`Record::COLUMNS`] in order.
pub trait Record: Sized {
    /// Human name used in errors ("message", "folder"...).
    const KIND: &'static str;
    const TABLE: &'static str;
    /// Column names, excluding `id`, in the order of [`Record::values`].
    const COLUMNS: &'static [&'static str];

    fn id(&self) -> Id;
    fn set_id(&mut self, id: Id);
    fn values(&self) -> Result<Vec<Value>>;
    fn from_row(row: &Row) -> rusqlite::Result<Self>;
}

/// Handle to the SQLite database. Cheap reads; mutations should go through commands.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens (or creates) the database file and runs pending migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    /// In-memory database (tests).
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", true)?;
        // Returns the resulting mode ("wal", or "memory" for in-memory databases).
        let _mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    /// Foreign keys are off while migrating (they cannot change inside a transaction), so a
    /// migration can rebuild a referenced table.
    fn migrate(&self) -> Result<()> {
        self.conn.pragma_update(None, "foreign_keys", false)?;
        let result = self.run_migrations();
        self.conn.pragma_update(None, "foreign_keys", true)?;
        result
    }

    fn run_migrations(&self) -> Result<()> {
        let version: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        for (index, sql) in MIGRATIONS.iter().enumerate() {
            let target = index as i64 + 1;
            if target <= version {
                continue;
            }
            self.transaction(|store| {
                store.conn.execute_batch(sql)?;
                store.conn.pragma_update(None, "user_version", target)?;
                Ok(())
            })?;
        }
        Ok(())
    }

    /// The connection, for typed queries kept in their own module (e.g. [`crate::remote`]).
    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Current schema version.
    pub fn schema_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?)
    }

    /// Runs `f` atomically (nestable savepoint): rolled back if it returns an error.
    pub fn transaction<R>(&self, f: impl FnOnce(&Store) -> Result<R>) -> Result<R> {
        self.conn.execute_batch("SAVEPOINT mailbox_tx")?;
        match f(self) {
            Ok(value) => {
                self.conn.execute_batch("RELEASE mailbox_tx")?;
                Ok(value)
            }
            Err(err) => {
                // Keep the original error; a failed rollback leaves nothing better to report.
                let _ = self
                    .conn
                    .execute_batch("ROLLBACK TO mailbox_tx; RELEASE mailbox_tx");
                Err(err)
            }
        }
    }

    // ---------- generic CRUD ----------

    /// Inserts (id 0, assigns the new id) or upserts (explicit id) a record. Returns its id.
    pub fn save<T: Record>(&self, record: &mut T) -> Result<Id> {
        let values = record.values()?;
        let columns = T::COLUMNS.join(", ");
        if record.id() == 0 {
            let placeholders = (1..=values.len())
                .map(|i| format!("?{i}"))
                .collect::<Vec<_>>();
            let sql = format!(
                "INSERT INTO {} ({columns}) VALUES ({})",
                T::TABLE,
                placeholders.join(", ")
            );
            self.conn
                .execute(&sql, rusqlite::params_from_iter(values))?;
            record.set_id(self.conn.last_insert_rowid());
        } else {
            let placeholders = (1..=values.len() + 1)
                .map(|i| format!("?{i}"))
                .collect::<Vec<_>>();
            let updates = T::COLUMNS
                .iter()
                .map(|column| format!("{column} = excluded.{column}"))
                .collect::<Vec<_>>();
            let sql = format!(
                "INSERT INTO {} (id, {columns}) VALUES ({}) ON CONFLICT(id) DO UPDATE SET {}",
                T::TABLE,
                placeholders.join(", "),
                updates.join(", ")
            );
            let all = std::iter::once(Value::Integer(record.id())).chain(values);
            self.conn.execute(&sql, rusqlite::params_from_iter(all))?;
        }
        Ok(record.id())
    }

    pub fn get<T: Record>(&self, id: Id) -> Result<Option<T>> {
        let sql = format!(
            "SELECT id, {} FROM {} WHERE id = ?1",
            T::COLUMNS.join(", "),
            T::TABLE
        );
        Ok(self.conn.query_row(&sql, [id], T::from_row).optional()?)
    }

    /// Like [`Store::get`] but a missing row is an [`Error::NotFound`].
    pub fn require<T: Record>(&self, id: Id) -> Result<T> {
        self.get(id)?.ok_or(Error::NotFound { kind: T::KIND, id })
    }

    /// Deletes a row; returns whether it existed.
    pub fn delete<T: Record>(&self, id: Id) -> Result<bool> {
        let sql = format!("DELETE FROM {} WHERE id = ?1", T::TABLE);
        Ok(self.conn.execute(&sql, [id])? > 0)
    }

    /// Selects records with a trailing SQL `clause` (e.g. `"WHERE folder_id = ?1 ORDER BY date"`).
    /// `clause` must be a constant: always pass user values through `params`.
    pub fn query<T: Record>(&self, clause: &str, params: impl Params) -> Result<Vec<T>> {
        let sql = format!(
            "SELECT id, {} FROM {} {clause}",
            T::COLUMNS.join(", "),
            T::TABLE
        );
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(params, T::from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<T>>>()?)
    }

    // ---------- typed queries ----------

    pub fn list_accounts(&self) -> Result<Vec<Account>> {
        self.query("ORDER BY id", [])
    }

    /// Folders of an account, siblings ordered by position (build the tree with `parent_id`).
    pub fn list_folders(&self, account_id: Id) -> Result<Vec<Folder>> {
        self.query(
            "WHERE account_id = ?1 ORDER BY position, name",
            [account_id],
        )
    }

    pub fn folder_by_role(&self, account_id: Id, role: FolderRole) -> Result<Option<Folder>> {
        let folders: Vec<Folder> = self.query(
            "WHERE account_id = ?1 AND role = ?2 ORDER BY id LIMIT 1",
            params![account_id, role.as_str()],
        )?;
        Ok(folders.into_iter().next())
    }

    /// Messages of a folder, newest first.
    pub fn list_messages(&self, folder_id: Id) -> Result<Vec<Message>> {
        self.query(
            "WHERE folder_id = ?1 ORDER BY date DESC, id DESC",
            [folder_id],
        )
    }

    /// All messages of a conversation, oldest first.
    pub fn list_thread(&self, thread_id: &str) -> Result<Vec<Message>> {
        self.query(
            "WHERE thread_id = ?1 ORDER BY date ASC, id ASC",
            [thread_id],
        )
    }

    /// Search with the query language of [`crate::search`] (`from:ada has:attachment ...`),
    /// newest first.
    pub fn search_messages(&self, text: &str, limit: usize) -> Result<Vec<Message>> {
        crate::search::search(self, &crate::search::SearchQuery::parse(text), limit)
    }

    pub fn unread_count(&self, folder_id: Id) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE folder_id = ?1 AND is_read = 0",
            [folder_id],
            |row| row.get(0),
        )?)
    }

    pub fn list_calendars(&self) -> Result<Vec<Calendar>> {
        self.query("ORDER BY account_id, id", [])
    }

    /// Events overlapping `[from, to)` plus every recurring event starting before `to`
    /// (the caller expands recurrences).
    pub fn list_events_between(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<CalendarEvent>> {
        self.query(
            "WHERE start_at < ?2 AND (end_at > ?1 OR recurrence IS NOT NULL) ORDER BY start_at",
            params![timestamp(&from), timestamp(&to)],
        )
    }

    pub fn list_contacts(&self) -> Result<Vec<Contact>> {
        self.query("ORDER BY display_name COLLATE NOCASE", [])
    }

    /// Open tasks first, then by due date (undated last).
    pub fn list_tasks(&self) -> Result<Vec<Task>> {
        self.query("ORDER BY completed, due IS NULL, due, id", [])
    }

    pub fn list_categories(&self) -> Result<Vec<Category>> {
        self.query("ORDER BY name COLLATE NOCASE", [])
    }

    pub fn list_rules(&self, account_id: Id) -> Result<Vec<Rule>> {
        self.query("WHERE account_id = ?1 ORDER BY id", [account_id])
    }

    /// Deletes an account with its mail, folders, calendars, rules and `account.<id>.*`
    /// settings. Contacts and tasks move to another account when there is one (they are not
    /// synced, so deleting them would lose data), otherwise they are deleted too.
    pub fn delete_account(&self, account_id: Id) -> Result<()> {
        self.transaction(|store| {
            let other: Option<Id> = store
                .conn
                .query_row(
                    "SELECT id FROM accounts WHERE id <> ?1 ORDER BY id LIMIT 1",
                    [account_id],
                    |row| row.get(0),
                )
                .optional()?;
            match other {
                Some(other) => {
                    for table in ["contacts", "tasks"] {
                        store.conn.execute(
                            &format!("UPDATE {table} SET account_id = ?1 WHERE account_id = ?2"),
                            [other, account_id],
                        )?;
                    }
                }
                None => {
                    for table in ["contacts", "tasks"] {
                        store.conn.execute(
                            &format!("DELETE FROM {table} WHERE account_id = ?1"),
                            [account_id],
                        )?;
                    }
                }
            }
            const STATEMENTS: &[&str] = &[
                "DELETE FROM message_flag_due WHERE id IN                  (SELECT id FROM messages WHERE account_id = ?1)",
                "DELETE FROM messages WHERE account_id = ?1",
                "DELETE FROM events WHERE calendar_id IN                  (SELECT id FROM calendars WHERE account_id = ?1)",
                "DELETE FROM calendars WHERE account_id = ?1",
                "DELETE FROM rules WHERE account_id = ?1",
                "DELETE FROM signatures WHERE account_id = ?1",
                "DELETE FROM folders WHERE account_id = ?1",
                "DELETE FROM settings WHERE key LIKE 'account.' || ?1 || '.%'",
                "DELETE FROM accounts WHERE id = ?1",
            ];
            for sql in STATEMENTS {
                store.conn.execute(sql, [account_id])?;
            }
            Ok(())
        })
    }
}

// ---------- column conversion helpers ----------

pub(crate) fn timestamp(value: &DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

fn time(value: &DateTime<Utc>) -> Value {
    Value::Text(timestamp(value))
}

fn flag(value: bool) -> Value {
    Value::Integer(i64::from(value))
}

fn json<T: Serialize>(value: &T) -> Result<Value> {
    Ok(Value::Text(serde_json::to_string(value)?))
}

fn conversion_error(
    index: usize,
    err: Box<dyn std::error::Error + Send + Sync>,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, Type::Text, err)
}

fn get_json<T: DeserializeOwned>(row: &Row, index: usize) -> rusqlite::Result<T> {
    let raw: String = row.get(index)?;
    serde_json::from_str(&raw).map_err(|err| conversion_error(index, Box::new(err)))
}

fn get_time(row: &Row, index: usize) -> rusqlite::Result<DateTime<Utc>> {
    let raw: String = row.get(index)?;
    DateTime::parse_from_rfc3339(&raw)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|err| conversion_error(index, Box::new(err)))
}

fn get_enum<T>(row: &Row, index: usize, parse: fn(&str) -> Option<T>) -> rusqlite::Result<T> {
    let raw: String = row.get(index)?;
    parse(&raw).ok_or_else(|| conversion_error(index, format!("unknown value `{raw}`").into()))
}

// ---------- Record impls ----------

impl Record for Account {
    const KIND: &'static str = "account";
    const TABLE: &'static str = "accounts";
    const COLUMNS: &'static [&'static str] = &["name", "email", "color"];

    fn id(&self) -> Id {
        self.id
    }
    fn set_id(&mut self, id: Id) {
        self.id = id;
    }
    fn values(&self) -> Result<Vec<Value>> {
        Ok(vec![
            text(&self.name),
            text(&self.email),
            Value::Integer(i64::from(self.color)),
        ])
    }
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            name: row.get(1)?,
            email: row.get(2)?,
            color: row.get(3)?,
        })
    }
}

impl Record for Folder {
    const KIND: &'static str = "folder";
    const TABLE: &'static str = "folders";
    const COLUMNS: &'static [&'static str] =
        &["account_id", "parent_id", "name", "role", "position"];

    fn id(&self) -> Id {
        self.id
    }
    fn set_id(&mut self, id: Id) {
        self.id = id;
    }
    fn values(&self) -> Result<Vec<Value>> {
        Ok(vec![
            Value::Integer(self.account_id),
            self.parent_id.map_or(Value::Null, Value::Integer),
            text(&self.name),
            text(self.role.as_str()),
            Value::Integer(self.position),
        ])
    }
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            account_id: row.get(1)?,
            parent_id: row.get(2)?,
            name: row.get(3)?,
            role: get_enum(row, 4, FolderRole::parse)?,
            position: row.get(5)?,
        })
    }
}

impl Record for Message {
    const KIND: &'static str = "message";
    const TABLE: &'static str = "messages";
    const COLUMNS: &'static [&'static str] = &[
        "account_id",
        "folder_id",
        "thread_id",
        "from_name",
        "from_email",
        "to_json",
        "cc_json",
        "subject",
        "date",
        "preview",
        "body_text",
        "body_html",
        "is_read",
        "is_flagged",
        "is_pinned",
        "importance",
        "categories_json",
        "attachments_json",
    ];

    fn id(&self) -> Id {
        self.id
    }
    fn set_id(&mut self, id: Id) {
        self.id = id;
    }
    fn values(&self) -> Result<Vec<Value>> {
        Ok(vec![
            Value::Integer(self.account_id),
            Value::Integer(self.folder_id),
            text(&self.thread_id),
            text(&self.from.name),
            text(&self.from.email),
            json(&self.to)?,
            json(&self.cc)?,
            text(&self.subject),
            time(&self.date),
            text(&self.preview),
            text(&self.body_text),
            self.body_html.as_deref().map_or(Value::Null, text),
            flag(self.flags.read),
            flag(self.flags.flagged),
            flag(self.flags.pinned),
            text(self.importance.as_str()),
            json(&self.categories)?,
            json(&self.attachments)?,
        ])
    }
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            account_id: row.get(1)?,
            folder_id: row.get(2)?,
            thread_id: row.get(3)?,
            from: Address {
                name: row.get(4)?,
                email: row.get(5)?,
            },
            to: get_json(row, 6)?,
            cc: get_json(row, 7)?,
            subject: row.get(8)?,
            date: get_time(row, 9)?,
            preview: row.get(10)?,
            body_text: row.get(11)?,
            body_html: row.get(12)?,
            flags: MessageFlags {
                read: row.get(13)?,
                flagged: row.get(14)?,
                pinned: row.get(15)?,
            },
            importance: get_enum(row, 16, Importance::parse)?,
            categories: get_json(row, 17)?,
            attachments: get_json(row, 18)?,
        })
    }
}

impl Record for Calendar {
    const KIND: &'static str = "calendar";
    const TABLE: &'static str = "calendars";
    const COLUMNS: &'static [&'static str] = &["account_id", "name", "color"];

    fn id(&self) -> Id {
        self.id
    }
    fn set_id(&mut self, id: Id) {
        self.id = id;
    }
    fn values(&self) -> Result<Vec<Value>> {
        Ok(vec![
            Value::Integer(self.account_id),
            text(&self.name),
            Value::Integer(i64::from(self.color)),
        ])
    }
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            account_id: row.get(1)?,
            name: row.get(2)?,
            color: row.get(3)?,
        })
    }
}

impl Record for CalendarEvent {
    const KIND: &'static str = "event";
    const TABLE: &'static str = "events";
    const COLUMNS: &'static [&'static str] = &[
        "calendar_id",
        "title",
        "location",
        "description",
        "start_at",
        "end_at",
        "all_day",
        "recurrence",
        "attendees_json",
        "reminders_json",
        "categories_json",
    ];

    fn id(&self) -> Id {
        self.id
    }
    fn set_id(&mut self, id: Id) {
        self.id = id;
    }
    fn values(&self) -> Result<Vec<Value>> {
        Ok(vec![
            Value::Integer(self.calendar_id),
            text(&self.title),
            text(&self.location),
            text(&self.description),
            time(&self.start),
            time(&self.end),
            flag(self.all_day),
            self.recurrence.as_deref().map_or(Value::Null, text),
            json(&self.attendees)?,
            json(&self.reminders)?,
            json(&self.categories)?,
        ])
    }
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            calendar_id: row.get(1)?,
            title: row.get(2)?,
            location: row.get(3)?,
            description: row.get(4)?,
            start: get_time(row, 5)?,
            end: get_time(row, 6)?,
            all_day: row.get(7)?,
            recurrence: row.get(8)?,
            attendees: get_json(row, 9)?,
            reminders: get_json(row, 10)?,
            categories: get_json(row, 11)?,
        })
    }
}

impl Record for Contact {
    const KIND: &'static str = "contact";
    const TABLE: &'static str = "contacts";
    const COLUMNS: &'static [&'static str] = &[
        "account_id",
        "display_name",
        "emails_json",
        "phones_json",
        "company",
        "job_title",
        "notes",
        "favorite",
    ];

    fn id(&self) -> Id {
        self.id
    }
    fn set_id(&mut self, id: Id) {
        self.id = id;
    }
    fn values(&self) -> Result<Vec<Value>> {
        Ok(vec![
            Value::Integer(self.account_id),
            text(&self.display_name),
            json(&self.emails)?,
            json(&self.phones)?,
            text(&self.company),
            text(&self.job_title),
            text(&self.notes),
            flag(self.favorite),
        ])
    }
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            account_id: row.get(1)?,
            display_name: row.get(2)?,
            emails: get_json(row, 3)?,
            phones: get_json(row, 4)?,
            company: row.get(5)?,
            job_title: row.get(6)?,
            notes: row.get(7)?,
            favorite: row.get(8)?,
        })
    }
}

impl Record for Task {
    const KIND: &'static str = "task";
    const TABLE: &'static str = "tasks";
    const COLUMNS: &'static [&'static str] = &[
        "account_id",
        "title",
        "notes",
        "due",
        "completed",
        "importance",
        "categories_json",
        "created",
    ];

    fn id(&self) -> Id {
        self.id
    }
    fn set_id(&mut self, id: Id) {
        self.id = id;
    }
    fn values(&self) -> Result<Vec<Value>> {
        Ok(vec![
            Value::Integer(self.account_id),
            text(&self.title),
            text(&self.notes),
            self.due.map_or(Value::Null, |due| {
                Value::Text(due.format("%Y-%m-%d").to_string())
            }),
            flag(self.completed),
            text(self.importance.as_str()),
            json(&self.categories)?,
            time(&self.created),
        ])
    }
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        let due: Option<String> = row.get(4)?;
        let due = due
            .map(|raw| NaiveDate::parse_from_str(&raw, "%Y-%m-%d"))
            .transpose()
            .map_err(|err| conversion_error(4, Box::new(err)))?;
        Ok(Self {
            id: row.get(0)?,
            account_id: row.get(1)?,
            title: row.get(2)?,
            notes: row.get(3)?,
            due,
            completed: row.get(5)?,
            importance: get_enum(row, 6, Importance::parse)?,
            categories: get_json(row, 7)?,
            created: get_time(row, 8)?,
        })
    }
}

impl Record for Category {
    const KIND: &'static str = "category";
    const TABLE: &'static str = "categories";
    const COLUMNS: &'static [&'static str] = &["name", "color"];

    fn id(&self) -> Id {
        self.id
    }
    fn set_id(&mut self, id: Id) {
        self.id = id;
    }
    fn values(&self) -> Result<Vec<Value>> {
        Ok(vec![
            text(&self.name),
            Value::Integer(i64::from(self.color)),
        ])
    }
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            name: row.get(1)?,
            color: row.get(2)?,
        })
    }
}

impl Record for Rule {
    const KIND: &'static str = "rule";
    const TABLE: &'static str = "rules";
    const COLUMNS: &'static [&'static str] = &[
        "account_id",
        "name",
        "enabled",
        "conditions_json",
        "actions_json",
    ];

    fn id(&self) -> Id {
        self.id
    }
    fn set_id(&mut self, id: Id) {
        self.id = id;
    }
    fn values(&self) -> Result<Vec<Value>> {
        Ok(vec![
            Value::Integer(self.account_id),
            text(&self.name),
            flag(self.enabled),
            json(&self.conditions)?,
            json(&self.actions)?,
        ])
    }
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            account_id: row.get(1)?,
            name: row.get(2)?,
            enabled: row.get(3)?,
            conditions: get_json(row, 4)?,
            actions: get_json(row, 5)?,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use chrono::TimeZone;

    pub(crate) fn store() -> Store {
        Store::open_in_memory().unwrap()
    }

    pub(crate) fn account(store: &Store) -> Account {
        let mut account = Account {
            id: 0,
            name: "Work".into(),
            email: "me@example.com".into(),
            color: 0x3b82f6,
        };
        store.save(&mut account).unwrap();
        account
    }

    pub(crate) fn folder(store: &Store, account_id: Id, role: FolderRole) -> Folder {
        let mut folder = Folder {
            id: 0,
            account_id,
            parent_id: None,
            name: role.as_str().into(),
            role,
            position: 0,
        };
        store.save(&mut folder).unwrap();
        folder
    }

    pub(crate) fn message(account_id: Id, folder_id: Id) -> Message {
        Message {
            id: 0,
            account_id,
            folder_id,
            thread_id: "t1".into(),
            from: Address::new("Ada", "ada@example.com"),
            to: vec![Address::new("Me", "me@example.com")],
            cc: vec![],
            subject: "Hello".into(),
            date: Utc.with_ymd_and_hms(2026, 11, 2, 9, 30, 0).unwrap(),
            preview: "Hi there".into(),
            body_text: "Hi there, how are you?".into(),
            body_html: None,
            flags: MessageFlags::default(),
            importance: Importance::High,
            categories: vec!["Work".into()],
            attachments: vec![Attachment {
                name: "a.pdf".into(),
                mime: "application/pdf".into(),
                size: 42,
            }],
        }
    }

    #[test]
    fn migrations_are_idempotent() {
        let store = store();
        assert_eq!(store.schema_version().unwrap(), MIGRATIONS.len() as i64);
        store.migrate().unwrap();
        assert_eq!(store.schema_version().unwrap(), MIGRATIONS.len() as i64);
    }

    #[test]
    fn message_roundtrip_update_delete() {
        let store = store();
        let account = account(&store);
        let inbox = folder(&store, account.id, FolderRole::Inbox);
        let mut msg = message(account.id, inbox.id);
        let id = store.save(&mut msg).unwrap();
        assert!(id > 0);
        assert_eq!(store.require::<Message>(id).unwrap(), msg);

        msg.flags.read = true;
        msg.subject = "Updated".into();
        store.save(&mut msg).unwrap();
        assert_eq!(store.require::<Message>(id).unwrap(), msg);
        assert_eq!(store.list_messages(inbox.id).unwrap().len(), 1);
        assert_eq!(store.unread_count(inbox.id).unwrap(), 0);
        assert_eq!(store.search_messages("updat", 10).unwrap().len(), 1);

        assert!(store.delete::<Message>(id).unwrap());
        assert!(store.get::<Message>(id).unwrap().is_none());
        assert!(matches!(
            store.require::<Message>(id),
            Err(Error::NotFound { .. })
        ));
    }

    #[test]
    fn other_records_roundtrip() {
        let store = store();
        let account = account(&store);
        let mut calendar = Calendar {
            id: 0,
            account_id: account.id,
            name: "Cal".into(),
            color: 1,
        };
        store.save(&mut calendar).unwrap();
        let start = Utc.with_ymd_and_hms(2026, 11, 3, 10, 0, 0).unwrap();
        let mut event = CalendarEvent {
            id: 0,
            calendar_id: calendar.id,
            title: "Standup".into(),
            location: "Room 1".into(),
            description: String::new(),
            start,
            end: start + chrono::Duration::minutes(15),
            all_day: false,
            recurrence: Some("FREQ=DAILY".into()),
            attendees: vec![Attendee {
                address: Address::new("Bob", "bob@example.com"),
                status: ResponseStatus::Accepted,
                optional: false,
            }],
            reminders: vec![10],
            categories: vec![],
        };
        store.save(&mut event).unwrap();
        assert_eq!(store.require::<CalendarEvent>(event.id).unwrap(), event);
        let later = start + chrono::Duration::days(30);
        assert_eq!(
            store
                .list_events_between(later, later + chrono::Duration::days(1))
                .unwrap()
                .len(),
            1
        );

        let mut contact = Contact {
            id: 0,
            account_id: account.id,
            display_name: "Bob".into(),
            emails: vec!["bob@example.com".into()],
            phones: vec![],
            company: "Acme".into(),
            job_title: "CTO".into(),
            notes: String::new(),
            favorite: true,
        };
        store.save(&mut contact).unwrap();
        assert_eq!(store.list_contacts().unwrap(), vec![contact]);

        let mut task = Task {
            id: 0,
            account_id: account.id,
            title: "Ship".into(),
            notes: String::new(),
            due: NaiveDate::from_ymd_opt(2026, 11, 20),
            completed: false,
            importance: Importance::Normal,
            categories: vec![],
            created: start,
        };
        store.save(&mut task).unwrap();
        assert_eq!(store.list_tasks().unwrap(), vec![task]);

        let mut rule = Rule {
            id: 0,
            account_id: account.id,
            name: "News".into(),
            enabled: true,
            conditions: vec![RuleCondition::SubjectContains("news".into())],
            actions: vec![RuleAction::MarkRead],
        };
        store.save(&mut rule).unwrap();
        assert_eq!(store.list_rules(account.id).unwrap(), vec![rule]);

        let mut category = Category {
            id: 0,
            name: "Red".into(),
            color: 0xff0000,
        };
        store.save(&mut category).unwrap();
        assert_eq!(store.list_categories().unwrap(), vec![category]);
    }

    #[test]
    fn folder_with_messages_cannot_be_deleted() {
        let store = store();
        let account = account(&store);
        let inbox = folder(&store, account.id, FolderRole::Inbox);
        store.save(&mut message(account.id, inbox.id)).unwrap();
        assert!(store.delete::<Folder>(inbox.id).is_err());
        assert_eq!(
            store.folder_by_role(account.id, FolderRole::Inbox).unwrap(),
            Some(inbox)
        );
    }

    #[test]
    fn transaction_rolls_back_on_error() {
        let store = store();
        let result: Result<()> = store.transaction(|store| {
            account(store);
            Err(Error::Invalid("boom".into()))
        });
        assert!(result.is_err());
        assert!(store.list_accounts().unwrap().is_empty());
    }

    #[test]
    fn delete_account_removes_its_data_only() {
        let store = store();
        let gone = account(&store);
        let kept = account(&store);
        let inbox = folder(&store, gone.id, FolderRole::Inbox);
        let mut child = folder(&store, gone.id, FolderRole::Custom);
        child.parent_id = Some(inbox.id);
        store.save(&mut child).unwrap();
        store.save(&mut message(gone.id, child.id)).unwrap();
        let other_inbox = folder(&store, kept.id, FolderRole::Inbox);
        store.save(&mut message(kept.id, other_inbox.id)).unwrap();
        crate::settings::apply_set(
            &store,
            format!("account.{}.servers", gone.id),
            Some("{}".into()),
        )
        .unwrap();

        crate::command::apply(&store, crate::Command::DeleteAccount(gone.id)).unwrap();

        let accounts = store.list_accounts().unwrap();
        assert_eq!(
            accounts.iter().map(|a| a.id).collect::<Vec<_>>(),
            vec![kept.id]
        );
        assert!(store.list_folders(gone.id).unwrap().is_empty());
        assert_eq!(store.list_messages(other_inbox.id).unwrap().len(), 1);
        assert_eq!(
            crate::settings::get(&store, &format!("account.{}.servers", gone.id)).unwrap(),
            None
        );
    }
}
