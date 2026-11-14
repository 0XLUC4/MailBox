//! End to end: the real IMAP/SMTP backend and sync worker against `mailbox-devserver`
//! running in-process on 127.0.0.1 (the server the app is tested with by hand).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use mailbox_core::{Address, FolderRole, Importance, Message, MessageFlags};
use mailbox_devserver::{Config, DevServer};
use mailbox_sync::config::{AccountConfig, AuthConfig, Security, ServerConfig};
use mailbox_sync::engine::{FolderCursor, SyncInput, run_cycle};
use mailbox_sync::formats::{OutgoingHeaders, build_outgoing};
use mailbox_sync::imap_smtp::{ImapSmtpBackend, test_connection};
use mailbox_sync::mime::parse_message;
use mailbox_sync::worker::BackendFactory;
use mailbox_sync::{
    AttachmentData, MailBackend, OutgoingMessage, PushOp, RemoteFlags, Request, SyncEvent,
    SyncResult, WorkerHandle,
};

const ME: &str = "me@mailbox.test";

fn start(seed: bool) -> (DevServer, AccountConfig) {
    let server = DevServer::start(Config {
        seed,
        ..Config::default()
    })
    .unwrap();
    let config = config_for(&server);
    (server, config)
}

fn config_for(server: &DevServer) -> AccountConfig {
    let endpoint = |port| ServerConfig {
        host: "127.0.0.1".into(),
        port,
        security: Security::Plain,
    };
    AccountConfig {
        email: ME.into(),
        username: None,
        imap: endpoint(server.imap_port()),
        smtp: endpoint(server.smtp_port()),
        auth: AuthConfig::Password,
        offline_days: 30,
        poll_minutes: 5,
        idle: true,
        save_sent: None,
    }
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

/// Events until the end of the current request.
fn until_done(worker: &WorkerHandle) -> Vec<SyncEvent> {
    let mut events = Vec::new();
    while let Some(event) = worker.recv_timeout(Duration::from_secs(20)) {
        let done = matches!(
            event,
            SyncEvent::Finished { .. } | SyncEvent::Sent { .. } | SyncEvent::SendFailed { .. }
        );
        events.push(event);
        if done {
            break;
        }
    }
    events
}

fn sync(
    backend: &mut dyn MailBackend,
    cursors: BTreeMap<String, FolderCursor>,
    push: Vec<PushOp>,
) -> Vec<SyncEvent> {
    let input = SyncInput {
        push,
        cursors,
        offline_days: 30,
        max_initial: 100,
    };
    let mut events = Vec::new();
    run_cycle(backend, input, Utc::now(), &mut |event| events.push(event)).unwrap();
    events
}

/// Cursors a caller would keep after applying `events`.
fn cursors(events: &[SyncEvent]) -> BTreeMap<String, FolderCursor> {
    let mut cursors: BTreeMap<String, FolderCursor> = BTreeMap::new();
    for event in events {
        match event {
            SyncEvent::NewMessages { path, messages } => {
                let cursor = cursors.entry(path.clone()).or_default();
                for message in messages {
                    cursor.known.insert(message.uid, message.flags);
                }
            }
            SyncEvent::FolderSynced {
                path,
                uid_validity,
                highest_uid,
            } => {
                let cursor = cursors.entry(path.clone()).or_default();
                cursor.uid_validity = *uid_validity;
                cursor.highest_uid = *highest_uid;
            }
            _ => {}
        }
    }
    cursors
}

fn subjects(events: &[SyncEvent], folder: &str) -> Vec<String> {
    let mut subjects: Vec<String> = events
        .iter()
        .flat_map(|event| match event {
            SyncEvent::NewMessages { path, messages } if path == folder => messages
                .iter()
                .map(|m| m.parsed.message.subject.clone())
                .collect(),
            _ => vec![],
        })
        .collect();
    subjects.sort();
    subjects
}

fn outgoing_message(to: &str) -> Message {
    Message {
        id: 1,
        account_id: 1,
        folder_id: 1,
        thread_id: String::new(),
        from: Address::new("Me", ME),
        to: vec![
            Address::new("Ada", "ada@example.org"),
            Address::new("Me", to),
        ],
        cc: vec![Address::new("", "cc@example.org")],
        subject: "Plans for Thursday".into(),
        date: Utc::now(),
        preview: String::new(),
        body_text: "See you at 10.".into(),
        body_html: Some("<p>See you at <b>10</b>.</p>".into()),
        flags: MessageFlags::default(),
        importance: Importance::High,
        categories: vec![],
        attachments: vec![],
    }
}

#[test]
fn connection_test_lists_every_special_folder() {
    let (_server, config) = start(false);
    let folders = test_connection(&config, Some("whatever")).unwrap();
    let roles: Vec<FolderRole> = folders.iter().map(|folder| folder.role).collect();
    for role in [
        FolderRole::Inbox,
        FolderRole::Sent,
        FolderRole::Drafts,
        FolderRole::Trash,
        FolderRole::Archive,
        FolderRole::Junk,
    ] {
        assert!(roles.contains(&role), "{role:?} missing in {roles:?}");
    }
}

#[test]
fn send_delivers_correct_mime_files_sent_and_reaches_a_local_inbox() {
    let (server, config) = start(false);
    let headers = OutgoingHeaders {
        message_id: Some("<out-1@mailbox.test>".into()),
        bcc: vec![Address::new("Hidden", "hidden@example.net")],
        in_reply_to: Some("<parent@example.org>".into()),
        references: vec!["<parent@example.org>".into()],
        read_receipt: false,
    };
    let attachment = AttachmentData {
        name: "notes.txt".into(),
        mime: "text/plain".into(),
        bytes: b"attached notes".to_vec(),
    };
    let raw = build_outgoing(&outgoing_message(ME), &headers, &[attachment]).unwrap();
    let recipients: Vec<String> = [
        "ada@example.org",
        ME,
        "cc@example.org",
        "hidden@example.net",
    ]
    .map(str::to_owned)
    .to_vec();

    let worker = worker(&config);
    assert!(worker.request(Request::Send(OutgoingMessage {
        local_id: 7,
        from: ME.into(),
        recipients: recipients.clone(),
        raw: raw.clone(),
        sent_path: Some("Sent".into()),
    })));
    assert_eq!(until_done(&worker), vec![SyncEvent::Sent { local_id: 7 }]);

    server.with_store(|store| {
        // SMTP: one delivery, every envelope recipient, no trace of Bcc in the message.
        assert_eq!(store.deliveries.len(), 1);
        let delivery = &store.deliveries[0];
        assert_eq!(delivery.from, ME);
        assert_eq!(delivery.recipients, recipients);
        let text = delivery.text();
        for expected in [
            "Subject: Plans for Thursday",
            "Message-ID: <out-1@mailbox.test>",
            "In-Reply-To: <parent@example.org>",
            "multipart/mixed",
            "multipart/alternative",
            "filename=\"notes.txt\"",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
        }
        assert!(!text.contains("hidden@example.net"), "Bcc leaked:\n{text}");
        let parsed = parse_message(&delivery.data, None).unwrap();
        assert_eq!(parsed.message.to.len(), 2);
        assert_eq!(parsed.message.cc[0].email, "cc@example.org");
        assert!(parsed.message.body_html.unwrap().contains("<b>10</b>"));
        assert_eq!(parsed.attachments[0].bytes, b"attached notes");

        // IMAP: a read copy in Sent, byte for byte; a copy for me in INBOX.
        let sent = store.messages("Sent");
        assert_eq!(sent.len(), 1);
        assert!(sent[0].has_flag("\\Seen"));
        assert_eq!(sent[0].raw, raw);
        assert_eq!(store.subjects("INBOX"), vec!["Plans for Thursday"]);
    });

    // The next sync downloads both copies.
    let events = sync(&mut backend(&config), BTreeMap::new(), vec![]);
    assert_eq!(subjects(&events, "INBOX"), vec!["Plans for Thursday"]);
    assert_eq!(subjects(&events, "Sent"), vec!["Plans for Thursday"]);
}

#[test]
fn send_fails_while_the_server_is_down_and_works_again_after() {
    let (server, config) = start(false);
    server.stop();
    let raw = build_outgoing(&outgoing_message(ME), &OutgoingHeaders::default(), &[]).unwrap();
    let message = OutgoingMessage {
        local_id: 3,
        from: ME.into(),
        recipients: vec![ME.into()],
        raw,
        sent_path: Some("Sent".into()),
    };
    let worker = worker(&config);
    worker.request(Request::Send(message.clone()));
    let events = until_done(&worker);
    assert!(
        matches!(
            events.as_slice(),
            [SyncEvent::SendFailed { local_id: 3, .. }]
        ),
        "{events:?}"
    );
    assert!(server.with_store(|store| store.deliveries.is_empty()));

    // The server is back (same ports): Retry sends it.
    let back = DevServer::start(Config {
        imap_port: config.imap.port,
        smtp_port: config.smtp.port,
        ..Config::default()
    })
    .unwrap();
    worker.request(Request::Send(message));
    assert_eq!(until_done(&worker), vec![SyncEvent::Sent { local_id: 3 }]);
    back.with_store(|store| {
        assert_eq!(store.deliveries.len(), 1);
        assert_eq!(store.messages("Sent").len(), 1);
    });
}

#[test]
fn sync_downloads_seeded_mail_then_only_new_mail() {
    let (server, config) = start(true);
    let mut backend = backend(&config);
    let events = sync(&mut backend, BTreeMap::new(), vec![]);
    assert_eq!(
        subjects(&events, "INBOX"),
        vec![
            "Planning meeting",
            "Re: Planning meeting",
            "Welcome to the devserver",
            "Your invoice"
        ]
    );
    assert_eq!(subjects(&events, "Archive"), vec!["Old notes"]);
    let invoice = events
        .iter()
        .flat_map(|event| match event {
            SyncEvent::NewMessages { messages, .. } => messages.clone(),
            _ => vec![],
        })
        .find(|message| message.parsed.message.subject == "Your invoice")
        .unwrap();
    assert!(invoice.has_body);
    assert_eq!(invoice.parsed.attachments[0].name, "invoice.txt");

    server.with_store(|store| {
        store.append(
            "INBOX",
            b"From: x@example.org\r\nSubject: Fresh\r\nMessage-ID: <fresh@x>\r\n\r\nhi\r\n",
            &[],
        )
    });
    let events = sync(&mut backend, cursors(&events), vec![]);
    assert_eq!(subjects(&events, "INBOX"), vec!["Fresh"]);
}

#[test]
fn local_flags_moves_and_deletes_are_pushed() {
    let (server, config) = start(true);
    let mut backend = backend(&config);
    let first = sync(&mut backend, BTreeMap::new(), vec![]);
    let push = vec![
        PushOp::SetFlags {
            path: "INBOX".into(),
            uid: 1,
            flags: RemoteFlags {
                seen: true,
                flagged: true,
            },
        },
        PushOp::Move {
            path: "INBOX".into(),
            uid: 2,
            target: "Archive".into(),
        },
        PushOp::Delete {
            path: "INBOX".into(),
            uid: 4,
        },
    ];
    let events = sync(&mut backend, cursors(&first), push);
    let pushed = events
        .iter()
        .filter(|event| matches!(event, SyncEvent::Pushed(_)))
        .count();
    assert_eq!(pushed, 3, "{events:?}");

    server.with_store(|store| {
        let inbox = store.folder("INBOX").unwrap();
        assert_eq!(
            inbox.messages.keys().copied().collect::<Vec<_>>(),
            vec![1, 3]
        );
        assert!(inbox.messages[&1].has_flag("\\Seen"));
        assert!(inbox.messages[&1].has_flag("\\Flagged"));
        assert_eq!(
            store.subjects("Archive"),
            vec!["Old notes", "Planning meeting"]
        );
    });
    // The moved message shows up in Archive in the same cycle.
    assert_eq!(subjects(&events, "Archive"), vec!["Planning meeting"]);
}

#[test]
fn idle_wakes_up_when_mail_arrives() {
    let (server, config) = start(false);
    let store = server.store();
    let delivery = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        mailbox_devserver::lock(&store).deliver(ME, &[ME.into()], b"Subject: ping\r\n\r\nx\r\n");
    });
    let mut backend = backend(&config);
    backend.connect().unwrap();
    let changed = backend
        .wait_for_changes("INBOX", Duration::from_secs(10))
        .unwrap();
    assert!(changed);
    delivery.join().unwrap();
}
