//! IMAP4rev1 subset: what Mailbox (and simple clients) use. One thread per connection.
//!
//! Commands: CAPABILITY, NOOP, LOGOUT, LOGIN, AUTHENTICATE PLAIN, ID, ENABLE, NAMESPACE,
//! LIST, LSUB, STATUS, CREATE, DELETE, RENAME, SUBSCRIBE, UNSUBSCRIBE, SELECT, EXAMINE,
//! CLOSE, UNSELECT, CHECK, EXPUNGE, APPEND, IDLE and FETCH, STORE, COPY, MOVE, SEARCH,
//! EXPUNGE with or without the UID prefix.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::{Mailbox, Mailstore, SharedStore, StoredMessage, header_block, header_value, lock};

const CAPABILITIES: &str = "IMAP4rev1 IDLE MOVE UIDPLUS SPECIAL-USE ID ENABLE AUTH=PLAIN";
/// Idle clients are dropped after this (RFC 3501 asks for at least 30 min).
const SESSION_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// How often IDLE checks the mailbox for changes.
const IDLE_TICK: Duration = Duration::from_millis(200);
/// Marks a literal inside a command line: `\0<index>`.
const LITERAL: char = '\0';

struct Session {
    out: TcpStream,
    reader: BufReader<TcpStream>,
    store: SharedStore,
    verbose: bool,
    selected: Option<Selected>,
}

struct Selected {
    name: String,
    read_only: bool,
    /// What the client was last told (to report changes in NOOP and IDLE).
    exists: usize,
    generation: u64,
}

/// A parsed command line: tag, command (upper case, `UID ` prefix kept), arguments and the
/// bytes of the literals they refer to.
struct Command {
    tag: String,
    name: String,
    args: Vec<String>,
    literals: Vec<Vec<u8>>,
}

impl Command {
    fn arg(&self, index: usize) -> &str {
        self.args.get(index).map_or("", String::as_str)
    }

    /// An argument as bytes (literal content or the string itself).
    fn bytes(&self, index: usize) -> Vec<u8> {
        let arg = self.arg(index);
        literal_index(arg)
            .and_then(|i| self.literals.get(i).cloned())
            .unwrap_or_else(|| arg.as_bytes().to_vec())
    }

    fn text(&self, index: usize) -> String {
        String::from_utf8_lossy(&self.bytes(index)).into_owned()
    }
}

pub(crate) fn serve(stream: TcpStream, store: &SharedStore, verbose: bool) -> io::Result<()> {
    stream.set_read_timeout(Some(SESSION_TIMEOUT))?;
    let mut session = Session {
        out: stream.try_clone()?,
        reader: BufReader::new(stream),
        store: store.clone(),
        verbose,
        selected: None,
    };
    session.send("* OK [CAPABILITY IMAP4rev1] Mailbox devserver ready")?;
    while let Some(command) = session.read_command()? {
        if !session.handle(&command)? {
            break;
        }
    }
    Ok(())
}

impl Session {
    fn send(&mut self, line: &str) -> io::Result<()> {
        if self.verbose {
            eprintln!("imap S: {line}");
        }
        self.out.write_all(line.as_bytes())?;
        self.out.write_all(b"\r\n")
    }

    fn send_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.verbose {
            let text = String::from_utf8_lossy(&bytes[..bytes.len().min(200)]).into_owned();
            eprintln!("imap S: {}", text.trim_end());
        }
        self.out.write_all(bytes)
    }

    /// Reads one command, following `{n}` / `{n+}` literals.
    fn read_command(&mut self) -> io::Result<Option<Command>> {
        let mut line = String::new();
        let mut literals = Vec::new();
        loop {
            let mut part = Vec::new();
            if self.reader.read_until(b'\n', &mut part)? == 0 {
                return Ok(None);
            }
            let text = String::from_utf8_lossy(&part)
                .trim_end_matches(['\r', '\n'])
                .to_owned();
            if self.verbose {
                eprintln!("imap C: {text}");
            }
            let Some((before, size, non_sync)) = literal_suffix(&text) else {
                line.push_str(&text);
                break;
            };
            if !non_sync {
                self.send("+ Ready for literal data")?;
            }
            let mut bytes = vec![0; size];
            self.reader.read_exact(&mut bytes)?;
            line.push_str(before);
            line.push(LITERAL);
            line.push_str(&literals.len().to_string());
            literals.push(bytes);
        }
        let mut tokens = tokens(&line).into_iter();
        let tag = tokens.next().unwrap_or_default();
        let mut name = tokens.next().unwrap_or_default().to_ascii_uppercase();
        let mut args: Vec<String> = tokens.collect();
        if name == "UID" && !args.is_empty() {
            name = format!("UID {}", args.remove(0).to_ascii_uppercase());
        }
        Ok(Some(Command {
            tag,
            name,
            args,
            literals,
        }))
    }

    /// Runs one command. `false` ends the session.
    fn handle(&mut self, cmd: &Command) -> io::Result<bool> {
        let tag = cmd.tag.as_str();
        let ok = |text: &str| format!("{tag} OK {text}");
        let result: Result<String, String> = match cmd.name.as_str() {
            "CAPABILITY" => {
                self.send(&format!("* CAPABILITY {CAPABILITIES}"))?;
                Ok(ok("CAPABILITY completed"))
            }
            "NOOP" | "CHECK" => {
                self.report_changes()?;
                Ok(ok("NOOP completed"))
            }
            "LOGOUT" => {
                self.send("* BYE logging out")?;
                self.send(&ok("LOGOUT completed"))?;
                return Ok(false);
            }
            // Any user name and password are accepted.
            "LOGIN" => Ok(ok("LOGIN completed")),
            "AUTHENTICATE" => {
                if cmd.args.len() < 2 {
                    self.send("+ ")?;
                    let mut response = Vec::new();
                    self.reader.read_until(b'\n', &mut response)?;
                }
                Ok(ok("AUTHENTICATE completed"))
            }
            "ID" => {
                self.send("* ID (\"name\" \"mailbox-devserver\")")?;
                Ok(ok("ID completed"))
            }
            "ENABLE" => Ok(ok("ENABLE completed")),
            "NAMESPACE" => {
                self.send("* NAMESPACE ((\"\" \"/\")) NIL NIL")?;
                Ok(ok("NAMESPACE completed"))
            }
            "LIST" | "LSUB" => {
                self.list(cmd)?;
                Ok(ok("LIST completed"))
            }
            "STATUS" => self.status(cmd),
            "CREATE" => self.create(cmd),
            "DELETE" => self.delete(cmd),
            "RENAME" => self.rename(cmd),
            "SUBSCRIBE" | "UNSUBSCRIBE" => Ok(ok("done")),
            "SELECT" | "EXAMINE" => self.select(cmd),
            "APPEND" => self.append(cmd),
            "CLOSE" | "UNSELECT" => {
                if cmd.name == "CLOSE" {
                    // CLOSE expunges silently.
                    if let Some(name) = self.writable() {
                        expunge(&mut lock(&self.store), &name, None);
                    }
                }
                self.selected = None;
                Ok(ok("closed"))
            }
            "IDLE" => self.idle(tag),
            name => match self.selected.as_ref().map(|s| s.name.clone()) {
                None => Err(format!(
                    "{tag} BAD {name}: unknown command or no mailbox selected"
                )),
                Some(folder) => self.selected_command(cmd, &folder),
            },
        };
        match result {
            Ok(line) | Err(line) => self.send(&line)?,
        }
        Ok(true)
    }

    /// Commands that need a selected mailbox. `Ok(tagged reply)` / `Err(tagged error)`.
    fn selected_command(&mut self, cmd: &Command, folder: &str) -> Result<String, String> {
        let tag = &cmd.tag;
        let (uid, name) = match cmd.name.strip_prefix("UID ") {
            Some(name) => (true, name),
            None => (false, cmd.name.as_str()),
        };
        let io = |err: io::Error| format!("{tag} NO {err}");
        match name {
            "FETCH" => self.fetch(cmd, folder, uid).map_err(io)?,
            "STORE" => self.store_flags(cmd, folder, uid).map_err(io)?,
            "COPY" | "MOVE" => return self.copy(cmd, folder, uid, name == "MOVE"),
            "SEARCH" => self.search(cmd, folder, uid).map_err(io)?,
            "EXPUNGE" => {
                let only = uid.then(|| {
                    let store = lock(&self.store);
                    store
                        .folder(folder)
                        .map(|mailbox| select_set(mailbox, cmd.arg(0), true))
                        .unwrap_or_default()
                });
                let gone = expunge(&mut lock(&self.store), folder, only.as_deref());
                for seq in gone {
                    self.send(&format!("* {seq} EXPUNGE")).map_err(io)?;
                }
                self.sync_selected();
            }
            _ => return Err(format!("{tag} BAD unknown command {}", cmd.name)),
        }
        Ok(format!("{tag} OK {} completed", cmd.name))
    }

    fn writable(&self) -> Option<String> {
        self.selected
            .as_ref()
            .filter(|selected| !selected.read_only)
            .map(|selected| selected.name.clone())
    }

    /// Remembers the selected mailbox's current size and generation.
    fn sync_selected(&mut self) {
        let Some(selected) = self.selected.as_mut() else {
            return;
        };
        if let Some(mailbox) = lock(&self.store).folder(&selected.name) {
            selected.exists = mailbox.messages.len();
            selected.generation = mailbox.generation;
        }
    }

    /// Untagged EXISTS when the selected mailbox changed since the client last heard of it.
    /// Returns whether something was reported.
    fn report_changes(&mut self) -> io::Result<bool> {
        let Some(selected) = &self.selected else {
            return Ok(false);
        };
        let current = lock(&self.store)
            .folder(&selected.name)
            .map(|mailbox| (mailbox.messages.len(), mailbox.generation));
        let Some((exists, generation)) = current else {
            return Ok(false);
        };
        if generation == selected.generation {
            return Ok(false);
        }
        self.send(&format!("* {exists} EXISTS"))?;
        self.sync_selected();
        Ok(true)
    }

    fn list(&mut self, cmd: &Command) -> io::Result<()> {
        let pattern = cmd.text(1);
        if pattern.is_empty() {
            return self.send("* LIST (\\Noselect) \"/\" \"\"");
        }
        let lines: Vec<String> = lock(&self.store)
            .folders
            .iter()
            .filter(|(name, _)| glob(&pattern, name))
            .map(|(name, mailbox)| {
                let attrs = format!("\\HasNoChildren {}", mailbox.special_use);
                format!("* {} ({}) \"/\" {}", cmd.name, attrs.trim(), quote(name))
            })
            .collect();
        for line in lines {
            self.send(&line)?;
        }
        Ok(())
    }

    fn status(&mut self, cmd: &Command) -> Result<String, String> {
        let tag = &cmd.tag;
        let name = cmd.text(0);
        let line = {
            let store = lock(&self.store);
            let mailbox = store
                .folder(&name)
                .ok_or_else(|| format!("{tag} NO no such mailbox"))?;
            let unseen = mailbox
                .messages
                .values()
                .filter(|m| !m.has_flag("\\Seen"))
                .count();
            format!(
                "* STATUS {} (MESSAGES {} RECENT 0 UIDNEXT {} UIDVALIDITY {} UNSEEN {unseen})",
                quote(&name),
                mailbox.messages.len(),
                mailbox.uid_next,
                mailbox.uid_validity
            )
        };
        self.send(&line).map_err(|err| format!("{tag} NO {err}"))?;
        Ok(format!("{tag} OK STATUS completed"))
    }

    fn create(&mut self, cmd: &Command) -> Result<String, String> {
        let name = cmd.text(0);
        let mut store = lock(&self.store);
        if name.is_empty() || store.folder(&name).is_some() {
            return Err(format!("{} NO [ALREADYEXISTS] mailbox exists", cmd.tag));
        }
        store.folders.insert(name, Mailbox::new(""));
        Ok(format!("{} OK CREATE completed", cmd.tag))
    }

    fn delete(&mut self, cmd: &Command) -> Result<String, String> {
        let name = cmd.text(0);
        let mut store = lock(&self.store);
        match store.folder_name(&name) {
            Some(name) if name != "INBOX" => {
                store.folders.remove(&name);
                Ok(format!("{} OK DELETE completed", cmd.tag))
            }
            _ => Err(format!("{} NO cannot delete that mailbox", cmd.tag)),
        }
    }

    fn rename(&mut self, cmd: &Command) -> Result<String, String> {
        let (from, to) = (cmd.text(0), cmd.text(1));
        let mut store = lock(&self.store);
        let Some(from) = store.folder_name(&from).filter(|name| name != "INBOX") else {
            return Err(format!("{} NO no such mailbox", cmd.tag));
        };
        if store.folder(&to).is_some() {
            return Err(format!("{} NO [ALREADYEXISTS] target exists", cmd.tag));
        }
        if let Some(mut mailbox) = store.folders.remove(&from) {
            mailbox.uid_validity += 1;
            store.folders.insert(to, mailbox);
        }
        Ok(format!("{} OK RENAME completed", cmd.tag))
    }

    fn select(&mut self, cmd: &Command) -> Result<String, String> {
        let tag = &cmd.tag;
        let read_only = cmd.name == "EXAMINE";
        self.selected = None;
        let (name, lines) = {
            let store = lock(&self.store);
            let name = cmd.text(0);
            let mailbox = store
                .folder(&name)
                .ok_or_else(|| format!("{tag} NO [NONEXISTENT] no such mailbox"))?;
            let lines = vec![
                "* FLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft)".to_owned(),
                "* OK [PERMANENTFLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft \\*)] ok"
                    .to_owned(),
                format!("* {} EXISTS", mailbox.messages.len()),
                "* 0 RECENT".to_owned(),
                format!("* OK [UIDVALIDITY {}] ok", mailbox.uid_validity),
                format!("* OK [UIDNEXT {}] ok", mailbox.uid_next),
            ];
            (store.folder_name(&name).unwrap_or(name), lines)
        };
        for line in lines {
            self.send(&line).map_err(|err| format!("{tag} NO {err}"))?;
        }
        self.selected = Some(Selected {
            name,
            read_only,
            exists: 0,
            generation: 0,
        });
        self.sync_selected();
        let mode = if read_only { "READ-ONLY" } else { "READ-WRITE" };
        Ok(format!("{tag} OK [{mode}] {} completed", cmd.name))
    }

    /// `APPEND mailbox [(flags)] ["date"] literal`
    fn append(&mut self, cmd: &Command) -> Result<String, String> {
        let tag = &cmd.tag;
        let literal = (1..cmd.args.len())
            .find(|i| literal_index(cmd.arg(*i)).is_some())
            .ok_or_else(|| format!("{tag} BAD APPEND needs a message literal"))?;
        let flags: Vec<String> = cmd.args[1..literal]
            .iter()
            .filter(|arg| arg.starts_with('('))
            .flat_map(|arg| flag_list(arg))
            .collect();
        let date = cmd.args[1..literal]
            .iter()
            .find(|arg| !arg.starts_with('('))
            .and_then(|date| DateTime::parse_from_str(date, "%d-%b-%Y %H:%M:%S %z").ok())
            .map(|date| date.with_timezone(&Utc));
        let raw = cmd.bytes(literal);
        let mut store = lock(&self.store);
        let name = cmd.text(0);
        let flags: Vec<&str> = flags.iter().map(String::as_str).collect();
        let uid = store
            .append(&name, &raw, &flags)
            .ok_or_else(|| format!("{tag} NO [TRYCREATE] no such mailbox"))?;
        let mailbox = store
            .folder_mut(&name)
            .ok_or_else(|| format!("{tag} NO gone"))?;
        if let (Some(date), Some(message)) = (date, mailbox.messages.get_mut(&uid)) {
            message.internal_date = date;
        }
        Ok(format!(
            "{tag} OK [APPENDUID {} {uid}] APPEND completed",
            mailbox.uid_validity
        ))
    }

    fn idle(&mut self, tag: &str) -> Result<String, String> {
        let io = |err: io::Error| format!("{tag} NO {err}");
        self.send("+ idling").map_err(io)?;
        self.reader
            .get_ref()
            .set_read_timeout(Some(IDLE_TICK))
            .map_err(io)?;
        let mut line = Vec::new();
        let result = loop {
            if let Err(err) = self.report_changes() {
                break Err(io(err));
            }
            match self.reader.read_until(b'\n', &mut line) {
                Ok(0) => break Err(format!("{tag} NO connection closed")),
                Ok(_) if line.ends_with(b"\n") => {
                    let text = String::from_utf8_lossy(&line).trim().to_ascii_uppercase();
                    if self.verbose {
                        eprintln!("imap C: {text}");
                    }
                    if text == "DONE" {
                        break Ok(format!("{tag} OK IDLE terminated"));
                    }
                    line.clear();
                }
                Ok(_) => {}
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(err) => break Err(io(err)),
            }
        };
        self.reader
            .get_ref()
            .set_read_timeout(Some(SESSION_TIMEOUT))
            .map_err(io)?;
        result
    }

    fn fetch(&mut self, cmd: &Command, folder: &str, uid: bool) -> io::Result<()> {
        let items = fetch_items(&cmd.args[1.min(cmd.args.len())..].join(" "));
        let marks_seen = self.writable().is_some()
            && items.iter().any(|item| {
                let item = item.to_ascii_uppercase();
                (item.starts_with("BODY[") || item == "RFC822" || item == "RFC822.TEXT")
                    && !item.contains(".PEEK")
            });
        let responses = {
            let mut store = lock(&self.store);
            let Some(mailbox) = store.folder_mut(folder) else {
                return Ok(());
            };
            let uids = select_set(mailbox, cmd.arg(0), uid);
            let mut responses = Vec::with_capacity(uids.len());
            for message_uid in uids {
                let seq = mailbox.seq_of(message_uid).unwrap_or(0);
                let Some(message) = mailbox.messages.get_mut(&message_uid) else {
                    continue;
                };
                if marks_seen && !message.has_flag("\\Seen") {
                    message.flags.push("\\Seen".into());
                }
                responses.push(fetch_response(seq, message_uid, message, &items, uid));
            }
            if marks_seen {
                mailbox.generation += 1;
            }
            responses
        };
        self.sync_selected();
        for response in responses {
            self.send_bytes(&response)?;
        }
        Ok(())
    }

    /// `STORE set (+|-)FLAGS[.SILENT] (flags)`
    fn store_flags(&mut self, cmd: &Command, folder: &str, uid: bool) -> io::Result<()> {
        let mode = cmd.arg(1).to_ascii_uppercase();
        let silent = mode.ends_with(".SILENT");
        let names = flag_list(&cmd.args[2.min(cmd.args.len())..].join(" "));
        let lines = {
            let mut store = lock(&self.store);
            let Some(mailbox) = store.folder_mut(folder) else {
                return Ok(());
            };
            let uids = select_set(mailbox, cmd.arg(0), uid);
            let mut lines = Vec::new();
            for message_uid in uids {
                let seq = mailbox.seq_of(message_uid).unwrap_or(0);
                let Some(message) = mailbox.messages.get_mut(&message_uid) else {
                    continue;
                };
                if mode.starts_with('+') || mode.starts_with('-') {
                    for name in &names {
                        message
                            .flags
                            .retain(|flag| !flag.eq_ignore_ascii_case(name));
                        if mode.starts_with('+') {
                            message.flags.push(name.clone());
                        }
                    }
                } else {
                    message.flags = names.clone();
                }
                let uid_item = if uid {
                    format!(" UID {message_uid}")
                } else {
                    String::new()
                };
                lines.push(format!(
                    "* {seq} FETCH (FLAGS ({}){uid_item})",
                    message.flags.join(" ")
                ));
            }
            mailbox.generation += 1;
            lines
        };
        self.sync_selected();
        if !silent {
            for line in lines {
                self.send(&line)?;
            }
        }
        Ok(())
    }

    fn copy(
        &mut self,
        cmd: &Command,
        folder: &str,
        uid: bool,
        is_move: bool,
    ) -> Result<String, String> {
        let tag = &cmd.tag;
        let target = cmd.text(1);
        let io = |err: io::Error| format!("{tag} NO {err}");
        let (copyuid, expunged) = {
            let mut store = lock(&self.store);
            if store.folder(&target).is_none() {
                return Err(format!("{tag} NO [TRYCREATE] no such mailbox"));
            }
            let Some(source) = store.folder(folder) else {
                return Err(format!("{tag} NO mailbox is gone"));
            };
            let uids = select_set(source, cmd.arg(0), uid);
            let messages: Vec<StoredMessage> = uids
                .iter()
                .filter_map(|uid| source.messages.get(uid).cloned())
                .collect();
            let mut new_uids = Vec::with_capacity(messages.len());
            for message in messages {
                let flags: Vec<&str> = message.flags.iter().map(String::as_str).collect();
                if let Some(new_uid) = store.append(&target, &message.raw, &flags) {
                    if let Some(copy) = store
                        .folder_mut(&target)
                        .and_then(|mailbox| mailbox.messages.get_mut(&new_uid))
                    {
                        copy.internal_date = message.internal_date;
                    }
                    new_uids.push(new_uid);
                }
            }
            let validity = store
                .folder(&target)
                .map_or(0, |mailbox| mailbox.uid_validity);
            let copyuid = (!uids.is_empty()).then(|| {
                format!(
                    "[COPYUID {validity} {} {}]",
                    join(&uids, ","),
                    join(&new_uids, ",")
                )
            });
            let expunged = if is_move {
                remove(&mut store, folder, &uids)
            } else {
                Vec::new()
            };
            (copyuid, expunged)
        };
        let code = copyuid.unwrap_or_default();
        if is_move {
            if !code.is_empty() {
                self.send(&format!("* OK {code} moved")).map_err(io)?;
            }
            for seq in expunged {
                self.send(&format!("* {seq} EXPUNGE")).map_err(io)?;
            }
            self.sync_selected();
            Ok(format!("{tag} OK {} completed", cmd.name))
        } else {
            Ok(format!("{tag} OK {code} {} completed", cmd.name))
        }
    }

    fn search(&mut self, cmd: &Command, folder: &str, uid: bool) -> io::Result<()> {
        let criteria: Vec<String> = (0..cmd.args.len()).map(|i| cmd.text(i)).collect();
        let found = {
            let store = lock(&self.store);
            let Some(mailbox) = store.folder(folder) else {
                return Ok(());
            };
            mailbox
                .messages
                .iter()
                .enumerate()
                .filter(|(index, (message_uid, message))| {
                    matches_all(&criteria, mailbox, index + 1, **message_uid, message)
                })
                .map(|(index, (message_uid, _))| {
                    if uid {
                        message_uid.to_string()
                    } else {
                        (index + 1).to_string()
                    }
                })
                .collect::<Vec<_>>()
        };
        let line = if found.is_empty() {
            "* SEARCH".to_owned()
        } else {
            format!("* SEARCH {}", found.join(" "))
        };
        self.send(&line)
    }
}

// ---------- mailbox operations ----------

/// Removes `uids` from `folder`; returns their sequence numbers, highest first (so each
/// `EXPUNGE` response stays valid as the client applies them).
fn remove(store: &mut Mailstore, folder: &str, uids: &[u32]) -> Vec<usize> {
    let Some(mailbox) = store.folder_mut(folder) else {
        return Vec::new();
    };
    let mut seqs: Vec<usize> = uids.iter().filter_map(|uid| mailbox.seq_of(*uid)).collect();
    seqs.sort_unstable_by(|a, b| b.cmp(a));
    for uid in uids {
        mailbox.messages.remove(uid);
    }
    mailbox.generation += 1;
    seqs
}

/// Removes `\Deleted` messages (only among `only` when given).
fn expunge(store: &mut Mailstore, folder: &str, only: Option<&[u32]>) -> Vec<usize> {
    let Some(mailbox) = store.folder(folder) else {
        return Vec::new();
    };
    let uids: Vec<u32> = mailbox
        .messages
        .iter()
        .filter(|(uid, message)| {
            message.has_flag("\\Deleted") && only.is_none_or(|only| only.contains(uid))
        })
        .map(|(uid, _)| *uid)
        .collect();
    remove(store, folder, &uids)
}

/// UIDs matched by a sequence set (`1,4:6,9:*`), by UID or by sequence number.
fn select_set(mailbox: &Mailbox, set: &str, by_uid: bool) -> Vec<u32> {
    let uids: Vec<u32> = mailbox.messages.keys().copied().collect();
    let max = if by_uid {
        uids.last().copied().unwrap_or(0)
    } else {
        uids.len() as u32
    };
    let ranges: Vec<(u32, u32)> = set
        .split(',')
        .filter_map(|part| {
            let (from, to) = part.split_once(':').unwrap_or((part, part));
            let parse = |s: &str| {
                if s == "*" {
                    Some(max)
                } else {
                    s.parse::<u32>().ok()
                }
            };
            let (from, to) = (parse(from)?, parse(to)?);
            Some((from.min(to), from.max(to)))
        })
        .collect();
    uids.iter()
        .enumerate()
        .filter(|(index, uid)| {
            let key = if by_uid { **uid } else { *index as u32 + 1 };
            ranges.iter().any(|(from, to)| key >= *from && key <= *to)
        })
        .map(|(_, uid)| *uid)
        .collect()
}

fn matches_all(
    criteria: &[String],
    mailbox: &Mailbox,
    seq: usize,
    uid: u32,
    message: &StoredMessage,
) -> bool {
    let mut index = 0;
    while index < criteria.len() {
        let (matched, used) = matches_one(&criteria[index..], mailbox, seq, uid, message);
        if !matched {
            return false;
        }
        index += used.max(1);
    }
    true
}

/// Evaluates the first criterion of `criteria`; returns (matches, tokens consumed).
/// Unknown criteria match everything (the client filters again anyway).
fn matches_one(
    criteria: &[String],
    mailbox: &Mailbox,
    seq: usize,
    uid: u32,
    message: &StoredMessage,
) -> (bool, usize) {
    let key = criteria[0].to_ascii_uppercase();
    let next = criteria.get(1).map_or("", String::as_str);
    let flag = |name: &str| message.has_flag(name);
    let contains =
        |haystack: &str, needle: &str| haystack.to_lowercase().contains(&needle.to_lowercase());
    let header = |name: &str| header_value(&message.raw, name).unwrap_or_default();
    match key.as_str() {
        "ALL" => (true, 1),
        "SEEN" => (flag("\\Seen"), 1),
        "UNSEEN" => (!flag("\\Seen"), 1),
        "FLAGGED" => (flag("\\Flagged"), 1),
        "UNFLAGGED" => (!flag("\\Flagged"), 1),
        "DELETED" => (flag("\\Deleted"), 1),
        "UNDELETED" => (!flag("\\Deleted"), 1),
        "ANSWERED" => (flag("\\Answered"), 1),
        "UNANSWERED" => (!flag("\\Answered"), 1),
        "DRAFT" => (flag("\\Draft"), 1),
        "UID" => (select_set(mailbox, next, true).contains(&uid), 2),
        "SUBJECT" | "FROM" | "TO" | "CC" | "BCC" => (contains(&header(&key), next), 2),
        "HEADER" => {
            let value = criteria.get(2).map_or("", String::as_str);
            (contains(&header(next), value), 3)
        }
        "TEXT" | "BODY" => (contains(&String::from_utf8_lossy(&message.raw), next), 2),
        "NOT" => {
            let (matched, used) = matches_one(&criteria[1..], mailbox, seq, uid, message);
            (!matched, used + 1)
        }
        "OR" => {
            let (a, used_a) = matches_one(&criteria[1..], mailbox, seq, uid, message);
            let rest = &criteria[(1 + used_a).min(criteria.len())..];
            if rest.is_empty() {
                return (a, 1 + used_a);
            }
            let (b, used_b) = matches_one(rest, mailbox, seq, uid, message);
            (a || b, 1 + used_a + used_b)
        }
        "SINCE" | "BEFORE" | "ON" | "SENTSINCE" | "SENTBEFORE" | "SENTON" | "LARGER"
        | "SMALLER" | "KEYWORD" | "UNKEYWORD" => (true, 2),
        _ if key.starts_with('(') => {
            let inner = tokens(key.trim_start_matches('(').trim_end_matches(')'));
            (matches_all(&inner, mailbox, seq, uid, message), 1)
        }
        _ if key
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit() || c == '*') =>
        {
            (select_set(mailbox, &key, false).contains(&uid), 1)
        }
        _ => (true, 1),
    }
}

// ---------- FETCH ----------

/// Splits `(UID FLAGS BODY.PEEK[HEADER.FIELDS (A B)])` into items; expands the macros.
fn fetch_items(text: &str) -> Vec<String> {
    let text = text.trim();
    let inner = text
        .strip_prefix('(')
        .and_then(|t| t.strip_suffix(')'))
        .unwrap_or(text);
    let items = tokens(inner);
    items
        .into_iter()
        .flat_map(|item| {
            match item.to_ascii_uppercase().as_str() {
                "ALL" => vec!["FLAGS", "INTERNALDATE", "RFC822.SIZE", "ENVELOPE"],
                "FAST" => vec!["FLAGS", "INTERNALDATE", "RFC822.SIZE"],
                "FULL" => vec!["FLAGS", "INTERNALDATE", "RFC822.SIZE", "ENVELOPE", "BODY"],
                _ => return vec![item],
            }
            .into_iter()
            .map(str::to_owned)
            .collect()
        })
        .collect()
}

fn fetch_response(
    seq: usize,
    uid: u32,
    message: &StoredMessage,
    items: &[String],
    by_uid: bool,
) -> Vec<u8> {
    let mut parts: Vec<Vec<u8>> = Vec::new();
    let has = |name: &str| items.iter().any(|item| item.eq_ignore_ascii_case(name));
    if by_uid || has("UID") {
        parts.push(format!("UID {uid}").into_bytes());
    }
    for item in items {
        let upper = item.to_ascii_uppercase();
        match upper.as_str() {
            "UID" => {}
            "FLAGS" => parts.push(format!("FLAGS ({})", message.flags.join(" ")).into_bytes()),
            "INTERNALDATE" => parts.push(
                format!(
                    "INTERNALDATE \"{}\"",
                    message.internal_date.format("%d-%b-%Y %H:%M:%S %z")
                )
                .into_bytes(),
            ),
            "RFC822.SIZE" => parts.push(format!("RFC822.SIZE {}", message.raw.len()).into_bytes()),
            "ENVELOPE" => parts.push(envelope(&message.raw).into_bytes()),
            "BODY" | "BODYSTRUCTURE" => parts.push(
                format!(
                    "{upper} (\"TEXT\" \"PLAIN\" (\"CHARSET\" \"UTF-8\") NIL NIL \"8BIT\" {} {})",
                    message.raw.len(),
                    message.raw.split(|b| *b == b'\n').count()
                )
                .into_bytes(),
            ),
            "RFC822" => parts.push(literal_item("RFC822", &message.raw)),
            "RFC822.HEADER" => {
                parts.push(literal_item("RFC822.HEADER", header_block(&message.raw)))
            }
            "RFC822.TEXT" => parts.push(literal_item("RFC822.TEXT", body_text(&message.raw))),
            _ if upper.starts_with("BODY[") || upper.starts_with("BODY.PEEK[") => {
                parts.push(body_section(&upper, &message.raw));
            }
            _ => {}
        }
    }
    let mut response = format!("* {seq} FETCH (").into_bytes();
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            response.push(b' ');
        }
        response.extend_from_slice(part);
    }
    response.extend_from_slice(b")\r\n");
    response
}

/// `BODY[]`, `BODY.PEEK[HEADER]`, `BODY[TEXT]<0.100>`... (MIME part numbers get the whole
/// message: enough for a test server).
fn body_section(item: &str, raw: &[u8]) -> Vec<u8> {
    let open = item.find('[').unwrap_or(0);
    let close = item.rfind(']').unwrap_or(item.len());
    let section = &item[open + 1..close];
    let partial = item[close..]
        .strip_prefix("]<")
        .and_then(|rest| rest.strip_suffix('>'))
        .and_then(|range| range.split_once('.'))
        .and_then(|(start, len)| Some((start.parse::<usize>().ok()?, len.parse::<usize>().ok()?)));
    let mut bytes: &[u8] = if section.starts_with("HEADER") {
        header_block(raw)
    } else if section == "TEXT" {
        body_text(raw)
    } else {
        raw
    };
    let mut name = format!("BODY[{section}]");
    if let Some((start, len)) = partial {
        let start = start.min(bytes.len());
        bytes = &bytes[start..(start + len).min(bytes.len())];
        name.push_str(&format!("<{start}>"));
    }
    literal_item(&name, bytes)
}

fn literal_item(name: &str, bytes: &[u8]) -> Vec<u8> {
    let mut item = format!("{name} {{{}}}\r\n", bytes.len()).into_bytes();
    item.extend_from_slice(bytes);
    item
}

fn body_text(raw: &[u8]) -> &[u8] {
    &raw[header_block(raw).len()..]
}

/// A minimal ENVELOPE: date, subject and message-id; addresses as NIL.
fn envelope(raw: &[u8]) -> String {
    let field = |name: &str| header_value(raw, name).map_or("NIL".to_owned(), |v| quote(&v));
    format!(
        "ENVELOPE ({} {} NIL NIL NIL NIL NIL NIL {} {})",
        field("date"),
        field("subject"),
        field("in-reply-to"),
        field("message-id")
    )
}

// ---------- parsing helpers ----------

/// `text` ending in `{12}` or `{12+}`: (text before it, size, non-synchronizing).
fn literal_suffix(text: &str) -> Option<(&str, usize, bool)> {
    let inner = text.strip_suffix('}')?;
    let open = inner.rfind('{')?;
    let spec = &inner[open + 1..];
    let (digits, non_sync) = match spec.strip_suffix('+') {
        Some(digits) => (digits, true),
        None => (spec, false),
    };
    let size = digits.parse().ok()?;
    Some((&text[..open], size, non_sync))
}

fn literal_index(token: &str) -> Option<usize> {
    token.strip_prefix(LITERAL)?.parse().ok()
}

/// Splits arguments: atoms (brackets kept whole), "quoted strings" (unquoted) and
/// (parenthesised lists) kept whole, nested ones included.
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
            _ => {
                let mut token = String::new();
                let (mut parens, mut brackets, mut quoted) = (0i32, 0i32, false);
                while let Some(&c) = chars.peek() {
                    if c == ' ' && parens <= 0 && brackets <= 0 && !quoted {
                        break;
                    }
                    match c {
                        '"' => quoted = !quoted,
                        '(' if !quoted => parens += 1,
                        ')' if !quoted => parens -= 1,
                        '[' if !quoted => brackets += 1,
                        ']' if !quoted => brackets -= 1,
                        _ => {}
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

/// `(\Seen \Flagged)` or `\Seen` → flag names.
fn flag_list(text: &str) -> Vec<String> {
    text.trim_matches(['(', ')'])
        .split_whitespace()
        .map(|flag| flag.trim_matches(['(', ')']).to_owned())
        .filter(|flag| !flag.is_empty())
        .collect()
}

/// LIST pattern match: `*` and `%` match any run of characters.
fn glob(pattern: &str, name: &str) -> bool {
    fn matches(pattern: &[char], name: &[char]) -> bool {
        match pattern.split_first() {
            None => name.is_empty(),
            Some(('*' | '%', rest)) => (0..=name.len()).any(|i| matches(rest, &name[i..])),
            Some((c, rest)) => name
                .split_first()
                .is_some_and(|(n, name)| n.eq_ignore_ascii_case(c) && matches(rest, name)),
        }
    }
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    matches(&pattern, &name)
}

fn quote(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

fn join(values: &[u32], separator: &str) -> String {
    values
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(separator)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizer_keeps_lists_and_sections_whole() {
        assert_eq!(
            tokens(r#"a1 UID FETCH 1:* (UID BODY.PEEK[HEADER.FIELDS (FROM TO)])"#),
            vec![
                "a1",
                "UID",
                "FETCH",
                "1:*",
                "(UID BODY.PEEK[HEADER.FIELDS (FROM TO)])"
            ]
        );
        assert_eq!(
            tokens(r#"a LOGIN "me" "p\"w""#),
            vec!["a", "LOGIN", "me", "p\"w"]
        );
        assert_eq!(
            fetch_items("(UID FLAGS BODY.PEEK[HEADER.FIELDS (A B)])"),
            vec!["UID", "FLAGS", "BODY.PEEK[HEADER.FIELDS (A B)]"]
        );
    }

    #[test]
    fn sets_select_by_uid_and_sequence() {
        let mut store = Mailstore::new("x");
        for _ in 0..5 {
            store.append("INBOX", b"Subject: x\r\n\r\n", &[]);
        }
        store.folder_mut("INBOX").unwrap().messages.remove(&2);
        let inbox = store.folder("INBOX").unwrap();
        assert_eq!(select_set(inbox, "1:*", true), vec![1, 3, 4, 5]);
        assert_eq!(select_set(inbox, "2,4", true), vec![4]);
        assert_eq!(select_set(inbox, "2:3", false), vec![3, 4]);
        assert_eq!(select_set(inbox, "9:*", true), vec![5]);
    }

    #[test]
    fn literals_and_globs() {
        assert_eq!(
            literal_suffix("a APPEND Sent {12}"),
            Some(("a APPEND Sent ", 12, false))
        );
        assert_eq!(literal_suffix("a {3+}"), Some(("a ", 3, true)));
        assert_eq!(literal_suffix("a (x)"), None);
        assert!(glob("*", "Sent"));
        assert!(glob("S%", "Sent"));
        assert!(!glob("T*", "Sent"));
    }

    #[test]
    fn body_sections() {
        let raw = b"Subject: s\r\n\r\nhello world";
        assert_eq!(
            body_section("BODY.PEEK[HEADER]", raw),
            b"BODY[HEADER] {14}\r\nSubject: s\r\n\r\n".to_vec()
        );
        assert_eq!(
            body_section("BODY[TEXT]<6.5>", raw),
            b"BODY[TEXT]<6> {5}\r\nworld".to_vec()
        );
    }
}
