//! Desktop notifications and the unread count of the main window.
//!
//! - New mail (called by the sync bridge with the ids that just arrived in an inbox): muted
//!   conversations are archived silently, automatic replies are queued, then one system
//!   notification shows the sender and subject (`Settings::notify_new_mail`, `notify_sound`).
//! - Reminders: [`reminder`] (`Settings::notify_reminders`).
//! - Windows: a toast; clicking it brings the main window forward (and opens the message).
//! - The window title (shown on the taskbar button) carries the inbox unread count. A real
//!   overlay badge needs `ITaskbarList3` (unsafe COM), which this workspace forbids.
//!
//! macOS and Linux: a system notification through `osascript` / `notify-send` (clicking it
//! does not open the message), the in-app toasts and the title.

use mailbox_core::dates::LocalizedFormat;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use gpui::{AnyWindowHandle, App, Entity, Global};
use mailbox_core::{FolderRole, Id, Message};

use crate::app_state::{AppState, Module};
use crate::settings::{prefs, tr};

/// How often toast clicks are picked up.
const POLL_INTERVAL: Duration = Duration::from_millis(400);

/// What a toast click opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activation {
    App,
    Message(Id),
}

struct Notifications {
    clicks: Sender<Activation>,
}

impl Global for Notifications {}

/// Starts the click listener and the unread-count title. Call once, after the main window opens.
pub fn init(state: Entity<AppState>, main_window: AnyWindowHandle, cx: &mut App) {
    let (clicks, received) = channel();
    cx.set_global(Notifications { clicks });
    poll_clicks(state.clone(), main_window, received, cx);

    let mut last_title = String::new();
    update_title(&state, main_window, &mut last_title, cx);
    cx.observe(&state, move |state, cx| {
        update_title(&state, main_window, &mut last_title, cx)
    })
    .detach();
}

/// Brings the main window forward; opens `message` when given.
pub fn focus(
    state: &Entity<AppState>,
    main_window: AnyWindowHandle,
    activation: Activation,
    cx: &mut App,
) {
    cx.activate(true);
    main_window
        .update(cx, |_, window, _| window.activate_window())
        .ok();
    if let Activation::Message(id) = activation {
        let message = state.read(cx).store().get::<Message>(id).ok().flatten();
        if let Some(message) = message {
            state.update(cx, |state, cx| {
                state.set_module(Module::Mail, cx);
                state.select_folder(Some(message.folder_id), cx);
                state.select_message(Some(id), cx);
            });
        }
    }
}

fn poll_clicks(
    state: Entity<AppState>,
    main_window: AnyWindowHandle,
    received: Receiver<Activation>,
    cx: &mut App,
) {
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(POLL_INTERVAL).await;
            let clicks: Vec<Activation> = received.try_iter().collect();
            let Some(last) = clicks.last().copied() else {
                continue;
            };
            cx.update(|cx| focus(&state, main_window, last, cx));
        }
    })
    .detach();
}

/// Unread messages in every inbox.
fn inbox_unread(state: &AppState) -> i64 {
    let store = state.store();
    store
        .list_accounts()
        .unwrap_or_default()
        .iter()
        .filter_map(|account| {
            store
                .folder_by_role(account.id, FolderRole::Inbox)
                .ok()
                .flatten()
        })
        .map(|inbox| store.unread_count(inbox.id).unwrap_or(0))
        .sum()
}

fn update_title(
    state: &Entity<AppState>,
    main_window: AnyWindowHandle,
    last: &mut String,
    cx: &mut App,
) {
    let unread = inbox_unread(state.read(cx));
    let title = if unread > 0 {
        format!("Mailbox ({unread})")
    } else {
        "Mailbox".to_owned()
    };
    if *last != title {
        main_window
            .update(cx, |_, window, _| window.set_window_title(&title))
            .ok();
        *last = title;
    }
}

/// Shows a desktop notification; clicking it runs `activation`. Errors are ignored: a missing
/// toast must never disturb the app.
pub fn show(title: &str, body: &str, activation: Activation, cx: &App) {
    let Some(global) = cx.try_global::<Notifications>() else {
        return;
    };
    let clicks = global.clicks.clone();
    let sound = prefs(cx).notify_sound;
    platform::show(title, body, sound, move || {
        // The receiver lives as long as the app.
        let _ = clicks.send(activation);
    });
}

/// Called by the sync bridge with unread messages that just arrived in an inbox. Deferred:
/// the bridge is mid-update and automatic replies go back through it (Outbox, send).
pub fn on_new_mail(state: &Entity<AppState>, ids: Vec<Id>, cx: &mut App) {
    let state = state.clone();
    cx.defer(move |cx| handle_new_mail(&state, ids, cx));
}

fn handle_new_mail(state: &Entity<AppState>, ids: Vec<Id>, cx: &mut App) {
    let ids = crate::later::archive_muted(state, ids, cx);
    if ids.is_empty() {
        return;
    }
    crate::auto_reply::process(state, &ids, cx);
    if !prefs(cx).notify_new_mail {
        return;
    }
    let messages: Vec<Message> = {
        let store = state.read(cx).store();
        ids.iter()
            .filter_map(|id| store.get::<Message>(*id).ok().flatten())
            // Rules may have filed or read them already.
            .filter(|message| !message.flags.read)
            .collect()
    };
    match messages.as_slice() {
        [] => {}
        [message] => show(
            message.from.display(),
            &subject_line(message),
            Activation::Message(message.id),
            cx,
        ),
        [first, ..] => {
            let body: Vec<String> = messages.iter().take(3).map(subject_line).collect();
            show(
                &tr(cx, "{} new messages").replace("{}", &messages.len().to_string()),
                &body.join("\n"),
                Activation::Message(first.id),
                cx,
            )
        }
    }
}

fn subject_line(message: &Message) -> String {
    if message.subject.trim().is_empty() {
        "(no subject)".to_owned()
    } else {
        message.subject.clone()
    }
}

/// Desktop notification for a calendar reminder.
pub fn reminder(title: &str, start: DateTime<Utc>, cx: &App) {
    if !prefs(cx).notify_reminders {
        return;
    }
    let when = start
        .with_timezone(&Local)
        .format_in(prefs(cx).language, "%a %d %b, %H:%M");
    let title = if title.trim().is_empty() {
        tr(cx, "Reminder")
    } else {
        title
    };
    show(title, &when, Activation::App, cx);
}

#[cfg(windows)]
mod platform {
    use std::path::PathBuf;
    use std::sync::OnceLock;

    use tauri_winrt_notification::{IconCrop, Sound, Toast};

    /// AppUserModelID shown as the toast source (registered under HKCU on first use).
    const APP_ID: &str = "Mailbox.Desktop";

    /// Unpackaged apps need their AUMID registered (with a display name and icon) for toasts
    /// to show. Returns the icon file (a copy in the data dir: toasts take a file path), if
    /// written.
    fn register_app_id() -> Option<&'static PathBuf> {
        static ICON: OnceLock<Option<PathBuf>> = OnceLock::new();
        ICON.get_or_init(|| {
            let icon = crate::sync_bridge::data_dir().join("app-icon.png");
            let icon = std::fs::write(&icon, crate::titlebar::APP_ICON_PNG)
                .is_ok()
                .then_some(icon);
            let key = windows_registry::CURRENT_USER
                .create(format!(r"Software\Classes\AppUserModelId\{APP_ID}"));
            if let Ok(key) = key {
                let _ = key.set_string("DisplayName", "Mailbox");
                if let Some(icon) = &icon {
                    let _ = key.set_string("IconUri", icon.to_string_lossy().as_ref());
                }
            }
            icon
        })
        .as_ref()
    }

    pub fn show(title: &str, body: &str, sound: bool, on_click: impl Fn() + Send + 'static) {
        let icon = register_app_id();
        let mut toast = Toast::new(APP_ID);
        // Windows caches the registered icon (an entry made before it was set shows none):
        // the toast carries the logo itself.
        if let Some(icon) = icon {
            toast = toast.icon(icon, IconCrop::Square, "Mailbox");
        }
        let result = toast
            .title(title)
            .text1(body)
            .sound(sound.then_some(Sound::Default))
            .on_activated(move |_| {
                on_click();
                Ok(())
            })
            .show();
        if let Err(err) = result {
            crate::log::error(format_args!("notification failed: {err}"));
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use std::process::Command;

    /// Shows a system notification. Clicks are not reported (`on_click` unused).
    pub fn show(title: &str, body: &str, sound: bool, _on_click: impl Fn() + Send + 'static) {
        let mut command = command(title, body, sound);
        // Waited on a thread: no zombie process, no blocking the UI.
        std::thread::spawn(move || {
            if let Err(err) = command.status() {
                crate::log::error(format_args!("notification failed: {err}"));
            }
        });
    }

    /// The text as one argument: never read as an option (a sender named `-e ...` would
    /// otherwise add AppleScript to run).
    fn argument(text: &str) -> String {
        if text.starts_with('-') {
            format!(" {text}")
        } else {
            text.to_owned()
        }
    }

    #[cfg(target_os = "macos")]
    fn command(title: &str, body: &str, sound: bool) -> Command {
        let display = if sound {
            r#"display notification (item 2 of argv) with title (item 1 of argv) sound name "Glass""#
        } else {
            "display notification (item 2 of argv) with title (item 1 of argv)"
        };
        let mut command = Command::new("osascript");
        command
            .args(["-e", "on run argv", "-e", display, "-e", "end run"])
            .args([argument(title), argument(body)]);
        command
    }

    #[cfg(not(target_os = "macos"))]
    fn command(title: &str, body: &str, sound: bool) -> Command {
        let mut command = Command::new("notify-send");
        command.args(["--app-name=Mailbox", "--icon=mail-unread"]);
        if sound {
            command.arg("--hint=string:sound-name:message-new-email");
        }
        command.arg("--").args([argument(title), argument(body)]);
        command
    }
}
