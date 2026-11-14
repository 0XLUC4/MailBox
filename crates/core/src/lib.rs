//! Mailbox core: domain model, SQLite store, commands (undo/redo) and sample data.
//!
//! Every mutation goes through [`Command`] + [`History::run`]; reads go straight to [`Store`].

pub mod command;
pub mod compose;
pub mod error;
pub mod model;
pub mod sample;
pub mod store;

pub use command::{Command, History};
pub use error::{Error, Result};
pub use model::*;
pub use store::Store;
