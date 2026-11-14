//! Mail module: folder pane | message list | reading pane.
//! Owner: mail agent (this folder). Selection lives in `AppState` so panes stay decoupled.

mod folder_pane;
mod message_list;
mod reading_pane;

pub use folder_pane::FolderPaneView;
pub use message_list::MessageListView;
pub use reading_pane::ReadingPaneView;

use gpui::*;

use crate::app_state::AppState;
use crate::theme::theme;

pub struct MailView {
    folder_pane: Entity<FolderPaneView>,
    message_list: Entity<MessageListView>,
    reading_pane: Entity<ReadingPaneView>,
}

impl MailView {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            folder_pane: cx.new(|cx| FolderPaneView::new(state.clone(), window, cx)),
            message_list: cx.new(|cx| MessageListView::new(state.clone(), window, cx)),
            reading_pane: cx.new(|cx| ReadingPaneView::new(state, window, cx)),
        }
    }
}

impl Render for MailView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        div()
            .flex()
            .flex_row()
            .size_full()
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(240.))
                    .h_full()
                    .bg(t.surface)
                    .border_r_1()
                    .border_color(t.border_subtle)
                    .child(self.folder_pane.clone()),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(380.))
                    .h_full()
                    .border_r_1()
                    .border_color(t.border_subtle)
                    .child(self.message_list.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .h_full()
                    .child(self.reading_pane.clone()),
            )
    }
}
