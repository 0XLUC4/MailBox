//! App-wide GPUI actions: the catalog (labels, icons, shortcuts) used by the ribbon and the
//! command palette, the key bindings, and the shell-level handlers.
//!
//! Modules handle the actions they own on their root element (e.g. the mail view handles
//! [`Reply`]); the handlers installed here on the shell root only run when nothing closer to the
//! focus handled the action, so they act as the default implementation. Actions owned by other
//! modules get an app-level "not available yet" toast ([`register_fallbacks`]).

use gpui::{Action, App, Entity, InteractiveElement, KeyBinding, SharedString, Window};
use mailbox_core::settings::{Density, ReadingPane, RibbonMode, ThemePreference};
use mailbox_core::{Command, FolderRole, Message, rules};

use crate::app_state::{AppState, Module};
use crate::components::IconName;
use crate::search::SearchView;
use crate::settings::{prefs, update_settings};
use crate::{GoToCalendar, GoToMail, GoToPeople, GoToSettings, GoToTasks, Redo, Undo, dialogs};

gpui::actions!(
    mailbox,
    [
        /// "New" for the active module (mail, event, contact or task).
        NewItem,
        NewMail,
        NewEvent,
        NewContact,
        NewTask,
        Reply,
        ReplyAll,
        Forward,
        /// Deletes the selection (mail: to Trash, permanently from Trash).
        DeleteSelection,
        Archive,
        MoveToFolder,
        Categorize,
        ToggleFlag,
        MarkRead,
        MarkUnread,
        MarkAllRead,
        OpenRules,
        RunRules,
        SendReceive,
        CalendarToday,
        CalendarDayView,
        CalendarWeekView,
        CalendarMonthView,
        CompleteTask,
        ToggleCommandPalette,
        FocusSearch,
        ToggleRibbonMode,
        CycleReadingPane,
        ToggleConversations,
        ToggleDensity,
        ToggleDarkMode,
        OpenAddAccount,
        OpenAccountSettings,
        OpenShortcuts,
        OpenAbout,
        OpenSignatures,
    ]
);

/// Describes an action for menus, the ribbon and the palette.
pub struct ActionSpec {
    /// Stable id used by the ribbon layout.
    pub id: &'static str,
    /// English label (translated with [`crate::settings::tr`]).
    pub label: &'static str,
    pub icon: IconName,
    /// Shortcut shown to the user; `mod` is Ctrl (Cmd on macOS). Bound in [`bind_keys`].
    pub shortcut: Option<&'static str>,
    /// Extra words matched by the palette.
    pub keywords: &'static str,
    pub build: fn() -> Box<dyn Action>,
}

impl ActionSpec {
    pub fn action(&self) -> Box<dyn Action> {
        (self.build)()
    }

    /// Shortcut for display ("Ctrl+R" / "⌘R").
    pub fn shortcut_text(&self) -> Option<String> {
        self.shortcut.map(|shortcut| {
            if cfg!(target_os = "macos") {
                shortcut.replace("mod+", "⌘").replace("shift+", "⇧")
            } else {
                shortcut
                    .replace("mod+", "Ctrl+")
                    .replace("shift+", "Shift+")
            }
        })
    }
}

macro_rules! spec {
    ($id:literal, $label:literal, $icon:ident, $shortcut:expr, $keywords:literal, $action:expr) => {
        ActionSpec {
            id: $id,
            label: $label,
            icon: IconName::$icon,
            shortcut: $shortcut,
            keywords: $keywords,
            build: || -> Box<dyn Action> { Box::new($action) },
        }
    };
}

/// Every user-facing action, in palette order.
#[rustfmt::skip]
pub static ACTIONS: &[ActionSpec] = &[
    spec!("new", "New item", Plus, Some("mod+N"), "create compose add", NewItem),
    spec!("new-mail", "New mail", SquarePen, None, "compose write email message", NewMail),
    spec!("new-event", "New event", CalendarPlus, None, "meeting appointment", NewEvent),
    spec!("new-contact", "New contact", UserPlus, None, "person address", NewContact),
    spec!("new-task", "New task", ListChecks, None, "todo", NewTask),
    spec!("reply", "Reply", Reply, Some("mod+R"), "respond answer", Reply),
    spec!("reply-all", "Reply all", ReplyAll, Some("mod+shift+R"), "respond", ReplyAll),
    spec!("forward", "Forward", Forward, Some("mod+shift+F"), "send on", Forward),
    spec!("delete", "Delete", Trash, Some("Delete"), "remove trash bin", DeleteSelection),
    spec!("archive", "Archive", Archive, Some("mod+shift+E"), "file done", Archive),
    spec!("move", "Move to folder", FolderInput, Some("mod+shift+V"), "file", MoveToFolder),
    spec!("categorize", "Categorize", Tag, None, "label category colour", Categorize),
    spec!("flag", "Flag / unflag", Flag, Some("Insert"), "follow up star", ToggleFlag),
    spec!("mark-read", "Mark as read", MailOpen, Some("mod+Q"), "seen", MarkRead),
    spec!("mark-unread", "Mark as unread", Mail, Some("mod+U"), "unseen new", MarkUnread),
    spec!("mark-all-read", "Mark all as read", MailCheck, None, "folder seen", MarkAllRead),
    spec!("rules", "Rules", ListFilter, None, "filters automation manage", OpenRules),
    spec!("run-rules", "Run rules", WandSparkles, None, "apply filters now", RunRules),
    spec!("send-receive", "Send / receive all", RefreshCw, Some("F9"), "sync fetch", SendReceive),
    spec!("today", "Today", Calendar1, None, "calendar now", CalendarToday),
    spec!("day-view", "Day", CalendarDays, None, "calendar view", CalendarDayView),
    spec!("week-view", "Week", CalendarRange, None, "calendar view", CalendarWeekView),
    spec!("month-view", "Month", Calendar, None, "calendar view", CalendarMonthView),
    spec!("complete-task", "Mark complete", CircleCheck, None, "done finish", CompleteTask),
    spec!("undo", "Undo", Undo2, Some("mod+Z"), "revert", Undo),
    spec!("redo", "Redo", Redo2, Some("mod+Y"), "again", Redo),
    spec!("go-mail", "Go to Mail", Mail, Some("mod+1"), "inbox module", GoToMail),
    spec!("go-calendar", "Go to Calendar", CalendarDays, Some("mod+2"), "module", GoToCalendar),
    spec!("go-people", "Go to People", Users, Some("mod+3"), "contacts module", GoToPeople),
    spec!("go-tasks", "Go to Tasks", SquareCheck, Some("mod+4"), "todo module", GoToTasks),
    spec!("settings", "Open settings", Settings, Some("mod+,"), "preferences options", GoToSettings),
    spec!("palette", "Command palette", Command, Some("mod+K"), "actions run", ToggleCommandPalette),
    spec!("search", "Search", Search, Some("mod+E"), "find query", FocusSearch),
    spec!("ribbon-mode", "Classic ribbon", LayoutPanelTop, None, "toolbar simplified", ToggleRibbonMode),
    spec!("reading-pane", "Reading pane", PanelRight, None, "preview layout bottom", CycleReadingPane),
    spec!("conversations", "Conversations", MessagesSquare, None, "threads group", ToggleConversations),
    spec!("density", "Compact", Rows3, None, "density comfortable spacing", ToggleDensity),
    spec!("dark-mode", "Dark mode", Moon, None, "theme light appearance", ToggleDarkMode),
    spec!("add-account", "Add account", UserPlus, None, "imap smtp gmail outlook", OpenAddAccount),
    spec!("account-settings", "Account settings", UserCog, None, "server imap smtp", OpenAccountSettings),
    spec!("signatures", "Signatures", Pencil, None, "signature compose", OpenSignatures),
    spec!("shortcuts", "Keyboard shortcuts", Keyboard, Some("mod+/"), "keys help", OpenShortcuts),
    spec!("about", "About Mailbox", Info, None, "version help", OpenAbout),
];

/// Looks up a catalog entry by id.
pub fn spec(id: &str) -> Option<&'static ActionSpec> {
    ACTIONS.iter().find(|spec| spec.id == id)
}

/// Platform primary modifier + key, as a `&'static str`.
macro_rules! primary {
    ($key:literal) => {
        if cfg!(target_os = "macos") {
            concat!("cmd-", $key)
        } else {
            concat!("ctrl-", $key)
        }
    };
}

/// Key bindings for the catalog (keep in sync with [`ActionSpec::shortcut`]).
pub fn bind_keys(cx: &mut App) {
    let shell = Some("Shell");
    cx.bind_keys([
        KeyBinding::new(primary!("k"), ToggleCommandPalette, shell),
        KeyBinding::new(primary!("shift-p"), ToggleCommandPalette, shell),
        KeyBinding::new(primary!("e"), FocusSearch, shell),
        KeyBinding::new(primary!("n"), NewItem, shell),
        KeyBinding::new(primary!("r"), Reply, shell),
        KeyBinding::new(primary!("shift-r"), ReplyAll, shell),
        KeyBinding::new(primary!("shift-f"), Forward, shell),
        KeyBinding::new(primary!("shift-e"), Archive, shell),
        KeyBinding::new(primary!("shift-v"), MoveToFolder, shell),
        KeyBinding::new(primary!("q"), MarkRead, shell),
        KeyBinding::new(primary!("u"), MarkUnread, shell),
        KeyBinding::new(primary!("/"), OpenShortcuts, shell),
        KeyBinding::new("delete", DeleteSelection, shell),
        KeyBinding::new("insert", ToggleFlag, shell),
        KeyBinding::new("f9", SendReceive, shell),
    ]);
}

/// Attaches the default handlers to the shell root element.
pub fn install<E: InteractiveElement>(
    element: E,
    state: Entity<AppState>,
    search: Entity<SearchView>,
) -> E {
    let element = handle::<_, ToggleCommandPalette>(element, &state, |state, window, cx| {
        crate::command_palette::open(state.clone(), window, cx)
    });
    let element = element.on_action(move |_: &FocusSearch, window, cx| {
        search.update(cx, |search, cx| search.focus(window, cx));
    });
    let element = handle::<_, NewItem>(element, &state, |state, window, cx| {
        let action: Box<dyn Action> = match state.read(cx).module() {
            Module::Mail => Box::new(NewMail),
            Module::Calendar => Box::new(NewEvent),
            Module::People => Box::new(NewContact),
            Module::Tasks => Box::new(NewTask),
            Module::Settings => Box::new(OpenAddAccount),
        };
        window.dispatch_action(action, cx);
    });
    let element = handle::<_, DeleteSelection>(element, &state, delete_selection);
    let element = handle::<_, Archive>(element, &state, |state, _, cx| {
        move_selection_to_role(state, FolderRole::Archive, cx)
    });
    let element = handle::<_, ToggleFlag>(element, &state, |state, _, cx| {
        with_selected(state, cx, |message| Command::SetMessageFlags {
            ids: vec![message.id],
            read: None,
            flagged: Some(!message.flags.flagged),
            pinned: None,
        })
    });
    let element = handle::<_, MarkRead>(element, &state, |state, _, cx| set_read(state, true, cx));
    let element =
        handle::<_, MarkUnread>(element, &state, |state, _, cx| set_read(state, false, cx));
    let element =
        handle::<_, MarkAllRead>(element, &state, |state, _, cx| mark_all_read(state, cx));
    let element = handle::<_, MoveToFolder>(element, &state, |state, window, cx| {
        let Some(message) = selected_message(state, cx) else {
            return no_selection(state, cx);
        };
        dialogs::pick_folder(state.clone(), message.id, window, cx);
    });
    let element = handle::<_, Categorize>(element, &state, |state, window, cx| {
        let Some(message) = selected_message(state, cx) else {
            return no_selection(state, cx);
        };
        dialogs::pick_category(state.clone(), message.id, window, cx);
    });
    let element = handle::<_, OpenRules>(element, &state, |state, window, cx| {
        dialogs::rules::open(state.clone(), window, cx)
    });
    let element = handle::<_, RunRules>(element, &state, |state, _, cx| run_rules(state, cx));
    let element = handle::<_, ToggleRibbonMode>(element, &state, |state, _, cx| {
        update_settings(state, cx, |settings| {
            settings.ribbon = match settings.ribbon {
                RibbonMode::Simplified => RibbonMode::Classic,
                RibbonMode::Classic => RibbonMode::Simplified,
            }
        })
    });
    let element = handle::<_, CycleReadingPane>(element, &state, |state, _, cx| {
        update_settings(state, cx, |settings| {
            settings.reading_pane = match settings.reading_pane {
                ReadingPane::Right => ReadingPane::Bottom,
                ReadingPane::Bottom => ReadingPane::Off,
                ReadingPane::Off => ReadingPane::Right,
            }
        })
    });
    let element = handle::<_, ToggleConversations>(element, &state, |state, _, cx| {
        update_settings(state, cx, |settings| {
            settings.conversations = !settings.conversations
        })
    });
    let element = handle::<_, ToggleDensity>(element, &state, |state, _, cx| {
        update_settings(state, cx, |settings| {
            settings.density = match settings.density {
                Density::Compact => Density::Comfortable,
                Density::Comfortable => Density::Compact,
            }
        })
    });
    let element = handle::<_, ToggleDarkMode>(element, &state, |state, _, cx| {
        let dark = crate::theme::theme(cx).is_dark;
        update_settings(state, cx, |settings| {
            settings.theme = if dark {
                ThemePreference::Light
            } else {
                ThemePreference::Dark
            }
        })
    });
    let element = handle::<_, OpenAddAccount>(element, &state, |state, window, cx| {
        dialogs::account::open_add_account(state.clone(), window, cx)
    });
    let element = handle::<_, OpenAccountSettings>(element, &state, |state, window, cx| {
        let account = current_account(state, cx);
        match account {
            Some(id) => dialogs::account::open_account_settings(state.clone(), id, window, cx),
            None => dialogs::account::open_add_account(state.clone(), window, cx),
        }
    });
    let element = handle::<_, OpenShortcuts>(element, &state, |_, window, cx| {
        dialogs::open_shortcuts(window, cx)
    });
    handle::<_, OpenAbout>(element, &state, |_, window, cx| {
        dialogs::open_about(window, cx)
    })
}

/// App-level fallbacks for actions owned by other modules (compose, calendar, tasks, sync).
/// Global listeners run after every element handler, so they never shadow a module's handler;
/// they only explain why nothing happened. Call once (the ribbon does).
pub fn register_fallbacks(state: &Entity<AppState>, cx: &mut App) {
    unavailable::<NewMail>(state, "New mail", cx);
    unavailable::<NewEvent>(state, "New event", cx);
    unavailable::<NewContact>(state, "New contact", cx);
    unavailable::<NewTask>(state, "New task", cx);
    unavailable::<Reply>(state, "Reply", cx);
    unavailable::<ReplyAll>(state, "Reply all", cx);
    unavailable::<Forward>(state, "Forward", cx);
    unavailable::<SendReceive>(state, "Send / receive", cx);
    unavailable::<CalendarToday>(state, "Calendar views", cx);
    unavailable::<CalendarDayView>(state, "Calendar views", cx);
    unavailable::<CalendarWeekView>(state, "Calendar views", cx);
    unavailable::<CalendarMonthView>(state, "Calendar views", cx);
    unavailable::<CompleteTask>(state, "Completing tasks", cx);
    unavailable::<OpenSignatures>(state, "Signatures", cx);
}

type Handler = fn(&Entity<AppState>, &mut Window, &mut App);

fn handle<E: InteractiveElement, A: Action>(
    element: E,
    state: &Entity<AppState>,
    handler: Handler,
) -> E {
    let state = state.clone();
    element.on_action(move |_: &A, window, cx| handler(&state, window, cx))
}

fn unavailable<A: Action>(state: &Entity<AppState>, what: &'static str, cx: &mut App) {
    let state = state.downgrade();
    cx.on_action(move |_: &A, cx| {
        if let Some(state) = state.upgrade() {
            state.update(cx, |state, cx| {
                state.notify_info(format!("{what} is not available here yet"), cx)
            });
        }
    });
}

// ---------- default mail handlers ----------

fn selected_message(state: &Entity<AppState>, cx: &App) -> Option<Message> {
    let state = state.read(cx);
    let id = state.selected_message()?;
    state.store().get::<Message>(id).ok().flatten()
}

fn no_selection(state: &Entity<AppState>, cx: &mut App) {
    state.update(cx, |state, cx| {
        state.notify_info("Select a message first", cx)
    });
}

/// Runs the command built from the selected message, or explains there is no selection.
fn with_selected(state: &Entity<AppState>, cx: &mut App, build: impl FnOnce(&Message) -> Command) {
    match selected_message(state, cx) {
        Some(message) => {
            let command = build(&message);
            state.update(cx, |state, cx| {
                state.run(command, cx);
            });
        }
        None => no_selection(state, cx),
    }
}

fn set_read(state: &Entity<AppState>, read: bool, cx: &mut App) {
    with_selected(state, cx, |message| Command::SetMessageFlags {
        ids: vec![message.id],
        read: Some(read),
        flagged: None,
        pinned: None,
    })
}

fn mark_all_read(state: &Entity<AppState>, cx: &mut App) {
    state.update(cx, |state, cx| {
        let Some(folder) = state.selected_folder() else {
            return;
        };
        let ids: Vec<_> = state
            .store()
            .list_messages(folder)
            .unwrap_or_default()
            .into_iter()
            .filter(|message| !message.flags.read)
            .map(|message| message.id)
            .collect();
        if ids.is_empty() {
            state.notify_info("Everything is already read", cx);
        } else {
            state.run(
                Command::SetMessageFlags {
                    ids,
                    read: Some(true),
                    flagged: None,
                    pinned: None,
                },
                cx,
            );
        }
    });
}

/// Moves the selected message to the account folder with `role`.
fn move_selection_to_role(state: &Entity<AppState>, role: FolderRole, cx: &mut App) {
    let Some(message) = selected_message(state, cx) else {
        return no_selection(state, cx);
    };
    state.update(cx, |state, cx| {
        let target = state.store().folder_by_role(message.account_id, role);
        match target {
            Ok(Some(folder)) if folder.id == message.folder_id => {}
            Ok(Some(folder)) => {
                if state.run(
                    Command::MoveMessages {
                        ids: vec![message.id],
                        folder_id: folder.id,
                    },
                    cx,
                ) {
                    state.select_message(None, cx);
                }
            }
            Ok(None) => {
                state.notify_info(format!("This account has no {} folder", role.as_str()), cx)
            }
            Err(err) => state.report_error(err, cx),
        }
    });
}

fn delete_selection(state: &Entity<AppState>, window: &mut Window, cx: &mut App) {
    if state.read(cx).module() != Module::Mail {
        return state.update(cx, |state, cx| {
            state.notify_info("Delete is handled by the active view", cx)
        });
    }
    let Some(message) = selected_message(state, cx) else {
        return no_selection(state, cx);
    };
    let in_trash = state
        .read(cx)
        .store()
        .folder_by_role(message.account_id, FolderRole::Trash)
        .ok()
        .flatten()
        .is_none_or(|trash| trash.id == message.folder_id);
    if !in_trash {
        return move_selection_to_role(state, FolderRole::Trash, cx);
    }
    let state = state.clone();
    let subject: SharedString = if message.subject.is_empty() {
        "(no subject)".into()
    } else {
        message.subject.clone().into()
    };
    dialogs::confirm(
        dialogs::Confirm {
            title: "Delete permanently?".into(),
            description: format!(
                "\u{201c}{subject}\u{201d} will be deleted. You can still undo it."
            )
            .into(),
            ok: "Delete".into(),
            danger: true,
        },
        move |_, cx| {
            state.update(cx, |state, cx| {
                if state.run(
                    Command::DeleteMessages {
                        ids: vec![message.id],
                    },
                    cx,
                ) {
                    state.select_message(None, cx);
                }
            });
        },
        window,
        cx,
    );
}

/// Account of the selected folder, else the first account.
pub fn current_account(state: &Entity<AppState>, cx: &App) -> Option<mailbox_core::Id> {
    let state = state.read(cx);
    let store = state.store();
    state
        .selected_folder()
        .and_then(|id| store.get::<mailbox_core::Folder>(id).ok().flatten())
        .map(|folder| folder.account_id)
        .or_else(|| {
            store
                .list_accounts()
                .ok()
                .and_then(|accounts| accounts.first().map(|account| account.id))
        })
}

/// Runs the rules of the selected folder's account on that folder.
pub fn run_rules(state: &Entity<AppState>, cx: &mut App) {
    let account = current_account(state, cx);
    state.update(cx, |state, cx| {
        let (Some(account), Some(folder)) = (account, state.selected_folder()) else {
            return state.notify_info("Select a folder to run rules on", cx);
        };
        let ids: Vec<_> = state
            .store()
            .list_messages(folder)
            .unwrap_or_default()
            .into_iter()
            .map(|message| message.id)
            .collect();
        let result = rules::run_command(state.store(), account, &ids);
        match result {
            Ok((Some(command), changed)) => {
                if state.run(command, cx) {
                    state.notify_info(format!("Rules updated {changed} message(s)"), cx);
                }
            }
            Ok((None, _)) => state.notify_info("No message matched your rules", cx),
            Err(err) => state.report_error(err, cx),
        }
    });
}

/// Whether a catalog action is currently "on" (for toggle buttons in the ribbon).
pub fn is_checked(id: &str, cx: &App) -> bool {
    let settings = prefs(cx);
    match id {
        "ribbon-mode" => settings.ribbon == RibbonMode::Classic,
        "conversations" => settings.conversations,
        "density" => settings.density == Density::Compact,
        "dark-mode" => crate::theme::theme(cx).is_dark,
        "reading-pane" => settings.reading_pane != ReadingPane::Off,
        _ => false,
    }
}

/// Whether a catalog action can run now (mail actions need a selected message).
pub fn is_enabled(id: &str, state: &AppState) -> bool {
    match id {
        "reply" | "reply-all" | "forward" | "delete" | "archive" | "move" | "categorize"
        | "flag" | "mark-read" | "mark-unread" => {
            state.module() != Module::Mail || state.selected_message().is_some()
        }
        "mark-all-read" | "run-rules" => state.selected_folder().is_some(),
        "undo" => state.history().can_undo(),
        "redo" => state.history().can_redo(),
        _ => true,
    }
}
