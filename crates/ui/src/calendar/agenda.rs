//! Agenda: the next 30 days as a list grouped by day.

use chrono::{Datelike, NaiveDate};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::ResponseStatus;
use mailbox_core::calendar::Occurrence;

use super::layout;
use super::{CalendarView, ViewData};
use crate::components::{Badge, EmptyState, Icon, IconName};
use crate::theme::{
    RADIUS_FULL, RADIUS_MD, SPACE_0_5, SPACE_1, SPACE_2, SPACE_3, SPACE_4, SPACE_6, TEXT_MD,
    TEXT_SM, TEXT_XL, TEXT_XS, Theme,
};

impl CalendarView {
    pub(super) fn render_agenda(
        &self,
        data: &ViewData,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let groups: Vec<(NaiveDate, Vec<&Occurrence>)> = data
            .days
            .iter()
            .filter_map(|&day| {
                let items: Vec<&Occurrence> = data
                    .occurrences
                    .iter()
                    .filter(|occurrence| {
                        let (first, last) = layout::day_span(occurrence);
                        first <= day && day <= last
                    })
                    .collect();
                (!items.is_empty()).then_some((day, items))
            })
            .collect();

        if groups.is_empty() {
            let last = data.days.last().copied().unwrap_or(self.anchor);
            return div().id("agenda").size_full().child(
                EmptyState::new(IconName::CalendarCheck, "Nothing planned").description(format!(
                    "No events between {} and {}. Press N to create one.",
                    self.anchor.format("%-d %b"),
                    last.format("%-d %b")
                )),
            );
        }

        let sections: Vec<AnyElement> = groups
            .into_iter()
            .map(|(day, items)| {
                let is_today = day == data.today;
                let rows: Vec<AnyElement> = items
                    .into_iter()
                    .map(|occurrence| {
                        self.render_agenda_row(day, occurrence, data, t, cx)
                            .into_any_element()
                    })
                    .collect();
                div()
                    .flex()
                    .flex_row()
                    .gap(SPACE_4)
                    .py(SPACE_3)
                    .border_b_1()
                    .border_color(t.border_subtle)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_shrink_0()
                            .w(px(88.))
                            .gap(SPACE_0_5)
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(SPACE_2)
                                    .child(
                                        div()
                                            .text_size(TEXT_XL)
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(if is_today { t.accent } else { t.text })
                                            .child(day.day().to_string()),
                                    )
                                    .child(
                                        div()
                                            .text_size(TEXT_XS)
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(t.text_subtle)
                                            .child(day.format("%a").to_string().to_uppercase()),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(TEXT_XS)
                                    .text_color(if is_today { t.accent } else { t.text_muted })
                                    .child(if (day - data.today).num_days().abs() <= 1 {
                                        layout::relative_day(day, data.today)
                                    } else {
                                        day.format("%B").to_string()
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.))
                            .gap(SPACE_1)
                            .children(rows),
                    )
                    .into_any_element()
            })
            .collect();

        div()
            .id("agenda")
            .size_full()
            .overflow_y_scroll()
            .px(SPACE_6)
            .py(SPACE_2)
            .children(sections)
    }

    fn render_agenda_row(
        &self,
        day: NaiveDate,
        occurrence: &Occurrence,
        data: &ViewData,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let event = &occurrence.event;
        let color = data.color(event.calendar_id);
        let selected = self.selected == Some(occurrence.key());
        let status = data.my_status(occurrence);
        let time = if layout::is_all_day_like(occurrence) {
            let (first, last) = layout::day_span(occurrence);
            if first == last {
                "All day".to_owned()
            } else {
                format!(
                    "Day {} of {}",
                    (day - first).num_days() + 1,
                    (last - first).num_days() + 1
                )
            }
        } else {
            layout::time_range_label(occurrence)
        };
        let (id, ts) = occurrence.key();
        let down = occurrence.clone();
        let calendar_name = data
            .calendars
            .iter()
            .find(|calendar| calendar.id == event.calendar_id)
            .map(|calendar| calendar.name.clone())
            .unwrap_or_default();

        div()
            .id(ElementId::Name(
                format!("agenda-{}-{id}-{ts}", day.num_days_from_ce()).into(),
            ))
            .flex()
            .flex_row()
            .items_center()
            .gap(SPACE_3)
            .px(SPACE_3)
            .py(SPACE_2)
            .rounded(RADIUS_MD)
            .cursor_pointer()
            .when(selected, |el| el.bg(t.accent_subtle))
            .when(!selected, |el| el.hover(|style| style.bg(t.hover)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.focus.focus(window, cx);
                    this.selected = Some(down.key());
                    if event.click_count >= 2 {
                        this.open_occurrence(down.clone(), cx);
                    }
                    cx.notify();
                }),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(110.))
                    .text_size(TEXT_SM)
                    .text_color(t.text_muted)
                    .child(time),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(4.))
                    .h(px(32.))
                    .rounded(RADIUS_FULL)
                    .bg(color),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(SPACE_2)
                            .child(
                                div()
                                    .truncate()
                                    .text_size(TEXT_MD)
                                    .font_weight(FontWeight::MEDIUM)
                                    .when(status == ResponseStatus::Declined, |el| {
                                        el.line_through().text_color(t.text_subtle)
                                    })
                                    .child(if event.title.trim().is_empty() {
                                        "(No title)".to_owned()
                                    } else {
                                        event.title.clone()
                                    }),
                            )
                            .when(occurrence.is_part_of_series(), |el| {
                                el.child(
                                    Icon::new(IconName::Repeat)
                                        .size(px(12.))
                                        .text_color(t.text_subtle),
                                )
                            })
                            .when(occurrence.details.private, |el| {
                                el.child(
                                    Icon::new(IconName::Lock)
                                        .size(px(12.))
                                        .text_color(t.text_subtle),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(SPACE_2)
                            .text_size(TEXT_XS)
                            .text_color(t.text_muted)
                            .child(calendar_name)
                            .when(!event.location.is_empty(), |el| {
                                el.child("·").child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap(px(3.))
                                        .child(Icon::new(IconName::MapPin).size(px(11.)))
                                        .child(div().truncate().child(event.location.clone())),
                                )
                            })
                            .when(!event.attendees.is_empty(), |el| {
                                el.child("·").child(format!(
                                    "{} {}",
                                    event.attendees.len(),
                                    if event.attendees.len() == 1 {
                                        "guest"
                                    } else {
                                        "guests"
                                    }
                                ))
                            }),
                    ),
            )
            .when(status == ResponseStatus::NeedsAction, |el| {
                el.child(Badge::new("Invitation").color(t.warning))
            })
            .when(status == ResponseStatus::Tentative, |el| {
                el.child(Badge::new("Tentative").color(t.text_muted))
            })
    }
}
