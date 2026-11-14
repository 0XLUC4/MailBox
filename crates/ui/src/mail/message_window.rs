//! A message in its own window (double-click in the list, or "Open in new window").

use gpui::*;
use mailbox_core::{Id, Message};

use super::reading_pane::{PaneSource, ReadingPaneView};
use crate::app_state::AppState;
use crate::theme::{TEXT_MD, theme};

struct MessageWindow {
    pane: Entity<ReadingPaneView>,
}

impl Render for MessageWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        div()
            .size_full()
            .bg(t.background)
            .text_color(t.text)
            .text_size(TEXT_MD)
            .child(self.pane.clone())
    }
}

/// Opens message `id` in a new window; failures show a toast.
pub fn open(state: Entity<AppState>, id: Id, cx: &mut App) {
    let title = state
        .read(cx)
        .store()
        .get::<Message>(id)
        .ok()
        .flatten()
        .map(|message| message.subject)
        .filter(|subject| !subject.is_empty())
        .unwrap_or_else(|| "Message".into());
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(860.), px(760.)), cx)),
        window_min_size: Some(size(px(480.), px(360.))),
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let pane_state = state.clone();
    let opened = gpui::open_window(options, cx, move |window, cx| {
        let pane = cx.new(|cx| ReadingPaneView::new(pane_state, PaneSource::Fixed(id), window, cx));
        cx.new(|_| MessageWindow { pane })
    });
    if let Err(err) = opened {
        state.update(cx, |state, cx| {
            state.report_error(format!("Couldn't open the message window: {err}"), cx)
        });
    }
}
