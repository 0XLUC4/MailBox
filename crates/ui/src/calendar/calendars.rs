//! Calendar list management: the default local calendar, create / rename / recolour /
//! delete a calendar (modal card opened from the sidebar).

use gpui::component::input::{Input, InputEvent, InputState};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::calendar::{CalendarCommand, LOCAL_ACCOUNT};
use mailbox_core::{Calendar, CalendarEvent, Command, Id, command};

use super::CalendarView;
use crate::app_state::AppState;
use crate::components::{Button, ButtonVariant, Icon, IconName};
use crate::theme::{ACCENTS, SPACE_2, SPACE_3, SPACE_6, TEXT_LG, TEXT_SM, Theme, hex};

/// The calendar being created (`id: None`) or edited.
pub(super) struct CalendarEdit {
    id: Option<Id>,
    name: Entity<InputState>,
    color: u32,
    _subscription: Subscription,
}

/// A first run (no account) still gets a "Calendar" kept on this computer.
pub(super) fn ensure_default_calendar(state: &Entity<AppState>, cx: &mut App) {
    let store = state.read(cx).store();
    if !store
        .list_calendars()
        .is_ok_and(|calendars| calendars.is_empty())
    {
        return;
    }
    let calendar = Calendar {
        id: 0,
        account_id: LOCAL_ACCOUNT,
        name: "Calendar".into(),
        color: ACCENTS[0].1,
    };
    if let Err(err) = command::apply(store, Command::SaveCalendar(Box::new(calendar))) {
        state.update(cx, |state, cx| state.report_error(err, cx));
    }
}

impl CalendarView {
    /// Opens the card for a new calendar (`None`) or an existing one.
    pub(super) fn edit_calendar(
        &mut self,
        id: Option<Id>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let calendars = self
            .state
            .read(cx)
            .store()
            .list_calendars()
            .unwrap_or_default();
        let existing = id.and_then(|id| calendars.iter().find(|c| c.id == id));
        let name = existing.map_or_else(String::new, |c| c.name.clone());
        let color = existing.map_or(ACCENTS[calendars.len() % ACCENTS.len()].1, |c| c.color);
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Calendar name")
                .default_value(name)
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        let subscription = cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.save_calendar_edit(cx);
            }
        });
        self.calendar_edit = Some(CalendarEdit {
            id,
            name: input,
            color,
            _subscription: subscription,
        });
        cx.notify();
    }

    fn save_calendar_edit(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.calendar_edit.as_ref() else {
            return;
        };
        let name = edit.name.read(cx).value().trim().to_owned();
        if name.is_empty() {
            return;
        }
        let store = self.state.read(cx).store();
        let calendar = match edit.id {
            Some(id) => match store.get::<Calendar>(id).ok().flatten() {
                Some(calendar) => Calendar {
                    name,
                    color: edit.color,
                    ..calendar
                },
                None => return,
            },
            None => Calendar {
                id: 0,
                account_id: LOCAL_ACCOUNT,
                name,
                color: edit.color,
            },
        };
        let ok = self.state.update(cx, |state, cx| {
            state.run(Command::SaveCalendar(Box::new(calendar)), cx)
        });
        if ok {
            self.calendar_edit = None;
            cx.notify();
        }
    }

    /// Deletes a calendar and its events in one undoable step.
    fn delete_calendar(&mut self, id: Id, cx: &mut Context<Self>) {
        // Deleting locally would delete every event on the server at the next sync.
        if crate::dav_bridge::is_synced_calendar(id, cx) {
            self.state.update(cx, |state, cx| {
                state.report_error(
                    "This calendar is synced with a CalDAV server: delete it on the server.",
                    cx,
                )
            });
            return;
        }
        let events: Vec<Id> = self
            .state
            .read(cx)
            .store()
            .query::<CalendarEvent>("WHERE calendar_id = ?1", [id])
            .unwrap_or_default()
            .iter()
            .map(|event| event.id)
            .collect();
        let command = Command::Batch(vec![
            CalendarCommand::Restore {
                put: Vec::new(),
                remove: events,
            }
            .into_command(),
            Command::DeleteCalendar(id),
        ]);
        if self.state.update(cx, |state, cx| state.run(command, cx)) {
            self.hidden.remove(&id);
            self.calendar_edit = None;
            cx.notify();
        }
    }

    pub(super) fn render_calendar_edit(
        &self,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let edit = self.calendar_edit.as_ref()?;
        let id = edit.id;
        // Keep at least one calendar so new events always have a home.
        let deletable = id.is_some()
            && self
                .state
                .read(cx)
                .store()
                .list_calendars()
                .map_or(0, |c| c.len())
                > 1
            && !id.is_some_and(|id| crate::dav_bridge::is_synced_calendar(id, cx));
        let swatches = ACCENTS.iter().map(|&(label, color)| {
            let active = color == edit.color;
            div()
                .id(SharedString::from(format!("cal-color-{label}")))
                .flex()
                .items_center()
                .justify_center()
                .size(px(24.))
                .rounded_full()
                .bg(hex(color))
                .cursor_pointer()
                .when(active, |el| {
                    el.border_2().border_color(t.text).child(
                        Icon::new(IconName::Check)
                            .size(px(12.))
                            .text_color(t.on_accent),
                    )
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(edit) = this.calendar_edit.as_mut() {
                        edit.color = color;
                        cx.notify();
                    }
                }))
        });
        Some(
            div()
                .id("cal-edit-backdrop")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(hsla(0., 0., 0., if t.is_dark { 0.45 } else { 0.25 }))
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.calendar_edit = None;
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .id("cal-edit-card")
                        .w(px(360.))
                        .p(SPACE_6)
                        .flex()
                        .flex_col()
                        .gap(SPACE_3)
                        .rounded(px(12.))
                        .border_1()
                        .border_color(t.border)
                        .bg(t.elevated)
                        .text_color(t.text)
                        .shadow_lg()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(
                            div()
                                .text_size(TEXT_LG)
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(if id.is_some() {
                                    "Edit calendar"
                                } else {
                                    "New calendar"
                                }),
                        )
                        .child(Input::new(&edit.name))
                        .child(
                            div()
                                .text_size(TEXT_SM)
                                .text_color(t.text_muted)
                                .child("Colour"),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .flex_wrap()
                                .gap(SPACE_2)
                                .children(swatches),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap(SPACE_2)
                                .pt(SPACE_2)
                                .when_some(id.filter(|_| deletable), |el, id| {
                                    el.child(
                                        Button::new("cal-edit-delete")
                                            .variant(ButtonVariant::Danger)
                                            .label("Delete")
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.delete_calendar(id, cx)
                                            })),
                                    )
                                })
                                .child(div().flex_1())
                                .child(
                                    Button::new("cal-edit-cancel")
                                        .ghost()
                                        .label("Cancel")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.calendar_edit = None;
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("cal-edit-save")
                                        .primary()
                                        .label("Save")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.save_calendar_edit(cx)
                                        })),
                                ),
                        ),
                ),
        )
    }
}
