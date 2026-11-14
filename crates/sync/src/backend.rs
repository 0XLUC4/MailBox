//! The [`MailBackend`] contract and an in-memory implementation.
//!
//! Messages are addressed like IMAP: a folder path plus a UID that is stable while the
//! folder's UIDVALIDITY does not change.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use mailbox_core::{FolderRole, MessageFlags};
use serde::{Deserialize, Serialize};

use crate::{SyncError, SyncResult};

/// IMAP unique identifier inside a folder.
pub type Uid = u32;

/// Flags mirrored with the server (`pinned` is local only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RemoteFlags {
    pub seen: bool,
    pub flagged: bool,
}

impl From<MessageFlags> for RemoteFlags {
    fn from(flags: MessageFlags) -> Self {
        Self {
            seen: flags.read,
            flagged: flags.flagged,
        }
    }
}

/// A server folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFolder {
    /// Full server path (e.g. `INBOX/Projects`), the folder's key.
    pub path: String,
    /// Last path segment, for display.
    pub name: String,
    pub parent: Option<String>,
    pub role: FolderRole,
}

impl RemoteFolder {
    /// Splits `path` on `delimiter` to fill `name` and `parent`.
    pub fn new(path: &str, delimiter: Option<&str>, role: FolderRole) -> Self {
        let split = delimiter
            .filter(|d| !d.is_empty())
            .and_then(|d| path.rsplit_once(d));
        let (parent, name) = match split {
            Some((parent, name)) => (Some(parent.to_owned()), name.to_owned()),
            None => (None, path.to_owned()),
        };
        Self {
            path: path.to_owned(),
            name,
            parent,
            role,
        }
    }
}

/// Result of selecting a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FolderStatus {
    pub uid_validity: u32,
    pub uid_next: Uid,
}

/// A fetched message: only the header block, or the full RFC 5322 source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawMessage {
    pub uid: Uid,
    pub flags: RemoteFlags,
    pub internal_date: Option<DateTime<Utc>>,
    pub data: Vec<u8>,
}

/// A mail server connection for one account (blocking; run it off the UI thread).
pub trait MailBackend: Send {
    /// Opens/authenticates the connection (no-op when already connected).
    fn connect(&mut self) -> SyncResult<()>;

    /// Closes the connection; the next call to [`MailBackend::connect`] reopens it.
    fn disconnect(&mut self) {}

    /// Selectable folders with their role (virtual folders like Gmail "All Mail" excluded).
    fn list_folders(&mut self) -> SyncResult<Vec<RemoteFolder>>;

    /// UIDVALIDITY / UIDNEXT of a folder.
    fn status(&mut self, path: &str) -> SyncResult<FolderStatus>;

    /// Every UID of the folder with its flags.
    fn fetch_flags(&mut self, path: &str) -> SyncResult<Vec<(Uid, RemoteFlags)>>;

    /// Header blocks only (cheap listing).
    fn fetch_headers(&mut self, path: &str, uids: &[Uid]) -> SyncResult<Vec<RawMessage>>;

    /// Full sources (bodies + attachments).
    fn fetch_full(&mut self, path: &str, uids: &[Uid]) -> SyncResult<Vec<RawMessage>>;

    /// Sets `\Seen` / `\Flagged` to exactly `flags`.
    fn set_flags(&mut self, path: &str, uids: &[Uid], flags: RemoteFlags) -> SyncResult<()>;

    fn move_messages(&mut self, path: &str, uids: &[Uid], target: &str) -> SyncResult<()>;

    /// Permanent delete (`\Deleted` + expunge).
    fn delete_messages(&mut self, path: &str, uids: &[Uid]) -> SyncResult<()>;

    /// Stores a message in a folder (e.g. a copy in Sent after SMTP).
    fn append(&mut self, path: &str, raw: &[u8], flags: RemoteFlags) -> SyncResult<()>;

    /// Submits a message (SMTP).
    fn send(&mut self, from: &str, recipients: &[String], raw: &[u8]) -> SyncResult<()>;

    /// Blocks until the folder changes (`true`) or `timeout` elapses (`false`). IMAP IDLE.
    fn wait_for_changes(&mut self, _path: &str, _timeout: Duration) -> SyncResult<bool> {
        Err(SyncError::Unsupported("IDLE"))
    }
}

/// Maps LIST attributes (RFC 6154 special-use, as their `Debug` names like `Sent`, `Trash`,
/// `NoSelect`) and well-known names to a role. `None` = skip (not selectable or virtual).
pub fn classify_folder(path: &str, attributes: &[String]) -> Option<FolderRole> {
    let has = |name: &str| {
        attributes
            .iter()
            .any(|attr| attr.trim_start_matches('\\').eq_ignore_ascii_case(name))
    };
    if has("NoSelect") || has("NonExistent") || has("All") || has("Flagged") {
        return None;
    }
    for (attr, role) in [
        ("Sent", FolderRole::Sent),
        ("Drafts", FolderRole::Drafts),
        ("Trash", FolderRole::Trash),
        ("Junk", FolderRole::Junk),
        ("Archive", FolderRole::Archive),
    ] {
        if has(attr) {
            return Some(role);
        }
    }
    if path.eq_ignore_ascii_case("INBOX") {
        return Some(FolderRole::Inbox);
    }
    let leaf = path
        .rsplit(['/', '.'])
        .next()
        .unwrap_or(path)
        .to_lowercase();
    let role = match leaf.as_str() {
        "sent" | "sent items" | "sent mail" | "sent messages" | "envoyés" | "éléments envoyés" => {
            FolderRole::Sent
        }
        "drafts" | "brouillons" => FolderRole::Drafts,
        "trash"
        | "deleted items"
        | "deleted messages"
        | "bin"
        | "corbeille"
        | "éléments supprimés" => FolderRole::Trash,
        "junk" | "spam" | "junk e-mail" | "junk email" | "courrier indésirable" => {
            FolderRole::Junk
        }
        "archive" | "archives" => FolderRole::Archive,
        _ => FolderRole::Custom,
    };
    Some(role)
}

/// In-memory server for tests and demos.
#[derive(Debug, Clone, Default)]
pub struct MemoryBackend {
    pub folders: BTreeMap<String, MemoryFolder>,
    /// Raw messages submitted through [`MailBackend::send`].
    pub sent: Vec<Vec<u8>>,
    pub connected: bool,
    /// Makes `connect` fail (simulates an unreachable server).
    pub offline: bool,
}

#[derive(Debug, Clone)]
pub struct MemoryFolder {
    pub role: FolderRole,
    pub uid_validity: u32,
    pub uid_next: Uid,
    pub messages: BTreeMap<Uid, (RemoteFlags, Vec<u8>)>,
}

impl MemoryBackend {
    pub fn add_folder(&mut self, path: &str, role: FolderRole) {
        self.folders.insert(
            path.to_owned(),
            MemoryFolder {
                role,
                uid_validity: 1,
                uid_next: 1,
                messages: BTreeMap::new(),
            },
        );
    }

    /// Adds a message to a folder and returns its UID.
    pub fn deliver(&mut self, path: &str, raw: &[u8], flags: RemoteFlags) -> SyncResult<Uid> {
        let folder = self.folder(path)?;
        let uid = folder.uid_next;
        folder.uid_next += 1;
        folder.messages.insert(uid, (flags, raw.to_vec()));
        Ok(uid)
    }

    fn folder(&mut self, path: &str) -> SyncResult<&mut MemoryFolder> {
        if !self.connected {
            return Err(SyncError::Connection("not connected".into()));
        }
        self.folders
            .get_mut(path)
            .ok_or_else(|| SyncError::Protocol(format!("no folder {path}")))
    }

    fn fetch(
        &mut self,
        path: &str,
        uids: &[Uid],
        headers_only: bool,
    ) -> SyncResult<Vec<RawMessage>> {
        let folder = self.folder(path)?;
        Ok(uids
            .iter()
            .filter_map(|uid| {
                let (flags, raw) = folder.messages.get(uid)?;
                let data = if headers_only {
                    header_block(raw).to_vec()
                } else {
                    raw.clone()
                };
                Some(RawMessage {
                    uid: *uid,
                    flags: *flags,
                    internal_date: None,
                    data,
                })
            })
            .collect())
    }
}

/// The header block of a raw message, including the blank separator line.
pub fn header_block(raw: &[u8]) -> &[u8] {
    for separator in [&b"\r\n\r\n"[..], &b"\n\n"[..]] {
        if let Some(pos) = raw.windows(separator.len()).position(|w| w == separator) {
            return &raw[..pos + separator.len()];
        }
    }
    raw
}

impl MailBackend for MemoryBackend {
    fn connect(&mut self) -> SyncResult<()> {
        if self.offline {
            return Err(SyncError::Connection("server unreachable".into()));
        }
        self.connected = true;
        Ok(())
    }

    fn disconnect(&mut self) {
        self.connected = false;
    }

    fn list_folders(&mut self) -> SyncResult<Vec<RemoteFolder>> {
        if !self.connected {
            return Err(SyncError::Connection("not connected".into()));
        }
        Ok(self
            .folders
            .iter()
            .map(|(path, folder)| RemoteFolder::new(path, Some("/"), folder.role))
            .collect())
    }

    fn status(&mut self, path: &str) -> SyncResult<FolderStatus> {
        let folder = self.folder(path)?;
        Ok(FolderStatus {
            uid_validity: folder.uid_validity,
            uid_next: folder.uid_next,
        })
    }

    fn fetch_flags(&mut self, path: &str) -> SyncResult<Vec<(Uid, RemoteFlags)>> {
        let folder = self.folder(path)?;
        Ok(folder
            .messages
            .iter()
            .map(|(uid, (flags, _))| (*uid, *flags))
            .collect())
    }

    fn fetch_headers(&mut self, path: &str, uids: &[Uid]) -> SyncResult<Vec<RawMessage>> {
        self.fetch(path, uids, true)
    }

    fn fetch_full(&mut self, path: &str, uids: &[Uid]) -> SyncResult<Vec<RawMessage>> {
        self.fetch(path, uids, false)
    }

    fn set_flags(&mut self, path: &str, uids: &[Uid], flags: RemoteFlags) -> SyncResult<()> {
        let folder = self.folder(path)?;
        for uid in uids {
            if let Some(entry) = folder.messages.get_mut(uid) {
                entry.0 = flags;
            }
        }
        Ok(())
    }

    fn move_messages(&mut self, path: &str, uids: &[Uid], target: &str) -> SyncResult<()> {
        self.folder(target)?;
        let moved: Vec<_> = {
            let folder = self.folder(path)?;
            uids.iter()
                .filter_map(|uid| folder.messages.remove(uid))
                .collect()
        };
        for (flags, raw) in moved {
            self.deliver(target, &raw, flags)?;
        }
        Ok(())
    }

    fn delete_messages(&mut self, path: &str, uids: &[Uid]) -> SyncResult<()> {
        let folder = self.folder(path)?;
        for uid in uids {
            folder.messages.remove(uid);
        }
        Ok(())
    }

    fn append(&mut self, path: &str, raw: &[u8], flags: RemoteFlags) -> SyncResult<()> {
        self.deliver(path, raw, flags).map(|_| ())
    }

    fn send(&mut self, _from: &str, _recipients: &[String], raw: &[u8]) -> SyncResult<()> {
        // SMTP is a separate connection: only reachability matters.
        if self.offline {
            return Err(SyncError::Connection("server unreachable".into()));
        }
        self.sent.push(raw.to_vec());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attrs(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn special_use_attributes_win_over_names() {
        assert_eq!(
            classify_folder("[Gmail]/Sent Mail", &attrs(&["HasNoChildren", "Sent"])),
            Some(FolderRole::Sent)
        );
        assert_eq!(
            classify_folder("Bin", &attrs(&["Trash"])),
            Some(FolderRole::Trash)
        );
        assert_eq!(classify_folder("[Gmail]/All Mail", &attrs(&["All"])), None);
        assert_eq!(classify_folder("[Gmail]", &attrs(&["NoSelect"])), None);
    }

    #[test]
    fn well_known_names_without_attributes() {
        assert_eq!(classify_folder("inbox", &[]), Some(FolderRole::Inbox));
        assert_eq!(
            classify_folder("INBOX.Sent Items", &[]),
            Some(FolderRole::Sent)
        );
        assert_eq!(classify_folder("Spam", &[]), Some(FolderRole::Junk));
        assert_eq!(
            classify_folder("Projects/2026", &[]),
            Some(FolderRole::Custom)
        );
    }

    #[test]
    fn remote_folder_splits_parent() {
        let folder = RemoteFolder::new("INBOX/Projects/A", Some("/"), FolderRole::Custom);
        assert_eq!(folder.name, "A");
        assert_eq!(folder.parent.as_deref(), Some("INBOX/Projects"));
        let root = RemoteFolder::new("INBOX", None, FolderRole::Inbox);
        assert_eq!(root.parent, None);
    }

    #[test]
    fn header_block_stops_at_blank_line() {
        assert_eq!(
            header_block(b"A: 1\r\nB: 2\r\n\r\nbody"),
            b"A: 1\r\nB: 2\r\n\r\n"
        );
        assert_eq!(header_block(b"A: 1\n\nbody"), b"A: 1\n\n");
    }
}
