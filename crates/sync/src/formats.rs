//! File formats: `.eml` (one RFC 5322 message) and `.mbox` (mboxrd: `From ` separator lines,
//! `>From ` quoting), plus building RFC 5322 sources from core messages (export and SMTP).

use std::io::Write;
use std::path::Path;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use lettre::address::Envelope;
use lettre::message::header::{ContentType, HeaderName, HeaderValue};
use lettre::message::{Attachment as MimeAttachment, Mailbox, MultiPart, SinglePart};
use mailbox_core::{Address, Importance, Message};
use serde::{Deserialize, Serialize};

use crate::mime::{AttachmentData, ParsedMail, parse_message};
use crate::thread::{ThreadIndex, normalize_id};
use crate::{SyncError, SyncResult};

/// Parses an `.eml` file.
pub fn read_eml(data: &[u8]) -> SyncResult<ParsedMail> {
    let mut parsed = parse_message(data, None)?;
    parsed.assign_thread(&mut ThreadIndex::new());
    Ok(parsed)
}

/// Builds an `.eml` file for a message (see [`build_rfc5322`]).
pub fn write_eml(
    message: &Message,
    message_id: Option<&str>,
    attachments: &[AttachmentData],
) -> SyncResult<Vec<u8>> {
    build_rfc5322(message, message_id, attachments)
}

/// One mbox entry: the envelope sender and date of the `From ` line, and the raw message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MboxEntry {
    pub sender: String,
    pub date: DateTime<Utc>,
    pub raw: Vec<u8>,
}

/// Parses every message of an mbox file, threading them together. Unparsable entries are skipped.
pub fn read_mbox(data: &[u8]) -> Vec<ParsedMail> {
    let mut index = ThreadIndex::new();
    split_mbox(data)
        .iter()
        .filter_map(|raw| parse_message(raw, None).ok())
        .map(|mut parsed| {
            parsed.assign_thread(&mut index);
            parsed
        })
        .collect()
}

/// Splits an mbox file into raw messages (mboxrd unquoting).
pub fn split_mbox(data: &[u8]) -> Vec<Vec<u8>> {
    let mut messages = Vec::new();
    let mut current: Option<Vec<u8>> = None;
    for line in data.split_inclusive(|byte| *byte == b'\n') {
        if line.starts_with(b"From ") {
            messages.extend(current.take().map(strip_separator));
            current = Some(Vec::new());
            continue;
        }
        // Anything before the first `From ` line is not a message.
        let Some(buffer) = current.as_mut() else {
            continue;
        };
        if line.starts_with(b">") && is_quoted_from(line) {
            buffer.extend_from_slice(&line[1..]);
        } else {
            buffer.extend_from_slice(line);
        }
    }
    messages.extend(current.map(strip_separator));
    messages
}

/// Writes messages as mboxrd.
pub fn write_mbox(out: &mut impl Write, entries: &[MboxEntry]) -> std::io::Result<()> {
    for entry in entries {
        let sender = entry.sender.trim();
        let sender = if sender.is_empty() || sender.contains(char::is_whitespace) {
            "MAILER-DAEMON"
        } else {
            sender
        };
        writeln!(
            out,
            "From {sender} {}",
            entry.date.format("%a %b %e %H:%M:%S %Y")
        )?;
        for line in entry.raw.split_inclusive(|byte| *byte == b'\n') {
            if is_quoted_from(line) {
                out.write_all(b">")?;
            }
            out.write_all(line)?;
        }
        if !entry.raw.ends_with(b"\n") {
            out.write_all(b"\n")?;
        }
        out.write_all(b"\n")?;
    }
    Ok(())
}

/// `From ` preceded by zero or more `>`.
fn is_quoted_from(line: &[u8]) -> bool {
    let start = line.iter().take_while(|byte| **byte == b'>').count();
    line[start..].starts_with(b"From ")
}

/// Drops the blank line that separates mbox entries.
fn strip_separator(mut message: Vec<u8>) -> Vec<u8> {
    if message.ends_with(b"\n\n") {
        message.pop();
    } else if message.ends_with(b"\n\r\n") {
        message.truncate(message.len() - 2);
    }
    message
}

/// Outgoing headers that [`Message`] does not hold (they come from the compose draft).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OutgoingHeaders {
    /// RFC 5322 `Message-ID` (with or without angle brackets).
    pub message_id: Option<String>,
    /// Envelope-only recipients: never written into the message.
    pub bcc: Vec<Address>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    /// Asks for a read receipt (`Disposition-Notification-To: <sender>`).
    pub read_receipt: bool,
}

impl OutgoingHeaders {
    /// Reads headers saved with [`OutgoingHeaders::save`]; `None` if missing or unreadable.
    pub fn load(path: &Path) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub fn save(&self, path: &Path) -> SyncResult<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }
}

/// Builds an RFC 5322 source: text (+ HTML alternative), attachments, `Importance`.
/// Invalid addresses are skipped (an invalid sender becomes `unknown@localhost`) so an export
/// never fails on one odd message.
pub fn build_rfc5322(
    message: &Message,
    message_id: Option<&str>,
    attachments: &[AttachmentData],
) -> SyncResult<Vec<u8>> {
    let headers = OutgoingHeaders {
        message_id: message_id.map(str::to_owned),
        ..OutgoingHeaders::default()
    };
    build_outgoing(message, &headers, attachments)
}

/// Like [`build_rfc5322`], plus reply threading (`In-Reply-To`, `References`) and the
/// read-receipt request. Bcc stays out of the headers (pass it to the SMTP envelope).
pub fn build_outgoing(
    message: &Message,
    headers: &OutgoingHeaders,
    attachments: &[AttachmentData],
) -> SyncResult<Vec<u8>> {
    let from = mailbox(&message.from)
        .or_else(|| {
            mailbox(&Address::new(
                message.from.name.clone(),
                "unknown@localhost",
            ))
        })
        .ok_or_else(|| SyncError::Parse("invalid sender".into()))?;
    let to: Vec<Mailbox> = message.to.iter().filter_map(mailbox).collect();
    let cc: Vec<Mailbox> = message.cc.iter().filter_map(mailbox).collect();

    let sender_email = from.email.to_string();
    // The SMTP envelope is never written into the message; it only has to be valid.
    let mut recipients: Vec<_> = to.iter().chain(&cc).map(|mb| mb.email.clone()).collect();
    if recipients.is_empty() {
        recipients.push(from.email.clone());
    }
    let envelope = Envelope::new(Some(from.email.clone()), recipients)
        .map_err(|err| SyncError::Parse(err.to_string()))?;

    let mut builder = lettre::Message::builder()
        .from(from)
        .subject(message.subject.clone())
        .date(SystemTime::from(message.date))
        .envelope(envelope);
    for mailbox in to {
        builder = builder.to(mailbox);
    }
    for mailbox in cc {
        builder = builder.cc(mailbox);
    }
    let angle = |id: &str| format!("<{}>", normalize_id(id));
    if let Some(id) = &headers.message_id {
        builder = builder.message_id(Some(angle(id)));
    }
    if let Some(id) = headers
        .in_reply_to
        .as_deref()
        .filter(|id| !id.trim().is_empty())
    {
        builder = builder.in_reply_to(angle(id));
    }
    let references: Vec<String> = headers
        .references
        .iter()
        .filter(|id| !id.trim().is_empty())
        .map(|id| angle(id))
        .collect();
    if !references.is_empty() {
        builder = builder.references(references.join(" "));
    }
    if headers.read_receipt {
        builder = builder.raw_header(HeaderValue::new(
            HeaderName::new_from_ascii_str("Disposition-Notification-To"),
            format!("<{}>", sender_email),
        ));
    }
    let importance = match message.importance {
        Importance::High => Some("high"),
        Importance::Low => Some("low"),
        Importance::Normal => None,
    };
    if let Some(value) = importance {
        builder = builder.raw_header(HeaderValue::new(
            HeaderName::new_from_ascii_str("Importance"),
            value.to_owned(),
        ));
    }

    let text = message.body_text.clone();
    let built = match (&message.body_html, attachments.is_empty()) {
        (None, true) => builder.header(ContentType::TEXT_PLAIN).body(text),
        (Some(html), true) => {
            builder.multipart(MultiPart::alternative_plain_html(text, html.clone()))
        }
        (html, false) => {
            let mut mixed = match html {
                Some(html) => MultiPart::mixed()
                    .multipart(MultiPart::alternative_plain_html(text, html.clone())),
                None => MultiPart::mixed().singlepart(SinglePart::plain(text)),
            };
            for attachment in attachments {
                let content_type = ContentType::parse(&attachment.mime)
                    .or_else(|_| ContentType::parse("application/octet-stream"))
                    .map_err(|err| SyncError::Parse(format!("{err:?}")))?;
                mixed = mixed.singlepart(
                    MimeAttachment::new(attachment.name.clone())
                        .body(attachment.bytes.clone(), content_type),
                );
            }
            builder.multipart(mixed)
        }
    }
    .map_err(|err| SyncError::Parse(err.to_string()))?;
    Ok(built.formatted())
}

fn mailbox(address: &Address) -> Option<Mailbox> {
    let email = address.email.trim().parse().ok()?;
    let name = address.name.trim();
    Some(Mailbox::new(
        (!name.is_empty()).then(|| name.to_owned()),
        email,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mime::tests::{MULTIPART, REPLY, SIMPLE};
    use chrono::TimeZone;
    use mailbox_core::{Attachment, MessageFlags};

    fn entry(raw: &[u8]) -> MboxEntry {
        MboxEntry {
            sender: "alice@example.com".into(),
            date: Utc.with_ymd_and_hms(2026, 11, 14, 9, 30, 0).unwrap(),
            raw: raw.to_vec(),
        }
    }

    #[test]
    fn mbox_roundtrip_is_lossless() {
        let tricky = b"Subject: quoting\r\n\r\nFrom the start\r\n>From quoted\r\nend\r\n";
        let raws: Vec<&[u8]> = vec![SIMPLE, REPLY, MULTIPART, tricky];
        let mut file = Vec::new();
        write_mbox(
            &mut file,
            &raws.iter().map(|raw| entry(raw)).collect::<Vec<_>>(),
        )
        .unwrap();

        let text = String::from_utf8_lossy(&file);
        assert!(text.starts_with("From alice@example.com Sat Nov 14 09:30:00 2026\n"));
        assert!(text.contains("\n>From the start"));
        assert!(text.contains("\n>>From quoted"));

        let split = split_mbox(&file);
        assert_eq!(split.len(), 4);
        for (original, read) in raws.iter().zip(&split) {
            assert_eq!(read.as_slice(), *original);
        }
    }

    #[test]
    fn read_mbox_threads_messages() {
        let mut file = Vec::new();
        write_mbox(&mut file, &[entry(SIMPLE), entry(REPLY)]).unwrap();
        let messages = read_mbox(&file);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].message.thread_id, messages[1].message.thread_id);
        assert_eq!(messages[1].message.subject, "Re: Cafe meeting");
    }

    #[test]
    fn ignores_text_before_first_separator() {
        let file = b"garbage\nFrom x Sat Nov 14 09:30:00 2026\nSubject: a\n\nbody\n\n";
        assert_eq!(split_mbox(file), vec![b"Subject: a\n\nbody\n".to_vec()]);
    }

    fn sample_message() -> Message {
        Message {
            id: 7,
            account_id: 1,
            folder_id: 1,
            thread_id: "t".into(),
            from: Address::new("Alice Martin", "alice@example.com"),
            to: vec![
                Address::new("Bob", "bob@example.com"),
                Address::new("", "bad address"),
            ],
            cc: vec![Address::new("René", "rene@example.com")],
            subject: "Café — plans".into(),
            date: Utc.with_ymd_and_hms(2026, 11, 14, 9, 30, 0).unwrap(),
            preview: String::new(),
            body_text: "Hello Bob,\nsee you at 10.\n".into(),
            body_html: Some("<p>Hello <b>Bob</b></p>".into()),
            flags: MessageFlags::default(),
            importance: Importance::High,
            categories: vec![],
            attachments: vec![],
        }
    }

    #[test]
    fn eml_export_then_import_keeps_content() {
        let message = sample_message();
        let attachment = AttachmentData {
            name: "data.bin".into(),
            mime: "application/octet-stream".into(),
            bytes: vec![0, 159, 146, 150, 10, 13, 255],
        };
        let raw = write_eml(&message, Some("<id-1@example.com>"), &[attachment]).unwrap();
        let parsed = read_eml(&raw).unwrap();
        let back = &parsed.message;
        assert_eq!(parsed.message_id, "id-1@example.com");
        assert_eq!(back.from, message.from);
        assert_eq!(back.to, vec![Address::new("Bob", "bob@example.com")]);
        assert_eq!(back.cc, message.cc);
        assert_eq!(back.subject, message.subject);
        assert_eq!(back.date, message.date);
        assert_eq!(back.importance, Importance::High);
        assert_eq!(
            back.body_text.replace("\r\n", "\n").trim(),
            message.body_text.trim()
        );
        assert!(
            back.body_html
                .as_deref()
                .unwrap_or_default()
                .contains("<b>Bob</b>")
        );
        assert_eq!(
            back.attachments,
            vec![Attachment {
                name: "data.bin".into(),
                mime: "application/octet-stream".into(),
                size: 7
            }]
        );
        assert_eq!(
            parsed.attachments[0].bytes,
            vec![0, 159, 146, 150, 10, 13, 255]
        );
    }

    #[test]
    fn plain_message_without_recipients_still_exports() {
        let mut message = sample_message();
        message.to.clear();
        message.cc.clear();
        message.body_html = None;
        let raw = write_eml(&message, None, &[]).unwrap();
        let parsed = read_eml(&raw).unwrap();
        let body = parsed.message.body_text.replace("\r\n", "\n");
        assert_eq!(body.trim(), "Hello Bob,\nsee you at 10.");
    }
}
