//! Mailbox desktop UI (GPUI via gpui-kit). Entry point: [`run`].
//!
//! See ARCHITECTURE.md for the data flow and which agent owns which module.

pub mod app_state;
pub mod calendar;
pub mod command_palette;
pub mod components;
pub mod compose;
pub mod dialogs;
pub mod mail;
pub mod people;
pub mod ribbon;
pub mod search;
pub mod settings;
pub mod shell;
pub mod sync_bridge;
pub mod tasks;
pub mod theme;

// App-wide actions (ribbon, command palette, shortcuts).
pub mod actions;

use gpui::{
    App, AppContext as _, KeyBinding, TitlebarOptions, WindowBounds, WindowOptions, px, size,
};
use mailbox_core::Store;

use crate::app_state::AppState;
use crate::shell::Shell;

gpui::actions!(
    mailbox,
    [
        Undo,
        Redo,
        GoToMail,
        GoToCalendar,
        GoToPeople,
        GoToTasks,
        GoToSettings,
        SendReceive,
        ToggleWorkOffline
    ]
);

/// Opens the main window and runs the event loop until the window is closed.
pub fn run(store: Store) {
    gpui::application()
        .with_assets(gpui::assets::AllAssets)
        .run(move |cx: &mut App| {
            gpui::init(cx);
            bind_keys(cx);
            actions::bind_keys(cx);

            let state = cx.new(|_| AppState::new(store));
            sync_bridge::init(state.clone(), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1360.), px(860.)), cx)),
                window_min_size: Some(size(px(900.), px(560.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Mailbox".into()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let opened = gpui::open_window(options, cx, |window, cx| {
                theme::init(window, cx);
                cx.new(|cx| Shell::new(state, window, cx))
            });
            let main_window = match opened {
                Ok((handle, _)) => handle.window_id(),
                Err(err) => {
                    eprintln!("mailbox: failed to open window: {err:#}");
                    cx.quit();
                    return;
                }
            };
            // Secondary windows (popped-out compose) close without quitting.
            cx.on_window_closed(move |cx, closed| {
                if closed == main_window {
                    cx.quit();
                }
            })
            .detach();
            cx.activate(true);
        });
}

/// Platform primary modifier + key, as a `&'static str` (KeyBinding keeps the borrow).
macro_rules! primary {
    ($key:literal) => {
        if cfg!(target_os = "macos") { concat!("cmd-", $key) } else { concat!("ctrl-", $key) }
    };
}

fn bind_keys(cx: &mut App) {
    let context = Some("Shell");
    cx.bind_keys([
        KeyBinding::new(primary!("z"), Undo, context),
        KeyBinding::new(primary!("shift-z"), Redo, context),
        KeyBinding::new(primary!("y"), Redo, context),
        KeyBinding::new(primary!("1"), GoToMail, context),
        KeyBinding::new(primary!("2"), GoToCalendar, context),
        KeyBinding::new(primary!("3"), GoToPeople, context),
        KeyBinding::new(primary!("4"), GoToTasks, context),
        KeyBinding::new(primary!(","), GoToSettings, context),
        KeyBinding::new("f9", SendReceive, context),
    ]);
}
