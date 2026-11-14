//! App-wide GPUI actions: the catalog (labels, icons, shortcuts) used by the ribbon and the
//! command palette, the key bindings, and the shell-level handlers.
//!
//! Views may handle an action closer to the focus (e.g. the message list acts on its whole
//! multi-selection); the handlers installed here on the shell root run otherwise, so they are the
//! default implementation. Calendar, People and Tasks actions are routed to those views through
//! [`ModuleViews`].

use chrono::{Duration, Timelike as _, Utc};
use gpui::{
    Action, App, Entity, Global, InteractiveElement, KeyBinding, SharedString, WeakEntity, Window,
};
use mailbox_core::settings::{Density, ReadingPane, RibbonMode, ThemePreference};
use mailbox_core::{Command, FolderRole, Message, rules};

pub use crate::SendReceive; // defined with the sync bridge
use crate::app_state::{AppState, Module};
use crate::calendar::{CalendarView, EditorTarget};
use crate::components::IconName;
use crate::compose::{ComposeRequest, open_compose};
use crate::everyday::{
    ExportMailbox, ImportMailbox, MuteConversation, OpenAutoReply, OpenReadReceipts, PrintMessage,
    SaveAllAttachments, SaveMessageAs, SetDefaultMailApp, SnoozeMessage,
};
use crate::mail::ComposeKind;
use crate::people::PeopleView;
use crate::search::SearchView;
use crate::settings::{prefs, update_settings};
use crate::tasks::TasksView;
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
    spec!("snooze", "Snooze", AlarmClock, Some("mod+shift+H"), "later remind reappear", SnoozeMessage),
    spec!("mute", "Mute conversation", BellOff, None, "ignore silence thread", MuteConversation),
    spec!("print", "Print", Printer, Some("mod+P"), "paper pdf", PrintMessage),
    spec!("save-eml", "Save as .eml", FileDown, Some("F12"), "export message file", SaveMessageAs),
    spec!("save-attachments", "Save all attachments", Download, None, "files download", SaveAllAttachments),
    spec!("export-mailbox", "Export mailbox (.mbox)", FolderOutput, None, "backup file mbox", ExportMailbox),
    spec!("import-mailbox", "Import .mbox / .eml", FolderInput, None, "restore file mbox eml", ImportMailbox),
    spec!("auto-reply", "Automatic replies", PlaneTakeoff, None, "out of office vacation away", OpenAutoReply),
    spec!("read-receipts", "Read receipts", MailCheck, None, "disposition notification", OpenReadReceipts),
    spec!("default-mail-app", "Set as default mail app", Mail, None, "mailto handler windows", SetDefaultMailApp),
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
        KeyBinding::new(primary!("shift-h"), SnoozeMessage, shell),
        KeyBinding::new(primary!("p"), PrintMessage, shell),
        KeyBinding::new("f12", SaveMessageAs, shell),
        KeyBinding::new("delete", DeleteSelection, shell),
        KeyBinding::new("insert", ToggleFlag, shell),
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
    let element = handle::<_, NewMail>(element, &state, |state, window, cx| {
        open_compose(state, ComposeRequest::New, window, cx)
    });
    let element = handle::<_, Reply>(element, &state, |state, window, cx| {
        respond(state, ComposeKind::Reply, window, cx)
    });
    let element = handle::<_, ReplyAll>(element, &state, |state, window, cx| {
        respond(state, ComposeKind::ReplyAll, window, cx)
    });
    let element = handle::<_, Forward>(element, &state, |state, window, cx| {
        respond(state, ComposeKind::Forward, window, cx)
    });
    let element = handle::<_, NewEvent>(element, &state, new_event);
    let element = handle::<_, CalendarToday>(element, &state, |state, _, cx| {
        with_calendar(state, cx, |calendar, cx| calendar.action_today(cx))
    });
    let element = handle::<_, CalendarDayView>(element, &state, |state, _, cx| {
        with_calendar(state, cx, |calendar, cx| calendar.action_show("day", cx))
    });
    let element = handle::<_, CalendarWeekView>(element, &state, |state, _, cx| {
        with_calendar(state, cx, |calendar, cx| calendar.action_show("week", cx))
    });
    let element = handle::<_, CalendarMonthView>(element, &state, |state, _, cx| {
        with_calendar(state, cx, |calendar, cx| calendar.action_show("month", cx))
    });
    let element = handle::<_, NewContact>(element, &state, |state, window, cx| {
        go_to(state, Module::People, cx);
        if let Some(people) = views(cx).and_then(|views| views.people.upgrade()) {
            people.update(cx, |people, cx| people.action_new_contact(window, cx));
        }
    });
    let element = handle::<_, NewTask>(element, &state, |state, window, cx| {
        go_to(state, Module::Tasks, cx);
        if let Some(tasks) = views(cx).and_then(|views| views.tasks.upgrade()) {
            tasks.update(cx, |tasks, cx| tasks.action_new_task(window, cx));
        }
    });
    let element = handle::<_, CompleteTask>(element, &state, |state, _, cx| {
        let done = views(cx)
            .and_then(|views| views.tasks.upgrade())
            .is_some_and(|tasks| tasks.update(cx, |tasks, cx| tasks.action_complete(cx)));
        if !done {
            state.update(cx, |state, cx| state.notify_info("Select a task first", cx));
        }
    });
    let element = handle::<_, OpenSignatures>(element, &state, |state, window, cx| {
        crate::signatures::open(state.clone(), window, cx)
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
        crate::mail::open_move_dialog(state.clone(), vec![message.id], window, cx);
    });
    let element = handle::<_, Categorize>(element, &state, |state, window, cx| {
        let Some(message) = selected_message(state, cx) else {
            return no_selection(state, cx);
        };
        crate::mail::open_category_dialog(state.clone(), vec![message.id], window, cx);
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
        crate::shortcuts::open(window, cx)
    });
    let element = crate::everyday::install(element, &state);
    handle::<_, OpenAbout>(element, &state, |_, window, cx| {
        dialogs::open_about(window, cx)
    })
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

// ---------- module routing ----------

/// Weak handles to the module views, set by the shell, so ribbon and palette actions can reach
/// them even when the focus is elsewhere.
pub struct ModuleViews {
    pub calendar: WeakEntity<CalendarView>,
    pub people: WeakEntity<PeopleView>,
    pub tasks: WeakEntity<TasksView>,
}

impl Global for ModuleViews {}

fn views(cx: &App) -> Option<&ModuleViews> {
    cx.try_global::<ModuleViews>()
}

fn go_to(state: &Entity<AppState>, module: Module, cx: &mut App) {
    state.update(cx, |state, cx| state.set_module(module, cx));
}

/// Shows the calendar and runs `run` on it.
fn with_calendar(
    state: &Entity<AppState>,
    cx: &mut App,
    run: impl FnOnce(&mut CalendarView, &mut gpui::Context<CalendarView>),
) {
    go_to(state, Module::Calendar, cx);
    if let Some(calendar) = views(cx).and_then(|views| views.calendar.upgrade()) {
        calendar.update(cx, run);
    }
}

/// New event: in the calendar at its anchor day, elsewhere at the next half hour.
fn new_event(state: &Entity<AppState>, _window: &mut Window, cx: &mut App) {
    if state.read(cx).module() == Module::Calendar
        && let Some(calendar) = views(cx).and_then(|views| views.calendar.upgrade())
    {
        return calendar.update(cx, |calendar, cx| calendar.action_new_event(cx));
    }
    let now = Utc::now();
    let start = now + Duration::minutes(30 - i64::from(now.minute()) % 30)
        - Duration::seconds(i64::from(now.second()))
        - Duration::nanoseconds(i64::from(now.nanosecond()));
    crate::calendar::open_event_editor(
        state.clone(),
        EditorTarget::New {
            start,
            end: start + Duration::minutes(30),
            all_day: false,
            calendar_id: None,
            title: String::new(),
        },
        cx,
    );
}

/// Reply / Reply all / Forward to the message in the reading pane.
fn respond(state: &Entity<AppState>, kind: ComposeKind, window: &mut Window, cx: &mut App) {
    match selected_message(state, cx) {
        Some(message) => crate::mail::compose_from(state, message.id, kind, window, cx),
        None => no_selection(state, cx),
    }
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
    match state.read(cx).module() {
        Module::Mail => {}
        Module::People => {
            let deleted = views(cx)
                .and_then(|views| views.people.upgrade())
                .is_some_and(|people| people.update(cx, |people, cx| people.action_delete(cx)));
            if !deleted {
                state.update(cx, |state, cx| {
                    state.notify_info("Select a contact first", cx)
                });
            }
            return;
        }
        Module::Tasks => {
            let deleted = views(cx)
                .and_then(|views| views.tasks.upgrade())
                .is_some_and(|tasks| tasks.update(cx, |tasks, cx| tasks.action_delete(cx)));
            if !deleted {
                state.update(cx, |state, cx| state.notify_info("Select a task first", cx));
            }
            return;
        }
        // Calendar deletes the selected event with its own Delete key handling.
        Module::Calendar | Module::Settings => return,
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
        id if crate::everyday::NEEDS_MESSAGE.contains(&id) => {
            state.module() == Module::Mail && state.selected_message().is_some()
        }
        "mark-all-read" | "run-rules" | "import-mailbox" => state.selected_folder().is_some(),
        "undo" => state.history().can_undo(),
        "redo" => state.history().can_redo(),
        _ => true,
    }
}
