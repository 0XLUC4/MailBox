//! Folder pane: Favorites, then each account with its nested, collapsible folders.
//! Unread badges, drop target for dragged messages (Ctrl = copy), folder create/rename/delete,
//! and a compact rail when collapsed.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::component::WindowExt as _;
use gpui::component::input::{Input, InputEvent, InputState};
use gpui::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::{Account, Folder, FolderRole, Id, mail_ops};

use super::{DraggedMessages, MailPrefs, ops};
use crate::app_state::AppState;
use crate::components::{Badge, Button, Icon, IconButton, IconName};
use crate::theme::{
    ICON_SIZE, ICON_SIZE_SM, RADIUS_MD, SPACE_1, SPACE_2, SPACE_3, TEXT_MD, TEXT_XS, Theme, theme,
};

/// Icon for a folder: special folders by role, a few well-known custom names.
pub fn folder_icon(folder: &Folder) -> IconName {
    match folder.role {
        FolderRole::Inbox => IconName::Inbox,
        FolderRole::Sent => IconName::Send,
        FolderRole::Drafts => IconName::SquarePen,
        FolderRole::Archive => IconName::Archive,
        FolderRole::Trash => IconName::Trash,
        FolderRole::Junk => IconName::OctagonAlert,
        FolderRole::Custom if folder.name.eq_ignore_ascii_case("outbox") => IconName::Clock,
        FolderRole::Custom => IconName::Folder,
    }
}

/// Display name: special folders use the familiar names.
pub fn folder_name(folder: &Folder) -> String {
    match folder.role {
        FolderRole::Trash => "Deleted Items".into(),
        FolderRole::Sent => "Sent Items".into(),
        _ => folder.name.clone(),
    }
}

/// Depth-first order with nesting depth, siblings kept in store order.
pub fn folder_tree(folders: &[Folder]) -> Vec<(usize, &Folder)> {
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

type OnSubmit = dyn Fn(String, &mut Window, &mut App) -> bool;

/// Opens a small dialog asking for a name. `on_submit` returns whether to close.
pub fn prompt_text(
    title: &'static str,
    ok_label: &'static str,
    initial: String,
    window: &mut Window,
    cx: &mut App,
    on_submit: impl Fn(String, &mut Window, &mut App) -> bool + 'static,
) {
    let input = cx.new(|cx| InputState::new(window, cx).default_value(initial));
    let on_submit: Rc<OnSubmit> = Rc::new(on_submit);
    let enter_submit = on_submit.clone();
    let subscription =
        window.subscribe(&input, cx, move |input, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                let value = input.read(cx).value().to_string();
                if enter_submit(value, window, cx) {
                    window.close_dialog(cx);
                }
            }
        });
    let initial_focus = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        let _keep_alive = &subscription;
        if initial_focus.replace(false) {
            let handle = input.focus_handle(cx);
            handle.focus(window, cx);
        }
        let ok_input = input.clone();
        let ok_submit = on_submit.clone();
        dialog
            .title(title)
            .w(px(380.))
            .child(div().py(SPACE_2).child(Input::new(&input)))
            .footer(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(SPACE_2)
                    .w_full()
                    .child(
                        Button::new("prompt-cancel")
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(Button::new("prompt-ok").primary().label(ok_label).on_click(
                        move |_, window, cx| {
                            let value = ok_input.read(cx).value().to_string();
                            if ok_submit(value, window, cx) {
                                window.close_dialog(cx);
                            }
                        },
                    )),
            )
    });
}

pub struct FolderPaneView {
    state: Entity<AppState>,
    prefs: Entity<MailPrefs>,
    /// Folders whose children are hidden.
    collapsed: HashSet<Id>,
    collapsed_accounts: HashSet<Id>,
    favorites_collapsed: bool,
    /// `None` until first render, then each account's Inbox by default.
    favorites: Option<Vec<Id>>,
    _subscriptions: Vec<Subscription>,
}

/// Per-render data for one folder row.
struct RowInfo {
    count: i64,
    selected: bool,
}

impl FolderPaneView {
    pub fn new(
        state: Entity<AppState>,
        prefs: Entity<MailPrefs>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = vec![
            cx.observe(&state, |_, _, cx| cx.notify()),
            cx.observe(&prefs, |_, _, cx| cx.notify()),
        ];
        Self {
            state,
            prefs,
            collapsed: HashSet::new(),
            collapsed_accounts: HashSet::new(),
            favorites_collapsed: false,
            favorites: None,
            _subscriptions: subscriptions,
        }
    }

    fn toggle_favorite(&mut self, folder_id: Id, cx: &mut Context<Self>) {
        let favorites = self.favorites.get_or_insert_with(Vec::new);
        if let Some(index) = favorites.iter().position(|&id| id == folder_id) {
            favorites.remove(index);
        } else {
            favorites.push(folder_id);
        }
        cx.notify();
    }

    fn set_collapsed(prefs: &Entity<MailPrefs>, collapsed: bool, cx: &mut App) {
        prefs.update(cx, |prefs, cx| {
            prefs.folders_collapsed = collapsed;
            cx.notify();
        });
    }

    fn new_folder_dialog(
        state: Entity<AppState>,
        account_id: Id,
        parent_id: Option<Id>,
        window: &mut Window,
        cx: &mut App,
    ) {
        prompt_text(
            "New folder",
            "Create",
            String::new(),
            window,
            cx,
            move |name, _, cx| {
                let command =
                    mail_ops::new_folder(state.read(cx).store(), account_id, parent_id, &name);
                ops::run(&state, command, cx)
            },
        );
    }

    fn rename_dialog(state: Entity<AppState>, folder: &Folder, window: &mut Window, cx: &mut App) {
        let folder_id = folder.id;
        prompt_text(
            "Rename folder",
            "Rename",
            folder.name.clone(),
            window,
            cx,
            move |name, _, cx| {
                let command = mail_ops::rename_folder(state.read(cx).store(), folder_id, &name);
                ops::run(&state, command, cx)
            },
        );
    }

    fn delete_dialog(state: Entity<AppState>, folder: &Folder, window: &mut Window, cx: &mut App) {
        let folder_id = folder.id;
        let name = folder.name.clone();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let state = state.clone();
            alert
                .title(format!("Delete \"{name}\"?"))
                .description("Its subfolders are deleted too and its messages move to Deleted Items. You can undo this.")
                .confirm()
                .ok_text("Delete")
                .ok_variant(gpui::component::button::ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    let command = mail_ops::delete_folder(state.read(cx).store(), folder_id);
                    if ops::run(&state, command, cx) {
                        state.update(cx, |state, cx| {
                            if state.selected_folder() == Some(folder_id) {
                                state.select_folder(None, cx);
                            }
                        });
                    }
                    true
                })
        });
    }

    /// "Move folder": the account's folders outside the moved subtree, plus the top level.
    fn move_dialog(state: Entity<AppState>, folder: &Folder, window: &mut Window, cx: &mut App) {
        let folder_id = folder.id;
        let current_parent = folder.parent_id;
        let folders = state
            .read(cx)
            .store()
            .list_folders(folder.account_id)
            .unwrap_or_default();
        // The folder and its descendants are not valid targets.
        let mut excluded = HashSet::from([folder_id]);
        for (_, candidate) in folder_tree(&folders) {
            if candidate
                .parent_id
                .is_some_and(|parent| excluded.contains(&parent))
            {
                excluded.insert(candidate.id);
            }
        }
        let targets: Rc<Vec<(usize, Folder)>> = Rc::new(
            folder_tree(&folders)
                .into_iter()
                .filter(|(_, candidate)| !excluded.contains(&candidate.id))
                .map(|(depth, candidate)| (depth, candidate.clone()))
                .collect(),
        );
        let title: SharedString = format!("Move “{}” to", folder.name).into();
        window.open_dialog(cx, move |dialog, _, cx| {
            let t = theme(cx).clone();
            let row = |id: ElementId, depth: usize, icon: IconName, label: String, here: bool| {
                div()
                    .id(id)
                    .flex()
                    .flex_row()
                    .flex_shrink_0()
                    .items_center()
                    .gap(SPACE_2)
                    .h(px(30.))
                    .pl(px(8. + depth as f32 * 16.))
                    .pr(SPACE_2)
                    .rounded(RADIUS_MD)
                    .cursor_pointer()
                    .text_size(crate::theme::TEXT_SM)
                    .hover(|style| style.bg(t.hover))
                    .child(Icon::new(icon).size(ICON_SIZE_SM).text_color(t.text_muted))
                    .child(div().flex_1().min_w(px(0.)).truncate().child(label))
                    .when(here, |el| {
                        el.child(
                            Icon::new(IconName::Check)
                                .size(ICON_SIZE_SM)
                                .text_color(t.accent),
                        )
                    })
            };
            let pick = |parent: Option<Id>| {
                let state = state.clone();
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let command = mail_ops::move_folder(state.read(cx).store(), folder_id, parent);
                    if ops::run(&state, command, cx) {
                        window.close_dialog(cx);
                    }
                }
            };
            let mut list = div().flex().flex_col().gap(px(2.)).child(
                row(
                    "move-top".into(),
                    0,
                    IconName::FolderTree,
                    "Top level".into(),
                    current_parent.is_none(),
                )
                .on_click(pick(None)),
            );
            for (depth, target) in targets.iter() {
                list = list.child(
                    row(
                        ("move-to", target.id as u64).into(),
                        depth + 1,
                        folder_icon(target),
                        folder_name(target),
                        current_parent == Some(target.id),
                    )
                    .on_click(pick(Some(target.id))),
                );
            }
            dialog.w(px(360.)).title(title.clone()).child(
                div()
                    .id("move-folder-picker")
                    .max_h(px(420.))
                    .overflow_y_scroll()
                    .child(list),
            )
        });
    }

    fn folder_menu(
        menu: PopupMenu,
        pane: &Entity<Self>,
        state: &Entity<AppState>,
        folder: &Folder,
        is_favorite: bool,
    ) -> PopupMenu {
        let custom = folder.role == FolderRole::Custom;
        let item = |label: &str, icon: IconName| PopupMenuItem::new(label.to_owned()).icon(icon);

        let (s, f) = (state.clone(), folder.clone());
        let new_sub = item("New subfolder", IconName::FolderPlus).on_click(move |_, window, cx| {
            Self::new_folder_dialog(s.clone(), f.account_id, Some(f.id), window, cx)
        });
        let (s, f) = (state.clone(), folder.clone());
        let rename = item("Rename", IconName::FolderPen)
            .disabled(!custom)
            .on_click(move |_, window, cx| Self::rename_dialog(s.clone(), &f, window, cx));
        let (s, f) = (state.clone(), folder.clone());
        let move_to = item("Move folder…", IconName::FolderInput)
            .disabled(!custom)
            .on_click(move |_, window, cx| Self::move_dialog(s.clone(), &f, window, cx));
        let (s, f) = (state.clone(), folder.clone());
        let delete = item("Delete folder", IconName::FolderX)
            .disabled(!custom)
            .on_click(move |_, window, cx| Self::delete_dialog(s.clone(), &f, window, cx));
        let (s, id) = (state.clone(), folder.id);
        let mark_read = item("Mark all as read", IconName::MailOpen).on_click(move |_, _, cx| {
            let command = mail_ops::mark_folder_read(s.read(cx).store(), id);
            ops::run(&s, command, cx);
        });
        let (p, id) = (pane.clone(), folder.id);
        let favorite = item(
            if is_favorite {
                "Remove from Favorites"
            } else {
                "Add to Favorites"
            },
            IconName::Star,
        )
        .on_click(move |_, _, cx| p.update(cx, |pane, cx| pane.toggle_favorite(id, cx)));

        menu.item(new_sub)
            .item(rename)
            .item(move_to)
            .item(delete)
            .separator()
            .item(mark_read)
            .item(favorite)
    }

    #[allow(clippy::too_many_arguments)]
    fn folder_row(
        &self,
        prefix: &'static str,
        folder: &Folder,
        label: String,
        depth: usize,
        has_children: bool,
        info: &RowInfo,
        is_favorite: bool,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let folder_id = folder.id;
        let selected = info.selected;
        let collapsed = self.collapsed.contains(&folder_id);
        let state = self.state.clone();
        let drop_state = self.state.clone();
        let pane = cx.entity();
        let menu_state = self.state.clone();
        let menu_folder = folder.clone();
        let accent_subtle = t.accent_subtle;
        let accent = t.accent;

        let chevron = if has_children {
            div()
                .id((prefix, folder_id as u64))
                .flex()
                .items_center()
                .justify_center()
                .size(px(16.))
                .rounded(RADIUS_MD)
                .text_color(t.text_subtle)
                .hover(|s| s.bg(t.hover))
                .child(
                    Icon::new(if collapsed {
                        IconName::ChevronRight
                    } else {
                        IconName::ChevronDown
                    })
                    .size(px(12.)),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    if !this.collapsed.remove(&folder_id) {
                        this.collapsed.insert(folder_id);
                    }
                    cx.notify();
                }))
                .into_any_element()
        } else {
            div().size(px(16.)).into_any_element()
        };

        div()
            .id(ElementId::Name(format!("{prefix}-{folder_id}").into()))
            .flex()
            .flex_row()
            .items_center()
            .gap(SPACE_1)
            .h(px(30.))
            .pl(px(4. + depth as f32 * 14.))
            .pr(SPACE_2)
            .rounded(RADIUS_MD)
            .cursor_pointer()
            .text_size(TEXT_MD)
            .when(selected, |el| {
                el.bg(t.accent_subtle)
                    .text_color(t.accent)
                    .font_weight(FontWeight::MEDIUM)
            })
            .when(!selected, |el| {
                el.text_color(t.text).hover(|s| s.bg(t.hover))
            })
            .child(chevron)
            .child(
                Icon::new(folder_icon(folder))
                    .size(ICON_SIZE)
                    .text_color(if selected { t.accent } else { t.text_muted }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .ml(SPACE_1)
                    .truncate()
                    .child(label),
            )
            .when(info.count > 0, |el| {
                el.child(if selected || folder.role == FolderRole::Inbox {
                    Badge::count(info.count).accent()
                } else {
                    Badge::count(info.count)
                })
            })
            .on_click(move |_, _, cx| {
                state.update(cx, |state, cx| state.select_folder(Some(folder_id), cx));
            })
            .drag_over::<DraggedMessages>(move |style, _, _, _| {
                style.bg(accent_subtle).border_1().border_color(accent)
            })
            .on_drop(move |dragged: &DraggedMessages, window, cx| {
                let modifiers = window.modifiers();
                let copy = modifiers.control || modifiers.platform;
                ops::move_to(&drop_state, dragged.ids.clone(), folder_id, copy, cx);
            })
            .context_menu(move |menu, _, _| {
                Self::folder_menu(menu, &pane, &menu_state, &menu_folder, is_favorite)
            })
            .into_any_element()
    }

    fn section_header(id: ElementId, label: String, collapsed: bool, t: &Theme) -> Stateful<Div> {
        div()
            .id(id)
            .flex()
            .flex_row()
            .items_center()
            .gap(SPACE_1)
            .h(px(28.))
            .mt(SPACE_2)
            .px(SPACE_1)
            .rounded(RADIUS_MD)
            .cursor_pointer()
            .text_size(TEXT_XS)
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(t.text_subtle)
            .hover(|s| s.bg(t.hover).text_color(t.text_muted))
            .child(
                Icon::new(if collapsed {
                    IconName::ChevronRight
                } else {
                    IconName::ChevronDown
                })
                .size(px(12.)),
            )
            .child(label)
    }

    fn render_collapsed(
        &self,
        favorites: &[(Folder, i64)],
        selected: Option<Id>,
        t: &Theme,
    ) -> AnyElement {
        let prefs = self.prefs.clone();
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(SPACE_1)
            .size_full()
            .py(SPACE_2)
            .child(
                IconButton::new("folders-expand", IconName::PanelLeftOpen)
                    .tooltip("Show folders")
                    .on_click(move |_, _, cx| Self::set_collapsed(&prefs, false, cx)),
            )
            .child(div().w(px(20.)).h(px(1.)).my(SPACE_1).bg(t.border_subtle))
            .children(favorites.iter().map(|(folder, unread)| {
                let state = self.state.clone();
                let folder_id = folder.id;
                div()
                    .relative()
                    .child(
                        IconButton::new(("folder-mini", folder_id as u64), folder_icon(folder))
                            .active(selected == Some(folder_id))
                            .tooltip(folder_name(folder))
                            .on_click(move |_, _, cx| {
                                state.update(cx, |state, cx| {
                                    state.select_folder(Some(folder_id), cx)
                                });
                            }),
                    )
                    .when(*unread > 0, |el| {
                        el.child(
                            div()
                                .absolute()
                                .top(px(3.))
                                .right(px(3.))
                                .size(px(7.))
                                .rounded(px(4.))
                                .bg(t.accent),
                        )
                    })
            }))
            .into_any_element()
    }
}

impl Render for FolderPaneView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let state = self.state.read(cx);
        let store = state.store();
        let selected = state.selected_folder();
        // Render-time reads: errors just show an empty pane (a toast would fire every frame).
        let accounts: Vec<Account> = store.list_accounts().unwrap_or_default();
        let folders_by_account: Vec<(Account, Vec<Folder>)> = accounts
            .into_iter()
            .map(|account| {
                let folders = store.list_folders(account.id).unwrap_or_default();
                (account, folders)
            })
            .collect();
        let mut counts: HashMap<Id, i64> = HashMap::new();
        for (_, folders) in &folders_by_account {
            for folder in folders {
                let count = if folder.role == FolderRole::Drafts {
                    store
                        .list_messages(folder.id)
                        .map(|messages| messages.len() as i64)
                        .unwrap_or(0)
                } else {
                    store.unread_count(folder.id).unwrap_or(0)
                };
                counts.insert(folder.id, count);
            }
        }
        let multi_account = folders_by_account.len() > 1;

        let favorite_ids = self
            .favorites
            .get_or_insert_with(|| {
                folders_by_account
                    .iter()
                    .flat_map(|(_, folders)| folders.iter())
                    .filter(|folder| folder.role == FolderRole::Inbox)
                    .map(|folder| folder.id)
                    .collect()
            })
            .clone();
        let favorites: Vec<(Folder, String)> = favorite_ids
            .iter()
            .filter_map(|id| {
                folders_by_account.iter().find_map(|(account, folders)| {
                    folders.iter().find(|f| f.id == *id).map(|folder| {
                        let name = folder_name(folder);
                        let label = if multi_account && folder.role != FolderRole::Custom {
                            format!("{name} · {}", account.name)
                        } else {
                            name
                        };
                        (folder.clone(), label)
                    })
                })
            })
            .collect();

        if self.prefs.read(cx).folders_collapsed {
            let mini: Vec<(Folder, i64)> = favorites
                .iter()
                .map(|(folder, _)| (folder.clone(), counts.get(&folder.id).copied().unwrap_or(0)))
                .collect();
            return self.render_collapsed(&mini, selected, &t);
        }

        let info = |folder: &Folder| RowInfo {
            count: counts.get(&folder.id).copied().unwrap_or(0),
            selected: selected == Some(folder.id),
        };

        let mut content: Vec<AnyElement> = Vec::new();

        // Favorites.
        if !favorites.is_empty() {
            content.push(
                Self::section_header(
                    "favorites-header".into(),
                    "FAVORITES".into(),
                    self.favorites_collapsed,
                    &t,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.favorites_collapsed = !this.favorites_collapsed;
                    cx.notify();
                }))
                .into_any_element(),
            );
            if !self.favorites_collapsed {
                for (folder, label) in &favorites {
                    let row = self.folder_row(
                        "fav",
                        folder,
                        label.clone(),
                        0,
                        false,
                        &info(folder),
                        true,
                        &t,
                        cx,
                    );
                    content.push(row);
                }
            }
        }

        // Accounts.
        for (account, folders) in &folders_by_account {
            let account_id = account.id;
            let collapsed = self.collapsed_accounts.contains(&account_id);
            let menu_state = self.state.clone();
            content.push(
                Self::section_header(
                    ("account", account_id as u64).into(),
                    account.name.to_uppercase(),
                    collapsed,
                    &t,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.collapsed_accounts.remove(&account_id) {
                        this.collapsed_accounts.insert(account_id);
                    }
                    cx.notify();
                }))
                .context_menu(move |menu, _, _| {
                    let state = menu_state.clone();
                    menu.item(
                        PopupMenuItem::new("New folder")
                            .icon(IconName::FolderPlus)
                            .on_click(move |_, window, cx| {
                                Self::new_folder_dialog(state.clone(), account_id, None, window, cx)
                            }),
                    )
                })
                .into_any_element(),
            );
            if collapsed {
                continue;
            }
            let has_children: HashSet<Id> = folders.iter().filter_map(|f| f.parent_id).collect();
            // Skip descendants of collapsed folders.
            let mut hidden_below: Option<usize> = None;
            for (depth, folder) in folder_tree(folders) {
                if let Some(limit) = hidden_below {
                    if depth > limit {
                        continue;
                    }
                    hidden_below = None;
                }
                if self.collapsed.contains(&folder.id) {
                    hidden_below = Some(depth);
                }
                let is_favorite = favorite_ids.contains(&folder.id);
                let row = self.folder_row(
                    "folder",
                    folder,
                    folder_name(folder),
                    depth,
                    has_children.contains(&folder.id),
                    &info(folder),
                    is_favorite,
                    &t,
                    cx,
                );
                content.push(row);
            }
        }

        let prefs = self.prefs.clone();
        let new_state = self.state.clone();
        let first_account = folders_by_account.first().map(|(account, _)| account.id);
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_shrink_0()
                    .items_center()
                    .gap(SPACE_1)
                    .h(px(40.))
                    .px(SPACE_3)
                    .border_b_1()
                    .border_color(t.border_subtle)
                    .child(
                        div()
                            .flex_1()
                            .text_size(TEXT_MD)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Folders"),
                    )
                    .when_some(first_account, |el, account_id| {
                        el.child(
                            IconButton::new("folder-new", IconName::FolderPlus)
                                .icon_size(ICON_SIZE_SM)
                                .tooltip("New folder")
                                .on_click(move |_, window, cx| {
                                    let parent = new_state.read(cx).selected_folder();
                                    let (account, parent) = parent
                                        .and_then(|id| {
                                            new_state
                                                .read(cx)
                                                .store()
                                                .get::<Folder>(id)
                                                .ok()
                                                .flatten()
                                        })
                                        .map_or((account_id, None), |folder| {
                                            (folder.account_id, Some(folder.id))
                                        });
                                    Self::new_folder_dialog(
                                        new_state.clone(),
                                        account,
                                        parent,
                                        window,
                                        cx,
                                    );
                                }),
                        )
                    })
                    .child(
                        IconButton::new("folders-collapse", IconName::PanelLeftClose)
                            .icon_size(ICON_SIZE_SM)
                            .tooltip("Hide folders")
                            .on_click(move |_, _, cx| Self::set_collapsed(&prefs, true, cx)),
                    ),
            )
            .child(
                div()
                    .id("folder-pane-scroll")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .px(SPACE_2)
                    .pb(SPACE_3)
                    .gap(px(1.))
                    .children(content),
            )
            .into_any_element()
    }
}
