//! Mail operations built on [`Command`]: copy, follow-up flags with due dates, moves to a
//! special folder (archive, junk, trash), two-step delete, folder create/rename/delete and
//! category toggling. Builders return a `Command` to run through `History::run` (undoable).

use std::collections::{HashMap, HashSet};

use chrono::{Datelike, Duration, NaiveDate};
use rusqlite::Row;
use rusqlite::types::{Type, Value};

use crate::command::Command;
use crate::error::{Error, Result};
use crate::model::{Folder, FolderRole, Id, Message};
use crate::store::{Record, Store};

/// Due date of a flagged message. Stored in its own table keyed by message id, so the
/// shared `Message` model stays unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlagDue {
    pub message_id: Id,
    pub due: NaiveDate,
}

const DATE_FORMAT: &str = "%Y-%m-%d";

impl Record for FlagDue {
    const KIND: &'static str = "flag due date";
    const TABLE: &'static str = "message_flag_due";
    const COLUMNS: &'static [&'static str] = &["due"];

    fn id(&self) -> Id {
        self.message_id
    }
    fn set_id(&mut self, id: Id) {
        self.message_id = id;
    }
    fn values(&self) -> Result<Vec<Value>> {
        Ok(vec![Value::Text(self.due.format(DATE_FORMAT).to_string())])
    }
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        let text: String = row.get(1)?;
        let due = NaiveDate::parse_from_str(&text, DATE_FORMAT).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(1, Type::Text, Box::new(err))
        })?;
        Ok(Self {
            message_id: row.get(0)?,
            due,
        })
    }
}

/// Due date of a message's flag, if any (only meaningful while the message is flagged).
pub fn flag_due(store: &Store, message_id: Id) -> Result<Option<NaiveDate>> {
    Ok(store.get::<FlagDue>(message_id)?.map(|flag| flag.due))
}

/// Every stored due date, by message id (cheap: one row per flagged message with a date).
pub fn flag_dues(store: &Store) -> Result<HashMap<Id, NaiveDate>> {
    Ok(store
        .query::<FlagDue>("ORDER BY id", [])?
        .into_iter()
        .map(|flag| (flag.message_id, flag.due))
        .collect())
}

/// Follow-up presets offered by the "Flag" menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagPreset {
    Today,
    Tomorrow,
    ThisWeek,
    NextWeek,
    NoDate,
}

impl FlagPreset {
    pub const ALL: [FlagPreset; 5] = [
        Self::Today,
        Self::Tomorrow,
        Self::ThisWeek,
        Self::NextWeek,
        Self::NoDate,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Today => "Today",
            Self::Tomorrow => "Tomorrow",
            Self::ThisWeek => "This week",
            Self::NextWeek => "Next week",
            Self::NoDate => "No date",
        }
    }

    /// Due date relative to `today`: "This week" is Friday (or today on weekends),
    /// "Next week" is next Monday.
    pub fn due(self, today: NaiveDate) -> Option<NaiveDate> {
        let weekday = i64::from(today.weekday().num_days_from_monday());
        match self {
            Self::Today => Some(today),
            Self::Tomorrow => Some(today + Duration::days(1)),
            Self::ThisWeek => Some(today + Duration::days((4 - weekday).max(0))),
            Self::NextWeek => Some(today + Duration::days(7 - weekday)),
            Self::NoDate => None,
        }
    }
}

/// `FlagMessages`: sets the follow-up flag and its due date (cleared when unflagged).
pub(crate) fn apply_flag(
    store: &Store,
    ids: &[Id],
    flagged: bool,
    due: Option<NaiveDate>,
) -> Result<Command> {
    let due = due.filter(|_| flagged);
    let mut inverses = Vec::with_capacity(ids.len());
    for &id in ids {
        let mut message = store.require::<Message>(id)?;
        let previous_due = flag_due(store, id)?;
        if message.flags.flagged == flagged && previous_due == due {
            continue;
        }
        let previous_flagged = message.flags.flagged;
        if previous_flagged != flagged {
            message.flags.flagged = flagged;
            store.save(&mut message)?;
        }
        match due {
            Some(due) => {
                store.save(&mut FlagDue {
                    message_id: id,
                    due,
                })?;
            }
            None => {
                store.delete::<FlagDue>(id)?;
            }
        }
        inverses.push(Command::FlagMessages {
            ids: vec![id],
            flagged: previous_flagged,
            due: previous_due,
        });
    }
    Ok(Command::Batch(inverses))
}

/// `CopyMessages`: duplicates messages into `folder_id`; the inverse deletes the copies.
pub(crate) fn apply_copy(store: &Store, ids: &[Id], folder_id: Id) -> Result<Command> {
    let folder = store.require::<Folder>(folder_id)?;
    let mut copies = Vec::with_capacity(ids.len());
    for &id in ids {
        let mut copy = store.require::<Message>(id)?;
        copy.id = 0;
        copy.folder_id = folder.id;
        copy.account_id = folder.account_id;
        let copy_id = store.save(&mut copy)?;
        // A reused rowid must not inherit a stale due date.
        store.delete::<FlagDue>(copy_id)?;
        copies.push(copy_id);
    }
    Ok(Command::DeleteMessages { ids: copies })
}

fn load(store: &Store, ids: &[Id]) -> Result<Vec<Message>> {
    ids.iter().map(|&id| store.require::<Message>(id)).collect()
}

fn role_folder(store: &Store, account_id: Id, role: FolderRole) -> Result<Folder> {
    store
        .folder_by_role(account_id, role)?
        .ok_or_else(|| Error::Invalid(format!("this account has no {} folder", role.as_str())))
}

/// Moves each message to its own account's folder with `role` (archive, junk, trash...).
/// Messages already there are skipped.
pub fn move_to_role(store: &Store, ids: &[Id], role: FolderRole) -> Result<Command> {
    let mut groups: Vec<(Id, Vec<Id>)> = Vec::new();
    for message in load(store, ids)? {
        let target = role_folder(store, message.account_id, role)?;
        if message.folder_id == target.id {
            continue;
        }
        match groups
            .iter_mut()
            .find(|(folder_id, _)| *folder_id == target.id)
        {
            Some((_, ids)) => ids.push(message.id),
            None => groups.push((target.id, vec![message.id])),
        }
    }
    Ok(Command::Batch(
        groups
            .into_iter()
            .map(|(folder_id, ids)| Command::MoveMessages { ids, folder_id })
            .collect(),
    ))
}

/// What a "Delete" does: first to Deleted Items, then (from there) permanently.
#[derive(Debug, Clone, PartialEq)]
pub enum Deletion {
    /// Soft delete; run directly.
    MoveToTrash(Command),
    /// Every message is already in Deleted Items: confirm with the user before running.
    Permanent(Command),
}

pub fn delete_messages(store: &Store, ids: &[Id]) -> Result<Deletion> {
    let messages = load(store, ids)?;
    let mut in_trash = Vec::new();
    let mut elsewhere = Vec::new();
    for message in &messages {
        let trash = role_folder(store, message.account_id, FolderRole::Trash)?;
        if message.folder_id == trash.id {
            in_trash.push(message.id);
        } else {
            elsewhere.push(message.id);
        }
    }
    Ok(if elsewhere.is_empty() {
        Deletion::Permanent(Command::DeleteMessages { ids: in_trash })
    } else {
        Deletion::MoveToTrash(move_to_role(store, &elsewhere, FolderRole::Trash)?)
    })
}

/// Adds `name` to every message, or removes it when they all have it already.
pub fn toggle_category(store: &Store, ids: &[Id], name: &str) -> Result<Command> {
    let messages = load(store, ids)?;
    let all_have = messages
        .iter()
        .all(|message| message.categories.iter().any(|c| c == name));
    Ok(Command::Batch(
        messages
            .into_iter()
            .map(|message| {
                let mut categories = message.categories;
                if all_have {
                    categories.retain(|c| c != name);
                } else if !categories.iter().any(|c| c == name) {
                    categories.push(name.to_owned());
                }
                Command::SetMessageCategories {
                    ids: vec![message.id],
                    categories,
                }
            })
            .collect(),
    ))
}

/// Marks every unread message of a folder as read.
pub fn mark_folder_read(store: &Store, folder_id: Id) -> Result<Command> {
    let ids = store
        .list_messages(folder_id)?
        .into_iter()
        .filter(|message| !message.flags.read)
        .map(|message| message.id)
        .collect();
    Ok(Command::SetMessageFlags {
        ids,
        read: Some(true),
        flagged: None,
        pinned: None,
    })
}

fn clean_name(
    store: &Store,
    account_id: Id,
    parent_id: Option<Id>,
    name: &str,
    except: Id,
) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::Invalid("folder name can't be empty".into()));
    }
    let taken = store.list_folders(account_id)?.into_iter().any(|folder| {
        folder.id != except
            && folder.parent_id == parent_id
            && folder.name.eq_ignore_ascii_case(name)
    });
    if taken {
        return Err(Error::Invalid(format!(
            "a folder named \"{name}\" already exists here"
        )));
    }
    Ok(name.to_owned())
}

/// Creates a custom folder at the end of its siblings.
pub fn new_folder(
    store: &Store,
    account_id: Id,
    parent_id: Option<Id>,
    name: &str,
) -> Result<Command> {
    let name = clean_name(store, account_id, parent_id, name, 0)?;
    let position = store
        .list_folders(account_id)?
        .iter()
        .filter(|folder| folder.parent_id == parent_id)
        .map(|folder| folder.position + 1)
        .max()
        .unwrap_or(0);
    Ok(Command::SaveFolder(Box::new(Folder {
        id: 0,
        account_id,
        parent_id,
        name,
        role: FolderRole::Custom,
        position,
    })))
}

pub fn rename_folder(store: &Store, folder_id: Id, name: &str) -> Result<Command> {
    let mut folder = store.require::<Folder>(folder_id)?;
    folder.name = clean_name(store, folder.account_id, folder.parent_id, name, folder.id)?;
    Ok(Command::SaveFolder(Box::new(folder)))
}

/// Deletes a custom folder and its subfolders; their messages go to Deleted Items.
pub fn delete_folder(store: &Store, folder_id: Id) -> Result<Command> {
    let folder = store.require::<Folder>(folder_id)?;
    if folder.role != FolderRole::Custom {
        return Err(Error::Invalid("special folders can't be deleted".into()));
    }
    let trash = role_folder(store, folder.account_id, FolderRole::Trash)?;
    let folders = store.list_folders(folder.account_id)?;

    // Collect the subtree, parents before children.
    let mut subtree = vec![folder.id];
    let mut index = 0;
    while index < subtree.len() {
        let parent = subtree[index];
        subtree.extend(
            folders
                .iter()
                .filter(|f| f.parent_id == Some(parent))
                .map(|f| f.id),
        );
        index += 1;
    }
    let seen: HashSet<Id> = subtree.iter().copied().collect();
    if seen.contains(&trash.id) {
        return Err(Error::Invalid("this folder contains Deleted Items".into()));
    }

    let mut commands = Vec::new();
    for &id in &subtree {
        let ids: Vec<Id> = store.list_messages(id)?.into_iter().map(|m| m.id).collect();
        if !ids.is_empty() {
            commands.push(Command::MoveMessages {
                ids,
                folder_id: trash.id,
            });
        }
    }
    commands.extend(subtree.iter().rev().map(|&id| Command::DeleteFolder(id)));
    Ok(Command::Batch(commands))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::History;
    use crate::store::tests::{account, folder, message, store};

    struct Fixture {
        store: Store,
        inbox: Folder,
        archive: Folder,
        trash: Folder,
        msg: Message,
    }

    fn setup() -> Fixture {
        let store = store();
        let account = account(&store);
        let inbox = folder(&store, account.id, FolderRole::Inbox);
        let archive = folder(&store, account.id, FolderRole::Archive);
        let trash = folder(&store, account.id, FolderRole::Trash);
        let mut msg = message(account.id, inbox.id);
        store.save(&mut msg).unwrap();
        Fixture {
            store,
            inbox,
            archive,
            trash,
            msg,
        }
    }

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn flag_with_due_date_undo_redo() {
        let f = setup();
        let mut history = History::new();
        let due = date(2026, 11, 20);
        let command = Command::FlagMessages {
            ids: vec![f.msg.id],
            flagged: true,
            due: Some(due),
        };
        assert_eq!(command.label(), "Flag message");
        history.run(&f.store, command).unwrap();
        assert!(f.store.require::<Message>(f.msg.id).unwrap().flags.flagged);
        assert_eq!(flag_due(&f.store, f.msg.id).unwrap(), Some(due));
        assert_eq!(flag_dues(&f.store).unwrap().get(&f.msg.id), Some(&due));

        history.undo(&f.store).unwrap();
        assert_eq!(f.store.require::<Message>(f.msg.id).unwrap(), f.msg);
        assert_eq!(flag_due(&f.store, f.msg.id).unwrap(), None);

        history.redo(&f.store).unwrap();
        assert_eq!(flag_due(&f.store, f.msg.id).unwrap(), Some(due));

        // Unflagging clears the date; undo restores both.
        history
            .run(
                &f.store,
                Command::FlagMessages {
                    ids: vec![f.msg.id],
                    flagged: false,
                    due: Some(due),
                },
            )
            .unwrap();
        assert!(!f.store.require::<Message>(f.msg.id).unwrap().flags.flagged);
        assert_eq!(flag_due(&f.store, f.msg.id).unwrap(), None);
        history.undo(&f.store).unwrap();
        assert!(f.store.require::<Message>(f.msg.id).unwrap().flags.flagged);
        assert_eq!(flag_due(&f.store, f.msg.id).unwrap(), Some(due));
    }

    #[test]
    fn copy_creates_and_undo_removes_copies() {
        let f = setup();
        let mut history = History::new();
        let command = Command::CopyMessages {
            ids: vec![f.msg.id],
            folder_id: f.archive.id,
        };
        assert_eq!(command.label(), "Copy message");
        history.run(&f.store, command).unwrap();
        let copies = f.store.list_messages(f.archive.id).unwrap();
        assert_eq!(copies.len(), 1);
        assert_ne!(copies[0].id, f.msg.id);
        assert_eq!(copies[0].subject, f.msg.subject);
        assert_eq!(f.store.list_messages(f.inbox.id).unwrap().len(), 1);

        history.undo(&f.store).unwrap();
        assert!(f.store.list_messages(f.archive.id).unwrap().is_empty());
        history.redo(&f.store).unwrap();
        assert_eq!(f.store.list_messages(f.archive.id).unwrap(), copies);
    }

    #[test]
    fn archive_and_two_step_delete() {
        let f = setup();
        let mut history = History::new();
        let archive = move_to_role(&f.store, &[f.msg.id], FolderRole::Archive).unwrap();
        assert_eq!(archive.label(), "Move message");
        history.run(&f.store, archive).unwrap();
        assert_eq!(f.store.list_messages(f.archive.id).unwrap().len(), 1);

        let Deletion::MoveToTrash(soft) = delete_messages(&f.store, &[f.msg.id]).unwrap() else {
            panic!("first delete must go to Deleted Items");
        };
        history.run(&f.store, soft).unwrap();
        assert_eq!(f.store.list_messages(f.trash.id).unwrap().len(), 1);

        let Deletion::Permanent(hard) = delete_messages(&f.store, &[f.msg.id]).unwrap() else {
            panic!("second delete must be permanent");
        };
        history.run(&f.store, hard).unwrap();
        assert!(f.store.get::<Message>(f.msg.id).unwrap().is_none());

        for _ in 0..3 {
            history.undo(&f.store).unwrap();
        }
        assert_eq!(f.store.require::<Message>(f.msg.id).unwrap(), f.msg);
    }

    #[test]
    fn toggle_category_adds_then_removes() {
        let f = setup();
        let mut history = History::new();
        let add = toggle_category(&f.store, &[f.msg.id], "Urgent").unwrap();
        history.run(&f.store, add).unwrap();
        let categories = f.store.require::<Message>(f.msg.id).unwrap().categories;
        assert_eq!(categories, vec!["Work".to_owned(), "Urgent".to_owned()]);

        let remove = toggle_category(&f.store, &[f.msg.id], "Work").unwrap();
        history.run(&f.store, remove).unwrap();
        let categories = f.store.require::<Message>(f.msg.id).unwrap().categories;
        assert_eq!(categories, vec!["Urgent".to_owned()]);

        history.undo(&f.store).unwrap();
        history.undo(&f.store).unwrap();
        assert_eq!(f.store.require::<Message>(f.msg.id).unwrap(), f.msg);
    }

    #[test]
    fn folder_create_rename_delete_tree() {
        let f = setup();
        let mut history = History::new();
        let account_id = f.inbox.account_id;
        history
            .run(
                &f.store,
                new_folder(&f.store, account_id, Some(f.inbox.id), "Projects").unwrap(),
            )
            .unwrap();
        let projects = f
            .store
            .list_folders(account_id)
            .unwrap()
            .into_iter()
            .find(|folder| folder.name == "Projects")
            .unwrap();
        assert!(new_folder(&f.store, account_id, Some(f.inbox.id), "projects").is_err());
        assert!(new_folder(&f.store, account_id, None, "  ").is_err());
        history
            .run(
                &f.store,
                new_folder(&f.store, account_id, Some(projects.id), "Atlas").unwrap(),
            )
            .unwrap();
        history
            .run(
                &f.store,
                rename_folder(&f.store, projects.id, "Work").unwrap(),
            )
            .unwrap();
        assert_eq!(f.store.require::<Folder>(projects.id).unwrap().name, "Work");

        let atlas = f
            .store
            .list_folders(account_id)
            .unwrap()
            .into_iter()
            .find(|folder| folder.name == "Atlas")
            .unwrap();
        history
            .run(
                &f.store,
                Command::MoveMessages {
                    ids: vec![f.msg.id],
                    folder_id: atlas.id,
                },
            )
            .unwrap();
        let before = f.store.list_folders(account_id).unwrap();

        history
            .run(&f.store, delete_folder(&f.store, projects.id).unwrap())
            .unwrap();
        assert!(f.store.get::<Folder>(projects.id).unwrap().is_none());
        assert!(f.store.get::<Folder>(atlas.id).unwrap().is_none());
        assert_eq!(f.store.list_messages(f.trash.id).unwrap().len(), 1);
        assert!(delete_folder(&f.store, f.inbox.id).is_err());

        history.undo(&f.store).unwrap();
        assert_eq!(f.store.list_folders(account_id).unwrap(), before);
        assert_eq!(
            f.store.require::<Message>(f.msg.id).unwrap().folder_id,
            atlas.id
        );
    }

    #[test]
    fn mark_folder_read_is_reversible() {
        let f = setup();
        let mut history = History::new();
        history
            .run(&f.store, mark_folder_read(&f.store, f.inbox.id).unwrap())
            .unwrap();
        assert_eq!(f.store.unread_count(f.inbox.id).unwrap(), 0);
        history.undo(&f.store).unwrap();
        assert_eq!(f.store.unread_count(f.inbox.id).unwrap(), 1);
    }

    #[test]
    fn flag_presets() {
        let wednesday = date(2026, 11, 11);
        assert_eq!(FlagPreset::Today.due(wednesday), Some(wednesday));
        assert_eq!(
            FlagPreset::Tomorrow.due(wednesday),
            Some(date(2026, 11, 12))
        );
        assert_eq!(
            FlagPreset::ThisWeek.due(wednesday),
            Some(date(2026, 11, 13))
        );
        assert_eq!(
            FlagPreset::NextWeek.due(wednesday),
            Some(date(2026, 11, 16))
        );
        assert_eq!(FlagPreset::NoDate.due(wednesday), None);
        let saturday = date(2026, 11, 14);
        assert_eq!(FlagPreset::ThisWeek.due(saturday), Some(saturday));
        assert_eq!(FlagPreset::NextWeek.due(saturday), Some(date(2026, 11, 16)));
    }
}
