//! Task detail side panel (title, steps, My Day, reminder, due date, repeat, list, linked
//! email, notes) and the panel for a flagged message that has no task yet.

use crate::settings::prefs;
use chrono::{DateTime, Datelike, Days, Local, NaiveDate, Timelike, Utc};
use gpui::component::date_picker::{DatePicker, DatePickerEvent, DatePickerState};
use gpui::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui::component::time_field::TimePrecision;
use gpui::component::tooltip::Tooltip;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::dates::LocalizedFormat;
use mailbox_core::todo::{self, Repeat, Step, TaskCard, TaskList};
use mailbox_core::{Command, Id, Message, TaskCommand};

use super::{Selection, TasksView, check_circle, day_text, option_chip, reminder_label};
use crate::app_state::Module;
use crate::components::{Banner, Button, Icon, IconButton, IconName};
use crate::settings::tr;
use crate::theme::{
    ICON_SIZE, ICON_SIZE_SM, ICON_SIZE_XS, PANE_HEADER_HEIGHT, RADIUS_FULL, RADIUS_LG, RADIUS_MD,
    SPACE_1, SPACE_1_5, SPACE_2, SPACE_3, SPACE_4, TEXT_LG, TEXT_MD, TEXT_SM, TEXT_XS, Theme,
};

/// Inputs of the detail panel for one task; edits are committed on Enter / blur.
pub(super) struct TaskDetail {
    id: Id,
    title: Entity<InputState>,
    notes: Entity<TextareaState>,
    new_step: Entity<InputState>,
    due: Entity<DatePickerState>,
    remind: Entity<DatePickerState>,
    _subscriptions: Vec<Subscription>,
}

impl TaskDetail {
    pub(super) fn new(card: &TaskCard, window: &mut Window, cx: &mut Context<TasksView>) -> Self {
        let id = card.id();
        let title_text = card.task.title.clone();
        let notes_text = card.task.notes.clone();
        let due_value = card.task.due;
        let remind_value = card
            .details
            .reminder
            .map(|at| at.with_timezone(&Local).naive_local());
        let title = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(tr(cx, "Title"))
                .default_value(title_text)
        });
        let notes = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(tr(cx, "Add a note"))
                .default_value(notes_text)
        });
        let new_step = cx.new(|cx| InputState::new(window, cx).placeholder(tr(cx, "Add step")));
        let due = cx.new(|cx| {
            let mut picker = DatePickerState::new(window, cx)
                .date_format("%a %e %b %Y")
                .first_day_of_week(chrono::Weekday::Mon);
            if let Some(date) = due_value {
                picker.set_date(date, window, cx);
            }
            picker
        });
        let remind = cx.new(|cx| {
            let mut picker = DatePickerState::new(window, cx)
                .date_format("%a %e %b %Y, %H:%M")
                .time_precision(TimePrecision::Minute)
                .default_time(chrono::NaiveTime::from_hms_opt(9, 0, 0).unwrap_or_default())
                .first_day_of_week(chrono::Weekday::Mon);
            if let Some(at) = remind_value {
                picker.set_date_time(at, window, cx);
            }
            picker
        });

        let subscriptions = vec![
            cx.subscribe_in(
                &title,
                window,
                move |this, input, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                        let text = input.read(cx).value().trim().to_owned();
                        if !text.is_empty() {
                            this.update_task(id, move |card| card.task.title = text, cx);
                        }
                    }
                },
            ),
            cx.subscribe_in(
                &notes,
                window,
                move |this, input, event: &InputEvent, _, cx| {
                    if let InputEvent::Blur = event {
                        let text = input.read(cx).value().trim_end().to_owned();
                        this.update_task(id, move |card| card.task.notes = text, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &new_step,
                window,
                move |this, input, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        let text = input.read(cx).value().trim().to_owned();
                        if !text.is_empty() {
                            this.update_task(
                                id,
                                move |card| {
                                    card.details.steps.push(Step {
                                        title: text,
                                        done: false,
                                    })
                                },
                                cx,
                            );
                            input.update(cx, |input, cx| input.set_value("", window, cx));
                        }
                    }
                },
            ),
            cx.subscribe_in(
                &due,
                window,
                move |this, _, event: &DatePickerEvent, _, cx| {
                    let DatePickerEvent::Change(value) = event;
                    let date = value.start().map(|at| at.date());
                    this.update_task(id, move |card| card.task.due = date, cx);
                },
            ),
            cx.subscribe_in(
                &remind,
                window,
                move |this, _, event: &DatePickerEvent, _, cx| {
                    let DatePickerEvent::Change(value) = event;
                    let at = value.start().and_then(local_to_utc);
                    this.update_task(id, move |card| card.details.reminder = at, cx);
                },
            ),
        ];

        Self {
            id,
            title,
            notes,
            new_step,
            due,
            remind,
            _subscriptions: subscriptions,
        }
    }

    /// True when the inputs no longer show `card` and the user is not typing in them.
    pub(super) fn is_stale(&self, card: &TaskCard, window: &Window, cx: &App) -> bool {
        if self.id != card.id() {
            return true;
        }
        let editing = self.title.read(cx).focus_handle(cx).is_focused(window)
            || self.notes.read(cx).focus_handle(cx).is_focused(window)
            || self.new_step.read(cx).focus_handle(cx).is_focused(window);
        if editing {
            return false;
        }
        self.title.read(cx).value().trim() != card.task.title
            || self.notes.read(cx).value().trim_end() != card.task.notes.trim_end()
            || self.due.read(cx).date().start() != card.task.due
            || self
                .remind
                .read(cx)
                .date_time()
                .start()
                .and_then(local_to_utc)
                != card.details.reminder
    }
}

impl TasksView {
    pub(super) fn render_detail(
        &self,
        id: Id,
        lists: &[TaskList],
        today: NaiveDate,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let store = self.state.read(cx).store();
        let card = todo::task_card(store, id).ok().flatten()?;
        let linked = card
            .details
            .message_id
            .map(|message_id| (message_id, store.get::<Message>(message_id).ok().flatten()));
        let detail = self.detail.as_ref().filter(|detail| detail.id == id)?;

        let completed = card.task.completed;
        let important = card.is_important();
        let selection = Selection::Task(id);
        let step_tooltip = tr(cx, "Mark as complete");
        let remove_step = tr(cx, "Remove step");
        let clear = tr(cx, "Clear");

        // ----- title + steps -----
        let steps = card.details.steps.iter().enumerate().map(|(index, step)| {
            let done = step.done;
            div()
                .id(("step", index as u64))
                .group("step-row")
                .flex()
                .items_center()
                .gap(SPACE_2)
                .h(px(30.))
                .px(SPACE_1)
                .rounded(RADIUS_MD)
                .hover(|s| s.bg(t.hover))
                .child(
                    div()
                        .id(("step-check", index as u64))
                        .flex()
                        .flex_shrink_0()
                        .items_center()
                        .justify_center()
                        .size(px(16.))
                        .rounded(RADIUS_FULL)
                        .border_1()
                        .cursor_pointer()
                        .when(!done, |el| {
                            el.tooltip(move |window, cx| {
                                Tooltip::new(step_tooltip).build(window, cx)
                            })
                        })
                        .when(done, |el| {
                            el.bg(t.accent).border_color(t.accent).child(
                                Icon::new(IconName::Check)
                                    .size(ICON_SIZE_XS)
                                    .text_color(t.on_accent),
                            )
                        })
                        .when(!done, |el| el.border_color(t.text_subtle))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.update_task(
                                id,
                                move |card| {
                                    if let Some(step) = card.details.steps.get_mut(index) {
                                        step.done = !step.done;
                                    }
                                },
                                cx,
                            )
                        })),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .truncate()
                        .text_size(TEXT_MD)
                        .when(done, |el| el.line_through().text_color(t.text_muted))
                        .child(step.title.clone()),
                )
                .child(
                    IconButton::new(("remove-step", index as u64), IconName::X)
                        .size(px(22.))
                        .icon_size(ICON_SIZE_SM)
                        .tooltip(remove_step)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.update_task(
                                id,
                                move |card| {
                                    if index < card.details.steps.len() {
                                        card.details.steps.remove(index);
                                    }
                                },
                                cx,
                            )
                        })),
                )
        });
        let title_card = panel_card(t)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(SPACE_2)
                    .child(
                        check_circle(
                            "detail-check",
                            completed,
                            false,
                            tr(
                                cx,
                                if completed {
                                    "Mark as not complete"
                                } else {
                                    "Mark as complete"
                                },
                            ),
                            t,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if completed {
                                this.reopen(id, cx);
                            } else {
                                this.check(selection, cx);
                            }
                        })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .child(Input::new(&detail.title).appearance(false)),
                    )
                    .child(
                        IconButton::new("detail-star", IconName::Star)
                            .active(important)
                            .tooltip(tr(
                                cx,
                                if important {
                                    "Remove importance"
                                } else {
                                    "Mark as important"
                                },
                            ))
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.toggle_important(id, cx)),
                            ),
                    ),
            )
            .children(steps)
            .child(
                Input::new(&detail.new_step).appearance(false).prefix(
                    Icon::new(IconName::Plus)
                        .size(ICON_SIZE_SM)
                        .text_color(t.accent),
                ),
            );

        // ----- My Day -----
        let in_my_day = card.in_my_day(today);
        let my_day = panel_card(t).child(
            action_row(
                "detail-my-day",
                IconName::Sun,
                tr(
                    cx,
                    if in_my_day {
                        "Added to My Day"
                    } else {
                        "Add to My Day"
                    },
                )
                .to_owned(),
                in_my_day,
                t,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.update_task(
                    id,
                    move |card| card.details.my_day = if in_my_day { None } else { Some(today) },
                    cx,
                )
            })),
        );

        // ----- reminder -----
        let reminder = card.details.reminder;
        let reminder_choices: Vec<(&'static str, Option<DateTime<Utc>>)> = vec![
            (tr(cx, "Later today"), later_today()),
            (tr(cx, "Tomorrow 9:00"), at_local(next_day(today), 9)),
            (tr(cx, "Next week"), at_local(next_monday(today), 9)),
        ];
        let reminder_card = panel_card(t)
            .child(action_row(
                "detail-reminder",
                IconName::Bell,
                reminder.map_or_else(
                    || tr(cx, "Remind me").to_owned(),
                    |at| tr(cx, "Remind me {}").replace("{}", &reminder_label(at, today, cx)),
                ),
                reminder.is_some(),
                t,
            ))
            .child(
                chip_row()
                    .children(reminder_choices.into_iter().enumerate().filter_map(
                        |(index, (label, at))| {
                            let at = at?;
                            Some(
                                option_chip(
                                    ("reminder-choice", index as u64),
                                    label,
                                    reminder == Some(at),
                                    t,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.update_task(
                                            id,
                                            move |card| card.details.reminder = Some(at),
                                            cx,
                                        )
                                    },
                                )),
                            )
                        },
                    ))
                    .when(reminder.is_some(), |el| {
                        el.child(clear_button(
                            "clear-reminder",
                            clear,
                            cx.listener(move |this, _, _, cx| {
                                this.update_task(id, |card| card.details.reminder = None, cx)
                            }),
                        ))
                    }),
            )
            .child(
                div().px(SPACE_1).child(
                    DatePicker::new(&detail.remind)
                        .placeholder(tr(cx, "Pick a date and time"))
                        .cleanable(true),
                ),
            );

        // ----- due date -----
        let due = card.task.due;
        let due_choices = [
            (tr(cx, "Today"), today),
            (tr(cx, "Tomorrow"), next_day(today)),
            (tr(cx, "Next week"), next_monday(today)),
        ];
        let overdue = card.is_overdue(today);
        let due_card =
            panel_card(t)
                .child(
                    action_row(
                        "detail-due",
                        IconName::CalendarDays,
                        due.map_or_else(
                            || tr(cx, "Add due date").to_owned(),
                            |due| tr(cx, "Due {}").replace("{}", &day_text(due, today, cx)),
                        ),
                        due.is_some(),
                        t,
                    )
                    .when(overdue, |el| el.text_color(t.danger)),
                )
                .child(
                    chip_row()
                        .children(due_choices.into_iter().enumerate().map(
                            |(index, (label, date))| {
                                option_chip(
                                    ("due-choice", index as u64),
                                    label,
                                    due == Some(date),
                                    t,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.update_task(
                                            id,
                                            move |card| card.task.due = Some(date),
                                            cx,
                                        )
                                    },
                                ))
                            },
                        ))
                        .when(due.is_some(), |el| {
                            el.child(clear_button(
                                "clear-due",
                                clear,
                                cx.listener(move |this, _, _, cx| {
                                    this.update_task(id, |card| card.task.due = None, cx)
                                }),
                            ))
                        }),
                )
                .child(
                    div().px(SPACE_1).child(
                        DatePicker::new(&detail.due)
                            .placeholder(tr(cx, "Pick a due date"))
                            .cleanable(true),
                    ),
                );

        // ----- repeat -----
        let repeat = card.details.repeat;
        let repeat_card = panel_card(t)
            .child(action_row(
                "detail-repeat",
                IconName::Repeat,
                repeat.map_or_else(
                    || tr(cx, "Repeat").to_owned(),
                    |repeat| tr(cx, "Repeat: {}").replace("{}", tr(cx, repeat.label())),
                ),
                repeat.is_some(),
                t,
            ))
            .child(
                chip_row()
                    .children(Repeat::ALL.into_iter().enumerate().map(|(index, choice)| {
                        option_chip(
                            ("repeat-choice", index as u64),
                            tr(cx, choice.label()),
                            repeat == Some(choice),
                            t,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.update_task(
                                id,
                                move |card| {
                                    card.details.repeat = Some(choice);
                                    // A repeating task needs a first due date.
                                    if card.task.due.is_none() {
                                        card.task.due = Some(today);
                                    }
                                },
                                cx,
                            )
                        }))
                    }))
                    .when(repeat.is_some(), |el| {
                        el.child(clear_button(
                            "clear-repeat",
                            clear,
                            cx.listener(move |this, _, _, cx| {
                                this.update_task(id, |card| card.details.repeat = None, cx)
                            }),
                        ))
                    }),
            );

        // ----- list -----
        let list_id = card.details.list_id;
        let list_chips = std::iter::once((None, tr(cx, "Tasks").to_owned()))
            .chain(lists.iter().map(|list| (Some(list.id), list.name.clone())))
            .enumerate()
            .map(|(index, (target, name))| {
                option_chip(("move-list", index as u64), name, list_id == target, t).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.update_task(id, move |card| card.details.list_id = target, cx)
                    }),
                )
            });
        let list_card = panel_card(t)
            .child(section_title(tr(cx, "List"), t))
            .child(chip_row().children(list_chips));

        // ----- linked email -----
        let linked_card = linked.map(|(message_id, message)| {
            let body = match message {
                Some(message) => {
                    let folder_id = message.folder_id;
                    div()
                        .flex()
                        .items_center()
                        .gap(SPACE_2)
                        .child(
                            Icon::new(IconName::Mail)
                                .size(ICON_SIZE)
                                .text_color(t.accent),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w(px(0.))
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(TEXT_MD)
                                        .child(message.subject.clone()),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(TEXT_XS)
                                        .text_color(t.text_muted)
                                        .child(
                                            tr(cx, "From {}")
                                                .replace("{}", &message.from.display()),
                                        ),
                                ),
                        )
                        .child(
                            Button::new("open-linked-email")
                                .compact()
                                .label(tr(cx, "Open"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_message(message_id, folder_id, cx)
                                })),
                        )
                        .into_any_element()
                }
                None => Banner::warning(
                    "linked-email-missing",
                    tr(cx, "The linked email no longer exists"),
                )
                .into_any_element(),
            };
            panel_card(t)
                .child(section_title(tr(cx, "Linked email"), t))
                .child(body)
        });

        // ----- footer -----
        let footer_text = match card.details.completed_at {
            Some(at) if completed => tr(cx, "Completed {}").replace(
                "{}",
                &at.with_timezone(&Local)
                    .format_in(prefs(cx).language, "%a %-d %b"),
            ),
            _ => tr(cx, "Created {}").replace(
                "{}",
                &card
                    .task
                    .created
                    .with_timezone(&Local)
                    .format_in(prefs(cx).language, "%a %-d %b"),
            ),
        };

        Some(
            div()
                .flex()
                .flex_col()
                .size_full()
                .child(
                    div()
                        .id("task-detail")
                        .flex_1()
                        .min_h(px(0.))
                        .overflow_y_scroll()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(SPACE_3)
                                .p(SPACE_4)
                                .child(title_card)
                                .child(my_day)
                                .child(reminder_card)
                                .child(due_card)
                                .child(repeat_card)
                                .child(list_card)
                                .when_some(linked_card, |el, linked| el.child(linked))
                                .child(
                                    panel_card(t)
                                        .child(section_title(tr(cx, "Notes"), t))
                                        .child(Textarea::new(&detail.notes).h(px(120.))),
                                ),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(SPACE_2)
                        .h(PANE_HEADER_HEIGHT)
                        .px(SPACE_3)
                        .border_t_1()
                        .border_color(t.border_subtle)
                        .child(
                            IconButton::new("close-detail", IconName::PanelRightClose)
                                .tooltip(tr(cx, "Close"))
                                .on_click(cx.listener(|this, _, _, cx| this.close_detail(cx))),
                        )
                        .child(
                            div()
                                .flex_1()
                                .text_center()
                                .text_size(TEXT_SM)
                                .text_color(t.text_subtle)
                                .child(footer_text),
                        )
                        .child(
                            IconButton::new("delete-task", IconName::Trash)
                                .tooltip(tr(cx, "Delete task"))
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.delete_task(id, cx)),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    /// Panel for a flagged message that is not a task yet.
    pub(super) fn render_message_detail(
        &self,
        message: &Message,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (message_id, folder_id) = (message.id, message.folder_id);
        let subject = if message.subject.trim().is_empty() {
            tr(cx, "(no subject)").to_owned()
        } else {
            message.subject.clone()
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .id("flag-detail")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .p(SPACE_4)
                    .child(
                        panel_card(t)
                            .gap(SPACE_3)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(SPACE_1_5)
                                    .text_size(TEXT_XS)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(t.danger)
                                    .child(Icon::new(IconName::Flag).size(ICON_SIZE_XS))
                                    .child(tr(cx, "Flagged email")),
                            )
                            .child(
                                div()
                                    .text_size(TEXT_LG)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(subject),
                            )
                            .child(div().text_size(TEXT_SM).text_color(t.text_muted).child(
                                format!(
                                        "{} · {}",
                                        tr(cx, "From {}").replace("{}", &message.from.display()),
                                        message
                                            .date
                                            .with_timezone(&Local)
                                            .format_in(prefs(cx).language, "%a %-d %b %Y, %H:%M")
                                    ),
                            ))
                            .when(!message.preview.trim().is_empty(), |el| {
                                el.child(
                                    div()
                                        .text_size(TEXT_MD)
                                        .text_color(t.text_muted)
                                        .child(message.preview.clone()),
                                )
                            })
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .gap(SPACE_2)
                                    .child(
                                        Button::new("flag-open")
                                            .primary()
                                            .icon(IconName::MailOpen)
                                            .label(tr(cx, "Open email"))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.open_message(message_id, folder_id, cx)
                                            })),
                                    )
                                    .child(
                                        Button::new("flag-to-task")
                                            .icon(IconName::ListPlus)
                                            .label(tr(cx, "Make it a task"))
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.task_from_flag(message_id, window, cx)
                                            })),
                                    )
                                    .child(
                                        Button::new("flag-complete")
                                            .ghost()
                                            .icon(IconName::Check)
                                            .label(tr(cx, "Mark as complete"))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.check(Selection::Message(message_id), cx);
                                                this.close_detail(cx);
                                            })),
                                    ),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(PANE_HEADER_HEIGHT)
                    .px(SPACE_3)
                    .border_t_1()
                    .border_color(t.border_subtle)
                    .child(
                        IconButton::new("close-flag-detail", IconName::PanelRightClose)
                            .tooltip(tr(cx, "Close"))
                            .on_click(cx.listener(|this, _, _, cx| this.close_detail(cx))),
                    ),
            )
            .into_any_element()
    }

    /// Turns a flagged message into a real task linked to it and opens that task.
    fn task_from_flag(&mut self, message_id: Id, window: &mut Window, cx: &mut Context<Self>) {
        let message = self
            .state
            .read(cx)
            .store()
            .get::<Message>(message_id)
            .ok()
            .flatten();
        let Some(message) = message else {
            return;
        };
        let card = todo::task_from_message(&message, Utc::now());
        if !self.run(Command::Todo(TaskCommand::SaveTask(Box::new(card))), cx) {
            return;
        }
        let created = todo::list_task_cards(self.state.read(cx).store())
            .unwrap_or_default()
            .into_iter()
            .filter(|card| card.details.message_id == Some(message_id))
            .map(|card| card.id())
            .max();
        if let Some(id) = created {
            self.open(Selection::Task(id), window, cx);
        }
    }

    fn open_message(&mut self, message_id: Id, folder_id: Id, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            state.select_folder(Some(folder_id), cx);
            state.select_message(Some(message_id), cx);
            state.set_module(Module::Mail, cx);
        });
    }
}

fn panel_card(t: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(SPACE_1)
        .p(SPACE_2)
        .rounded(RADIUS_LG)
        .bg(t.elevated)
        .border_1()
        .border_color(t.border_subtle)
}

fn section_title(title: &'static str, t: &Theme) -> impl IntoElement {
    div()
        .px(SPACE_1)
        .text_size(TEXT_XS)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(t.text_subtle)
        .child(title.to_uppercase())
}

/// Icon + label line; accent coloured when the setting is on.
fn action_row(
    id: &'static str,
    icon: IconName,
    label: String,
    active: bool,
    t: &Theme,
) -> Stateful<Div> {
    let color = if active { t.accent } else { t.text_muted };
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(SPACE_2)
        .h(px(32.))
        .px(SPACE_1)
        .rounded(RADIUS_MD)
        .cursor_pointer()
        .text_size(TEXT_MD)
        .text_color(color)
        .hover(|s| s.bg(t.hover))
        .child(Icon::new(icon).size(ICON_SIZE).text_color(color))
        .child(div().flex_1().min_w(px(0.)).truncate().child(label))
}

fn chip_row() -> Div {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(SPACE_1_5)
        .px(SPACE_1)
        .pb(SPACE_1)
}

fn clear_button(
    id: &'static str,
    tooltip: &'static str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    IconButton::new(id, IconName::X)
        .size(px(24.))
        .icon_size(ICON_SIZE_SM)
        .tooltip(tooltip)
        .on_click(on_click)
}

/// Local wall time to UTC (`None` in a DST gap).
fn local_to_utc(at: chrono::NaiveDateTime) -> Option<DateTime<Utc>> {
    at.and_local_timezone(Local)
        .earliest()
        .map(|at| at.with_timezone(&Utc))
}

fn next_day(date: NaiveDate) -> NaiveDate {
    date.checked_add_days(Days::new(1)).unwrap_or(date)
}

/// The Monday after `date` (a week later when `date` is a Monday).
fn next_monday(date: NaiveDate) -> NaiveDate {
    let days = 7 - u64::from(date.weekday().num_days_from_monday());
    date.checked_add_days(Days::new(days)).unwrap_or(date)
}

/// `hour`:00 local time on `date`, in UTC.
fn at_local(date: NaiveDate, hour: u32) -> Option<DateTime<Utc>> {
    date.and_hms_opt(hour, 0, 0)?
        .and_local_timezone(Local)
        .earliest()
        .map(|at| at.with_timezone(&Utc))
}

/// 18:00 today, or in an hour (on the hour) when it is already late.
fn later_today() -> Option<DateTime<Utc>> {
    let now = Local::now();
    if now.hour() < 17 {
        at_local(now.date_naive(), 18)
    } else {
        let next_hour = now.hour() + 1;
        (next_hour < 24)
            .then(|| at_local(now.date_naive(), next_hour))
            .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::{local_to_utc, next_monday};
    use chrono::NaiveDate;

    #[test]
    fn date_helpers() {
        let date = |y, m, d| NaiveDate::from_ymd_opt(y, m, d).unwrap();
        let noon = date(2026, 6, 1).and_hms_opt(12, 0, 0).unwrap();
        assert!(local_to_utc(noon).is_some());
        // 2026-11-14 is a Saturday, 2026-11-16 a Monday.
        assert_eq!(next_monday(date(2026, 11, 14)), date(2026, 11, 16));
        assert_eq!(next_monday(date(2026, 11, 16)), date(2026, 11, 23));
    }
}
