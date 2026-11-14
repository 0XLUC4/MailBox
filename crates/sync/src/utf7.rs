//! IMAP modified UTF-7 mailbox names (RFC 3501 §5.1.3): `Messages envoy&AOk-s` is
//! "Messages envoyés", `&-` is a literal `&`.
//!
//! Server paths are kept exactly as the server sent them (they are the keys of the sync state
//! and are sent back unchanged in commands); only display names are decoded. [`encode`] builds
//! a server path from a name typed by the user.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+,";

/// Decodes a mailbox name. Invalid sequences are kept as they are (never fails).
pub fn decode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut rest = name;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('-') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let encoded = &after[..end];
        match decode_run(encoded) {
            Some(text) => out.push_str(&text),
            None => out.push_str(&rest[start..start + end + 2]),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// `""` is `&-` (a literal `&`); otherwise modified base64 of UTF-16BE.
fn decode_run(encoded: &str) -> Option<String> {
    if encoded.is_empty() {
        return Some("&".to_owned());
    }
    let mut bytes = Vec::with_capacity(encoded.len() * 3 / 4);
    let (mut buffer, mut bits) = (0u32, 0u32);
    for byte in encoded.bytes() {
        let value = ALPHABET.iter().position(|&c| c == byte)? as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push((buffer >> bits) as u8);
        }
    }
    // Leftover bits must be zero padding; UTF-16 needs whole 16-bit units.
    if buffer & ((1 << bits) - 1) != 0 || bytes.len() % 2 != 0 {
        return None;
    }
    let units = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_be_bytes(*pair));
    let text = char::decode_utf16(units)
        .collect::<Result<String, _>>()
        .ok()?;
    // A run that only holds printable ASCII is not valid modified UTF-7.
    (!text.chars().all(|c| (' '..='~').contains(&c))).then_some(text)
}

/// Encodes a mailbox name for the server.
pub fn encode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut run: Vec<u16> = Vec::new();
    for c in name.chars() {
        if (' '..='~').contains(&c) {
            flush_run(&mut run, &mut out);
            if c == '&' {
                out.push_str("&-");
            } else {
                out.push(c);
            }
        } else {
            let mut units = [0u16; 2];
            run.extend_from_slice(c.encode_utf16(&mut units));
        }
    }
    flush_run(&mut run, &mut out);
    out
}

fn flush_run(run: &mut Vec<u16>, out: &mut String) {
    if run.is_empty() {
        return;
    }
    let bytes: Vec<u8> = run.drain(..).flat_map(u16::to_be_bytes).collect();
    out.push('&');
    for chunk in bytes.chunks(3) {
        let buffer = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, &b)| acc | (u32::from(b) << (16 - 8 * i)));
        for i in 0..=chunk.len() {
            out.push(char::from(
                ALPHABET[((buffer >> (18 - 6 * i)) & 0x3f) as usize],
            ));
        }
    }
    out.push('-');
}

#[cfg(test)]
mod tests {
    use super::*;

    const CASES: &[(&str, &str)] = &[
        ("INBOX", "INBOX"),
        ("Messages envoy&AOk-s", "Messages envoyés"),
        ("[Gmail]/Messages envoy&AOk-s", "[Gmail]/Messages envoyés"),
        ("&AMk-l&AOk-ments supprim&AOk-s", "Éléments supprimés"),
        ("Tom &- Jerry", "Tom & Jerry"),
        ("&-&-", "&&"),
        // RFC 3501 example.
        ("~peter/mail/&U,BTFw-/&ZeVnLIqe-", "~peter/mail/台北/日本語"),
        ("&ZeVnLIqe-", "日本語"),
        ("Entw&APw-rfe", "Entwürfe"),
        ("&2D3eAA-", "😀"),
    ];

    #[test]
    fn decodes_and_encodes_rfc_3501_names() {
        for (wire, text) in CASES {
            assert_eq!(decode(wire), *text, "decode {wire}");
            assert_eq!(encode(text), *wire, "encode {text}");
            assert_eq!(decode(&encode(text)), *text);
        }
    }

    #[test]
    fn invalid_sequences_are_kept() {
        for name in [
            "R&D", "R&D-team", "a&", "&AOk", "&!!-",
            "&AEE-", // "A": ASCII must not be encoded
            "&2D0-", // lone surrogate
        ] {
            assert_eq!(decode(name), name);
        }
    }
}
