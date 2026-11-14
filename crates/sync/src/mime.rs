//! RFC 5322 / MIME parsing (via `mail-parser`: encodings, charsets, multipart) into the core
//! [`Message`] model.

use chrono::{DateTime, Utc};
use mail_parser::{Addr, HeaderValue, MessageParser, MimeHeaders};
use mailbox_core::{Address, Attachment, Importance, Message, MessageFlags};

use crate::thread::{ThreadIndex, normalize_id};
use crate::{SyncError, SyncResult};

/// Characters kept in [`Message::preview`].
pub const PREVIEW_LEN: usize = 120;

/// Attachment content, to be written to disk by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentData {
    pub name: String,
    pub mime: String,
    pub bytes: Vec<u8>,
}

/// A parsed message plus the headers the core model does not keep.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedMail {
    /// Ids are 0; `thread_id` is the message's own id until [`ParsedMail::assign_thread`].
    pub message: Message,
    /// Normalized `Message-ID` (no angle brackets), empty when missing.
    pub message_id: String,
    pub in_reply_to: Vec<String>,
    pub references: Vec<String>,
    pub attachments: Vec<AttachmentData>,
    /// `Disposition-Notification-To`: the sender asks for a read receipt.
    pub receipt_to: Option<Address>,
}

impl ParsedMail {
    /// Sets `message.thread_id` from the threading headers.
    pub fn assign_thread(&mut self, index: &mut ThreadIndex) {
        // No ids at all (exported or hand-made files): group by subject.
        let fallback = subject_thread(&self.message.subject);
        self.message.thread_id = index.assign(
            &self.message_id,
            &self.in_reply_to,
            &self.references,
            &fallback,
        );
    }
}

/// Parses a full message (or just its header block: bodies are then empty).
/// `received` is used when the `Date` header is missing or invalid.
pub fn parse_message(raw: &[u8], received: Option<DateTime<Utc>>) -> SyncResult<ParsedMail> {
    let parsed = MessageParser::default()
        .parse(raw)
        .ok_or_else(|| SyncError::Parse("not an RFC 5322 message".into()))?;

    let date = parsed
        .date()
        .and_then(|date| DateTime::from_timestamp(date.to_timestamp(), 0))
        .or(received)
        .unwrap_or_else(Utc::now);
    let from = parsed
        .from()
        .or_else(|| parsed.sender())
        .and_then(|list| list.first())
        .map(address)
        .unwrap_or_default();
    let body_html = parsed.body_html(0).map(|html| html.into_owned());
    // HTML-only mail: mail-parser's own conversion glues headings, divs and table cells
    // together ("order!Your parcel"), so convert with block breaks.
    let html_only = parsed.text_part(0).is_some_and(|part| part.is_text_html());
    let body_text = match &body_html {
        Some(html) if html_only => html_text(html),
        _ => parsed
            .body_text(0)
            .map(|text| text.into_owned())
            .unwrap_or_default(),
    };
    let attachments: Vec<AttachmentData> = parsed
        .attachments()
        .map(|part| AttachmentData {
            name: part.attachment_name().unwrap_or("attachment").to_owned(),
            mime: part
                .content_type()
                .map(|ct| match ct.subtype() {
                    Some(sub) => format!("{}/{}", ct.ctype(), sub),
                    None => ct.ctype().to_owned(),
                })
                .unwrap_or_else(|| "application/octet-stream".to_owned())
                .to_lowercase(),
            bytes: part.contents().to_vec(),
        })
        .collect();
    let message_id = parsed.message_id().map(normalize_id).unwrap_or_default();

    let message = Message {
        id: 0,
        account_id: 0,
        folder_id: 0,
        thread_id: message_id.clone(),
        from,
        to: addresses(parsed.to()),
        cc: addresses(parsed.cc()),
        subject: parsed.subject().unwrap_or_default().trim().to_owned(),
        date,
        preview: preview(&body_text),
        body_text,
        body_html,
        flags: MessageFlags::default(),
        importance: importance(
            parsed.header_raw("Importance"),
            parsed.header_raw("X-Priority"),
        ),
        categories: Vec::new(),
        attachments: attachments
            .iter()
            .map(|data| Attachment {
                name: data.name.clone(),
                mime: data.mime.clone(),
                size: data.bytes.len() as u64,
            })
            .collect(),
    };
    Ok(ParsedMail {
        message,
        message_id,
        in_reply_to: id_list(parsed.in_reply_to()),
        references: id_list(parsed.references()),
        attachments,
        receipt_to: receipt_request(parsed.header_raw("Disposition-Notification-To")),
    })
}

/// Address of a `Disposition-Notification-To` header (`Name <a@b.c>` or bare).
fn receipt_request(value: Option<&str>) -> Option<Address> {
    let value = value?.trim();
    let email = match (value.rfind('<'), value.rfind('>')) {
        (Some(start), Some(end)) if start < end => &value[start + 1..end],
        _ => value,
    }
    .trim();
    email.contains('@').then(|| Address::new("", email))
}

fn address(addr: &Addr) -> Address {
    Address::new(
        addr.name().unwrap_or_default().trim(),
        addr.address().unwrap_or_default().trim(),
    )
}

fn addresses(list: Option<&mail_parser::Address<'_>>) -> Vec<Address> {
    list.map(|list| list.iter().map(address).collect())
        .unwrap_or_default()
}

fn id_list(value: &HeaderValue) -> Vec<String> {
    let ids: Vec<String> = match (value.as_text_list(), value.as_text()) {
        (Some(list), _) => list.iter().map(|id| normalize_id(id)).collect(),
        (None, Some(text)) => text.split_whitespace().map(normalize_id).collect(),
        (None, None) => Vec::new(),
    };
    ids.into_iter().filter(|id| !id.is_empty()).collect()
}

/// `Importance: high|low` or `X-Priority: 1..5` (1-2 high, 4-5 low).
fn importance(importance: Option<&str>, priority: Option<&str>) -> Importance {
    if let Some(value) = importance {
        match value.trim().to_lowercase().as_str() {
            "high" => return Importance::High,
            "low" => return Importance::Low,
            _ => {}
        }
    }
    match priority.and_then(|p| p.trim().chars().next()) {
        Some('1' | '2') => Importance::High,
        Some('4' | '5') => Importance::Low,
        _ => Importance::Normal,
    }
}

/// Plain text of an HTML body: line breaks after block elements, spaces between cells.
pub fn html_text(html: &str) -> String {
    const BLOCKS: [&str; 13] = [
        "</h1",
        "</h2",
        "</h3",
        "</h4",
        "</h5",
        "</h6",
        "</div",
        "</tr",
        "</li",
        "</table",
        "</blockquote",
        "</ul",
        "</ol",
    ];
    let lower = html.to_ascii_lowercase();
    let mut spaced = String::with_capacity(html.len() + 64);
    let mut last = 0;
    for (index, _) in lower.match_indices("</") {
        let rest = &lower[index..];
        let boundary = |tag: &str| {
            rest.starts_with(tag)
                && rest[tag.len()..].starts_with(|c: char| c == '>' || c.is_whitespace())
        };
        let insert = if BLOCKS.iter().any(|tag| boundary(tag)) {
            "<br>"
        } else if boundary("</td") || boundary("</th") {
            " "
        } else {
            continue;
        };
        spaced.push_str(&html[last..index]);
        spaced.push_str(insert);
        last = index;
    }
    spaced.push_str(&html[last..]);
    mail_parser::decoders::html::html_to_text(&spaced)
}

/// Thread id of messages without threading headers: [`SUBJECT_THREAD`] + the subject
/// without `Re:` / `Fwd:` prefixes.
pub fn subject_thread(subject: &str) -> String {
    format!(
        "{SUBJECT_THREAD}{}",
        mailbox_core::compose::conversation_subject(subject)
    )
}

/// Prefix of [`subject_thread`] ids.
pub const SUBJECT_THREAD: &str = "subject:";

/// First [`PREVIEW_LEN`] characters of the text with whitespace collapsed.
pub fn preview(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(PREVIEW_LEN)
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const SIMPLE: &[u8] = b"From: Alice Martin <alice@example.com>\r\n\
To: Bob <bob@example.com>, carol@example.com\r\n\
Cc: =?UTF-8?Q?Ren=C3=A9?= <rene@example.com>\r\n\
Subject: =?ISO-8859-1?Q?Caf=E9?= meeting\r\n\
Date: Sat, 14 Nov 2026 09:30:00 +0100\r\n\
Message-ID: <root@example.com>\r\n\
X-Priority: 1 (Highest)\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Hello Bob,\r\n\r\nSee you   at 10.\r\n";

    pub const REPLY: &[u8] = b"From: Bob <bob@example.com>\r\n\
To: Alice Martin <alice@example.com>\r\n\
Subject: Re: Cafe meeting\r\n\
Date: Sat, 14 Nov 2026 10:00:00 +0000\r\n\
Message-ID: <reply@example.com>\r\n\
In-Reply-To: <root@example.com>\r\n\
References: <root@example.com>\r\n\
\r\n\
Sure.\r\n";

    pub const MULTIPART: &[u8] = b"From: news@example.org\r\n\
To: bob@example.com\r\n\
Subject: Report\r\n\
Date: Fri, 13 Nov 2026 08:00:00 +0000\r\n\
Message-ID: <multi@example.org>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"outer\"\r\n\
\r\n\
--outer\r\n\
Content-Type: multipart/alternative; boundary=\"inner\"\r\n\
\r\n\
--inner\r\n\
Content-Type: text/plain; charset=iso-8859-1\r\n\
Content-Transfer-Encoding: quoted-printable\r\n\
\r\n\
R=E9sum=E9 attached.\r\n\
--inner\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<p>R\xc3\xa9sum\xc3\xa9 <b>attached</b>.</p>\r\n\
--inner--\r\n\
--outer\r\n\
Content-Type: application/pdf; name=\"report.pdf\"\r\n\
Content-Disposition: attachment; filename=\"report.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERi0xLjQK\r\n\
--outer--\r\n";

    #[test]
    fn parses_headers_addresses_and_charsets() {
        let parsed = parse_message(SIMPLE, None).unwrap();
        let message = &parsed.message;
        assert_eq!(
            message.from,
            Address::new("Alice Martin", "alice@example.com")
        );
        assert_eq!(message.to.len(), 2);
        assert_eq!(message.to[1].email, "carol@example.com");
        assert_eq!(message.cc[0].name, "René");
        assert_eq!(message.subject, "Café meeting");
        assert_eq!(message.date.to_rfc3339(), "2026-11-14T08:30:00+00:00");
        assert_eq!(message.importance, Importance::High);
        assert_eq!(message.preview, "Hello Bob, See you at 10.");
        assert_eq!(parsed.message_id, "root@example.com");
        assert!(message.attachments.is_empty());
    }

    #[test]
    fn parses_multipart_bodies_and_attachments() {
        let parsed = parse_message(MULTIPART, None).unwrap();
        assert_eq!(parsed.message.body_text.trim(), "Résumé attached.");
        let html = parsed.message.body_html.as_deref().unwrap();
        assert!(html.contains("<b>attached</b>"));
        assert_eq!(parsed.attachments.len(), 1);
        let pdf = &parsed.attachments[0];
        assert_eq!(pdf.name, "report.pdf");
        assert_eq!(pdf.mime, "application/pdf");
        assert_eq!(pdf.bytes, b"%PDF-1.4\n");
        assert_eq!(parsed.message.attachments[0].size, 9);
    }

    #[test]
    fn header_only_parse_has_empty_body() {
        let parsed = parse_message(crate::backend::header_block(SIMPLE), None).unwrap();
        assert_eq!(parsed.message.subject, "Café meeting");
        assert!(parsed.message.body_text.trim().is_empty());
    }

    #[test]
    fn threads_reply_with_root() {
        let mut index = ThreadIndex::new();
        let mut root = parse_message(SIMPLE, None).unwrap();
        let mut reply = parse_message(REPLY, None).unwrap();
        assert_eq!(reply.in_reply_to, vec!["root@example.com".to_owned()]);
        root.assign_thread(&mut index);
        reply.assign_thread(&mut index);
        assert_eq!(root.message.thread_id, reply.message.thread_id);
    }

    #[test]
    fn rejects_garbage_gracefully() {
        // mail-parser is lenient; whatever it returns must not panic and keep a date.
        if let Ok(parsed) = parse_message(b"", None) {
            assert!(parsed.message.subject.is_empty());
        }
    }

    #[test]
    fn reads_receipt_requests() {
        let raw = b"From: a@b.co\r\nDisposition-Notification-To: Ada <ada@b.co>\r\n\r\nhi\r\n";
        let parsed = parse_message(raw, None).unwrap();
        assert_eq!(parsed.receipt_to.map(|a| a.email), Some("ada@b.co".into()));
        let plain = parse_message(b"From: a@b.co\r\nSubject: x\r\n\r\nhi\r\n", None).unwrap();
        assert!(plain.receipt_to.is_none());
    }

    #[test]
    fn html_only_mail_keeps_block_breaks() {
        let raw = b"From: shop@example.com\r\nSubject: Order\r\nContent-Type: text/html\r\n\r\n\
<h2>Thanks for your order!</h2><div>Your parcel is on its way.</div>\
<table><tr><td>Item</td><td>Price</td></tr></table>";
        let parsed = parse_message(raw, None).unwrap();
        assert!(
            parsed
                .message
                .preview
                .starts_with("Thanks for your order! Your parcel"),
            "{}",
            parsed.message.preview
        );
        assert!(parsed.message.body_text.contains("Item Price"));
    }

    #[test]
    fn messages_without_ids_thread_by_subject() {
        let mut index = ThreadIndex::new();
        let mut first =
            parse_message(b"From: a@example.com\r\nSubject: Q4 budget\r\n\r\nHi", None).unwrap();
        let mut reply = parse_message(
            b"From: b@example.com\r\nSubject: RE: q4 Budget\r\n\r\nOk",
            None,
        )
        .unwrap();
        first.assign_thread(&mut index);
        reply.assign_thread(&mut index);
        assert_eq!(first.message.thread_id, "subject:q4 budget");
        assert_eq!(first.message.thread_id, reply.message.thread_id);
    }
}
