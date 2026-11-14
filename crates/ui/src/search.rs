//! Search box in the top bar: query language (`mailbox_core::search`), suggestions, scope and
//! recent searches.
//!
//! It writes the *effective* query to `AppState::search_query`: the typed text, plus
//! `folder:<id>` when the scope is "This folder". The message list runs it with
//! `store.search_messages(query, limit)`, which understands the whole language.

use gpui::component::input::{Escape, Input, InputEvent, InputState, MoveDown, MoveUp};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::search::{OPERATORS, SearchQuery};

use crate::app_state::{AppState, Module};
use crate::components::{Icon, IconName};
use crate::settings::tr;
use crate::theme::{
    ICON_SIZE_SM, RADIUS_LG, RADIUS_MD, SPACE_1, SPACE_2, SPACE_3, TEXT_SM, TEXT_XS, theme,
};

/// Recent searches kept for the session.
const MAX_RECENT: usize = 8;
/// Suggestions shown at once.
const MAX_SUGGESTIONS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    CurrentFolder,
    AllMailboxes,
}

#[derive(Debug, Clone, PartialEq)]
struct Suggestion {
    icon: IconName,
    label: String,
    detail: String,
    /// Full input text after accepting.
    replacement: String,
}

pub struct SearchView {
    state: Entity<AppState>,
    input: Entity<InputState>,
    scope: Scope,
    recent: Vec<String>,
    open: bool,
    highlighted: usize,
    /// Last query written to `AppState`, to tell our writes from external ones.
    written: String,
    _subscriptions: Vec<Subscription>,
}

impl SearchView {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search mail, people, events (Ctrl+E)")
        });
        let subscriptions =
            vec![
                cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                    match event {
                        InputEvent::Change => {
                            this.open = true;
                            this.highlighted = 0;
                            this.publish(cx);
                        }
                        InputEvent::PressEnter { .. } => this.accept(window, cx),
                        InputEvent::Focus => {
                            this.open = true;
                            cx.notify();
                        }
                        InputEvent::Blur => {
                            this.open = false;
                            cx.notify();
                        }
                    }
                }),
                cx.observe_in(&state, window, |this, _, window, cx| this.sync(window, cx)),
            ];
        Self {
            state,
            input,
            scope: Scope::AllMailboxes,
            recent: Vec::new(),
            open: false,
            highlighted: 0,
            written: String::new(),
            _subscriptions: subscriptions,
        }
    }

    /// Focuses the search box (Ctrl+E).
    pub fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
        self.open = true;
        cx.notify();
    }

    fn text(&self, cx: &App) -> String {
        self.input.read(cx).value().trim().to_owned()
    }

    /// Typed text + scope, as stored in `AppState`.
    fn effective_query(&self, cx: &App) -> String {
        let text = self.text(cx);
        if text.is_empty() {
            return text;
        }
        let state = self.state.read(cx);
        let scoped = self.scope == Scope::CurrentFolder
            && state.module() == Module::Mail
            && !SearchQuery::parse(&text).has_folder_filter();
        match state.selected_folder() {
            Some(folder) if scoped => format!("{text} folder:{folder}"),
            _ => text,
        }
    }

    fn publish(&mut self, cx: &mut Context<Self>) {
        let query = self.effective_query(cx);
        self.written = query.clone();
        self.state
            .update(cx, |state, cx| state.set_search_query(query, cx));
        cx.notify();
    }

    /// Follows external changes: a cleared query (folder click) or one set elsewhere (palette),
    /// and re-scopes when the folder changes.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.state.read(cx).search_query().to_owned();
        if current != self.written {
            self.written = current.clone();
            self.input
                .update(cx, |input, cx| input.set_value(current, window, cx));
            cx.notify();
        } else if !current.is_empty() && self.effective_query(cx) != current {
            self.publish(cx);
        }
    }

    fn remember(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        self.recent.retain(|recent| recent != &text);
        self.recent.insert(0, text);
        self.recent.truncate(MAX_RECENT);
    }

    /// Enter: accepts the highlighted suggestion, or runs the query and remembers it.
    fn accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let suggestions = self.suggestions(cx);
        if self.open
            && let Some(suggestion) = suggestions.get(self.highlighted)
            && suggestion.replacement.trim() != self.text(cx)
        {
            return self.apply(suggestion.replacement.clone(), window, cx);
        }
        self.remember(self.text(cx));
        self.open = false;
        self.publish(cx);
    }

    fn apply(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let complete = !text.ends_with(':');
        self.input
            .update(cx, |input, cx| input.set_value(text.clone(), window, cx));
        self.highlighted = 0;
        if complete {
            self.remember(text.trim().to_owned());
        }
        self.open = !complete;
        self.publish(cx);
    }

    fn move_highlight(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.suggestions(cx).len();
        if count == 0 {
            return;
        }
        self.open = true;
        self.highlighted = (self.highlighted as isize + delta).rem_euclid(count as isize) as usize;
        cx.notify();
    }

    fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.open = false;
        self.publish(cx);
    }

    fn suggestions(&self, cx: &App) -> Vec<Suggestion> {
        let raw = self.input.read(cx).value().to_string();
        let store = self.state.read(cx).store();
        if raw.trim().is_empty() {
            let recent = self.recent.iter().map(|query| Suggestion {
                icon: IconName::Clock,
                label: query.clone(),
                detail: String::new(),
                replacement: query.clone(),
            });
            let tips = ["from:", "has:attachment", "is:unread", "after:"]
                .into_iter()
                .filter_map(|operator| OPERATORS.iter().find(|(op, _)| *op == operator))
                .map(|(op, detail)| Suggestion {
                    icon: IconName::ListFilter,
                    label: (*op).to_owned(),
                    detail: (*detail).to_owned(),
                    replacement: (*op).to_owned(),
                });
            return recent.chain(tips).take(MAX_SUGGESTIONS).collect();
        }

        // Complete the last (partial) token.
        // ASCII separators only, so `index + 1` is a char boundary.
        let (head, token) = match raw.rfind([' ', '\t']) {
            Some(index) => raw.split_at(index + 1),
            None => ("", raw.as_str()),
        };
        let token_lower = token.to_lowercase();
        let negation = if token_lower.starts_with('-') {
            "-"
        } else {
            ""
        };
        let bare = token_lower.trim_start_matches('-');
        let with = |value: &str| format!("{head}{negation}{value}");
        let mut out = Vec::new();

        if let Some((key, partial)) = bare.split_once(':') {
            let values: Vec<String> = match key {
                "in" => {
                    let mut names: Vec<String> = store
                        .list_accounts()
                        .unwrap_or_default()
                        .iter()
                        .flat_map(|account| store.list_folders(account.id).unwrap_or_default())
                        .map(|folder| folder.name.to_lowercase())
                        .collect();
                    names.insert(0, "all".to_owned());
                    names.sort();
                    names.dedup();
                    names
                }
                "category" | "cat" => store
                    .list_categories()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|category| category.name.to_lowercase())
                    .collect(),
                "from" | "to" | "cc" => store
                    .list_contacts()
                    .unwrap_or_default()
                    .into_iter()
                    .flat_map(|contact| contact.emails)
                    .collect(),
                "is" => ["unread", "read", "flagged", "pinned", "important"]
                    .map(str::to_owned)
                    .to_vec(),
                "has" => vec!["attachment".to_owned()],
                "importance" => ["high", "normal", "low"].map(str::to_owned).to_vec(),
                "after" | "before" => vec![chrono::Local::now().format("%Y-%m-%d").to_string()],
                _ => Vec::new(),
            };
            let partial = partial.to_lowercase();
            for value in values {
                if value.to_lowercase().starts_with(&partial)
                    || value.to_lowercase().contains(&partial)
                {
                    let quoted = if value.contains(' ') {
                        format!("\"{value}\"")
                    } else {
                        value.clone()
                    };
                    out.push(Suggestion {
                        icon: IconName::ListFilter,
                        label: format!("{key}:{value}"),
                        detail: String::new(),
                        replacement: with(&format!("{key}:{quoted} ")),
                    });
                }
            }
        } else {
            for (operator, detail) in OPERATORS {
                if operator.starts_with(bare) && *operator != bare {
                    let space = if operator.ends_with(':') { "" } else { " " };
                    out.push(Suggestion {
                        icon: IconName::ListFilter,
                        label: (*operator).to_owned(),
                        detail: (*detail).to_owned(),
                        replacement: with(&format!("{operator}{space}")),
                    });
                }
            }
            if bare.len() >= 2 {
                for contact in store.list_contacts().unwrap_or_default() {
                    let name = contact.display_name.to_lowercase();
                    let Some(email) = contact.emails.first() else {
                        continue;
                    };
                    if name.contains(bare) || email.to_lowercase().contains(bare) {
                        out.push(Suggestion {
                            icon: IconName::User,
                            label: format!("from:{email}"),
                            detail: contact.display_name.clone(),
                            replacement: with(&format!("from:{email} ")),
                        });
                    }
                }
            }
            for recent in &self.recent {
                if recent.to_lowercase().contains(&raw.trim().to_lowercase())
                    && recent != raw.trim()
                {
                    out.push(Suggestion {
                        icon: IconName::Clock,
                        label: recent.clone(),
                        detail: String::new(),
                        replacement: recent.clone(),
                    });
                }
            }
        }
        out.truncate(MAX_SUGGESTIONS);
        out
    }

    fn render_scope(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let (label, icon) = match self.scope {
            Scope::CurrentFolder => (tr(cx, "This folder"), IconName::Folder),
            Scope::AllMailboxes => (tr(cx, "All mailboxes"), IconName::Mails),
        };
        div()
            .id("search-scope")
            .flex()
            .flex_row()
            .items_center()
            .gap(SPACE_1)
            .px(SPACE_1)
            .h(px(20.))
            .rounded(RADIUS_MD)
            .bg(t.hover)
            .text_size(TEXT_XS)
            .text_color(t.text_muted)
            .cursor_pointer()
            .hover(|style| style.bg(t.active))
            .child(Icon::new(icon).size(px(12.)))
            .child(label)
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .on_click(cx.listener(|this, _, _, cx| {
                this.scope = match this.scope {
                    Scope::CurrentFolder => Scope::AllMailboxes,
                    Scope::AllMailboxes => Scope::CurrentFolder,
                };
                this.publish(cx);
            }))
    }

    fn render_suggestions(&self, suggestions: Vec<Suggestion>, cx: &mut Context<Self>) -> Div {
        let t = theme(cx).clone();
        let empty_input = self.text(cx).is_empty();
        let title = if empty_input && !self.recent.is_empty() {
            tr(cx, "Recent searches")
        } else {
            tr(cx, "Search operators")
        };
        div()
            .w(px(380.))
            .mt(SPACE_1)
            .p(SPACE_1)
            .flex()
            .flex_col()
            .rounded(RADIUS_LG)
            .border_1()
            .border_color(t.border)
            .bg(t.elevated)
            .shadow_lg()
            .child(
                div()
                    .px(SPACE_2)
                    .py(SPACE_1)
                    .text_size(TEXT_XS)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.text_subtle)
                    .child(title),
            )
            .children(
                suggestions
                    .into_iter()
                    .enumerate()
                    .map(|(index, suggestion)| {
                        let highlighted = index == self.highlighted;
                        let replacement = suggestion.replacement.clone();
                        div()
                            .id(("search-suggestion", index as u64))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(SPACE_2)
                            .h(px(28.))
                            .px(SPACE_2)
                            .rounded(RADIUS_MD)
                            .text_size(TEXT_SM)
                            .cursor_pointer()
                            .when(highlighted, |el| el.bg(t.accent_subtle))
                            .when(!highlighted, |el| el.hover(|style| style.bg(t.hover)))
                            .child(
                                Icon::new(suggestion.icon)
                                    .size(ICON_SIZE_SM)
                                    .text_color(t.text_subtle),
                            )
                            .child(div().truncate().child(suggestion.label))
                            .child(
                                div()
                                    .flex_1()
                                    .truncate()
                                    .text_color(t.text_muted)
                                    .text_size(TEXT_XS)
                                    .child(suggestion.detail),
                            )
                            // Mouse down (not click) so the choice lands before the input blurs.
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, window, cx| {
                                    window.prevent_default();
                                    this.apply(replacement.clone(), window, cx);
                                }),
                            )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(SPACE_3)
                    .px(SPACE_2)
                    .pt(SPACE_1)
                    .mt(SPACE_1)
                    .border_t_1()
                    .border_color(t.border_subtle)
                    .text_size(TEXT_XS)
                    .text_color(t.text_subtle)
                    .child("↑↓ select")
                    .child("↵ search")
                    .child("esc close"),
            )
    }
}

impl Render for SearchView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = theme(cx).text_subtle;
        let suggestions = if self.open {
            self.suggestions(cx)
        } else {
            Vec::new()
        };
        self.highlighted = self.highlighted.min(suggestions.len().saturating_sub(1));
        let show_scope = self.state.read(cx).module() == Module::Mail;

        div()
            .relative()
            .flex()
            .flex_col()
            .w_full()
            .capture_action(cx.listener(|this, _: &MoveDown, _, cx| {
                if this.open {
                    this.move_highlight(1, cx);
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &MoveUp, _, cx| {
                if this.open {
                    this.move_highlight(-1, cx);
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                if this.open {
                    this.open = false;
                    cx.notify();
                } else if !this.text(cx).is_empty() {
                    this.clear(window, cx);
                } else {
                    return;
                }
                cx.stop_propagation();
            }))
            .child(
                Input::new(&self.input)
                    .prefix(
                        Icon::new(IconName::Search)
                            .size(ICON_SIZE_SM)
                            .text_color(muted),
                    )
                    .when(show_scope, |input| input.suffix(self.render_scope(cx)))
                    .cleanable(true),
            )
            .when(!suggestions.is_empty(), |el| {
                el.child(
                    deferred(anchored().child(self.render_suggestions(suggestions, cx)))
                        .with_priority(1),
                )
            })
    }
}
