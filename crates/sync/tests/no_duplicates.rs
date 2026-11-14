//! Regression tests for duplicated mail: the store must hold each server message once, however
//! often it is synced: state file lost (app killed before saving it), restarts, two sync
//! cycles with the same stale cursors (F9 / IDLE / timer racing), two workers at once.
//! Runs the real IMAP backend against the in-process `mailbox-devserver`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use mailbox_core::{Folder, FolderRole, Id, Message, Store};
use mailbox_devserver::{Config, DevServer, bulk_messages};
use mailbox_sync::config::{AccountConfig, AuthConfig, Security, ServerConfig};
use mailbox_sync::engine::{SyncInput, run_cycle};
use mailbox_sync::imap_smtp::ImapSmtpBackend;
use mailbox_sync::local::store_fetched;
use mailbox_sync::worker::BackendFactory;
use mailbox_sync::{MailBackend, Request, SyncEvent, SyncResult, SyncState, WorkerHandle};

/// Seeded INBOX (4) + Archive (1) + bulk INBOX mail.
const BULK: usize = 50;
const TOTAL: usize = 5 + BULK;

fn server() -> (DevServer, AccountConfig) {
    let server = DevServer::start(Config {
        seed: true,
        ..Config::default()
    })
    .unwrap();
    server.with_store(|store| bulk_messages(store, BULK));
    let endpoint = |port| ServerConfig {
        host: "127.0.0.1".into(),
        port,
        security: Security::Plain,
    };
    let config = AccountConfig {
        email: "me@mailbox.test".into(),
        username: None,
        imap: endpoint(server.imap_port()),
        smtp: endpoint(server.smtp_port()),
        auth: AuthConfig::Password,
        offline_days: 30,
        poll_minutes: 5,
        idle: false,
        save_sent: None,
    };
    (server, config)
}

fn backend(config: &AccountConfig) -> ImapSmtpBackend {
    ImapSmtpBackend::with_password(config.clone(), "any password").unwrap()
}

fn worker(config: &AccountConfig) -> WorkerHandle {
    let config = config.clone();
    let factory: BackendFactory =
        Arc::new(move || -> SyncResult<Box<dyn MailBackend>> { Ok(Box::new(backend(&config))) });
    WorkerHandle::spawn(factory, None).unwrap()
}

fn input(state: &SyncState) -> SyncInput {
    SyncInput {
        push: Vec::new(),
        cursors: state.cursors(),
        offline_days: 30,
        max_initial: 5000,
    }
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mailbox-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn account(store: &Store) -> Id {
    let mut account = mailbox_core::Account {
        id: 0,
        name: "Me".into(),
        email: "me@mailbox.test".into(),
        color: 0,
    };
    store.save(&mut account).unwrap()
}

/// Applies sync events like the app's bridge: folders mapped onto existing local ones (by
/// role, else name), new mail through [`store_fetched`], cursors, vanished mail.
fn apply(store: &Store, state: &mut SyncState, account_id: Id, events: Vec<SyncEvent>) {
    for event in events {
        match event {
            SyncEvent::Folders(folders) => {
                for remote in folders {
                    let local = store.list_folders(account_id).unwrap();
                    let known = state.local_folder(&remote.path);
                    let matching = local.iter().find(|folder| match remote.role {
                        FolderRole::Custom => folder.name == remote.name,
                        role => folder.role == role,
                    });
                    let id = known
                        .or(matching.map(|folder| folder.id))
                        .unwrap_or_else(|| {
                            let mut folder = Folder {
                                id: 0,
                                account_id,
                                parent_id: None,
                                name: remote.name.clone(),
                                role: remote.role,
                                position: 0,
                            };
                            store.save(&mut folder).unwrap()
                        });
                    state.folder_mut(&remote).local_id = Some(id);
                }
            }
            SyncEvent::NewMessages {
                path,
                uid_validity,
                messages,
            } => {
                for message in messages {
                    store_fetched(store, state, account_id, &path, uid_validity, message).unwrap();
                }
            }
            SyncEvent::FolderSynced {
                path,
                uid_validity,
                highest_uid,
            } => state.folder_synced(&path, uid_validity, highest_uid),
            SyncEvent::Vanished { path, uids } => {
                for id in state.vanish(&path, &uids) {
                    store.delete::<Message>(id).unwrap();
                }
            }
            SyncEvent::Finished { error } => assert_eq!(error, None),
            _ => {}
        }
    }
}

fn sync_once(config: &AccountConfig, store: &Store, state: &mut SyncState, account_id: Id) {
    let mut events = Vec::new();
    let mut backend = backend(config);
    run_cycle(&mut backend, input(state), Utc::now(), &mut |event| {
        events.push(event)
    })
    .unwrap();
    apply(store, state, account_id, events);
}

fn messages(store: &Store, account_id: Id) -> Vec<Message> {
    store
        .list_folders(account_id)
        .unwrap()
        .iter()
        .flat_map(|folder| store.list_messages(folder.id).unwrap())
        .collect()
}

/// Events of one worker request (until `Finished`).
fn until_finished(worker: &WorkerHandle) -> Vec<SyncEvent> {
    let mut events = Vec::new();
    while let Some(event) = worker.recv_timeout(Duration::from_secs(30)) {
        let done = matches!(event, SyncEvent::Finished { .. });
        events.push(event);
        if done {
            break;
        }
    }
    events
}

#[test]
fn lost_state_restarts_and_new_mail_never_duplicate() {
    let (server, config) = server();
    let dir = temp_dir("no-dup-restart");
    let (db, state_file) = (dir.join("mailbox.db"), dir.join("sync").join("1.json"));
    let store = Store::open(&db).unwrap();
    let account_id = account(&store);

    let mut state = SyncState::default();
    sync_once(&config, &store, &mut state, account_id);
    assert_eq!(messages(&store, account_id).len(), TOTAL);

    // Killed before the state was saved: the next run starts from an empty state.
    let mut state = SyncState::default();
    sync_once(&config, &store, &mut state, account_id);
    assert_eq!(messages(&store, account_id).len(), TOTAL);

    // Killed mid-cycle: half of the links saved.
    for folder in state.folders.values_mut() {
        let keep = folder.messages.len() / 2;
        folder.messages = std::mem::take(&mut folder.messages)
            .into_iter()
            .take(keep)
            .collect();
        folder.highest_uid = 0;
    }
    sync_once(&config, &store, &mut state, account_id);
    assert_eq!(messages(&store, account_id).len(), TOTAL);

    // Clean restart: store and state reopened from disk.
    state.save(&state_file).unwrap();
    drop(store);
    let store = Store::open(&db).unwrap();
    let mut state = SyncState::load(&state_file).unwrap();
    sync_once(&config, &store, &mut state, account_id);
    assert_eq!(messages(&store, account_id).len(), TOTAL);

    // New mail still arrives, once.
    server.with_store(|store| bulk_messages(store, 1));
    sync_once(&config, &store, &mut state, account_id);
    sync_once(&config, &store, &mut SyncState::default(), account_id);
    assert_eq!(messages(&store, account_id).len(), TOTAL + 1);

    // Every stored message has its server identity.
    for message in messages(&store, account_id) {
        assert!(store.message_remote(message.id).unwrap().0.is_some());
    }
    server.stop();
    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn concurrent_sync_triggers_never_duplicate() {
    let (server, config) = server();
    let store = Store::open_in_memory().unwrap();
    let account_id = account(&store);
    let mut state = SyncState::default();

    // F9 / IDLE / timer firing while a cycle runs: two cycles queued with the same cursors.
    let first = worker(&config);
    assert!(first.request(Request::Sync(input(&state))));
    assert!(first.request(Request::Sync(input(&state))));
    // A second worker (account restarted while the old one still syncs) at the same time.
    let second = worker(&config);
    assert!(second.request(Request::Sync(input(&state))));

    let mut events = until_finished(&first);
    let mut other = until_finished(&second);
    let mut again = until_finished(&first);
    // Interleave as the bridge would see them when draining several channels.
    let mut mixed = Vec::new();
    while !(events.is_empty() && other.is_empty() && again.is_empty()) {
        for queue in [&mut events, &mut other, &mut again] {
            if !queue.is_empty() {
                mixed.push(queue.remove(0));
            }
        }
    }
    apply(&store, &mut state, account_id, mixed);

    let stored = messages(&store, account_id);
    assert_eq!(stored.len(), TOTAL);
    let mut ids: Vec<String> = stored
        .iter()
        .map(|m| store.message_remote(m.id).unwrap().1)
        .collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), TOTAL, "each Message-ID stored once");
    server.stop();
}

#[test]
fn notification_previews_skip_separator_lines() {
    let (server, config) = server();
    let store = Store::open_in_memory().unwrap();
    let account_id = account(&store);
    sync_once(&config, &store, &mut SyncState::default(), account_id);
    let notifications: Vec<Message> = messages(&store, account_id)
        .into_iter()
        .filter(|m| m.subject.contains("new notifications"))
        .collect();
    assert_eq!(notifications.len(), BULK);
    for message in notifications {
        assert!(
            message.preview.starts_with("See your notifications"),
            "{}",
            message.preview
        );
    }
    server.stop();
}
