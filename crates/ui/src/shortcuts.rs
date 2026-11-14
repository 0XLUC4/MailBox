//! Keyboard shortcuts help: every catalog shortcut (from `actions::ACTIONS`) plus the
//! context-specific keys of the message list, compose and calendar.

use gpui::component::WindowExt as _;
use gpui::*;

use crate::dialogs::{kbd, shortcut_rows};
use crate::settings::tr;
use crate::theme::{SPACE_1, SPACE_3, TEXT_SM, TEXT_XS, theme};

/// `(section, [(keys, description)])`; `mod` is Ctrl (Cmd on macOS).
const CONTEXT_KEYS: &[(&str, &[(&str, &str)])] = &[
    (
        "Message list",
        &[
            ("Up / Down", "Previous / next message"),
            ("Shift+Up / Shift+Down", "Extend the selection"),
            ("mod+A", "Select all"),
            ("Enter", "Open in a new window"),
            ("Right / Left", "Expand / collapse a conversation"),
            ("Escape", "Clear the selection"),
            ("Shift+Delete", "Delete permanently"),
            ("Backspace", "Archive"),
            ("Double-click", "Open in a new window"),
        ],
    ),
    (
        "Compose",
        &[
            ("mod+Enter", "Send"),
            ("mod+B", "Bold"),
            ("mod+I", "Italic"),
            ("mod+U", "Underline"),
            ("mod+K", "Insert link"),
        ],
    ),
    (
        "Calendar",
        &[
            ("Left / Right", "Previous / next day"),
            ("Up / Down", "Previous / next week"),
            ("Page Up / Page Down", "Previous / next period"),
            ("T", "Today"),
            (
                "D / X / W / M / A",
                "Day / work week / week / month / agenda",
            ),
            ("N", "New event"),
            ("Enter", "Open the selected event"),
            ("Delete", "Delete the selected event"),
            ("Escape", "Close the editor or prompt"),
            ("mod+S", "Save in the event editor"),
        ],
    ),
    (
        "Everywhere",
        &[
            ("mod+shift+P", "Command palette"),
            ("mod+shift+Z", "Redo"),
            ("Escape", "Close a dialog"),
        ],
    ),
];

fn display(keys: &str) -> String {
    if cfg!(target_os = "macos") {
        keys.replace("mod+", "⌘").replace("shift+", "⇧")
    } else {
        keys.replace("mod+", "Ctrl+").replace("shift+", "Shift+")
    }
}

fn section(title: &str, cx: &App) -> Div {
    div()
        .pt(SPACE_3)
        .pb(SPACE_1)
        .text_size(TEXT_XS)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme(cx).text_subtle)
        .child(title.to_uppercase())
}

fn context_rows(cx: &App) -> Vec<AnyElement> {
    let mut rows = Vec::new();
    for (title, keys) in CONTEXT_KEYS {
        rows.push(section(title, cx).into_any_element());
        for (keys, description) in keys.iter() {
            rows.push(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(SPACE_3)
                    .py(SPACE_1)
                    .child(div().flex_1().text_size(TEXT_SM).child(*description))
                    .child(kbd(&display(keys), cx))
                    .into_any_element(),
            );
        }
    }
    rows
}

/// Opens the shortcuts help.
pub fn open(window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, |dialog, _, cx| {
        dialog
            .w(px(500.))
            .title(tr(cx, "Keyboard shortcuts"))
            .child(
                div()
                    .id("shortcuts-all")
                    .max_h(px(520.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .child(section("Commands", cx))
                    .children(shortcut_rows(cx))
                    .children(context_rows(cx)),
            )
    });
}

#[cfg(test)]
mod tests {
    use super::display;

    #[test]
    fn shows_platform_modifiers() {
        if !cfg!(target_os = "macos") {
            assert_eq!(display("mod+shift+P"), "Ctrl+Shift+P");
        }
    }
}
