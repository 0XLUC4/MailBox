//! SMTP submission subset: EHLO/HELO, AUTH PLAIN/LOGIN (anything accepted), MAIL, RCPT,
//! DATA (dot-unstuffed), RSET, NOOP, VRFY, QUIT.

use std::io::{self, BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;

use crate::{SharedStore, lock};

const TIMEOUT: Duration = Duration::from_secs(5 * 60);

pub(crate) fn serve(stream: TcpStream, store: &SharedStore, verbose: bool) -> io::Result<()> {
    stream.set_read_timeout(Some(TIMEOUT))?;
    let mut out = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let reply = |out: &mut TcpStream, line: &str| -> io::Result<()> {
        if verbose {
            eprintln!("smtp S: {line}");
        }
        out.write_all(line.as_bytes())?;
        out.write_all(b"\r\n")
    };
    reply(&mut out, "220 mailbox-devserver ESMTP ready")?;

    let mut from: Option<String> = None;
    let mut recipients: Vec<String> = Vec::new();
    loop {
        let Some(line) = read_line(&mut reader)? else {
            return Ok(());
        };
        if verbose {
            eprintln!("smtp C: {line}");
        }
        let upper = line.to_ascii_uppercase();
        let verb = upper.split_whitespace().next().unwrap_or_default();
        match verb {
            "EHLO" => {
                for line in [
                    "250-mailbox-devserver",
                    "250-AUTH PLAIN LOGIN",
                    "250-8BITMIME",
                    "250-SMTPUTF8",
                    "250 SIZE 52428800",
                ] {
                    reply(&mut out, line)?;
                }
            }
            "HELO" => reply(&mut out, "250 mailbox-devserver")?,
            "AUTH" => {
                let words: Vec<&str> = upper.split_whitespace().collect();
                // PLAIN without initial response: one challenge; LOGIN: user, then password.
                let prompts: &[&str] = match (words.get(1).copied(), words.len()) {
                    (Some("PLAIN"), 2) => &["334 "],
                    (Some("LOGIN"), 2) => &["334 VXNlcm5hbWU6", "334 UGFzc3dvcmQ6"],
                    (Some("LOGIN"), _) => &["334 UGFzc3dvcmQ6"],
                    _ => &[],
                };
                for prompt in prompts {
                    reply(&mut out, prompt)?;
                    if read_line(&mut reader)?.is_none() {
                        return Ok(());
                    }
                }
                reply(&mut out, "235 2.7.0 Authentication successful")?;
            }
            "MAIL" => {
                from = Some(address(&line));
                recipients.clear();
                reply(&mut out, "250 2.1.0 OK")?;
            }
            "RCPT" => {
                if from.is_none() {
                    reply(&mut out, "503 5.5.1 MAIL first")?;
                } else {
                    recipients.push(address(&line));
                    reply(&mut out, "250 2.1.5 OK")?;
                }
            }
            "DATA" => {
                if recipients.is_empty() {
                    reply(&mut out, "503 5.5.1 RCPT first")?;
                    continue;
                }
                reply(&mut out, "354 End data with <CR><LF>.<CR><LF>")?;
                let data = read_data(&mut reader)?;
                let sender = from.take().unwrap_or_default();
                let count = recipients.len();
                lock(store).deliver(&sender, &std::mem::take(&mut recipients), &data);
                if verbose {
                    eprintln!(
                        "smtp: stored {} bytes from {sender} to {count} recipient(s)",
                        data.len()
                    );
                }
                reply(&mut out, "250 2.0.0 OK queued")?;
            }
            "RSET" => {
                from = None;
                recipients.clear();
                reply(&mut out, "250 2.0.0 OK")?;
            }
            "NOOP" => reply(&mut out, "250 2.0.0 OK")?,
            "VRFY" => reply(&mut out, "252 2.1.5 Cannot verify, will accept")?,
            "QUIT" => {
                reply(&mut out, "221 2.0.0 Bye")?;
                return Ok(());
            }
            _ => reply(&mut out, "502 5.5.2 Command not recognized")?,
        }
    }
}

fn read_line(reader: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut line = Vec::new();
    if reader.read_until(b'\n', &mut line)? == 0 {
        return Ok(None);
    }
    Ok(Some(
        String::from_utf8_lossy(&line)
            .trim_end_matches(['\r', '\n'])
            .to_owned(),
    ))
}

/// Message lines up to the lone `.`, dot-unstuffed, CRLF kept.
fn read_data(reader: &mut impl BufRead) -> io::Result<Vec<u8>> {
    let mut data = Vec::new();
    loop {
        let mut line = Vec::new();
        if reader.read_until(b'\n', &mut line)? == 0 || line == b".\r\n" || line == b".\n" {
            return Ok(data);
        }
        let line = line.strip_prefix(b".").unwrap_or(&line);
        data.extend_from_slice(line);
    }
}

/// `MAIL FROM:<a@b> SIZE=10` → `a@b`.
fn address(line: &str) -> String {
    line.split_once('<')
        .and_then(|(_, rest)| rest.split_once('>'))
        .map(|(address, _)| address.trim().to_owned())
        .unwrap_or_else(|| {
            line.split_once(':')
                .map(|(_, rest)| {
                    rest.split_whitespace()
                        .next()
                        .unwrap_or_default()
                        .to_owned()
                })
                .unwrap_or_default()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_is_unstuffed() {
        let mut input: &[u8] = b"Subject: x\r\n\r\n..dot\r\nend\r\n.\r\nQUIT\r\n";
        assert_eq!(
            read_data(&mut input).unwrap(),
            b"Subject: x\r\n\r\n.dot\r\nend\r\n"
        );
        assert_eq!(address("MAIL FROM:<a@b.c> SIZE=1"), "a@b.c");
        assert_eq!(address("RCPT TO:x@y"), "x@y");
    }
}
