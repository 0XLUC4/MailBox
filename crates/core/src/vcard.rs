//! vCard (`.vcf`) import/export for [`ContactCard`]s.
//!
//! Reads vCard 2.1/3.0/4.0 (folded lines, escapes, `TYPE` params, base64 or `data:` photos,
//! quoted-printable values) and writes vCard 4.0 (RFC 6350) with CRLF and 75-octet folding.
//! Unknown properties are ignored.

use chrono::NaiveDate;

use crate::model::Id;
use crate::people::{ContactCard, PostalAddress, trim_labels};

/// Non-standard property carrying the favourite flag through export/import.
const FAVORITE_PROPERTY: &str = "X-MAILBOX-FAVORITE";

/// Parses every `BEGIN:VCARD`..`END:VCARD` block. Cards without a name or email are skipped.
pub fn parse_vcards(input: &str, account_id: Id) -> Vec<ContactCard> {
    let mut cards = Vec::new();
    let mut current: Option<ContactCard> = None;
    for line in unfold(input) {
        let Some(property) = Property::parse(&line) else {
            continue;
        };
        match property.name.as_str() {
            "BEGIN" if property.value.eq_ignore_ascii_case("VCARD") => {
                current = Some(ContactCard::new(account_id));
            }
            "END" if property.value.eq_ignore_ascii_case("VCARD") => {
                if let Some(mut card) = current.take() {
                    trim_labels(&mut card.details.email_labels);
                    trim_labels(&mut card.details.phone_labels);
                    let name = card.name();
                    if !name.is_empty() {
                        card.contact.display_name = name;
                        cards.push(card);
                    }
                }
            }
            _ => {
                if let Some(card) = current.as_mut() {
                    apply_property(card, &property);
                }
            }
        }
    }
    cards
}

fn apply_property(card: &mut ContactCard, property: &Property) {
    let (contact, details) = (&mut card.contact, &mut card.details);
    let text = || property.text();
    match property.name.as_str() {
        "FN" => contact.display_name = text(),
        "N" => {
            let parts = property.components(';');
            details.last_name = parts.first().cloned().unwrap_or_default();
            details.first_name = parts.get(1).cloned().unwrap_or_default();
        }
        "NICKNAME" => {
            details.nickname = property
                .components(',')
                .into_iter()
                .next()
                .unwrap_or_default();
        }
        "EMAIL" => {
            let value = text();
            let value = strip_scheme(&value, "mailto:").trim();
            if !value.is_empty() {
                details
                    .email_labels
                    .resize(contact.emails.len(), String::new());
                details.email_labels.push(property.label());
                contact.emails.push(value.to_owned());
            }
        }
        "TEL" => {
            let value = text();
            let value = strip_scheme(&value, "tel:").trim();
            if !value.is_empty() {
                let label = match property.label().as_str() {
                    "cell" => "mobile".to_owned(),
                    other => other.to_owned(),
                };
                details
                    .phone_labels
                    .resize(contact.phones.len(), String::new());
                details.phone_labels.push(label);
                contact.phones.push(value.to_owned());
            }
        }
        "ADR" => {
            let parts = property.components(';');
            let part = |index: usize| parts.get(index).cloned().unwrap_or_default();
            let street = [part(0), part(1), part(2)]
                .into_iter()
                .filter(|line| !line.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            let address = PostalAddress {
                label: property.label(),
                street,
                city: part(3),
                region: part(4),
                postal_code: part(5),
                country: part(6),
            };
            if !address.is_empty() {
                details.addresses.push(address);
            }
        }
        "ORG" => {
            contact.company = property
                .components(';')
                .into_iter()
                .next()
                .unwrap_or_default()
        }
        "TITLE" => contact.job_title = text(),
        "NOTE" => {
            let note = text();
            contact.notes = if contact.notes.is_empty() {
                note
            } else {
                format!("{}\n{note}", contact.notes)
            };
        }
        "BDAY" => details.birthday = parse_date(&property.value),
        "URL" if details.website.is_empty() => details.website = text(),
        "PHOTO" => details.photo = property.photo().or(details.photo.take()),
        "CATEGORIES" => {
            for category in property.components(',') {
                let category = category.trim();
                if !category.is_empty() && !details.categories.iter().any(|c| c == category) {
                    details.categories.push(category.to_owned());
                }
            }
        }
        FAVORITE_PROPERTY => contact.favorite = text().eq_ignore_ascii_case("true"),
        _ => {}
    }
}

fn strip_scheme<'a>(value: &'a str, scheme: &str) -> &'a str {
    match value.get(..scheme.len()) {
        Some(prefix) if prefix.eq_ignore_ascii_case(scheme) => &value[scheme.len()..],
        _ => value,
    }
}

/// `1815-12-10`, `18151210`, `1815-12-10T00:00:00Z`. Year-less dates (`--1210`) are dropped.
fn parse_date(value: &str) -> Option<NaiveDate> {
    let date = value.trim().split('T').next().unwrap_or_default();
    NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .or_else(|_| NaiveDate::parse_from_str(date, "%Y%m%d"))
        .ok()
}

/// Joins folded lines (a line starting with a space or tab continues the previous one).
fn unfold(input: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw in input.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        if let (Some(rest), Some(last)) = (raw.strip_prefix([' ', '\t']), lines.last_mut()) {
            last.push_str(rest);
        } else if !raw.trim().is_empty() {
            lines.push(raw.to_owned());
        }
    }
    lines
}

/// One content line: `[group.]NAME[;PARAM=VALUE...]:VALUE`.
struct Property {
    /// Upper-case, without group.
    name: String,
    /// Upper-case keys; a bare v2.1 param (`;WORK`) becomes `TYPE=WORK`.
    params: Vec<(String, String)>,
    value: String,
}

impl Property {
    fn parse(line: &str) -> Option<Self> {
        let mut in_quotes = false;
        let colon = line.char_indices().find_map(|(index, c)| {
            if c == '"' {
                in_quotes = !in_quotes;
            }
            (c == ':' && !in_quotes).then_some(index)
        })?;
        let (head, value) = (&line[..colon], &line[colon + 1..]);
        let mut parts = head.split(';');
        let name = parts
            .next()?
            .rsplit('.')
            .next()?
            .trim()
            .to_ascii_uppercase();
        if name.is_empty() {
            return None;
        }
        let params = parts
            .map(|param| match param.split_once('=') {
                Some((key, value)) => (
                    key.trim().to_ascii_uppercase(),
                    value.trim().trim_matches('"').to_owned(),
                ),
                None => ("TYPE".to_owned(), param.trim().to_owned()),
            })
            .collect();
        let mut property = Self {
            name,
            params,
            value: value.to_owned(),
        };
        if property
            .param("ENCODING")
            .is_some_and(|encoding| encoding.eq_ignore_ascii_case("QUOTED-PRINTABLE"))
        {
            property.value = decode_quoted_printable(&property.value);
        }
        Some(property)
    }

    fn param(&self, key: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    /// Lower-case `TYPE` values (`TYPE=work,voice;TYPE=pref` -> work, voice, pref).
    fn types(&self) -> Vec<String> {
        self.params
            .iter()
            .filter(|(key, _)| key == "TYPE")
            .flat_map(|(_, value)| value.split(','))
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| !value.is_empty())
            .collect()
    }

    /// First meaningful `TYPE` ("home", "work"...), for address labels.
    fn label(&self) -> String {
        const IGNORED: &[&str] = &[
            "pref", "postal", "parcel", "intl", "dom", "internet", "voice",
        ];
        self.types()
            .into_iter()
            .find(|value| !IGNORED.contains(&value.as_str()))
            .unwrap_or_default()
    }

    fn text(&self) -> String {
        unescape(&self.value)
    }

    /// Splits on unescaped `separator`, then unescapes each component.
    fn components(&self, separator: char) -> Vec<String> {
        let mut parts = Vec::new();
        let mut current = String::new();
        let mut chars = self.value.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                current.push(c);
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            } else if c == separator {
                parts.push(unescape(&current));
                current.clear();
            } else {
                current.push(c);
            }
        }
        parts.push(unescape(&current));
        parts
    }

    /// Photo as a `data:` URI: v4 data URIs as-is, v3 inline base64 converted. URLs are ignored.
    fn photo(&self) -> Option<String> {
        let value = self.value.trim();
        if value.starts_with("data:image/") {
            return Some(value.to_owned());
        }
        let encoding = self.param("ENCODING").unwrap_or_default();
        if !(encoding.eq_ignore_ascii_case("b") || encoding.eq_ignore_ascii_case("base64")) {
            return None;
        }
        let kind = self
            .types()
            .into_iter()
            .next()
            .unwrap_or_else(|| "jpeg".into());
        let kind = kind.trim_start_matches("image/");
        let data: String = value.chars().filter(|c| !c.is_whitespace()).collect();
        (!data.is_empty()).then(|| format!("data:image/{kind};base64,{data}"))
    }
}

fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n' | 'N') => out.push('\n'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn decode_quoted_printable(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let hex = bytes
            .get(index + 1..index + 3)
            .and_then(|pair| std::str::from_utf8(pair).ok())
            .and_then(|pair| u8::from_str_radix(pair, 16).ok());
        match (bytes[index], hex) {
            (b'=', Some(byte)) => {
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

// ---------- writer ----------

/// Serializes cards as vCard 4.0 (CRLF line endings).
pub fn write_vcards(cards: &[ContactCard]) -> String {
    cards.iter().map(write_vcard).collect()
}

pub fn write_vcard(card: &ContactCard) -> String {
    let (contact, details) = (&card.contact, &card.details);
    let mut lines = vec![
        "BEGIN:VCARD".to_owned(),
        "VERSION:4.0".to_owned(),
        format!("FN:{}", escape(&card.name())),
        format!(
            "N:{};{};;;",
            escape(&details.last_name),
            escape(&details.first_name)
        ),
    ];
    let mut push = |name: &str, value: &str| {
        if !value.trim().is_empty() {
            lines.push(format!("{name}:{}", escape(value)));
        }
    };
    push("NICKNAME", &details.nickname);
    for (index, email) in contact.emails.iter().enumerate() {
        push(
            &format!("EMAIL{}", type_param(details.email_label(index))),
            email,
        );
    }
    for (index, phone) in contact.phones.iter().enumerate() {
        let label = match details.phone_label(index) {
            "mobile" => "cell",
            other => other,
        };
        push(&format!("TEL{}", type_param(label)), phone);
    }
    push("ORG", &contact.company);
    push("TITLE", &contact.job_title);
    push("URL", &details.website);
    push("NOTE", &contact.notes);
    for address in &details.addresses {
        let params = type_param(&address.label);
        lines.push(format!(
            "ADR{params}:;;{};{};{};{};{}",
            escape(&address.street),
            escape(&address.city),
            escape(&address.region),
            escape(&address.postal_code),
            escape(&address.country)
        ));
    }
    if let Some(birthday) = details.birthday {
        lines.push(format!("BDAY:{}", birthday.format("%Y%m%d")));
    }
    if let Some(photo) = details
        .photo
        .as_deref()
        .filter(|photo| photo.starts_with("data:"))
    {
        lines.push(format!("PHOTO:{photo}"));
    }
    if !details.categories.is_empty() {
        let categories: Vec<String> = details.categories.iter().map(|c| escape(c)).collect();
        lines.push(format!("CATEGORIES:{}", categories.join(",")));
    }
    if contact.favorite {
        lines.push(format!("{FAVORITE_PROPERTY}:TRUE"));
    }
    lines.push("END:VCARD".to_owned());

    let mut out = String::new();
    for line in &lines {
        out.push_str(&fold(line));
        out.push_str("\r\n");
    }
    out
}

/// `;TYPE=work` for a label (ASCII letters and digits kept), empty for none.
fn type_param(label: &str) -> String {
    let label: String = label.chars().filter(char::is_ascii_alphanumeric).collect();
    if label.is_empty() {
        String::new()
    } else {
        format!(";TYPE={}", label.to_ascii_lowercase())
    }
}

fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            ',' => out.push_str("\\,"),
            ';' => out.push_str("\\;"),
            other => out.push(other),
        }
    }
    out
}

/// Splits a line into chunks of at most 75 octets (continuations start with a space).
fn fold(line: &str) -> String {
    const LIMIT: usize = 75;
    let mut out = String::with_capacity(line.len() + line.len() / LIMIT * 3);
    let mut width = 0;
    for c in line.chars() {
        if width + c.len_utf8() > LIMIT {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(c);
        width += c.len_utf8();
    }
    out
}

// ---------- photos ----------

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let byte = |index: usize| u32::from(chunk.get(index).copied().unwrap_or(0));
        let group = (byte(0) << 16) | (byte(1) << 8) | byte(2);
        for index in 0..4 {
            if index <= chunk.len() {
                out.push(char::from(
                    BASE64[(group >> (18 - 6 * index)) as usize & 63],
                ));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Standard or URL-safe base64; whitespace is ignored. `None` on invalid input.
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut buffer, mut bits) = (0u32, 0u32);
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b' ' | b'\t' | b'\r' | b'\n' => continue,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// `data:<mime>;base64,<data>`.
pub fn photo_data_uri(mime: &str, bytes: &[u8]) -> String {
    format!("data:{mime};base64,{}", base64_encode(bytes))
}

/// Splits a base64 `data:` URI into its MIME type and bytes.
pub fn parse_data_uri(uri: &str) -> Option<(String, Vec<u8>)> {
    let (meta, data) = uri.strip_prefix("data:")?.split_once(',')?;
    let mime = meta.strip_suffix(";base64")?;
    Some((mime.to_ascii_lowercase(), base64_decode(data)?))
}

/// MIME type for an image file extension (`png`, `jpg`...).
pub fn image_mime(extension: &str) -> Option<&'static str> {
    match extension.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::people::ContactDetails;

    const V3: &str = "BEGIN:VCARD\r\n\
VERSION:3.0\r\n\
N:Lovelace;Ada;;Countess;\r\n\
FN:Ada Lovelace\r\n\
NICKNAME:Ada,Countess\r\n\
ORG:Analytical Engines\\, Ltd;Research\r\n\
TITLE:Mathematician\r\n\
item1.EMAIL;TYPE=INTERNET,WORK:ada@example.com\r\n\
EMAIL;type=HOME:mailto:ada@home.example\r\n\
TEL;TYPE=CELL:+44 20 1234 5678\r\n\
ADR;TYPE=HOME,POSTAL:;;12 St James's Square;London;;SW1Y 4JH;United Kingdom\r\n\
BDAY:1815-12-10\r\n\
NOTE:First line\\nsecond line\\; with semicolon and a very long tail that must be \r\n \
 folded across lines\r\n\
PHOTO;ENCODING=b;TYPE=JPEG:/9j/4AAQ\r\n\
CATEGORIES:Friends,VIP\r\n\
END:VCARD\r\n\
BEGIN:VCARD\r\n\
VERSION:3.0\r\n\
FN:Charles Babbage\r\n\
TEL:+44 1\r\n\
END:VCARD\r\n\
BEGIN:VCARD\r\n\
VERSION:3.0\r\n\
END:VCARD\r\n";

    #[test]
    fn parses_vcard_3() {
        let cards = parse_vcards(V3, 7);
        assert_eq!(cards.len(), 2, "the empty card is skipped");
        let ada = &cards[0];
        assert_eq!(ada.contact.account_id, 7);
        assert_eq!(ada.contact.display_name, "Ada Lovelace");
        assert_eq!(ada.details.first_name, "Ada");
        assert_eq!(ada.details.last_name, "Lovelace");
        assert_eq!(ada.details.nickname, "Ada");
        assert_eq!(ada.contact.company, "Analytical Engines, Ltd");
        assert_eq!(ada.contact.job_title, "Mathematician");
        assert_eq!(
            ada.contact.emails,
            vec!["ada@example.com", "ada@home.example"]
        );
        assert_eq!(ada.contact.phones, vec!["+44 20 1234 5678"]);
        assert_eq!(ada.details.phone_labels, vec!["mobile"]);
        assert_eq!(ada.details.email_labels, vec!["work", "home"]);
        assert_eq!(
            ada.details.addresses,
            vec![PostalAddress {
                label: "home".into(),
                street: "12 St James's Square".into(),
                city: "London".into(),
                region: String::new(),
                postal_code: "SW1Y 4JH".into(),
                country: "United Kingdom".into(),
            }]
        );
        assert_eq!(ada.details.birthday, NaiveDate::from_ymd_opt(1815, 12, 10));
        assert_eq!(
            ada.contact.notes,
            "First line\nsecond line; with semicolon and a very long tail that must be folded across lines"
        );
        assert_eq!(
            ada.details.photo.as_deref(),
            Some("data:image/jpeg;base64,/9j/4AAQ")
        );
        assert_eq!(ada.details.categories, vec!["Friends", "VIP"]);
        assert_eq!(cards[1].contact.display_name, "Charles Babbage");
    }

    #[test]
    fn parses_vcard_4_and_lf_endings() {
        let input = "BEGIN:VCARD\nVERSION:4.0\nFN:Grace Hopper\nEMAIL;PREF=1:grace@navy.example\n\
TEL;VALUE=uri;TYPE=\"voice,cell\":tel:+1-555-0100\nBDAY:19061209\n\
PHOTO:data:image/png;base64,iVBORw0KGgo=\nX-MAILBOX-FAVORITE:TRUE\nEND:VCARD\n";
        let cards = parse_vcards(input, 1);
        assert_eq!(cards.len(), 1);
        let grace = &cards[0];
        assert_eq!(grace.contact.emails, vec!["grace@navy.example"]);
        assert_eq!(grace.contact.phones, vec!["+1-555-0100"]);
        assert_eq!(grace.details.birthday, NaiveDate::from_ymd_opt(1906, 12, 9));
        assert_eq!(
            grace.details.photo.as_deref(),
            Some("data:image/png;base64,iVBORw0KGgo=")
        );
        assert!(grace.contact.favorite);
    }

    #[test]
    fn parses_vcard_2_1_quoted_printable() {
        let input = "BEGIN:VCARD\r\nVERSION:2.1\r\nN;CHARSET=UTF-8;ENCODING=QUOTED-PRINTABLE:Durand;=C3=89ric\r\n\
TEL;WORK;VOICE:0102030405\r\nEND:VCARD\r\n";
        let cards = parse_vcards(input, 1);
        assert_eq!(cards[0].details.first_name, "Éric");
        assert_eq!(cards[0].contact.display_name, "Éric Durand");
        assert_eq!(cards[0].contact.phones, vec!["0102030405"]);
    }

    #[test]
    fn write_then_parse_roundtrips() {
        let mut card = ContactCard::new(3);
        card.contact.display_name = "Zoë O'Brien; PhD".into();
        card.contact.emails = vec!["zoe@example.com".into(), "z@work.example".into()];
        card.contact.phones = vec!["+353 1 234".into()];
        card.contact.company = "Acme, Inc.".into();
        card.contact.job_title = "CTO".into();
        card.contact.notes = "Line one\nLine two with a backslash \\ and ünïcödé ".repeat(4);
        card.contact.favorite = true;
        card.details = ContactDetails {
            id: 0,
            first_name: "Zoë".into(),
            last_name: "O'Brien".into(),
            nickname: "Z".into(),
            birthday: NaiveDate::from_ymd_opt(1990, 2, 28),
            website: "https://zoe.example/a,b".into(),
            photo: Some(photo_data_uri("image/png", &[0, 1, 2, 250, 251, 252, 253])),
            addresses: vec![PostalAddress {
                label: "work".into(),
                street: "1 Main St; Floor 2".into(),
                city: "Dublin".into(),
                region: "Leinster".into(),
                postal_code: "D01".into(),
                country: "Ireland".into(),
            }],
            categories: vec!["Work".into(), "A,B".into()],
            email_labels: vec!["home".into(), "work".into()],
            phone_labels: vec!["mobile".into()],
        };
        let text = write_vcards(std::slice::from_ref(&card));
        assert!(text.starts_with("BEGIN:VCARD\r\nVERSION:4.0\r\n"));
        assert!(text.ends_with("END:VCARD\r\n"));
        assert!(text.split("\r\n").all(|line| line.len() <= 75));

        let parsed = parse_vcards(&text, 3);
        assert_eq!(parsed, vec![card]);
    }

    #[test]
    fn base64_roundtrip_and_data_uri() {
        let inputs: [&[u8]; 7] = [b"", b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar"];
        for input in inputs {
            assert_eq!(base64_decode(&base64_encode(input)).as_deref(), Some(input));
        }
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_decode("Zm9v\r\n YmE="), Some(b"fooba".to_vec()));
        assert_eq!(base64_decode("Zm9v!"), None);
        let uri = photo_data_uri("image/png", b"png!");
        assert_eq!(
            parse_data_uri(&uri),
            Some(("image/png".to_owned(), b"png!".to_vec()))
        );
        assert_eq!(parse_data_uri("https://example.com/a.png"), None);
        assert_eq!(image_mime("JPG"), Some("image/jpeg"));
    }
}
