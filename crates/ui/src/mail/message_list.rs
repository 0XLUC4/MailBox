//! Messages of the selected folder (or search results). Minimal reference implementation:
//! the mail agent adds date grouping, threading, multi-select and virtualization.

use chrono::{DateTime, Local, Utc};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::{Command, Message};

use crate::app_state::AppState;
use crate::components::{Avatar, EmptyState, Icon, IconName};
use crate::theme::{ICON_SIZE_SM, SPACE_0_5, SPACE_1, SPACE_2, SPACE_3, TEXT_MD, TEXT_SM, theme};

/// Max search results shown.
const SEARCH_LIMIT: usize = 200;

pub struct MessageListView {
    state: Entity<AppState>,
    _subscription: Subscription,
}

impl MessageListView {
    pub fn new(state: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&state, |_, _, cx| cx.notify());
        Self {
            state,
            _subscription: subscription,
        }
    }

    /// Opens a message and marks it read through a command (undoable).
    fn open(&mut self, message: &Message, cx: &mut Context<Self>) {
        let id = message.id;
        let unread = !message.flags.read;
        self.state.update(cx, |state, cx| {
            state.select_message(Some(id), cx);
            if unread {
                state.run(
                    Command::SetMessageFlags {
                        ids: vec![id],
                        read: Some(true),
                        flagged: None,
                        pinned: None,
                    },
                    cx,
                );
            }
        });
    }
}

/// "14:05" today, "Mon" this week, "03 Nov" otherwise.
pub fn short_date(date: DateTime<Utc>) -> String {
    let local = date.with_timezone(&Local);
    let now = Local::now();
    if local.date_naive() == now.date_naive() {
        local.format("%H:%M").to_string()
    } else if (now - local).num_days() < 7 {
        local.format("%a").to_string()
    } else {
        local.format("%d %b").to_string()
    }
}

impl Render for MessageListView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let state = self.state.read(cx);
        let query = state.search_query().trim().to_owned();
        let selected = state.selected_message();
        let messages = if !query.is_empty() {
            state
                .store()
                .search_messages(&query, SEARCH_LIMIT)
                .unwrap_or_default()
        } else if let Some(folder) = state.selected_folder() {
            state.store().list_messages(folder).unwrap_or_default()
        } else {
            Vec::new()
        };

        if messages.is_empty() {
            let (title, description) = if query.is_empty() {
                ("No messages", "This folder is empty.")
            } else {
                ("No results", "Try another search.")
            };
            return div()
                .id("message-list")
                .size_full()
                .child(EmptyState::new(IconName::Inbox, title).description(description));
        }

        let rows = messages.into_iter().map(|message| {
            let is_selected = selected == Some(message.id);
            let unread = !message.flags.read;
            let row = div()
                .id(("message", message.id as u64))
                .flex()
                .flex_row()
                .gap(SPACE_3)
                .px(SPACE_3)
                .py(SPACE_2)
                .border_b_1()
                .border_color(t.border_subtle)
                .cursor_pointer()
                .when(is_selected, |el| el.bg(t.accent_subtle))
                .when(!is_selected, |el| el.hover(|s| s.bg(t.hover)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(SPACE_1)
                        .child(Avatar::new(message.from.display().to_owned()).size(px(32.)))
                        .when(unread, |el| {
                            el.child(div().size(px(7.)).rounded(px(4.)).bg(t.accent))
                        }),
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
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(SPACE_2)
                                .text_size(TEXT_MD)
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .truncate()
                                        .when(unread, |el| el.font_weight(FontWeight::SEMIBOLD))
                                        .child(message.from.display().to_owned()),
                                )
                                .when(message.flags.flagged, |el| {
                                    el.child(
                                        Icon::new(IconName::Flag)
                                            .size(ICON_SIZE_SM)
                                            .text_color(t.danger),
                                    )
                                })
                                .when(!message.attachments.is_empty(), |el| {
                                    el.child(
                                        Icon::new(IconName::Paperclip)
                                            .size(ICON_SIZE_SM)
                                            .text_color(t.text_subtle),
                                    )
                                })
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_size(TEXT_SM)
                                        .text_color(if unread { t.accent } else { t.text_subtle })
                                        .child(short_date(message.date)),
                                ),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(TEXT_MD)
                                .when(unread, |el| el.font_weight(FontWeight::MEDIUM))
                                .child(message.subject.clone()),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(TEXT_SM)
                                .text_color(t.text_muted)
                                .child(message.preview.clone()),
                        ),
                );
            row.on_click(cx.listener(move |this, _, _, cx| this.open(&message, cx)))
        });

        div()
            .id("message-list")
            .size_full()
            .overflow_y_scroll()
            .children(rows.collect::<Vec<_>>())
    }
}
