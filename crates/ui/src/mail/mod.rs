//! Mail module: folder pane | message list | reading pane (right, bottom or off).
//! Owner: mail agent (this folder). Selection lives in `AppState` so panes stay decoupled;
//! layout preferences shared by the panes live in [`MailPrefs`].
//!
//! Compose integration: Reply / Reply all / Forward / mailto links call
//! [`crate::compose::open_compose`] (see [`ops::compose_from`]).

mod folder_pane;
pub(crate) mod html;
mod list_model;
mod message_list;
mod message_menu;
mod message_window;
mod ops;
mod reading_pane;
mod remote_images;
#[cfg(any(windows, target_os = "macos"))]
mod webview;

pub use folder_pane::FolderPaneView;
pub use message_list::MessageListView;
pub use message_menu::{open_category_dialog, open_move_dialog};
pub use message_window::open as open_message_window;
pub use ops::{ComposeKind, compose_from};
pub use reading_pane::{PaneSource, ReadingPaneView};

use gpui::*;
use mailbox_core::Id;

use crate::app_state::AppState;
use crate::components::{Icon, IconName};
use crate::theme::{ICON_SIZE_SM, RADIUS_LG, SPACE_1_5, SPACE_2, SPACE_3, TEXT_SM, TEXT_XS, theme};

gpui::actions!(
    mail,
    [
        SelectNext,
        SelectPrevious,
        ExtendNext,
        ExtendPrevious,
        SelectAllMessages,
        ClearSelection,
        DeleteSelected,
        ArchiveSelected,
        MarkRead,
        MarkUnread,
        ToggleFlag,
        OpenSelected,
        ExpandThread,
        CollapseThread
    ]
);

/// Message-list shortcuts (Outlook-like), active while the list has focus.
fn bind_keys(cx: &mut App) {
    let context = Some("MessageList");
    let mac = cfg!(target_os = "macos");
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("up", SelectPrevious, context),
        KeyBinding::new("shift-down", ExtendNext, context),
        KeyBinding::new("shift-up", ExtendPrevious, context),
        KeyBinding::new(
            if mac { "cmd-a" } else { "ctrl-a" },
            SelectAllMessages,
            context,
        ),
        KeyBinding::new("escape", ClearSelection, context),
        KeyBinding::new("delete", DeleteSelected, context),
        KeyBinding::new(
            if mac { "backspace" } else { "shift-delete" },
            DeleteSelected,
            context,
        ),
        KeyBinding::new(
            if mac { "cmd-e" } else { "backspace" },
            ArchiveSelected,
            context,
        ),
        KeyBinding::new("ctrl-q", MarkRead, context),
        KeyBinding::new("ctrl-u", MarkUnread, context),
        KeyBinding::new("insert", ToggleFlag, context),
        KeyBinding::new("enter", OpenSelected, context),
        KeyBinding::new("right", ExpandThread, context),
        KeyBinding::new("left", CollapseThread, context),
    ]);
}

pub use mailbox_core::settings::ReadingPane as ReadingPanePosition;

/// Reading pane position, from the stored settings (View tab, list toolbar, Settings).
pub fn pane_position(cx: &App) -> ReadingPanePosition {
    crate::settings::prefs(cx).reading_pane
}

/// Layout state shared by the mail panes that is not worth persisting.
#[derive(Debug, Default)]
pub struct MailPrefs {
    pub folders_collapsed: bool,
}

/// Payload of a message drag (onto a folder: move, Ctrl: copy).
#[derive(Debug, Clone)]
pub struct DraggedMessages {
    pub ids: Vec<Id>,
    pub label: SharedString,
}

impl Render for DraggedMessages {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(SPACE_2)
            .max_w(px(300.))
            .px(SPACE_3)
            .py(SPACE_1_5)
            .rounded(RADIUS_LG)
            .bg(t.elevated)
            .border_1()
            .border_color(t.accent)
            .shadow_lg()
            .text_size(TEXT_SM)
            .text_color(t.text)
            .child(
                Icon::new(if self.ids.len() > 1 {
                    IconName::Mails
                } else {
                    IconName::Mail
                })
                .size(ICON_SIZE_SM)
                .text_color(t.accent),
            )
            .child(div().min_w(px(0.)).truncate().child(self.label.clone()))
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(TEXT_XS)
                    .text_color(t.text_subtle)
                    .child("Ctrl to copy"),
            )
    }
}

pub struct MailView {
    prefs: Entity<MailPrefs>,
    folder_pane: Entity<FolderPaneView>,
    message_list: Entity<MessageListView>,
    reading_pane: Entity<ReadingPaneView>,
    _subscription: Subscription,
}

impl MailView {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        bind_keys(cx);
        remote_images::install(cx);
        let prefs = cx.new(|_| MailPrefs::default());
        let subscription = cx.observe(&prefs, |_, _, cx| cx.notify());
        Self {
            folder_pane: cx.new(|cx| FolderPaneView::new(state.clone(), prefs.clone(), window, cx)),
            message_list: cx
                .new(|cx| MessageListView::new(state.clone(), prefs.clone(), window, cx)),
            reading_pane: cx
                .new(|cx| ReadingPaneView::new(state, PaneSource::Selected, window, cx)),
            prefs,
            _subscription: subscription,
        }
    }
}

impl Render for MailView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let prefs = self.prefs.read(cx);
        let position = pane_position(cx);
        let folders_width = if prefs.folders_collapsed {
            px(48.)
        } else {
            px(244.)
        };

        let list = div()
            .flex_shrink_0()
            .min_w(px(0.))
            .min_h(px(0.))
            .bg(t.background)
            .child(self.message_list.clone());
        let reading = div()
            .flex_1()
            .min_w(px(0.))
            .min_h(px(0.))
            .child(self.reading_pane.clone());
        let content = match position {
            ReadingPanePosition::Right => div()
                .flex()
                .flex_row()
                .flex_1()
                .min_w(px(0.))
                .h_full()
                .child(
                    list.w(px(400.))
                        .h_full()
                        .border_r_1()
                        .border_color(t.border_subtle),
                )
                .child(reading.h_full()),
            ReadingPanePosition::Bottom => div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(0.))
                .h_full()
                .child(
                    list.w_full()
                        .h(relative(0.45))
                        .border_b_1()
                        .border_color(t.border_subtle),
                )
                .child(reading.w_full()),
            ReadingPanePosition::Off => div()
                .flex()
                .flex_1()
                .min_w(px(0.))
                .h_full()
                .child(list.flex_1().h_full()),
        };

        div()
            .flex()
            .flex_row()
            .size_full()
            .child(
                div()
                    .flex_shrink_0()
                    .w(folders_width)
                    .h_full()
                    .bg(t.surface)
                    .border_r_1()
                    .border_color(t.border_subtle)
                    .child(self.folder_pane.clone()),
            )
            .child(content)
    }
}
