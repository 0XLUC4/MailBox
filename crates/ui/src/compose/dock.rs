//! Docked compose editors, overlaid on the right of the main window content, one tab each.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::Id;

use super::{ComposeGlobal, ComposeView};
use crate::components::{Icon, IconButton, IconName};
use crate::settings::tr;
use crate::theme::{
    ICON_SIZE_SM, ICON_SIZE_XS, RADIUS_MD, SPACE_1, SPACE_2, TEXT_SM, Theme, theme,
};

/// Width of the docked editor (it never exceeds the content area).
const DOCK_WIDTH: Pixels = px(760.);

pub struct ComposeDock {
    views: Vec<Entity<ComposeView>>,
    active: usize,
    _observers: Vec<Subscription>,
}

impl ComposeDock {
    fn new() -> Self {
        Self {
            views: Vec::new(),
            active: 0,
            _observers: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.views.is_empty()
    }

    fn push(&mut self, view: Entity<ComposeView>, cx: &mut Context<Self>) {
        self.views.push(view);
        self.active = self.views.len() - 1;
        self.observe_views(cx);
        cx.notify();
    }

    fn remove(&mut self, view: &Entity<ComposeView>, cx: &mut Context<Self>) {
        let Some(index) = self.views.iter().position(|other| other == view) else {
            return;
        };
        self.views.remove(index);
        if self.active > index || self.active >= self.views.len() {
            self.active = self.active.saturating_sub(1);
        }
        self.observe_views(cx);
        cx.notify();
    }

    /// Re-renders tab titles when an editor changes.
    fn observe_views(&mut self, cx: &mut Context<Self>) {
        self._observers = self
            .views
            .iter()
            .map(|view| cx.observe(view, |_, _, cx| cx.notify()))
            .collect();
    }

    fn render_tabs(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let hover = t.hover;
        let tabs = self
            .views
            .iter()
            .enumerate()
            .map(|(index, view)| {
                let title = view.read(cx).title(cx);
                let is_active = index == self.active;
                let target = view.clone();
                div()
                    .id(("compose-tab", index as u64))
                    .flex()
                    .flex_row()
                    .flex_shrink_0()
                    .items_center()
                    .gap(SPACE_1)
                    .h(px(28.))
                    .max_w(px(220.))
                    .pl(SPACE_2)
                    .pr(SPACE_1)
                    .rounded(RADIUS_MD)
                    .text_size(TEXT_SM)
                    .cursor_pointer()
                    .when(is_active, |el| el.bg(t.background).text_color(t.text))
                    .when(!is_active, |el| {
                        el.text_color(t.text_muted)
                            .hover(move |style| style.bg(hover))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.active = index;
                        cx.notify();
                    }))
                    .child(Icon::new(IconName::SquarePen).size(ICON_SIZE_SM))
                    .child(div().min_w(px(0.)).truncate().child(title))
                    .child(
                        IconButton::new(("compose-tab-close", index as u64), IconName::X)
                            .size(px(20.))
                            .icon_size(ICON_SIZE_XS)
                            .tooltip(tr(cx, "Close (keeps the draft)"))
                            .on_click(move |_, window, cx| {
                                target.update(cx, |view, cx| view.close(window, cx));
                            }),
                    )
            })
            .collect::<Vec<_>>();
        div()
            .id("compose-tabs")
            .flex()
            .flex_row()
            .flex_shrink_0()
            .items_center()
            .gap(SPACE_1)
            .h(px(40.))
            .px(SPACE_2)
            .overflow_x_scroll()
            .bg(t.surface)
            .border_b_1()
            .border_color(t.border_subtle)
            .children(tabs)
    }
}

impl Render for ComposeDock {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let active = self.views.get(self.active).cloned();
        let tabs = (self.views.len() > 1).then(|| self.render_tabs(&t, cx));
        div()
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .w(DOCK_WIDTH)
            .max_w(relative(1.))
            .flex()
            .flex_col()
            .bg(t.background)
            .border_l_1()
            .border_color(t.border)
            .shadow_lg()
            .occlude()
            .children(tabs)
            .children(active)
    }
}

fn dock_entity(cx: &App) -> Option<Entity<ComposeDock>> {
    cx.try_global::<ComposeGlobal>()
        .and_then(|global| global.dock.clone())
}

/// The window hosting the dock (where undo-send toasts and re-opened drafts go).
pub(super) fn main_window(cx: &App) -> Option<AnyWindowHandle> {
    cx.try_global::<ComposeGlobal>()
        .and_then(|global| global.main_window)
}

pub(super) fn add(view: Entity<ComposeView>, window: &mut Window, cx: &mut App) {
    let dock = match dock_entity(cx) {
        Some(dock) => dock,
        None => {
            let dock = cx.new(|_| ComposeDock::new());
            let global = cx.default_global::<ComposeGlobal>();
            global.dock = Some(dock.clone());
            global.main_window = Some(window.window_handle());
            dock
        }
    };
    dock.update(cx, |dock, cx| dock.push(view, cx));
}

pub(super) fn remove(view: &Entity<ComposeView>, cx: &mut App) {
    if let Some(dock) = dock_entity(cx) {
        dock.update(cx, |dock, cx| dock.remove(view, cx));
    }
}

/// Brings an already open editor of draft `id` to the front. Returns whether one was found.
pub(super) fn activate_draft(id: Id, cx: &mut App) -> bool {
    let Some(dock) = dock_entity(cx) else {
        return false;
    };
    let index = dock
        .read(cx)
        .views
        .iter()
        .position(|view| view.read(cx).message_id() == id);
    if let Some(index) = index {
        dock.update(cx, |dock, cx| {
            dock.active = index;
            cx.notify();
        });
    }
    index.is_some()
}
