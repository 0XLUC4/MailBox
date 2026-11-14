//! Mailbox core: domain model, SQLite store, commands (undo/redo).
//! Sample data ([`sample`]) is only compiled for tests or with the `dev` feature.
//!
//! Every mutation goes through [`Command`] + [`History::run`]; reads go straight to [`Store`].

pub mod command;
pub mod compose;
pub mod error;
pub mod mail_ops;
pub mod model;
pub mod people;
pub mod remote;
#[cfg(any(test, feature = "dev"))]
pub mod sample;
pub mod store;
pub mod todo;
pub mod vcard;

// Search language, rules engine, preferences.
pub mod rules;
pub mod search;
pub mod settings;
pub mod smart_folder;

pub mod calendar;
pub mod ics;
pub mod recurrence;

// Everyday mail features: snooze / send later / mute, out of office, read receipts, export.
pub mod auto_reply;
pub mod export;
pub mod later;
pub mod receipts;

pub use command::{Command, History};
pub use error::{Error, Result};
pub use model::*;
pub use people::PeopleCommand;
pub use store::Store;
pub use todo::TaskCommand;
