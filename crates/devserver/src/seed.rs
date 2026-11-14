//! Sample mail for `--seed`: a few recent messages in INBOX (one with an attachment, one
//! thread), one in Archive.

use chrono::{Duration, Utc};

use crate::Mailstore;

/// Fills `store` with sample messages addressed to `me@<domain>`.
pub fn seed_messages(store: &mut Mailstore) {
    let domain = store.domain.clone();
    let now = Utc::now();
    let date = |hours: i64| (now - Duration::hours(hours)).to_rfc2822();
    let samples = [
        (
            "INBOX",
            &[][..],
            format!(
                "From: Ada Lovelace <ada@example.org>\r\nTo: Me <me@{domain}>\r\n\
                 Subject: Welcome to the devserver\r\nDate: {}\r\n\
                 Message-ID: <seed-1@example.org>\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
                 Hello!\r\n\r\nThis message comes from mailbox-devserver. Reply to it to test\r\n\
                 replies; write to anyone@{domain} to receive your own mail.\r\n\r\nAda\r\n",
                date(30)
            ),
        ),
        (
            "INBOX",
            &[],
            format!(
                "From: Grace Hopper <grace@example.org>\r\nTo: Me <me@{domain}>\r\n\
                 Cc: Ada Lovelace <ada@example.org>\r\nSubject: Planning meeting\r\n\
                 Date: {}\r\nMessage-ID: <seed-2@example.org>\r\nMIME-Version: 1.0\r\n\
                 Content-Type: multipart/alternative; boundary=\"b1\"\r\n\r\n\
                 --b1\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
                 Can we meet on Thursday at 10?\r\n- agenda\r\n- budget\r\n\r\n\
                 --b1\r\nContent-Type: text/html; charset=utf-8\r\n\r\n\
                 <p>Can we meet on <b>Thursday at 10</b>?</p><ul><li>agenda</li><li>budget</li></ul>\r\n\
                 --b1--\r\n",
                date(5)
            ),
        ),
        (
            "INBOX",
            &["\\Seen"],
            format!(
                "From: Alan Turing <alan@example.org>\r\nTo: Me <me@{domain}>\r\n\
                 Subject: Re: Planning meeting\r\nDate: {}\r\n\
                 Message-ID: <seed-3@example.org>\r\nIn-Reply-To: <seed-2@example.org>\r\n\
                 References: <seed-2@example.org>\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
                 Thursday works for me.\r\n\r\n> Can we meet on Thursday at 10?\r\n",
                date(3)
            ),
        ),
        (
            "INBOX",
            &[],
            format!(
                "From: Billing <billing@example.com>\r\nTo: me@{domain}\r\n\
                 Subject: Your invoice\r\nDate: {}\r\nMessage-ID: <seed-4@example.com>\r\n\
                 MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"m1\"\r\n\r\n\
                 --m1\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
                 Your invoice is attached.\r\n\r\n--m1\r\n\
                 Content-Type: text/plain; name=\"invoice.txt\"\r\n\
                 Content-Disposition: attachment; filename=\"invoice.txt\"\r\n\r\n\
                 Total: 42 EUR\r\n--m1--\r\n",
                date(1)
            ),
        ),
        (
            "Archive",
            &["\\Seen"],
            format!(
                "From: Ada Lovelace <ada@example.org>\r\nTo: me@{domain}\r\n\
                 Subject: Old notes\r\nDate: {}\r\nMessage-ID: <seed-5@example.org>\r\n\r\n\
                 Archived a while ago.\r\n",
                date(24 * 10)
            ),
        ),
    ];
    for (folder, flags, raw) in samples {
        store.append(folder, raw.as_bytes(), flags);
    }
}

/// Appends `count` notification-style messages to INBOX (`--bulk`): multipart mail whose
/// text part starts with a separator line, like social network notifications. Enough of them
/// make a first sync slow, to test closing the app mid-sync.
pub fn bulk_messages(store: &mut Mailstore, count: usize) {
    let domain = store.domain.clone();
    let now = Utc::now();
    for index in 1..=count {
        let date = (now - Duration::minutes(index as i64 * 7)).to_rfc2822();
        let raw = format!(
            "From: Notifications <notification@social.example>\r\nTo: me@{domain}\r\n\
             Subject: You have {index} new notifications\r\nDate: {date}\r\n\
             Message-ID: <bulk-{index}@social.example>\r\nMIME-Version: 1.0\r\n\
             Content-Type: multipart/alternative; boundary=\"n{index}\"\r\n\r\n\
             --n{index}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
             ========================================\r\n\
             See your notifications ({index}).\r\nhttps://social.example/n/{index}\r\n\r\n\
             --n{index}\r\nContent-Type: text/html; charset=utf-8\r\n\r\n\
             <p>See your <b>notifications</b> ({index}).</p>\r\n--n{index}--\r\n"
        );
        store.append("INBOX", raw.as_bytes(), &[]);
    }
}
