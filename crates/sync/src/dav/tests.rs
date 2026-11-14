//! End-to-end flows against the in-process mock server: discovery, PROPFIND/REPORT/PUT/DELETE,
//! conflicts, sync tokens and the ctag/etag fallback.

use std::collections::HashMap;

use chrono::{TimeZone, Utc};
use mailbox_core::CalendarEvent;
use mailbox_core::calendar::EventDetails;
use mailbox_core::people::ContactCard;

use super::discovery::{Home, find_home, list_collections};
use super::http::{Auth, DavClient, parse_url};
use super::items::{self, LocalItem};
use super::mock::{MockDav, PASSWORD, USER};
use super::sync::{
    CollectionOutcome, CollectionState, DavState, ItemLink, RemoteChange, sync_account,
    sync_collection,
};
use super::{Kind, RemoteCollection};
use crate::SyncError;

fn client() -> DavClient {
    DavClient::new(Some(Auth::Basic {
        user: USER.into(),
        password: PASSWORD.into(),
    }))
}

fn ics(uid: &str, title: &str) -> String {
    let event = CalendarEvent {
        id: 0,
        calendar_id: 0,
        title: title.into(),
        location: String::new(),
        description: String::new(),
        start: Utc.with_ymd_and_hms(2026, 5, 4, 9, 0, 0).unwrap(),
        end: Utc.with_ymd_and_hms(2026, 5, 4, 10, 0, 0).unwrap(),
        all_day: false,
        recurrence: None,
        attendees: Vec::new(),
        reminders: Vec::new(),
        categories: Vec::new(),
    };
    let details = EventDetails {
        uid: uid.into(),
        ..EventDetails::default_for(1)
    };
    items::event_resource(&(event, details), &[])
}

fn item(local_id: i64, uid: &str, title: &str) -> LocalItem {
    LocalItem {
        local_id,
        uid: uid.into(),
        text: ics(uid, title),
    }
}

fn work(mock: &MockDav) -> RemoteCollection {
    let home = Home {
        kind: Kind::Calendar,
        url: mock.url("/cal/me/"),
        auto_schedule: false,
    };
    list_collections(&client(), &home)
        .unwrap()
        .into_iter()
        .find(|c| c.name == "Work")
        .unwrap()
}

/// What the bridge does after applying an upsert: link the stored item.
fn apply(outcome: &mut CollectionOutcome, local: &mut Vec<LocalItem>, next_id: &mut i64) {
    for change in std::mem::take(&mut outcome.changes) {
        match change {
            RemoteChange::Upsert {
                href,
                etag,
                text,
                local_id,
            } => {
                let id = local_id.unwrap_or_else(|| {
                    *next_id += 1;
                    *next_id
                });
                let uid = items::resource_uid(&text).unwrap();
                local.retain(|i| i.local_id != id);
                let stored = LocalItem {
                    local_id: id,
                    uid: uid.clone(),
                    text,
                };
                outcome.state.link(
                    href,
                    ItemLink {
                        local_id: id,
                        uid,
                        etag,
                        hash: stored.hash(),
                    },
                );
                local.push(stored);
            }
            RemoteChange::Delete { local_id } => local.retain(|i| i.local_id != local_id),
        }
    }
}

#[test]
fn discovery_uses_well_known_and_filters_collections() {
    let mock = MockDav::start(true);
    mock.server().auto_schedule = true;
    let start = parse_url(&mock.url("/")).unwrap();

    let home = find_home(&client(), &start, Kind::Calendar).unwrap();
    assert_eq!(home.url, mock.url("/cal/me/"));
    assert!(home.auto_schedule);
    let calendars = list_collections(&client(), &home).unwrap();
    assert_eq!(
        calendars.len(),
        1,
        "task-only calendar skipped: {calendars:?}"
    );
    assert_eq!(calendars[0].url, mock.url("/cal/me/work/"));
    assert_eq!(calendars[0].color, Some(0x3366CC));
    assert!(calendars[0].sync_token.is_some() && calendars[0].ctag.is_some());

    let book_home = find_home(&client(), &start, Kind::AddressBook).unwrap();
    assert!(!book_home.auto_schedule);
    let books = list_collections(&client(), &book_home).unwrap();
    assert_eq!(books[0].name, "Contacts");
    assert_eq!(books[0].kind, Kind::AddressBook);

    let wrong = DavClient::new(Some(Auth::Basic {
        user: USER.into(),
        password: "nope".into(),
    }));
    assert!(matches!(
        find_home(&wrong, &start, Kind::Calendar),
        Err(SyncError::Auth(_))
    ));
}

fn two_way(sync_tokens: bool) {
    let mock = MockDav::start(sync_tokens);
    mock.server()
        .put("/cal/me/work/a.ics", &ics("a@x", "From server"));
    let mut next_id = 100;
    let mut local = vec![item(7, "b@x", "From here")];

    // 1. Initial: download A, upload B.
    let remote = work(&mock);
    let mut outcome =
        sync_collection(&client(), &remote, CollectionState::new(&remote), &local).unwrap();
    assert_eq!(outcome.pushed, 1);
    assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
    assert!(matches!(
        &outcome.changes[..],
        [RemoteChange::Upsert { local_id: None, .. }]
    ));
    apply(&mut outcome, &mut local, &mut next_id);
    let state = outcome.state;
    assert_eq!(state.items.len(), 2);
    assert!(mock.server().resources.contains_key("/cal/me/work/b-x.ics"));
    let a_id = state.items[&mock.url("/cal/me/work/a.ics")].local_id;

    // 2. Nothing changed: no download, no upload.
    let remote = work(&mock);
    let gets = mock.server().count("REPORT");
    let outcome = sync_collection(&client(), &remote, state, &local).unwrap();
    assert!(outcome.changes.is_empty() && outcome.pushed == 0);
    let multigets = mock.server().count("REPORT") - gets - usize::from(sync_tokens);
    assert_eq!(multigets, 0, "our own upload is not downloaded again");
    let state = outcome.state;

    // 3. Server edits A, deletes nothing; we edit B and delete... nothing yet.
    mock.server()
        .put("/cal/me/work/a.ics", &ics("a@x", "Edited on server"));
    local.retain(|i| i.local_id != 7);
    local.push(item(7, "b@x", "Edited here"));
    let remote = work(&mock);
    let mut outcome = sync_collection(&client(), &remote, state, &local).unwrap();
    assert_eq!(outcome.pushed, 1);
    assert!(outcome.conflicts.is_empty());
    match &outcome.changes[..] {
        [RemoteChange::Upsert { local_id, text, .. }] => {
            assert_eq!(*local_id, Some(a_id));
            assert!(text.contains("Edited on server"));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(
        mock.server().resources["/cal/me/work/b-x.ics"]
            .1
            .contains("Edited here")
    );
    apply(&mut outcome, &mut local, &mut next_id);
    let state = outcome.state;

    // 4. Both sides edit A: server wins, the local version is kept as a copy.
    mock.server()
        .put("/cal/me/work/a.ics", &ics("a@x", "Server v3"));
    local.retain(|i| i.local_id != a_id);
    local.push(item(a_id, "a@x", "Local v3"));
    let remote = work(&mock);
    let mut outcome = sync_collection(&client(), &remote, state, &local).unwrap();
    assert_eq!(outcome.conflicts, vec![a_id]);
    assert_eq!(outcome.pushed, 0);
    assert!(
        mock.server().resources["/cal/me/work/a.ics"]
            .1
            .contains("Server v3")
    );
    apply(&mut outcome, &mut local, &mut next_id);
    let state = outcome.state;

    // 5. Deleted here (B) and deleted there (A).
    mock.server().delete("/cal/me/work/a.ics");
    local.retain(|i| i.local_id != 7);
    let remote = work(&mock);
    let outcome = sync_collection(&client(), &remote, state, &local).unwrap();
    assert_eq!(
        outcome.changes,
        vec![RemoteChange::Delete { local_id: a_id }]
    );
    assert_eq!(outcome.pushed, 1);
    assert!(outcome.state.items.is_empty());
    assert!(mock.server().resources.is_empty());
}

#[test]
fn two_way_sync_with_sync_tokens() {
    two_way(true);
}

#[test]
fn two_way_sync_with_ctag_and_etags() {
    two_way(false);
}

#[test]
fn stale_etag_on_push_is_a_conflict() {
    let mock = MockDav::start(false);
    let etag = mock
        .server()
        .put("/cal/me/work/a.ics", &ics("a@x", "Server"));
    let remote = work(&mock);
    let mut state = CollectionState::new(&remote);
    state.ctag = remote.ctag.clone();
    // We believe we have the latest version, but the etag is stale.
    let stale = LocalItem {
        local_id: 1,
        uid: "a@x".into(),
        text: ics("a@x", "Old"),
    };
    state.link(
        mock.url("/cal/me/work/a.ics"),
        ItemLink {
            local_id: 1,
            uid: "a@x".into(),
            etag: "\"old\"".into(),
            hash: stale.hash(),
        },
    );
    let edited = item(1, "a@x", "Local edit");
    let outcome = sync_collection(&client(), &remote, state, &[edited]).unwrap();
    assert_eq!(outcome.conflicts, vec![1]);
    match &outcome.changes[..] {
        [
            RemoteChange::Upsert {
                etag: got,
                local_id: Some(1),
                text,
                ..
            },
        ] => {
            assert_eq!(got, &etag);
            assert!(text.contains("Server"));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(
        mock.server().resources["/cal/me/work/a.ics"]
            .1
            .contains("Server")
    );
}

#[test]
fn recurring_event_with_exception_roundtrips_through_server() {
    let mock = MockDav::start(true);
    let at = |d, h| Utc.with_ymd_and_hms(2026, 6, d, h, 0, 0).unwrap();
    let mut master = CalendarEvent {
        id: 1,
        calendar_id: 1,
        title: "Weekly".into(),
        location: String::new(),
        description: String::new(),
        start: at(1, 9),
        end: at(1, 10),
        all_day: false,
        recurrence: Some("FREQ=WEEKLY;COUNT=4".into()),
        attendees: Vec::new(),
        reminders: Vec::new(),
        categories: Vec::new(),
    };
    let master_details = EventDetails {
        uid: "weekly@x".into(),
        time_zone: Some("UTC".into()),
        exdates: vec![at(8, 9)],
        ..EventDetails::default_for(1)
    };
    master.id = 1;
    let mut moved = master.clone();
    moved.id = 2;
    moved.recurrence = None;
    moved.start = at(8, 14);
    moved.end = at(8, 15);
    let moved_details = EventDetails {
        event_id: 2,
        master_id: Some(1),
        recurrence_id: Some(at(8, 9)),
        ..master_details.clone()
    };
    let text = items::event_resource(&(master, master_details), &[(moved, moved_details)]);
    let remote = work(&mock);
    let pushed = sync_collection(
        &client(),
        &remote,
        CollectionState::new(&remote),
        &[LocalItem {
            local_id: 1,
            uid: "weekly@x".into(),
            text,
        }],
    )
    .unwrap();
    assert_eq!(pushed.pushed, 1);

    // Another device downloads it.
    let fresh =
        sync_collection(&client(), &work(&mock), CollectionState::new(&remote), &[]).unwrap();
    let [RemoteChange::Upsert { text, .. }] = &fresh.changes[..] else {
        panic!("{:?}", fresh.changes);
    };
    let events = items::parse_event_resource(text).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].details.as_ref().unwrap().exdates, vec![at(8, 9)]);
    assert_eq!(
        events[1].details.as_ref().unwrap().recurrence_id,
        Some(at(8, 9))
    );
    assert_eq!(events[1].event.start, at(8, 14));
}

#[test]
fn account_cycle_syncs_calendars_and_contacts() {
    let mock = MockDav::start(true);
    let mut card = ContactCard::new(1);
    card.contact.display_name = "Grace Hopper".into();
    card.contact.emails = vec!["grace@example.com".into()];
    card.contact.phones = vec!["+1 555 0100".into()];
    card.details.email_labels = vec!["work".into()];
    card.details.phone_labels = vec!["mobile".into()];
    let contacts_url = mock.url("/card/me/contacts/");
    let local = HashMap::from([(
        contacts_url.clone(),
        vec![LocalItem {
            local_id: 5,
            uid: "grace".into(),
            text: items::contact_resource(&card, "grace"),
        }],
    )]);
    let start = parse_url(&mock.url("/")).unwrap();
    let outcome = sync_account(
        &client(),
        DavState::default(),
        &[Kind::Calendar, Kind::AddressBook],
        |_| Ok(start.clone()),
        &local,
    )
    .unwrap();
    assert_eq!(outcome.state.homes.len(), 2);
    assert_eq!(outcome.collections.len(), 2);
    let (book, result) = outcome
        .collections
        .iter()
        .find(|(c, _)| c.kind == Kind::AddressBook)
        .unwrap();
    assert_eq!(book.url, contacts_url);
    assert_eq!(result.as_ref().unwrap().pushed, 1);
    let stored = &mock.server().resources["/card/me/contacts/grace.vcf"].1;
    let (parsed, uid) = items::parse_contact_resource(stored, 9).unwrap();
    assert_eq!(
        (parsed.contact.display_name.as_str(), uid.as_str()),
        ("Grace Hopper", "grace")
    );
    // Email/phone labels travel as vCard TYPE params.
    assert_eq!(parsed.details.email_labels, vec!["work"]);
    assert_eq!(parsed.details.phone_labels, vec!["mobile"]);
}
