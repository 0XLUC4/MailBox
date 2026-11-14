//! Selected message: header, actions and plain-text body. Minimal reference implementation.

use chrono::Local;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::{Command, FolderRole, Message};

use crate::app_state::AppState;
use crate::components::{
    Avatar, Badge, EmptyState, IconButton, IconName, Toolbar, avatar_color, toolbar_separator,
};
use crate::theme::{SPACE_1, SPACE_2, SPACE_3, SPACE_6, TEXT_MD, TEXT_SM, TEXT_XL, theme};

pub struct ReadingPaneView {
    state: Entity<AppState>,
    _subscription: Subscription,
}

impl ReadingPaneView {
    pub fn new(state: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&state, |_, _, cx| cx.notify());
        Self {
            state,
            _subscription: subscription,
        }
    }

    fn run(&mut self, command: Command, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            state.run(command, cx);
        });
    }

    /// Soft delete: move to the account's Trash folder.
    fn trash(&mut self, message: &Message, cx: &mut Context<Self>) {
        let trash = self
            .state
            .read(cx)
            .store()
            .folder_by_role(message.account_id, FolderRole::Trash);
        let id = message.id;
        self.state.update(cx, |state, cx| match trash {
            Ok(Some(folder)) => {
                if state.run(
                    Command::MoveMessages {
                        ids: vec![id],
                        folder_id: folder.id,
                    },
                    cx,
                ) {
                    state.select_message(None, cx);
                }
            }
            Ok(None) => state.report_error("This account has no Trash folder", cx),
            Err(err) => state.report_error(err, cx),
        });
    }

    fn toolbar(&self, message: &Message, cx: &mut Context<Self>) -> Toolbar {
        let flagged = message.flags.flagged;
        let read = message.flags.read;
        let id = message.id;
        let to_trash = message.clone();
        Toolbar::new()
            .child(IconButton::new("reply", IconName::Reply).tooltip("Reply"))
            .child(IconButton::new("reply-all", IconName::ReplyAll).tooltip("Reply all"))
            .child(IconButton::new("forward", IconName::Forward).tooltip("Forward"))
            .child(toolbar_separator(cx))
            .child(
                IconButton::new("flag", IconName::Flag)
                    .active(flagged)
                    .tooltip(if flagged { "Unflag" } else { "Flag" })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.run(
                            Command::SetMessageFlags {
                                ids: vec![id],
                                read: None,
                                flagged: Some(!flagged),
                                pinned: None,
                            },
                            cx,
                        );
                    })),
            )
            .child(
                IconButton::new(
                    "unread",
                    if read {
                        IconName::Mail
                    } else {
                        IconName::MailOpen
                    },
                )
                .tooltip(if read {
                    "Mark as unread"
                } else {
                    "Mark as read"
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.run(
                        Command::SetMessageFlags {
                            ids: vec![id],
                            read: Some(!read),
                            flagged: None,
                            pinned: None,
                        },
                        cx,
                    );
                })),
            )
            .child(
                IconButton::new("trash", IconName::Trash)
                    .tooltip("Delete")
                    .on_click(cx.listener(move |this, _, _, cx| this.trash(&to_trash, cx))),
            )
    }
}

impl Render for ReadingPaneView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let state = self.state.read(cx);
        let message = state
            .selected_message()
            .and_then(|id| state.store().get::<Message>(id).ok().flatten());
        let Some(message) = message else {
            return div().size_full().child(
                EmptyState::new(IconName::MailOpen, "No message selected")
                    .description("Pick a message in the list to read it here."),
            );
        };

        let recipients = message
            .to
            .iter()
            .map(|a| a.display().to_owned())
            .collect::<Vec<_>>()
            .join(", ");
        let date = message
            .date
            .with_timezone(&Local)
            .format("%A %d %B %Y, %H:%M")
            .to_string();
        let toolbar = self.toolbar(&message, cx);

        div().size_full().flex().flex_col().child(toolbar).child(
            div()
                .id("reading-body")
                .flex_1()
                .min_h(px(0.))
                .overflow_y_scroll()
                .px(SPACE_6)
                .py(SPACE_3)
                .flex()
                .flex_col()
                .gap(SPACE_3)
                .child(
                    div()
                        .text_size(TEXT_XL)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(message.subject.clone()),
                )
                .when(!message.categories.is_empty(), |el| {
                    el.child(
                        div().flex().flex_row().gap(SPACE_1).children(
                            message
                                .categories
                                .iter()
                                .map(|name| Badge::new(name.clone()).color(avatar_color(name))),
                        ),
                    )
                })
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(SPACE_3)
                        .child(Avatar::new(message.from.display().to_owned()).size(px(40.)))
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
                                        .gap(SPACE_2)
                                        .text_size(TEXT_MD)
                                        .child(
                                            div()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child(message.from.display().to_owned()),
                                        )
                                        .child(
                                            div()
                                                .text_color(t.text_muted)
                                                .child(format!("<{}>", message.from.email)),
                                        ),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(TEXT_SM)
                                        .text_color(t.text_muted)
                                        .child(format!("To: {recipients}")),
                                ),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_size(TEXT_SM)
                                .text_color(t.text_subtle)
                                .child(date),
                        ),
                )
                .child(div().h(px(1.)).bg(t.border_subtle))
                .child(
                    div()
                        .text_size(TEXT_MD)
                        .line_height(px(21.))
                        .child(message.body_text.clone()),
                ),
        )
    }
}
