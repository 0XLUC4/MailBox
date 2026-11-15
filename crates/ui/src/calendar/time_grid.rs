//! Day / Work week / Week: day headers, all-day lanes and the scrollable hour grid.

use crate::settings::prefs;
use chrono::{Datelike, Local, NaiveDate, Timelike, Weekday};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::calendar::Occurrence;
use mailbox_core::dates::LocalizedFormat;
use mailbox_core::{Calendar, Id};

use super::layout::{self, Placement, ViewKind};
use super::{
    CalendarView, ChipStyle, DRAG_OPACITY, DragKind, HOUR_HEIGHT, HitArea, ViewData, chip_style,
};
use crate::components::{Icon, IconName};
use crate::settings::tr;
use crate::theme::{
    ICON_SIZE_XS, RADIUS_FULL, RADIUS_MD, RADIUS_SM, SPACE_0_5, SPACE_1, SPACE_1_5, SPACE_2,
    TEXT_2XS, TEXT_SM, TEXT_XL, TEXT_XS, Theme, hex, text_on,
};

const GUTTER: f32 = 56.;
const ALL_DAY_LANE: f32 = 22.;
const MIN_EVENT_HEIGHT: f32 = 20.;

/// `+01:00` → `UTC+1`, `-03:30` → `UTC-3:30` (fits the narrow time gutter).
fn short_offset(offset: &str) -> String {
    let (sign, rest) = offset.split_at(offset.len().min(1));
    let (hours, minutes) = rest.split_once(':').unwrap_or((rest, "00"));
    let hours = hours.trim_start_matches('0');
    let hours = if hours.is_empty() { "0" } else { hours };
    if minutes == "00" {
        format!("UTC{sign}{hours}")
    } else {
        format!("UTC{sign}{hours}:{minutes}")
    }
}

fn minutes_to_px(minutes: i64) -> f32 {
    minutes as f32 / 60. * HOUR_HEIGHT
}

/// Element id of an occurrence rendered in a given column.
fn occurrence_id(prefix: &str, column: usize, occurrence: &Occurrence) -> ElementId {
    let (id, start) = occurrence.key();
    ElementId::Name(format!("{prefix}-{column}-{id}-{start}").into())
}

impl CalendarView {
    pub(super) fn render_time_grid(
        &self,
        data: &ViewData,
        t: &Theme,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let lanes: Vec<Option<&Calendar>> = if self.side_by_side && data.visible.len() > 1 {
            data.visible.iter().map(Some).collect()
        } else {
            vec![None]
        };
        let columns: Vec<(NaiveDate, Option<Id>)> = data
            .days
            .iter()
            .flat_map(|day| lanes.iter().map(move |lane| (*day, lane.map(|c| c.id))))
            .collect();

        let header = self.render_day_headers(data, &lanes, t, cx);
        let all_day = self.render_all_day_row(data, t, cx);
        let body_columns: Vec<AnyElement> = columns
            .iter()
            .enumerate()
            .map(|(index, (day, lane))| {
                self.render_day_column(index, *day, *lane, data, t, cx)
                    .into_any_element()
            })
            .collect();

        let now_minutes = i64::from(data.now.hour() * 60 + data.now.minute());
        let show_now = data.days.contains(&data.today);
        let gutter = div()
            .relative()
            .flex_shrink_0()
            .w(px(GUTTER))
            .h_full()
            .children((1..24).map(|hour| {
                div()
                    .absolute()
                    .top(px(hour as f32 * HOUR_HEIGHT - 7.))
                    .right(px(8.))
                    .text_size(TEXT_XS)
                    .text_color(t.text_subtle)
                    .child(layout::format_minutes(hour * 60))
            }))
            .when(show_now, |el| {
                el.child(
                    div()
                        .absolute()
                        .top(px(minutes_to_px(now_minutes) - 8.))
                        .right(px(4.))
                        .px(SPACE_1)
                        .rounded(RADIUS_SM)
                        .bg(t.danger)
                        .text_color(text_on(t.danger))
                        .text_size(TEXT_XS)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(layout::format_minutes(now_minutes)),
                )
            });

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
            .child(header)
            .child(all_day)
            .child(
                div()
                    .id("time-grid-scroll")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .w_full()
                            .h(px(HOUR_HEIGHT * 24.))
                            .child(gutter)
                            .children(body_columns),
                    ),
            )
    }

    fn render_day_headers(
        &self,
        data: &ViewData,
        lanes: &[Option<&Calendar>],
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let offset = short_offset(&Local::now().format("%:z").to_string());
        let show_lanes = lanes.len() > 1;
        let days: Vec<AnyElement> = data
            .days
            .iter()
            .enumerate()
            .map(|(index, &day)| {
                let is_today = day == data.today;
                let weekend = matches!(day.weekday(), Weekday::Sat | Weekday::Sun);
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.))
                    .border_l_1()
                    .border_color(t.border_subtle)
                    .child(
                        div()
                            .id(("day-header", index as u64))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(SPACE_2)
                            .h(px(48.))
                            .px(SPACE_2)
                            .cursor_pointer()
                            .hover(|style| style.bg(t.hover))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.go_to(day, cx);
                                this.set_kind(ViewKind::Day, cx);
                            }))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .min_w(px(32.))
                                    .h(px(32.))
                                    .px(SPACE_1)
                                    .rounded(RADIUS_FULL)
                                    .text_size(TEXT_XL)
                                    .font_weight(if is_today {
                                        FontWeight::SEMIBOLD
                                    } else {
                                        FontWeight::NORMAL
                                    })
                                    .when(is_today, |el| el.bg(t.accent).text_color(t.on_accent))
                                    .when(!is_today, |el| {
                                        el.text_color(if weekend { t.text_muted } else { t.text })
                                    })
                                    .child(day.day().to_string()),
                            )
                            .child(
                                div()
                                    .text_size(TEXT_XS)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(if is_today { t.accent } else { t.text_subtle })
                                    .child(day.format_in(prefs(cx).language, "%a").to_uppercase()),
                            ),
                    )
                    .when(show_lanes, |el| {
                        el.child(div().flex().flex_row().children(lanes.iter().map(|lane| {
                            let (name, color) = lane
                                .map(|c| (c.name.clone(), hex(c.color)))
                                .unwrap_or_else(|| (String::new(), t.accent));
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(SPACE_1)
                                .px(SPACE_1)
                                .pb(SPACE_1)
                                .child(div().size(px(6.)).rounded(RADIUS_FULL).bg(color))
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(TEXT_XS)
                                        .text_color(t.text_muted)
                                        .child(name),
                                )
                        })))
                    })
                    .into_any_element()
            })
            .collect();

        div()
            .flex()
            .flex_row()
            .flex_shrink_0()
            .border_b_1()
            .border_color(t.border_subtle)
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_end()
                    .justify_end()
                    .w(px(GUTTER))
                    .pr(SPACE_2)
                    .pb(SPACE_1)
                    .text_size(TEXT_2XS)
                    .text_color(t.text_subtle)
                    .child(offset),
            )
            .children(days)
    }

    fn render_all_day_row(
        &self,
        data: &ViewData,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let Some(&first_day) = data.days.first() else {
            return div();
        };
        let count = data.days.len();
        let mut items: Vec<(usize, usize, &Occurrence)> = data
            .occurrences
            .iter()
            .filter(|occurrence| layout::is_all_day_like(occurrence))
            .filter_map(|occurrence| {
                let (first, last) = layout::day_span(occurrence);
                let first = (first - first_day).num_days().max(0) as usize;
                let last = (last - first_day).num_days();
                (last >= 0 && first < count)
                    .then(|| (first, (last as usize).min(count - 1), occurrence))
            })
            .collect();
        items.sort_by_key(|(first, last, occurrence)| {
            (
                *first,
                usize::MAX - (*last - *first),
                occurrence.event.start,
            )
        });
        let lanes = layout::pack_lanes(
            &items
                .iter()
                .map(|(first, last, _)| (*first, *last))
                .collect::<Vec<_>>(),
        );
        let quick_all_day = self.quick.as_ref().filter(|quick| quick.all_day);
        let lane_count =
            lanes.iter().max().map_or(0, |max| max + 1) + usize::from(quick_all_day.is_some());
        let height = (lane_count.max(1) as f32) * ALL_DAY_LANE + 6.;

        let cells: Vec<AnyElement> = data
            .days
            .iter()
            .enumerate()
            .map(|(index, &day)| {
                div()
                    .id(("all-day-cell", index as u64))
                    .flex_1()
                    .h_full()
                    .border_l_1()
                    .border_color(t.border_subtle)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.commit_pending_quick(cx);
                            this.focus.focus(window, cx);
                            if event.click_count >= 2 {
                                this.start_quick_create(day, 0, 0, true, None, window, cx);
                            }
                        }),
                    )
                    .into_any_element()
            })
            .collect();

        let bars: Vec<AnyElement> = items
            .iter()
            .zip(&lanes)
            .map(|((first, last, occurrence), lane)| {
                let span = last - first + 1;
                self.render_all_day_bar(
                    occurrence,
                    *first as f32 / count as f32,
                    span as f32 / count as f32,
                    *lane as f32 * ALL_DAY_LANE + 3.,
                    data,
                    t,
                    cx,
                )
                .into_any_element()
            })
            .collect();

        let quick = quick_all_day.and_then(|quick| {
            let index = (quick.day - first_day).num_days();
            (0..count as i64).contains(&index).then(|| {
                div()
                    .absolute()
                    .left(relative(index as f32 / count as f32))
                    .w(relative(1. / count as f32))
                    .top(px((lane_count - 1) as f32 * ALL_DAY_LANE + 3.))
                    .h(px(ALL_DAY_LANE - 2.))
                    .px(px(2.))
                    .child(
                        div()
                            .size_full()
                            .px(SPACE_1)
                            .rounded(RADIUS_SM)
                            .border_1()
                            .border_color(t.accent)
                            .bg(t.elevated)
                            .shadow_md()
                            .child(self.render_quick_input(quick, t, cx)),
                    )
            })
        });

        div()
            .flex()
            .flex_row()
            .flex_shrink_0()
            .border_b_1()
            .border_color(t.border)
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .justify_end()
                    .w(px(GUTTER))
                    .pr(SPACE_2)
                    .text_size(TEXT_2XS)
                    .text_color(t.text_subtle)
                    .child(tr(cx, "All day")),
            )
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .h(px(height))
                    .children(cells)
                    .children(bars)
                    .when_some(quick, |el, quick| el.child(quick)),
            )
    }

    #[allow(clippy::too_many_arguments)]
    fn render_all_day_bar(
        &self,
        occurrence: &Occurrence,
        left: f32,
        width: f32,
        top: f32,
        data: &ViewData,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let selected = self.selected == Some(occurrence.key());
        let style = chip_style(
            data.color(occurrence.event.calendar_id),
            t,
            selected,
            data.my_status(occurrence),
            occurrence.details.show_as,
        );
        let dragging = self.drag.as_ref().is_some_and(|drag| {
            drag.moved && drag.occurrence.as_ref().map(Occurrence::key) == Some(occurrence.key())
        });
        let target = occurrence.clone();
        div()
            .id(occurrence_id("all-day", 0, occurrence))
            .absolute()
            .left(relative(left))
            .w(relative(width))
            .top(px(top))
            .h(px(ALL_DAY_LANE - 2.))
            .px(px(2.))
            .cursor_pointer()
            .when(dragging, |el| el.opacity(DRAG_OPACITY))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.commit_pending_quick(cx);
                    this.on_event_mouse_down(&target, DragKind::Move, event, window, cx);
                    // All-day items move by whole days.
                    if let Some(drag) = this.drag.as_mut() {
                        drag.timed = false;
                    }
                }),
            )
            .child(chip_body(occurrence, &style, true, cx))
    }

    fn render_day_column(
        &self,
        index: usize,
        day: NaiveDate,
        lane: Option<Id>,
        data: &ViewData,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let weekend = matches!(day.weekday(), Weekday::Sat | Weekday::Sun);
        let work_start = i64::from(layout::WORK_START_HOUR) * 60;
        let work_end = i64::from(layout::WORK_END_HOUR) * 60;
        let in_lane = |occurrence: &Occurrence| {
            lane.is_none_or(|calendar| occurrence.event.calendar_id == calendar)
        };

        // Timed occurrences of this column, laid out side by side.
        let mut items: Vec<(&Occurrence, (i64, i64))> = data
            .occurrences
            .iter()
            .filter(|occurrence| !layout::is_all_day_like(occurrence) && in_lane(occurrence))
            .filter_map(|occurrence| Some((occurrence, layout::minutes_on(occurrence, day)?)))
            .collect();
        items.sort_by_key(|(occurrence, (from, to))| (*from, -(to - from), occurrence.event.id));
        let min_len = (MIN_EVENT_HEIGHT / HOUR_HEIGHT * 60.).ceil() as i64;
        let placements = layout::layout_columns(
            &items.iter().map(|(_, span)| *span).collect::<Vec<_>>(),
            min_len,
        );
        let blocks: Vec<AnyElement> = items
            .iter()
            .zip(placements)
            .map(|((occurrence, span), placement)| {
                self.render_timed_block(index, occurrence, *span, placement, data, t, cx)
                    .into_any_element()
            })
            .collect();

        // Drag previews.
        let drag = self.drag.as_ref();
        let move_ghost = drag
            .filter(|drag| drag.moved && drag.kind != DragKind::Create)
            .and_then(|drag| drag.ghost())
            .filter(|ghost| !layout::is_all_day_like(ghost) && in_lane(ghost))
            .and_then(|ghost| {
                let span = layout::minutes_on(&ghost, day)?;
                Some((ghost, span))
            });
        let create_ghost = drag
            .filter(|drag| {
                drag.kind == DragKind::Create && drag.origin.0 == day && drag.calendar == lane
            })
            .map(|drag| drag.create_range());
        let quick = self.quick.as_ref().filter(|quick| {
            !quick.all_day && quick.day == day && lane.is_none_or(|id| id == quick.calendar_id)
        });

        let hits = self.hits.clone();
        let now_top = (day == data.today)
            .then(|| minutes_to_px(i64::from(data.now.hour() * 60 + data.now.minute())));

        div()
            .id(("day-column", index as u64))
            .relative()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .border_l_1()
            .border_color(t.border_subtle)
            .bg(if weekend { t.surface } else { t.background })
            .child(
                canvas(
                    move |bounds, _, _| {
                        hits.borrow_mut().push(HitArea {
                            day,
                            calendar: lane,
                            bounds,
                            timed: true,
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .when(!weekend, |el| {
                el.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .w_full()
                        .h(px(minutes_to_px(work_start)))
                        .bg(t.surface),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(minutes_to_px(work_end)))
                        .left_0()
                        .w_full()
                        .h(px(minutes_to_px(24 * 60 - work_end)))
                        .bg(t.surface),
                )
            })
            .children((1..24).map(|hour| {
                div()
                    .absolute()
                    .top(px(hour as f32 * HOUR_HEIGHT))
                    .left_0()
                    .w_full()
                    .h(px(1.))
                    .bg(t.border_subtle)
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.commit_pending_quick(cx);
                    this.on_empty_mouse_down(event, window, cx);
                }),
            )
            .children(blocks)
            .when_some(create_ghost, |el, (_, from, to)| {
                el.child(
                    div()
                        .absolute()
                        .top(px(minutes_to_px(from)))
                        .h(px(minutes_to_px(to - from).max(MIN_EVENT_HEIGHT)))
                        .left(px(2.))
                        .right(px(4.))
                        .px(SPACE_2)
                        .py(SPACE_0_5)
                        .rounded(RADIUS_MD)
                        .border_1()
                        .border_color(t.accent)
                        .bg(t.accent_subtle)
                        .text_size(TEXT_XS)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.accent)
                        .child(format!(
                            "{} – {}",
                            layout::format_minutes(from),
                            layout::format_minutes(to)
                        )),
                )
            })
            .when_some(move_ghost, |el, (ghost, (from, to))| {
                let color = data.color(ghost.event.calendar_id);
                el.child(
                    div()
                        .absolute()
                        .top(px(minutes_to_px(from)))
                        .h(px(minutes_to_px(to - from).max(MIN_EVENT_HEIGHT)))
                        .left(px(2.))
                        .right(px(4.))
                        .px(SPACE_2)
                        .py(SPACE_0_5)
                        .rounded(RADIUS_MD)
                        .border_1()
                        .border_color(color)
                        .bg(color.alpha(0.85))
                        .shadow_lg()
                        .text_color(text_on(color))
                        .text_size(TEXT_XS)
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .truncate()
                                .child(ghost.event.title.clone()),
                        )
                        .child(div().child(format!(
                            "{}{}",
                            layout::time_range_label(&ghost, prefs(cx).language),
                            if self.drag.as_ref().is_some_and(|drag| drag.copy) {
                                format!("  {}", tr(cx, "(copy)"))
                            } else {
                                String::new()
                            }
                        ))),
                )
            })
            .when_some(quick, |el, quick| {
                el.child(
                    div()
                        .absolute()
                        .top(px(minutes_to_px(quick.start)))
                        .h(px(minutes_to_px(quick.end - quick.start).max(52.)))
                        .left(px(2.))
                        .right(px(4.))
                        .px(SPACE_2)
                        .py(SPACE_1)
                        .flex()
                        .flex_col()
                        .rounded(RADIUS_MD)
                        .border_1()
                        .border_color(t.accent)
                        .bg(t.elevated)
                        .shadow_lg()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(
                            div()
                                .truncate()
                                .text_size(TEXT_XS)
                                .text_color(t.accent)
                                .font_weight(FontWeight::MEDIUM)
                                .child(format!(
                                    "{} – {}  -  {}",
                                    layout::format_minutes(quick.start),
                                    layout::format_minutes(quick.end),
                                    tr(cx, "Enter to save, Esc to cancel")
                                )),
                        )
                        .child(self.render_quick_input(quick, t, cx)),
                )
            })
            .when_some(now_top, |el, top| {
                el.child(
                    div()
                        .absolute()
                        .top(px(top - 1.))
                        .left_0()
                        .w_full()
                        .h(px(2.))
                        .bg(t.danger),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(top - 4.5))
                        .left(px(-4.5))
                        .size(px(9.))
                        .rounded(RADIUS_FULL)
                        .bg(t.danger),
                )
            })
    }

    #[allow(clippy::too_many_arguments)]
    fn render_timed_block(
        &self,
        column: usize,
        occurrence: &Occurrence,
        (from, to): (i64, i64),
        placement: Placement,
        data: &ViewData,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let selected = self.selected == Some(occurrence.key());
        let style = chip_style(
            data.color(occurrence.event.calendar_id),
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
        let top = minutes_to_px(from);
        let height = minutes_to_px(to - from).max(MIN_EVENT_HEIGHT);
        let columns = placement.columns.max(1) as f32;
        let compact = height < 40.;
        let (down, resize_end, resize_start) =
            (occurrence.clone(), occurrence.clone(), occurrence.clone());

        div()
            .id(occurrence_id("timed", column, occurrence))
            .absolute()
            .top(px(top))
            .h(px(height))
            .left(relative(placement.column as f32 / columns))
            .w(relative(placement.span as f32 / columns))
            .pl(px(1.))
            .pr(px(3.))
            .pb(px(1.))
            .cursor_pointer()
            .when(dragging, |el| el.opacity(DRAG_OPACITY))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.commit_pending_quick(cx);
                    this.on_event_mouse_down(&down, DragKind::Move, event, window, cx)
                }),
            )
            .child(
                div()
                    .size_full()
                    .when(selected, |el| el.shadow_md())
                    .child(chip_body(occurrence, &style, compact, cx)),
            )
            .child(
                div()
                    .id(occurrence_id("resize-start", column, occurrence))
                    .absolute()
                    .top_0()
                    .left_0()
                    .w_full()
                    .h(px(4.))
                    .cursor_ns_resize()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.on_event_mouse_down(
                                &resize_start,
                                DragKind::ResizeStart,
                                event,
                                window,
                                cx,
                            )
                        }),
                    ),
            )
            .child(
                div()
                    .id(occurrence_id("resize-end", column, occurrence))
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .w_full()
                    .h(px(6.))
                    .cursor_ns_resize()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.on_event_mouse_down(
                                &resize_end,
                                DragKind::ResizeEnd,
                                event,
                                window,
                                cx,
                            )
                        }),
                    ),
            )
    }

    /// Saves a quick-create block that has text (clicking elsewhere keeps what was typed).
    pub(super) fn commit_pending_quick(&mut self, cx: &mut Context<Self>) {
        let Some(quick) = self.quick.as_ref() else {
            return;
        };
        let title = quick.input.read(cx).value().trim().to_owned();
        self.commit_quick_create(title, cx);
    }
}

/// The coloured body of an event: left bar, title, time and small status icons.
pub(super) fn chip_body(
    occurrence: &Occurrence,
    style: &ChipStyle,
    compact: bool,
    cx: &App,
) -> impl IntoElement + use<> {
    let event = &occurrence.event;
    let title = if event.title.trim().is_empty() {
        tr(cx, "(No title)").to_owned()
    } else {
        event.title.clone()
    };
    let time = layout::time_range_label(occurrence, prefs(cx).language);
    let icons = [
        (occurrence.is_part_of_series(), IconName::Repeat),
        (occurrence.details.private, IconName::Lock),
        (!event.attendees.is_empty(), IconName::Users),
    ];
    let icon_row = || {
        div()
            .flex()
            .flex_row()
            .flex_shrink_0()
            .gap(SPACE_0_5)
            .children(
                icons
                    .into_iter()
                    .filter(|(on, _)| *on)
                    .map(|(_, icon)| Icon::new(icon).size(ICON_SIZE_XS).text_color(style.muted)),
            )
    };

    div()
        .size_full()
        .flex()
        .flex_row()
        .overflow_hidden()
        .rounded(RADIUS_SM)
        .bg(style.bg)
        .when_some(style.border, |el, border| {
            el.border_1().border_color(border)
        })
        .child(div().flex_shrink_0().w(px(3.)).h_full().bg(style.bar))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(0.))
                .px(SPACE_1_5)
                .py(SPACE_0_5)
                .text_size(TEXT_XS)
                .line_height(px(15.))
                .text_color(style.fg)
                .when(compact, |el| {
                    el.child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(SPACE_1)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .truncate()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .when(style.strike, |el| el.line_through())
                                    .child(if event.all_day {
                                        title.clone()
                                    } else {
                                        format!(
                                            "{title}  {}",
                                            time.split(' ').next().unwrap_or_default()
                                        )
                                    }),
                            )
                            .child(icon_row()),
                    )
                })
                .when(!compact, |el| {
                    el.child(
                        div()
                            .flex()
                            .flex_row()
                            .items_start()
                            .gap(SPACE_1)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_size(TEXT_SM)
                                    .line_height(px(16.))
                                    .when(style.strike, |el| el.line_through())
                                    .child(title),
                            )
                            .child(div().pt(px(3.)).child(icon_row())),
                    )
                    .child(div().truncate().text_color(style.muted).child(time))
                    .when(!event.location.is_empty(), |el| {
                        el.child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(SPACE_1)
                                .text_color(style.muted)
                                .child(Icon::new(IconName::MapPin).size(ICON_SIZE_XS))
                                .child(div().truncate().child(event.location.clone())),
                        )
                    })
                }),
        )
}

#[cfg(test)]
mod tests {
    use super::short_offset;

    #[test]
    fn short_offsets() {
        assert_eq!(short_offset("+01:00"), "UTC+1");
        assert_eq!(short_offset("-03:30"), "UTC-3:30");
        assert_eq!(short_offset("+00:00"), "UTC+0");
        assert_eq!(short_offset("+10:00"), "UTC+10");
    }
}
