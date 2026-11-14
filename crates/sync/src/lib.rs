//! Protocol backends. Only the [`MailBackend`] contract exists for now;
//! IMAP/SMTP (async-imap, lettre) implementations come later.
//!
//! Backends never write to the store themselves: they return data and the caller
//! turns it into `mailbox_core::Command`s, so sync results go through the same path as user edits.

use chrono::{DateTime, Utc};
use mailbox_core::{Folder, Id, Message, MessageFlags};

/// Errors a backend can report.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("connection failed: {0}")]
    Connection(String),
    #[error("authentication failed")]
    Auth,
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("not supported by this backend: {0}")]
    Unsupported(&'static str),
}

pub type SyncResult<T> = Result<T, SyncError>;

/// Remote folder reference (server-side path, e.g. `INBOX/Projects`).
pub type RemotePath = String;

/// A mail server connection for one account. Message ids are local store ids;
/// implementations keep their own mapping to server UIDs.
pub trait MailBackend: Send {
    /// Opens/authenticates the connection.
    fn connect(&mut self) -> SyncResult<()>;

    /// Lists server folders, mapped to local [`Folder`]s (id 0 = not stored yet).
    fn list_folders(&mut self) -> SyncResult<Vec<Folder>>;

    /// Fetches messages of `folder` received after `since` (all when `None`).
    fn fetch_since(
        &mut self,
        folder: &RemotePath,
        since: Option<DateTime<Utc>>,
    ) -> SyncResult<Vec<Message>>;

    /// Sends a message (SMTP or equivalent).
    fn send(&mut self, message: &Message) -> SyncResult<()>;

    /// Pushes flag changes to the server.
    fn set_flags(&mut self, ids: &[Id], flags: MessageFlags) -> SyncResult<()>;

    /// Moves messages to another server folder.
    fn move_messages(&mut self, ids: &[Id], target: &RemotePath) -> SyncResult<()>;
}
