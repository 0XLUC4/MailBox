//! macOS menu bar and the standard app and window shortcuts (Cmd+Q, Cmd+H, Cmd+W, Cmd+M).
//!
//! Without them a macOS app cannot be quit from the keyboard or the menu bar, and the Edit menu
//! (Cut, Copy, Paste, Select All) that macOS users expect is missing. Text fields bind their own
//! Cmd+C/V/X/A/Z; the Edit menu dispatches those same input actions. Windows and Linux have no
//! menu bar: their windows close from the caption buttons.

use gpui::component::input;
use gpui::{App, KeyBinding, Menu, MenuItem, OsAction, SystemMenuType, Window};

use crate::GoToSettings;
use crate::actions::OpenAbout;

gpui::actions!(
    mailbox,
    [Quit, Hide, HideOthers, ShowAll, CloseWindow, Minimize, Zoom]
);

/// Installs the menu bar and its shortcuts (macOS only).
pub fn init(cx: &mut App) {
    if !cfg!(target_os = "macos") {
        return;
    }
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &CloseWindow, cx| with_active_window(cx, |window| window.remove_window()));
    cx.on_action(|_: &Minimize, cx| with_active_window(cx, |window| window.minimize_window()));
    cx.on_action(|_: &Zoom, cx| with_active_window(cx, |window| window.zoom_window()));
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
        KeyBinding::new("cmd-m", Minimize, None),
    ]);
    cx.set_menus([
        // macOS names the first menu after the app itself.
        Menu::new("Mailbox").items([
            MenuItem::action("About Mailbox", OpenAbout),
            MenuItem::separator(),
            MenuItem::action("Settings…", GoToSettings),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Mailbox", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit Mailbox", Quit),
        ]),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", input::Undo, OsAction::Undo),
            MenuItem::os_action("Redo", input::Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", input::Cut, OsAction::Cut),
            MenuItem::os_action("Copy", input::Copy, OsAction::Copy),
            MenuItem::os_action("Paste", input::Paste, OsAction::Paste),
            MenuItem::os_action("Select All", input::SelectAll, OsAction::SelectAll),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
            MenuItem::separator(),
            MenuItem::action("Close Window", CloseWindow),
        ]),
    ]);
}

fn with_active_window(cx: &mut App, run: fn(&mut Window)) {
    if let Some(window) = cx.active_window() {
        window.update(cx, |_, window, _| run(window)).ok();
    }
}
