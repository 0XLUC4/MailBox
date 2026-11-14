//! Every mutation is a [`Command`]. Applying one returns its inverse command, which gives
//! undo/redo for free ([`History`]) and a serializable API for a future CLI/agent.

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::model::*;
use crate::store::{Record, Store};

/// Maximum number of undo steps kept.
const HISTORY_LIMIT: usize = 200;

/// A mutation of the store. `Save*` with id 0 creates, otherwise creates-or-updates with that id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "args", rename_all = "snake_case")]
pub enum Command {
    /// `None` leaves a flag unchanged.
    SetMessageFlags {
        ids: Vec<Id>,
        read: Option<bool>,
        flagged: Option<bool>,
        pinned: Option<bool>,
    },
    MoveMessages {
        ids: Vec<Id>,
        folder_id: Id,
    },
    SetMessageCategories {
        ids: Vec<Id>,
        categories: Vec<String>,
    },
    /// Permanent delete (use `MoveMessages` to the trash folder for a soft delete).
    DeleteMessages {
        ids: Vec<Id>,
    },
    SaveMessage(Box<Message>),
    SaveAccount(Box<Account>),
    SaveFolder(Box<Folder>),
    DeleteFolder(Id),
    SaveCalendar(Box<Calendar>),
    DeleteCalendar(Id),
    SaveEvent(Box<CalendarEvent>),
    DeleteEvent(Id),
    SaveContact(Box<Contact>),
    DeleteContact(Id),
    SaveTask(Box<Task>),
    DeleteTask(Id),
    SetTaskCompleted {
        id: Id,
        completed: bool,
    },
    SaveCategory(Box<Category>),
    DeleteCategory(Id),
    SaveRule(Box<Rule>),
    DeleteRule(Id),
    /// Applied in order, atomically.
    Batch(Vec<Command>),
    /// Duplicates messages into a folder (new ids); undo deletes the copies.
    CopyMessages {
        ids: Vec<Id>,
        folder_id: Id,
    },
    /// Sets or clears the follow-up flag; `due` is kept only while flagged.
    FlagMessages {
        ids: Vec<Id>,
        flagged: bool,
        due: Option<chrono::NaiveDate>,
    },
    /// Contacts with details, groups, merge (see [`crate::people`]).
    People(crate::people::PeopleCommand),
    /// Tasks with details and lists (see [`crate::todo`]).
    Todo(crate::todo::TaskCommand),
    /// Key/value preference (JSON text); `None` removes the key. See [`crate::settings`].
    SetSetting {
        key: String,
        value: Option<String>,
    },
    /// Compose: save a draft message with its metadata (see `compose`).
    SaveDraft(Box<crate::compose::Draft>),
    /// Compose: delete a draft message (by message id) and its metadata.
    DeleteDraft(Id),
    /// Compose: move a draft into the Outbox folder, where the sync engine sends it.
    QueueSend {
        message_id: Id,
        folder_id: Id,
    },
    SaveSignature(Box<crate::compose::Signature>),
    /// Calendar operation with details and recurrence exceptions (see [`crate::calendar`]).
    Calendar(Box<crate::calendar::CalendarCommand>),
}

impl Command {
    /// Short human label, used for "Undo <label>" and toasts.
    pub fn label(&self) -> String {
        let count = |ids: &[Id]| {
            if ids.len() == 1 {
                "message".to_owned()
            } else {
                format!("{} messages", ids.len())
            }
        };
        match self {
            Self::SetMessageFlags {
                ids,
                read: Some(true),
                flagged: None,
                pinned: None,
            } => {
                format!("Mark {} as read", count(ids))
            }
            Self::SetMessageFlags {
                ids,
                read: Some(false),
                flagged: None,
                pinned: None,
            } => {
                format!("Mark {} as unread", count(ids))
            }
            Self::SetMessageFlags {
                ids,
                read: None,
                flagged: Some(flag),
                pinned: None,
            } => {
                format!("{} {}", if *flag { "Flag" } else { "Unflag" }, count(ids))
            }
            Self::SetMessageFlags {
                ids,
                read: None,
                flagged: None,
                pinned: Some(pin),
            } => {
                format!("{} {}", if *pin { "Pin" } else { "Unpin" }, count(ids))
            }
            Self::SetMessageFlags { ids, .. } => format!("Update {}", count(ids)),
            Self::MoveMessages { ids, .. } => format!("Move {}", count(ids)),
            Self::SetMessageCategories { ids, .. } => format!("Categorize {}", count(ids)),
            Self::DeleteMessages { ids } => format!("Delete {}", count(ids)),
            Self::SaveMessage(_) => "Save message".into(),
            Self::SaveAccount(_) => "Save account".into(),
            Self::SaveFolder(_) => "Save folder".into(),
            Self::DeleteFolder(_) => "Delete folder".into(),
            Self::SaveCalendar(_) => "Save calendar".into(),
            Self::DeleteCalendar(_) => "Delete calendar".into(),
            Self::SaveEvent(_) => "Save event".into(),
            Self::DeleteEvent(_) => "Delete event".into(),
            Self::SaveContact(_) => "Save contact".into(),
            Self::DeleteContact(_) => "Delete contact".into(),
            Self::SaveTask(_) => "Save task".into(),
            Self::DeleteTask(_) => "Delete task".into(),
            Self::SetTaskCompleted {
                completed: true, ..
            } => "Complete task".into(),
            Self::SetTaskCompleted {
                completed: false, ..
            } => "Reopen task".into(),
            Self::SaveCategory(_) => "Save category".into(),
            Self::DeleteCategory(_) => "Delete category".into(),
            Self::SaveRule(_) => "Save rule".into(),
            Self::DeleteRule(_) => "Delete rule".into(),
            Self::Batch(commands) => match commands.as_slice() {
                [single] => single.label(),
                _ => format!("{} changes", commands.len()),
            },
            Self::CopyMessages { ids, .. } => format!("Copy {}", count(ids)),
            Self::FlagMessages { ids, flagged, .. } => {
                format!(
                    "{} {}",
                    if *flagged { "Flag" } else { "Unflag" },
                    count(ids)
                )
            }
            Self::People(command) => command.label(),
            Self::Todo(command) => command.label(),
            Self::SetSetting { .. } => "Change settings".into(),
            Self::SaveDraft(_) => "Save draft".into(),
            Self::DeleteDraft(_) => "Discard draft".into(),
            Self::QueueSend { .. } => "Send message".into(),
            Self::SaveSignature(_) => "Save signature".into(),
            Self::Calendar(command) => command.label(),
        }
    }
}

/// Applies `command` atomically and returns the command that reverts it.
pub fn apply(store: &Store, command: Command) -> Result<Command> {
    store.transaction(|store| apply_inner(store, command))
}

fn apply_inner(store: &Store, command: Command) -> Result<Command> {
    match command {
        Command::SetMessageFlags {
            ids,
            read,
            flagged,
            pinned,
        } => update_messages(store, &ids, |message| {
            if let Some(read) = read {
                message.flags.read = read;
            }
            if let Some(flagged) = flagged {
                message.flags.flagged = flagged;
            }
            if let Some(pinned) = pinned {
                message.flags.pinned = pinned;
            }
        }),
        Command::MoveMessages { ids, folder_id } => {
            let folder = store.require::<Folder>(folder_id)?;
            update_messages(store, &ids, |message| {
                message.folder_id = folder.id;
                message.account_id = folder.account_id;
            })
        }
        Command::SetMessageCategories { ids, categories } => {
            update_messages(store, &ids, |message| {
                message.categories = categories.clone()
            })
        }
        Command::DeleteMessages { ids } => {
            let inverses = ids
                .iter()
                .map(|&id| delete::<Message>(store, id, Command::SaveMessage))
                .collect::<Result<Vec<_>>>()?;
            Ok(Command::Batch(inverses))
        }
        Command::SaveMessage(message) => save(store, *message, Command::SaveMessage, |id| {
            Command::DeleteMessages { ids: vec![id] }
        }),
        Command::SaveAccount(account) => {
            // Accounts are never deleted through commands: undoing a creation is not supported.
            let previous = store.get::<Account>(account.id)?;
            let mut account = *account;
            store.save(&mut account)?;
            Ok(Command::Batch(
                previous
                    .map(|old| Command::SaveAccount(Box::new(old)))
                    .into_iter()
                    .collect(),
            ))
        }
        Command::SaveFolder(folder) => {
            save(store, *folder, Command::SaveFolder, Command::DeleteFolder)
        }
        Command::DeleteFolder(id) => delete::<Folder>(store, id, Command::SaveFolder),
        Command::SaveCalendar(calendar) => save(
            store,
            *calendar,
            Command::SaveCalendar,
            Command::DeleteCalendar,
        ),
        Command::DeleteCalendar(id) => delete::<Calendar>(store, id, Command::SaveCalendar),
        Command::SaveEvent(event) => save(store, *event, Command::SaveEvent, Command::DeleteEvent),
        Command::DeleteEvent(id) => delete::<CalendarEvent>(store, id, Command::SaveEvent),
        Command::SaveContact(contact) => save(
            store,
            *contact,
            Command::SaveContact,
            Command::DeleteContact,
        ),
        Command::DeleteContact(id) => delete::<Contact>(store, id, Command::SaveContact),
        Command::SaveTask(task) => save(store, *task, Command::SaveTask, Command::DeleteTask),
        Command::DeleteTask(id) => delete::<Task>(store, id, Command::SaveTask),
        Command::SetTaskCompleted { id, completed } => {
            let mut task = store.require::<Task>(id)?;
            let previous = task.clone();
            task.completed = completed;
            store.save(&mut task)?;
            Ok(Command::SaveTask(Box::new(previous)))
        }
        Command::SaveCategory(category) => save(
            store,
            *category,
            Command::SaveCategory,
            Command::DeleteCategory,
        ),
        Command::DeleteCategory(id) => delete::<Category>(store, id, Command::SaveCategory),
        Command::SaveRule(rule) => save(store, *rule, Command::SaveRule, Command::DeleteRule),
        Command::DeleteRule(id) => delete::<Rule>(store, id, Command::SaveRule),
        Command::Batch(commands) => {
            let mut inverses = commands
                .into_iter()
                .map(|command| apply_inner(store, command))
                .collect::<Result<Vec<_>>>()?;
            inverses.reverse();
            Ok(Command::Batch(inverses))
        }
        Command::CopyMessages { ids, folder_id } => {
            crate::mail_ops::apply_copy(store, &ids, folder_id)
        }
        Command::FlagMessages { ids, flagged, due } => {
            crate::mail_ops::apply_flag(store, &ids, flagged, due)
        }
        Command::People(command) => crate::people::apply(store, command),
        Command::Todo(command) => crate::todo::apply(store, command),
        Command::SetSetting { key, value } => crate::settings::apply_set(store, key, value),
        Command::SaveDraft(draft) => crate::compose::apply_save_draft(store, *draft),
        Command::DeleteDraft(id) => crate::compose::apply_delete_draft(store, id),
        Command::QueueSend {
            message_id,
            folder_id,
        } => crate::compose::apply_queue_send(store, message_id, folder_id),
        Command::SaveSignature(signature) => {
            crate::compose::apply_save_signature(store, *signature)
        }
        Command::Calendar(command) => crate::calendar::apply(store, *command),
    }
}

/// Saves `record`; the inverse restores the previous version or deletes the new row.
fn save<T: Record>(
    store: &Store,
    mut record: T,
    restore: fn(Box<T>) -> Command,
    remove: fn(Id) -> Command,
) -> Result<Command> {
    let previous = if record.id() == 0 {
        None
    } else {
        store.get::<T>(record.id())?
    };
    let id = store.save(&mut record)?;
    Ok(match previous {
        Some(old) => restore(Box::new(old)),
        None => remove(id),
    })
}

/// Deletes a row; the inverse re-creates it with the same id.
fn delete<T: Record>(store: &Store, id: Id, restore: fn(Box<T>) -> Command) -> Result<Command> {
    let old = store.require::<T>(id)?;
    store.delete::<T>(id)?;
    Ok(restore(Box::new(old)))
}

fn update_messages(store: &Store, ids: &[Id], change: impl Fn(&mut Message)) -> Result<Command> {
    let mut inverses = Vec::with_capacity(ids.len());
    for &id in ids {
        let mut message = store.require::<Message>(id)?;
        let previous = message.clone();
        change(&mut message);
        if message != previous {
            store.save(&mut message)?;
            inverses.push(Command::SaveMessage(Box::new(previous)));
        }
    }
    Ok(Command::Batch(inverses))
}

#[derive(Debug, Clone)]
struct Entry {
    label: String,
    command: Command,
}

/// Undo/redo stacks. Run every user mutation through [`History::run`].
#[derive(Debug, Default)]
pub struct History {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies `command`, records its inverse and clears the redo stack.
    pub fn run(&mut self, store: &Store, command: Command) -> Result<()> {
        let label = command.label();
        let inverse = apply(store, command)?;
        self.undo.push(Entry {
            label,
            command: inverse,
        });
        if self.undo.len() > HISTORY_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
        Ok(())
    }

    /// Reverts the last command. Returns its label, or `None` when there is nothing to undo.
    pub fn undo(&mut self, store: &Store) -> Result<Option<String>> {
        let Some(entry) = self.undo.pop() else {
            return Ok(None);
        };
        let command = apply(store, entry.command)?;
        self.redo.push(Entry {
            label: entry.label.clone(),
            command,
        });
        Ok(Some(entry.label))
    }

    /// Re-applies the last undone command. Returns its label, or `None`.
    pub fn redo(&mut self, store: &Store) -> Result<Option<String>> {
        let Some(entry) = self.redo.pop() else {
            return Ok(None);
        };
        let command = apply(store, entry.command)?;
        self.undo.push(Entry {
            label: entry.label.clone(),
            command,
        });
        Ok(Some(entry.label))
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|entry| entry.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|entry| entry.label.as_str())
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::{account, folder, message, store};
    use chrono::Utc;

    fn setup() -> (Store, Folder, Folder, Message) {
        let store = store();
        let account = account(&store);
        let inbox = folder(&store, account.id, FolderRole::Inbox);
        let archive = folder(&store, account.id, FolderRole::Archive);
        let mut msg = message(account.id, inbox.id);
        store.save(&mut msg).unwrap();
        (store, inbox, archive, msg)
    }

    #[test]
    fn flags_undo_redo() {
        let (store, _, _, msg) = setup();
        let mut history = History::new();
        let command = Command::SetMessageFlags {
            ids: vec![msg.id],
            read: Some(true),
            flagged: None,
            pinned: None,
        };
        assert_eq!(command.label(), "Mark message as read");
        history.run(&store, command).unwrap();
        assert!(store.require::<Message>(msg.id).unwrap().flags.read);
        assert_eq!(history.undo_label(), Some("Mark message as read"));

        assert_eq!(
            history.undo(&store).unwrap().as_deref(),
            Some("Mark message as read")
        );
        assert_eq!(store.require::<Message>(msg.id).unwrap(), msg);
        assert!(history.can_redo());

        history.redo(&store).unwrap();
        assert!(store.require::<Message>(msg.id).unwrap().flags.read);
        assert!(history.undo(&store).unwrap().is_some());
        assert!(history.undo(&store).unwrap().is_none());
    }

    #[test]
    fn move_and_delete_are_reversible() {
        let (store, inbox, archive, msg) = setup();
        let mut history = History::new();
        history
            .run(
                &store,
                Command::MoveMessages {
                    ids: vec![msg.id],
                    folder_id: archive.id,
                },
            )
            .unwrap();
        assert!(store.list_messages(inbox.id).unwrap().is_empty());
        assert_eq!(store.list_messages(archive.id).unwrap().len(), 1);

        history
            .run(&store, Command::DeleteMessages { ids: vec![msg.id] })
            .unwrap();
        assert!(store.get::<Message>(msg.id).unwrap().is_none());

        history.undo(&store).unwrap();
        history.undo(&store).unwrap();
        assert_eq!(store.require::<Message>(msg.id).unwrap(), msg);
    }

    #[test]
    fn create_undo_redo_keeps_id() {
        let (store, ..) = setup();
        let account_id = store.list_accounts().unwrap()[0].id;
        let mut history = History::new();
        let task = Task {
            id: 0,
            account_id,
            title: "Write tests".into(),
            notes: String::new(),
            due: None,
            completed: false,
            importance: Importance::High,
            categories: vec![],
            created: Utc::now(),
        };
        history
            .run(&store, Command::SaveTask(Box::new(task)))
            .unwrap();
        let created = store.list_tasks().unwrap().remove(0);

        history
            .run(
                &store,
                Command::SetTaskCompleted {
                    id: created.id,
                    completed: true,
                },
            )
            .unwrap();
        assert!(store.require::<Task>(created.id).unwrap().completed);

        history.undo(&store).unwrap();
        history.undo(&store).unwrap();
        assert!(store.list_tasks().unwrap().is_empty());

        history.redo(&store).unwrap();
        assert_eq!(store.require::<Task>(created.id).unwrap(), created);
    }

    #[test]
    fn failing_batch_is_atomic() {
        let (store, _, _, msg) = setup();
        let mut history = History::new();
        let batch = Command::Batch(vec![
            Command::SetMessageFlags {
                ids: vec![msg.id],
                read: Some(true),
                flagged: Some(true),
                pinned: None,
            },
            Command::DeleteTask(999),
        ]);
        assert!(history.run(&store, batch).is_err());
        assert_eq!(store.require::<Message>(msg.id).unwrap(), msg);
        assert!(!history.can_undo());
    }

    #[test]
    fn commands_serialize_to_json() {
        let command = Command::MoveMessages {
            ids: vec![1, 2],
            folder_id: 3,
        };
        let json = serde_json::to_string(&command).unwrap();
        assert_eq!(
            json,
            r#"{"type":"move_messages","args":{"ids":[1,2],"folder_id":3}}"#
        );
        assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), command);
    }
}
