//! Export helpers with no I/O: printable HTML for a message, safe file names for `.eml` /
//! `.mbox` exports. (The `.eml`/`.mbox` encoders live in `mailbox_sync::formats`.)

use chrono::Local;

use crate::dates::LocalizedFormat;
use crate::settings::Language;
use crate::{Address, Message};

/// Escapes text for HTML element content and attribute values.
pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn address_list(list: &[Address]) -> String {
    list.iter()
        .map(|address| {
            if address.name.is_empty() {
                address.email.clone()
            } else {
                format!("{} <{}>", address.name, address.email)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// A standalone HTML page for printing `message`. `body_text` is the plain text to show
/// (the message's HTML is never embedded: it may carry scripts or remote content). The page
/// opens the print dialog when loaded. The date is written in `language`.
pub fn print_html(message: &Message, body_text: &str, language: Language) -> String {
    let mut rows = vec![
        ("From", address_list(std::slice::from_ref(&message.from))),
        ("To", address_list(&message.to)),
    ];
    if !message.cc.is_empty() {
        rows.push(("Cc", address_list(&message.cc)));
    }
    rows.push((
        "Date",
        message
            .date
            .with_timezone(&Local)
            .format_in(language, "%A %d %B %Y, %H:%M"),
    ));
    if !message.attachments.is_empty() {
        let names: Vec<_> = message
            .attachments
            .iter()
            .map(|a| a.name.as_str())
            .collect();
        rows.push(("Attachments", names.join(", ")));
    }
    let header: String = rows
        .iter()
        .map(|(label, value)| format!("<tr><th>{label}</th><td>{}</td></tr>", escape_html(value)))
        .collect();
    let subject = if message.subject.is_empty() {
        "(no subject)".to_owned()
    } else {
        escape_html(&message.subject)
    };
    format!(
        "<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>{subject}</title>\
<style>body{{font:14px/1.5 system-ui,sans-serif;margin:32px;color:#111}}\
h1{{font-size:20px;margin:0 0 12px}}table{{border-collapse:collapse;margin-bottom:16px}}\
th{{text-align:left;padding:2px 12px 2px 0;color:#555;font-weight:600;vertical-align:top}}\
pre{{white-space:pre-wrap;font:inherit;border-top:1px solid #ccc;padding-top:16px}}</style>\
</head><body onload=\"window.print()\"><h1>{subject}</h1><table>{header}</table>\
<pre>{}</pre></body></html>\n",
        escape_html(body_text)
    )
}

/// A file name (no separators or reserved characters) from free text, with `extension`.
pub fn file_name(text: &str, extension: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| {
            if c.is_control() || r#"<>:"/\|?*"#.contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned: String = cleaned.trim().trim_matches('.').chars().take(80).collect();
    let stem = if cleaned.trim().is_empty() {
        "message".to_owned()
    } else {
        cleaned.trim().to_owned()
    };
    format!("{stem}.{extension}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Attachment, Importance, MessageFlags};
    use chrono::Utc;

    fn message() -> Message {
        Message {
            id: 1,
            account_id: 1,
            folder_id: 1,
            thread_id: String::new(),
            from: Address::new("Ada <x>", "ada@x.org"),
            to: vec![Address::new("", "me@x.org")],
            cc: vec![],
            subject: "Q3 <plan> & budget".into(),
            date: Utc::now(),
            preview: String::new(),
            body_text: String::new(),
            body_html: None,
            flags: MessageFlags::default(),
            importance: Importance::Normal,
            categories: vec![],
            attachments: vec![Attachment {
                name: "a.pdf".into(),
                mime: "application/pdf".into(),
                size: 1,
            }],
        }
    }

    #[test]
    fn print_page_escapes_everything() {
        let page = print_html(&message(), "<script>alert(1)</script>", Language::En);
        assert!(page.contains("Q3 &lt;plan&gt; &amp; budget"));
        assert!(page.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!page.contains("<script>"));
        assert!(page.contains("Ada &lt;x&gt; &lt;ada@x.org&gt;"));
        assert!(page.contains("a.pdf"));
    }

    #[test]
    fn file_names_are_safe() {
        assert_eq!(file_name("Re: a/b?", "eml"), "Re_ a_b_.eml");
        assert_eq!(file_name("  ..  ", "mbox"), "message.mbox");
        assert_eq!(file_name("Inbox", "mbox"), "Inbox.mbox");
    }
}
