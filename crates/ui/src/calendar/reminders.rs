//! Reminders: a background timer checks due reminders every 20 s and pops a small
//! "Reminders" window (dismiss, snooze 5/10/15 min, open the event).
//! Dismissed/snoozed state is kept for the session only.

use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Local, Utc};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::Id;
use mailbox_core::calendar::{self as cal, Occurrence};

use super::editor::{EditorTarget, open_event_editor};
use super::layout;
use crate::app_state::AppState;
use crate::components::{Button, Icon, IconName};
use crate::theme::{
    RADIUS_LG, RADIUS_MD, SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_LG, TEXT_MD, TEXT_SM, TEXT_XS,
    hex, theme,
};

/// Reminders already due when the app starts are shown if they fired this recently.
const STARTUP_LOOKBACK_MINUTES: i64 = 30;

#[derive(Debug, Clone)]
pub struct ActiveReminder {
    key: (Id, i64),
    occurrence: Occurrence,
}

pub struct ReminderService {
    state: Entity<AppState>,
    last_check: DateTime<Utc>,
    active: Vec<ActiveReminder>,
    snoozed: Vec<(ActiveReminder, DateTime<Utc>)>,
    window: Option<AnyWindowHandle>,
    _task: Task<()>,
}

impl ReminderService {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let task = cx.spawn(async move |this, cx| {
            let mut delay = StdDuration::from_secs(2);
            loop {
                cx.background_executor().timer(delay).await;
                delay = StdDuration::from_secs(20);
                if this.update(cx, |service, cx| service.tick(cx)).is_err() {
                    break;
                }
            }
        });
        Self {
            state,
            last_check: Utc::now() - Duration::minutes(STARTUP_LOOKBACK_MINUTES),
            active: Vec::new(),
            snoozed: Vec::new(),
            window: None,
            _task: task,
        }
    }

    fn is_known(&self, key: (Id, i64)) -> bool {
        self.active.iter().any(|reminder| reminder.key == key)
            || self.snoozed.iter().any(|(reminder, _)| reminder.key == key)
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        let now = Utc::now();
        let due = cal::due_reminders(self.state.read(cx).store(), self.last_check, now)
            .unwrap_or_default();
        self.last_check = now;
        let mut changed = false;
        for reminder in due {
            let key = reminder.occurrence.key();
            if !self.is_known(key) {
                let event = &reminder.occurrence.event;
                crate::notifications::reminder(&event.title, event.start, cx);
                self.active.push(ActiveReminder {
                    key,
                    occurrence: reminder.occurrence,
                });
                changed = true;
            }
        }
        let (ready, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.snoozed)
            .into_iter()
            .partition(|(_, until)| *until <= now);
        self.snoozed = waiting;
        for (reminder, _) in ready {
            self.active.push(reminder);
            changed = true;
        }
        if changed {
            self.show(cx);
            cx.notify();
        } else if !self.active.is_empty() {
            // Keep "in 5 min" labels fresh.
            cx.notify();
        }
    }

    /// Brings the reminder window up (opening it when needed).
    fn show(&mut self, cx: &mut Context<Self>) {
        let service = cx.entity();
        let existing = self.window;
        cx.defer(move |cx| {
            if let Some(handle) = existing
                && handle
                    .update(cx, |_, window, _| window.activate_window())
                    .is_ok()
            {
                return;
            }
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(440.), px(360.)), cx)),
                window_min_size: Some(size(px(360.), px(220.))),
                focus: true,
                ..crate::titlebar::window_options("Reminders")
            };
            let for_window = service.clone();
            let opened = crate::titlebar::open_window(options, cx, move |window, cx| {
                cx.new(|cx| ReminderWindow::new(for_window, window, cx))
            });
            service.update(cx, |service, cx| match opened {
                Ok(handle) => service.window = Some(handle),
                Err(err) => service.state.update(cx, |state, cx| {
                    state.report_error(format!("Could not open reminders: {err}"), cx)
                }),
            });
        });
    }

    fn dismiss(&mut self, key: (Id, i64), cx: &mut Context<Self>) {
        self.active.retain(|reminder| reminder.key != key);
        cx.notify();
    }

    fn dismiss_all(&mut self, cx: &mut Context<Self>) {
        self.active.clear();
        cx.notify();
    }

    fn snooze(&mut self, key: (Id, i64), minutes: i64, cx: &mut Context<Self>) {
        if let Some(index) = self.active.iter().position(|reminder| reminder.key == key) {
            let reminder = self.active.remove(index);
            self.snoozed
                .push((reminder, Utc::now() + Duration::minutes(minutes)));
        }
        cx.notify();
    }
}

/// "in 10 min", "now", "started 5 min ago".
fn when_label(start: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let minutes = (start - now).num_minutes();
    let span = |m: i64| {
        if m >= 24 * 60 {
            format!("{} d", m / (24 * 60))
        } else if m >= 60 {
            format!("{} h {:02} min", m / 60, m % 60)
        } else {
            format!("{m} min")
        }
    };
    match minutes {
        m if m > 0 => format!("in {}", span(m)),
        0 => "now".into(),
        m => format!("started {} ago", span(-m)),
    }
}

pub struct ReminderWindow {
    service: Entity<ReminderService>,
    _subscription: Subscription,
}

impl ReminderWindow {
    fn new(service: Entity<ReminderService>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&service, |_, _, cx| cx.notify());
        Self {
            service,
            _subscription: subscription,
        }
    }

    /// Runs `f` on the service, then closes the window once nothing is left.
    fn act(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut ReminderService, &mut Context<ReminderService>),
    ) {
        let empty = self.service.update(cx, |service, cx| {
            f(service, cx);
            service.active.is_empty()
        });
        if empty {
            self.service.update(cx, |service, _| service.window = None);
            window.remove_window();
        }
    }
}

impl Render for ReminderWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let service = self.service.read(cx);
        let reminders = service.active.clone();
        let state = service.state.clone();
        let calendars = state.read(cx).store().list_calendars().unwrap_or_default();
        let now = Utc::now();

        let rows: Vec<AnyElement> = reminders
            .into_iter()
            .map(|reminder| {
                let key = reminder.key;
                let occurrence = reminder.occurrence.clone();
                let event = &reminder.occurrence.event;
                let color = calendars
                    .iter()
                    .find(|calendar| calendar.id == event.calendar_id)
                    .map_or(t.accent, |calendar| hex(calendar.color));
                let started = event.start <= now;
                let local_start = event.start.with_timezone(&Local);
                let state = state.clone();
                let snooze_button = |minutes: i64| {
                    Button::new(ElementId::Name(
                        format!("snooze-{minutes}-{}-{}", key.0, key.1).into(),
                    ))
                    .ghost()
                    .compact()
                    .label(format!("{minutes} min"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.act(window, cx, |service, cx| service.snooze(key, minutes, cx))
                    }))
                };
                div()
                    .flex()
                    .flex_col()
                    .gap(SPACE_2)
                    .p(SPACE_3)
                    .rounded(RADIUS_LG)
                    .border_1()
                    .border_color(t.border_subtle)
                    .bg(t.elevated)
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_start()
                            .gap(SPACE_3)
                            .child(
                                div()
                                    .flex()
                                    .flex_shrink_0()
                                    .items_center()
                                    .justify_center()
                                    .size(px(32.))
                                    .rounded(RADIUS_MD)
                                    .bg(color.alpha(0.15))
                                    .child(
                                        Icon::new(IconName::BellRing)
                                            .size(px(16.))
                                            .text_color(color),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .gap(px(2.))
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(TEXT_MD)
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(event.title.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_size(TEXT_SM)
                                            .text_color(if started {
                                                t.danger
                                            } else {
                                                t.text_muted
                                            })
                                            .child(format!(
                                                "{} · {}",
                                                when_label(event.start, now),
                                                if event.all_day {
                                                    local_start.format("%a %-d %b").to_string()
                                                } else {
                                                    format!(
                                                        "{} {}",
                                                        local_start.format("%a"),
                                                        layout::time_range_label(
                                                            &reminder.occurrence
                                                        )
                                                    )
                                                }
                                            )),
                                    )
                                    .when(!event.location.is_empty(), |el| {
                                        el.child(
                                            div()
                                                .flex()
                                                .flex_row()
                                                .items_center()
                                                .gap(SPACE_1)
                                                .text_size(TEXT_XS)
                                                .text_color(t.text_muted)
                                                .child(Icon::new(IconName::MapPin).size(px(11.)))
                                                .child(event.location.clone()),
                                        )
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(SPACE_1)
                            .child(
                                div()
                                    .text_size(TEXT_XS)
                                    .text_color(t.text_subtle)
                                    .child("Snooze"),
                            )
                            .child(snooze_button(5))
                            .child(snooze_button(10))
                            .child(snooze_button(15))
                            .child(div().flex_1())
                            .child(
                                Button::new(ElementId::Name(
                                    format!("open-{}-{}", key.0, key.1).into(),
                                ))
                                .compact()
                                .label("Open")
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        let target = if occurrence.is_recurring_instance() {
                                            EditorTarget::Occurrence {
                                                occurrence: Box::new(occurrence.clone()),
                                            }
                                        } else {
                                            EditorTarget::Event {
                                                id: occurrence.event.id,
                                            }
                                        };
                                        open_event_editor(state.clone(), target, cx);
                                        this.act(window, cx, |service, cx| {
                                            service.dismiss(key, cx)
                                        });
                                    },
                                )),
                            )
                            .child(
                                Button::new(ElementId::Name(
                                    format!("dismiss-{}-{}", key.0, key.1).into(),
                                ))
                                .primary()
                                .compact()
                                .label("Dismiss")
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.act(window, cx, |service, cx| service.dismiss(key, cx))
                                    },
                                )),
                            ),
                    )
                    .into_any_element()
            })
            .collect();
        let count = rows.len();

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(t.background)
            .text_color(t.text)
            .text_size(TEXT_MD)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(SPACE_2)
                    .px(SPACE_4)
                    .h(px(48.))
                    .border_b_1()
                    .border_color(t.border_subtle)
                    .child(Icon::new(IconName::Bell).size(px(16.)).text_color(t.accent))
                    .child(
                        div()
                            .flex_1()
                            .text_size(TEXT_LG)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(if count == 1 {
                                "1 reminder".to_owned()
                            } else {
                                format!("{count} reminders")
                            }),
                    )
                    .when(count > 1, |el| {
                        el.child(
                            Button::new("dismiss-all")
                                .ghost()
                                .compact()
                                .label("Dismiss all")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.act(window, cx, |service, cx| service.dismiss_all(cx))
                                })),
                        )
                    }),
            )
            .child(
                div()
                    .id("reminder-list")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .p(SPACE_3)
                    .gap(SPACE_2)
                    .children(rows),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::when_label;
    use chrono::{Duration, TimeZone as _, Utc};

    #[test]
    fn when_labels() {
        let now = Utc.with_ymd_and_hms(2026, 11, 16, 9, 0, 0).unwrap();
        assert_eq!(when_label(now + Duration::minutes(10), now), "in 10 min");
        assert_eq!(when_label(now, now), "now");
        assert_eq!(
            when_label(now - Duration::minutes(5), now),
            "started 5 min ago"
        );
        assert_eq!(
            when_label(now + Duration::minutes(90), now),
            "in 1 h 30 min"
        );
        assert_eq!(when_label(now + Duration::days(2), now), "in 2 d");
    }
}
