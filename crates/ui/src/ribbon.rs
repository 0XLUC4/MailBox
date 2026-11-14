//! Ribbon: contextual actions for the active module, in the top bar.
//! Minimal version (New + Undo/Redo). Owner: ribbon agent adds per-module groups.

use gpui::*;

use crate::app_state::{AppState, Module};
use crate::components::{Button, IconButton, IconName, toolbar_separator};
use crate::theme::SPACE_1;

pub struct RibbonView {
    state: Entity<AppState>,
    _subscription: Subscription,
}

impl RibbonView {
    pub fn new(state: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&state, |_, _, cx| cx.notify());
        Self {
            state,
            _subscription: subscription,
        }
    }
}

impl Render for RibbonView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let (new_label, new_icon) = match state.module() {
            Module::Mail => ("New mail", IconName::SquarePen),
            Module::Calendar => ("New event", IconName::CalendarPlus),
            Module::People => ("New contact", IconName::UserPlus),
            Module::Tasks => ("New task", IconName::Plus),
            Module::Settings => ("New account", IconName::Plus),
        };
        let undo_label = state
            .history()
            .undo_label()
            .map(|label| format!("Undo {label}"));
        let redo_label = state
            .history()
            .redo_label()
            .map(|label| format!("Redo {label}"));

        let new_state = self.state.clone();
        let undo_state = self.state.clone();
        let redo_state = self.state.clone();
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(SPACE_1)
            .child(
                Button::new("ribbon-new")
                    .primary()
                    .icon(new_icon)
                    .label(new_label)
                    .on_click(move |_, _, cx| {
                        new_state.update(cx, |state, cx| state.notify_info("Coming soon", cx));
                    }),
            )
            .child(toolbar_separator(cx))
            .child(
                IconButton::new("ribbon-undo", IconName::Undo2)
                    .disabled(undo_label.is_none())
                    .tooltip(undo_label.unwrap_or_else(|| "Nothing to undo".into()))
                    .on_click(move |_, _, cx| undo_state.update(cx, |state, cx| state.undo(cx))),
            )
            .child(
                IconButton::new("ribbon-redo", IconName::Redo2)
                    .disabled(redo_label.is_none())
                    .tooltip(redo_label.unwrap_or_else(|| "Nothing to redo".into()))
                    .on_click(move |_, _, cx| redo_state.update(cx, |state, cx| state.redo(cx))),
            )
    }
}
