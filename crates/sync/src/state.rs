//! Persisted per-account sync state: folder cursors and links between server messages
//! (folder path + UID) and local store ids.
//!
//! Local edits are never queued explicitly: [`SyncState::diff_local`] compares the store with
//! the last flags/folder seen on the server, so changes made while offline are pushed on the
//! next successful sync (the "offline queue").

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use mailbox_core::{FolderRole, Id};
use serde::{Deserialize, Serialize};

use crate::SyncResult;
use crate::backend::{RemoteFlags, RemoteFolder, Uid};
use crate::engine::{FolderCursor, PushOp};
use crate::thread::ThreadIndex;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SyncState {
    /// Keyed by server path.
    #[serde(default)]
    pub folders: BTreeMap<String, FolderState>,
    /// Message-ID → local id of messages moved or sent locally, waiting to reappear on the
    /// server under a new UID (then linked instead of duplicated).
    #[serde(default)]
    pub pending_adoptions: BTreeMap<String, Id>,
    #[serde(default)]
    pub threads: ThreadIndex,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FolderState {
    pub role: FolderRole,
    /// Local folder mirroring this server folder.
    #[serde(default)]
    pub local_id: Option<Id>,
    #[serde(default)]
    pub uid_validity: u32,
    #[serde(default)]
    pub highest_uid: Uid,
    #[serde(default)]
    pub messages: BTreeMap<Uid, Link>,
}

/// A server message stored locally.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub local_id: Id,
    /// Flags last seen on (or pushed to) the server.
    pub flags: RemoteFlags,
    #[serde(default)]
    pub message_id: String,
    /// `false` = headers only, body fetched on demand.
    #[serde(default)]
    pub has_body: bool,
}

/// What the store currently says about a linked message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalMessage {
    pub folder_id: Id,
    pub flags: RemoteFlags,
}

impl SyncState {
    /// Loads the state file; a missing file is an empty state.
    pub fn load(path: &Path) -> SyncResult<Self> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err.into()),
        }
    }

    /// Writes the state file atomically (temp file + rename).
    pub fn save(&self, path: &Path) -> SyncResult<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, serde_json::to_vec(self)?)?;
        std::fs::rename(&temp, path)?;
        Ok(())
    }

    /// Registers a server folder (keeps cursors and links of a known one).
    pub fn folder_mut(&mut self, folder: &RemoteFolder) -> &mut FolderState {
        let state = self
            .folders
            .entry(folder.path.clone())
            .or_insert_with(|| FolderState {
                role: folder.role,
                local_id: None,
                uid_validity: 0,
                highest_uid: 0,
                messages: BTreeMap::new(),
            });
        state.role = folder.role;
        state
    }

    pub fn local_folder(&self, path: &str) -> Option<Id> {
        self.folders.get(path).and_then(|folder| folder.local_id)
    }

    pub fn path_of_local_folder(&self, folder_id: Id) -> Option<&str> {
        self.folders
            .iter()
            .find(|(_, folder)| folder.local_id == Some(folder_id))
            .map(|(path, _)| path.as_str())
    }

    pub fn path_of_role(&self, role: FolderRole) -> Option<&str> {
        self.folders
            .iter()
            .find(|(_, folder)| folder.role == role)
            .map(|(path, _)| path.as_str())
    }

    /// Server location of a local message.
    pub fn find(&self, local_id: Id) -> Option<(&str, Uid, &Link)> {
        self.folders.iter().find_map(|(path, folder)| {
            folder
                .messages
                .iter()
                .find(|(_, link)| link.local_id == local_id)
                .map(|(uid, link)| (path.as_str(), *uid, link))
        })
    }

    pub fn link(&mut self, path: &str, uid: Uid, link: Link) {
        if let Some(folder) = self.folders.get_mut(path) {
            folder.messages.insert(uid, link);
        }
    }

    pub fn linked_ids(&self) -> impl Iterator<Item = Id> + '_ {
        self.folders
            .values()
            .flat_map(|folder| folder.messages.values().map(|link| link.local_id))
    }

    /// Cursors to pass to [`crate::engine::run_cycle`].
    pub fn cursors(&self) -> BTreeMap<String, FolderCursor> {
        self.folders
            .iter()
            .map(|(path, folder)| {
                let cursor = FolderCursor {
                    uid_validity: folder.uid_validity,
                    highest_uid: folder.highest_uid,
                    known: folder
                        .messages
                        .iter()
                        .map(|(uid, link)| (*uid, link.flags))
                        .collect(),
                };
                (path.clone(), cursor)
            })
            .collect()
    }

    /// Local changes not yet on the server. `local` holds every message of the account
    /// (a linked id missing from it was deleted locally).
    pub fn diff_local(&self, local: &HashMap<Id, LocalMessage>) -> Vec<PushOp> {
        let mut ops = Vec::new();
        for (path, folder) in &self.folders {
            for (uid, link) in &folder.messages {
                let (path, uid) = (path.clone(), *uid);
                let Some(message) = local.get(&link.local_id) else {
                    ops.push(PushOp::Delete { path, uid });
                    continue;
                };
                if folder.local_id != Some(message.folder_id) {
                    // Moved to a folder not mirrored on the server: leave the server copy.
                    if let Some(target) = self.path_of_local_folder(message.folder_id) {
                        ops.push(PushOp::Move {
                            path,
                            uid,
                            target: target.to_owned(),
                        });
                    }
                } else if message.flags != link.flags {
                    ops.push(PushOp::SetFlags {
                        path,
                        uid,
                        flags: message.flags,
                    });
                }
            }
        }
        ops
    }

    /// Records a change confirmed by the server.
    pub fn apply_pushed(&mut self, op: &PushOp) {
        let Some(folder) = self.folders.get_mut(op.path()) else {
            return;
        };
        match op {
            PushOp::SetFlags { uid, flags, .. } => {
                if let Some(link) = folder.messages.get_mut(uid) {
                    link.flags = *flags;
                }
            }
            PushOp::Move { uid, .. } => {
                if let Some(link) = folder.messages.remove(uid)
                    && !link.message_id.is_empty()
                {
                    self.pending_adoptions
                        .insert(link.message_id, link.local_id);
                }
            }
            PushOp::Delete { uid, .. } => {
                folder.messages.remove(uid);
            }
        }
    }

    /// Local message waiting for this Message-ID, if any (consumed).
    pub fn take_adoption(&mut self, message_id: &str) -> Option<Id> {
        if message_id.is_empty() {
            return None;
        }
        self.pending_adoptions.remove(message_id)
    }

    /// Drops every link of the folder; returns the local ids to delete.
    pub fn reset_folder(&mut self, path: &str) -> Vec<Id> {
        let Some(folder) = self.folders.get_mut(path) else {
            return Vec::new();
        };
        folder.uid_validity = 0;
        folder.highest_uid = 0;
        std::mem::take(&mut folder.messages)
            .into_values()
            .map(|link| link.local_id)
            .collect()
    }

    /// Drops links of vanished UIDs; returns the local ids no other link still uses (two
    /// server copies merged into one local message: it stays while one copy remains).
    pub fn vanish(&mut self, path: &str, uids: &[Uid]) -> Vec<Id> {
        let Some(folder) = self.folders.get_mut(path) else {
            return Vec::new();
        };
        let removed: BTreeSet<Id> = uids
            .iter()
            .filter_map(|uid| folder.messages.remove(uid))
            .map(|link| link.local_id)
            .collect();
        removed
            .into_iter()
            .filter(|id| self.find(*id).is_none())
            .collect()
    }

    /// Points links (and pending adoptions) at the message that absorbed a removed duplicate
    /// (`removed → kept`). Returns whether anything changed.
    pub fn remap_local_ids(&mut self, merged: &HashMap<Id, Id>) -> bool {
        let mut changed = false;
        let links = self
            .folders
            .values_mut()
            .flat_map(|folder| folder.messages.values_mut().map(|link| &mut link.local_id));
        for id in links.chain(self.pending_adoptions.values_mut()) {
            if let Some(kept) = merged.get(id) {
                *id = *kept;
                changed = true;
            }
        }
        changed
    }

    /// Records remote flag changes; returns `(local id, flags)` to apply locally.
    pub fn remote_flags(
        &mut self,
        path: &str,
        changes: &[(Uid, RemoteFlags)],
    ) -> Vec<(Id, RemoteFlags)> {
        let Some(folder) = self.folders.get_mut(path) else {
            return Vec::new();
        };
        changes
            .iter()
            .filter_map(|(uid, flags)| {
                let link = folder.messages.get_mut(uid)?;
                link.flags = *flags;
                Some((link.local_id, *flags))
            })
            .collect()
    }

    pub fn folder_synced(&mut self, path: &str, uid_validity: u32, highest_uid: Uid) {
        if let Some(folder) = self.folders.get_mut(path) {
            folder.uid_validity = uid_validity;
            folder.highest_uid = highest_uid;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const READ: RemoteFlags = RemoteFlags {
        seen: true,
        flagged: false,
    };

    fn state() -> SyncState {
        let mut state = SyncState::default();
        state
            .folder_mut(&RemoteFolder::new("INBOX", None, FolderRole::Inbox))
            .local_id = Some(10);
        state
            .folder_mut(&RemoteFolder::new("Trash", None, FolderRole::Trash))
            .local_id = Some(11);
        for (uid, local_id) in [(1, 100), (2, 101), (3, 102), (4, 103)] {
            state.link(
                "INBOX",
                uid,
                Link {
                    local_id,
                    flags: RemoteFlags::default(),
                    message_id: format!("m{uid}@x"),
                    has_body: true,
                },
            );
        }
        state
    }

    fn local(folder_id: Id, flags: RemoteFlags) -> LocalMessage {
        LocalMessage { folder_id, flags }
    }

    #[test]
    fn diff_finds_flag_changes_moves_and_deletes() {
        let state = state();
        let local = HashMap::from([
            (100, local(10, RemoteFlags::default())), // unchanged
            (101, local(10, READ)),                   // marked read
            (102, local(11, RemoteFlags::default())), // moved to trash
                                                      // 103 deleted locally
        ]);
        let ops = state.diff_local(&local);
        assert_eq!(
            ops,
            vec![
                PushOp::SetFlags {
                    path: "INBOX".into(),
                    uid: 2,
                    flags: READ
                },
                PushOp::Move {
                    path: "INBOX".into(),
                    uid: 3,
                    target: "Trash".into()
                },
                PushOp::Delete {
                    path: "INBOX".into(),
                    uid: 4
                },
            ]
        );
    }

    #[test]
    fn pushed_changes_update_links_and_queue_adoption() {
        let mut state = state();
        state.apply_pushed(&PushOp::SetFlags {
            path: "INBOX".into(),
            uid: 2,
            flags: READ,
        });
        state.apply_pushed(&PushOp::Move {
            path: "INBOX".into(),
            uid: 3,
            target: "Trash".into(),
        });
        state.apply_pushed(&PushOp::Delete {
            path: "INBOX".into(),
            uid: 4,
        });

        assert_eq!(state.find(101).map(|(_, _, link)| link.flags), Some(READ));
        assert!(state.find(102).is_none());
        assert!(state.find(103).is_none());
        assert_eq!(state.take_adoption("m3@x"), Some(102));
        assert_eq!(state.take_adoption("m3@x"), None);

        // Nothing left to push once the store matches.
        let local = HashMap::from([(100, local(10, Default::default())), (101, local(10, READ))]);
        assert!(state.diff_local(&local).is_empty());
    }

    #[test]
    fn remote_changes_vanish_and_reset() {
        let mut state = state();
        assert_eq!(
            state.remote_flags("INBOX", &[(1, READ), (9, READ)]),
            vec![(100, READ)]
        );
        assert_eq!(state.vanish("INBOX", &[2]), vec![101]);
        state.folder_synced("INBOX", 5, 4);
        assert_eq!(state.cursors()["INBOX"].uid_validity, 5);
        assert_eq!(state.cursors()["INBOX"].known.len(), 3);
        let mut removed = state.reset_folder("INBOX");
        removed.sort();
        assert_eq!(removed, vec![100, 102, 103]);
        assert_eq!(state.cursors()["INBOX"], FolderCursor::default());
    }

    #[test]
    fn vanishing_one_of_two_server_copies_keeps_the_merged_message() {
        let mut state = state();
        // Duplicates 101 and 102 were merged into 100 (two server copies, one local message).
        assert!(state.remap_local_ids(&HashMap::from([(101, 100), (102, 100)])));
        assert!(!state.remap_local_ids(&HashMap::from([(999, 100)])));
        assert_eq!(state.vanish("INBOX", &[2]), Vec::<Id>::new());
        assert_eq!(state.vanish("INBOX", &[1, 3]), vec![100]);
        assert_eq!(state.find(103).map(|(_, uid, _)| uid), Some(4));
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("mailbox-sync-state-{}", std::process::id()));
        let path = dir.join("1.json");
        let mut state = state();
        state.threads.assign("a@x", &[], &[], "f");
        state.save(&path).unwrap();
        assert_eq!(SyncState::load(&path).unwrap(), state);
        assert_eq!(
            SyncState::load(&dir.join("missing.json")).unwrap(),
            SyncState::default()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn lookups() {
        let state = state();
        assert_eq!(state.path_of_local_folder(11), Some("Trash"));
        assert_eq!(state.path_of_role(FolderRole::Inbox), Some("INBOX"));
        assert_eq!(state.local_folder("INBOX"), Some(10));
        assert_eq!(
            state.find(102).map(|(path, uid, _)| (path, uid)),
            Some(("INBOX", 3))
        );
        assert_eq!(state.linked_ids().count(), 4);
    }
}
