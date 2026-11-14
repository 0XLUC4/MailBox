//! Settings module. Appearance (system/light/dark + accent) works; the rest is to come.
//! Owner: settings agent (accounts, rules, categories, notifications...).

use gpui::prelude::FluentBuilder as _;
use gpui::*;

use crate::app_state::AppState;
use crate::components::Button;
use crate::theme::{
    ACCENTS, Appearance, RADIUS_FULL, SPACE_2, SPACE_3, SPACE_6, TEXT_LG, TEXT_MD, TEXT_XL, accent,
    appearance, hex, set_accent, set_appearance, theme,
};

pub struct SettingsView {
    _state: Entity<AppState>,
}

impl SettingsView {
    pub fn new(state: Entity<AppState>, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self { _state: state }
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let current = appearance(cx);
        let current_accent = accent(cx);

        let appearance_buttons = [
            ("System", Appearance::System),
            ("Light", Appearance::Light),
            ("Dark", Appearance::Dark),
        ]
        .map(|(label, value)| {
            Button::new(("appearance", value as u64))
                .label(label)
                .when(value == current, |button| button.primary())
                .on_click(move |_, window, cx| set_appearance(value, window, cx))
        });

        let swatches = ACCENTS.iter().map(|&(name, color)| {
            let selected = color == current_accent;
            div()
                .id(name)
                .size(px(24.))
                .rounded(RADIUS_FULL)
                .bg(hex(color))
                .cursor_pointer()
                .border_2()
                .border_color(if selected { t.text } else { t.background })
                .on_click(move |_, window, cx| set_accent(color, window, cx))
        });

        div()
            .id("settings")
            .size_full()
            .overflow_y_scroll()
            .p(SPACE_6)
            .flex()
            .flex_col()
            .gap(SPACE_3)
            .child(
                div()
                    .text_size(TEXT_XL)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Settings"),
            )
            .child(
                div()
                    .mt(SPACE_3)
                    .text_size(TEXT_LG)
                    .font_weight(FontWeight::MEDIUM)
                    .child("Appearance"),
            )
            .child(
                div()
                    .text_size(TEXT_MD)
                    .text_color(t.text_muted)
                    .child("Theme"),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(SPACE_2)
                    .children(appearance_buttons),
            )
            .child(
                div()
                    .text_size(TEXT_MD)
                    .text_color(t.text_muted)
                    .child("Accent colour"),
            )
            .child(div().flex().flex_row().gap(SPACE_2).children(swatches))
    }
}
