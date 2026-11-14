//! Recipient parsing: `Name <a@b.c>` lists, validation, `mailto:` URLs.

use crate::model::Address;

/// Splits a recipients line on `,` `;` or newlines (outside quotes and `<...>`) into addresses.
///
/// Entries are parsed best-effort: `Name <email>`, `"Doe, John" <j@d.com>` or a bare email.
/// Use [`is_valid_email`] to flag entries that are not addresses.
pub fn parse_recipients(input: &str) -> Vec<Address> {
    let mut entries = Vec::new();
    let mut current = String::new();
    let (mut in_quotes, mut in_angle, mut escaped) = (false, false, false);
    for c in input.chars() {
        if escaped {
            current.push(c);
            escaped = false;
            continue;
        }
        match c {
            '\\' if in_quotes => {
                escaped = true;
                current.push(c);
            }
            '"' => {
                in_quotes = !in_quotes;
                current.push(c);
            }
            '<' if !in_quotes => {
                in_angle = true;
                current.push(c);
            }
            '>' if !in_quotes => {
                in_angle = false;
                current.push(c);
            }
            ',' | ';' | '\n' | '\r' if !in_quotes && !in_angle => {
                entries.extend(parse_address(&current));
                current.clear();
            }
            _ => current.push(c),
        }
    }
    entries.extend(parse_address(&current));
    entries
}

/// Parses one entry; `None` when it is blank.
pub fn parse_address(entry: &str) -> Option<Address> {
    let entry = entry.trim();
    if entry.is_empty() {
        return None;
    }
    if let (Some(open), true) = (entry.rfind('<'), entry.ends_with('>')) {
        let email = entry[open + 1..entry.len() - 1].trim();
        let name = unquote(entry[..open].trim());
        return Some(Address::new(name, strip_mailto(email)));
    }
    Some(Address::new("", strip_mailto(entry)))
}

fn strip_mailto(email: &str) -> String {
    let lower = email.to_ascii_lowercase();
    if lower.starts_with("mailto:") {
        email[7..].trim().to_owned()
    } else {
        email.to_owned()
    }
}

fn unquote(name: &str) -> String {
    let inner = name
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(name);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out.trim().to_owned()
}

/// Pragmatic email check: `local@domain.tld`, no spaces, sane dots.
pub fn is_valid_email(email: &str) -> bool {
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    let local_ok = !local.is_empty()
        && local.len() <= 64
        && !local.starts_with('.')
        && !local.ends_with('.')
        && local
            .chars()
            .all(|c| c.is_alphanumeric() || "!#$%&'*+-/=?^_`{|}~.".contains(c));
    let labels: Vec<&str> = domain.split('.').collect();
    let domain_ok = labels.len() >= 2
        && labels.iter().all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_alphanumeric() || c == '-')
        });
    local_ok && domain_ok
}

/// `Name <email>` for display and editing, or just the email when there is no name.
pub fn display_recipient(address: &Address) -> String {
    if address.name.is_empty() {
        address.email.clone()
    } else {
        format!("{} <{}>", address.name, address.email)
    }
}

/// Fields of a `mailto:` URL (RFC 6068).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mailto {
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub subject: String,
    pub body: String,
}

/// Parses `mailto:a@b.c,d@e.f?subject=Hi&cc=x@y.z&body=Hello%20there`.
pub fn parse_mailto(url: &str) -> Mailto {
    let url = url.trim();
    let rest = if url.len() >= 7 && url[..7].eq_ignore_ascii_case("mailto:") {
        &url[7..]
    } else {
        url
    };
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let mut mailto = Mailto {
        to: parse_recipients(&percent_decode(path)),
        ..Default::default()
    };
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let value = percent_decode(value);
        match key.to_ascii_lowercase().as_str() {
            "to" => mailto.to.extend(parse_recipients(&value)),
            "cc" => mailto.cc.extend(parse_recipients(&value)),
            "bcc" => mailto.bcc.extend(parse_recipients(&value)),
            "subject" => mailto.subject = value,
            "body" => mailto.body = value.replace("\r\n", "\n"),
            _ => {}
        }
    }
    mailto
}

/// Decodes `%XX` escapes (and keeps `+` literal, as RFC 6068 requires).
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let hex = bytes
            .get(index + 1..index + 3)
            .and_then(|pair| std::str::from_utf8(pair).ok())
            .and_then(|pair| u8::from_str_radix(pair, 16).ok());
        match (bytes[index], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                index += 3;
            }
            (byte, _) => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lists_with_names_quotes_and_separators() {
        let parsed = parse_recipients(
            r#"Ada Lovelace <ada@example.com>; bob@x.org, "Doe, John" <j@d.com>,"#,
        );
        assert_eq!(
            parsed,
            vec![
                Address::new("Ada Lovelace", "ada@example.com"),
                Address::new("", "bob@x.org"),
                Address::new("Doe, John", "j@d.com"),
            ]
        );
        assert_eq!(
            parse_recipients(r#""Al \"The Pal\"" <al@x.io>"#),
            vec![Address::new(r#"Al "The Pal""#, "al@x.io")]
        );
        assert!(parse_recipients(" , ; ").is_empty());
        assert_eq!(
            parse_address("mailto:me@x.io"),
            Some(Address::new("", "me@x.io"))
        );
    }

    #[test]
    fn validates_emails() {
        for good in ["a@b.co", "first.last+tag@sub.example.org", "é@exemple.fr"] {
            assert!(is_valid_email(good), "{good}");
        }
        for bad in [
            "", "a", "a@b", "@b.co", "a@.co", "a b@c.de", "a@b..co", ".a@b.co", "a@-b.co",
        ] {
            assert!(!is_valid_email(bad), "{bad}");
        }
    }

    #[test]
    fn parses_mailto_urls() {
        let mailto = parse_mailto(
            "mailto:ada@example.com,bob@x.org?subject=Hello%20there&cc=c@d.ef&body=Line%201%0D%0ALine+2",
        );
        assert_eq!(mailto.to.len(), 2);
        assert_eq!(mailto.cc, vec![Address::new("", "c@d.ef")]);
        assert_eq!(mailto.subject, "Hello there");
        assert_eq!(mailto.body, "Line 1\nLine+2");
        assert_eq!(display_recipient(&mailto.to[0]), "ada@example.com");
        assert_eq!(
            display_recipient(&Address::new("Ada", "a@b.co")),
            "Ada <a@b.co>"
        );
    }
}
