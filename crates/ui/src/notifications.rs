//! Desktop notifications and the unread count of the main window.
//!
//! - New mail (called by the sync bridge with the ids that just arrived in an inbox): muted
//!   conversations are archived silently, automatic replies are queued, then one Windows toast
//!   shows the sender and subject (`Settings::notify_new_mail`, `notify_sound`).
//! - Reminders: [`reminder`] (`Settings::notify_reminders`).
//! - Clicking a toast brings the main window forward (and opens the message).
//! - The window title (shown on the taskbar button) carries the inbox unread count. A real
//!   overlay badge needs `ITaskbarList3` (unsafe COM), which this workspace forbids.
//!
//! Other platforms: only the in-app toasts (the bridge already shows one) and the title.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use gpui::{AnyWindowHandle, App, Entity, Global};
use mailbox_core::{FolderRole, Id, Message};

use crate::app_state::{AppState, Module};
use crate::settings::prefs;

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
                &format!("{} new messages", messages.len()),
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
        .format("%a %d %b, %H:%M")
        .to_string();
    let title = if title.trim().is_empty() {
        "Reminder"
    } else {
        title
    };
    show(title, &when, Activation::App, cx);
}

#[cfg(windows)]
mod platform {
    use std::sync::Once;

    use tauri_winrt_notification::{Sound, Toast};

    /// AppUserModelID shown as the toast source (registered under HKCU on first use).
    const APP_ID: &str = "Mailbox.Desktop";

    /// Unpackaged apps need their AUMID registered (with a display name and icon) for toasts
    /// to show.
    fn register_app_id() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            let key = windows_registry::CURRENT_USER
                .create(format!(r"Software\Classes\AppUserModelId\{APP_ID}"));
            if let Ok(key) = key {
                let _ = key.set_string("DisplayName", "Mailbox");
                // Toasts show the app icon from a file path: keep a copy in the data dir.
                let icon = crate::sync_bridge::data_dir().join("app-icon.png");
                if std::fs::write(&icon, crate::titlebar::APP_ICON_PNG).is_ok() {
                    let _ = key.set_string("IconUri", icon.to_string_lossy().as_ref());
                }
            }
        });
    }

    pub fn show(title: &str, body: &str, sound: bool, on_click: impl Fn() + Send + 'static) {
        register_app_id();
        let result = Toast::new(APP_ID)
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
    pub fn show(_title: &str, _body: &str, _sound: bool, _on_click: impl Fn() + Send + 'static) {}
}
