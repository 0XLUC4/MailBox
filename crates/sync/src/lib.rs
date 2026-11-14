//! Mail sync and file formats.
//!
//! - [`backend`]: the [`MailBackend`] contract (folder path + UID addressing), an in-memory
//!   backend for tests, folder-role detection.
//! - [`imap_smtp`]: IMAP (imap + rustls) + SMTP (lettre) implementation, password or OAuth2.
//! - [`engine`]: one sync cycle (push local changes, detect new/changed/vanished mail) as
//!   pure logic over a backend, emitting [`engine::SyncEvent`]s.
//! - [`state`]: persisted per-account sync state (UIDVALIDITY cursors, UID ↔ local id links)
//!   and the local→remote diff that doubles as the offline queue.
//! - [`local`]: fetched messages → store as an upsert on their server identity (never a
//!   duplicate, even when the sync state is lost or a sync runs twice).
//! - [`worker`]: background thread (+ optional IDLE thread) driven by channels.
//! - [`mime`], [`thread`], [`formats`]: RFC 5322 parsing, conversation threading, .eml/.mbox.
//! - [`config`], [`oauth`]: account server settings, OS keyring credentials, OAuth2 PKCE.
//! - [`dav`]: CalDAV/CardDAV discovery and two-way sync of calendars and contacts.
//!
//! Backends never write to the store: the UI bridge turns events into `mailbox_core::Command`s
//! (new mail through [`local::store_fetched`]).

pub mod backend;
pub mod config;
pub mod dav;
pub mod engine;
pub mod formats;
pub mod imap_smtp;
pub mod local;
pub mod mime;
pub mod oauth;
pub mod state;
pub mod thread;
pub mod worker;

pub use backend::{MailBackend, MemoryBackend, RemoteFlags, RemoteFolder, Uid};
pub use config::AccountConfig;
pub use engine::{PushOp, SyncEvent};
pub use mime::{AttachmentData, ParsedMail};
pub use state::SyncState;
pub use thread::ThreadIndex;
pub use worker::{OutgoingMessage, Request, WorkerHandle};

/// Errors a backend or sync step can report.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("connection failed: {0}")]
    Connection(String),
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("not supported by this backend: {0}")]
    Unsupported(&'static str),
    #[error("cannot parse message: {0}")]
    Parse(String),
    #[error("credentials: {0}")]
    Credentials(String),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid data: {0}")]
    Json(#[from] serde_json::Error),
}

pub type SyncResult<T> = Result<T, SyncError>;
