//! Local data <-> DAV resources, reusing the core `.ics` and `.vcf` code.
//!
//! - An event resource holds a series and its modified occurrences (same UID). Locally a
//!   series also lists its modified occurrences in `exdates`; on the wire they are only
//!   `RECURRENCE-ID` overrides, so they are removed on export and added back on import.
//! - Contacts carry a `UID` line (required by CardDAV) that the core vCard writer omits.

use mailbox_core::calendar::{EventDetails, EventSnapshot};
use mailbox_core::people::ContactCard;
use mailbox_core::{CalendarEvent, Id, ics, vcard};

use crate::{SyncError, SyncResult};

/// One resource ready to upload, with what identifies it locally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalItem {
    /// Series event id or contact id.
    pub local_id: Id,
    pub uid: String,
    pub text: String,
}

impl LocalItem {
    pub fn hash(&self) -> u64 {
        content_hash(&self.text)
    }
}

/// Serializes a series (or one-off event) with its modified occurrences.
pub fn event_resource(
    master: &(CalendarEvent, EventDetails),
    exceptions: &[(CalendarEvent, EventDetails)],
) -> String {
    let mut series = master.clone();
    series
        .1
        .exdates
        .retain(|at| !exceptions.iter().any(|(_, d)| d.recurrence_id == Some(*at)));
    let mut all = vec![series];
    all.extend(exceptions.iter().cloned());
    ics::export(&all, None)
}

/// Parses an event resource: the series (or one-off) first, then its overrides.
pub fn parse_event_resource(text: &str) -> SyncResult<Vec<EventSnapshot>> {
    let mut events = ics::parse(text)
        .map_err(|err| SyncError::Parse(err.to_string()))?
        .events;
    let overrides: Vec<_> = events
        .iter()
        .filter_map(|s| s.details.as_ref()?.recurrence_id)
        .collect();
    for snapshot in &mut events {
        if let Some(details) = snapshot.details.as_mut()
            && details.recurrence_id.is_none()
            && snapshot.event.recurrence.is_some()
        {
            for at in &overrides {
                if !details.exdates.contains(at) {
                    details.exdates.push(*at);
                }
            }
            details.exdates.sort();
        }
    }
    events.sort_by_key(|s| s.details.as_ref().and_then(|d| d.recurrence_id));
    if events.is_empty() {
        return Err(SyncError::Parse("calendar resource without event".into()));
    }
    Ok(events)
}

/// A vCard with its `UID`.
pub fn contact_resource(card: &ContactCard, uid: &str) -> String {
    let text = vcard::write_vcard(card);
    let uid_line = format!("UID:{uid}\r\n");
    match text
        .find("VERSION:")
        .and_then(|at| text[at..].find("\r\n").map(|end| at + end + 2))
    {
        Some(at) => format!("{}{uid_line}{}", &text[..at], &text[at..]),
        None => text.replacen("BEGIN:VCARD\r\n", &format!("BEGIN:VCARD\r\n{uid_line}"), 1),
    }
}

/// Parses a contact resource: the card and its UID (empty when missing).
pub fn parse_contact_resource(text: &str, account_id: Id) -> SyncResult<(ContactCard, String)> {
    let card = vcard::parse_vcards(text, account_id)
        .into_iter()
        .next()
        .ok_or_else(|| SyncError::Parse("address book resource without contact".into()))?;
    Ok((card, resource_uid(text).unwrap_or_default()))
}

/// `UID` of an .ics/.vcf text (first occurrence).
pub fn resource_uid(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.strip_prefix("UID:"))
        .map(|uid| uid.trim().to_owned())
        .filter(|uid| !uid.is_empty())
}

/// Change detector for local content (FNV-1a, stable across builds). `DTSTAMP` is ignored
/// since the writer stamps the current time.
pub fn content_hash(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for line in text.lines().filter(|line| !line.starts_with("DTSTAMP")) {
        for byte in line.bytes().chain(*b"\n") {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    hash
}

/// A file name safe in a URL path for a new resource.
pub fn resource_name(uid: &str, extension: &str) -> String {
    let name: String = uid
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-_.".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("{name}.{extension}")
}

/// iMIP invitation (`METHOD:REQUEST`) to attach to an email when the server does not
/// schedule: `(file name, MIME type, content)`.
pub fn invitation_attachment(
    event: &CalendarEvent,
    details: &EventDetails,
) -> (String, String, String) {
    (
        "invite.ics".to_owned(),
        "text/calendar; method=REQUEST; charset=utf-8".to_owned(),
        ics::request(event, details),
    )
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use mailbox_core::calendar::Method;

    use super::*;

    fn event(title: &str, hour: u32, rule: Option<&str>) -> CalendarEvent {
        CalendarEvent {
            id: 0,
            calendar_id: 0,
            title: title.into(),
            location: String::new(),
            description: String::new(),
            start: Utc.with_ymd_and_hms(2026, 3, 2, hour, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 3, 2, hour + 1, 0, 0).unwrap(),
            all_day: false,
            recurrence: rule.map(str::to_owned),
            attendees: Vec::new(),
            reminders: Vec::new(),
            categories: Vec::new(),
        }
    }

    fn details(uid: &str) -> EventDetails {
        EventDetails {
            uid: uid.into(),
            time_zone: Some("UTC".into()),
            ..EventDetails::default_for(1)
        }
    }

    #[test]
    fn recurring_series_with_exception_roundtrips() {
        let moved_from = Utc.with_ymd_and_hms(2026, 3, 9, 9, 0, 0).unwrap();
        let skipped = Utc.with_ymd_and_hms(2026, 3, 16, 9, 0, 0).unwrap();
        let mut master_details = details("series@x");
        // Locally the modified occurrence is also an exdate.
        master_details.exdates = vec![moved_from, skipped];
        let master = (
            event("Standup", 9, Some("FREQ=WEEKLY;COUNT=5")),
            master_details,
        );
        let mut exception_event = event("Standup (late)", 11, None);
        exception_event.start = Utc.with_ymd_and_hms(2026, 3, 9, 11, 0, 0).unwrap();
        exception_event.end = Utc.with_ymd_and_hms(2026, 3, 9, 12, 0, 0).unwrap();
        let exception = (
            exception_event,
            EventDetails {
                recurrence_id: Some(moved_from),
                ..details("series@x")
            },
        );

        let text = event_resource(&master, std::slice::from_ref(&exception));
        assert_eq!(text.matches("BEGIN:VEVENT").count(), 2);
        assert_eq!(
            text.matches("EXDATE").count(),
            1,
            "override is not an EXDATE:\n{text}"
        );
        assert!(text.contains("RECURRENCE-ID"));

        let parsed = parse_event_resource(&text).unwrap();
        assert_eq!(parsed.len(), 2);
        let series = parsed[0].details.as_ref().unwrap();
        assert_eq!(series.recurrence_id, None);
        assert_eq!(series.exdates, vec![moved_from, skipped]);
        assert_eq!(
            parsed[0].event.recurrence.as_deref(),
            Some("FREQ=WEEKLY;COUNT=5")
        );
        assert_eq!(
            parsed[1].details.as_ref().unwrap().recurrence_id,
            Some(moved_from)
        );
        assert_eq!(parsed[1].event.title, "Standup (late)");
        assert_eq!(resource_uid(&text).as_deref(), Some("series@x"));
    }

    #[test]
    fn contact_uid_is_written_and_read() {
        let mut card = ContactCard::new(3);
        card.contact.display_name = "Ada Lovelace".into();
        card.contact.emails = vec!["ada@example.com".into()];
        let text = contact_resource(&card, "ada-1");
        assert!(
            text.starts_with("BEGIN:VCARD\r\nVERSION:4.0\r\nUID:ada-1\r\n"),
            "{text}"
        );
        let (parsed, uid) = parse_contact_resource(&text, 3).unwrap();
        assert_eq!(uid, "ada-1");
        assert_eq!(parsed.contact.emails, card.contact.emails);
        assert_eq!(parsed.contact.account_id, 3);
    }

    #[test]
    fn hash_ignores_dtstamp_and_names_are_safe() {
        assert_eq!(
            content_hash("A\r\nDTSTAMP:1\r\nB"),
            content_hash("A\r\nDTSTAMP:2\r\nB")
        );
        assert_ne!(content_hash("A"), content_hash("B"));
        assert_eq!(resource_name("a b/c@x", "ics"), "a-b-c-x.ics");
    }

    #[test]
    fn invitation_is_a_request() {
        let (name, mime, text) = invitation_attachment(&event("Review", 9, None), &details("r@x"));
        assert_eq!(name, "invite.ics");
        assert!(mime.contains("method=REQUEST"));
        assert_eq!(ics::parse(&text).unwrap().method, Some(Method::Request));
    }
}
