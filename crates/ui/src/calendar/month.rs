//! Month view: 6 week rows; multi-day events as bars, timed events as dotted lines.
//! Drag an event to another day to move it (Ctrl to copy); double-click a day to create.

use crate::settings::prefs;
use chrono::{Datelike, Weekday};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::calendar::Occurrence;
use mailbox_core::dates::LocalizedFormat;

use super::layout::{self, ViewKind};
use super::time_grid::chip_body;
use super::{CalendarView, DRAG_OPACITY, DragKind, HitArea, ViewData, chip_style};
use crate::settings::tr;
use crate::theme::{RADIUS_FULL, RADIUS_SM, SPACE_1, SPACE_2, TEXT_SM, TEXT_XS, Theme, text_on};

const MAX_LANES: usize = 3;
const LANE_HEIGHT: f32 = 20.;
const DATE_ROW: f32 = 28.;

impl CalendarView {
    pub(super) fn render_month(
        &self,
        data: &ViewData,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let weekday_header = div()
            .flex()
            .flex_row()
            .flex_shrink_0()
            .border_b_1()
            .border_color(t.border_subtle)
            .children(data.days.iter().take(7).map(|day| {
                div()
                    .flex_1()
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .px(SPACE_2)
                    .text_size(TEXT_XS)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.text_subtle)
                    .child(day.format_in(prefs(cx).language, "%a").to_uppercase())
            }));
        let weeks: Vec<AnyElement> = data
            .days
            .chunks(7)
            .enumerate()
            .map(|(week, days)| {
                self.render_week_row(week, days, data, t, cx)
                    .into_any_element()
            })
            .collect();

        div()
            .flex()
            .flex_col()
            .size_full()
            .on_mouse_move(
                cx.listener(|this, event: &MouseMoveEvent, _, cx| this.on_mouse_move(event, cx)),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, window, cx| this.on_mouse_up(window, cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, window, cx| this.on_mouse_up(window, cx)),
            )
            .child(weekday_header)
            .children(weeks)
    }

    fn render_week_row(
        &self,
        week: usize,
        days: &[chrono::NaiveDate],
        data: &ViewData,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let Some(&first_day) = days.first() else {
            return div();
        };
        let count = days.len();
        let month = self.anchor.month();
        let drop_day = self
            .drag
            .as_ref()
            .filter(|drag| drag.moved && drag.kind == DragKind::Move)
            .map(|drag| drag.current.0);

        let mut items: Vec<(usize, usize, &Occurrence)> = data
            .occurrences
            .iter()
            .filter_map(|occurrence| {
                let (first, last) = layout::day_span(occurrence);
                let first = (first - first_day).num_days();
                let last = (last - first_day).num_days();
                (last >= 0 && first < count as i64).then(|| {
                    (
                        first.max(0) as usize,
                        (last as usize).min(count - 1),
                        occurrence,
                    )
                })
            })
            .collect();
        items.sort_by_key(|(first, last, occurrence)| {
            (
                *first,
                usize::MAX - (*last - *first),
                !layout::is_all_day_like(occurrence),
                occurrence.event.start,
            )
        });
        let lanes = layout::pack_lanes(
            &items
                .iter()
                .map(|(first, last, _)| (*first, *last))
                .collect::<Vec<_>>(),
        );
        let mut hidden = vec![0usize; count];
        for ((first, last, _), lane) in items.iter().zip(&lanes) {
            if *lane >= MAX_LANES {
                for slot in hidden.iter_mut().take(*last + 1).skip(*first) {
                    *slot += 1;
                }
            }
        }

        let cells: Vec<AnyElement> = days
            .iter()
            .enumerate()
            .map(|(index, &day)| {
                let is_today = day == data.today;
                let in_month = day.month() == month;
                let weekend = matches!(day.weekday(), Weekday::Sat | Weekday::Sun);
                let is_anchor = day == self.anchor;
                let is_drop = drop_day == Some(day);
                let hits = self.hits.clone();
                let label = if day.day() == 1 {
                    day.format_in(prefs(cx).language, "%-d %b")
                } else {
                    day.day().to_string()
                };
                div()
                    .id(("month-cell", (week * 7 + index) as u64))
                    .relative()
                    .flex_1()
                    .min_w(px(0.))
                    .h_full()
                    .border_l_1()
                    .border_color(t.border_subtle)
                    .when(weekend || !in_month, |el| el.bg(t.surface))
                    .when(is_drop, |el| el.bg(t.accent_subtle))
                    .child(
                        canvas(
                            move |bounds, _, _| {
                                hits.borrow_mut().push(HitArea {
                                    day,
                                    calendar: None,
                                    bounds,
                                    timed: false,
                                });
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, window, cx| {
                            this.on_empty_mouse_down(event, window, cx);
                        }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .h(px(DATE_ROW))
                            .items_center()
                            .px(SPACE_1)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .min_w(px(22.))
                                    .h(px(22.))
                                    .px(SPACE_1)
                                    .rounded(RADIUS_FULL)
                                    .text_size(TEXT_SM)
                                    .when(is_today, |el| {
                                        el.bg(t.accent)
                                            .text_color(t.on_accent)
                                            .font_weight(FontWeight::SEMIBOLD)
                                    })
                                    .when(!is_today, |el| {
                                        el.text_color(if in_month { t.text } else { t.text_subtle })
                                            .when(is_anchor, |el| {
                                                el.font_weight(FontWeight::BOLD)
                                                    .text_color(t.accent)
                                            })
                                    })
                                    .child(label),
                            ),
                    )
                    .when(hidden[index] > 0, |el| {
                        let more = hidden[index];
                        el.child(
                            div()
                                .id(("month-more", (week * 7 + index) as u64))
                                .absolute()
                                .left(px(4.))
                                .top(px(DATE_ROW + MAX_LANES as f32 * LANE_HEIGHT))
                                .px(SPACE_1)
                                .rounded(RADIUS_SM)
                                .text_size(TEXT_XS)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(t.text_muted)
                                .cursor_pointer()
                                .hover(|style| style.bg(t.hover).text_color(t.text))
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.go_to(day, cx);
                                    this.set_kind(ViewKind::Day, cx);
                                }))
                                .child(tr(cx, "+{} more").replace("{}", &more.to_string())),
                        )
                    })
                    .into_any_element()
            })
            .collect();

        let bars: Vec<AnyElement> = items
            .iter()
            .zip(&lanes)
            .filter(|(_, lane)| **lane < MAX_LANES)
            .map(|((first, last, occurrence), lane)| {
                self.render_month_item(
                    week,
                    occurrence,
                    *first as f32 / count as f32,
                    (*last - *first + 1) as f32 / count as f32,
                    DATE_ROW + *lane as f32 * LANE_HEIGHT,
                    data,
                    t,
                    cx,
                )
                .into_any_element()
            })
            .collect();

        div()
            .relative()
            .flex()
            .flex_row()
            .flex_1()
            .min_h(px(0.))
            .overflow_hidden()
            .border_b_1()
            .border_color(t.border_subtle)
            .children(cells)
            .children(bars)
    }

    #[allow(clippy::too_many_arguments)]
    fn render_month_item(
        &self,
        week: usize,
        occurrence: &Occurrence,
        left: f32,
        width: f32,
        top: f32,
        data: &ViewData,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let selected = self.selected == Some(occurrence.key());
        let color = data.color(occurrence.event.calendar_id);
        let style = chip_style(
            color,
            t,
            selected,
            data.my_status(occurrence),
            occurrence.details.show_as,
        );
        let dragging = self.drag.as_ref().is_some_and(|drag| {
            drag.moved
                && !drag.copy
                && drag.occurrence.as_ref().map(Occurrence::key) == Some(occurrence.key())
        });
        let (id, start) = occurrence.key();
        let target = occurrence.clone();
        let filled = layout::is_all_day_like(occurrence);
        let time = layout::local_span(occurrence).0.format("%H:%M").to_string();
        let on_color = text_on(color);

        div()
            .id(ElementId::Name(
                format!("month-item-{week}-{id}-{start}").into(),
            ))
            .absolute()
            .left(relative(left))
            .w(relative(width))
            .top(px(top))
            .h(px(LANE_HEIGHT - 2.))
            .px(px(3.))
            .cursor_pointer()
            .when(dragging, |el| el.opacity(DRAG_OPACITY))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.on_event_mouse_down(&target, DragKind::Move, event, window, cx)
                }),
            )
            .when(filled, |el| {
                el.child(chip_body(occurrence, &style, true, cx))
            })
            .when(!filled, |el| {
                el.child(
                    div()
                        .size_full()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(SPACE_1)
                        .px(SPACE_1)
                        .rounded(RADIUS_SM)
                        .text_size(TEXT_XS)
                        .when(selected, |el| el.bg(color).text_color(on_color))
                        .when(!selected, |el| el.hover(|style| style.bg(t.hover)))
                        .child(
                            div()
                                .flex_shrink_0()
                                .size(px(7.))
                                .rounded(RADIUS_FULL)
                                .bg(if selected { on_color } else { color }),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_color(if selected { on_color } else { t.text_muted })
                                .child(time),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .truncate()
                                .font_weight(FontWeight::MEDIUM)
                                .when(style.strike, |el| el.line_through())
                                .child(occurrence.event.title.clone()),
                        ),
                )
            })
    }
}
