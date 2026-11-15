//! People module: sidebar (all, favourites, recent correspondents, duplicates, groups,
//! categories, import/export, CardDAV) | virtualized A–Z contact list with search and
//! multi-select (Ctrl/Shift+click, Delete) | contact card, editor or bulk actions.
//!
//! Writes go through `Command::People` (undoable). "Email" and "Schedule meeting" do not
//! compose or create events here: they emit [`PeopleEvent`]s that the compose and calendar
//! modules handle by subscribing to the `PeopleView` entity.

mod card;
mod form;

use crate::settings::prefs;
use mailbox_core::dates::LocalizedFormat;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::component::WindowExt as _;
use gpui::component::button::{Button as KitButton, ButtonVariants as _};
use gpui::component::input::{Input, InputEvent, InputState};
use gpui::component::notification::Notification;
use gpui::component::tooltip::Tooltip;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::people::{self, ContactCard, ContactGroup, Suggestion};
use mailbox_core::{Address, Command, Id, PeopleCommand, vcard};

use crate::app_state::{AppState, Module};
use crate::components::{Avatar, Button, EmptyState, Icon, IconButton, IconName};
use crate::settings::tr;
use crate::theme::{
    DISABLED_OPACITY, ICON_SIZE, ICON_SIZE_SM, NAV_ROW_HEIGHT, PANE_HEADER_HEIGHT, RADIUS_FULL,
    RADIUS_MD, RADIUS_SM, SPACE_0_5, SPACE_1, SPACE_2, SPACE_3, TEXT_2XS, TEXT_LG, TEXT_MD,
    TEXT_SM, TEXT_XS, Theme, theme,
};

use form::ContactForm;

/// Height of a contact row (the list is a uniform, virtualized list).
const ROW_HEIGHT: Pixels = px(52.);
/// Height of the visible row inside it (same as the Tasks rows).
const LIST_ROW_HEIGHT: Pixels = px(48.);
/// Jump-bar letters, in list order.
const LETTERS: [char; 27] = [
    'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', 'P', 'Q', 'R', 'S',
    'T', 'U', 'V', 'W', 'X', 'Y', 'Z', '#',
];

/// Hooks for other modules (subscribe with `cx.subscribe(&people_view, ...)`).
#[derive(Debug, Clone)]
pub enum PeopleEvent {
    /// "Email": start a new message to these recipients (compose module).
    Compose(Vec<Address>),
    /// "Schedule meeting": start a new event with these attendees (calendar module).
    ScheduleMeeting(Vec<Address>),
}

#[derive(Debug, Clone, PartialEq)]
enum Filter {
    All,
    Favorites,
    /// People you mailed with who are not contacts yet.
    Recent,
    Duplicates,
    Group(Id),
    Category(String),
}

/// One visible list row, precomputed in `render` for the virtualized list.
struct Row {
    id: Id,
    name: SharedString,
    subtitle: SharedString,
    favorite: bool,
    /// Section letter, set on the first row of each section.
    letter: Option<char>,
}

pub struct PeopleView {
    state: Entity<AppState>,
    filter: Filter,
    selected: Option<Id>,
    search: Entity<InputState>,
    new_group: Entity<InputState>,
    form: Option<ContactForm>,
    rows: Vec<Row>,
    scroll: UniformListScrollHandle,
    /// Decoded photo of the open card, keyed by its data URI.
    photo_cache: Option<(String, Arc<Image>)>,
    /// Multi-selection (Ctrl/Shift+click), in click order.
    checked: Vec<Id>,
    /// Last plainly clicked row: start of a Shift+click range.
    anchor: Option<Id>,
    list_focus: FocusHandle,
    /// Whole module: receives the shared "New" / "Delete" actions (ribbon, Delete key).
    root_focus: FocusHandle,
    /// Contact editor: clicks on its non-text parts keep Enter / Esc working.
    form_focus: FocusHandle,
    /// People is the current module (to focus it once when it becomes active).
    active: bool,
    /// "Recent contacts", recomputed after each store change.
    suggestions: Option<Arc<Vec<Suggestion>>>,
    _subscriptions: Vec<Subscription>,
}

/// How many recent correspondents the "Recent contacts" list shows.
const MAX_SUGGESTIONS: usize = 200;

impl EventEmitter<PeopleEvent> for PeopleView {}

/// The People view, for app-level handlers (ribbon buttons, Delete key) that run while
/// focus is outside it.
struct ActiveView(WeakEntity<PeopleView>);

impl Global for ActiveView {}

fn with_view(cx: &mut App, f: impl FnOnce(&mut PeopleView, &mut Context<PeopleView>)) -> bool {
    let view = cx
        .try_global::<ActiveView>()
        .and_then(|active| active.0.upgrade());
    match view {
        Some(view) => {
            view.update(cx, f);
            true
        }
        None => false,
    }
}

/// "New contact" from anywhere (ribbon, command palette). False without a People view.
pub fn new_contact(window: &mut Window, cx: &mut App) -> bool {
    with_view(cx, |view, cx| {
        view.state
            .update(cx, |state, cx| state.set_module(Module::People, cx));
        view.new_contact(window, cx)
    })
}

/// "Delete" while People is active: the selected contacts. False without a People view.
pub fn delete_selection(window: &mut Window, cx: &mut App) -> bool {
    with_view(cx, |view, cx| view.delete_selection(window, cx))
}

impl PeopleView {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(tr(cx, "Search people")));
        let new_group = cx.new(|cx| InputState::new(window, cx).placeholder(tr(cx, "New group")));
        let subscriptions = vec![
            cx.observe_in(&state, window, |this: &mut Self, state, window, cx| {
                this.suggestions = None;
                // Entering People focuses it, so "New" / "Delete" (ribbon, keys) reach it.
                let active = state.read(cx).module() == Module::People;
                if active && !this.active && this.form.is_none() {
                    this.root_focus.focus(window, cx);
                }
                this.active = active;
                cx.notify();
            }),
            cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
                if let InputEvent::Change = event {
                    cx.notify();
                }
            }),
            cx.subscribe_in(
                &new_group,
                window,
                |this, input, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        let name = input.read(cx).value().trim().to_owned();
                        if !name.is_empty() && this.create_group(name, cx) {
                            input.update(cx, |input, cx| input.set_value("", window, cx));
                        }
                    }
                },
            ),
        ];
        cx.set_global(ActiveView(cx.entity().downgrade()));
        Self {
            state,
            filter: Filter::All,
            selected: None,
            search,
            new_group,
            form: None,
            rows: Vec::new(),
            scroll: UniformListScrollHandle::new(),
            photo_cache: None,
            checked: Vec::new(),
            anchor: None,
            list_focus: cx.focus_handle(),
            root_focus: cx.focus_handle(),
            form_focus: cx.focus_handle(),
            active: false,
            suggestions: None,
            _subscriptions: subscriptions,
        }
    }

    fn suggestions(&mut self, cards: &[ContactCard], cx: &App) -> Arc<Vec<Suggestion>> {
        if let Some(cached) = &self.suggestions {
            return cached.clone();
        }
        let found = people::suggested_contacts(self.state.read(cx).store(), cards, MAX_SUGGESTIONS)
            .unwrap_or_default();
        let found = Arc::new(found);
        self.suggestions = Some(found.clone());
        found
    }

    // ---------- actions ----------

    fn run(&mut self, command: PeopleCommand, cx: &mut Context<Self>) -> bool {
        self.state
            .update(cx, |state, cx| state.run(Command::People(command), cx))
    }

    fn select(&mut self, id: Option<Id>, cx: &mut Context<Self>) {
        self.selected = id;
        self.anchor = id;
        self.checked.clear();
        self.form = None;
        cx.notify();
    }

    /// Plain click selects; Ctrl+click toggles; Shift+click extends from the anchor.
    fn click_row(
        &mut self,
        id: Id,
        modifiers: Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.list_focus.focus(window, cx);
        if modifiers.shift {
            let position = |target: Id| self.rows.iter().position(|row| row.id == target);
            let anchor = self.anchor.or(self.selected).and_then(position);
            if let (Some(from), Some(to)) = (anchor, position(id)) {
                let (start, end) = (from.min(to), from.max(to));
                self.checked = self.rows[start..=end].iter().map(|row| row.id).collect();
                self.form = None;
                cx.notify();
                return;
            }
        }
        if modifiers.control || modifiers.platform {
            if self.checked.is_empty()
                && let Some(current) = self.selected
            {
                self.checked.push(current);
            }
            if let Some(index) = self.checked.iter().position(|checked| *checked == id) {
                self.checked.remove(index);
            } else {
                self.checked.push(id);
            }
            self.anchor = Some(id);
            self.selected = self.checked.last().copied();
            self.form = None;
            cx.notify();
            return;
        }
        self.select(Some(id), cx);
    }

    /// Contacts the bulk actions apply to: the multi-selection, else the open contact.
    fn targets(&self) -> Vec<Id> {
        if self.checked.is_empty() {
            self.selected.into_iter().collect()
        } else {
            self.checked.clone()
        }
    }

    fn on_list_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let _ = window;
        match event.keystroke.key.as_str() {
            "escape" if !self.checked.is_empty() => {
                self.checked.clear();
                cx.notify();
            }
            "a" if event.keystroke.modifiers.control || event.keystroke.modifiers.platform => {
                self.checked = self.rows.iter().map(|row| row.id).collect();
                cx.notify();
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    /// Delete key / ribbon "Delete": the multi-selection or the open contact.
    fn delete_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.form.is_some() {
            return;
        }
        let targets = self.targets();
        if targets.is_empty() {
            let message = tr(cx, "Select a contact to delete");
            self.state
                .update(cx, |state, cx| state.notify_info(message, cx));
        } else {
            self.delete_contacts(targets, window, cx);
        }
    }

    fn set_filter(&mut self, filter: Filter, cx: &mut Context<Self>) {
        self.filter = filter;
        self.checked.clear();
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    /// Account new contacts are created in: the CardDAV one (so they sync), else the first.
    fn account_id(&self, cx: &App) -> Option<Id> {
        if let Some(account_id) = crate::dav_bridge::contacts_account(cx) {
            return Some(account_id);
        }
        let accounts = self.state.read(cx).store().list_accounts().ok()?;
        accounts.first().map(|account| account.id)
    }

    /// The first account, or an error toast explaining contacts need one.
    fn require_account(&mut self, cx: &mut Context<Self>) -> Option<Id> {
        let account_id = self.account_id(cx);
        if account_id.is_none() {
            let message = tr(
                cx,
                "Contacts are stored with an account: add a mail or CardDAV account first.",
            );
            self.report(message.to_owned(), cx);
        }
        account_id
    }

    fn new_contact(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(account_id) = self.require_account(cx) else {
            return;
        };
        self.checked.clear();
        let mut card = ContactCard::new(account_id);
        if let Filter::Category(category) = &self.filter {
            card.details.categories.push(category.clone());
        }
        self.form = Some(ContactForm::new(card, window, cx));
        cx.notify();
    }

    fn edit_contact(&mut self, id: Id, window: &mut Window, cx: &mut Context<Self>) {
        let card = people::contact_card(self.state.read(cx).store(), id);
        match card {
            Ok(Some(card)) => {
                self.checked.clear();
                self.form = Some(ContactForm::new(card, window, cx));
                cx.notify();
            }
            Ok(None) => {}
            Err(err) => self
                .state
                .update(cx, |state, cx| state.report_error(err, cx)),
        }
    }

    fn cancel_form(&mut self, cx: &mut Context<Self>) {
        self.form = None;
        cx.notify();
    }

    fn save_form(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.form.as_mut() else {
            return;
        };
        let card = match form.to_card(cx) {
            Ok(card) => card,
            Err(message) => {
                form.error = Some(message.into());
                cx.notify();
                return;
            }
        };
        let is_new = card.id() == 0;
        let group = match &self.filter {
            Filter::Group(group) if is_new => Some(*group),
            _ => None,
        };
        if !self.run(PeopleCommand::SaveContact(Box::new(card.clone())), cx) {
            return;
        }
        let id = if is_new {
            self.newest_contact_id(cx)
        } else {
            Some(card.id())
        };
        // A contact created while a group is open joins it (one more undo step).
        if let (Some(group), Some(id)) = (group, id) {
            self.toggle_member(group, id, cx);
        }
        self.form = None;
        self.selected = id;
        self.anchor = id;
        let noun = tr(
            cx,
            if is_new {
                "Contact created"
            } else {
                "Contact saved"
            },
        );
        let message = format!("{noun} · {}", tr(cx, "Ctrl+Z to undo"));
        self.state
            .update(cx, |state, cx| state.notify_info(message, cx));
        cx.notify();
    }

    fn newest_contact_id(&self, cx: &App) -> Option<Id> {
        let contacts = self.state.read(cx).store().list_contacts().ok()?;
        contacts.iter().map(|contact| contact.id).max()
    }

    fn delete_contacts(&mut self, ids: Vec<Id>, window: &mut Window, cx: &mut Context<Self>) {
        let count = ids.len();
        if self.run(PeopleCommand::DeleteContacts(ids), cx) {
            self.select(None, cx);
            let message = count_label(count, "Contact deleted", "{} contacts deleted", cx);
            self.undo_toast(message, window, cx);
        }
    }

    /// Info toast with an Undo button (the last command is undone).
    fn undo_toast(&self, message: String, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.clone();
        let notification = Notification::new()
            .message(message)
            .action(move |_, _, cx| {
                let state = state.clone();
                let notification = cx.entity();
                KitButton::new("people-undo")
                    .primary()
                    .label(tr(cx, "Undo"))
                    .on_click(move |_, window, cx| {
                        state.update(cx, |state, cx| state.undo(cx));
                        notification
                            .update(cx, |notification, cx| notification.dismiss(window, cx));
                    })
            })
            .autohide(true);
        window.push_notification(notification, cx);
    }

    /// Saves a "Recent contacts" suggestion as a contact, in one click.
    fn save_suggestion(&mut self, address: Address, cx: &mut Context<Self>) {
        let Some(account_id) = self.require_account(cx) else {
            return;
        };
        let card = people::card_from_address(&address, account_id);
        if self.run(PeopleCommand::SaveContact(Box::new(card)), cx) {
            let message = tr(cx, "Saved {} to contacts").replace("{}", &address.display());
            self.state
                .update(cx, |state, cx| state.notify_info(message, cx));
        }
    }

    /// Adds every target to a group (one undo step).
    fn add_to_group(&mut self, group_id: Id, ids: Vec<Id>, cx: &mut Context<Self>) {
        let group = self
            .state
            .read(cx)
            .store()
            .get::<ContactGroup>(group_id)
            .ok()
            .flatten();
        let Some(mut group) = group else {
            return;
        };
        let before = group.members.len();
        for id in ids {
            if !group.members.contains(&id) {
                group.members.push(id);
            }
        }
        let added = group.members.len() - before;
        let name = group.name.clone();
        if added > 0 && self.run(PeopleCommand::SaveGroup(Box::new(group)), cx) {
            let message = tr(cx, "Added {} to {}")
                .replacen("{}", &added.to_string(), 1)
                .replacen("{}", &name, 1);
            self.state
                .update(cx, |state, cx| state.notify_info(message, cx));
        }
    }

    fn compose_to(&mut self, ids: &[Id], cx: &mut Context<Self>) {
        let store = self.state.read(cx).store();
        let recipients: Vec<Address> = ids
            .iter()
            .filter_map(|id| people::contact_card(store, *id).ok().flatten())
            .filter_map(|card| {
                card.primary_email()
                    .map(|email| Address::new(card.name(), email))
            })
            .collect();
        if recipients.is_empty() {
            let message = tr(cx, "None of these contacts has an email address");
            self.report(message.to_owned(), cx);
        } else {
            self.emit_compose(recipients, cx);
        }
    }

    fn create_group(&mut self, name: String, cx: &mut Context<Self>) -> bool {
        let group = ContactGroup {
            id: 0,
            name,
            members: Vec::new(),
        };
        if !self.run(PeopleCommand::SaveGroup(Box::new(group)), cx) {
            return false;
        }
        let newest = people::list_contact_groups(self.state.read(cx).store())
            .unwrap_or_default()
            .iter()
            .map(|group| group.id)
            .max();
        if let Some(id) = newest {
            self.set_filter(Filter::Group(id), cx);
        }
        true
    }

    fn toggle_member(&mut self, group_id: Id, contact_id: Id, cx: &mut Context<Self>) {
        let group = self
            .state
            .read(cx)
            .store()
            .get::<ContactGroup>(group_id)
            .ok()
            .flatten();
        let Some(mut group) = group else {
            return;
        };
        if group.members.contains(&contact_id) {
            group.members.retain(|member| *member != contact_id);
        } else {
            group.members.push(contact_id);
        }
        self.run(PeopleCommand::SaveGroup(Box::new(group)), cx);
    }

    fn delete_group(&mut self, id: Id, cx: &mut Context<Self>) {
        if self.run(PeopleCommand::DeleteGroup(id), cx) && self.filter == Filter::Group(id) {
            self.set_filter(Filter::All, cx);
        }
    }

    fn jump_to(&mut self, letter: char, cx: &mut Context<Self>) {
        let rank = |c: char| {
            LETTERS
                .iter()
                .position(|l| *l == c)
                .unwrap_or(LETTERS.len())
        };
        let target = rank(letter);
        let index = self
            .rows
            .iter()
            .position(|row| row.letter.is_some_and(|l| rank(l) >= target));
        if let Some(index) = index {
            self.scroll.scroll_to_item(index, ScrollStrategy::Top);
            cx.notify();
        }
    }

    fn emit_compose(&mut self, recipients: Vec<Address>, cx: &mut Context<Self>) {
        if !recipients.is_empty() {
            cx.emit(PeopleEvent::Compose(recipients));
        }
    }

    fn emit_meeting(&mut self, attendees: Vec<Address>, cx: &mut Context<Self>) {
        if !attendees.is_empty() {
            cx.emit(PeopleEvent::ScheduleMeeting(attendees));
        }
    }

    // ---------- import / export ----------

    fn import(&mut self, cx: &mut Context<Self>) {
        let Some(account_id) = self.require_account(cx) else {
            return;
        };
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some(tr(cx, "Import contacts (.vcf)").into()),
        });
        cx.spawn(async move |this, cx| {
            let paths = match picked.await {
                Ok(Ok(Some(paths))) => paths,
                Ok(Err(err)) => {
                    let _ = this.update(cx, |this, cx| this.report_picker_error(err, cx));
                    return;
                }
                _ => return,
            };
            let mut cards = Vec::new();
            let mut failures = Vec::new();
            for path in paths {
                match std::fs::read(&path) {
                    Ok(bytes) => cards.extend(vcard::parse_vcards(
                        &String::from_utf8_lossy(&bytes),
                        account_id,
                    )),
                    Err(err) => failures.push(format!("{}: {err}", path.display())),
                }
            }
            let _ = this.update(cx, |this, cx| this.finish_import(cards, failures, cx));
        })
        .detach();
    }

    fn finish_import(
        &mut self,
        cards: Vec<ContactCard>,
        failures: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        for failure in failures {
            let message = tr(cx, "Import failed: {}").replace("{}", &failure);
            self.report(message, cx);
        }
        if cards.is_empty() {
            let message = tr(cx, "No contacts found in the selected files");
            self.report(message.to_owned(), cx);
            return;
        }
        let count = cards.len();
        let commands = cards
            .into_iter()
            .map(|card| Command::People(PeopleCommand::SaveContact(Box::new(card))))
            .collect();
        let message = format!(
            "{} · {}",
            count_label(count, "Imported 1 contact", "Imported {} contacts", cx),
            tr(cx, "Ctrl+Z to undo")
        );
        self.state.update(cx, |state, cx| {
            if state.run(Command::Batch(commands), cx) {
                state.notify_info(message, cx);
            }
        });
        self.set_filter(Filter::All, cx);
    }

    /// Exports `ids` (or every contact) to a `.vcf` file chosen by the user.
    fn export(&mut self, ids: Option<Vec<Id>>, cx: &mut Context<Self>) {
        let cards: Vec<ContactCard> = people::list_contact_cards(self.state.read(cx).store())
            .unwrap_or_default()
            .into_iter()
            .filter(|card| ids.as_ref().is_none_or(|ids| ids.contains(&card.id())))
            .collect();
        if cards.is_empty() {
            let message = tr(cx, "No contacts to export");
            self.report(message.to_owned(), cx);
            return;
        }
        let file_name = match cards.as_slice() {
            [single] => format!("{}.vcf", safe_file_name(&single.name())),
            _ => "contacts.vcf".to_owned(),
        };
        let text = vcard::write_vcards(&cards);
        let count = cards.len();
        let directory = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .unwrap_or_default();
        let picked = cx.prompt_for_new_path(&directory, Some(&file_name));
        cx.spawn(async move |this, cx| {
            let path = match picked.await {
                Ok(Ok(Some(path))) => path,
                Ok(Err(err)) => {
                    let _ = this.update(cx, |this, cx| this.report_picker_error(err, cx));
                    return;
                }
                _ => return,
            };
            let result = std::fs::write(&path, text);
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => {
                    let message =
                        count_label(count, "Exported 1 contact", "Exported {} contacts", cx);
                    this.state
                        .update(cx, |state, cx| state.notify_info(message, cx));
                }
                Err(err) => {
                    let message = tr(cx, "Export failed: {}").replace("{}", &err.to_string());
                    this.report(message, cx);
                }
            });
        })
        .detach();
    }

    fn report(&mut self, message: String, cx: &mut Context<Self>) {
        self.state
            .update(cx, |state, cx| state.report_error(message, cx));
    }

    fn report_picker_error(&mut self, err: impl std::fmt::Display, cx: &mut Context<Self>) {
        let message = tr(cx, "Could not open the file picker: {}").replace("{}", &err.to_string());
        self.report(message, cx);
    }

    // ---------- rendering ----------

    fn visible_cards(
        &self,
        cards: &[ContactCard],
        groups: &[ContactGroup],
        cx: &App,
    ) -> Vec<ContactCard> {
        let query = self.search.read(cx).value().to_string();
        let members: Option<&[Id]> = match &self.filter {
            Filter::Group(id) => Some(
                groups
                    .iter()
                    .find(|group| group.id == *id)
                    .map_or(&[][..], |group| group.members.as_slice()),
            ),
            _ => None,
        };
        cards
            .iter()
            .filter(|card| match &self.filter {
                Filter::All | Filter::Duplicates | Filter::Recent => true,
                Filter::Favorites => card.contact.favorite,
                Filter::Group(_) => members.is_some_and(|members| members.contains(&card.id())),
                Filter::Category(category) => card.details.categories.contains(category),
            })
            .filter(|card| card.matches(&query))
            .cloned()
            .collect()
    }

    fn render_sidebar(
        &self,
        cards: &[ContactCard],
        groups: &[ContactGroup],
        duplicates: usize,
        recent: usize,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let favorites = cards.iter().filter(|card| card.contact.favorite).count();
        let mut categories: Vec<String> = cards
            .iter()
            .flat_map(|card| card.details.categories.iter().cloned())
            .collect();
        categories.sort_by_key(|category| category.to_lowercase());
        categories.dedup();
        let category_colors = self
            .state
            .read(cx)
            .store()
            .list_categories()
            .unwrap_or_default();

        let filter_row = |id: ElementId,
                          icon: IconName,
                          label: SharedString,
                          count: usize,
                          filter: Filter,
                          cx: &mut Context<Self>| {
            let active = self.filter == filter;
            nav_row(id, icon, label, count, active, t)
                .on_click(cx.listener(move |this, _, _, cx| this.set_filter(filter.clone(), cx)))
        };

        let mut top = vec![
            filter_row(
                "people-all".into(),
                IconName::Users,
                tr(cx, "All contacts").into(),
                cards.len(),
                Filter::All,
                cx,
            ),
            filter_row(
                "people-favorites".into(),
                IconName::Star,
                tr(cx, "Favorites").into(),
                favorites,
                Filter::Favorites,
                cx,
            ),
            filter_row(
                "people-recent".into(),
                IconName::Clock,
                tr(cx, "Recent contacts").into(),
                recent,
                Filter::Recent,
                cx,
            ),
        ];
        if duplicates > 0 {
            top.push(filter_row(
                "people-duplicates".into(),
                IconName::GitMerge,
                tr(cx, "Duplicates").into(),
                duplicates,
                Filter::Duplicates,
                cx,
            ));
        }

        let delete_group_tooltip = tr(cx, "Delete group");
        let group_rows: Vec<AnyElement> = groups
            .iter()
            .map(|group| {
                let group_id = group.id;
                let active = self.filter == Filter::Group(group_id);
                let present = group
                    .members
                    .iter()
                    .filter(|member| cards.iter().any(|card| card.id() == **member))
                    .count();
                filter_row(
                    ("people-group", group_id as u64).into(),
                    IconName::UsersRound,
                    group.name.clone().into(),
                    present,
                    Filter::Group(group_id),
                    cx,
                )
                .group("group-row")
                .child(
                    div()
                        .when(!active, |el| {
                            el.opacity(0.)
                                .group_hover("group-row", |style| style.opacity(1.))
                        })
                        .child(
                            IconButton::new(("delete-group", group_id as u64), IconName::Trash)
                                .size(px(22.))
                                .icon_size(ICON_SIZE_SM)
                                .tooltip(delete_group_tooltip)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.delete_group(group_id, cx)
                                })),
                        ),
                )
                .into_any_element()
            })
            .collect();

        let category_rows: Vec<AnyElement> = categories
            .into_iter()
            .enumerate()
            .map(|(index, category)| {
                let color = category_colors
                    .iter()
                    .find(|known| known.name == category)
                    .map_or(t.text_subtle, |known| crate::theme::hex(known.color));
                let count = cards
                    .iter()
                    .filter(|card| card.details.categories.contains(&category))
                    .count();
                let active = self.filter == Filter::Category(category.clone());
                let filter = Filter::Category(category.clone());
                div()
                    .id(("people-category", index as u64))
                    .flex()
                    .items_center()
                    .gap(SPACE_2)
                    .h(NAV_ROW_HEIGHT)
                    .px(SPACE_2)
                    .rounded(RADIUS_MD)
                    .cursor_pointer()
                    .text_size(TEXT_MD)
                    .when(active, |el| el.bg(t.accent_subtle).text_color(t.accent))
                    .when(!active, |el| el.hover(|s| s.bg(t.hover)))
                    .child(div().size(px(10.)).rounded(RADIUS_SM).bg(color).mx(SPACE_1))
                    .child(div().flex_1().min_w(px(0.)).truncate().child(category))
                    .child(
                        div()
                            .text_size(TEXT_XS)
                            .text_color(t.text_subtle)
                            .child(count.to_string()),
                    )
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.set_filter(filter.clone(), cx)),
                    )
                    .into_any_element()
            })
            .collect();

        div()
            .id("people-sidebar")
            .flex()
            .flex_col()
            .size_full()
            .overflow_y_scroll()
            .p(SPACE_2)
            .gap(px(1.))
            .child(
                div().px(SPACE_1).pb(SPACE_2).child(
                    Button::new("new-contact")
                        .primary()
                        .icon(IconName::UserPlus)
                        .label(tr(cx, "New contact"))
                        .on_click(cx.listener(|this, _, window, cx| this.new_contact(window, cx))),
                ),
            )
            .children(top)
            .child(section_label(tr(cx, "Groups"), t))
            .children(group_rows)
            .child(
                div().px(SPACE_1).pt(SPACE_1).child(
                    Input::new(&self.new_group).prefix(
                        Icon::new(IconName::Plus)
                            .size(ICON_SIZE_SM)
                            .text_color(t.text_subtle),
                    ),
                ),
            )
            .when(!category_rows.is_empty(), |el| {
                el.child(section_label(tr(cx, "Categories"), t))
                    .children(category_rows)
            })
            .child(div().flex_1().min_h(SPACE_3))
            .child(
                div()
                    .flex()
                    .gap(SPACE_1)
                    .px(SPACE_1)
                    .pt(SPACE_2)
                    .border_t_1()
                    .border_color(t.border_subtle)
                    .child(
                        Button::new("import-vcf")
                            .ghost()
                            .compact()
                            .icon(IconName::Download)
                            .label(tr(cx, "Import"))
                            .on_click(cx.listener(|this, _, _, cx| this.import(cx))),
                    )
                    .child(
                        Button::new("export-vcf")
                            .ghost()
                            .compact()
                            .icon(IconName::Upload)
                            .label(tr(cx, "Export"))
                            .on_click(cx.listener(|this, _, _, cx| this.export(None, cx))),
                    ),
            )
            .child(self.render_carddav(t, cx))
    }

    /// CardDAV status + "Sync now", or "Connect CardDAV account" when none is set up.
    fn render_carddav(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let row = div()
            .flex()
            .items_center()
            .gap(SPACE_1)
            .px(SPACE_1)
            .pt(SPACE_1);
        match crate::dav_bridge::status(cx) {
            Some(status) => row
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .text_size(TEXT_XS)
                        .text_color(t.text_subtle)
                        .whitespace_normal()
                        .child(status),
                )
                .child(
                    IconButton::new("carddav-sync", IconName::RefreshCw)
                        .icon_size(ICON_SIZE_SM)
                        .tooltip(tr(cx, "Sync contacts now"))
                        .on_click(|_, _, cx| crate::dav_bridge::sync_now(cx)),
                ),
            None => row.child(
                Button::new("carddav-connect")
                    .ghost()
                    .compact()
                    .icon(IconName::RefreshCw)
                    .label(tr(cx, "Connect CardDAV"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        crate::dav_bridge::open_add_dav_account(this.state.clone(), window, cx)
                    })),
            ),
        }
    }

    /// First-run / empty list: what contacts are and how to get some.
    fn render_empty(&self, has_account: bool, recent: usize, cx: &mut Context<Self>) -> AnyElement {
        let carddav = Button::new("empty-carddav")
            .icon(IconName::RefreshCw)
            .label(tr(cx, "Connect CardDAV account"))
            .on_click(cx.listener(|this, _, window, cx| {
                crate::dav_bridge::open_add_dav_account(this.state.clone(), window, cx)
            }));
        if !has_account {
            return EmptyState::new(IconName::Contact, tr(cx, "No account yet"))
                .description(tr(
                    cx,
                    "Contacts are kept with an account. Add a mail account, or connect a \
                     CardDAV address book (iCloud, Fastmail, Nextcloud…).",
                ))
                .action(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(SPACE_2)
                        .child(
                            Button::new("empty-add-account")
                                .primary()
                                .icon(IconName::UserPlus)
                                .label(tr(cx, "Add mail account"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    crate::dialogs::account::open_add_account(
                                        this.state.clone(),
                                        window,
                                        cx,
                                    )
                                })),
                        )
                        .child(carddav),
                )
                .into_any_element();
        }
        let mut description = tr(
            cx,
            "Create one, import a .vcf file or sync an address book.",
        )
        .to_owned();
        if recent > 0 {
            description.push(' ');
            description.push_str(&count_label(
                recent,
                "1 person from your mail is waiting in Recent contacts.",
                "{} people from your mail are waiting in Recent contacts.",
                cx,
            ));
        }
        EmptyState::new(IconName::Users, tr(cx, "No contacts yet"))
            .description(description)
            .action(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(SPACE_2)
                    .child(
                        Button::new("empty-new")
                            .primary()
                            .icon(IconName::UserPlus)
                            .label(tr(cx, "New contact"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.new_contact(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("empty-import")
                            .icon(IconName::Download)
                            .label(tr(cx, "Import .vcf"))
                            .on_click(cx.listener(|this, _, _, cx| this.import(cx))),
                    )
                    .child(carddav)
                    .when(recent > 0, |el| {
                        el.child(
                            Button::new("empty-recent")
                                .ghost()
                                .icon(IconName::Clock)
                                .label(tr(cx, "See recent contacts"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.set_filter(Filter::Recent, cx)
                                })),
                        )
                    }),
            )
            .into_any_element()
    }

    /// "Recent contacts": correspondents not saved yet, each savable in one click.
    fn render_recent(
        &self,
        suggestions: &[Suggestion],
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let query = self.search.read(cx).value().trim().to_lowercase();
        let shown: Vec<Suggestion> = suggestions
            .iter()
            .filter(|suggestion| {
                query.is_empty()
                    || suggestion.address.email.to_lowercase().contains(&query)
                    || suggestion.address.name.to_lowercase().contains(&query)
            })
            .cloned()
            .collect();
        if shown.is_empty() {
            return EmptyState::new(IconName::Clock, tr(cx, "No recent contacts"))
                .description(tr(
                    cx,
                    "People you write to or hear from show up here until you save them.",
                ))
                .into_any_element();
        }
        let email_tooltip = tr(cx, "Email");
        let save_label = tr(cx, "Save");
        let last_template = tr(cx, "Last on {}");
        div()
            .id("recent-contacts")
            .size_full()
            .overflow_y_scroll()
            .py(SPACE_1)
            .children(shown.into_iter().enumerate().map(|(index, suggestion)| {
                let address = suggestion.address.clone();
                let to = vec![address.clone()];
                let name = address.display().to_owned();
                let meta = format!(
                    "{} · {}",
                    count_label(suggestion.messages, "1 email", "{} emails", cx),
                    last_template.replace(
                        "{}",
                        &suggestion
                            .last
                            .with_timezone(&chrono::Local)
                            .format_in(prefs(cx).language, "%-d %b %Y")
                    )
                );
                div()
                    .id(("recent", index as u64))
                    .tooltip({
                        let meta = meta.clone();
                        move |window, cx| Tooltip::new(meta.clone()).build(window, cx)
                    })
                    .flex()
                    .items_center()
                    .gap(SPACE_3)
                    .h(LIST_ROW_HEIGHT)
                    .mx(SPACE_2)
                    .px(SPACE_2)
                    .rounded(RADIUS_MD)
                    .hover(|s| s.bg(t.hover))
                    .child(Avatar::new(name.clone()).size(px(32.)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.))
                            .child(div().truncate().text_size(TEXT_MD).child(name))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(TEXT_SM)
                                    .text_color(t.text_muted)
                                    .child(if address.name.trim().is_empty() {
                                        meta
                                    } else {
                                        address.email.clone()
                                    }),
                            ),
                    )
                    .child(
                        IconButton::new(("recent-email", index as u64), IconName::Mail)
                            .icon_size(ICON_SIZE_SM)
                            .tooltip(email_tooltip)
                            .on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.emit_compose(to.clone(), cx)
                                }),
                            ),
                    )
                    .child(
                        Button::new(("recent-save", index as u64))
                            .compact()
                            .icon(IconName::UserPlus)
                            .label(save_label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.save_suggestion(address.clone(), cx)
                            })),
                    )
            }))
            .into_any_element()
    }

    /// Detail pane for a multi-selection: email, add to group, export, delete.
    fn render_bulk(
        &self,
        groups: &[ContactGroup],
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ids = self.checked.clone();
        let count = ids.len();
        let (email_ids, export_ids, delete_ids) = (ids.clone(), ids.clone(), ids.clone());
        let group_chips: Vec<AnyElement> = groups
            .iter()
            .map(|group| {
                let (group_id, ids) = (group.id, ids.clone());
                let all_in = ids.iter().all(|id| group.members.contains(id));
                card::chip(
                    ("bulk-group", group_id as u64),
                    group.name.clone(),
                    all_in,
                    t,
                )
                .on_click(
                    cx.listener(move |this, _, _, cx| this.add_to_group(group_id, ids.clone(), cx)),
                )
                .into_any_element()
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .size_full()
            .items_center()
            .justify_center()
            .gap(SPACE_3)
            .p(SPACE_3)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(64.))
                    .rounded(RADIUS_FULL)
                    .bg(t.accent_subtle)
                    .text_color(t.accent)
                    .text_size(TEXT_LG)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(count.to_string()),
            )
            .child(
                div()
                    .text_size(TEXT_LG)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr(cx, "{} contacts selected").replace("{}", &count.to_string())),
            )
            .child(
                div()
                    .text_size(TEXT_SM)
                    .text_color(t.text_subtle)
                    .text_center()
                    .child(format!(
                        "{} · {} · {}",
                        tr(cx, "Ctrl+click to add or remove"),
                        tr(cx, "Shift+click for a range"),
                        tr(cx, "Esc to clear")
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_center()
                    .gap(SPACE_2)
                    .child(
                        Button::new("bulk-email")
                            .primary()
                            .icon(IconName::Mail)
                            .label(tr(cx, "Email all"))
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.compose_to(&email_ids, cx)),
                            ),
                    )
                    .child(
                        Button::new("bulk-export")
                            .icon(IconName::Upload)
                            .label(tr(cx, "Export"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.export(Some(export_ids.clone()), cx)
                            })),
                    )
                    .child(
                        Button::new("bulk-delete")
                            .danger()
                            .icon(IconName::Trash)
                            .label(tr(cx, "Delete"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.delete_contacts(delete_ids.clone(), window, cx)
                            })),
                    ),
            )
            .when(!group_chips.is_empty(), |el| {
                el.child(
                    div()
                        .pt(SPACE_2)
                        .text_size(TEXT_XS)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.text_subtle)
                        .child(tr(cx, "Add to group")),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .justify_center()
                        .max_w(px(420.))
                        .gap(SPACE_1)
                        .children(group_chips),
                )
            })
            .into_any_element()
    }

    fn render_list_header(
        &self,
        title: SharedString,
        count: usize,
        group_emails: Vec<Address>,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .px(SPACE_3)
            .pb(SPACE_3)
            .border_b_1()
            .border_color(t.border_subtle)
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(PANE_HEADER_HEIGHT)
                    .gap(SPACE_2)
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_size(TEXT_LG)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(TEXT_SM)
                            .text_color(t.text_subtle)
                            .child(count.to_string()),
                    )
                    .when(!group_emails.is_empty(), |el| {
                        el.child(
                            IconButton::new("email-group", IconName::Mail)
                                .size(px(28.))
                                .tooltip(tr(cx, "Email everyone"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.emit_compose(group_emails.clone(), cx)
                                })),
                        )
                    }),
            )
            .child(
                Input::new(&self.search)
                    .prefix(
                        Icon::new(IconName::Search)
                            .size(ICON_SIZE_SM)
                            .text_color(t.text_subtle),
                    )
                    .cleanable(true),
            )
    }

    fn render_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx).clone();
        let Some(row) = self.rows.get(index) else {
            return div().h(ROW_HEIGHT).into_any_element();
        };
        let id = row.id;
        let checked = self.checked.contains(&id);
        let selected = checked || (self.checked.is_empty() && self.selected == Some(id));
        div()
            .id(("contact-row", id as u64))
            .w_full()
            .h(ROW_HEIGHT)
            .flex()
            .items_center()
            .pr(SPACE_2)
            .when(row.letter.is_some() && index > 0, |el| {
                el.border_t_1().border_color(t.border_subtle)
            })
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(28.))
                    .text_center()
                    .text_size(TEXT_SM)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.accent)
                    .when_some(row.letter, |el, letter| el.child(letter.to_string())),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w(px(0.))
                    .items_center()
                    .gap(SPACE_3)
                    .h(LIST_ROW_HEIGHT)
                    .px(SPACE_2)
                    .rounded(RADIUS_MD)
                    .cursor_pointer()
                    .when(selected, |el| el.bg(t.accent_subtle))
                    .when(!selected, |el| el.hover(|s| s.bg(t.hover)))
                    .child(if checked {
                        div()
                            .flex()
                            .flex_shrink_0()
                            .items_center()
                            .justify_center()
                            .size(px(32.))
                            .rounded(RADIUS_FULL)
                            .bg(t.accent)
                            .text_color(t.on_accent)
                            .child(Icon::new(IconName::Check).size(ICON_SIZE))
                            .into_any_element()
                    } else {
                        Avatar::new(row.name.clone())
                            .size(px(32.))
                            .into_any_element()
                    })
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(TEXT_MD)
                                    .font_weight(FontWeight::MEDIUM)
                                    .when(selected, |el| el.text_color(t.accent))
                                    .child(row.name.clone()),
                            )
                            .when(!row.subtitle.is_empty(), |el| {
                                el.child(
                                    div()
                                        .truncate()
                                        .text_size(TEXT_SM)
                                        .text_color(t.text_muted)
                                        .child(row.subtitle.clone()),
                                )
                            }),
                    )
                    .when(row.favorite, |el| {
                        el.child(
                            Icon::new(IconName::Star)
                                .size(ICON_SIZE_SM)
                                .text_color(t.warning),
                        )
                    }),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.click_row(id, event.modifiers(), window, cx)
            }))
            .into_any_element()
    }

    fn render_jump_bar(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let present: Vec<char> = self.rows.iter().filter_map(|row| row.letter).collect();
        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .justify_center()
            .w(px(18.))
            .h_full()
            .py(SPACE_2)
            .children(LETTERS.iter().enumerate().map(|(index, &letter)| {
                let available = present.contains(&letter);
                div()
                    .id(("jump", index as u64))
                    .flex()
                    .justify_center()
                    .text_size(TEXT_2XS)
                    .line_height(px(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(letter.to_string())
                    .when(available, |el| {
                        el.cursor_pointer()
                            .text_color(t.accent)
                            .hover(|s| s.text_color(t.text))
                            .on_click(cx.listener(move |this, _, _, cx| this.jump_to(letter, cx)))
                    })
                    .when(!available, |el| {
                        el.text_color(t.text_subtle).opacity(DISABLED_OPACITY)
                    })
            }))
    }

    fn render_duplicates(
        &self,
        cards: &[ContactCard],
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let groups = people::find_duplicates(cards);
        if groups.is_empty() {
            return EmptyState::new(IconName::GitMerge, tr(cx, "No duplicates"))
                .description(tr(cx, "Contacts sharing an email or a name show up here."))
                .into_any_element();
        }
        let merge_template = tr(cx, "Merge {}");
        let find = |id: Id| cards.iter().find(|card| card.id() == id);
        div()
            .id("duplicates")
            .size_full()
            .overflow_y_scroll()
            .p(SPACE_3)
            .flex()
            .flex_col()
            .gap(SPACE_3)
            .children(groups.into_iter().enumerate().map(|(index, ids)| {
                let keep = ids[0];
                let others: Vec<Id> = ids[1..].to_vec();
                let label = merge_template.replace("{}", &ids.len().to_string());
                div()
                    .flex()
                    .flex_col()
                    .gap(SPACE_2)
                    .p(SPACE_3)
                    .rounded(RADIUS_MD)
                    .border_1()
                    .border_color(t.border)
                    .bg(t.elevated)
                    .children(ids.iter().filter_map(|&id| find(id)).map(|card| {
                        let id = card.id();
                        div()
                            .id(("duplicate", id as u64))
                            .flex()
                            .items_center()
                            .gap(SPACE_2)
                            .cursor_pointer()
                            .child(Avatar::new(card.name()).size(px(26.)))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .child(div().truncate().text_size(TEXT_MD).child(card.name()))
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(TEXT_SM)
                                            .text_color(t.text_muted)
                                            .child(card.contact.emails.join(", ")),
                                    ),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| this.select(Some(id), cx)))
                    }))
                    .child(
                        div().flex().justify_end().child(
                            Button::new(("merge", index as u64))
                                .primary()
                                .compact()
                                .icon(IconName::GitMerge)
                                .label(label)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let others = others.clone();
                                    if this.run(PeopleCommand::Merge { keep, others }, cx) {
                                        this.select(Some(keep), cx);
                                    }
                                })),
                        ),
                    )
            }))
            .into_any_element()
    }
}

impl Render for PeopleView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let store = self.state.read(cx).store();
        let cards = people::list_contact_cards(store);
        let load_failed = cards.is_err();
        let cards = cards.unwrap_or_default();
        let groups = people::list_contact_groups(store).unwrap_or_default();
        let duplicates = people::find_duplicates(&cards).len();
        let has_account = store
            .list_accounts()
            .is_ok_and(|accounts| !accounts.is_empty());
        let suggestions = self.suggestions(&cards, cx);
        let stale_filter = match &self.filter {
            Filter::Duplicates => duplicates == 0,
            Filter::Group(id) => !groups.iter().any(|group| group.id == *id),
            _ => false,
        };
        if stale_filter {
            self.filter = Filter::All;
        }

        let visible = self.visible_cards(&cards, &groups, cx);
        let mut previous_letter = None;
        self.rows = visible
            .iter()
            .map(|card| {
                let letter = card.section_letter();
                let first = previous_letter != Some(letter);
                previous_letter = Some(letter);
                let subtitle = [card.contact.job_title.trim(), card.contact.company.trim()]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join(" · ");
                let subtitle = if subtitle.is_empty() {
                    card.primary_email().unwrap_or_default().to_owned()
                } else {
                    subtitle
                };
                Row {
                    id: card.id(),
                    name: card.name().into(),
                    subtitle: subtitle.into(),
                    favorite: card.contact.favorite,
                    letter: first.then_some(letter),
                }
            })
            .collect();

        let (title, group_emails): (SharedString, Vec<Address>) = match &self.filter {
            Filter::All => (tr(cx, "All contacts").into(), Vec::new()),
            Filter::Favorites => (tr(cx, "Favorites").into(), Vec::new()),
            Filter::Recent => (tr(cx, "Recent contacts").into(), Vec::new()),
            Filter::Duplicates => (tr(cx, "Duplicates").into(), Vec::new()),
            Filter::Category(category) => (category.clone().into(), Vec::new()),
            Filter::Group(id) => (
                groups
                    .iter()
                    .find(|group| group.id == *id)
                    .map_or_else(|| tr(cx, "Group").into(), |group| group.name.clone().into()),
                visible
                    .iter()
                    .filter_map(|card| {
                        card.primary_email()
                            .map(|email| Address::new(card.name(), email))
                    })
                    .collect(),
            ),
        };

        // Drop selections of contacts that no longer exist (deleted, merged, synced away).
        self.checked
            .retain(|id| cards.iter().any(|card| card.id() == *id));
        let list: AnyElement = if self.filter == Filter::Duplicates {
            self.render_duplicates(&cards, &t, cx)
        } else if self.filter == Filter::Recent {
            self.render_recent(&suggestions, &t, cx)
        } else if load_failed {
            EmptyState::new(IconName::TriangleAlert, tr(cx, "Couldn't load contacts"))
                .description(tr(cx, "Try again in a moment."))
                .into_any_element()
        } else if cards.is_empty() {
            self.render_empty(has_account, suggestions.len(), cx)
        } else if self.rows.is_empty() {
            EmptyState::new(IconName::Users, tr(cx, "Nobody here"))
                .description(tr(cx, "Try another search or filter."))
                .into_any_element()
        } else {
            div()
                .flex()
                .size_full()
                .child(
                    uniform_list(
                        "contact-list",
                        self.rows.len(),
                        cx.processor(|this, range: Range<usize>, _window, cx| {
                            range
                                .map(|index| this.render_row(index, cx))
                                .collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&self.scroll)
                    .flex_1()
                    .h_full()
                    .py(SPACE_1),
                )
                .child(self.render_jump_bar(&t, cx))
                .into_any_element()
        };

        let detail: AnyElement = if self.form.is_some() {
            self.render_form(&t, window, cx).into_any_element()
        } else if self.checked.len() > 1 {
            self.render_bulk(&groups, &t, cx)
        } else if self.filter == Filter::Recent {
            EmptyState::new(IconName::UserPlus, tr(cx, "Save people you mail with"))
                .description(tr(
                    cx,
                    "Everyone you write to or hear from appears here until saved. Save adds them to your contacts and to compose suggestions.",
                ))
                .into_any_element()
        } else if let Some(card) = self
            .selected
            .and_then(|id| cards.iter().find(|card| card.id() == id))
        {
            let card = card.clone();
            self.render_card(&card, &groups, &t, cx).into_any_element()
        } else {
            EmptyState::new(IconName::Contact, tr(cx, "Select a contact"))
                .description(format!(
                    "{} · {}",
                    count_label(cards.len(), "1 contact", "{} contacts", cx),
                    count_label(groups.len(), "1 group", "{} groups", cx)
                ))
                .into_any_element()
        };

        let count = match self.filter {
            Filter::Recent => suggestions.len(),
            Filter::Duplicates => duplicates,
            _ => self.rows.len(),
        };
        div()
            .id("people")
            .track_focus(&self.root_focus)
            .on_action(
                cx.listener(|this, _: &crate::actions::NewContact, window, cx| {
                    this.new_contact(window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::actions::DeleteSelection, window, cx| {
                    this.delete_selection(window, cx)
                }),
            )
            .flex()
            .flex_row()
            .size_full()
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(232.))
                    .h_full()
                    .bg(t.surface)
                    .border_r_1()
                    .border_color(t.border_subtle)
                    .child(self.render_sidebar(
                        &cards,
                        &groups,
                        duplicates,
                        suggestions.len(),
                        &t,
                        cx,
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_shrink_0()
                    .w(px(340.))
                    .h_full()
                    .border_r_1()
                    .border_color(t.border_subtle)
                    .child(self.render_list_header(title, count, group_emails, &t, cx))
                    .child(
                        div()
                            .id("contact-list-pane")
                            .track_focus(&self.list_focus)
                            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                                this.on_list_key(event, window, cx)
                            }))
                            .flex_1()
                            .min_h(px(0.))
                            .child(list),
                    ),
            )
            .child(div().flex_1().min_w(px(0.)).h_full().child(detail))
    }
}

/// Sidebar navigation row.
fn nav_row(
    id: ElementId,
    icon: IconName,
    label: SharedString,
    count: usize,
    active: bool,
    t: &Theme,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(SPACE_2)
        .h(NAV_ROW_HEIGHT)
        .px(SPACE_2)
        .rounded(RADIUS_MD)
        .cursor_pointer()
        .text_size(TEXT_MD)
        .when(active, |el| {
            el.bg(t.accent_subtle)
                .text_color(t.accent)
                .font_weight(FontWeight::MEDIUM)
        })
        .when(!active, |el| el.text_color(t.text).hover(|s| s.bg(t.hover)))
        .child(Icon::new(icon).size(ICON_SIZE).text_color(if active {
            t.accent
        } else {
            t.text_muted
        }))
        .child(div().flex_1().min_w(px(0.)).truncate().child(label))
        .when(count > 0, |el| {
            el.child(
                div()
                    .text_size(TEXT_XS)
                    .text_color(t.text_subtle)
                    .child(count.to_string()),
            )
        })
}

/// Small upper-case section title.
fn section_label(label: &'static str, t: &Theme) -> impl IntoElement {
    div()
        .px(SPACE_2)
        .pt(SPACE_3)
        .pb(SPACE_0_5)
        .text_size(TEXT_XS)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(t.text_subtle)
        .child(label.to_uppercase())
}

/// `one` when `count` is 1, else `many` with `{}` replaced by the count (both translated).
fn count_label(count: usize, one: &'static str, many: &'static str, cx: &App) -> String {
    if count == 1 {
        tr(cx, one).to_owned()
    } else {
        tr(cx, many).replace("{}", &count.to_string())
    }
}

/// Keeps letters, digits, spaces, `-` and `_` (for a suggested file name).
fn safe_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        "contact".to_owned()
    } else {
        cleaned.to_owned()
    }
}

/// Decodes a `data:image/...` photo for `img()`.
fn decode_photo(uri: &str) -> Option<Arc<Image>> {
    let (mime, bytes) = vcard::parse_data_uri(uri)?;
    let format = match mime.as_str() {
        "image/png" => ImageFormat::Png,
        "image/jpeg" | "image/jpg" => ImageFormat::Jpeg,
        "image/gif" => ImageFormat::Gif,
        "image/webp" => ImageFormat::Webp,
        "image/bmp" => ImageFormat::Bmp,
        _ => return None,
    };
    Some(Arc::new(Image::from_bytes(format, bytes)))
}
