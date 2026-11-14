//! Shared application state: the store, the undo history and the cross-view selection.
//!
//! Views hold an `Entity<AppState>`, read with `state.read(cx).store()`, mutate with
//! `state.update(cx, |state, cx| state.run(command, cx))` and `cx.observe(&state, ...)` to re-render.

use gpui::{Context, EventEmitter, SharedString};
use mailbox_core::{Command, FolderRole, History, Id, Store};

use crate::components::IconName;

/// Top-level modules shown in the left rail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Module {
    Mail,
    Calendar,
    People,
    Tasks,
    Settings,
}

impl Module {
    /// Modules in rail order (Settings is pinned at the bottom).
    pub const MAIN: [Module; 4] = [Self::Mail, Self::Calendar, Self::People, Self::Tasks];

    pub fn label(self) -> &'static str {
        match self {
            Self::Mail => "Mail",
            Self::Calendar => "Calendar",
            Self::People => "People",
            Self::Tasks => "Tasks",
            Self::Settings => "Settings",
        }
    }

    pub fn icon(self) -> IconName {
        match self {
            Self::Mail => IconName::Mail,
            Self::Calendar => IconName::CalendarDays,
            Self::People => IconName::Users,
            Self::Tasks => IconName::SquareCheck,
            Self::Settings => IconName::Settings,
        }
    }
}

/// Events for the shell (toasts). Views normally just observe the entity.
#[derive(Debug, Clone)]
pub enum AppEvent {
    Error(SharedString),
    Info(SharedString),
}

pub struct AppState {
    store: Store,
    history: History,
    module: Module,
    selected_folder: Option<Id>,
    selected_message: Option<Id>,
    search_query: String,
    /// Sync status line (set by `sync_bridge`).
    sync_status: SharedString,
}

impl EventEmitter<AppEvent> for AppState {}

impl AppState {
    /// Wraps the store and selects the first account's inbox.
    pub fn new(store: Store) -> Self {
        let selected_folder = store
            .list_accounts()
            .ok()
            .and_then(|accounts| accounts.first().map(|account| account.id))
            .and_then(|account_id| {
                store
                    .folder_by_role(account_id, FolderRole::Inbox)
                    .ok()
                    .flatten()
            })
            .map(|folder| folder.id);
        Self {
            store,
            history: History::new(),
            module: Module::Mail,
            selected_folder,
            selected_message: None,
            search_query: String::new(),
            sync_status: SharedString::default(),
        }
    }

    /// Read-only access. Never mutate through it: use [`AppState::run`].
    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn history(&self) -> &History {
        &self.history
    }

    /// Applies a command (undoable), re-renders observers, shows a toast on error.
    /// Returns whether it succeeded.
    pub fn run(&mut self, command: Command, cx: &mut Context<Self>) -> bool {
        let result = self.history.run(&self.store, command);
        self.finish(result.map(|_| None), cx)
    }

    pub fn undo(&mut self, cx: &mut Context<Self>) {
        let result = self
            .history
            .undo(&self.store)
            .map(|label| label.map(|l| format!("Undid: {l}")));
        self.finish(result, cx);
    }

    pub fn redo(&mut self, cx: &mut Context<Self>) {
        let result = self
            .history
            .redo(&self.store)
            .map(|label| label.map(|l| format!("Redid: {l}")));
        self.finish(result, cx);
    }

    fn finish(
        &mut self,
        result: mailbox_core::Result<Option<String>>,
        cx: &mut Context<Self>,
    ) -> bool {
        match result {
            Ok(info) => {
                if let Some(info) = info {
                    cx.emit(AppEvent::Info(info.into()));
                }
                cx.notify();
                true
            }
            Err(err) => {
                self.report_error(err, cx);
                false
            }
        }
    }

    /// Shows an error toast (for failures outside `run`, e.g. a failed read).
    pub fn report_error(&mut self, err: impl std::fmt::Display, cx: &mut Context<Self>) {
        cx.emit(AppEvent::Error(err.to_string().into()));
    }

    /// Shows an informational toast.
    pub fn notify_info(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        cx.emit(AppEvent::Info(message.into()));
    }

    // ---------- selection (UI state, not undoable) ----------

    pub fn module(&self) -> Module {
        self.module
    }

    pub fn set_module(&mut self, module: Module, cx: &mut Context<Self>) {
        if self.module != module {
            self.module = module;
            cx.notify();
        }
    }

    pub fn selected_folder(&self) -> Option<Id> {
        self.selected_folder
    }

    pub fn select_folder(&mut self, folder: Option<Id>, cx: &mut Context<Self>) {
        if self.selected_folder != folder {
            self.selected_folder = folder;
            self.selected_message = None;
            cx.notify();
        }
    }

    pub fn selected_message(&self) -> Option<Id> {
        self.selected_message
    }

    pub fn select_message(&mut self, message: Option<Id>, cx: &mut Context<Self>) {
        if self.selected_message != message {
            self.selected_message = message;
            cx.notify();
        }
    }

    pub fn search_query(&self) -> &str {
        &self.search_query
    }

    pub fn set_search_query(&mut self, query: impl Into<String>, cx: &mut Context<Self>) {
        let query = query.into();
        if self.search_query != query {
            self.search_query = query;
            cx.notify();
        }
    }

    // ---------- sync status (set by sync_bridge) ----------

    /// e.g. "Up to date", "Syncing me@example.com…", "Working offline". Empty = no sync.
    pub fn sync_status(&self) -> &SharedString {
        &self.sync_status
    }

    pub fn set_sync_status(&mut self, status: impl Into<SharedString>, cx: &mut Context<Self>) {
        let status = status.into();
        if self.sync_status != status {
            self.sync_status = status;
            cx.notify();
        }
    }
}
