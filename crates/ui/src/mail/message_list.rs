//! Message list: virtualized rows grouped by date/sender/importance, conversations, pinned on
//! top, filters, sort, multi-select (Ctrl/Shift), keyboard navigation, hover quick actions,
//! drag to a folder and a context menu.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use chrono::{DateTime, Local, NaiveDate, Utc};
use gpui::component::button::{Button as KitButton, ButtonVariants as _};
use gpui::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui::component::{Sizable as _, VirtualListScrollHandle, v_virtual_list};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::settings::Density;
use mailbox_core::{FolderRole, Id, Importance, Message, mail_ops};

use super::list_model::{BuildContext, Filter, Grouping, ListOptions, Row, SortKey, build_rows};
use super::{
    ArchiveSelected, ClearSelection, CollapseThread, DeleteSelected, DraggedMessages, ExpandThread,
    ExtendNext, ExtendPrevious, MailPrefs, MarkRead, MarkUnread, OpenSelected, ReadingPanePosition,
    SelectAllMessages, SelectNext, SelectPrevious, ToggleFlag, folder_pane::folder_label,
    message_menu, message_window, ops,
};
use crate::actions;
use crate::app_state::AppState;
use crate::components::{Avatar, Badge, EmptyState, Icon, IconButton, IconName};
use crate::settings::{prefs, update_settings};
use crate::theme::{
    ICON_SIZE_SM, RADIUS_FULL, RADIUS_MD, SPACE_1, SPACE_1_5, SPACE_2, SPACE_3, TEXT_LG, TEXT_MD,
    TEXT_SM, TEXT_XS, Theme, hex, theme,
};

/// Max search results shown.
const SEARCH_LIMIT: usize = 500;
const HEADER_HEIGHT: f32 = 30.;
const ROW_HEIGHT: f32 = 80.;
const CHILD_ROW_HEIGHT: f32 = 58.;
/// Compact density: no preview line.
const COMPACT_ROW_HEIGHT: f32 = 58.;
const COMPACT_CHILD_ROW_HEIGHT: f32 = 36.;

/// "14:05" today, "Mon" this week, "03 Nov" otherwise.
pub fn short_date(date: DateTime<Utc>) -> String {
    let local = date.with_timezone(&Local);
    let now = Local::now();
    if local.date_naive() == now.date_naive() {
        local.format("%H:%M").to_string()
    } else if (now - local).num_days() < 7 {
        local.format("%a %H:%M").to_string()
    } else if local.format("%Y").to_string() == now.format("%Y").to_string() {
        local.format("%d %b").to_string()
    } else {
        local.format("%d/%m/%Y").to_string()
    }
}

/// "Today", "Tomorrow", "Mon 16", "Overdue" style label for a flag due date.
fn due_label(due: NaiveDate) -> (String, bool) {
    let today = Local::now().date_naive();
    let days = (due - today).num_days();
    match days {
        d if d < 0 => (format!("Overdue {}", due.format("%d %b")), true),
        0 => ("Due today".into(), true),
        1 => ("Due tomorrow".into(), false),
        _ => (format!("Due {}", due.format("%a %d %b")), false),
    }
}

pub struct MessageListView {
    state: Entity<AppState>,
    prefs: Entity<MailPrefs>,
    focus_handle: FocusHandle,
    scroll_handle: VirtualListScrollHandle,
    options: ListOptions,
    expanded: HashSet<String>,
    /// Selected message ids (order of selection) and the last clicked one.
    selection: Vec<Id>,
    anchor: Option<Id>,
    /// Compact density (from the settings).
    compact: bool,
    // Cache, rebuilt on store/selection changes.
    source: (Option<Id>, String),
    title: String,
    messages: Vec<Message>,
    rows: Rc<Vec<Row>>,
    sizes: Rc<Vec<Size<Pixels>>>,
    dues: HashMap<Id, NaiveDate>,
    category_colors: HashMap<String, u32>,
    mention_tokens: Vec<String>,
    /// Muted conversations (thread ids).
    muted: HashSet<String>,
    _subscriptions: Vec<Subscription>,
}

impl MessageListView {
    pub fn new(
        state: Entity<AppState>,
        prefs: Entity<MailPrefs>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = vec![
            cx.observe(&state, |this, _, cx| this.refresh(cx)),
            cx.observe(&prefs, |_, _, cx| cx.notify()),
        ];
        let mut view = Self {
            state,
            prefs,
            focus_handle: cx.focus_handle(),
            scroll_handle: VirtualListScrollHandle::new(),
            options: ListOptions::default(),
            expanded: HashSet::new(),
            selection: Vec::new(),
            anchor: None,
            compact: false,
            source: (None, String::new()),
            title: String::new(),
            messages: Vec::new(),
            rows: Rc::new(Vec::new()),
            sizes: Rc::new(Vec::new()),
            dues: HashMap::new(),
            category_colors: HashMap::new(),
            mention_tokens: Vec::new(),
            muted: HashSet::new(),
            _subscriptions: subscriptions,
        };
        view.refresh(cx);
        view
    }

    // ---------- data ----------

    /// Re-reads the store and rebuilds rows (called when `AppState` changes).
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let store = state.store();
        let query = state.search_query().trim().to_owned();
        let folder_id = state.selected_folder();
        let source = (folder_id, query.clone());

        self.messages = if !query.is_empty() {
            store
                .search_messages(&query, SEARCH_LIMIT)
                .unwrap_or_default()
        } else if let Some(id) = folder_id {
            store.list_messages(id).unwrap_or_default()
        } else {
            Vec::new()
        };
        self.title = if query.is_empty() {
            folder_id
                .and_then(|id| folder_label(store, id))
                .unwrap_or_default()
        } else {
            "Search results".into()
        };
        self.dues = mail_ops::flag_dues(store).unwrap_or_default();
        self.category_colors = store
            .list_categories()
            .unwrap_or_default()
            .into_iter()
            .map(|category| (category.name, category.color))
            .collect();
        self.mention_tokens = store
            .list_accounts()
            .unwrap_or_default()
            .iter()
            .filter_map(|account| {
                let local = account.email.split('@').next()?;
                let first = local.split(['.', '_', '-']).next()?;
                (!first.is_empty()).then(|| first.to_lowercase())
            })
            .collect();
        self.muted = mailbox_core::later::Later::load(store)
            .map(|later| later.muted_threads.into_iter().collect())
            .unwrap_or_default();
        let selected = state.selected_message();

        let source_changed = source != self.source;
        if source_changed {
            self.source = source;
            self.selection.clear();
            self.anchor = None;
            self.expanded.clear();
        }
        // Keep the selection in sync with what still exists and with the reading pane.
        let present: HashSet<Id> = self.messages.iter().map(|m| m.id).collect();
        self.selection.retain(|id| present.contains(id));
        match selected {
            Some(id) if present.contains(&id) && !self.selection.contains(&id) => {
                self.selection = vec![id];
                self.anchor = Some(id);
            }
            None if self.selection.len() == 1 => self.selection.clear(),
            _ => {}
        }
        if self.anchor.is_some_and(|id| !self.selection.contains(&id)) {
            self.anchor = self.selection.last().copied();
        }
        self.sync_settings(cx);
        self.rebuild_rows();
        if source_changed && !self.rows.is_empty() {
            self.scroll_handle.scroll_to_item(0, ScrollStrategy::Top);
        }
        cx.notify();
    }

    /// Follows the stored "conversations" and density settings; true when they changed.
    fn sync_settings(&mut self, cx: &App) -> bool {
        let settings = prefs(cx);
        let compact = settings.density == Density::Compact;
        let changed = self.options.threaded != settings.conversations || self.compact != compact;
        self.options.threaded = settings.conversations;
        self.compact = compact;
        changed
    }

    fn row_height(&self, child: bool) -> f32 {
        match (self.compact, child) {
            (false, false) => ROW_HEIGHT,
            (false, true) => CHILD_ROW_HEIGHT,
            (true, false) => COMPACT_ROW_HEIGHT,
            (true, true) => COMPACT_CHILD_ROW_HEIGHT,
        }
    }

    fn rebuild_rows(&mut self) {
        let ctx = BuildContext {
            today: Local::now().date_naive(),
            offset: *Local::now().offset(),
            mention_tokens: &self.mention_tokens,
            expanded: &self.expanded,
        };
        let rows = build_rows(&self.messages, &self.options, &ctx);
        let sizes = rows
            .iter()
            .map(|row| {
                let height = match row {
                    Row::Header { .. } => HEADER_HEIGHT,
                    Row::Message { child, .. } => self.row_height(*child),
                };
                size(px(1.), px(height))
            })
            .collect();
        self.rows = Rc::new(rows);
        self.sizes = Rc::new(sizes);
    }

    fn set_options(&mut self, options: ListOptions, cx: &mut Context<Self>) {
        self.options = options;
        self.rebuild_rows();
        cx.notify();
    }

    /// Message ids in display order.
    fn visible_ids(&self) -> Vec<Id> {
        self.rows
            .iter()
            .filter_map(|row| match row {
                Row::Message { index, .. } => self.messages.get(*index).map(|m| m.id),
                Row::Header { .. } => None,
            })
            .collect()
    }

    fn row_of(&self, id: Id) -> Option<usize> {
        self.rows.iter().position(|row| {
            matches!(row, Row::Message { index, .. } if self.messages.get(*index).is_some_and(|m| m.id == id))
        })
    }

    fn message(&self, id: Id) -> Option<&Message> {
        self.messages.iter().find(|m| m.id == id)
    }

    // ---------- selection ----------

    /// Replaces the selection; `open` shows `primary` in the reading pane and marks it read.
    fn set_selection(&mut self, selection: Vec<Id>, primary: Option<Id>, cx: &mut Context<Self>) {
        self.selection = selection;
        self.anchor = primary;
        if let Some(row) = primary.and_then(|id| self.row_of(id)) {
            self.scroll_handle
                .scroll_to_item(row, ScrollStrategy::Nearest);
        }
        // The reading pane shows a collapsed conversation whole: opening it reads all of it.
        let unread: Vec<Id> = primary
            .map(|id| self.with_conversations(&[id]))
            .unwrap_or_default()
            .into_iter()
            .filter(|id| self.message(*id).is_some_and(|m| !m.flags.read))
            .collect();
        let single = self.selection.len() == 1;
        self.state.update(cx, |state, cx| {
            state.select_message(primary, cx);
        });
        if single
            && !unread.is_empty()
            && let Some(id) = primary
        {
            self.mark_read_later(id, unread, cx);
        }
        cx.notify();
    }

    /// Marks `ids` (the opened message `id`, or its conversation) read now, after the
    /// configured delay while `id` is still open, or never (Settings).
    fn mark_read_later(&mut self, id: Id, ids: Vec<Id>, cx: &mut Context<Self>) {
        match prefs(cx).mark_read_after_secs {
            None => {}
            Some(0) => ops::set_read(&self.state, ids, true, cx),
            Some(secs) => {
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(std::time::Duration::from_secs(u64::from(secs)))
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        if this.state.read(cx).selected_message() == Some(id) {
                            ops::set_read(&this.state, ids, true, cx);
                        }
                    });
                })
                .detach();
            }
        }
    }

    fn click(&mut self, id: Id, modifiers: Modifiers, cx: &mut Context<Self>) {
        if modifiers.shift
            && let Some(anchor) = self.anchor
        {
            let ids = self.visible_ids();
            let (Some(a), Some(b)) = (
                ids.iter().position(|&x| x == anchor),
                ids.iter().position(|&x| x == id),
            ) else {
                return self.set_selection(vec![id], Some(id), cx);
            };
            let range = ids[a.min(b)..=a.max(b)].to_vec();
            // Keep the anchor so further shift-clicks extend from it.
            self.selection = range;
            self.state
                .update(cx, |state, cx| state.select_message(Some(id), cx));
            cx.notify();
        } else if modifiers.control || modifiers.platform {
            let mut selection = self.selection.clone();
            if let Some(index) = selection.iter().position(|&x| x == id) {
                selection.remove(index);
                let primary = selection.last().copied();
                self.set_selection(selection, primary, cx);
            } else {
                selection.push(id);
                self.selection = selection;
                self.anchor = Some(id);
                self.state
                    .update(cx, |state, cx| state.select_message(Some(id), cx));
                cx.notify();
            }
        } else {
            self.set_selection(vec![id], Some(id), cx);
        }
    }

    /// Ids a context-menu or drag acts on: the selection when `id` is part of it.
    fn targets_for(&self, id: Id) -> Vec<Id> {
        if self.selection.contains(&id) {
            self.selected_targets()
        } else {
            self.with_conversations(&[id])
        }
    }

    /// The messages the selection acts on: a conversation's top row stands for all of it.
    fn selected_targets(&self) -> Vec<Id> {
        self.with_conversations(&self.selection)
    }

    /// `ids` plus, for each conversation top row among them, the rest of the conversation.
    fn with_conversations(&self, ids: &[Id]) -> Vec<Id> {
        let heads: HashSet<&str> = self
            .rows
            .iter()
            .filter_map(|row| match row {
                Row::Message {
                    index,
                    child: false,
                    thread_len,
                    ..
                } if *thread_len > 1 => self.messages.get(*index),
                _ => None,
            })
            .filter(|m| ids.contains(&m.id))
            .map(|m| m.thread_id.as_str())
            .collect();
        let mut targets = ids.to_vec();
        for message in &self.messages {
            if heads.contains(message.thread_id.as_str()) && !targets.contains(&message.id) {
                targets.push(message.id);
            }
        }
        targets
    }

    fn step(&mut self, delta: isize, extend: bool, cx: &mut Context<Self>) {
        let ids = self.visible_ids();
        if ids.is_empty() {
            return;
        }
        let current = self
            .anchor
            .and_then(|anchor| ids.iter().position(|&x| x == anchor));
        let next = match current {
            Some(index) => index.saturating_add_signed(delta).min(ids.len() - 1),
            None => 0,
        };
        let id = ids[next];
        if extend && !self.selection.is_empty() {
            if !self.selection.contains(&id) {
                self.selection.push(id);
            }
            self.anchor = Some(id);
            if let Some(row) = self.row_of(id) {
                self.scroll_handle
                    .scroll_to_item(row, ScrollStrategy::Nearest);
            }
            self.state
                .update(cx, |state, cx| state.select_message(Some(id), cx));
            cx.notify();
        } else {
            self.set_selection(vec![id], Some(id), cx);
        }
    }

    /// Runs a removal (delete / archive) then selects the next message, like Outlook.
    fn remove_selection(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        remove: impl FnOnce(&Entity<AppState>, Vec<Id>, &mut Window, &mut App),
    ) {
        if self.selection.is_empty() {
            return;
        }
        let ids = self.visible_ids();
        let selected: HashSet<Id> = self.selection.iter().copied().collect();
        let last = ids.iter().rposition(|id| selected.contains(id));
        let next = last
            .and_then(|last| ids[last + 1..].iter().find(|id| !selected.contains(id)))
            .or_else(|| ids.iter().rev().find(|id| !selected.contains(id)))
            .copied();
        let targets = self.selected_targets();
        let removed: HashSet<Id> = targets.iter().copied().collect();
        let next = next.filter(|id| !removed.contains(id));
        remove(&self.state, targets, window, cx);
        self.refresh(cx);
        if self.selection.is_empty()
            && let Some(next) = next.filter(|id| self.message(*id).is_some())
        {
            self.set_selection(vec![next], Some(next), cx);
        }
    }

    /// Runs `run` on the selection, or lets the shell's default handler run when it is empty.
    fn selection_or_propagate(
        &mut self,
        cx: &mut Context<Self>,
        run: impl FnOnce(&mut Self, Vec<Id>, &mut Context<Self>),
    ) {
        if self.selection.is_empty() {
            cx.propagate();
        } else {
            let targets = self.selected_targets();
            run(self, targets, cx);
        }
    }

    fn toggle_thread(&mut self, thread_id: &str, expand: Option<bool>, cx: &mut Context<Self>) {
        let expanded = self.expanded.contains(thread_id);
        let want = expand.unwrap_or(!expanded);
        if want == expanded {
            return;
        }
        if want {
            self.expanded.insert(thread_id.to_owned());
        } else {
            self.expanded.remove(thread_id);
        }
        self.rebuild_rows();
        cx.notify();
    }

    fn anchor_thread(&self) -> Option<String> {
        self.anchor
            .and_then(|id| self.message(id))
            .map(|m| m.thread_id.clone())
    }

    // ---------- rendering ----------

    fn render_header_row(&self, label: &str, count: usize, t: &Theme) -> AnyElement {
        div()
            .flex()
            .flex_row()
            .items_end()
            .gap(SPACE_2)
            .h(px(HEADER_HEIGHT))
            .px(SPACE_3)
            .pb(SPACE_1)
            .text_size(TEXT_XS)
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(t.text_muted)
            .child(label.to_owned())
            .child(
                div()
                    .text_color(t.text_subtle)
                    .font_weight(FontWeight::NORMAL)
                    .child(count.to_string()),
            )
            .into_any_element()
    }

    fn category_chips(&self, message: &Message) -> Vec<AnyElement> {
        let mut chips: Vec<AnyElement> = message
            .categories
            .iter()
            .take(2)
            .map(|name| {
                let color = self
                    .category_colors
                    .get(name)
                    .map_or_else(|| crate::components::avatar_color(name), |c| hex(*c));
                Badge::new(name.clone()).color(color).into_any_element()
            })
            .collect();
        if message.categories.len() > 2 {
            chips.push(Badge::count((message.categories.len() - 2) as i64).into_any_element());
        }
        chips
    }

    #[allow(clippy::too_many_arguments)]
    fn render_message_row(
        &self,
        message: &Message,
        child: bool,
        thread_len: usize,
        thread_unread: usize,
        expanded: bool,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = message.id;
        let selected = self.selection.contains(&id);
        let unread = !message.flags.read || (!child && !expanded && thread_unread > 0);
        let flagged = message.flags.flagged;
        let due = self.dues.get(&id).copied().filter(|_| flagged);
        let thread_id = message.thread_id.clone();
        let sender = message.from.display().to_owned();
        let list = cx.entity();
        let menu_state = self.state.clone();
        let drag_ids = self.targets_for(id);
        let drag_label: SharedString = if drag_ids.len() == 1 {
            message.subject.clone().into()
        } else {
            format!("{} messages", drag_ids.len()).into()
        };

        // A collapsed conversation row marks the whole conversation read / unread.
        let thread_ids = if child {
            vec![id]
        } else {
            self.with_conversations(&[id])
        };

        // Quick actions (visible on hover).
        let quick = {
            let thread_ids = thread_ids.clone();
            let (s1, s2, s3) = (self.state.clone(), self.state.clone(), list.clone());
            div()
                .absolute()
                .top(px(6.))
                .right(px(8.))
                .flex()
                .flex_row()
                .gap(px(2.))
                .p(px(2.))
                .rounded(RADIUS_MD)
                .bg(t.elevated)
                .border_1()
                .border_color(t.border_subtle)
                .shadow_sm()
                .opacity(0.)
                .group_hover("message-row", |s| s.opacity(1.))
                .child(
                    IconButton::new(
                        ("quick-read", id as u64),
                        if unread {
                            IconName::MailOpen
                        } else {
                            IconName::Mail
                        },
                    )
                    .size(px(24.))
                    .icon_size(ICON_SIZE_SM)
                    .tooltip(if unread {
                        "Mark as read"
                    } else {
                        "Mark as unread"
                    })
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        ops::set_read(&s1, thread_ids.clone(), unread, cx);
                    }),
                )
                .child(
                    IconButton::new(("quick-flag", id as u64), IconName::Flag)
                        .size(px(24.))
                        .icon_size(ICON_SIZE_SM)
                        .active(flagged)
                        .tooltip(if flagged { "Clear flag" } else { "Flag" })
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            ops::set_flag(&s2, vec![id], !flagged, cx);
                        }),
                )
                .child(
                    IconButton::new(("quick-delete", id as u64), IconName::Trash)
                        .size(px(24.))
                        .icon_size(ICON_SIZE_SM)
                        .tooltip("Delete")
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            s3.update(cx, |this, cx| {
                                if !this.selection.contains(&id) {
                                    this.selection = vec![id];
                                    this.anchor = Some(id);
                                }
                                this.remove_selection(window, cx, ops::delete);
                            });
                        }),
                )
        };

        // Left edge: unread marker, click toggles read.
        let edge_state = self.state.clone();
        let edge_ids = thread_ids.clone();
        let edge = div()
            .id(("read-edge", id as u64))
            .absolute()
            .left(px(0.))
            .top(px(0.))
            .bottom(px(0.))
            .w(px(10.))
            .cursor_pointer()
            .flex()
            .items_center()
            .child(
                div()
                    .w(px(3.))
                    .h_full()
                    .when(unread, |el| el.bg(t.accent))
                    .when(!unread, |el| {
                        el.group_hover("message-row", |s| s.bg(t.border))
                    }),
            )
            .on_click(move |_, _, cx| {
                cx.stop_propagation();
                ops::set_read(&edge_state, edge_ids.clone(), unread, cx);
            });

        let icons = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(3.))
            .flex_shrink_0()
            .when(message.flags.pinned, |el| {
                el.child(Icon::new(IconName::Pin).size(px(12.)).text_color(t.accent))
            })
            .when(self.muted.contains(&message.thread_id), |el| {
                el.child(
                    Icon::new(IconName::BellOff)
                        .size(px(12.))
                        .text_color(t.text_subtle),
                )
            })
            .when(message.importance == Importance::High, |el| {
                el.child(
                    Icon::new(IconName::CircleAlert)
                        .size(px(12.))
                        .text_color(t.danger),
                )
            })
            .when(message.importance == Importance::Low, |el| {
                el.child(
                    Icon::new(IconName::ArrowDown)
                        .size(px(12.))
                        .text_color(t.text_subtle),
                )
            })
            .when(!message.attachments.is_empty(), |el| {
                el.child(
                    Icon::new(IconName::Paperclip)
                        .size(px(12.))
                        .text_color(t.text_subtle),
                )
            });

        let time = div()
            .flex_shrink_0()
            .text_size(TEXT_SM)
            .text_color(if unread { t.accent } else { t.text_subtle })
            .when(unread, |el| el.font_weight(FontWeight::MEDIUM))
            .child(short_date(message.date));

        let first_line = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(SPACE_1_5)
            .text_size(TEXT_MD)
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .when(unread, |el| el.font_weight(FontWeight::SEMIBOLD))
                    .child(sender.clone()),
            )
            .when(thread_len > 1, |el| {
                el.child(
                    div()
                        .flex_shrink_0()
                        .px(px(5.))
                        .rounded(RADIUS_FULL)
                        .bg(t.active)
                        .text_size(TEXT_XS)
                        .text_color(t.text_muted)
                        .child(thread_len.to_string()),
                )
            })
            .child(icons)
            .child(time);

        let flag_view = flagged.then(|| {
            let (label, urgent) = due.map_or((String::new(), false), due_label);
            div()
                .flex()
                .flex_row()
                .flex_shrink_0()
                .items_center()
                .gap(px(3.))
                .text_size(TEXT_XS)
                .text_color(if urgent { t.danger } else { t.text_muted })
                .child(Icon::new(IconName::Flag).size(px(12.)).text_color(t.danger))
                .when(!label.is_empty(), |el| el.child(label))
        });

        let thread_toggle = (thread_len > 1 && !child).then(|| {
            let list = list.clone();
            let thread_id = thread_id.clone();
            div()
                .id(("thread-toggle", id as u64))
                .flex()
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .size(px(16.))
                .rounded(RADIUS_MD)
                .text_color(t.text_muted)
                .hover(|s| s.bg(t.active))
                .child(
                    Icon::new(if expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .size(px(12.)),
                )
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    list.update(cx, |this, cx| this.toggle_thread(&thread_id, None, cx));
                })
        });

        let subject_line = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(SPACE_1)
            .text_size(TEXT_MD)
            .when_some(thread_toggle, |el, toggle| el.child(toggle))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .when(unread, |el| {
                        el.font_weight(FontWeight::MEDIUM).text_color(t.accent)
                    })
                    .child(if message.subject.is_empty() {
                        "(no subject)".to_owned()
                    } else {
                        message.subject.clone()
                    }),
            )
            .when_some(flag_view, |el, flag| el.child(flag))
            // Compact rows have no preview line: categories move up here.
            .when(self.compact, |el| el.children(self.category_chips(message)));

        let preview_line = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(SPACE_1)
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .text_size(TEXT_SM)
                    .text_color(t.text_muted)
                    .child(message.preview.clone()),
            )
            .children(self.category_chips(message));

        let body = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .gap(px(2.))
            .child(first_line)
            .when(!child, |el| el.child(subject_line))
            .when(!self.compact, |el| el.child(preview_line));

        div()
            .id(("message", id as u64))
            .group("message-row")
            .relative()
            .flex()
            .flex_row()
            .items_start()
            .gap(SPACE_3)
            .w_full()
            .h(px(self.row_height(child)))
            .pl(if child { px(52.) } else { SPACE_3 })
            .pr(SPACE_3)
            .py(SPACE_2)
            .border_b_1()
            .border_color(t.border_subtle)
            .cursor_pointer()
            .when(selected, |el| el.bg(t.accent_subtle))
            .when(!selected, |el| el.hover(|s| s.bg(t.hover)))
            .when(child && !selected, |el| el.bg(t.surface.alpha(0.6)))
            .child(edge)
            .when(!child, |el| {
                el.child(
                    div()
                        .pt(px(2.))
                        .child(Avatar::new(sender.clone()).size(px(32.))),
                )
            })
            .child(body)
            .child(quick)
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.focus_handle.focus(window, cx);
                if event.click_count() >= 2 && crate::compose::is_draft(this.state.read(cx), id) {
                    crate::compose::edit_draft(&this.state, id, window, cx);
                } else if event.click_count() >= 2 {
                    message_window::open(this.state.clone(), id, cx);
                } else {
                    this.click(id, event.modifiers(), cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, _, cx| {
                    if !this.selection.contains(&id) {
                        this.selection = vec![id];
                        this.anchor = Some(id);
                        this.state
                            .update(cx, |state, cx| state.select_message(Some(id), cx));
                        cx.notify();
                    }
                }),
            )
            .on_drag(
                DraggedMessages {
                    ids: drag_ids,
                    label: drag_label,
                },
                |dragged, _, _, cx| cx.new(|_| dragged.clone()),
            )
            .context_menu(move |menu, window, cx| {
                let view = list.read(cx);
                let ids = view.targets_for(id);
                let single =
                    (view.selection.len() <= 1 || !view.selection.contains(&id)).then_some(id);
                message_menu::build(menu, &menu_state, ids, single, window, cx)
            })
            .into_any_element()
    }

    fn render_rows(
        &mut self,
        range: std::ops::Range<usize>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let t = theme(cx).clone();
        let rows = self.rows.clone();
        range
            .filter_map(|ix| rows.get(ix).map(|row| (ix, row)))
            .map(|(_, row)| match row {
                Row::Header { label, count } => self.render_header_row(label, *count, &t),
                Row::Message {
                    index,
                    child,
                    thread_len,
                    thread_unread,
                    expanded,
                } => match self.messages.get(*index).cloned() {
                    Some(message) => self.render_message_row(
                        &message,
                        *child,
                        *thread_len,
                        *thread_unread,
                        *expanded,
                        &t,
                        cx,
                    ),
                    None => div().into_any_element(),
                },
            })
            .collect()
    }

    fn options_menu(&self, menu: PopupMenu, list: Entity<Self>) -> PopupMenu {
        let options = self.options;
        let set = move |list: &Entity<Self>, options: ListOptions, cx: &mut App| {
            list.update(cx, |this, cx| this.set_options(options, cx));
        };
        let mut menu = menu.min_w(px(200.)).label("Sort by");
        for key in SortKey::ALL {
            let list = list.clone();
            menu = menu.item(
                PopupMenuItem::new(key.label())
                    .checked(options.sort == key)
                    .on_click(move |_, _, cx| {
                        set(
                            &list,
                            ListOptions {
                                sort: key,
                                ..options
                            },
                            cx,
                        )
                    }),
            );
        }
        let (asc_list, desc_list) = (list.clone(), list.clone());
        menu = menu
            .separator()
            .item(
                PopupMenuItem::new(if options.sort == SortKey::Date {
                    "Newest on top"
                } else {
                    "Descending"
                })
                .checked(options.descending)
                .on_click(move |_, _, cx| {
                    set(
                        &desc_list,
                        ListOptions {
                            descending: true,
                            ..options
                        },
                        cx,
                    )
                }),
            )
            .item(
                PopupMenuItem::new(if options.sort == SortKey::Date {
                    "Oldest on top"
                } else {
                    "Ascending"
                })
                .checked(!options.descending)
                .on_click(move |_, _, cx| {
                    set(
                        &asc_list,
                        ListOptions {
                            descending: false,
                            ..options
                        },
                        cx,
                    )
                }),
            )
            .separator()
            .label("Group by");
        for grouping in Grouping::ALL {
            let list = list.clone();
            menu = menu.item(
                PopupMenuItem::new(grouping.label())
                    .checked(options.grouping == grouping)
                    .on_click(move |_, _, cx| {
                        set(
                            &list,
                            ListOptions {
                                grouping,
                                ..options
                            },
                            cx,
                        )
                    }),
            );
        }
        let state = self.state.clone();
        menu.separator().item(
            // No icon: it would replace the check mark.
            PopupMenuItem::new("Show as conversations")
                .checked(options.threaded)
                .on_click(move |_, _, cx| {
                    update_settings(&state, cx, |settings| {
                        settings.conversations = !options.threaded
                    })
                }),
        )
    }

    fn layout_menu(
        menu: PopupMenu,
        state: Entity<AppState>,
        current: ReadingPanePosition,
    ) -> PopupMenu {
        let mut menu = menu.min_w(px(180.)).label("Reading pane");
        for (position, label, icon) in [
            (ReadingPanePosition::Right, "Right", IconName::PanelRight),
            (ReadingPanePosition::Bottom, "Bottom", IconName::PanelBottom),
            (ReadingPanePosition::Off, "Off", IconName::EyeOff),
        ] {
            let state = state.clone();
            menu = menu.item(
                PopupMenuItem::new(label)
                    .icon(icon)
                    .checked(current == position)
                    .on_click(move |_, _, cx| {
                        update_settings(&state, cx, |settings| settings.reading_pane = position);
                    }),
            );
        }
        menu
    }

    fn render_toolbar(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let prefs = self.prefs.read(cx);
        let folders_collapsed = prefs.folders_collapsed;
        let pane_position = super::pane_position(cx);
        let unread = self.messages.iter().filter(|m| !m.flags.read).count();
        let list = cx.entity();
        let layout_state = self.state.clone();
        let expand_prefs = self.prefs.clone();

        div()
            .flex()
            .flex_row()
            .flex_shrink_0()
            .items_center()
            .gap(SPACE_2)
            .h(px(40.))
            .px(SPACE_3)
            .border_b_1()
            .border_color(t.border_subtle)
            .when(folders_collapsed, |el| {
                el.child(
                    IconButton::new("list-show-folders", IconName::PanelLeftOpen)
                        .icon_size(ICON_SIZE_SM)
                        .tooltip("Show folders")
                        .on_click(move |_, _, cx| {
                            expand_prefs.update(cx, |prefs, cx| {
                                prefs.folders_collapsed = false;
                                cx.notify();
                            });
                        }),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .text_size(TEXT_LG)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.title.clone()),
            )
            .when(unread > 0, |el| {
                el.child(
                    div()
                        .text_size(TEXT_SM)
                        .text_color(t.text_subtle)
                        .child(format!("{unread} unread")),
                )
            })
            .child(
                KitButton::new("list-options")
                    .ghost()
                    .small()
                    .icon(IconName::ArrowDownWideNarrow)
                    .tooltip("Sort, group and conversations")
                    .dropdown_menu({
                        let list = list.clone();
                        move |menu, _, cx| {
                            let view = list.read(cx);
                            view.options_menu(menu, list.clone())
                        }
                    }),
            )
            .child(
                KitButton::new("list-layout")
                    .ghost()
                    .small()
                    .icon(match pane_position {
                        ReadingPanePosition::Right => IconName::PanelRight,
                        ReadingPanePosition::Bottom => IconName::PanelBottom,
                        ReadingPanePosition::Off => IconName::EyeOff,
                    })
                    .tooltip("Reading pane")
                    .dropdown_menu(move |menu, _, _| {
                        Self::layout_menu(menu, layout_state.clone(), pane_position)
                    }),
            )
    }

    fn render_filters(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let current = self.options.filter;
        div()
            .flex()
            .flex_row()
            .flex_shrink_0()
            .items_center()
            .gap(SPACE_1)
            .h(px(36.))
            .px(SPACE_3)
            .border_b_1()
            .border_color(t.border_subtle)
            .children(Filter::ALL.into_iter().map(|filter| {
                let active = filter == current;
                div()
                    .id(("filter", filter as u64))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(SPACE_1)
                    .h(px(24.))
                    .px(SPACE_2)
                    .rounded(RADIUS_FULL)
                    .cursor_pointer()
                    .text_size(TEXT_SM)
                    .when(active, |el| {
                        el.bg(t.accent_subtle)
                            .text_color(t.accent)
                            .font_weight(FontWeight::MEDIUM)
                    })
                    .when(!active, |el| {
                        el.text_color(t.text_muted)
                            .hover(|s| s.bg(t.hover).text_color(t.text))
                    })
                    .when(filter == Filter::Mentions, |el| {
                        el.child(Icon::new(IconName::AtSign).size(px(12.)))
                    })
                    .when(filter == Filter::Attachments, |el| {
                        el.child(Icon::new(IconName::Paperclip).size(px(12.)))
                    })
                    .when(filter == Filter::Flagged, |el| {
                        el.child(Icon::new(IconName::Flag).size(px(12.)))
                    })
                    .child(filter.label())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let options = ListOptions {
                            filter,
                            ..this.options
                        };
                        this.set_options(options, cx);
                    }))
            }))
    }

    fn render_selection_bar(&self, t: &Theme, cx: &mut Context<Self>) -> Option<Div> {
        if self.selection.len() < 2 {
            return None;
        }
        let count = self.selection.len();
        let ids = self.selected_targets();
        let (read_ids, flag_ids) = (ids.clone(), ids.clone());
        let (s1, s2) = (self.state.clone(), self.state.clone());
        Some(
            div()
                .flex()
                .flex_row()
                .flex_shrink_0()
                .items_center()
                .gap(SPACE_1)
                .h(px(36.))
                .px(SPACE_3)
                .bg(t.accent_subtle)
                .border_b_1()
                .border_color(t.border_subtle)
                .child(
                    div()
                        .flex_1()
                        .text_size(TEXT_SM)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.accent)
                        .child(format!("{count} selected")),
                )
                .child(
                    IconButton::new("bulk-read", IconName::MailOpen)
                        .icon_size(ICON_SIZE_SM)
                        .tooltip("Mark as read / unread")
                        .on_click(move |_, _, cx| ops::toggle_read(&s1, read_ids.clone(), cx)),
                )
                .child(
                    IconButton::new("bulk-flag", IconName::Flag)
                        .icon_size(ICON_SIZE_SM)
                        .tooltip("Flag / clear flag")
                        .on_click(move |_, _, cx| ops::toggle_flag(&s2, flag_ids.clone(), cx)),
                )
                .child(
                    IconButton::new("bulk-archive", IconName::Archive)
                        .icon_size(ICON_SIZE_SM)
                        .tooltip("Archive")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.remove_selection(window, cx, |s, ids, _, cx| {
                                ops::move_to_role(s, ids, FolderRole::Archive, cx)
                            })
                        })),
                )
                .child(
                    IconButton::new("bulk-delete", IconName::Trash)
                        .icon_size(ICON_SIZE_SM)
                        .tooltip("Delete")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.remove_selection(window, cx, ops::delete)
                        })),
                )
                .child(
                    IconButton::new("bulk-clear", IconName::X)
                        .icon_size(ICON_SIZE_SM)
                        .tooltip("Clear selection")
                        .on_click(cx.listener(|this, _, _, cx| {
                            let primary = this.anchor;
                            this.set_selection(primary.into_iter().collect(), primary, cx);
                        })),
                ),
        )
    }
}

impl Focusable for MessageListView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for MessageListView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.sync_settings(cx) {
            self.rebuild_rows();
        }
        let t = theme(cx).clone();
        let toolbar = self.render_toolbar(&t, cx);
        let filters = self.render_filters(&t, cx);
        let selection_bar = self.render_selection_bar(&t, cx);

        let body = if self.rows.is_empty() {
            let (icon, title, description) = if !self.source.1.is_empty() {
                (IconName::Search, "No results", "Try another search.")
            } else if self.options.filter != Filter::All {
                (
                    IconName::ListFilter,
                    "Nothing here",
                    "No message matches this filter.",
                )
            } else if self.source.0.is_none() {
                (
                    IconName::Inbox,
                    "No folder selected",
                    "Pick a folder on the left.",
                )
            } else {
                (IconName::Inbox, "All caught up", "This folder is empty.")
            };
            div()
                .flex_1()
                .min_h(px(0.))
                .child(EmptyState::new(icon, title).description(description))
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_h(px(0.))
                .child(
                    v_virtual_list(
                        cx.entity(),
                        "message-rows",
                        self.sizes.clone(),
                        |this, range, _window, cx| this.render_rows(range, cx),
                    )
                    .track_scroll(&self.scroll_handle)
                    .size_full(),
                )
                .into_any_element()
        };

        div()
            .id("message-list")
            .key_context("MessageList")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.step(1, false, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.step(-1, false, cx)))
            .on_action(cx.listener(|this, _: &ExtendNext, _, cx| this.step(1, true, cx)))
            .on_action(cx.listener(|this, _: &ExtendPrevious, _, cx| this.step(-1, true, cx)))
            .on_action(cx.listener(|this, _: &SelectAllMessages, _, cx| {
                let ids = this.visible_ids();
                let primary = this.anchor.or_else(|| ids.first().copied());
                this.selection = ids;
                this.anchor = primary;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ClearSelection, _, cx| {
                let primary = this.anchor;
                this.set_selection(primary.into_iter().collect(), primary, cx);
            }))
            .on_action(cx.listener(|this, _: &DeleteSelected, window, cx| {
                this.remove_selection(window, cx, ops::delete)
            }))
            .on_action(cx.listener(|this, _: &ArchiveSelected, window, cx| {
                this.remove_selection(window, cx, |s, ids, _, cx| {
                    ops::move_to_role(s, ids, FolderRole::Archive, cx)
                })
            }))
            .on_action(cx.listener(|this, _: &MarkRead, _, cx| {
                ops::set_read(&this.state, this.selected_targets(), true, cx)
            }))
            .on_action(cx.listener(|this, _: &MarkUnread, _, cx| {
                ops::set_read(&this.state, this.selected_targets(), false, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleFlag, _, cx| {
                ops::toggle_flag(&this.state, this.selection.clone(), cx)
            }))
            // App actions (ribbon, palette, shortcuts) act on the whole selection here.
            .on_action(
                cx.listener(|this, _: &actions::DeleteSelection, window, cx| {
                    this.selection_or_propagate(cx, |this, _, cx| {
                        this.remove_selection(window, cx, ops::delete)
                    })
                }),
            )
            .on_action(cx.listener(|this, _: &actions::Archive, window, cx| {
                this.selection_or_propagate(cx, |this, _, cx| {
                    this.remove_selection(window, cx, |s, ids, _, cx| {
                        ops::move_to_role(s, ids, FolderRole::Archive, cx)
                    })
                })
            }))
            .on_action(cx.listener(|this, _: &actions::ToggleFlag, _, cx| {
                this.selection_or_propagate(cx, |this, ids, cx| {
                    ops::toggle_flag(&this.state, ids, cx)
                })
            }))
            .on_action(cx.listener(|this, _: &actions::MarkRead, _, cx| {
                this.selection_or_propagate(cx, |this, ids, cx| {
                    ops::set_read(&this.state, ids, true, cx)
                })
            }))
            .on_action(cx.listener(|this, _: &actions::MarkUnread, _, cx| {
                this.selection_or_propagate(cx, |this, ids, cx| {
                    ops::set_read(&this.state, ids, false, cx)
                })
            }))
            .on_action(cx.listener(|this, _: &actions::MoveToFolder, window, cx| {
                this.selection_or_propagate(cx, |this, ids, cx| {
                    message_menu::open_move_dialog(this.state.clone(), ids, window, cx)
                })
            }))
            .on_action(cx.listener(|this, _: &actions::Categorize, window, cx| {
                this.selection_or_propagate(cx, |this, ids, cx| {
                    message_menu::open_category_dialog(this.state.clone(), ids, window, cx)
                })
            }))
            .on_action(cx.listener(|this, _: &OpenSelected, _, cx| {
                if let Some(id) = this.anchor {
                    message_window::open(this.state.clone(), id, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ExpandThread, _, cx| {
                if let Some(thread) = this.anchor_thread() {
                    this.toggle_thread(&thread, Some(true), cx);
                }
            }))
            .on_action(cx.listener(|this, _: &CollapseThread, _, cx| {
                if let Some(thread) = this.anchor_thread() {
                    this.toggle_thread(&thread, Some(false), cx);
                }
            }))
            .flex()
            .flex_col()
            .size_full()
            .child(toolbar)
            .child(filters)
            .children(selection_bar)
            .child(body)
    }
}
