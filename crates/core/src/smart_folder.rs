//! Smart folders (All inboxes, Unread, Flagged, All drafts, All sent) and the folder pane's
//! Favorites list.
//!
//! A smart folder has a negative pseudo folder id ([`SmartFolder::id`]), so it can be selected,
//! listed ([`Store::list_messages`]), searched (`folder:<id>`) and marked read like a real one.
//! Favorites are an explicit list of folder ids (smart ones included) stored in the settings,
//! so removing a default favorite (an Inbox) stays removed after a restart.

use serde::{Deserialize, Serialize};

use crate::command::Command;
use crate::error::Result;
use crate::model::{FolderRole, Id, Message};
use crate::settings;
use crate::store::Store;

/// Settings key of the Favorites list (JSON array of folder ids).
pub const FAVORITES_KEY: &str = "mail.favorites";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SmartFolder {
    AllInboxes,
    Unread,
    Flagged,
    AllDrafts,
    AllSent,
}

/// Roles never shown in Unread / Flagged (deleted, junk, own mail).
const HIDDEN_ROLES: &str = "'trash', 'junk', 'drafts', 'sent'";

impl SmartFolder {
    pub const ALL: [SmartFolder; 5] = [
        Self::AllInboxes,
        Self::Unread,
        Self::Flagged,
        Self::AllDrafts,
        Self::AllSent,
    ];

    /// Pseudo folder id, always negative (real ids are positive rowids).
    pub fn id(self) -> Id {
        match self {
            Self::AllInboxes => -1,
            Self::Unread => -2,
            Self::Flagged => -3,
            Self::AllDrafts => -4,
            Self::AllSent => -5,
        }
    }

    pub fn from_id(id: Id) -> Option<Self> {
        Self::ALL.into_iter().find(|smart| smart.id() == id)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::AllInboxes => "All inboxes",
            Self::Unread => "Unread",
            Self::Flagged => "Flagged",
            Self::AllDrafts => "All drafts",
            Self::AllSent => "All sent",
        }
    }

    /// Whether the badge shows the total count (drafts, flags) rather than the unread count.
    pub fn counts_all(self) -> bool {
        matches!(self, Self::Flagged | Self::AllDrafts)
    }

    /// SQL condition on `messages` selecting this folder's content.
    pub(crate) fn condition(self) -> String {
        let in_roles = |roles: &str| format!("folder_id IN (SELECT id FROM folders WHERE {roles})");
        match self {
            Self::AllInboxes => in_roles("role = 'inbox'"),
            Self::AllDrafts => in_roles("role = 'drafts'"),
            Self::AllSent => in_roles("role = 'sent'"),
            Self::Unread => format!(
                "is_read = 0 AND {}",
                in_roles(&format!("role NOT IN ({HIDDEN_ROLES})"))
            ),
            Self::Flagged => format!(
                "is_flagged = 1 AND {}",
                in_roles("role NOT IN ('trash', 'junk')")
            ),
        }
    }

    /// Messages of every account, newest first.
    pub fn list_messages(self, store: &Store) -> Result<Vec<Message>> {
        store.query(
            &format!("WHERE {} ORDER BY date DESC, id DESC", self.condition()),
            [],
        )
    }

    /// Whether `message`, stored in a folder with `role` (`FolderRole::as_str`), belongs here.
    /// Mirrors [`Self::condition`], for search.
    pub fn matches(self, message: &Message, role: &str) -> bool {
        let role = FolderRole::parse(role).unwrap_or(FolderRole::Custom);
        match self {
            Self::AllInboxes => role == FolderRole::Inbox,
            Self::AllDrafts => role == FolderRole::Drafts,
            Self::AllSent => role == FolderRole::Sent,
            Self::Unread => {
                !message.flags.read
                    && !matches!(
                        role,
                        FolderRole::Trash
                            | FolderRole::Junk
                            | FolderRole::Drafts
                            | FolderRole::Sent
                    )
            }
            Self::Flagged => {
                message.flags.flagged && !matches!(role, FolderRole::Trash | FolderRole::Junk)
            }
        }
    }
}

// ---------- favorites ----------

/// The stored Favorites list, or the default one (All inboxes + each account's Inbox) when the
/// user never changed it.
pub fn favorites(store: &Store) -> Result<Vec<Id>> {
    if let Some(ids) = settings::get_json::<Vec<Id>>(store, FAVORITES_KEY)? {
        return Ok(ids);
    }
    let mut ids = vec![SmartFolder::AllInboxes.id()];
    for account in store.list_accounts()? {
        if let Some(inbox) = store.folder_by_role(account.id, FolderRole::Inbox)? {
            ids.push(inbox.id);
        }
    }
    Ok(ids)
}

/// Adds `folder_id` to Favorites, or removes it when already there (undoable).
pub fn toggle_favorite(store: &Store, folder_id: Id) -> Result<Command> {
    let mut ids = favorites(store)?;
    if let Some(index) = ids.iter().position(|&id| id == folder_id) {
        ids.remove(index);
    } else {
        ids.push(folder_id);
    }
    settings::set_json_command(FAVORITES_KEY, &ids)
}

/// Moves favorite `folder_id` to the place of favorite `target` (drag to reorder, undoable).
pub fn move_favorite(store: &Store, folder_id: Id, target: Id) -> Result<Command> {
    let mut ids = favorites(store)?;
    if let (Some(from), Some(to)) = (
        ids.iter().position(|&id| id == folder_id),
        ids.iter().position(|&id| id == target),
    ) {
        let moved = ids.remove(from);
        ids.insert(to, moved);
    }
    settings::set_json_command(FAVORITES_KEY, &ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::History;
    use crate::store::tests::{account, folder, message, store};

    #[test]
    fn ids_round_trip_and_are_negative() {
        for smart in SmartFolder::ALL {
            assert!(smart.id() < 0);
            assert_eq!(SmartFolder::from_id(smart.id()), Some(smart));
        }
        assert_eq!(SmartFolder::from_id(1), None);
    }

    #[test]
    fn smart_folders_merge_accounts() {
        let store = store();
        let mut history = History::default();
        let first = account(&store);
        let second = account(&store);
        let inbox_a = folder(&store, first.id, FolderRole::Inbox);
        let inbox_b = folder(&store, second.id, FolderRole::Inbox);
        let trash = folder(&store, first.id, FolderRole::Trash);
        let saved = |account_id: Id, folder_id: Id| {
            let mut saved = message(account_id, folder_id);
            store.save(&mut saved).unwrap();
            saved
        };
        let a = saved(first.id, inbox_a.id);
        let b = saved(second.id, inbox_b.id);
        let deleted = saved(first.id, trash.id);
        history
            .run(
                &store,
                Command::SetMessageFlags {
                    ids: vec![b.id, deleted.id],
                    read: Some(true),
                    flagged: Some(true),
                    pinned: None,
                },
            )
            .unwrap();
        let ids = |smart: SmartFolder| -> Vec<Id> {
            let mut ids: Vec<Id> = store
                .list_messages(smart.id())
                .unwrap()
                .iter()
                .map(|m| m.id)
                .collect();
            ids.sort();
            ids
        };
        let mut both = vec![a.id, b.id];
        both.sort();
        assert_eq!(ids(SmartFolder::AllInboxes), both);
        assert_eq!(ids(SmartFolder::Flagged), vec![b.id]);
        let unread = ids(SmartFolder::Unread);
        assert!(unread.contains(&a.id) && !unread.contains(&b.id));
        let found = store
            .search_messages(
                &format!("hello folder:{}", SmartFolder::AllInboxes.id()),
                10,
            )
            .unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(store.message_count(SmartFolder::Flagged.id()).unwrap(), 1);
        assert_eq!(
            store.unread_count(SmartFolder::AllInboxes.id()).unwrap(),
            store.unread_count(inbox_a.id).unwrap() + store.unread_count(inbox_b.id).unwrap()
        );
    }

    #[test]
    fn removed_default_favorite_stays_removed() {
        let store = store();
        let mut history = History::default();
        let account = account(&store);
        let inbox = folder(&store, account.id, FolderRole::Inbox);
        let all = SmartFolder::AllInboxes.id();
        assert_eq!(favorites(&store).unwrap(), vec![all, inbox.id]);

        let command = toggle_favorite(&store, inbox.id).unwrap();
        history.run(&store, command).unwrap();
        assert_eq!(favorites(&store).unwrap(), vec![all]);

        history.undo(&store).unwrap();
        assert_eq!(favorites(&store).unwrap(), vec![all, inbox.id]);
        history.redo(&store).unwrap();
        assert_eq!(favorites(&store).unwrap(), vec![all]);

        let command = toggle_favorite(&store, SmartFolder::Flagged.id()).unwrap();
        history.run(&store, command).unwrap();
        let command = move_favorite(&store, SmartFolder::Flagged.id(), all).unwrap();
        history.run(&store, command).unwrap();
        assert_eq!(
            favorites(&store).unwrap(),
            vec![SmartFolder::Flagged.id(), all]
        );
    }
}
