//! People module: sidebar (all, favourites, duplicates, groups, categories, import/export)
//! | virtualized A–Z contact list with search | contact card or editor.
//!
//! Writes go through `Command::People` (undoable). "Email" and "Schedule meeting" do not
//! compose or create events here: they emit [`PeopleEvent`]s that the compose and calendar
//! modules handle by subscribing to the `PeopleView` entity.

mod card;
mod form;

use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::component::input::{Input, InputEvent, InputState};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::people::{self, ContactCard, ContactGroup};
use mailbox_core::{Address, Command, Id, PeopleCommand, vcard};

use crate::app_state::AppState;
use crate::components::{Avatar, Button, EmptyState, Icon, IconButton, IconName};
use crate::theme::{
    ICON_SIZE, ICON_SIZE_SM, RADIUS_MD, SPACE_0_5, SPACE_1, SPACE_2, SPACE_3, TEXT_LG, TEXT_MD,
    TEXT_SM, TEXT_XS, Theme, theme,
};

use form::ContactForm;

/// Height of a contact row (the list is a uniform, virtualized list).
const ROW_HEIGHT: Pixels = px(52.);
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
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PeopleEvent> for PeopleView {}

impl PeopleView {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search people"));
        let new_group = cx.new(|cx| InputState::new(window, cx).placeholder("New group"));
        let subscriptions = vec![
            cx.observe(&state, |_, _, cx| cx.notify()),
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
            _subscriptions: subscriptions,
        }
    }

    // ---------- actions ----------

    fn run(&mut self, command: PeopleCommand, cx: &mut Context<Self>) -> bool {
        self.state
            .update(cx, |state, cx| state.run(Command::People(command), cx))
    }

    fn select(&mut self, id: Option<Id>, cx: &mut Context<Self>) {
        self.selected = id;
        self.form = None;
        cx.notify();
    }

    fn set_filter(&mut self, filter: Filter, cx: &mut Context<Self>) {
        self.filter = filter;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    /// Account new contacts are created in (the first one).
    fn account_id(&self, cx: &App) -> Option<Id> {
        let accounts = self.state.read(cx).store().list_accounts().ok()?;
        accounts.first().map(|account| account.id)
    }

    fn new_contact(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(account_id) = self.account_id(cx) else {
            self.state.update(cx, |state, cx| {
                state.report_error("Add an account first", cx)
            });
            return;
        };
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
                self.form = Some(ContactForm::new(card, window, cx));
                cx.notify();
            }
            Ok(None) => {}
            Err(err) => self
                .state
                .update(cx, |state, cx| state.report_error(err, cx)),
        }
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
        cx.notify();
    }

    fn newest_contact_id(&self, cx: &App) -> Option<Id> {
        let contacts = self.state.read(cx).store().list_contacts().ok()?;
        contacts.iter().map(|contact| contact.id).max()
    }

    fn delete_contact(&mut self, id: Id, cx: &mut Context<Self>) {
        if self.run(PeopleCommand::DeleteContacts(vec![id]), cx) {
            self.select(None, cx);
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
        let Some(account_id) = self.account_id(cx) else {
            self.state.update(cx, |state, cx| {
                state.report_error("Add an account first", cx)
            });
            return;
        };
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Import contacts".into()),
        });
        cx.spawn(async move |this, cx| {
            let paths = match picked.await {
                Ok(Ok(Some(paths))) => paths,
                Ok(Err(err)) => {
                    let message = format!("Could not open the file picker: {err}");
                    let _ = this.update(cx, |this, cx| this.report(message, cx));
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
            self.report(format!("Import failed: {failure}"), cx);
        }
        if cards.is_empty() {
            self.report("No contacts found in the selected files".to_owned(), cx);
            return;
        }
        let count = cards.len();
        let commands = cards
            .into_iter()
            .map(|card| Command::People(PeopleCommand::SaveContact(Box::new(card))))
            .collect();
        self.state.update(cx, |state, cx| {
            if state.run(Command::Batch(commands), cx) {
                state.notify_info(format!("Imported {count} contacts"), cx);
            }
        });
    }

    /// Exports `ids` (or every contact) to a `.vcf` file chosen by the user.
    fn export(&mut self, ids: Option<Vec<Id>>, cx: &mut Context<Self>) {
        let cards: Vec<ContactCard> = people::list_contact_cards(self.state.read(cx).store())
            .unwrap_or_default()
            .into_iter()
            .filter(|card| ids.as_ref().is_none_or(|ids| ids.contains(&card.id())))
            .collect();
        if cards.is_empty() {
            self.report("No contacts to export".to_owned(), cx);
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
            let Ok(Ok(Some(path))) = picked.await else {
                return;
            };
            let result = std::fs::write(&path, text);
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => this.state.update(cx, |state, cx| {
                    let noun = if count == 1 { "contact" } else { "contacts" };
                    state.notify_info(format!("Exported {count} {noun}"), cx);
                }),
                Err(err) => this.report(format!("Export failed: {err}"), cx),
            });
        })
        .detach();
    }

    fn report(&mut self, message: String, cx: &mut Context<Self>) {
        self.state
            .update(cx, |state, cx| state.report_error(message, cx));
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
                Filter::All | Filter::Duplicates => true,
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
                "All contacts".into(),
                cards.len(),
                Filter::All,
                cx,
            ),
            filter_row(
                "people-favorites".into(),
                IconName::Star,
                "Favorites".into(),
                favorites,
                Filter::Favorites,
                cx,
            ),
        ];
        if duplicates > 0 {
            top.push(filter_row(
                "people-duplicates".into(),
                IconName::GitMerge,
                "Duplicates".into(),
                duplicates,
                Filter::Duplicates,
                cx,
            ));
        }

        let group_rows: Vec<AnyElement> = groups
            .iter()
            .map(|group| {
                let group_id = group.id;
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
                        .opacity(0.)
                        .group_hover("group-row", |style| style.opacity(1.))
                        .child(
                            IconButton::new(("delete-group", group_id as u64), IconName::Trash)
                                .size(px(22.))
                                .icon_size(ICON_SIZE_SM)
                                .tooltip("Delete group")
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
                    .h(px(30.))
                    .px(SPACE_2)
                    .rounded(RADIUS_MD)
                    .cursor_pointer()
                    .text_size(TEXT_MD)
                    .when(active, |el| el.bg(t.accent_subtle).text_color(t.accent))
                    .when(!active, |el| el.hover(|s| s.bg(t.hover)))
                    .child(div().size(px(10.)).rounded(px(3.)).bg(color).mx(px(3.)))
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
                        .label("New contact")
                        .on_click(cx.listener(|this, _, window, cx| this.new_contact(window, cx))),
                ),
            )
            .children(top)
            .child(section_label("Groups", t))
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
                el.child(section_label("Categories", t))
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
                            .label("Import")
                            .on_click(cx.listener(|this, _, _, cx| this.import(cx))),
                    )
                    .child(
                        Button::new("export-vcf")
                            .ghost()
                            .compact()
                            .icon(IconName::Upload)
                            .label("Export")
                            .on_click(cx.listener(|this, _, _, cx| this.export(None, cx))),
                    ),
            )
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
            .gap(SPACE_2)
            .p(SPACE_3)
            .border_b_1()
            .border_color(t.border_subtle)
            .child(
                div()
                    .flex()
                    .items_center()
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
                                .tooltip("Email everyone")
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
        let selected = self.selected == Some(id);
        div()
            .id(("contact-row", id as u64))
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
                    .h(px(44.))
                    .px(SPACE_2)
                    .rounded(RADIUS_MD)
                    .cursor_pointer()
                    .when(selected, |el| el.bg(t.accent_subtle))
                    .when(!selected, |el| el.hover(|s| s.bg(t.hover)))
                    .child(Avatar::new(row.name.clone()).size(px(32.)))
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
            .on_click(cx.listener(move |this, _, _, cx| this.select(Some(id), cx)))
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
                    .text_size(px(9.5))
                    .line_height(px(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .cursor_pointer()
                    .text_color(if available {
                        t.accent
                    } else {
                        t.text_subtle.alpha(0.5)
                    })
                    .hover(|s| s.text_color(t.text))
                    .child(letter.to_string())
                    .on_click(cx.listener(move |this, _, _, cx| this.jump_to(letter, cx)))
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
            return EmptyState::new(IconName::GitMerge, "No duplicates")
                .description("Contacts sharing an email or a name show up here.")
                .into_any_element();
        }
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
                let label = format!("Merge {}", ids.len());
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
        let cards = people::list_contact_cards(store).unwrap_or_default();
        let groups = people::list_contact_groups(store).unwrap_or_default();
        let duplicates = people::find_duplicates(&cards).len();
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
            Filter::All => ("All contacts".into(), Vec::new()),
            Filter::Favorites => ("Favorites".into(), Vec::new()),
            Filter::Duplicates => ("Duplicates".into(), Vec::new()),
            Filter::Category(category) => (category.clone().into(), Vec::new()),
            Filter::Group(id) => (
                groups
                    .iter()
                    .find(|group| group.id == *id)
                    .map_or_else(|| "Group".into(), |group| group.name.clone().into()),
                visible
                    .iter()
                    .filter_map(|card| {
                        card.primary_email()
                            .map(|email| Address::new(card.name(), email))
                    })
                    .collect(),
            ),
        };

        let list: AnyElement = if self.filter == Filter::Duplicates {
            self.render_duplicates(&cards, &t, cx)
        } else if self.rows.is_empty() {
            let (title, description) = if cards.is_empty() {
                ("No contacts yet", "Create a contact or import a .vcf file.")
            } else {
                ("Nobody here", "Try another search or filter.")
            };
            EmptyState::new(IconName::Users, title)
                .description(description)
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
        } else if let Some(card) = self
            .selected
            .and_then(|id| cards.iter().find(|card| card.id() == id))
        {
            let card = card.clone();
            self.render_card(&card, &groups, &t, cx).into_any_element()
        } else {
            EmptyState::new(IconName::Contact, "Select a contact")
                .description(format!(
                    "{} contacts · {} groups",
                    cards.len(),
                    groups.len()
                ))
                .into_any_element()
        };

        let count = self.rows.len();
        div()
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
                    .child(self.render_sidebar(&cards, &groups, duplicates, &t, cx)),
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
                    .child(div().flex_1().min_h(px(0.)).child(list)),
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
        .h(px(30.))
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

/// Ribbon / command palette entry points (routed by `crate::actions`).
impl PeopleView {
    pub(crate) fn action_new_contact(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.new_contact(window, cx);
    }

    /// Deletes the selected contact; false when nothing is selected.
    pub(crate) fn action_delete(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(id) = self.selected else {
            return false;
        };
        self.delete_contact(id, cx);
        true
    }
}
