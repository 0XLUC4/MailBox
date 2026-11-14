//! People: contact details beyond the base [`Contact`] row (names, addresses, birthday,
//! photo, categories), contact groups, duplicate detection/merge and the [`PeopleCommand`]s
//! that change them. Run them as `Command::People(..)` so they get undo/redo.
//!
//! Details live in `contact_details`, one row per contact with the same id (deleted with
//! the contact). A contact without a details row reads as empty details.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, Duration, NaiveDate, Utc};
use rusqlite::types::Value;
use rusqlite::{Row, params};
use serde::{Deserialize, Serialize};

use crate::command::Command;
use crate::error::{Error, Result};
use crate::model::{Address, CalendarEvent, Contact, Id, Message};
use crate::store::{Record, Store};

/// Schema for this module, appended to `store::MIGRATIONS`.
pub(crate) const MIGRATION: &str = r#"
CREATE TABLE contact_details (
    id INTEGER PRIMARY KEY REFERENCES contacts(id) ON DELETE CASCADE,
    first_name TEXT NOT NULL,
    last_name TEXT NOT NULL,
    nickname TEXT NOT NULL,
    birthday TEXT,
    website TEXT NOT NULL,
    photo TEXT,
    addresses_json TEXT NOT NULL,
    categories_json TEXT NOT NULL
);
CREATE TABLE contact_groups (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    members_json TEXT NOT NULL
);
"#;

/// Labels for emails and phones ("work", "home", "mobile"...), appended to `store::MIGRATIONS`.
pub(crate) const MIGRATION_LABELS: &str = r#"
ALTER TABLE contact_details ADD COLUMN email_labels_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE contact_details ADD COLUMN phone_labels_json TEXT NOT NULL DEFAULT '[]';
"#;

/// A postal address. `label` is free text ("home", "work"...).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PostalAddress {
    pub label: String,
    pub street: String,
    pub city: String,
    pub region: String,
    pub postal_code: String,
    pub country: String,
}

impl PostalAddress {
    pub fn is_empty(&self) -> bool {
        [
            &self.street,
            &self.city,
            &self.region,
            &self.postal_code,
            &self.country,
        ]
        .iter()
        .all(|part| part.trim().is_empty())
    }

    /// "12 Main St, 75001 Paris, France".
    pub fn one_line(&self) -> String {
        let city = [self.postal_code.trim(), self.city.trim()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        [
            self.street.trim(),
            city.as_str(),
            self.region.trim(),
            self.country.trim(),
        ]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
    }
}

/// Extra contact fields, keyed by the contact id.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ContactDetails {
    /// Same id as the contact.
    pub id: Id,
    pub first_name: String,
    pub last_name: String,
    pub nickname: String,
    pub birthday: Option<NaiveDate>,
    pub website: String,
    /// Photo as a `data:image/...;base64,...` URI.
    pub photo: Option<String>,
    pub addresses: Vec<PostalAddress>,
    /// Category names (see [`crate::Category`]).
    pub categories: Vec<String>,
    /// Label of `contact.emails[i]` ("work", "home"...); missing or empty = no label.
    #[serde(default)]
    pub email_labels: Vec<String>,
    /// Label of `contact.phones[i]` ("mobile", "work"...); missing or empty = no label.
    #[serde(default)]
    pub phone_labels: Vec<String>,
}

impl ContactDetails {
    /// Label of the email at `index` (empty when none).
    pub fn email_label(&self, index: usize) -> &str {
        self.email_labels
            .get(index)
            .map_or("", |label| label.trim())
    }

    /// Label of the phone at `index` (empty when none).
    pub fn phone_label(&self, index: usize) -> &str {
        self.phone_labels
            .get(index)
            .map_or("", |label| label.trim())
    }
}

/// Drops trailing empty labels so "no labels" is always an empty list.
pub fn trim_labels(labels: &mut Vec<String>) {
    while labels.last().is_some_and(|label| label.trim().is_empty()) {
        labels.pop();
    }
}

/// A contact with its details: what the People module reads and writes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContactCard {
    pub contact: Contact,
    pub details: ContactDetails,
}

impl ContactCard {
    /// An empty, unsaved contact.
    pub fn new(account_id: Id) -> Self {
        Self {
            contact: Contact {
                id: 0,
                account_id,
                display_name: String::new(),
                emails: Vec::new(),
                phones: Vec::new(),
                company: String::new(),
                job_title: String::new(),
                notes: String::new(),
                favorite: false,
            },
            details: ContactDetails::default(),
        }
    }

    pub fn id(&self) -> Id {
        self.contact.id
    }

    pub fn primary_email(&self) -> Option<&str> {
        self.contact
            .emails
            .iter()
            .map(|email| email.trim())
            .find(|email| !email.is_empty())
    }

    /// Display name, else "First Last", else the first email.
    pub fn name(&self) -> String {
        let display = self.contact.display_name.trim();
        if !display.is_empty() {
            return display.to_owned();
        }
        let full = format!(
            "{} {}",
            self.details.first_name.trim(),
            self.details.last_name.trim()
        );
        let full = full.trim();
        if !full.is_empty() {
            return full.to_owned();
        }
        self.primary_email().unwrap_or_default().to_owned()
    }

    /// Section letter for the alphabetical list: `A`..`Z`, or `#`.
    pub fn section_letter(&self) -> char {
        self.name()
            .chars()
            .find(|c| c.is_alphanumeric())
            .map(fold_letter)
            .unwrap_or('#')
    }

    /// Case-insensitive match on names, emails, phones, company and job title.
    pub fn matches(&self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return true;
        }
        let digits: String = query.chars().filter(char::is_ascii_digit).collect();
        let contact = &self.contact;
        let details = &self.details;
        [
            &contact.display_name,
            &contact.company,
            &contact.job_title,
            &details.first_name,
            &details.last_name,
            &details.nickname,
        ]
        .into_iter()
        .chain(&contact.emails)
        .any(|field| field.to_lowercase().contains(&query))
            || (digits.len() >= 3
                && contact
                    .phones
                    .iter()
                    .any(|phone| phone_digits(phone).contains(&digits)))
    }
}

/// `É` -> `E`, `ß` -> `S`, digits and others -> `#`.
fn fold_letter(c: char) -> char {
    let upper = c.to_uppercase().next().unwrap_or(c);
    let folded = match upper {
        'À'..='Å' => 'A',
        'Ç' | 'Ć' | 'Č' => 'C',
        'È'..='Ë' => 'E',
        'Ì'..='Ï' => 'I',
        'Ñ' | 'Ń' => 'N',
        'Ò'..='Ö' | 'Ø' => 'O',
        'Ù'..='Ü' => 'U',
        'Ý' | 'Ÿ' => 'Y',
        'Š' | 'Ś' | 'ß' => 'S',
        'Ž' | 'Ź' | 'Ż' => 'Z',
        other => other,
    };
    if folded.is_ascii_uppercase() {
        folded
    } else {
        '#'
    }
}

fn phone_digits(phone: &str) -> String {
    phone.chars().filter(char::is_ascii_digit).collect()
}

/// A named list of contacts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContactGroup {
    pub id: Id,
    pub name: String,
    /// Contact ids, in insertion order.
    pub members: Vec<Id>,
}

// ---------- queries ----------

/// Every contact with its details, sorted by name (case-insensitive).
pub fn list_contact_cards(store: &Store) -> Result<Vec<ContactCard>> {
    let mut details: HashMap<Id, ContactDetails> = store
        .query::<ContactDetails>("ORDER BY id", [])?
        .into_iter()
        .map(|details| (details.id, details))
        .collect();
    let mut cards: Vec<ContactCard> = store
        .list_contacts()?
        .into_iter()
        .map(|contact| {
            let details = details.remove(&contact.id).unwrap_or(ContactDetails {
                id: contact.id,
                ..Default::default()
            });
            ContactCard { contact, details }
        })
        .collect();
    // A..Z sections, then `#`.
    cards.sort_by_cached_key(|card| {
        let letter = card.section_letter();
        (letter == '#', letter, card.name().to_lowercase())
    });
    Ok(cards)
}

pub fn contact_card(store: &Store, id: Id) -> Result<Option<ContactCard>> {
    let Some(contact) = store.get::<Contact>(id)? else {
        return Ok(None);
    };
    let details = store.get::<ContactDetails>(id)?.unwrap_or(ContactDetails {
        id,
        ..Default::default()
    });
    Ok(Some(ContactCard { contact, details }))
}

fn require_card(store: &Store, id: Id) -> Result<ContactCard> {
    contact_card(store, id)?.ok_or(Error::NotFound {
        kind: "contact",
        id,
    })
}

pub fn list_contact_groups(store: &Store) -> Result<Vec<ContactGroup>> {
    store.query("ORDER BY name COLLATE NOCASE, id", [])
}

/// Latest messages from or to any of `emails`, newest first.
pub fn recent_messages_with(
    store: &Store,
    emails: &[String],
    limit: usize,
) -> Result<Vec<Message>> {
    let mut found: Vec<Message> = Vec::new();
    for email in emails.iter().map(|email| email.trim()) {
        if email.is_empty() {
            continue;
        }
        // Addresses are JSON (`{"name":..,"email":"x@y"}`); LIKE is case-insensitive for ASCII.
        let quoted = format!("%\"{email}\"%");
        found.extend(store.query::<Message>(
            "WHERE from_email LIKE ?1 OR to_json LIKE ?2 OR cc_json LIKE ?2 \
             ORDER BY date DESC, id DESC LIMIT ?3",
            params![email, quoted, limit as i64],
        )?);
    }
    found.sort_by(|a, b| b.date.cmp(&a.date).then(b.id.cmp(&a.id)));
    found.dedup_by_key(|message| message.id);
    found.truncate(limit);
    Ok(found)
}

/// Events in the next `days` days with any of `emails` as attendee, soonest first.
/// Recurring events are included with their first occurrence's start.
pub fn upcoming_events_with(
    store: &Store,
    emails: &[String],
    from: DateTime<Utc>,
    days: i64,
) -> Result<Vec<CalendarEvent>> {
    let emails: Vec<String> = emails
        .iter()
        .map(|email| email.trim().to_lowercase())
        .filter(|email| !email.is_empty())
        .collect();
    if emails.is_empty() {
        return Ok(Vec::new());
    }
    let mut events: Vec<CalendarEvent> = store
        .list_events_between(from, from + Duration::days(days))?
        .into_iter()
        .filter(|event| {
            event
                .attendees
                .iter()
                .any(|attendee| emails.contains(&attendee.address.email.trim().to_lowercase()))
        })
        .collect();
    events.sort_by_key(|event| event.start);
    Ok(events)
}

/// Someone you exchanged mail with who is not a contact yet ("Recent contacts").
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub address: Address,
    /// Messages (among the latest scanned) they appear in.
    pub messages: usize,
    pub last: DateTime<Utc>,
}

/// How many recent messages [`suggested_contacts`] scans.
const SUGGESTION_SCAN: i64 = 1000;

/// People from the latest messages (sender, To, Cc) who are not contacts, not one of your
/// accounts and not automated senders; most recent first.
pub fn suggested_contacts(
    store: &Store,
    cards: &[ContactCard],
    limit: usize,
) -> Result<Vec<Suggestion>> {
    let mut known: HashSet<String> = cards
        .iter()
        .flat_map(|card| &card.contact.emails)
        .map(|email| email.trim().to_lowercase())
        .collect();
    known.extend(
        store
            .list_accounts()?
            .into_iter()
            .map(|account| account.email.trim().to_lowercase()),
    );
    let messages: Vec<Message> =
        store.query("ORDER BY date DESC, id DESC LIMIT ?1", [SUGGESTION_SCAN])?;
    let mut found: Vec<Suggestion> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for message in messages {
        let addresses = std::iter::once(&message.from)
            .chain(&message.to)
            .chain(&message.cc);
        for address in addresses {
            let email = address.email.trim().to_lowercase();
            if !email.contains('@') || known.contains(&email) || is_automated(&email) {
                continue;
            }
            match index.get(&email) {
                Some(&at) => {
                    let suggestion = &mut found[at];
                    suggestion.messages += 1;
                    if suggestion.address.name.trim().is_empty() {
                        suggestion.address.name = address.name.trim().to_owned();
                    }
                }
                None => {
                    index.insert(email.clone(), found.len());
                    found.push(Suggestion {
                        address: Address::new(address.name.trim(), address.email.trim()),
                        messages: 1,
                        last: message.date,
                    });
                }
            }
        }
    }
    found.truncate(limit);
    Ok(found)
}

/// `noreply@`, `mailer-daemon@`, `notifications@`...: not worth suggesting as a contact.
fn is_automated(email: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "noreply",
        "no-reply",
        "no_reply",
        "donotreply",
        "do-not-reply",
        "mailer-daemon",
        "postmaster",
        "notification",
        "bounce",
    ];
    let local = email.split('@').next().unwrap_or_default();
    PREFIXES.iter().any(|prefix| local.starts_with(prefix))
}

/// A new contact from a suggestion (name split into first/last when it has two words or more).
pub fn card_from_address(address: &Address, account_id: Id) -> ContactCard {
    let mut card = ContactCard::new(account_id);
    let name = address.name.trim();
    card.contact.display_name = name.to_owned();
    card.contact.emails = vec![address.email.trim().to_owned()];
    if let Some((first, last)) = name.rsplit_once(' ')
        && !name.contains('@')
    {
        card.details.first_name = first.trim().to_owned();
        card.details.last_name = last.trim().to_owned();
    }
    card
}

/// Groups of probable duplicates (same email, or same name ignoring case and spacing).
/// Each group has 2+ ids, oldest first (the natural one to keep).
pub fn find_duplicates(cards: &[ContactCard]) -> Vec<Vec<Id>> {
    fn root(parent: &mut [usize], mut index: usize) -> usize {
        while parent[index] != index {
            parent[index] = parent[parent[index]];
            index = parent[index];
        }
        index
    }

    let mut parent: Vec<usize> = (0..cards.len()).collect();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for (index, card) in cards.iter().enumerate() {
        let name = card
            .name()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        let keys = card
            .contact
            .emails
            .iter()
            .map(|email| email.trim().to_lowercase())
            .filter(|email| !email.is_empty())
            .map(|email| format!("e:{email}"))
            .chain((!name.is_empty() && !name.contains('@')).then(|| format!("n:{name}")));
        for key in keys {
            match seen.get(&key) {
                Some(&other) => {
                    let (a, b) = (root(&mut parent, index), root(&mut parent, other));
                    if a != b {
                        parent[a] = b;
                    }
                }
                None => {
                    seen.insert(key, index);
                }
            }
        }
    }

    let mut groups: BTreeMap<usize, Vec<Id>> = BTreeMap::new();
    for (index, card) in cards.iter().enumerate() {
        let group = root(&mut parent, index);
        groups.entry(group).or_default().push(card.id());
    }
    let mut groups: Vec<Vec<Id>> = groups
        .into_values()
        .filter(|ids| ids.len() > 1)
        .map(|mut ids| {
            ids.sort_unstable();
            ids
        })
        .collect();
    groups.sort();
    groups
}

/// Folds `others` into `keep`: lists are unioned, empty fields filled, notes concatenated.
pub fn merge_cards(keep: &ContactCard, others: &[ContactCard]) -> ContactCard {
    fn fill(target: &mut String, source: &str) {
        if target.trim().is_empty() {
            *target = source.to_owned();
        }
    }
    fn union_by<K: PartialEq>(
        target: &mut Vec<String>,
        source: &[String],
        key: impl Fn(&str) -> K,
    ) {
        for value in source {
            let value_key = key(value.as_str());
            if !value.trim().is_empty()
                && !target
                    .iter()
                    .any(|existing| key(existing.as_str()) == value_key)
            {
                target.push(value.clone());
            }
        }
    }

    /// Unions values, keeping each value's label aligned (a known value gains a missing label).
    fn union_labeled<K: PartialEq>(
        (values, labels): (&mut Vec<String>, &mut Vec<String>),
        (source, source_labels): (&[String], &[String]),
        key: impl Fn(&str) -> K,
    ) {
        labels.resize(values.len(), String::new());
        for (index, value) in source.iter().enumerate() {
            let label = source_labels.get(index).cloned().unwrap_or_default();
            let value_key = key(value.as_str());
            match values.iter().position(|v| key(v.as_str()) == value_key) {
                Some(found) => fill(&mut labels[found], &label),
                None if !value.trim().is_empty() => {
                    values.push(value.clone());
                    labels.push(label);
                }
                None => {}
            }
        }
        trim_labels(labels);
    }

    let mut merged = keep.clone();
    for other in others {
        let (contact, details) = (&mut merged.contact, &mut merged.details);
        fill(&mut contact.display_name, &other.contact.display_name);
        fill(&mut contact.company, &other.contact.company);
        fill(&mut contact.job_title, &other.contact.job_title);
        let notes = other.contact.notes.trim();
        if !notes.is_empty() && !contact.notes.contains(notes) {
            contact.notes = if contact.notes.trim().is_empty() {
                notes.to_owned()
            } else {
                format!("{}\n\n{notes}", contact.notes.trim_end())
            };
        }
        contact.favorite |= other.contact.favorite;
        union_labeled(
            (&mut contact.emails, &mut details.email_labels),
            (&other.contact.emails, &other.details.email_labels),
            |email| email.trim().to_lowercase(),
        );
        union_labeled(
            (&mut contact.phones, &mut details.phone_labels),
            (&other.contact.phones, &other.details.phone_labels),
            phone_digits,
        );

        fill(&mut details.first_name, &other.details.first_name);
        fill(&mut details.last_name, &other.details.last_name);
        fill(&mut details.nickname, &other.details.nickname);
        fill(&mut details.website, &other.details.website);
        details.birthday = details.birthday.or(other.details.birthday);
        if details.photo.is_none() {
            details.photo = other.details.photo.clone();
        }
        for address in &other.details.addresses {
            if !details.addresses.contains(address) {
                details.addresses.push(address.clone());
            }
        }
        union_by(
            &mut details.categories,
            &other.details.categories,
            str::to_lowercase,
        );
    }
    merged
}

// ---------- commands ----------

/// Contact mutations. Wrap in `Command::People` and run through `History::run`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "args", rename_all = "snake_case")]
pub enum PeopleCommand {
    /// Creates (id 0) or replaces a contact and its details.
    SaveContact(Box<ContactCard>),
    /// Deletes contacts (and their group memberships).
    DeleteContacts(Vec<Id>),
    SetFavorite {
        id: Id,
        favorite: bool,
    },
    /// Folds `others` into `keep` (see [`merge_cards`]) and deletes them.
    Merge {
        keep: Id,
        others: Vec<Id>,
    },
    SaveGroup(Box<ContactGroup>),
    DeleteGroup(Id),
}

impl PeopleCommand {
    pub fn label(&self) -> String {
        match self {
            Self::SaveContact(card) if card.contact.id == 0 => "Add contact".into(),
            Self::SaveContact(_) => "Edit contact".into(),
            Self::DeleteContacts(ids) if ids.len() == 1 => "Delete contact".into(),
            Self::DeleteContacts(ids) => format!("Delete {} contacts", ids.len()),
            Self::SetFavorite { favorite: true, .. } => "Add to favorites".into(),
            Self::SetFavorite {
                favorite: false, ..
            } => "Remove from favorites".into(),
            Self::Merge { others, .. } => format!("Merge {} contacts", others.len() + 1),
            Self::SaveGroup(group) if group.id == 0 => "Create group".into(),
            Self::SaveGroup(_) => "Edit group".into(),
            Self::DeleteGroup(_) => "Delete group".into(),
        }
    }
}

fn wrap(command: PeopleCommand) -> Command {
    Command::People(command)
}

/// Applies `command` inside the caller's transaction and returns its inverse.
pub(crate) fn apply(store: &Store, command: PeopleCommand) -> Result<Command> {
    match command {
        PeopleCommand::SaveContact(card) => {
            let mut card = *card;
            let previous = if card.id() == 0 {
                None
            } else {
                contact_card(store, card.id())?
            };
            let id = write_card(store, &mut card)?;
            Ok(wrap(match previous {
                Some(old) => PeopleCommand::SaveContact(Box::new(old)),
                None => PeopleCommand::DeleteContacts(vec![id]),
            }))
        }
        PeopleCommand::DeleteContacts(ids) => {
            let mut inverses = Vec::with_capacity(ids.len());
            for &id in &ids {
                let card = require_card(store, id)?;
                store.delete::<Contact>(id)?;
                inverses.push(wrap(PeopleCommand::SaveContact(Box::new(card))));
            }
            inverses.extend(rewrite_members(store, |member| {
                (!ids.contains(&member)).then_some(member)
            })?);
            Ok(Command::Batch(inverses))
        }
        PeopleCommand::SetFavorite { id, favorite } => {
            let mut card = require_card(store, id)?;
            let previous = card.clone();
            card.contact.favorite = favorite;
            write_card(store, &mut card)?;
            Ok(wrap(PeopleCommand::SaveContact(Box::new(previous))))
        }
        PeopleCommand::Merge { keep, others } => {
            let kept = require_card(store, keep)?;
            let others = others
                .into_iter()
                .filter(|&id| id != keep)
                .map(|id| require_card(store, id))
                .collect::<Result<Vec<_>>>()?;
            if others.is_empty() {
                return Err(Error::Invalid("nothing to merge".into()));
            }
            let mut merged = merge_cards(&kept, &others);
            write_card(store, &mut merged)?;
            let mut inverses = vec![wrap(PeopleCommand::SaveContact(Box::new(kept)))];
            let other_ids: Vec<Id> = others.iter().map(ContactCard::id).collect();
            inverses.extend(rewrite_members(store, |member| {
                Some(if other_ids.contains(&member) {
                    keep
                } else {
                    member
                })
            })?);
            for card in others {
                store.delete::<Contact>(card.id())?;
                inverses.push(wrap(PeopleCommand::SaveContact(Box::new(card))));
            }
            Ok(Command::Batch(inverses))
        }
        PeopleCommand::SaveGroup(group) => {
            let mut group = *group;
            group.name = group.name.trim().to_owned();
            if group.name.is_empty() {
                return Err(Error::Invalid("a group needs a name".into()));
            }
            let previous = if group.id == 0 {
                None
            } else {
                store.get::<ContactGroup>(group.id)?
            };
            let id = store.save(&mut group)?;
            Ok(wrap(match previous {
                Some(old) => PeopleCommand::SaveGroup(Box::new(old)),
                None => PeopleCommand::DeleteGroup(id),
            }))
        }
        PeopleCommand::DeleteGroup(id) => {
            let old = store.require::<ContactGroup>(id)?;
            store.delete::<ContactGroup>(id)?;
            Ok(wrap(PeopleCommand::SaveGroup(Box::new(old))))
        }
    }
}

/// Saves contact + details; fills an empty display name. Returns the contact id.
fn write_card(store: &Store, card: &mut ContactCard) -> Result<Id> {
    let name = card.name();
    if name.is_empty() {
        return Err(Error::Invalid("a contact needs a name or an email".into()));
    }
    card.contact.display_name = name;
    card.contact.emails.retain(|email| !email.trim().is_empty());
    card.contact.phones.retain(|phone| !phone.trim().is_empty());
    card.details.addresses.retain(|address| !address.is_empty());
    let id = store.save(&mut card.contact)?;
    card.details.id = id;
    store.save(&mut card.details)?;
    Ok(id)
}

/// Maps every group member (`None` drops it, duplicates are removed); returns the
/// inverses restoring the groups that changed.
fn rewrite_members(store: &Store, map: impl Fn(Id) -> Option<Id>) -> Result<Vec<Command>> {
    let mut inverses = Vec::new();
    for group in list_contact_groups(store)? {
        let mut seen = HashSet::new();
        let members: Vec<Id> = group
            .members
            .iter()
            .filter_map(|&member| map(member))
            .filter(|member| seen.insert(*member))
            .collect();
        if members != group.members {
            let mut updated = ContactGroup {
                members,
                ..group.clone()
            };
            store.save(&mut updated)?;
            inverses.push(wrap(PeopleCommand::SaveGroup(Box::new(group))));
        }
    }
    Ok(inverses)
}

// ---------- persistence ----------

fn json<T: Serialize>(value: &T) -> Result<Value> {
    Ok(Value::Text(serde_json::to_string(value)?))
}

fn from_json<T: serde::de::DeserializeOwned>(row: &Row, index: usize) -> rusqlite::Result<T> {
    let raw: String = row.get(index)?;
    serde_json::from_str(&raw).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(err))
    })
}

impl Record for ContactDetails {
    const KIND: &'static str = "contact details";
    const TABLE: &'static str = "contact_details";
    const COLUMNS: &'static [&'static str] = &[
        "first_name",
        "last_name",
        "nickname",
        "birthday",
        "website",
        "photo",
        "addresses_json",
        "categories_json",
        "email_labels_json",
        "phone_labels_json",
    ];

    fn id(&self) -> Id {
        self.id
    }
    fn set_id(&mut self, id: Id) {
        self.id = id;
    }
    fn values(&self) -> Result<Vec<Value>> {
        Ok(vec![
            Value::Text(self.first_name.clone()),
            Value::Text(self.last_name.clone()),
            Value::Text(self.nickname.clone()),
            self.birthday.map_or(Value::Null, |date| {
                Value::Text(date.format("%Y-%m-%d").to_string())
            }),
            Value::Text(self.website.clone()),
            self.photo.clone().map_or(Value::Null, Value::Text),
            json(&self.addresses)?,
            json(&self.categories)?,
            json(&self.email_labels)?,
            json(&self.phone_labels)?,
        ])
    }
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        let birthday: Option<String> = row.get(4)?;
        Ok(Self {
            id: row.get(0)?,
            first_name: row.get(1)?,
            last_name: row.get(2)?,
            nickname: row.get(3)?,
            // A malformed date is dropped rather than failing the whole contact list.
            birthday: birthday.and_then(|raw| NaiveDate::parse_from_str(&raw, "%Y-%m-%d").ok()),
            website: row.get(5)?,
            photo: row.get(6)?,
            addresses: from_json(row, 7)?,
            categories: from_json(row, 8)?,
            email_labels: from_json(row, 9)?,
            phone_labels: from_json(row, 10)?,
        })
    }
}

impl Record for ContactGroup {
    const KIND: &'static str = "contact group";
    const TABLE: &'static str = "contact_groups";
    const COLUMNS: &'static [&'static str] = &["name", "members_json"];

    fn id(&self) -> Id {
        self.id
    }
    fn set_id(&mut self, id: Id) {
        self.id = id;
    }
    fn values(&self) -> Result<Vec<Value>> {
        Ok(vec![Value::Text(self.name.clone()), json(&self.members)?])
    }
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            name: row.get(1)?,
            members: from_json(row, 2)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::History;
    use crate::model::{Account, Address, Attendee, Calendar, ResponseStatus};
    use crate::store::tests::{account, folder, message, store};
    use chrono::TimeZone;

    fn card(account: &Account, name: &str, email: &str) -> ContactCard {
        let mut card = ContactCard::new(account.id);
        card.contact.display_name = name.into();
        if !email.is_empty() {
            card.contact.emails.push(email.into());
        }
        card
    }

    fn run(store: &Store, history: &mut History, command: PeopleCommand) {
        history.run(store, Command::People(command)).unwrap();
    }

    fn rich_card(account: &Account) -> ContactCard {
        let mut card = card(account, "Ada Lovelace", "ada@example.com");
        card.contact.phones = vec!["+44 20 1234 5678".into()];
        card.details = ContactDetails {
            id: 0,
            first_name: "Ada".into(),
            last_name: "Lovelace".into(),
            nickname: "Countess".into(),
            birthday: NaiveDate::from_ymd_opt(1815, 12, 10),
            website: "https://ada.example".into(),
            photo: Some("data:image/png;base64,AAAA".into()),
            addresses: vec![PostalAddress {
                label: "home".into(),
                street: "12 St James's Square".into(),
                city: "London".into(),
                ..Default::default()
            }],
            categories: vec!["Friends".into()],
            email_labels: vec!["work".into()],
            phone_labels: vec!["mobile".into()],
        };
        card
    }

    #[test]
    fn save_edit_delete_undo_redo() {
        let store = store();
        let account = account(&store);
        let mut history = History::new();

        run(
            &store,
            &mut history,
            PeopleCommand::SaveContact(Box::new(rich_card(&account))),
        );
        let created = list_contact_cards(&store).unwrap().remove(0);
        assert_eq!(created.details.id, created.id());
        assert_eq!(created.details.nickname, "Countess");
        assert_eq!(history.undo_label(), Some("Add contact"));

        let mut edited = created.clone();
        edited.contact.company = "Analytical Engines".into();
        edited.details.addresses.clear();
        run(
            &store,
            &mut history,
            PeopleCommand::SaveContact(Box::new(edited.clone())),
        );
        assert_eq!(contact_card(&store, created.id()).unwrap(), Some(edited));

        run(
            &store,
            &mut history,
            PeopleCommand::DeleteContacts(vec![created.id()]),
        );
        assert!(contact_card(&store, created.id()).unwrap().is_none());
        assert!(store.get::<ContactDetails>(created.id()).unwrap().is_none());

        history.undo(&store).unwrap();
        history.undo(&store).unwrap();
        assert_eq!(
            contact_card(&store, created.id()).unwrap(),
            Some(created.clone())
        );
        history.undo(&store).unwrap();
        assert!(list_contact_cards(&store).unwrap().is_empty());
        history.redo(&store).unwrap();
        assert_eq!(contact_card(&store, created.id()).unwrap(), Some(created));
    }

    #[test]
    fn contact_without_name_or_email_is_rejected() {
        let store = store();
        let account = account(&store);
        let mut history = History::new();
        let empty = ContactCard::new(account.id);
        assert!(
            history
                .run(
                    &store,
                    Command::People(PeopleCommand::SaveContact(Box::new(empty)))
                )
                .is_err()
        );
        assert!(!history.can_undo());
    }

    #[test]
    fn display_name_falls_back_to_names_then_email() {
        let store = store();
        let account = account(&store);
        let mut history = History::new();
        let mut by_names = ContactCard::new(account.id);
        by_names.details.first_name = "Grace".into();
        by_names.details.last_name = "Hopper".into();
        let by_email = card(&account, "", "linus@example.com");
        run(
            &store,
            &mut history,
            PeopleCommand::SaveContact(Box::new(by_names)),
        );
        run(
            &store,
            &mut history,
            PeopleCommand::SaveContact(Box::new(by_email)),
        );
        let names: Vec<String> = list_contact_cards(&store)
            .unwrap()
            .iter()
            .map(|card| card.contact.display_name.clone())
            .collect();
        assert_eq!(names, vec!["Grace Hopper", "linus@example.com"]);
    }

    #[test]
    fn favorite_toggle_is_undoable() {
        let store = store();
        let account = account(&store);
        let mut history = History::new();
        run(
            &store,
            &mut history,
            PeopleCommand::SaveContact(Box::new(card(&account, "Bob", ""))),
        );
        let id = list_contact_cards(&store).unwrap()[0].id();
        run(
            &store,
            &mut history,
            PeopleCommand::SetFavorite { id, favorite: true },
        );
        assert!(store.require::<Contact>(id).unwrap().favorite);
        history.undo(&store).unwrap();
        assert!(!store.require::<Contact>(id).unwrap().favorite);
    }

    #[test]
    fn groups_follow_deletes_and_undo() {
        let store = store();
        let account = account(&store);
        let mut history = History::new();
        for name in ["Ann", "Ben"] {
            run(
                &store,
                &mut history,
                PeopleCommand::SaveContact(Box::new(card(&account, name, ""))),
            );
        }
        let ids: Vec<Id> = list_contact_cards(&store)
            .unwrap()
            .iter()
            .map(ContactCard::id)
            .collect();
        run(
            &store,
            &mut history,
            PeopleCommand::SaveGroup(Box::new(ContactGroup {
                id: 0,
                name: " Team ".into(),
                members: ids.clone(),
            })),
        );
        let group = list_contact_groups(&store).unwrap().remove(0);
        assert_eq!(group.name, "Team");

        run(
            &store,
            &mut history,
            PeopleCommand::DeleteContacts(vec![ids[0]]),
        );
        assert_eq!(
            list_contact_groups(&store).unwrap()[0].members,
            vec![ids[1]]
        );
        history.undo(&store).unwrap();
        assert_eq!(list_contact_groups(&store).unwrap()[0], group);

        run(&store, &mut history, PeopleCommand::DeleteGroup(group.id));
        assert!(list_contact_groups(&store).unwrap().is_empty());
        history.undo(&store).unwrap();
        assert_eq!(list_contact_groups(&store).unwrap(), vec![group]);
    }

    #[test]
    fn finds_duplicates_by_email_and_name() {
        let account = Account {
            id: 1,
            name: "A".into(),
            email: "a@x".into(),
            color: 0,
        };
        let mut cards = vec![
            card(&account, "Ada Lovelace", "ada@example.com"),
            card(&account, "A. Lovelace", "ADA@example.com "),
            card(&account, "ada  lovelace", ""),
            card(&account, "Bob", "bob@example.com"),
            card(&account, "Carol", ""),
            card(&account, "carol", "carol@example.com"),
        ];
        for (index, card) in cards.iter_mut().enumerate() {
            card.contact.id = index as Id + 1;
        }
        assert_eq!(find_duplicates(&cards), vec![vec![1, 2, 3], vec![5, 6]]);
    }

    #[test]
    fn merge_combines_and_undo_restores() {
        let store = store();
        let account = account(&store);
        let mut history = History::new();
        let mut first = card(&account, "Ada", "ada@example.com");
        first.contact.phones = vec!["+1 (555) 010-0000".into()];
        let mut second = rich_card(&account);
        second.contact.display_name = "Ada L.".into();
        second.contact.emails.push("ada@work.example".into());
        second.contact.phones = vec!["15550100000".into(), "+33 1 23".into()];
        second.contact.notes = "Met at the Royal Society".into();
        run(
            &store,
            &mut history,
            PeopleCommand::SaveContact(Box::new(first)),
        );
        run(
            &store,
            &mut history,
            PeopleCommand::SaveContact(Box::new(second)),
        );
        let before = list_contact_cards(&store).unwrap();
        let (keep, other) = (before[0].id(), before[1].id());
        run(
            &store,
            &mut history,
            PeopleCommand::SaveGroup(Box::new(ContactGroup {
                id: 0,
                name: "Team".into(),
                members: vec![other, keep],
            })),
        );

        run(
            &store,
            &mut history,
            PeopleCommand::Merge {
                keep,
                others: vec![other],
            },
        );
        let after = list_contact_cards(&store).unwrap();
        assert_eq!(after.len(), 1);
        let merged = &after[0];
        assert_eq!(merged.contact.display_name, "Ada");
        assert_eq!(
            merged.contact.emails,
            vec!["ada@example.com", "ada@work.example"]
        );
        assert_eq!(merged.contact.phones, vec!["+1 (555) 010-0000", "+33 1 23"]);
        assert_eq!(merged.contact.notes, "Met at the Royal Society");
        assert_eq!(
            merged.details.birthday,
            NaiveDate::from_ymd_opt(1815, 12, 10)
        );
        assert_eq!(merged.details.addresses.len(), 1);
        assert_eq!(list_contact_groups(&store).unwrap()[0].members, vec![keep]);

        history.undo(&store).unwrap();
        assert_eq!(list_contact_cards(&store).unwrap(), before);
        assert_eq!(
            list_contact_groups(&store).unwrap()[0].members,
            vec![other, keep]
        );
    }

    #[test]
    fn sections_and_search() {
        let account = Account {
            id: 1,
            name: "A".into(),
            email: "a@x".into(),
            color: 0,
        };
        let mut eric = card(&account, "Éric Durand", "eric@example.com");
        eric.contact.phones = vec!["+33 6 12 34 56 78".into()];
        assert_eq!(eric.section_letter(), 'E');
        assert_eq!(card(&account, "42 Studio", "").section_letter(), '#');
        assert_eq!(card(&account, "", "zoe@example.com").section_letter(), 'Z');
        assert!(eric.matches("durand"));
        assert!(eric.matches("EXAMPLE.com"));
        assert!(eric.matches("612 345"));
        assert!(!eric.matches("bob"));
    }

    #[test]
    fn related_mail_and_meetings() {
        let store = store();
        let account = account(&store);
        let inbox = folder(&store, account.id, crate::model::FolderRole::Inbox);
        let mut from_ada = message(account.id, inbox.id);
        store.save(&mut from_ada).unwrap();
        let mut to_ada = message(account.id, inbox.id);
        to_ada.from = Address::new("Me", "me@example.com");
        to_ada.to = vec![Address::new("Ada", "Ada@Example.com")];
        to_ada.date = Utc.with_ymd_and_hms(2026, 11, 5, 9, 0, 0).unwrap();
        store.save(&mut to_ada).unwrap();
        let mut other = message(account.id, inbox.id);
        other.from = Address::new("Bob", "bob@example.com");
        store.save(&mut other).unwrap();

        let emails = vec!["ada@example.com".to_owned()];
        let found = recent_messages_with(&store, &emails, 10).unwrap();
        assert_eq!(
            found.iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![to_ada.id, from_ada.id]
        );

        let mut calendar = Calendar {
            id: 0,
            account_id: account.id,
            name: "Cal".into(),
            color: 0,
        };
        store.save(&mut calendar).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 11, 14, 8, 0, 0).unwrap();
        let mut meeting = CalendarEvent {
            id: 0,
            calendar_id: calendar.id,
            title: "1:1".into(),
            location: String::new(),
            description: String::new(),
            start: now + Duration::days(2),
            end: now + Duration::days(2) + Duration::hours(1),
            all_day: false,
            recurrence: None,
            attendees: vec![Attendee {
                address: Address::new("Ada", "ADA@example.com"),
                status: ResponseStatus::Accepted,
                optional: false,
            }],
            reminders: vec![],
            categories: vec![],
        };
        store.save(&mut meeting).unwrap();
        let mut unrelated = meeting.clone();
        unrelated.id = 0;
        unrelated.attendees.clear();
        store.save(&mut unrelated).unwrap();
        assert_eq!(
            upcoming_events_with(&store, &emails, now, 30).unwrap(),
            vec![meeting]
        );
    }
    #[test]
    fn suggestions_skip_contacts_accounts_and_robots() {
        let store = store();
        let account = account(&store);
        let inbox = folder(&store, account.id, crate::model::FolderRole::Inbox);
        let mut first = message(account.id, inbox.id);
        first.from = Address::new("Ada Lovelace", "Ada@Example.com");
        first.cc = vec![
            Address::new("", "noreply@shop.example"),
            Address::new("Known", "known@example.com"),
        ];
        store.save(&mut first).unwrap();
        let mut second = message(account.id, inbox.id);
        second.date += Duration::days(1);
        second.from = Address::new("Grace", "grace@example.com");
        second.cc = vec![Address::new("", "ada@example.com")];
        store.save(&mut second).unwrap();

        let known = card(&account, "Known", "known@example.com");
        let found = suggested_contacts(&store, &[known], 10).unwrap();
        let emails: Vec<&str> = found.iter().map(|s| s.address.email.as_str()).collect();
        assert_eq!(emails, vec!["grace@example.com", "ada@example.com"]);
        assert_eq!(found[1].messages, 2);
        assert_eq!(
            found[1].address.name, "Ada Lovelace",
            "a name fills a bare address"
        );

        let saved = card_from_address(&found[1].address, account.id);
        assert_eq!(saved.details.first_name, "Ada");
        assert_eq!(saved.details.last_name, "Lovelace");
        assert_eq!(saved.contact.emails, vec!["ada@example.com"]);
    }

    #[test]
    fn labels_persist_and_merge_aligned() {
        let store = store();
        let account = account(&store);
        let mut history = History::new();
        let saved = rich_card(&account);
        run(
            &store,
            &mut history,
            PeopleCommand::SaveContact(Box::new(saved)),
        );
        let stored = list_contact_cards(&store).unwrap().remove(0);
        assert_eq!(stored.details.email_label(0), "work");
        assert_eq!(stored.details.phone_label(0), "mobile");
        assert_eq!(stored.details.phone_label(5), "");

        let mut other = card(&account, "Ada", "ADA@example.com");
        other.contact.emails.push("ada@home.example".into());
        other.details.email_labels = vec!["".into(), "home".into()];
        let merged = merge_cards(&stored, &[other]);
        assert_eq!(
            merged.contact.emails,
            vec!["ada@example.com", "ada@home.example"]
        );
        assert_eq!(merged.details.email_labels, vec!["work", "home"]);
    }
}
