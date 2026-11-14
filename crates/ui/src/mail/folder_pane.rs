//! Folder tree per account with unread counts. Minimal reference implementation.

use std::collections::HashMap;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::{Folder, FolderRole, Id};

use crate::app_state::AppState;
use crate::components::{Badge, Icon, IconName};
use crate::theme::{ICON_SIZE, RADIUS_MD, SPACE_1, SPACE_2, SPACE_3, TEXT_MD, TEXT_XS, hex, theme};

pub struct FolderPaneView {
    state: Entity<AppState>,
    _subscription: Subscription,
}

impl FolderPaneView {
    pub fn new(state: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&state, |_, _, cx| cx.notify());
        Self {
            state,
            _subscription: subscription,
        }
    }
}

fn role_icon(role: FolderRole) -> IconName {
    match role {
        FolderRole::Inbox => IconName::Inbox,
        FolderRole::Sent => IconName::Send,
        FolderRole::Drafts => IconName::SquarePen,
        FolderRole::Archive => IconName::Archive,
        FolderRole::Trash => IconName::Trash,
        FolderRole::Junk => IconName::CircleAlert,
        FolderRole::Custom => IconName::Folder,
    }
}

/// Depth-first order with nesting depth, siblings kept in store order.
fn tree(folders: &[Folder]) -> Vec<(usize, &Folder)> {
    let mut children: HashMap<Option<Id>, Vec<&Folder>> = HashMap::new();
    for folder in folders {
        children.entry(folder.parent_id).or_default().push(folder);
    }
    let mut ordered = Vec::with_capacity(folders.len());
    let mut stack: Vec<(usize, &Folder)> = children
        .get(&None)
        .into_iter()
        .flatten()
        .rev()
        .map(|f| (0, *f))
        .collect();
    while let Some((depth, folder)) = stack.pop() {
        ordered.push((depth, folder));
        if let Some(kids) = children.get(&Some(folder.id)) {
            stack.extend(kids.iter().rev().map(|kid| (depth + 1, *kid)));
        }
    }
    ordered
}

impl Render for FolderPaneView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let state = self.state.read(cx);
        let store = state.store();
        let selected = state.selected_folder();
        // Render-time reads: errors just show an empty pane (a toast would fire every frame).
        let accounts = store.list_accounts().unwrap_or_default();

        let mut sections = Vec::new();
        for account in &accounts {
            let folders = store.list_folders(account.id).unwrap_or_default();
            let mut rows = Vec::new();
            for (depth, folder) in tree(&folders) {
                let unread = store.unread_count(folder.id).unwrap_or(0);
                let is_selected = selected == Some(folder.id);
                let folder_id = folder.id;
                let state_handle = self.state.clone();
                rows.push(
                    div()
                        .id(("folder", folder.id as u64))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(SPACE_2)
                        .h(px(30.))
                        .pl(px(8. + depth as f32 * 14.))
                        .pr(SPACE_2)
                        .rounded(RADIUS_MD)
                        .cursor_pointer()
                        .text_size(TEXT_MD)
                        .when(is_selected, |el| {
                            el.bg(t.accent_subtle)
                                .text_color(t.accent)
                                .font_weight(FontWeight::MEDIUM)
                        })
                        .when(!is_selected, |el| {
                            el.text_color(t.text).hover(|s| s.bg(t.hover))
                        })
                        .child(
                            Icon::new(role_icon(folder.role))
                                .size(ICON_SIZE)
                                .text_color(if is_selected { t.accent } else { t.text_muted }),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .truncate()
                                .child(folder.name.clone()),
                        )
                        .when(unread > 0, |el| {
                            el.child(if is_selected {
                                Badge::count(unread).accent()
                            } else {
                                Badge::count(unread)
                            })
                        })
                        .on_click(move |_, _, cx| {
                            state_handle
                                .update(cx, |state, cx| state.select_folder(Some(folder_id), cx));
                        }),
                );
            }
            sections.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(SPACE_2)
                            .px(SPACE_2)
                            .pt(SPACE_3)
                            .pb(SPACE_1)
                            .text_size(TEXT_XS)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.text_subtle)
                            .child(div().size(px(8.)).rounded(px(4.)).bg(hex(account.color)))
                            .child(account.name.to_uppercase()),
                    )
                    .children(rows),
            );
        }

        div()
            .id("folder-pane")
            .size_full()
            .overflow_y_scroll()
            .p(SPACE_2)
            .children(sections)
    }
}
