//! Mailbox core: domain model, SQLite store, commands (undo/redo) and sample data.
//!
//! Every mutation goes through [`Command`] + [`History::run`]; reads go straight to [`Store`].

pub mod command;
pub mod error;
pub mod mail_ops;
pub mod model;
pub mod people;
pub mod sample;
pub mod store;
pub mod todo;
pub mod vcard;

// Search language, rules engine, preferences.
pub mod rules;
pub mod search;
pub mod settings;

pub use command::{Command, History};
pub use error::{Error, Result};
pub use model::*;
pub use people::PeopleCommand;
pub use store::Store;
pub use todo::TaskCommand;
