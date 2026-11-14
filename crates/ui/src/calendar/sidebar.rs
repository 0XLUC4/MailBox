//! Left pane: "New event", mini month navigator, calendar list, side-by-side toggle.

use std::collections::HashSet;

use chrono::{Datelike, Local, NaiveDate};
use gpui::prelude::FluentBuilder as _;
use gpui::*;

use super::layout::{self, ViewKind};
use super::{CalendarView, ViewData};
use crate::components::{Button, Icon, IconButton, IconName};
use crate::theme::{
    RADIUS_FULL, RADIUS_MD, RADIUS_SM, SPACE_0_5, SPACE_1, SPACE_2, SPACE_3, TEXT_MD, TEXT_SM,
    TEXT_XS, Theme, hex,
};

pub(super) const SIDEBAR_WIDTH: f32 = 240.;

impl CalendarView {
    pub(super) fn render_sidebar(
        &self,
        data: &ViewData,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        // Collected right away: the rows borrow `cx` only while being built.
        let calendar_groups: Vec<_> = data
            .accounts
            .iter()
            .filter_map(|account| {
                let calendars: Vec<_> = data
                    .calendars
                    .iter()
                    .filter(|calendar| calendar.account_id == account.id)
                    .cloned()
                    .collect();
                (!calendars.is_empty()).then(|| {
                    div()
                        .flex()
                        .flex_col()
                        .gap(SPACE_0_5)
                        .child(
                            div()
                                .px(SPACE_2)
                                .pt(SPACE_2)
                                .pb(SPACE_1)
                                .text_size(TEXT_XS)
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(t.text_subtle)
                                .truncate()
                                .child(account.email.to_uppercase()),
                        )
                        .children(calendars.into_iter().map(|calendar| {
                            let id = calendar.id;
                            let visible = !self.hidden.contains(&id);
                            let color = hex(calendar.color);
                            div()
                                .id(("cal-row", id as u64))
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(SPACE_2)
                                .h(px(28.))
                                .px(SPACE_2)
                                .rounded(RADIUS_MD)
                                .cursor_pointer()
                                .hover(|style| style.bg(t.hover))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if !this.hidden.remove(&id) {
                                        this.hidden.insert(id);
                                    }
                                    cx.notify();
                                }))
                                .child(
                                    div()
                                        .flex()
                                        .flex_shrink_0()
                                        .items_center()
                                        .justify_center()
                                        .size(px(14.))
                                        .rounded(px(3.))
                                        .border_1()
                                        .border_color(color)
                                        .when(visible, |el| {
                                            el.bg(color).child(
                                                Icon::new(IconName::Check)
                                                    .size(px(10.))
                                                    .text_color(t.on_accent),
                                            )
                                        }),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .truncate()
                                        .text_size(TEXT_MD)
                                        .text_color(if visible { t.text } else { t.text_muted })
                                        .child(calendar.name.clone()),
                                )
                        }))
                })
            })
            .collect();

        let side_by_side = self.side_by_side;
        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .bg(t.surface)
            .border_r_1()
            .border_color(t.border_subtle)
            .child(
                div().flex().flex_col().p(SPACE_3).child(
                    Button::new("cal-new")
                        .primary()
                        .icon(IconName::Plus)
                        .label("New event")
                        .on_click(cx.listener(|this, _, _, cx| this.new_event(cx))),
                ),
            )
            .child(div().px(SPACE_3).pb(SPACE_3).child(self.render_mini_month(
                self.sidebar_month,
                false,
                t,
                cx,
            )))
            .child(div().mx(SPACE_3).h(px(1.)).bg(t.border_subtle))
            .child(
                div()
                    .id("cal-list")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .px(SPACE_2)
                    .py(SPACE_2)
                    .gap(SPACE_1)
                    .children(calendar_groups),
            )
            .child(
                div()
                    .id("cal-side-by-side")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(SPACE_2)
                    .m(SPACE_2)
                    .px(SPACE_2)
                    .h(px(36.))
                    .rounded(RADIUS_MD)
                    .cursor_pointer()
                    .hover(|style| style.bg(t.hover))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.side_by_side = !this.side_by_side;
                        cx.notify();
                    }))
                    .child(
                        Icon::new(if side_by_side {
                            IconName::Columns2
                        } else {
                            IconName::Layers
                        })
                        .size(px(14.))
                        .text_color(t.text_muted),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(TEXT_SM)
                            .text_color(t.text_muted)
                            .child(if side_by_side {
                                "Side by side"
                            } else {
                                "Overlay"
                            }),
                    )
                    .child(switch(side_by_side, t)),
            )
    }

    /// Month navigator. `picker` = the toolbar popover (closes on pick).
    pub(super) fn render_mini_month(
        &self,
        month: NaiveDate,
        picker: bool,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let grid = layout::month_grid(month);
        let today = Local::now().date_naive();
        let highlighted: HashSet<NaiveDate> = match self.kind {
            ViewKind::Day | ViewKind::WorkWeek | ViewKind::Week => {
                layout::visible_days(self.kind, self.anchor)
                    .into_iter()
                    .collect()
            }
            _ => [self.anchor].into_iter().collect(),
        };
        let busy: HashSet<NaiveDate> = {
            let calendars = self
                .state
                .read(cx)
                .store()
                .list_calendars()
                .unwrap_or_default();
            let visible: Vec<_> = calendars
                .into_iter()
                .filter(|calendar| !self.hidden.contains(&calendar.id))
                .collect();
            self.occurrences_between(cx, &grid, &visible)
                .iter()
                .flat_map(|occurrence| {
                    let (first, last) = layout::day_span(occurrence);
                    first
                        .iter_days()
                        .take_while(move |day| *day <= last)
                        .take(42)
                })
                .collect()
        };
        let base = if picker { 1000 } else { 0 };
        let nav = move |forward: bool| {
            move |this: &mut CalendarView, cx: &mut Context<CalendarView>| {
                let step = if forward { 1 } else { -1 };
                if picker {
                    this.picker = this.picker.map(|m| layout::add_months(m, step));
                } else {
                    this.sidebar_month = layout::add_months(this.sidebar_month, step);
                }
                cx.notify();
            }
        };
        let prev = nav(false);
        let next = nav(true);

        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(28.))
            .child(
                div()
                    .flex_1()
                    .pl(SPACE_1)
                    .text_size(TEXT_MD)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(month.format("%B %Y").to_string()),
            )
            .child(
                IconButton::new(("mini-prev", base as u64), IconName::ChevronLeft)
                    .size(px(24.))
                    .icon_size(px(14.))
                    .on_click(cx.listener(move |this, _, _, cx| prev(this, cx))),
            )
            .child(
                IconButton::new(("mini-next", base as u64), IconName::ChevronRight)
                    .size(px(24.))
                    .icon_size(px(14.))
                    .on_click(cx.listener(move |this, _, _, cx| next(this, cx))),
            );

        let weekday_row =
            div()
                .flex()
                .flex_row()
                .children(["M", "T", "W", "T", "F", "S", "S"].map(|label| {
                    div()
                        .flex_1()
                        .h(px(22.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(TEXT_XS)
                        .text_color(t.text_subtle)
                        .child(label)
                }));

        let weeks = grid.chunks(7).enumerate().map(|(week_index, week)| {
            div()
                .flex()
                .flex_row()
                .children(week.iter().enumerate().map(|(day_index, &day)| {
                    let index = base + week_index * 7 + day_index;
                    let in_month = day.month() == month.month();
                    let is_today = day == today;
                    let in_range = highlighted.contains(&day);
                    let has_events = busy.contains(&day);
                    div().flex_1().flex().justify_center().child(
                        div()
                            .id(("mini-day", index as u64))
                            .relative()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(28.))
                            .rounded(RADIUS_FULL)
                            .text_size(TEXT_SM)
                            .cursor_pointer()
                            .when(in_range && !is_today, |el| {
                                el.bg(t.accent_subtle)
                                    .text_color(t.accent)
                                    .font_weight(FontWeight::SEMIBOLD)
                            })
                            .when(is_today, |el| {
                                el.bg(t.accent)
                                    .text_color(t.on_accent)
                                    .font_weight(FontWeight::SEMIBOLD)
                            })
                            .when(!in_range && !is_today, |el| {
                                el.text_color(if in_month { t.text } else { t.text_subtle })
                                    .hover(|style| style.bg(t.hover))
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if picker {
                                    this.picker = None;
                                }
                                this.go_to(day, cx);
                            }))
                            .child(day.day().to_string())
                            .when(has_events && !is_today, |el| {
                                el.child(
                                    div()
                                        .absolute()
                                        .bottom(px(3.))
                                        .left(px(12.))
                                        .size(px(4.))
                                        .rounded(RADIUS_SM)
                                        .bg(if in_range { t.accent } else { t.text_subtle }),
                                )
                            }),
                    )
                }))
        });

        div()
            .flex()
            .flex_col()
            .child(header)
            .child(weekday_row)
            .children(weeks.collect::<Vec<_>>())
            .when(picker, |el| {
                el.child(
                    div().flex().flex_row().justify_end().pt(SPACE_2).child(
                        Button::new("picker-today")
                            .ghost()
                            .compact()
                            .label("Today")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.picker = None;
                                this.today(cx);
                            })),
                    ),
                )
            })
    }
}

/// A small on/off switch (display only; the row handles the click).
pub(super) fn switch(on: bool, t: &Theme) -> Div {
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .w(px(28.))
        .h(px(16.))
        .px(px(2.))
        .rounded(RADIUS_FULL)
        .bg(if on { t.accent } else { t.active })
        .when(on, |el| el.justify_end())
        .child(
            div()
                .size(px(12.))
                .rounded(RADIUS_FULL)
                .bg(t.on_accent)
                .shadow_sm(),
        )
}
