//! Server identity of synced messages and the repair of duplicated ones.
//!
//! A server message is identified by account + server folder path + UIDVALIDITY + UID; the
//! `Message-ID` header is kept too. They live in extra `messages` columns that
//! [`Message`]'s [`Record`](crate::store::Record) impl does not list, so `Store::save` keeps them
//! when it updates a message. A UNIQUE index makes a second copy of one server message
//! impossible, whatever the sync state file says.

use std::collections::HashMap;

use rusqlite::{OptionalExtension, params};

use crate::compose::preview_text;
use crate::{FolderRole, Id, Message, Result, Store};

pub const MIGRATION: &str = r#"
ALTER TABLE messages ADD COLUMN remote_path TEXT;
ALTER TABLE messages ADD COLUMN remote_uid_validity INTEGER;
ALTER TABLE messages ADD COLUMN remote_uid INTEGER;
ALTER TABLE messages ADD COLUMN message_id_header TEXT NOT NULL DEFAULT '';
CREATE UNIQUE INDEX messages_remote
    ON messages(account_id, remote_path, remote_uid_validity, remote_uid);
CREATE INDEX messages_identity ON messages(folder_id, from_email, subject, date);
"#;

/// Where a message lives on the server.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RemoteId {
    pub path: String,
    pub uid_validity: u32,
    pub uid: u32,
}

/// A copy removed by [`Store::merge_duplicate_messages`] (or a folder removed by
/// [`Store::merge_duplicate_folders`]) and the one that absorbed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MergedDuplicate {
    pub removed: Id,
    pub kept: Id,
}

impl Store {
    /// The message stored for this server message, if any.
    pub fn message_by_remote(&self, account_id: Id, remote: &RemoteId) -> Result<Option<Id>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT id FROM messages WHERE account_id = ?1 AND remote_path = ?2 \
                 AND remote_uid_validity = ?3 AND remote_uid = ?4",
                params![account_id, remote.path, remote.uid_validity, remote.uid],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// A message of the same folder without server identity that is this one: same
    /// Message-ID, or same sender, subject and date (a copy stored before identities existed).
    ///
    /// Runs for every message a sync downloads: two indexed lookups (`messages_folder_message_id`,
    /// `messages_identity`), not one `OR` query, which SQLite answers by scanning the whole folder.
    pub fn unlinked_twin(&self, message: &Message, message_id: &str) -> Result<Option<Id>> {
        let by_message_id: Option<Id> = if message_id.is_empty() {
            None
        } else {
            self.conn().query_row(
                "SELECT MIN(id) FROM messages \
                 WHERE folder_id = ?1 AND message_id_header = ?2 AND remote_uid IS NULL",
                params![message.folder_id, message_id],
                |row| row.get(0),
            )?
        };
        let by_identity: Option<Id> = self.conn().query_row(
            "SELECT MIN(id) FROM messages \
             WHERE folder_id = ?1 AND from_email = ?2 AND subject = ?3 AND date = ?4 \
             AND remote_uid IS NULL AND (message_id_header = '' OR ?5 = '')",
            params![
                message.folder_id,
                message.from.email,
                message.subject,
                crate::store::timestamp(&message.date),
                message_id,
            ],
            |row| row.get(0),
        )?;
        Ok(by_message_id.into_iter().chain(by_identity).min())
    }

    /// Records the server identity of a message (`None` clears it, e.g. after a move) and its
    /// Message-ID (an empty one keeps the stored value).
    pub fn set_message_remote(
        &self,
        id: Id,
        remote: Option<&RemoteId>,
        message_id: &str,
    ) -> Result<()> {
        let (path, uid_validity, uid) = match remote {
            Some(remote) => (
                Some(remote.path.as_str()),
                Some(remote.uid_validity),
                Some(remote.uid),
            ),
            None => (None, None, None),
        };
        self.conn().execute(
            "UPDATE messages SET remote_path = ?2, remote_uid_validity = ?3, remote_uid = ?4, \
             message_id_header = CASE WHEN ?5 = '' THEN message_id_header ELSE ?5 END \
             WHERE id = ?1",
            params![id, path, uid_validity, uid, message_id],
        )?;
        Ok(())
    }

    /// Server identity and Message-ID of a message (`None` identity: not linked).
    pub fn message_remote(&self, id: Id) -> Result<(Option<RemoteId>, String)> {
        Ok(self.conn().query_row(
            "SELECT remote_path, remote_uid_validity, remote_uid, message_id_header \
             FROM messages WHERE id = ?1",
            [id],
            |row| {
                let path: Option<String> = row.get(0)?;
                let uid_validity: Option<u32> = row.get(1)?;
                let uid: Option<u32> = row.get(2)?;
                let remote = match (path, uid_validity, uid) {
                    (Some(path), Some(uid_validity), Some(uid)) => Some(RemoteId {
                        path,
                        uid_validity,
                        uid,
                    }),
                    _ => None,
                };
                Ok((remote, row.get(3)?))
            },
        )?)
    }

    /// Merges copies of one message stored more than once in the same folder (same
    /// Message-ID, or same sender + subject + date + body when it is missing). The most
    /// complete copy is kept and absorbs the user state of the others (read / flagged /
    /// pinned: OR, categories: union, flag due date, server identity). Copies with different
    /// server UIDs, Message-IDs or bodies are distinct messages and stay. Idempotent.
    pub fn merge_duplicate_messages(&self) -> Result<Vec<MergedDuplicate>> {
        self.transaction(|store| {
            let groups: Vec<(Id, String, String, String)> = {
                let mut statement = store.conn().prepare(
                    "SELECT folder_id, from_email, subject, date FROM messages \
                     GROUP BY folder_id, from_email, subject, date HAVING COUNT(*) > 1",
                )?;
                let rows = statement.query_map([], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })?;
                rows.collect::<rusqlite::Result<_>>()?
            };
            let mut merged = Vec::new();
            for (folder_id, from, subject, date) in groups {
                let messages: Vec<Message> = store.query(
                    "WHERE folder_id = ?1 AND from_email = ?2 AND subject = ?3 AND date = ?4 \
                     ORDER BY id",
                    params![folder_id, from, subject, date],
                )?;
                let mut copies = messages
                    .into_iter()
                    .map(|message| {
                        let (remote, message_id) = store.message_remote(message.id)?;
                        Ok(Copy {
                            message,
                            remote,
                            message_id,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                // Most complete first (stable: the oldest id wins a tie).
                copies.sort_by_key(|copy| std::cmp::Reverse(copy.completeness()));
                for cluster in clusters(copies) {
                    merged.extend(store.merge_cluster(cluster)?);
                }
            }
            Ok(merged)
        })
    }

    /// Keeps the first copy, folds the others into it and deletes them.
    fn merge_cluster(&self, cluster: Vec<Copy>) -> Result<Vec<MergedDuplicate>> {
        let mut copies = cluster.into_iter();
        let Some(mut keeper) = copies.next() else {
            return Ok(Vec::new());
        };
        let mut merged = Vec::new();
        for copy in copies {
            let kept = &mut keeper.message;
            kept.flags.read |= copy.message.flags.read;
            kept.flags.flagged |= copy.message.flags.flagged;
            kept.flags.pinned |= copy.message.flags.pinned;
            for category in copy.message.categories {
                if !kept.categories.contains(&category) {
                    kept.categories.push(category);
                }
            }
            if keeper.remote.is_none() {
                keeper.remote = copy.remote;
            }
            if keeper.message_id.is_empty() {
                keeper.message_id = copy.message_id;
            }
            // The keeper's flag due date wins; otherwise it takes this copy's.
            self.conn().execute(
                "UPDATE OR IGNORE message_flag_due SET id = ?2 WHERE id = ?1",
                [copy.message.id, kept.id],
            )?;
            self.conn().execute(
                "DELETE FROM message_flag_due WHERE id = ?1",
                [copy.message.id],
            )?;
            self.conn()
                .execute("DELETE FROM messages WHERE id = ?1", [copy.message.id])?;
            merged.push(MergedDuplicate {
                removed: copy.message.id,
                kept: kept.id,
            });
        }
        if !merged.is_empty() {
            self.save(&mut keeper.message)?;
            self.set_message_remote(
                keeper.message.id,
                keeper.remote.as_ref(),
                &keeper.message_id,
            )?;
        }
        Ok(merged)
    }

    /// Merges special folders (Inbox, Sent, Trash...) stored more than once for an account:
    /// older versions created a local folder for each server folder of a role ("Junk" and
    /// "Junk Email"), shown twice under the same name. Messages and subfolders move to the
    /// copy with the most messages (the oldest on a tie), the others are deleted. Idempotent.
    pub fn merge_duplicate_folders(&self) -> Result<Vec<MergedDuplicate>> {
        self.transaction(|store| {
            let pairs: Vec<MergedDuplicate> = {
                let mut statement = store.conn().prepare(
                    "SELECT id, (SELECT kept.id FROM folders kept \
                     WHERE kept.account_id = folders.account_id AND kept.role = folders.role \
                     ORDER BY (SELECT COUNT(*) FROM messages WHERE folder_id = kept.id) DESC, \
                     kept.id LIMIT 1) FROM folders WHERE role <> ?1",
                )?;
                let rows = statement.query_map([FolderRole::Custom.as_str()], |row| {
                    Ok(MergedDuplicate {
                        removed: row.get(0)?,
                        kept: row.get(1)?,
                    })
                })?;
                rows.collect::<rusqlite::Result<_>>()?
            };
            let merged: Vec<MergedDuplicate> = pairs
                .into_iter()
                .filter(|merge| merge.removed != merge.kept)
                .collect();
            for merge in &merged {
                let ids = [merge.kept, merge.removed];
                let conn = store.conn();
                conn.execute("UPDATE messages SET folder_id = ?1 WHERE folder_id = ?2", ids)?;
                conn.execute("UPDATE folders SET parent_id = ?1 WHERE parent_id = ?2", ids)?;
                conn.execute("DELETE FROM folders WHERE id = ?1", [merge.removed])?;
            }
            Ok(merged)
        })
    }

    /// Recomputes list previews that start with a separator line ("=====", "-----"), left by
    /// older versions for mail whose text part opens with one. Returns how many changed.
    pub fn repair_previews(&self) -> Result<usize> {
        let rows: Vec<(Id, String, String)> = {
            let mut statement = self.conn().prepare(
                "SELECT id, preview, body_text FROM messages WHERE preview GLOB '[^0-9A-Za-z]*'",
            )?;
            let rows =
                statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let mut changed = 0;
        for (id, old, body) in rows {
            let preview = preview_text(&body);
            if preview != old {
                self.conn().execute(
                    "UPDATE messages SET preview = ?2 WHERE id = ?1",
                    params![id, preview],
                )?;
                changed += 1;
            }
        }
        Ok(changed)
    }
}

/// A stored message with its server identity, while merging.
struct Copy {
    message: Message,
    remote: Option<RemoteId>,
    message_id: String,
}

impl Copy {
    /// What makes a copy worth keeping: a body, attachments, HTML, linked to the server.
    fn completeness(&self) -> (bool, usize, bool, usize, bool) {
        (
            self.body().is_some(),
            self.message.attachments.len(),
            self.message.body_html.is_some(),
            self.message.body_text.len(),
            self.remote.is_some(),
        )
    }

    /// The body to compare copies with: the HTML when there is one (the text of HTML mail is
    /// derived and changed between versions), else the text. `None` = headers only.
    fn body(&self) -> Option<&str> {
        let html = self.message.body_html.as_deref().map(str::trim);
        let text = self.message.body_text.trim();
        html.filter(|html| !html.is_empty())
            .or_else(|| (!text.is_empty()).then_some(text))
    }

    /// Whether both can be the same message (nothing tells them apart).
    fn same_as(&self, other: &Copy) -> bool {
        let differ = |a: &str, b: &str| !a.is_empty() && !b.is_empty() && a != b;
        let remotes_differ = matches!((&self.remote, &other.remote), (Some(a), Some(b)) if a != b);
        let bodies_differ = matches!((self.body(), other.body()), (Some(a), Some(b)) if a != b);
        !remotes_differ && !bodies_differ && !differ(&self.message_id, &other.message_id)
    }
}

/// Splits copies (most complete first) into groups of the same message.
fn clusters(copies: Vec<Copy>) -> Vec<Vec<Copy>> {
    let mut clusters: Vec<Vec<Copy>> = Vec::new();
    for copy in copies {
        match clusters
            .iter_mut()
            .find(|cluster| cluster.iter().all(|member| member.same_as(&copy)))
        {
            Some(cluster) => cluster.push(copy),
            None => clusters.push(vec![copy]),
        }
    }
    clusters
}

/// `removed → kept` lookup for callers remapping their own references (sync links, files).
pub fn merged_map(merged: &[MergedDuplicate]) -> HashMap<Id, Id> {
    merged
        .iter()
        .map(|merge| (merge.removed, merge.kept))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FolderRole;
    use crate::mail_ops::FlagDue;
    use crate::store::tests::{account, folder, message, store};
    use chrono::NaiveDate;

    fn remote(uid: u32) -> RemoteId {
        RemoteId {
            path: "INBOX".into(),
            uid_validity: 7,
            uid,
        }
    }

    fn save(store: &Store, mut message: Message) -> Id {
        store.save(&mut message).unwrap()
    }

    fn count(store: &Store, folder_id: Id) -> usize {
        store.list_messages(folder_id).unwrap().len()
    }

    #[test]
    fn remote_identity_is_unique_and_survives_saves() {
        let store = store();
        let account = account(&store);
        let inbox = folder(&store, account.id, FolderRole::Inbox);
        let id = save(&store, message(account.id, inbox.id));
        store
            .set_message_remote(id, Some(&remote(3)), "a@x")
            .unwrap();

        // A normal save (e.g. marking read) keeps the identity.
        let mut saved = store.require::<Message>(id).unwrap();
        saved.flags.read = true;
        store.save(&mut saved).unwrap();
        assert_eq!(
            store.message_by_remote(account.id, &remote(3)).unwrap(),
            Some(id)
        );
        assert_eq!(
            store.message_remote(id).unwrap(),
            (Some(remote(3)), "a@x".into())
        );

        // A second message cannot claim the same server message.
        let other = save(&store, message(account.id, inbox.id));
        assert!(
            store
                .set_message_remote(other, Some(&remote(3)), "")
                .is_err()
        );

        store.set_message_remote(id, None, "").unwrap();
        assert_eq!(
            store.message_by_remote(account.id, &remote(3)).unwrap(),
            None
        );
        assert_eq!(store.message_remote(id).unwrap().1, "a@x");
    }

    #[test]
    fn finds_unlinked_twins() {
        let store = store();
        let account = account(&store);
        let inbox = folder(&store, account.id, FolderRole::Inbox);
        let old = save(&store, message(account.id, inbox.id));
        let fetched = message(account.id, inbox.id);
        assert_eq!(store.unlinked_twin(&fetched, "m@x").unwrap(), Some(old));

        // Linked messages and other Message-IDs are not twins.
        store.set_message_remote(old, None, "other@x").unwrap();
        assert_eq!(store.unlinked_twin(&fetched, "m@x").unwrap(), None);
        assert_eq!(store.unlinked_twin(&fetched, "other@x").unwrap(), Some(old));
        store.set_message_remote(old, Some(&remote(1)), "").unwrap();
        assert_eq!(store.unlinked_twin(&fetched, "other@x").unwrap(), None);
    }

    /// The fixture of the bug: the same mail fetched three times (once headers only), a
    /// user flag on one copy, a category and a flag due date on another; plus look-alikes
    /// that are really different messages.
    #[test]
    fn merges_duplicates_keeping_the_most_complete_copy_and_user_state() {
        let store = store();
        let account = account(&store);
        let inbox = folder(&store, account.id, FolderRole::Inbox);
        let archive = folder(&store, account.id, FolderRole::Archive);

        let mut headers_only = message(account.id, inbox.id);
        headers_only.body_text.clear();
        headers_only.attachments.clear();
        headers_only.categories = vec!["Later".into()];
        let headers_only = save(&store, headers_only);
        let mut full = message(account.id, inbox.id);
        full.body_html = Some("<p>Hi there</p>".into());
        let full = save(&store, full);
        let mut read = message(account.id, inbox.id);
        read.body_html = Some("<p>Hi there</p>".into());
        read.body_text = "Hi there (older text conversion)".into();
        read.flags.read = true;
        read.flags.flagged = true;
        let read = save(&store, read);
        let due = NaiveDate::from_ymd_opt(2026, 12, 1).unwrap();
        store
            .save(&mut FlagDue {
                message_id: read,
                due,
            })
            .unwrap();

        // Look-alikes: other folder, other body, other server UIDs.
        save(&store, message(account.id, archive.id));
        let mut other_body = message(account.id, inbox.id);
        other_body.body_text = "Something else entirely".into();
        save(&store, other_body);
        let uid_one = save(&store, message(account.id, inbox.id));
        let uid_two = save(&store, message(account.id, inbox.id));
        store
            .set_message_remote(uid_one, Some(&remote(1)), "")
            .unwrap();
        store
            .set_message_remote(uid_two, Some(&remote(2)), "")
            .unwrap();
        assert_eq!(count(&store, inbox.id), 6);

        let merged = store.merge_duplicate_messages().unwrap();
        // `read` has the longest text: it is kept and absorbs the other two copies.
        assert_eq!(
            merged,
            vec![
                MergedDuplicate {
                    removed: full,
                    kept: read
                },
                MergedDuplicate {
                    removed: headers_only,
                    kept: read
                },
            ]
        );
        let message = store.require::<Message>(read).unwrap();
        assert!(message.body_html.is_some());
        assert_eq!(message.attachments.len(), 1);
        assert!(message.flags.read && message.flags.flagged);
        assert_eq!(
            message.categories,
            vec!["Work".to_owned(), "Later".to_owned()]
        );
        assert_eq!(
            store.get::<FlagDue>(read).unwrap().map(|d| d.due),
            Some(due)
        );
        for removed in [full, headers_only] {
            assert!(store.get::<Message>(removed).unwrap().is_none());
        }
        // Distinct server UIDs stay distinct; the other body stays; archive untouched.
        assert!(store.get::<Message>(uid_one).unwrap().is_some());
        assert!(store.get::<Message>(uid_two).unwrap().is_some());
        assert_eq!(count(&store, archive.id), 1);
        assert_eq!(count(&store, inbox.id), 4);

        // Idempotent.
        assert!(store.merge_duplicate_messages().unwrap().is_empty());
        assert_eq!(count(&store, inbox.id), 4);
    }

    #[test]
    fn merging_moves_the_server_identity_to_the_keeper() {
        let store = store();
        let account = account(&store);
        let inbox = folder(&store, account.id, FolderRole::Inbox);
        let mut headers_only = message(account.id, inbox.id);
        headers_only.body_text.clear();
        let linked = save(&store, headers_only);
        store
            .set_message_remote(linked, Some(&remote(9)), "m@x")
            .unwrap();
        let full = save(&store, message(account.id, inbox.id));

        let merged = store.merge_duplicate_messages().unwrap();
        assert_eq!(
            merged,
            vec![MergedDuplicate {
                removed: linked,
                kept: full
            }]
        );
        assert_eq!(
            store.message_by_remote(account.id, &remote(9)).unwrap(),
            Some(full)
        );
        assert_eq!(store.message_remote(full).unwrap().1, "m@x");
    }

    #[test]
    fn repairs_separator_previews() {
        let store = store();
        let account = account(&store);
        let inbox = folder(&store, account.id, FolderRole::Inbox);
        let mut bad = message(account.id, inbox.id);
        bad.body_text = "==========\r\nSee your notifications.\r\n".into();
        bad.preview = "========== See your notifications.".into();
        let bad = save(&store, bad);
        save(&store, message(account.id, inbox.id));

        assert_eq!(store.repair_previews().unwrap(), 1);
        assert_eq!(
            store.require::<Message>(bad).unwrap().preview,
            "See your notifications."
        );
        assert_eq!(store.repair_previews().unwrap(), 0);
    }

    /// Regression: "Junk" and "Junk Email" were both stored with the Junk role and showed
    /// up as two "Junk" folders.
    #[test]
    fn duplicate_special_folders_are_merged_with_their_mail() {
        let store = store();
        let account = account(&store);
        let empty = folder(&store, account.id, FolderRole::Junk);
        let junk = folder(&store, account.id, FolderRole::Junk);
        let mut child = folder(&store, account.id, FolderRole::Custom);
        child.parent_id = Some(empty.id);
        store.save(&mut child).unwrap();
        let spam = save(&store, message(account.id, junk.id));
        let custom = folder(&store, account.id, FolderRole::Custom);

        let merged = store.merge_duplicate_folders().unwrap();
        assert_eq!(
            merged,
            vec![MergedDuplicate {
                removed: empty.id,
                kept: junk.id,
            }]
        );
        let folders = store.list_folders(account.id).unwrap();
        let junk_folders: Vec<Id> = folders
            .iter()
            .filter(|folder| folder.role == FolderRole::Junk)
            .map(|folder| folder.id)
            .collect();
        assert_eq!(junk_folders, vec![junk.id]);
        assert!(folders.iter().any(|folder| folder.id == custom.id));
        let child = store.require::<crate::Folder>(child.id).unwrap();
        assert_eq!(child.parent_id, Some(junk.id));
        assert_eq!(store.require::<Message>(spam).unwrap().folder_id, junk.id);
        assert!(store.merge_duplicate_folders().unwrap().is_empty());
    }
}
