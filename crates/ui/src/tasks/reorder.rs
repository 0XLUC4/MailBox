//! Drag & drop reordering of open tasks (default sort, not grouped) and task reminders
//! (desktop notification + toast when a reminder time passes while the app runs).

use std::time::Duration;

use chrono::Utc;
use gpui::*;
use mailbox_core::todo::{self, TaskSort};
use mailbox_core::{Command, Id, TaskCommand};

use super::TasksView;
use crate::components::{Icon, IconName};
use crate::theme::{RADIUS_LG, SPACE_2, SPACE_3, TEXT_SM, theme};

/// How often task reminders are checked.
const REMINDER_TICK: Duration = Duration::from_secs(20);

/// A task row being dragged.
#[derive(Clone)]
pub(super) struct DraggedTask {
    pub id: Id,
    pub title: SharedString,
}

impl Render for DraggedTask {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        div()
            .flex()
            .items_center()
            .gap(SPACE_2)
            .max_w(px(320.))
            .px(SPACE_3)
            .py(SPACE_2)
            .rounded(RADIUS_LG)
            .bg(t.elevated)
            .border_1()
            .border_color(t.accent)
            .shadow_lg()
            .text_size(TEXT_SM)
            .text_color(t.text)
            .child(
                Icon::new(IconName::GripVertical)
                    .size(px(12.))
                    .text_color(t.text_subtle),
            )
            .child(div().truncate().child(self.title.clone()))
    }
}

impl TasksView {
    /// Rows can be dragged only in the manual order (default sort, no grouping).
    pub(super) fn can_reorder(&self) -> bool {
        self.sort == TaskSort::Created && !self.group_by_due
    }

    /// Moves `moved` to the place of `target` in the open tasks of the current list.
    pub(super) fn reorder(&mut self, moved: Id, target: Id, cx: &mut Context<Self>) {
        if moved == target {
            return;
        }
        let today = Self::today();
        let mut open: Vec<_> = todo::list_task_cards(self.state.read(cx).store())
            .unwrap_or_default()
            .into_iter()
            .filter(|card| !card.task.completed && card.is_in(self.list, today))
            .collect();
        todo::sort_tasks(&mut open, TaskSort::Created);
        let (Some(from), Some(to)) = (
            open.iter().position(|card| card.id() == moved),
            open.iter().position(|card| card.id() == target),
        ) else {
            return;
        };
        let card = open.remove(from);
        open.insert(to, card);
        let saves: Vec<Command> = open
            .into_iter()
            .enumerate()
            .filter_map(|(index, mut card)| {
                let position = index as i64 + 1;
                (card.details.position != position).then(|| {
                    card.details.position = position;
                    Command::Todo(TaskCommand::SaveTask(Box::new(card)))
                })
            })
            .collect();
        if !saves.is_empty() {
            self.run(Command::Batch(saves), cx);
        }
    }

    /// Notifies reminders that came due since the last check (not those before start-up:
    /// the user saw them in the list).
    pub(super) fn start_reminders(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let mut since = Utc::now();
            loop {
                cx.background_executor().timer(REMINDER_TICK).await;
                let now = Utc::now();
                let alive = this.update(cx, |this, cx| {
                    // Past midnight: My Day, Overdue and the counts change.
                    if since.with_timezone(&chrono::Local).date_naive()
                        != now.with_timezone(&chrono::Local).date_naive()
                    {
                        cx.notify();
                    }
                    this.fire_reminders(since, now, cx)
                });
                match alive {
                    Ok(true) => since = now,
                    // A failed read is retried at the next tick (nothing is skipped).
                    Ok(false) => {}
                    Err(_) => break,
                }
            }
        })
    }

    fn fire_reminders(
        &mut self,
        since: chrono::DateTime<Utc>,
        now: chrono::DateTime<Utc>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Ok(cards) = todo::list_task_cards(self.state.read(cx).store()) else {
            return false;
        };
        let due: Vec<_> = cards
            .into_iter()
            .filter(|card| !card.task.completed)
            .filter(|card| {
                card.details
                    .reminder
                    .is_some_and(|at| at > since && at <= now)
            })
            .collect();
        for card in due {
            let title = card.task.title.clone();
            if let Some(at) = card.details.reminder {
                crate::notifications::reminder(&title, at, cx);
            }
            let message = crate::settings::tr(cx, "Reminder: {}").replace("{}", &title);
            self.state
                .update(cx, |state, cx| state.notify_info(message, cx));
        }
        true
    }
}
