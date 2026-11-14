//! End-to-end tests of the real IMAP/SMTP backend against minimal in-process fake servers
//! on 127.0.0.1 (plain TCP, no real account involved).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use mailbox_core::{Address, FolderRole, Importance, Message, MessageFlags};
use mailbox_sync::backend::header_block;
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

const USER: &str = "me@example.com";
const PASSWORD: &str = "secret";

// ---------- fake IMAP ----------

#[derive(Default)]
struct FakeFolder {
    special_use: &'static str,
    uid_validity: u32,
    uid_next: u32,
    /// uid → (flags like `\Seen`, raw message)
    messages: BTreeMap<u32, (Vec<String>, Vec<u8>)>,
}

#[derive(Default)]
struct ImapServer {
    folders: BTreeMap<String, FakeFolder>,
}

impl ImapServer {
    fn with_folders() -> Self {
        let mut server = Self::default();
        for (name, special_use) in [
            ("INBOX", ""),
            ("Sent", " \\Sent"),
            ("Archive", " \\Archive"),
        ] {
            server.folders.insert(
                name.to_owned(),
                FakeFolder {
                    special_use,
                    uid_validity: 7,
                    uid_next: 1,
                    messages: BTreeMap::new(),
                },
            );
        }
        server
    }

    fn deliver(&mut self, folder: &str, raw: &[u8], flags: &[&str]) -> u32 {
        let folder = self.folders.get_mut(folder).expect("folder");
        let uid = folder.uid_next;
        folder.uid_next += 1;
        folder.messages.insert(
            uid,
            (
                flags.iter().map(|f| (*f).to_owned()).collect(),
                raw.to_vec(),
            ),
        );
        uid
    }
}

type Imap = Arc<Mutex<ImapServer>>;

fn spawn_imap(server: Imap) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let server = server.clone();
            std::thread::spawn(move || {
                let _ = serve_imap(stream, &server);
            });
        }
    });
    port
}

/// Splits IMAP arguments: atoms, "quoted strings" and (parenthesised lists) kept whole.
fn tokens(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            ' ' => {
                chars.next();
            }
            '"' => {
                chars.next();
                let mut token = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => token.extend(chars.next()),
                        '"' => break,
                        c => token.push(c),
                    }
                }
                out.push(token);
            }
            '(' => {
                let mut token = String::new();
                for c in chars.by_ref() {
                    token.push(c);
                    if c == ')' {
                        break;
                    }
                }
                out.push(token);
            }
            _ => {
                let mut token = String::new();
                while let Some(&c) = chars.peek() {
                    if c == ' ' {
                        break;
                    }
                    token.push(c);
                    chars.next();
                }
                out.push(token);
            }
        }
    }
    out
}

fn uid_set(set: &str, folder: &FakeFolder) -> Vec<u32> {
    let max = folder.messages.keys().next_back().copied().unwrap_or(0);
    let mut uids = Vec::new();
    for part in set.split(',') {
        let (from, to) = part.split_once(':').unwrap_or((part, part));
        let parse = |s: &str| {
            if s == "*" {
                max
            } else {
                s.parse().unwrap_or(0)
            }
        };
        let (from, to) = (parse(from), parse(to));
        uids.extend(
            folder
                .messages
                .keys()
                .copied()
                .filter(|uid| *uid >= from.min(to) && *uid <= from.max(to)),
        );
    }
    uids
}

fn serve_imap(stream: TcpStream, server: &Imap) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(20)))?;
    let mut out = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    out.write_all(b"* OK fake IMAP ready\r\n")?;
    let mut selected = String::new();
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let parts = tokens(line.trim_end());
        let (Some(tag), Some(command)) = (parts.first(), parts.get(1)) else {
            continue;
        };
        let mut command = command.to_ascii_uppercase();
        let mut args = &parts[2..];
        if command == "UID" {
            command = format!("UID {}", args[0].to_ascii_uppercase());
            args = &args[1..];
        }
        let ok = format!("{tag} OK done\r\n");
        let mut server = server.lock().unwrap();
        match command.as_str() {
            "CAPABILITY" => {
                out.write_all(b"* CAPABILITY IMAP4rev1 IDLE MOVE UIDPLUS\r\n")?;
                out.write_all(ok.as_bytes())?;
            }
            "LOGIN" => {
                if args.first().map(String::as_str) == Some(USER)
                    && args.get(1).map(String::as_str) == Some(PASSWORD)
                {
                    out.write_all(ok.as_bytes())?;
                } else {
                    out.write_all(format!("{tag} NO [AUTHENTICATIONFAILED] bad\r\n").as_bytes())?;
                }
            }
            "LIST" => {
                for (name, folder) in &server.folders {
                    let attrs = folder.special_use;
                    out.write_all(
                        format!("* LIST (\\HasNoChildren{attrs}) \"/\" \"{name}\"\r\n").as_bytes(),
                    )?;
                }
                out.write_all(ok.as_bytes())?;
            }
            "SELECT" | "EXAMINE" => {
                selected = args[0].clone();
                let Some(folder) = server.folders.get(&selected) else {
                    out.write_all(format!("{tag} NO no such folder\r\n").as_bytes())?;
                    continue;
                };
                out.write_all(
                    format!(
                        "* {} EXISTS\r\n* 0 RECENT\r\n* FLAGS (\\Seen \\Flagged \\Deleted)\r\n\
                         * OK [UIDVALIDITY {}] ok\r\n* OK [UIDNEXT {}] ok\r\n\
                         {tag} OK [READ-WRITE] selected\r\n",
                        folder.messages.len(),
                        folder.uid_validity,
                        folder.uid_next
                    )
                    .as_bytes(),
                )?;
            }
            "UID FETCH" => {
                let folder = &server.folders[&selected];
                let items = args[1..].join(" ");
                for uid in uid_set(&args[0], folder) {
                    let (flags, raw) = &folder.messages[&uid];
                    let seq = folder.messages.keys().position(|u| *u == uid).unwrap_or(0) + 1;
                    let mut response = format!(
                        "* {seq} FETCH (UID {uid} FLAGS ({}) INTERNALDATE \"14-Nov-2026 10:00:00 +0000\"",
                        flags.join(" ")
                    )
                    .into_bytes();
                    let section = if items.contains("BODY.PEEK[HEADER]") {
                        Some(("HEADER", header_block(raw)))
                    } else if items.contains("BODY.PEEK[]") {
                        Some(("", raw.as_slice()))
                    } else {
                        None
                    };
                    if let Some((name, bytes)) = section {
                        response.extend(format!(" BODY[{name}] {{{}}}\r\n", bytes.len()).bytes());
                        response.extend_from_slice(bytes);
                    }
                    response.extend_from_slice(b")\r\n");
                    out.write_all(&response)?;
                }
                out.write_all(ok.as_bytes())?;
            }
            "UID STORE" => {
                let mode = args[1].to_ascii_uppercase();
                let names: Vec<String> = args[2]
                    .trim_matches(['(', ')'])
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect();
                let folder = server.folders.get_mut(&selected).unwrap();
                for uid in uid_set(&args[0], folder) {
                    let flags = &mut folder.messages.get_mut(&uid).unwrap().0;
                    for name in &names {
                        flags.retain(|flag| flag != name);
                        if mode.starts_with('+') {
                            flags.push(name.clone());
                        }
                    }
                }
                out.write_all(ok.as_bytes())?;
            }
            "UID MOVE" => {
                let uids = uid_set(&args[0], &server.folders[&selected]);
                for uid in uids {
                    let (flags, raw) = server
                        .folders
                        .get_mut(&selected)
                        .unwrap()
                        .messages
                        .remove(&uid)
                        .unwrap();
                    let flags: Vec<&str> = flags.iter().map(String::as_str).collect();
                    server.deliver(&args[1], &raw, &flags);
                }
                out.write_all(ok.as_bytes())?;
            }
            "UID EXPUNGE" | "EXPUNGE" => {
                let folder = server.folders.get_mut(&selected).unwrap();
                folder
                    .messages
                    .retain(|_, (flags, _)| !flags.iter().any(|f| f == "\\Deleted"));
                out.write_all(ok.as_bytes())?;
            }
            "APPEND" => {
                let folder = args[0].clone();
                let flags: Vec<String> = args[1]
                    .trim_matches(['(', ')'])
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect();
                let size: usize = args
                    .last()
                    .unwrap()
                    .trim_matches(['{', '}', '+'])
                    .parse()
                    .unwrap();
                out.write_all(b"+ go ahead\r\n")?;
                let mut raw = vec![0; size];
                reader.read_exact(&mut raw)?;
                let mut rest = String::new();
                reader.read_line(&mut rest)?;
                let flags: Vec<&str> = flags.iter().map(String::as_str).collect();
                let uid = server.deliver(&folder, &raw, &flags);
                out.write_all(format!("{tag} OK [APPENDUID 7 {uid}] done\r\n").as_bytes())?;
            }
            "LOGOUT" => {
                out.write_all(format!("* BYE\r\n{ok}").as_bytes())?;
                return Ok(());
            }
            "NOOP" => out.write_all(ok.as_bytes())?,
            _ => out.write_all(format!("{tag} BAD unsupported\r\n").as_bytes())?,
        }
    }
}

// ---------- fake SMTP ----------

#[derive(Debug, Default, Clone)]
struct Delivery {
    from: String,
    recipients: Vec<String>,
    data: String,
}

type Smtp = Arc<Mutex<Vec<Delivery>>>;

fn spawn_smtp(deliveries: Smtp) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let deliveries = deliveries.clone();
            std::thread::spawn(move || {
                let _ = serve_smtp(stream, &deliveries);
            });
        }
    });
    port
}

fn serve_smtp(stream: TcpStream, deliveries: &Smtp) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(20)))?;
    let mut out = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    out.write_all(b"220 fake ESMTP\r\n")?;
    let mut current = Delivery::default();
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let upper = line.to_ascii_uppercase();
        let address = || {
            line.split_once('<')
                .and_then(|(_, rest)| rest.split_once('>'))
                .map(|(address, _)| address.to_owned())
                .unwrap_or_default()
        };
        if upper.starts_with("EHLO") || upper.starts_with("HELO") {
            out.write_all(b"250-fake\r\n250-AUTH PLAIN LOGIN\r\n250 8BITMIME\r\n")?;
        } else if upper.starts_with("AUTH") {
            out.write_all(b"235 2.7.0 authenticated\r\n")?;
        } else if upper.starts_with("MAIL FROM") {
            current = Delivery {
                from: address(),
                ..Delivery::default()
            };
            out.write_all(b"250 ok\r\n")?;
        } else if upper.starts_with("RCPT TO") {
            current.recipients.push(address());
            out.write_all(b"250 ok\r\n")?;
        } else if upper.starts_with("DATA") {
            out.write_all(b"354 end with .\r\n")?;
            let mut data = String::new();
            loop {
                let mut data_line = String::new();
                if reader.read_line(&mut data_line)? == 0 || data_line == ".\r\n" {
                    break;
                }
                data.push_str(data_line.strip_prefix('.').unwrap_or(&data_line));
            }
            current.data = data;
            deliveries
                .lock()
                .unwrap()
                .push(std::mem::take(&mut current));
            out.write_all(b"250 queued\r\n")?;
        } else if upper.starts_with("QUIT") {
            out.write_all(b"221 bye\r\n")?;
            return Ok(());
        } else {
            out.write_all(b"250 ok\r\n")?;
        }
    }
}

// ---------- helpers ----------

struct Servers {
    imap: Imap,
    smtp: Smtp,
    config: AccountConfig,
}

fn servers() -> Servers {
    let imap: Imap = Arc::new(Mutex::new(ImapServer::with_folders()));
    let smtp: Smtp = Arc::default();
    let server = |port| ServerConfig {
        host: "127.0.0.1".into(),
        port,
        security: Security::Plain,
    };
    let config = AccountConfig {
        email: USER.into(),
        username: None,
        imap: server(spawn_imap(imap.clone())),
        smtp: server(spawn_smtp(smtp.clone())),
        auth: AuthConfig::Password,
        offline_days: 30,
        poll_minutes: 5,
        idle: false,
        save_sent: None,
    };
    Servers { imap, smtp, config }
}

fn backend(config: &AccountConfig) -> ImapSmtpBackend {
    ImapSmtpBackend::with_password(config.clone(), PASSWORD).unwrap()
}

fn incoming(subject: &str) -> Vec<u8> {
    format!(
        "From: Ada <ada@example.org>\r\nTo: {USER}\r\nSubject: {subject}\r\n\
         Date: Fri, 13 Nov 2026 10:00:00 +0000\r\nMessage-ID: <{subject}@example.org>\r\n\r\n\
         Body of {subject}\r\n"
    )
    .into_bytes()
}

fn sync(backend: &mut dyn MailBackend, input: SyncInput) -> Vec<SyncEvent> {
    let mut events = Vec::new();
    run_cycle(backend, input, Utc::now(), &mut |event| events.push(event)).unwrap();
    events
}

fn input(cursors: BTreeMap<String, FolderCursor>, push: Vec<PushOp>) -> SyncInput {
    SyncInput {
        push,
        cursors,
        offline_days: 30,
        max_initial: 100,
    }
}

/// The INBOX cursor a caller would keep after applying `events`.
fn inbox_cursor(events: &[SyncEvent]) -> FolderCursor {
    let mut cursor = FolderCursor::default();
    for event in events {
        match event {
            SyncEvent::NewMessages { path, messages } if path == "INBOX" => {
                for message in messages {
                    cursor.known.insert(message.uid, message.flags);
                }
            }
            SyncEvent::FolderSynced {
                path,
                uid_validity,
                highest_uid,
            } if path == "INBOX" => {
                cursor.uid_validity = *uid_validity;
                cursor.highest_uid = *highest_uid;
            }
            _ => {}
        }
    }
    cursor
}

fn subjects(events: &[SyncEvent], folder: &str) -> Vec<String> {
    events
        .iter()
        .flat_map(|event| match event {
            SyncEvent::NewMessages { path, messages } if path == folder => messages
                .iter()
                .map(|m| m.parsed.message.subject.clone())
                .collect(),
            _ => vec![],
        })
        .collect()
}

// ---------- tests ----------

#[test]
fn connection_test_lists_folders_and_explains_bad_passwords() {
    let servers = servers();
    let folders = test_connection(&servers.config, Some(PASSWORD)).unwrap();
    let sent = folders.iter().find(|f| f.path == "Sent").unwrap();
    assert_eq!(sent.role, FolderRole::Sent);
    assert!(folders.iter().any(|f| f.role == FolderRole::Inbox));

    let error = test_connection(&servers.config, Some("wrong")).unwrap_err();
    assert!(error.starts_with("Incoming mail (IMAP)"), "{error}");
    assert!(error.contains("app password"), "{error}");

    let mut unreachable = servers.config.clone();
    unreachable.imap.port = 1;
    let error = test_connection(&unreachable, Some(PASSWORD)).unwrap_err();
    assert!(error.contains("cannot reach 127.0.0.1:1"), "{error}");
}

#[test]
fn send_produces_the_right_mime_and_lands_in_sent() {
    let servers = servers();
    let message = Message {
        id: 1,
        account_id: 1,
        folder_id: 1,
        thread_id: String::new(),
        from: Address::new("Me", USER),
        to: vec![Address::new("Ada", "ada@example.org")],
        cc: vec![Address::new("", "cc@example.org")],
        subject: "Re: Plans".into(),
        date: Utc::now(),
        preview: String::new(),
        body_text: "See you at 10.".into(),
        body_html: Some("<p>See you at <b>10</b>.</p>".into()),
        flags: MessageFlags::default(),
        importance: Importance::Normal,
        categories: vec![],
        attachments: vec![],
    };
    let headers = OutgoingHeaders {
        message_id: Some("<out-1@example.com>".into()),
        bcc: vec![Address::new("Hidden", "hidden@example.net")],
        in_reply_to: Some("<parent@example.org>".into()),
        references: vec!["<root@example.org>".into(), "parent@example.org".into()],
        read_receipt: true,
    };
    let attachment = AttachmentData {
        name: "notes.txt".into(),
        mime: "text/plain".into(),
        bytes: b"attached notes".to_vec(),
    };
    let raw = build_outgoing(&message, &headers, &[attachment]).unwrap();

    let config = servers.config.clone();
    let factory: BackendFactory =
        Arc::new(move || -> SyncResult<Box<dyn MailBackend>> { Ok(Box::new(backend(&config))) });
    let worker = WorkerHandle::spawn(factory, None).unwrap();
    let recipients = ["ada@example.org", "cc@example.org", "hidden@example.net"]
        .map(str::to_owned)
        .to_vec();
    assert!(worker.request(Request::Send(OutgoingMessage {
        local_id: 1,
        from: USER.into(),
        recipients: recipients.clone(),
        raw: raw.clone(),
        sent_path: servers.config.save_sent_copy().then(|| "Sent".to_owned()),
    })));
    let mut events = Vec::new();
    while let Some(event) = worker.recv_timeout(Duration::from_secs(20)) {
        let done = matches!(event, SyncEvent::Sent { .. } | SyncEvent::SendFailed { .. });
        events.push(event);
        if done {
            break;
        }
    }
    assert_eq!(events, vec![SyncEvent::Sent { local_id: 1 }]);

    // SMTP: envelope has every recipient, the message has no trace of Bcc.
    let deliveries = servers.smtp.lock().unwrap().clone();
    assert_eq!(deliveries.len(), 1);
    let delivery = &deliveries[0];
    assert_eq!(delivery.from, USER);
    assert_eq!(delivery.recipients, recipients);
    let data = &delivery.data;
    for expected in [
        "Message-ID: <out-1@example.com>",
        "In-Reply-To: <parent@example.org>",
        "References: <root@example.org> <parent@example.org>",
        "Disposition-Notification-To: <me@example.com>",
        "multipart/mixed",
        "multipart/alternative",
        "Content-Type: text/plain",
        "Content-Type: text/html",
        "Content-Disposition: attachment; filename=\"notes.txt\"",
    ] {
        assert!(data.contains(expected), "missing {expected:?} in:\n{data}");
    }
    assert!(!data.contains("hidden@example.net"), "Bcc leaked:\n{data}");
    assert!(
        !data.to_ascii_lowercase().contains("\nbcc:"),
        "Bcc header:\n{data}"
    );

    let parsed = parse_message(data.as_bytes(), None).unwrap();
    assert_eq!(parsed.message.subject, "Re: Plans");
    assert!(
        parsed
            .message
            .body_html
            .unwrap_or_default()
            .contains("<b>10</b>")
    );
    assert_eq!(parsed.attachments[0].bytes, b"attached notes");

    // IMAP: a read copy in Sent, byte for byte.
    let imap = servers.imap.lock().unwrap();
    let sent: Vec<_> = imap.folders["Sent"].messages.values().collect();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, vec!["\\Seen".to_owned()]);
    assert_eq!(sent[0].1, raw);
}

#[test]
fn smtp_failure_is_reported_and_nothing_is_filed() {
    let servers = servers();
    let mut config = servers.config.clone();
    config.smtp.port = 1; // nothing listens there
    let mut backend = backend(&config);
    let result = backend.send(
        USER,
        &["ada@example.org".into()],
        b"Subject: x\r\n\r\nx\r\n",
    );
    assert!(result.is_err());
    assert!(
        servers.imap.lock().unwrap().folders["Sent"]
            .messages
            .is_empty()
    );
}

#[test]
fn fetch_pulls_new_messages_incrementally() {
    let servers = servers();
    servers
        .imap
        .lock()
        .unwrap()
        .deliver("INBOX", &incoming("first"), &[]);
    let mut backend = backend(&servers.config);

    let events = sync(&mut backend, input(BTreeMap::new(), vec![]));
    assert!(matches!(&events[0], SyncEvent::Folders(folders) if folders[0].path == "INBOX"));
    assert_eq!(subjects(&events, "INBOX"), vec!["first"]);
    let first = events
        .iter()
        .find_map(|event| match event {
            SyncEvent::NewMessages { messages, .. } => messages.first().cloned(),
            _ => None,
        })
        .unwrap();
    assert!(first.has_body);
    assert_eq!(first.parsed.message.body_text.trim(), "Body of first");
    assert!(!first.parsed.message.flags.read);

    // New mail arrives: only it is fetched on the next cycle.
    servers
        .imap
        .lock()
        .unwrap()
        .deliver("INBOX", &incoming("second"), &["\\Seen"]);
    let cursor = inbox_cursor(&events);
    let events = sync(
        &mut backend,
        input(BTreeMap::from([("INBOX".to_owned(), cursor)]), vec![]),
    );
    assert_eq!(subjects(&events, "INBOX"), vec!["second"]);
}

#[test]
fn local_flags_and_moves_are_pushed_to_the_server() {
    let servers = servers();
    {
        let mut imap = servers.imap.lock().unwrap();
        imap.deliver("INBOX", &incoming("keep"), &[]);
        imap.deliver("INBOX", &incoming("archive-me"), &[]);
    }
    let mut backend = backend(&servers.config);
    let cursor = inbox_cursor(&sync(&mut backend, input(BTreeMap::new(), vec![])));

    let read_and_flagged = RemoteFlags {
        seen: true,
        flagged: true,
    };
    let push = vec![
        PushOp::SetFlags {
            path: "INBOX".into(),
            uid: 1,
            flags: read_and_flagged,
        },
        PushOp::Move {
            path: "INBOX".into(),
            uid: 2,
            target: "Archive".into(),
        },
    ];
    let events = sync(
        &mut backend,
        input(BTreeMap::from([("INBOX".to_owned(), cursor)]), push),
    );
    let pushed = events
        .iter()
        .filter(|e| matches!(e, SyncEvent::Pushed(_)))
        .count();
    assert_eq!(pushed, 2);

    let imap = servers.imap.lock().unwrap();
    let inbox = &imap.folders["INBOX"].messages;
    assert_eq!(inbox.len(), 1);
    let mut flags = inbox[&1].0.clone();
    flags.sort();
    assert_eq!(flags, vec!["\\Flagged".to_owned(), "\\Seen".to_owned()]);
    let archive: Vec<_> = imap.folders["Archive"].messages.values().collect();
    assert_eq!(archive.len(), 1);
    assert!(String::from_utf8_lossy(&archive[0].1).contains("Subject: archive-me"));
    // The moved message shows up in Archive on the server side of the same cycle.
    assert_eq!(subjects(&events, "Archive"), vec!["archive-me"]);
}
