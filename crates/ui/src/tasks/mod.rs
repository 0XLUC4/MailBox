//! Tasks module (Microsoft To Do style): smart lists (My Day, Important, Planned, Flagged email,
//! Tasks) + custom lists | task list with inline add, check-off animation, sort/group and a
//! collapsible Completed section | detail panel (steps, My Day, reminder, due, repeat, notes).
//!
//! Flagged messages show up in "Flagged email" automatically; completing one unflags it.
//! Writes go through `Command::Todo` / `Command::SetMessageFlags` (undoable).

mod detail;
mod reorder;

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use chrono::{DateTime, Datelike, Local, NaiveDate, Utc};
use gpui::component::input::{Input, InputEvent, InputState};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::todo::{self, DueBucket, SmartList, TaskCard, TaskList, TaskSort};
use mailbox_core::{Command, Id, Message, TaskCommand};

use crate::actions;
use crate::app_state::{AppState, Module};
use crate::components::{EmptyState, Icon, IconButton, IconName};
use crate::theme::{
    ACCENTS, ICON_SIZE, ICON_SIZE_SM, RADIUS_FULL, RADIUS_LG, RADIUS_MD, SPACE_0_5, SPACE_1,
    SPACE_1_5, SPACE_2, SPACE_3, SPACE_6, TEXT_2XL, TEXT_MD, TEXT_SM, TEXT_XS, Theme, hex, theme,
};

use detail::TaskDetail;

/// How long a checked task stays in place (struck through, fading) before moving.
const COMPLETE_DELAY: Duration = Duration::from_millis(380);

/// What the detail panel shows: a task, or a flagged message that has no task yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Selection {
    Task(Id),
    Message(Id),
}

pub struct TasksView {
    state: Entity<AppState>,
    focus: FocusHandle,
    /// Focus the view on its next render (the module was just opened).
    needs_focus: bool,
    /// "New task" from the ribbon: focus the add-task input on the next render.
    focus_new_task: bool,
    list: SmartList,
    selected: Option<Selection>,
    sort: TaskSort,
    group_by_due: bool,
    show_options: bool,
    completed_open: bool,
    /// Rows playing the check-off animation before their command runs.
    completing: HashSet<Selection>,
    new_task: Entity<InputState>,
    new_list: Entity<InputState>,
    /// Inline rename of the open custom list.
    rename: Option<(Entity<InputState>, Subscription)>,
    detail: Option<TaskDetail>,
    _reminders: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl TasksView {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let new_task = cx.new(|cx| InputState::new(window, cx).placeholder("Add a task"));
        let new_list = cx.new(|cx| InputState::new(window, cx).placeholder("New list"));
        let subscriptions = vec![
            cx.observe_in(&state, window, |this, state, window, cx| {
                if state.read(cx).module() != Module::Tasks {
                    this.needs_focus = true;
                }
                this.sync_detail(window, cx);
                cx.notify();
            }),
            cx.subscribe_in(
                &new_task,
                window,
                |this, input, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        let title = input.read(cx).value().trim().to_owned();
                        if !title.is_empty() && this.add_task(&title, cx) {
                            input.update(cx, |input, cx| input.set_value("", window, cx));
                        }
                    }
                },
            ),
            cx.subscribe_in(
                &new_list,
                window,
                |this, input, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        let name = input.read(cx).value().trim().to_owned();
                        if !name.is_empty() && this.create_list(name, window, cx) {
                            input.update(cx, |input, cx| input.set_value("", window, cx));
                        }
                    }
                },
            ),
        ];
        let view = cx.entity().downgrade();
        crate::calendar::module_action::<actions::NewTask, _>(
            &view,
            &state,
            Module::Tasks,
            cx,
            |this, cx| {
                this.focus_new_task = true;
                cx.notify();
            },
        );
        crate::calendar::module_action::<actions::CompleteTask, _>(
            &view,
            &state,
            Module::Tasks,
            cx,
            |this, cx| {
                if let Some(selection) = this.selected {
                    this.check(selection, cx);
                }
            },
        );
        Self {
            state,
            focus_new_task: false,
            focus: cx.focus_handle(),
            needs_focus: true,
            list: SmartList::MyDay,
            selected: None,
            sort: TaskSort::default(),
            group_by_due: false,
            show_options: false,
            completed_open: false,
            completing: HashSet::new(),
            new_task,
            new_list,
            rename: None,
            detail: None,
            _reminders: Self::start_reminders(cx),
            _subscriptions: subscriptions,
        }
    }

    // ---------- actions ----------

    fn run(&mut self, command: Command, cx: &mut Context<Self>) -> bool {
        self.state.update(cx, |state, cx| state.run(command, cx))
    }

    fn today() -> NaiveDate {
        Local::now().date_naive()
    }

    fn set_list(&mut self, list: SmartList, cx: &mut Context<Self>) {
        if self.list != list {
            self.list = list;
            self.group_by_due = list == SmartList::Planned;
            self.rename = None;
            self.selected = None;
            self.detail = None;
        }
        cx.notify();
    }

    fn add_task(&mut self, title: &str, cx: &mut Context<Self>) -> bool {
        // Tasks are not synced: keep them on this computer so removing an account keeps them.
        let account_id = mailbox_core::calendar::LOCAL_ACCOUNT;
        let card = self
            .list
            .new_task(account_id, title, Self::today(), Utc::now());
        self.run(Command::Todo(TaskCommand::SaveTask(Box::new(card))), cx)
    }

    fn create_list(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let count =
            todo::list_task_lists(self.state.read(cx).store()).map_or(0, |lists| lists.len());
        let list = TaskList {
            id: 0,
            name,
            color: ACCENTS[count % ACCENTS.len()].1,
            position: 0,
        };
        if !self.run(Command::Todo(TaskCommand::SaveList(Box::new(list))), cx) {
            return false;
        }
        let newest = todo::list_task_lists(self.state.read(cx).store())
            .unwrap_or_default()
            .iter()
            .map(|list| list.id)
            .max();
        if let Some(id) = newest {
            self.set_list(SmartList::List(id), cx);
            self.new_task
                .update(cx, |input, cx| input.focus(window, cx));
        }
        true
    }

    fn start_rename(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let SmartList::List(list_id) = self.list else {
            return;
        };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name));
        input.update(cx, |input, cx| input.focus(window, cx));
        let subscription = cx.subscribe_in(
            &input,
            window,
            move |this, input, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    let name = input.read(cx).value().trim().to_owned();
                    this.finish_rename(list_id, name, cx);
                }
            },
        );
        self.rename = Some((input, subscription));
        cx.notify();
    }

    fn finish_rename(&mut self, list_id: Id, name: String, cx: &mut Context<Self>) {
        if self.rename.take().is_none() {
            return;
        }
        let list = self
            .state
            .read(cx)
            .store()
            .get::<TaskList>(list_id)
            .ok()
            .flatten();
        if let Some(mut list) = list.filter(|list| !name.is_empty() && list.name != name) {
            list.name = name;
            self.run(Command::Todo(TaskCommand::SaveList(Box::new(list))), cx);
        }
        cx.notify();
    }

    fn delete_list(&mut self, list_id: Id, cx: &mut Context<Self>) {
        if self.run(Command::Todo(TaskCommand::DeleteList(list_id)), cx) {
            self.set_list(SmartList::Tasks, cx);
        }
    }

    fn open(&mut self, selection: Selection, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = Some(selection);
        self.detail = None;
        self.sync_detail(window, cx);
        cx.notify();
    }

    fn close_detail(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        self.detail = None;
        cx.notify();
    }

    /// Rebuilds the detail inputs when the selected task changed outside them (undo, other view).
    fn sync_detail(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Selection::Task(id)) = self.selected else {
            self.detail = None;
            return;
        };
        let card = todo::task_card(self.state.read(cx).store(), id)
            .ok()
            .flatten();
        let Some(card) = card else {
            self.selected = None;
            self.detail = None;
            return;
        };
        let stale = self
            .detail
            .as_ref()
            .is_none_or(|detail| detail.is_stale(&card, window, cx));
        if stale {
            self.detail = Some(TaskDetail::new(&card, window, cx));
        }
    }

    /// Loads a task, applies `change` and saves it when something changed (one undo step).
    fn update_task(&mut self, id: Id, change: impl FnOnce(&mut TaskCard), cx: &mut Context<Self>) {
        let card = todo::task_card(self.state.read(cx).store(), id)
            .ok()
            .flatten();
        let Some(mut card) = card else {
            return;
        };
        let before = card.clone();
        change(&mut card);
        if card != before {
            self.run(Command::Todo(TaskCommand::SaveTask(Box::new(card))), cx);
        }
    }

    fn delete_task(&mut self, id: Id, cx: &mut Context<Self>) {
        if self.run(Command::Todo(TaskCommand::DeleteTasks(vec![id])), cx) {
            self.close_detail(cx);
        }
    }

    /// Checks a row: plays the animation, then completes the task (or unflags the message).
    fn check(&mut self, selection: Selection, cx: &mut Context<Self>) {
        if !self.completing.insert(selection) {
            return;
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COMPLETE_DELAY).await;
            let _ = this.update(cx, |this, cx| {
                this.completing.remove(&selection);
                let command = this.complete_command(selection, cx);
                if let Some(command) = command {
                    this.run(command, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn complete_command(&self, selection: Selection, cx: &App) -> Option<Command> {
        let store = self.state.read(cx).store();
        match selection {
            Selection::Message(id) => Some(unflag(id)),
            Selection::Task(id) => {
                let card = todo::task_card(store, id).ok().flatten()?;
                let complete = Command::Todo(TaskCommand::SetCompleted {
                    id,
                    completed: true,
                });
                // Completing a flagged-email task clears the flag too, as one undo step.
                let flagged = card
                    .details
                    .message_id
                    .and_then(|message_id| store.get::<Message>(message_id).ok().flatten())
                    .filter(|message| message.flags.flagged);
                Some(match flagged {
                    Some(message) => Command::Batch(vec![complete, unflag(message.id)]),
                    None => complete,
                })
            }
        }
    }

    fn reopen(&mut self, id: Id, cx: &mut Context<Self>) {
        self.run(
            Command::Todo(TaskCommand::SetCompleted {
                id,
                completed: false,
            }),
            cx,
        );
    }

    fn toggle_important(&mut self, id: Id, cx: &mut Context<Self>) {
        self.update_task(
            id,
            |card| {
                card.task.importance = if card.is_important() {
                    mailbox_core::Importance::Normal
                } else {
                    mailbox_core::Importance::High
                };
            },
            cx,
        );
    }

    // ---------- rendering ----------

    fn list_title(&self, lists: &[TaskList]) -> String {
        match self.list {
            SmartList::List(id) => lists
                .iter()
                .find(|list| list.id == id)
                .map_or_else(|| "List".to_owned(), |list| list.name.clone()),
            other => other.label().to_owned(),
        }
    }

    fn render_sidebar(
        &self,
        counts: &HashMap<SmartList, usize>,
        lists: &[TaskList],
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let row = |list: SmartList,
                   icon: IconName,
                   color: Hsla,
                   label: String,
                   cx: &mut Context<Self>| {
            let active = self.list == list;
            let count = counts.get(&list).copied().unwrap_or(0);
            let id = match list {
                SmartList::List(id) => ("task-list", id as u64),
                other => (
                    "smart-list",
                    SmartList::BUILT_IN
                        .iter()
                        .position(|l| *l == other)
                        .unwrap_or(0) as u64,
                ),
            };
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(SPACE_2)
                .h(px(32.))
                .px(SPACE_2)
                .rounded(RADIUS_MD)
                .cursor_pointer()
                .text_size(TEXT_MD)
                .when(active, |el| {
                    el.bg(t.accent_subtle).font_weight(FontWeight::MEDIUM)
                })
                .when(!active, |el| el.hover(|s| s.bg(t.hover)))
                .child(Icon::new(icon).size(ICON_SIZE).text_color(color))
                .child(div().flex_1().min_w(px(0.)).truncate().child(label))
                .when(count > 0, |el| {
                    el.child(
                        div()
                            .text_size(TEXT_XS)
                            .text_color(if active { t.accent } else { t.text_subtle })
                            .child(count.to_string()),
                    )
                })
                .on_click(cx.listener(move |this, _, _, cx| this.set_list(list, cx)))
        };

        let built_in: Vec<Stateful<Div>> = SmartList::BUILT_IN
            .into_iter()
            .map(|list| {
                let (icon, color) = smart_style(list, t);
                row(list, icon, color, list.label().to_owned(), cx)
            })
            .collect();
        let custom: Vec<Stateful<Div>> = lists
            .iter()
            .map(|list| {
                row(
                    SmartList::List(list.id),
                    IconName::ListTodo,
                    hex(list.color),
                    list.name.clone(),
                    cx,
                )
            })
            .collect();

        div()
            .id("tasks-sidebar")
            .flex()
            .flex_col()
            .size_full()
            .overflow_y_scroll()
            .p(SPACE_2)
            .gap(px(1.))
            .children(built_in)
            .child(div().h(px(1.)).mx(SPACE_2).my(SPACE_2).bg(t.border_subtle))
            .children(custom)
            .child(
                div().px(SPACE_1).pt(SPACE_2).child(
                    Input::new(&self.new_list).prefix(
                        Icon::new(IconName::ListPlus)
                            .size(ICON_SIZE_SM)
                            .text_color(t.text_subtle),
                    ),
                ),
            )
    }

    fn render_header(&self, title: String, t: &Theme, cx: &mut Context<Self>) -> Div {
        let (icon, color) = match self.list {
            SmartList::List(_) => (IconName::ListTodo, t.accent),
            other => smart_style(other, t),
        };
        let custom_list = match self.list {
            SmartList::List(id) => Some(id),
            _ => None,
        };
        let subtitle =
            (self.list == SmartList::MyDay).then(|| Local::now().format("%A, %-d %B").to_string());
        let rename_name = title.clone();

        let title_element = match &self.rename {
            Some((input, _)) => div()
                .w(px(320.))
                .child(Input::new(input))
                .into_any_element(),
            None => div()
                .text_size(TEXT_2XL)
                .font_weight(FontWeight::SEMIBOLD)
                .truncate()
                .child(title)
                .into_any_element(),
        };

        let options = self.show_options.then(|| {
            let sort_chips = TaskSort::ALL.into_iter().enumerate().map(|(index, sort)| {
                option_chip(
                    ("task-sort", index as u64),
                    sort.label(),
                    self.sort == sort,
                    t,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.sort = sort;
                    cx.notify();
                }))
            });
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(SPACE_1_5)
                .child(
                    div()
                        .text_size(TEXT_XS)
                        .text_color(t.text_subtle)
                        .child("SORT"),
                )
                .children(sort_chips)
                .child(div().w(SPACE_3))
                .child(
                    option_chip("task-group-due", "Group by due date", self.group_by_due, t)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.group_by_due = !this.group_by_due;
                            cx.notify();
                        })),
                )
        });

        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .gap(SPACE_2)
            .px(SPACE_6)
            .pt(SPACE_6)
            .pb(SPACE_3)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(SPACE_3)
                    .child(Icon::new(icon).size(px(22.)).text_color(color))
                    .child(div().flex_1().min_w(px(0.)).child(title_element))
                    .child(
                        IconButton::new("task-options", IconName::ArrowUpDown)
                            .active(self.show_options)
                            .tooltip("Sort and group")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_options = !this.show_options;
                                cx.notify();
                            })),
                    )
                    .when_some(custom_list, |el, list_id| {
                        el.child(
                            IconButton::new("rename-list", IconName::Pencil)
                                .tooltip("Rename list")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.start_rename(rename_name.clone(), window, cx)
                                })),
                        )
                        .child(
                            IconButton::new("delete-list", IconName::Trash)
                                .tooltip("Delete list and its tasks")
                                .on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        this.delete_list(list_id, cx)
                                    }),
                                ),
                        )
                    }),
            )
            .when_some(subtitle, |el, subtitle| {
                el.child(
                    div()
                        .pl(px(34.))
                        .text_size(TEXT_SM)
                        .text_color(t.text_muted)
                        .child(subtitle),
                )
            })
            .when_some(options, |el, options| el.child(options))
    }

    fn render_task_row(
        &self,
        card: &TaskCard,
        lists: &[TaskList],
        today: NaiveDate,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = card.id();
        let selection = Selection::Task(id);
        let completing = self.completing.contains(&selection);
        let done = card.task.completed || completing;
        let selected = self.selected == Some(selection);
        let important = card.is_important();
        let completed = card.task.completed;
        let meta = self.task_meta(card, lists, today, t);

        let row = div()
            .id(("task-row", id as u64))
            .group("task-row")
            .flex()
            .items_center()
            .gap(SPACE_3)
            .min_h(px(48.))
            .px(SPACE_3)
            .py(SPACE_2)
            .rounded(RADIUS_LG)
            .bg(t.elevated)
            .border_1()
            .border_color(if selected { t.accent } else { t.border_subtle })
            .cursor_pointer()
            .hover(|s| s.bg(t.hover))
            .child(
                check_circle(("task-check", id as u64), done, t).on_click(cx.listener(
                    move |this, _, _, cx| {
                        cx.stop_propagation();
                        if completed {
                            this.reopen(id, cx);
                        } else {
                            this.check(selection, cx);
                        }
                    },
                )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.))
                    .gap(SPACE_0_5)
                    .child(
                        div()
                            .truncate()
                            .text_size(TEXT_MD)
                            .when(done, |el| el.line_through().text_color(t.text_muted))
                            .child(card.task.title.clone()),
                    )
                    .when(!meta.is_empty(), |el| {
                        el.child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap(SPACE_2)
                                .text_size(TEXT_XS)
                                .text_color(t.text_muted)
                                .children(meta),
                        )
                    }),
            )
            .child(
                div()
                    .id(("task-star", id as u64))
                    .flex_shrink_0()
                    .p(SPACE_1)
                    .rounded(RADIUS_MD)
                    .hover(|s| s.bg(t.hover))
                    .child(
                        Icon::new(IconName::Star)
                            .size(ICON_SIZE)
                            .text_color(if important { t.accent } else { t.text_subtle }),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle_important(id, cx);
                    })),
            )
            .on_click(cx.listener(move |this, _, window, cx| this.open(selection, window, cx)));
        // Manual order: drag a row onto another to move it there.
        let row = if !completed && self.can_reorder() {
            let accent = t.accent;
            row.on_drag(
                reorder::DraggedTask {
                    id,
                    title: card.task.title.clone().into(),
                },
                |dragged, _, _, cx| cx.new(|_| dragged.clone()),
            )
            .drag_over::<reorder::DraggedTask>(move |style, _, _, _| {
                style.border_color(accent).border_t_2()
            })
            .on_drop(cx.listener(
                move |this, dragged: &reorder::DraggedTask, _, cx| this.reorder(dragged.id, id, cx),
            ))
        } else {
            row
        };

        if completing {
            row.with_animation(
                ("task-completing", id as u64),
                Animation::new(COMPLETE_DELAY),
                |row, delta| row.opacity(1. - 0.55 * delta),
            )
            .into_any_element()
        } else {
            row.into_any_element()
        }
    }

    /// Small icon + text items under a task title.
    fn task_meta(
        &self,
        card: &TaskCard,
        lists: &[TaskList],
        today: NaiveDate,
        t: &Theme,
    ) -> Vec<AnyElement> {
        let mut meta = Vec::new();
        let item = |icon: Option<IconName>, text: String, color: Hsla| {
            div()
                .flex()
                .items_center()
                .gap(SPACE_1)
                .text_color(color)
                .when_some(icon, |el, icon| el.child(Icon::new(icon).size(px(12.))))
                .child(text)
                .into_any_element()
        };
        if !matches!(self.list, SmartList::List(_) | SmartList::Tasks) {
            let list_name = card
                .details
                .list_id
                .and_then(|list_id| lists.iter().find(|list| list.id == list_id))
                .map_or_else(|| "Tasks".to_owned(), |list| list.name.clone());
            meta.push(item(None, list_name, t.text_muted));
        }
        if self.list != SmartList::MyDay && card.in_my_day(today) {
            meta.push(item(Some(IconName::Sun), "My Day".into(), t.text_muted));
        }
        if let Some((done, total)) = card.step_progress() {
            meta.push(item(None, format!("{done} of {total}"), t.text_muted));
        }
        if let Some(due) = card.task.due {
            let color = if card.is_overdue(today) {
                t.danger
            } else if due == today && !card.task.completed {
                t.accent
            } else {
                t.text_muted
            };
            let mut text = day_label(due, today);
            if card.is_overdue(today) {
                text = format!("Overdue, {text}");
            }
            meta.push(item(Some(IconName::CalendarDays), text, color));
        }
        if let Some(reminder) = card.details.reminder {
            meta.push(item(
                Some(IconName::Bell),
                reminder_label(reminder, today),
                t.text_muted,
            ));
        }
        if card.details.repeat.is_some() {
            meta.push(item(Some(IconName::Repeat), String::new(), t.text_muted));
        }
        if card.details.message_id.is_some() {
            meta.push(item(Some(IconName::Mail), String::new(), t.text_muted));
        }
        if !card.task.notes.trim().is_empty() {
            meta.push(item(
                Some(IconName::StickyNote),
                String::new(),
                t.text_muted,
            ));
        }
        meta
    }

    fn render_message_row(
        &self,
        message: &Message,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = message.id;
        let selection = Selection::Message(id);
        let completing = self.completing.contains(&selection);
        let selected = self.selected == Some(selection);
        let subject = if message.subject.trim().is_empty() {
            "(no subject)".to_owned()
        } else {
            message.subject.clone()
        };
        let row = div()
            .id(("flag-row", id as u64))
            .group("task-row")
            .flex()
            .items_center()
            .gap(SPACE_3)
            .min_h(px(48.))
            .px(SPACE_3)
            .py(SPACE_2)
            .rounded(RADIUS_LG)
            .bg(t.elevated)
            .border_1()
            .border_color(if selected { t.accent } else { t.border_subtle })
            .cursor_pointer()
            .hover(|s| s.bg(t.hover))
            .child(
                check_circle(("flag-check", id as u64), completing, t).on_click(cx.listener(
                    move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.check(selection, cx);
                    },
                )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.))
                    .gap(SPACE_0_5)
                    .child(
                        div()
                            .truncate()
                            .text_size(TEXT_MD)
                            .when(completing, |el| el.line_through().text_color(t.text_muted))
                            .child(subject),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(SPACE_1)
                            .text_size(TEXT_XS)
                            .text_color(t.text_muted)
                            .child(Icon::new(IconName::Mail).size(px(12.)))
                            .child(format!(
                                "{} - {}",
                                message.from.display(),
                                message.date.with_timezone(&Local).format("%-d %b")
                            )),
                    ),
            )
            .child(
                Icon::new(IconName::Flag)
                    .size(ICON_SIZE_SM)
                    .text_color(t.danger),
            )
            .on_click(cx.listener(move |this, _, window, cx| this.open(selection, window, cx)));
        if completing {
            row.with_animation(
                ("flag-completing", id as u64),
                Animation::new(COMPLETE_DELAY),
                |row, delta| row.opacity(1. - 0.55 * delta),
            )
            .into_any_element()
        } else {
            row.into_any_element()
        }
    }
}

impl Render for TasksView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        // Ribbon actions (New task, Mark complete, Delete) dispatch from the focused element:
        // take focus each time the module is opened.
        if std::mem::take(&mut self.needs_focus) {
            self.focus.focus(window, cx);
        }
        if std::mem::take(&mut self.focus_new_task) {
            self.new_task
                .update(cx, |input, cx| input.focus(window, cx));
        }
        let today = Self::today();
        let store = self.state.read(cx).store();
        let cards = todo::list_task_cards(store).unwrap_or_default();
        let lists = todo::list_task_lists(store).unwrap_or_default();
        let flagged = todo::flagged_messages(store).unwrap_or_default();
        let list_missing = match self.list {
            SmartList::List(id) => !lists.iter().any(|list| list.id == id),
            _ => false,
        };
        if list_missing {
            self.list = SmartList::Tasks;
        }

        // Flagged messages already turned into a task show as that task.
        let linked: HashSet<Id> = cards
            .iter()
            .filter_map(|card| card.details.message_id)
            .collect();
        let loose_flags: Vec<Message> = flagged
            .into_iter()
            .filter(|message| !linked.contains(&message.id))
            .collect();

        let mut counts: HashMap<SmartList, usize> = HashMap::new();
        let all_lists = SmartList::BUILT_IN
            .into_iter()
            .chain(lists.iter().map(|list| SmartList::List(list.id)));
        for list in all_lists {
            let open = cards
                .iter()
                .filter(|card| !card.task.completed && card.is_in(list, today))
                .count();
            let extra = if list == SmartList::Flagged {
                loose_flags.len()
            } else {
                0
            };
            counts.insert(list, open + extra);
        }

        let (mut open, mut done): (Vec<TaskCard>, Vec<TaskCard>) = cards
            .into_iter()
            .filter(|card| card.is_in(self.list, today))
            .partition(|card| !card.task.completed);
        todo::sort_tasks(&mut open, self.sort);
        done.sort_by_key(|card| std::cmp::Reverse(card.details.completed_at));
        let show_flags = self.list == SmartList::Flagged;
        let title = self.list_title(&lists);

        // ----- open tasks (optionally grouped by due date) -----
        let mut body: Vec<AnyElement> = Vec::new();
        if self.group_by_due {
            let mut buckets: Vec<(DueBucket, Vec<&TaskCard>)> = Vec::new();
            for card in &open {
                let bucket = DueBucket::of(card.task.due, today);
                match buckets.iter_mut().find(|(existing, _)| *existing == bucket) {
                    Some((_, cards)) => cards.push(card),
                    None => buckets.push((bucket, vec![card])),
                }
            }
            buckets.sort_by_key(|(bucket, _)| *bucket);
            for (bucket, group) in buckets {
                body.push(group_label(bucket.label(), group.len(), &t).into_any_element());
                for card in group {
                    body.push(self.render_task_row(card, &lists, today, &t, cx));
                }
            }
        } else {
            for card in &open {
                body.push(self.render_task_row(card, &lists, today, &t, cx));
            }
        }
        if show_flags {
            for message in &loose_flags {
                body.push(self.render_message_row(message, &t, cx));
            }
        }

        // ----- completed -----
        if !done.is_empty() {
            let open_section = self.completed_open;
            body.push(
                div()
                    .id("completed-toggle")
                    .flex()
                    .items_center()
                    .gap(SPACE_1_5)
                    .mt(SPACE_3)
                    .px(SPACE_2)
                    .h(px(28.))
                    .w_full()
                    .rounded(RADIUS_MD)
                    .cursor_pointer()
                    .text_size(TEXT_SM)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.text_muted)
                    .hover(|s| s.bg(t.hover))
                    .child(
                        Icon::new(if open_section {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .size(ICON_SIZE_SM),
                    )
                    .child("Completed")
                    .child(
                        div()
                            .text_color(t.text_subtle)
                            .child(done.len().to_string()),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.completed_open = !this.completed_open;
                        cx.notify();
                    }))
                    .into_any_element(),
            );
            if open_section {
                for card in &done {
                    body.push(self.render_task_row(card, &lists, today, &t, cx));
                }
            }
        }

        let nothing =
            open.is_empty() && done.is_empty() && !(show_flags && !loose_flags.is_empty());
        let list_body: AnyElement = if nothing {
            let (icon, message) = match self.list {
                SmartList::MyDay => (
                    IconName::Sun,
                    "Focus on your day. Add tasks or pick some from Planned.",
                ),
                SmartList::Important => (IconName::Star, "Star tasks to see them here."),
                SmartList::Planned => (
                    IconName::CalendarDays,
                    "Tasks with a due date or reminder show up here.",
                ),
                SmartList::Flagged => (IconName::Flag, "Flag an email to turn it into a task."),
                SmartList::Tasks | SmartList::List(_) => {
                    (IconName::ListTodo, "Add your first task above.")
                }
            };
            EmptyState::new(icon, "All clear")
                .description(message)
                .into_any_element()
        } else {
            div()
                .id("task-scroll")
                .size_full()
                .overflow_y_scroll()
                .px(SPACE_6)
                .pb(SPACE_6)
                .child(div().flex().flex_col().gap(SPACE_1_5).children(body))
                .into_any_element()
        };

        let detail = match self.selected {
            Some(Selection::Task(id)) => self.render_detail(id, &lists, today, &t, cx),
            Some(Selection::Message(id)) => loose_flags
                .iter()
                .find(|message| message.id == id)
                .map(|message| self.render_message_detail(message, &t, cx)),
            None => None,
        };

        let sidebar = self.render_sidebar(&counts, &lists, &t, cx);
        let header = self.render_header(title, &t, cx);
        div()
            .id("tasks")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &actions::DeleteSelection, _, cx| {
                if let Some(Selection::Task(id)) = this.selected {
                    this.delete_task(id, cx);
                }
            }))
            .flex()
            .flex_row()
            .size_full()
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(232.))
                    .h_full()
                    .bg(t.surface)
                    .border_r_1()
                    .border_color(t.border_subtle)
                    .child(sidebar),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.))
                    .h_full()
                    .child(header)
                    .when(!show_flags, |el| {
                        el.child(
                            div().px(SPACE_6).pb(SPACE_3).child(
                                Input::new(&self.new_task).prefix(
                                    Icon::new(IconName::Plus)
                                        .size(ICON_SIZE)
                                        .text_color(t.accent),
                                ),
                            ),
                        )
                    })
                    .child(div().flex_1().min_h(px(0.)).child(list_body)),
            )
            .when_some(detail, |el, detail| {
                el.child(
                    div()
                        .flex_shrink_0()
                        .w(px(360.))
                        .h_full()
                        .bg(t.surface)
                        .border_l_1()
                        .border_color(t.border_subtle)
                        .child(detail),
                )
            })
    }
}

/// Round checkbox: empty ring, check on hover, filled when done.
fn check_circle(id: impl Into<ElementId>, done: bool, t: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(px(20.))
        .rounded(RADIUS_FULL)
        .border_1()
        .cursor_pointer()
        .when(done, |el| {
            el.bg(t.success).border_color(t.success).child(
                Icon::new(IconName::Check)
                    .size(px(12.))
                    .text_color(t.on_accent),
            )
        })
        .when(!done, |el| {
            el.border_color(t.text_subtle).child(
                div()
                    .opacity(0.)
                    .group_hover("task-row", |s| s.opacity(1.))
                    .child(
                        Icon::new(IconName::Check)
                            .size(px(12.))
                            .text_color(t.text_subtle),
                    ),
            )
        })
}

fn option_chip(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    active: bool,
    t: &Theme,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .h(px(24.))
        .px(SPACE_2)
        .rounded(RADIUS_FULL)
        .border_1()
        .cursor_pointer()
        .text_size(TEXT_SM)
        .when(active, |el| {
            el.bg(t.accent_subtle)
                .border_color(t.accent)
                .text_color(t.accent)
        })
        .when(!active, |el| {
            el.border_color(t.border)
                .text_color(t.text_muted)
                .hover(|s| s.bg(t.hover))
        })
        .child(label.into())
}

fn group_label(label: &'static str, count: usize, t: &Theme) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(SPACE_2)
        .mt(SPACE_3)
        .px(SPACE_1)
        .text_size(TEXT_SM)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(t.text_muted)
        .child(label)
        .child(div().text_color(t.text_subtle).child(count.to_string()))
}

/// Icon and colour of a built-in list.
fn smart_style(list: SmartList, t: &Theme) -> (IconName, Hsla) {
    match list {
        SmartList::MyDay => (IconName::Sun, t.warning),
        SmartList::Important => (IconName::Star, t.danger),
        SmartList::Planned => (IconName::CalendarDays, t.success),
        SmartList::Flagged => (IconName::Flag, t.danger),
        SmartList::Tasks => (IconName::House, t.accent),
        SmartList::List(_) => (IconName::ListTodo, t.accent),
    }
}

fn unflag(message_id: Id) -> Command {
    Command::SetMessageFlags {
        ids: vec![message_id],
        read: None,
        flagged: Some(false),
        pinned: None,
    }
}

/// "Today", "Tomorrow", "Yesterday", "Friday" (this week), "Mon 7 Dec" otherwise.
fn day_label(date: NaiveDate, today: NaiveDate) -> String {
    match (date - today).num_days() {
        0 => "Today".into(),
        1 => "Tomorrow".into(),
        -1 => "Yesterday".into(),
        2..=6 => date.format("%A").to_string(),
        _ if date.year() == today.year() => date.format("%a %-d %b").to_string(),
        _ => date.format("%a %-d %b %Y").to_string(),
    }
}

fn reminder_label(at: DateTime<Utc>, today: NaiveDate) -> String {
    let local = at.with_timezone(&Local);
    format!(
        "{} {}",
        day_label(local.date_naive(), today),
        local.format("%H:%M")
    )
}

/// Ribbon / command palette entry points (routed by `crate::actions`).
impl TasksView {
    /// Focuses the "Add a task" field.
    pub(crate) fn action_new_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.new_task
            .update(cx, |input, cx| input.focus(window, cx));
    }

    /// Completes the selected task (or unflags the selected message); false when none.
    pub(crate) fn action_complete(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(selection) = self.selected else {
            return false;
        };
        self.check(selection, cx);
        true
    }

    /// Deletes the selected task; false when no task is selected.
    pub(crate) fn action_delete(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(Selection::Task(id)) = self.selected else {
            return false;
        };
        self.delete_task(id, cx);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::day_label;
    use chrono::NaiveDate;

    #[test]
    fn day_labels() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 14).unwrap();
        let day = |d: u32| NaiveDate::from_ymd_opt(2026, 11, d).unwrap();
        assert_eq!(day_label(today, today), "Today");
        assert_eq!(day_label(day(15), today), "Tomorrow");
        assert_eq!(day_label(day(13), today), "Yesterday");
        assert_eq!(day_label(day(18), today), "Wednesday");
        assert_eq!(day_label(day(30), today), "Mon 30 Nov");
        assert_eq!(
            day_label(NaiveDate::from_ymd_opt(2027, 1, 4).unwrap(), today),
            "Mon 4 Jan 2027"
        );
    }
}
