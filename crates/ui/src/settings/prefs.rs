//! In-memory copy of the stored [`Settings`], readable by every view without a store query:
//! `prefs(cx).reading_pane`, `prefs(cx).density`...
//!
//! The store is the source of truth; [`super::SettingsView`] re-syncs this global (and the theme)
//! whenever the store changes, including on undo. Change settings with [`update_settings`].

use std::sync::LazyLock;

use gpui::{App, Entity, Global, Window, px};
use mailbox_core::settings::{Settings, ThemePreference};

use crate::app_state::AppState;
use crate::theme::{self, Appearance};

struct GlobalPrefs(Settings);

impl Global for GlobalPrefs {}

static DEFAULTS: LazyLock<Settings> = LazyLock::new(Settings::default);

/// Current preferences (defaults until the settings view has loaded them).
pub fn prefs(cx: &App) -> &Settings {
    cx.try_global::<GlobalPrefs>()
        .map_or(&*DEFAULTS, |global| &global.0)
}

/// Whether the stored settings have been loaded into the global yet.
pub fn loaded(cx: &App) -> bool {
    cx.has_global::<GlobalPrefs>()
}

fn appearance(theme: ThemePreference) -> Appearance {
    match theme {
        ThemePreference::System => Appearance::System,
        ThemePreference::Light => Appearance::Light,
        ThemePreference::Dark => Appearance::Dark,
    }
}

/// Installs `settings` as the current preferences and applies the visual ones.
pub fn apply(settings: &Settings, window: &mut Window, cx: &mut App) {
    let previous = prefs(cx).clone();
    let first = !loaded(cx);
    cx.set_global(GlobalPrefs(settings.clone()));
    if first || previous.theme != settings.theme || previous.accent != settings.accent {
        theme::set_appearance(appearance(settings.theme), window, cx);
        theme::set_accent(settings.accent, window, cx);
    }
    if first || previous.language != settings.language {
        crate::app_menu::set_menus(cx);
    }
    // Scales rem-based widgets (inputs, dialogs, menus); 16px rem at the 13px default size.
    let rem = f32::from(settings.font_size) * 16. / 13.;
    if window.rem_size() != px(rem) {
        window.set_rem_size(px(rem));
    }
    cx.refresh_windows();
}

/// Loads the stored settings, applies `change` and saves them through an undoable command.
pub fn update_settings(state: &Entity<AppState>, cx: &mut App, change: impl FnOnce(&mut Settings)) {
    state.update(cx, |state, cx| {
        let mut settings = match Settings::load(state.store()) {
            Ok(settings) => settings,
            Err(err) => return state.report_error(err, cx),
        };
        change(&mut settings);
        match settings.command() {
            Ok(command) => {
                state.run(command, cx);
            }
            Err(err) => state.report_error(err, cx),
        }
    });
}
