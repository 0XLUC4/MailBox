//! Command palette (Ctrl+K / Ctrl+Shift+P): fuzzy search over every action, folder, contact and
//! module. Running an action dispatches it from the element that had focus before the palette.

use std::cell::Cell;

use gpui::component::WindowExt as _;
use gpui::component::input::{Input, InputEvent, InputState, MoveDown, MoveUp};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::Id;

use crate::actions::ACTIONS;
use crate::app_state::{AppState, Module};
use crate::components::{Icon, IconName};
use crate::dialogs::kbd;
use crate::settings::tr;
use crate::theme::{
    ICON_SIZE, RADIUS_MD, SPACE_1, SPACE_2, SPACE_3, TEXT_MD, TEXT_SM, TEXT_XS, theme,
};

/// Rows rendered at most.
const MAX_ITEMS: usize = 60;

#[derive(Clone)]
enum Target {
    Action(fn() -> Box<dyn Action>),
    Folder(Id),
    Contact(String),
    Module(Module),
}

#[derive(Clone)]
struct Item {
    title: SharedString,
    subtitle: SharedString,
    icon: IconName,
    shortcut: Option<String>,
    /// Extra words matched with a lower weight.
    keywords: SharedString,
    target: Target,
    /// Shown when the query is empty.
    pinned: bool,
}

/// Fuzzy subsequence score (higher is better); `None` when `query` does not match `text`.
/// Rewards consecutive characters, word starts and a prefix match.
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    let query: Vec<char> = query
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if query.is_empty() {
        return Some(0);
    }
    let text: Vec<char> = text.to_lowercase().chars().collect();
    let mut score = 0;
    let mut position = 0;
    let mut previous: Option<usize> = None;
    for wanted in query {
        let found = (position..text.len()).find(|&index| text[index] == wanted)?;
        score += 1;
        if previous.is_some_and(|previous| previous + 1 == found) {
            score += 5;
        }
        if found == 0 || !text[found - 1].is_alphanumeric() {
            score += 3;
        }
        score -= i32::try_from(found - position).unwrap_or(i32::MAX).min(3);
        previous = Some(found);
        position = found + 1;
    }
    Some(score - i32::try_from(text.len() / 8).unwrap_or(0))
}

pub struct CommandPalette {
    state: Entity<AppState>,
    input: Entity<InputState>,
    all: Vec<Item>,
    matches: Vec<Item>,
    selected: usize,
    scroll: ScrollHandle,
    /// Element focused before opening: actions are dispatched from it.
    restore_focus: Option<FocusHandle>,
    _subscription: Subscription,
}

impl CommandPalette {
    fn new(
        state: Entity<AppState>,
        restore_focus: Option<FocusHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Type a command, folder or contact...")
        });
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => this.filter(cx),
                InputEvent::PressEnter { .. } => this.confirm(window, cx),
                _ => {}
            },
        );
        let all = Self::items(&state, cx);
        let mut palette = Self {
            state,
            input,
            matches: Vec::new(),
            all,
            selected: 0,
            scroll: ScrollHandle::new(),
            restore_focus,
            _subscription: subscription,
        };
        palette.filter(cx);
        palette
    }

    /// Every searchable entry: actions, modules, folders, contacts.
    fn items(state: &Entity<AppState>, cx: &App) -> Vec<Item> {
        let mut items: Vec<Item> = ACTIONS
            .iter()
            .map(|spec| Item {
                title: tr(cx, spec.label).into(),
                subtitle: "Command".into(),
                icon: spec.icon,
                shortcut: spec.shortcut_text(),
                keywords: format!("{} {}", spec.label, spec.keywords).into(),
                target: Target::Action(spec.build),
                pinned: true,
            })
            .collect();
        items.extend(
            [
                Module::Mail,
                Module::Calendar,
                Module::People,
                Module::Tasks,
            ]
            .map(|module| Item {
                title: format!("Open {}", tr(cx, module.label())).into(),
                subtitle: "Module".into(),
                icon: module.icon(),
                shortcut: None,
                keywords: "go to switch".into(),
                target: Target::Module(module),
                pinned: false,
            }),
        );
        let store = state.read(cx).store();
        for account in store.list_accounts().unwrap_or_default() {
            for folder in store.list_folders(account.id).unwrap_or_default() {
                items.push(Item {
                    title: folder.name.clone().into(),
                    subtitle: format!("Folder · {}", account.name).into(),
                    icon: IconName::Folder,
                    shortcut: None,
                    keywords: format!("go to folder {}", folder.role.as_str()).into(),
                    target: Target::Folder(folder.id),
                    pinned: false,
                });
            }
        }
        for contact in store.list_contacts().unwrap_or_default() {
            let email = contact.emails.first().cloned().unwrap_or_default();
            items.push(Item {
                title: contact.display_name.clone().into(),
                subtitle: if email.is_empty() {
                    "Contact".into()
                } else {
                    format!("Contact · {email}").into()
                },
                icon: IconName::User,
                shortcut: None,
                keywords: format!("{email} {} person", contact.company).into(),
                target: Target::Contact(contact.display_name),
                pinned: false,
            });
        }
        items
    }

    fn filter(&mut self, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value().trim().to_owned();
        self.matches = if query.is_empty() {
            self.all
                .iter()
                .filter(|item| item.pinned)
                .cloned()
                .collect()
        } else {
            let mut scored: Vec<(i32, &Item)> = self
                .all
                .iter()
                .filter_map(|item| {
                    let title = fuzzy_score(&query, &item.title);
                    let keywords = fuzzy_score(&query, &item.keywords).map(|score| score - 6);
                    title.max(keywords).map(|score| (score, item))
                })
                .collect();
            // Stable: equal scores keep catalog order (commands before folders and contacts).
            scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
            scored
                .into_iter()
                .take(MAX_ITEMS)
                .map(|(_, item)| item.clone())
                .collect()
        };
        self.selected = 0;
        self.scroll.scroll_to_item(0);
        cx.notify();
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.matches.is_empty() {
            return;
        }
        let count = self.matches.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(count) as usize;
        self.scroll.scroll_to_item(self.selected);
        cx.notify();
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.matches.get(self.selected) else {
            return;
        };
        let target = item.target.clone();
        let restore = self.restore_focus.clone();
        let state = self.state.clone();
        window.close_dialog(cx);
        // After the dialog is gone, so focus and dispatch happen in the main view.
        window.defer(cx, move |window, cx| {
            run(target, restore, state, window, cx)
        });
    }
}

fn run(
    target: Target,
    restore: Option<FocusHandle>,
    state: Entity<AppState>,
    window: &mut Window,
    cx: &mut App,
) {
    if let Some(handle) = &restore {
        handle.focus(window, cx);
    }
    match target {
        Target::Action(build) => match &restore {
            Some(handle) => handle.dispatch_action(build().as_ref(), window, cx),
            None => window.dispatch_action(build(), cx),
        },
        Target::Folder(folder) => state.update(cx, |state, cx| {
            state.set_module(Module::Mail, cx);
            state.select_folder(Some(folder), cx);
        }),
        Target::Contact(name) => state.update(cx, |state, cx| {
            state.set_module(Module::People, cx);
            state.set_search_query(name, cx);
        }),
        Target::Module(module) => state.update(cx, |state, cx| state.set_module(module, cx)),
    }
}

impl Render for CommandPalette {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let rows = self.matches.iter().enumerate().map(|(index, item)| {
            let selected = index == self.selected;
            div()
                .id(("palette-item", index as u64))
                .flex()
                .flex_row()
                .items_center()
                .gap(SPACE_3)
                .h(px(36.))
                .px(SPACE_2)
                .rounded(RADIUS_MD)
                .cursor_pointer()
                .when(selected, |el| el.bg(t.accent_subtle))
                .when(!selected, |el| el.hover(|style| style.bg(t.hover)))
                .child(
                    Icon::new(item.icon)
                        .size(ICON_SIZE)
                        .text_color(if selected { t.accent } else { t.text_muted }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .truncate()
                        .text_size(TEXT_MD)
                        .child(item.title.clone()),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(TEXT_XS)
                        .text_color(t.text_subtle)
                        .child(item.subtitle.clone()),
                )
                .when_some(item.shortcut.clone(), |el, keys| el.child(kbd(&keys, cx)))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.selected = index;
                    this.confirm(window, cx);
                }))
        });
        let rows: Vec<_> = rows.collect();
        let empty = rows.is_empty();

        div()
            .flex()
            .flex_col()
            .gap(SPACE_2)
            .capture_action(cx.listener(|this, _: &MoveDown, _, cx| {
                this.move_selection(1, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &MoveUp, _, cx| {
                this.move_selection(-1, cx);
                cx.stop_propagation();
            }))
            .child(
                Input::new(&self.input).prefix(
                    Icon::new(IconName::Command)
                        .size(ICON_SIZE)
                        .text_color(t.text_subtle),
                ),
            )
            .child(
                div()
                    .id("palette-list")
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .max_h(px(380.))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(rows)
                    .when(empty, |el| {
                        el.child(
                            div()
                                .py(SPACE_3)
                                .text_center()
                                .text_size(TEXT_SM)
                                .text_color(t.text_muted)
                                .child("No matching command, folder or contact"),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(SPACE_3)
                    .pt(SPACE_1)
                    .border_t_1()
                    .border_color(t.border_subtle)
                    .text_size(TEXT_XS)
                    .text_color(t.text_subtle)
                    .child("↑↓ navigate")
                    .child("↵ run")
                    .child("esc close"),
            )
    }
}

/// Opens the palette (closes it when already open).
pub fn open(state: Entity<AppState>, window: &mut Window, cx: &mut App) {
    if window.has_active_dialog(cx) {
        window.close_dialog(cx);
        return;
    }
    let restore = window.focused(cx);
    let palette = cx.new(|cx| CommandPalette::new(state, restore, window, cx));
    let input = palette.read(cx).input.clone();
    let first = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if first.replace(false) {
            input.update(cx, |input, cx| input.focus(window, cx));
        }
        dialog
            .w(px(600.))
            .margin_top(px(72.))
            .close_button(false)
            .child(palette.clone())
    });
}

#[cfg(test)]
mod tests {
    use super::fuzzy_score;

    #[test]
    fn fuzzy_matches_subsequences_and_ranks_prefixes() {
        assert!(fuzzy_score("rpl", "Reply").is_some());
        assert!(fuzzy_score("xyz", "Reply").is_none());
        assert_eq!(fuzzy_score("", "anything"), Some(0));
        let prefix = fuzzy_score("rep", "Reply all").unwrap();
        let scattered = fuzzy_score("rep", "Mark as read pending").unwrap_or(i32::MIN);
        assert!(prefix > scattered);
        let word_start = fuzzy_score("ma", "Mark all as read").unwrap();
        let inside = fuzzy_score("ma", "Format").unwrap();
        assert!(word_start > inside);
    }
}
