//! A tiny local mail server for testing Mailbox end to end: IMAP4rev1 (+ IDLE, MOVE, UIDPLUS)
//! and SMTP on 127.0.0.1, plain TCP, with in-memory mailboxes (INBOX, Sent, Drafts, Trash,
//! Archive, Junk). Any login is accepted; every account shares the same mailboxes.
//!
//! SMTP stores every accepted message ([`Mailstore::deliveries`]) and delivers a copy to INBOX
//! for each recipient of the local domain ([`Config::domain`]).
//!
//! ```no_run
//! let server = mailbox_devserver::DevServer::start(mailbox_devserver::Config::default())?;
//! println!("IMAP on {}, SMTP on {}", server.imap_port(), server.smtp_port());
//! server.with_store(|store| store.append("INBOX", b"Subject: hi\r\n\r\nhello\r\n", &[]));
//! # Ok::<(), std::io::Error>(())
//! ```

mod imap;
mod seed;
mod smtp;

use std::collections::BTreeMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;

use chrono::{DateTime, Utc};

pub use seed::seed_messages;

/// Folders every server starts with, and their special-use attribute.
pub const FOLDERS: [(&str, &str); 6] = [
    ("INBOX", ""),
    ("Sent", "\\Sent"),
    ("Drafts", "\\Drafts"),
    ("Trash", "\\Trash"),
    ("Archive", "\\Archive"),
    ("Junk", "\\Junk"),
];

#[derive(Debug, Clone)]
pub struct Config {
    /// 0 = any free port.
    pub imap_port: u16,
    pub smtp_port: u16,
    /// Recipients at this domain get the message in INBOX.
    pub domain: String,
    /// Fill INBOX with a few sample messages.
    pub seed: bool,
    /// Print every command and reply to stderr.
    pub verbose: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            imap_port: 0,
            smtp_port: 0,
            domain: "mailbox.test".into(),
            seed: false,
            verbose: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMessage {
    /// IMAP system flags (`\Seen`, `\Flagged`, `\Deleted`...) and keywords.
    pub flags: Vec<String>,
    pub raw: Vec<u8>,
    pub internal_date: DateTime<Utc>,
}

impl StoredMessage {
    pub fn has_flag(&self, flag: &str) -> bool {
        self.flags.iter().any(|f| f.eq_ignore_ascii_case(flag))
    }

    /// The `Subject:` header (unfolded, not decoded), for logs and tests.
    pub fn subject(&self) -> String {
        header_value(&self.raw, "subject").unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mailbox {
    pub special_use: String,
    pub uid_validity: u32,
    pub uid_next: u32,
    /// uid → message (ascending uid = sequence order).
    pub messages: BTreeMap<u32, StoredMessage>,
    /// Bumped on every change (IDLE watches it).
    pub generation: u64,
}

impl Mailbox {
    fn new(special_use: &str) -> Self {
        Self {
            special_use: special_use.to_owned(),
            uid_validity: 1,
            uid_next: 1,
            messages: BTreeMap::new(),
            generation: 0,
        }
    }

    /// 1-based sequence number of `uid`.
    pub fn seq_of(&self, uid: u32) -> Option<usize> {
        self.messages.keys().position(|u| *u == uid).map(|i| i + 1)
    }
}

/// One message accepted by SMTP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    pub from: String,
    /// Envelope recipients (To, Cc and Bcc).
    pub recipients: Vec<String>,
    pub data: Vec<u8>,
    pub at: DateTime<Utc>,
}

impl Delivery {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.data).into_owned()
    }
}

/// Everything the server holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mailstore {
    pub domain: String,
    pub folders: BTreeMap<String, Mailbox>,
    pub deliveries: Vec<Delivery>,
}

impl Mailstore {
    pub fn new(domain: &str) -> Self {
        Self {
            domain: domain.to_ascii_lowercase(),
            folders: FOLDERS
                .iter()
                .map(|(name, special_use)| ((*name).to_owned(), Mailbox::new(special_use)))
                .collect(),
            deliveries: Vec::new(),
        }
    }

    /// Case-insensitive for INBOX, exact otherwise (like real servers).
    pub fn folder_name(&self, name: &str) -> Option<String> {
        if name.eq_ignore_ascii_case("INBOX") {
            return Some("INBOX".to_owned());
        }
        self.folders.contains_key(name).then(|| name.to_owned())
    }

    pub fn folder(&self, name: &str) -> Option<&Mailbox> {
        self.folders.get(&self.folder_name(name)?)
    }

    pub fn folder_mut(&mut self, name: &str) -> Option<&mut Mailbox> {
        let name = self.folder_name(name)?;
        self.folders.get_mut(&name)
    }

    /// Stores a message; returns its UID (`None`: no such folder).
    pub fn append(&mut self, folder: &str, raw: &[u8], flags: &[&str]) -> Option<u32> {
        let mailbox = self.folder_mut(folder)?;
        let uid = mailbox.uid_next;
        mailbox.uid_next += 1;
        mailbox.generation += 1;
        mailbox.messages.insert(
            uid,
            StoredMessage {
                flags: flags.iter().map(|f| (*f).to_owned()).collect(),
                raw: raw.to_vec(),
                internal_date: Utc::now(),
            },
        );
        Some(uid)
    }

    /// Messages of a folder in UID order (empty if missing).
    pub fn messages(&self, folder: &str) -> Vec<&StoredMessage> {
        self.folder(folder)
            .map(|mailbox| mailbox.messages.values().collect())
            .unwrap_or_default()
    }

    /// Subjects of a folder in UID order.
    pub fn subjects(&self, folder: &str) -> Vec<String> {
        self.messages(folder)
            .iter()
            .map(|message| message.subject())
            .collect()
    }

    /// Is `address` a mailbox of this server?
    pub fn is_local(&self, address: &str) -> bool {
        address
            .rsplit_once('@')
            .is_some_and(|(_, domain)| domain.eq_ignore_ascii_case(&self.domain))
    }

    /// Records an SMTP delivery and files a copy in INBOX for local recipients.
    pub fn deliver(&mut self, from: &str, recipients: &[String], data: &[u8]) {
        if recipients.iter().any(|rcpt| self.is_local(rcpt)) {
            self.append("INBOX", data, &[]);
        }
        self.deliveries.push(Delivery {
            from: from.to_owned(),
            recipients: recipients.to_vec(),
            data: data.to_vec(),
            at: Utc::now(),
        });
    }
}

pub type SharedStore = Arc<Mutex<Mailstore>>;

/// Locks the store, recovering from a poisoned lock (a panicking connection thread must not
/// take the whole server down).
pub fn lock(store: &SharedStore) -> MutexGuard<'_, Mailstore> {
    store
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A running server. Dropping it (or [`DevServer::stop`]) closes the listeners; open
/// connections finish on their own.
pub struct DevServer {
    store: SharedStore,
    imap: SocketAddr,
    smtp: SocketAddr,
    stopped: Arc<AtomicBool>,
}

impl DevServer {
    pub fn start(config: Config) -> io::Result<Self> {
        let mut store = Mailstore::new(&config.domain);
        if config.seed {
            seed_messages(&mut store);
        }
        let store = Arc::new(Mutex::new(store));
        let stopped = Arc::new(AtomicBool::new(false));
        let imap = listen(config.imap_port, &stopped, {
            let (store, verbose) = (store.clone(), config.verbose);
            move |stream| imap::serve(stream, &store, verbose)
        })?;
        let smtp = listen(config.smtp_port, &stopped, {
            let (store, verbose) = (store.clone(), config.verbose);
            move |stream| smtp::serve(stream, &store, verbose)
        })?;
        Ok(Self {
            store,
            imap,
            smtp,
            stopped,
        })
    }

    pub fn imap_port(&self) -> u16 {
        self.imap.port()
    }

    pub fn smtp_port(&self) -> u16 {
        self.smtp.port()
    }

    pub fn store(&self) -> SharedStore {
        self.store.clone()
    }

    /// Runs `f` with the store locked.
    pub fn with_store<T>(&self, f: impl FnOnce(&mut Mailstore) -> T) -> T {
        f(&mut lock(&self.store))
    }

    /// Stops accepting connections (new connections are refused).
    pub fn stop(&self) {
        if !self.stopped.swap(true, Ordering::SeqCst) {
            // Wake the accept loops so they see the flag and drop their listeners.
            for address in [self.imap, self.smtp] {
                let _ = TcpStream::connect(address);
            }
        }
    }
}

impl Drop for DevServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn listen(
    port: u16,
    stopped: &Arc<AtomicBool>,
    serve: impl Fn(TcpStream) -> io::Result<()> + Send + Sync + Clone + 'static,
) -> io::Result<SocketAddr> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
    let address = listener.local_addr()?;
    let stopped = stopped.clone();
    thread::Builder::new()
        .name(format!("devserver-{}", address.port()))
        .spawn(move || {
            for stream in listener.incoming() {
                if stopped.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let serve = serve.clone();
                // A failed spawn only loses this connection.
                let _ = thread::Builder::new().spawn(move || {
                    // Connection errors (client gone, timeout) just end the session.
                    let _ = serve(stream);
                });
            }
        })?;
    Ok(address)
}

/// The header section of a message, blank line included.
pub fn header_block(raw: &[u8]) -> &[u8] {
    for separator in [&b"\r\n\r\n"[..], &b"\n\n"[..]] {
        if let Some(pos) = raw.windows(separator.len()).position(|w| w == separator) {
            return &raw[..pos + separator.len()];
        }
    }
    raw
}

/// First header `name` (case-insensitive), unfolded.
pub fn header_value(raw: &[u8], name: &str) -> Option<String> {
    let headers = String::from_utf8_lossy(header_block(raw)).replace("\r\n", "\n");
    let unfolded = headers.replace("\n ", " ").replace("\n\t", " ");
    unfolded.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_delivery_goes_to_inbox_and_is_recorded() {
        let mut store = Mailstore::new("mailbox.test");
        store.deliver(
            "me@mailbox.test",
            &["Ada@Mailbox.Test".into(), "bob@elsewhere.org".into()],
            b"Subject: Hello\r\n\r\nbody\r\n",
        );
        store.deliver(
            "me@mailbox.test",
            &["bob@elsewhere.org".into()],
            b"Subject: Away\r\n\r\nbody\r\n",
        );
        assert_eq!(store.subjects("inbox"), vec!["Hello"]);
        assert_eq!(store.deliveries.len(), 2);
    }

    #[test]
    fn headers_are_unfolded() {
        let raw = b"Subject: a\r\n long one\r\nFrom: x\r\n\r\nSubject: body\r\n";
        assert_eq!(header_value(raw, "SUBJECT").as_deref(), Some("a long one"));
        assert_eq!(header_value(raw, "to"), None);
    }
}
