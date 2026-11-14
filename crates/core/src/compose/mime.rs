//! RFC 5322 / MIME message building: headers (RFC 2047 encoded words), quoted-printable text,
//! base64 attachments, `multipart/alternative` inside `multipart/mixed`.
//!
//! Hand-rolled on purpose: it is small, fully tested and needs no extra dependency.

use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};

use crate::model::{Address, Importance};

/// A file attached to an outgoing message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingAttachment {
    pub name: String,
    pub mime: String,
    pub data: Vec<u8>,
}

/// Everything needed to build the RFC 5322 message.
#[derive(Debug, Clone, PartialEq)]
pub struct Outgoing {
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    /// Never written to the headers; only part of the SMTP envelope.
    pub bcc: Vec<Address>,
    pub subject: String,
    pub text: String,
    /// HTML fragment or document.
    pub html: String,
    pub date: DateTime<Utc>,
    /// With angle brackets, e.g. `<abc@example.com>`.
    pub message_id: String,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub importance: Importance,
    pub read_receipt: bool,
    pub attachments: Vec<OutgoingAttachment>,
}

impl Outgoing {
    /// Envelope recipients (To + Cc + Bcc), deduplicated, lowercase.
    pub fn envelope(&self) -> Vec<String> {
        let mut emails: Vec<String> = Vec::new();
        for address in self.to.iter().chain(&self.cc).chain(&self.bcc) {
            let email = address.email.trim().to_lowercase();
            if !email.is_empty() && !emails.contains(&email) {
                emails.push(email);
            }
        }
        emails
    }
}

/// A fresh, unique `Message-ID` on the sender's domain.
pub fn new_message_id(from_email: &str, now: DateTime<Utc>) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let domain = from_email
        .rsplit_once('@')
        .map(|(_, domain)| domain)
        .filter(|domain| !domain.is_empty())
        .unwrap_or("mailbox.local");
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = now.timestamp_nanos_opt().unwrap_or_default();
    format!("<{nanos:x}.{count}.{:x}@{domain}>", std::process::id())
}

/// Builds the full message with CRLF line endings.
pub fn build_message(message: &Outgoing) -> String {
    let boundary_seed = message
        .message_id
        .bytes()
        .fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
    let boundary = |part: &str| format!("=_mbx_{part}_{boundary_seed:016x}");

    let mut out = String::new();
    header(&mut out, "From", &format_address(&message.from));
    address_header(&mut out, "To", &message.to);
    address_header(&mut out, "Cc", &message.cc);
    header(&mut out, "Subject", &encode_word(&message.subject));
    header(&mut out, "Date", &message.date.to_rfc2822());
    header(&mut out, "Message-ID", &message.message_id);
    if let Some(parent) = &message.in_reply_to {
        header(&mut out, "In-Reply-To", parent);
    }
    if !message.references.is_empty() {
        header(&mut out, "References", &message.references.join("\r\n "));
    }
    match message.importance {
        Importance::High => {
            header(&mut out, "Importance", "High");
            header(&mut out, "X-Priority", "1");
        }
        Importance::Low => {
            header(&mut out, "Importance", "Low");
            header(&mut out, "X-Priority", "5");
        }
        Importance::Normal => {}
    }
    if message.read_receipt {
        header(
            &mut out,
            "Disposition-Notification-To",
            &format_address(&message.from),
        );
    }
    header(&mut out, "MIME-Version", "1.0");
    header(&mut out, "X-Mailer", "Mailbox");

    let alternative = boundary("alt");
    let alternative_body = {
        let mut body = String::new();
        body.push_str(&format!("--{alternative}\r\n"));
        text_part(&mut body, "text/plain", &message.text);
        body.push_str(&format!("--{alternative}\r\n"));
        text_part(&mut body, "text/html", &message.html);
        body.push_str(&format!("--{alternative}--\r\n"));
        body
    };
    let alternative_type = format!("multipart/alternative; boundary=\"{alternative}\"");

    if message.attachments.is_empty() {
        header(&mut out, "Content-Type", &alternative_type);
        out.push_str("\r\n");
        out.push_str(&alternative_body);
        return out;
    }

    let mixed = boundary("mix");
    header(
        &mut out,
        "Content-Type",
        &format!("multipart/mixed; boundary=\"{mixed}\""),
    );
    out.push_str("\r\nThis is a multi-part message in MIME format.\r\n");
    out.push_str(&format!("--{mixed}\r\n"));
    header(&mut out, "Content-Type", &alternative_type);
    out.push_str("\r\n");
    out.push_str(&alternative_body);
    for attachment in &message.attachments {
        out.push_str(&format!("--{mixed}\r\n"));
        let filename = filename_parameter(&attachment.name);
        header(
            &mut out,
            "Content-Type",
            &format!("{}; name{}", attachment.mime, &filename[8..]),
        );
        header(&mut out, "Content-Transfer-Encoding", "base64");
        header(
            &mut out,
            "Content-Disposition",
            &format!("attachment; {filename}"),
        );
        out.push_str("\r\n");
        out.push_str(&base64_lines(&attachment.data));
    }
    out.push_str(&format!("--{mixed}--\r\n"));
    out
}

fn header(out: &mut String, name: &str, value: &str) {
    out.push_str(name);
    out.push_str(": ");
    out.push_str(value);
    out.push_str("\r\n");
}

fn address_header(out: &mut String, name: &str, addresses: &[Address]) {
    if addresses.is_empty() {
        return;
    }
    let formatted: Vec<String> = addresses.iter().map(format_address).collect();
    let single_line = formatted.join(", ");
    let value = if name.len() + 2 + single_line.len() <= 78 {
        single_line
    } else {
        formatted.join(",\r\n ")
    };
    header(out, name, &value);
}

fn text_part(out: &mut String, mime: &str, text: &str) {
    header(out, "Content-Type", &format!("{mime}; charset=utf-8"));
    header(out, "Content-Transfer-Encoding", "quoted-printable");
    out.push_str("\r\n");
    out.push_str(&quoted_printable(text));
    out.push_str("\r\n");
}

/// `Name <email>` header form: quoted when needed, RFC 2047 encoded when not ASCII.
pub fn format_address(address: &Address) -> String {
    let name = address.name.trim();
    if name.is_empty() {
        return address.email.clone();
    }
    let display = if !name.is_ascii() {
        encode_word(name)
    } else if name.contains(|c: char| "()<>[]:;@\\,.\"".contains(c)) {
        format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        name.to_owned()
    };
    format!("{display} <{}>", address.email)
}

/// RFC 2047 `=?UTF-8?B?...?=` words (folded) when `text` is not plain ASCII.
pub fn encode_word(text: &str) -> String {
    if text.is_ascii() && !text.contains("=?") && !text.contains(['\r', '\n']) {
        return text.to_owned();
    }
    // 45 bytes of UTF-8 -> 60 base64 chars, keeping each encoded word under 75 chars.
    let mut words = Vec::new();
    let mut chunk = String::new();
    for c in text.chars().filter(|c| *c != '\r' && *c != '\n') {
        if chunk.len() + c.len_utf8() > 45 {
            words.push(format!("=?UTF-8?B?{}?=", base64(chunk.as_bytes())));
            chunk.clear();
        }
        chunk.push(c);
    }
    if !chunk.is_empty() || words.is_empty() {
        words.push(format!("=?UTF-8?B?{}?=", base64(chunk.as_bytes())));
    }
    words.join("\r\n ")
}

/// `filename="a.pdf"` or RFC 2231 `filename*=UTF-8''%C3%A9.pdf` for non-ASCII names.
fn filename_parameter(name: &str) -> String {
    let name = name.replace(['\r', '\n'], "");
    if name.is_ascii() {
        format!(
            "filename=\"{}\"",
            name.replace('\\', "\\\\").replace('"', "\\\"")
        )
    } else {
        let encoded: String = name
            .bytes()
            .map(|byte| {
                if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                    (byte as char).to_string()
                } else {
                    format!("%{byte:02X}")
                }
            })
            .collect();
        format!("filename*=UTF-8''{encoded}")
    }
}

/// Quoted-printable (RFC 2045) with CRLF line breaks and soft breaks at 76 columns.
pub fn quoted_printable(text: &str) -> String {
    let normalized = text.replace("\r\n", "\n");
    let lines: Vec<String> = normalized
        .split('\n')
        .map(|line| {
            let bytes = line.as_bytes();
            let mut out = String::new();
            let mut column = 0;
            for (index, &byte) in bytes.iter().enumerate() {
                let is_last = index + 1 == bytes.len();
                let literal = ((byte == b' ' || byte == b'\t') && !is_last)
                    || ((33..=126).contains(&byte) && byte != b'=');
                let token = if literal {
                    (byte as char).to_string()
                } else {
                    format!("={byte:02X}")
                };
                // Keep room for the trailing `=` of a soft break.
                if column + token.len() > 75 {
                    out.push_str("=\r\n");
                    column = 0;
                }
                column += token.len();
                out.push_str(&token);
            }
            out
        })
        .collect();
    lines.join("\r\n")
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding.
pub fn base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let bytes = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let triple = (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2]);
        for position in 0..4 {
            if position <= chunk.len() {
                let index = (triple >> (18 - 6 * position)) & 0x3f;
                out.push(char::from(BASE64[index as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Base64 wrapped at 76 columns, each line ending with CRLF.
fn base64_lines(data: &[u8]) -> String {
    let encoded = base64(data);
    let mut out = String::with_capacity(encoded.len() + encoded.len() / 38 + 2);
    for line in encoded.as_bytes().chunks(76) {
        out.push_str(&String::from_utf8_lossy(line));
        out.push_str("\r\n");
    }
    out
}

/// MIME type from a file extension (common types; `application/octet-stream` otherwise).
pub fn guess_mime(name: &str) -> &'static str {
    let extension = name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "json" => "application/json",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ics" => "text/calendar",
        "vcf" => "text/vcard",
        "txt" | "log" | "md" => "text/plain",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn outgoing() -> Outgoing {
        Outgoing {
            from: Address::new("Ada Lovelace", "ada@example.com"),
            to: vec![Address::new("Bob", "bob@x.org")],
            cc: vec![Address::new("Doe, John", "john@d.com")],
            bcc: vec![
                Address::new("", "secret@y.org"),
                Address::new("", "BOB@x.org"),
            ],
            subject: "Café ☕ plans".into(),
            text: "Hello é\nLine two".into(),
            html: "<div>Hello é</div>".into(),
            date: Utc.with_ymd_and_hms(2026, 11, 14, 9, 30, 0).unwrap(),
            message_id: "<1.2@example.com>".into(),
            in_reply_to: Some("<parent@x.org>".into()),
            references: vec!["<parent@x.org>".into()],
            importance: Importance::High,
            read_receipt: true,
            attachments: vec![],
        }
    }

    #[test]
    fn builds_alternative_message() {
        let raw = build_message(&outgoing());
        assert!(raw.starts_with("From: Ada Lovelace <ada@example.com>\r\n"));
        assert!(raw.contains("To: Bob <bob@x.org>\r\n"));
        assert!(raw.contains("Cc: \"Doe, John\" <john@d.com>\r\n"));
        assert!(
            !raw.contains("secret@y.org"),
            "Bcc must not leak into headers"
        );
        assert!(raw.contains(&format!("Subject: {}\r\n", encode_word("Café ☕ plans"))));
        assert!(raw.contains("Date: Sat, 14 Nov 2026 09:30:00 +0000\r\n"));
        assert!(raw.contains("In-Reply-To: <parent@x.org>\r\n"));
        assert!(raw.contains("X-Priority: 1\r\n"));
        assert!(raw.contains("Disposition-Notification-To: Ada Lovelace <ada@example.com>\r\n"));
        assert!(raw.contains("Content-Type: multipart/alternative; boundary=\"=_mbx_alt_"));
        assert!(raw.contains("Content-Type: text/plain; charset=utf-8\r\n"));
        assert!(raw.contains("Hello =C3=A9\r\nLine two\r\n"));
        assert!(raw.trim_end().ends_with("--"));
        assert!(!raw.contains("multipart/mixed"));
        assert!(raw.lines().all(|line| line.len() <= 998));
        assert_eq!(
            outgoing().envelope(),
            vec!["bob@x.org", "john@d.com", "secret@y.org"]
        );
    }

    #[test]
    fn builds_mixed_message_with_attachments() {
        let mut message = outgoing();
        message.attachments = vec![
            OutgoingAttachment {
                name: "notes.txt".into(),
                mime: guess_mime("notes.txt").into(),
                data: b"hello".to_vec(),
            },
            OutgoingAttachment {
                name: "résumé.pdf".into(),
                mime: guess_mime("résumé.PDF").into(),
                data: vec![0; 100],
            },
        ];
        let raw = build_message(&message);
        assert!(raw.contains("Content-Type: multipart/mixed; boundary=\"=_mbx_mix_"));
        assert!(raw.contains("Content-Type: text/plain; name=\"notes.txt\"\r\n"));
        assert!(raw.contains("Content-Disposition: attachment; filename=\"notes.txt\"\r\n"));
        assert!(raw.contains("\r\naGVsbG8=\r\n"));
        assert!(raw.contains("application/pdf; name*=UTF-8''r%C3%A9sum%C3%A9.pdf"));
        assert!(raw.lines().all(|line| line.len() <= 78));
        // One opening + closing delimiter per part, and both multiparts are closed.
        assert_eq!(raw.matches("--=_mbx_mix_").count(), 4);
        assert_eq!(raw.matches("--=_mbx_alt_").count(), 3);
    }

    #[test]
    fn encodes_words_base64_and_quoted_printable() {
        assert_eq!(encode_word("Hello"), "Hello");
        assert_eq!(encode_word("é"), "=?UTF-8?B?w6k=?=");
        let long = encode_word(&"é".repeat(40));
        assert!(long.split("\r\n ").all(|word| word.len() <= 75));
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(quoted_printable("a=b \nend "), "a=3Db=20\r\nend=20");
        let wrapped = quoted_printable(&"x".repeat(200));
        assert!(wrapped.split("\r\n").all(|line| line.len() <= 76));
        assert_eq!(wrapped.replace("=\r\n", ""), "x".repeat(200));
        assert_eq!(
            format_address(&Address::new("Zoë", "z@x.io")),
            "=?UTF-8?B?Wm/Dqw==?= <z@x.io>"
        );
        assert!(new_message_id("a@b.co", Utc::now()).ends_with("@b.co>"));
        assert_ne!(
            new_message_id("a@b.co", Utc::now()),
            new_message_id("a@b.co", Utc::now())
        );
    }
}
