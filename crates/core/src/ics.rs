//! iCalendar (.ics, RFC 5545) import/export and iTIP invitations (RFC 5546: REQUEST/REPLY/CANCEL).
//!
//! Own small parser: line unfolding, parameters, text escaping, `VEVENT` + `VALARM`.
//! Times with a `TZID` are resolved with chrono-tz (IANA names, plus common Windows names
//! sent by Outlook); floating times use the system zone.

use chrono::{DateTime, Duration, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};

use crate::calendar::{EventDetails, EventSnapshot, Invitation, Method, ShowAs, parse_tz};
use crate::error::{Error, Result};
use crate::model::*;
use crate::recurrence::to_utc;

const PRODID: &str = "-//Mailbox//Mailbox Calendar//EN";

/// A parsed .ics file.
#[derive(Debug, Clone, PartialEq)]
pub struct IcsCalendar {
    pub method: Option<Method>,
    /// Events with `id`/`calendar_id` 0; details always present.
    pub events: Vec<EventSnapshot>,
}

/// One content line: `NAME;PARAM=VALUE:value`.
#[derive(Debug, Clone, PartialEq)]
struct Property {
    name: String,
    params: Vec<(String, String)>,
    value: String,
}

impl Property {
    fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Parses .ics text.
pub fn parse(text: &str) -> Result<IcsCalendar> {
    let mut method = None;
    let mut events = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut current: Vec<Property> = Vec::new();
    let mut alarm: Vec<Property> = Vec::new();
    let mut alarms: Vec<Vec<Property>> = Vec::new();
    let mut saw_calendar = false;

    for line in unfold(text) {
        let property = parse_line(&line)?;
        match property.name.as_str() {
            "BEGIN" => {
                let component = property.value.to_ascii_uppercase();
                if component == "VCALENDAR" {
                    saw_calendar = true;
                }
                if component == "VEVENT" {
                    current.clear();
                    alarms.clear();
                }
                if component == "VALARM" {
                    alarm.clear();
                }
                stack.push(component);
            }
            "END" => {
                let component = property.value.to_ascii_uppercase();
                if stack.pop().as_deref() != Some(component.as_str()) {
                    return Err(Error::Invalid(format!("ics: unexpected END:{component}")));
                }
                if component == "VALARM" {
                    alarms.push(std::mem::take(&mut alarm));
                }
                if component == "VEVENT" {
                    events.push(build_event(&current, &alarms, method)?);
                }
            }
            _ => match stack.last().map(String::as_str) {
                Some("VCALENDAR") if property.name == "METHOD" => {
                    method = Method::parse(&property.value)
                }
                Some("VEVENT") => current.push(property),
                Some("VALARM") => alarm.push(property),
                _ => {}
            },
        }
    }
    if !saw_calendar {
        return Err(Error::Invalid("ics: missing BEGIN:VCALENDAR".into()));
    }
    Ok(IcsCalendar { method, events })
}

/// Joins folded lines (CRLF followed by a space or tab).
fn unfold(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(rest) = raw.strip_prefix([' ', '\t'])
            && let Some(last) = lines.last_mut()
        {
            last.push_str(rest);
            continue;
        }
        if !raw.trim().is_empty() {
            lines.push(raw.to_owned());
        }
    }
    lines
}

fn parse_line(line: &str) -> Result<Property> {
    // The value starts at the first ':' outside a quoted parameter.
    let mut in_quotes = false;
    let mut split = None;
    for (index, ch) in line.char_indices() {
        match ch {
            '"' => in_quotes = !in_quotes,
            ':' if !in_quotes => {
                split = Some(index);
                break;
            }
            _ => {}
        }
    }
    let split = split.ok_or_else(|| Error::Invalid(format!("ics: bad line `{line}`")))?;
    let (head, value) = (&line[..split], &line[split + 1..]);
    let mut parts = split_unquoted(head, ';').into_iter();
    let name = parts.next().unwrap_or_default().to_ascii_uppercase();
    let params = parts
        .filter_map(|part| {
            let (key, value) = part.split_once('=')?;
            Some((key.to_ascii_uppercase(), value.trim_matches('"').to_owned()))
        })
        .collect();
    Ok(Property {
        name,
        params,
        value: value.to_owned(),
    })
}

fn split_unquoted(text: &str, separator: char) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for ch in text.chars() {
        if ch == '"' {
            in_quotes = !in_quotes;
        }
        if ch == separator && !in_quotes {
            parts.push(std::mem::take(&mut current));
        } else {
            current.push(ch);
        }
    }
    parts.push(current);
    parts
}

fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n' | 'N') => out.push('\n'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace("\r\n", "\\n")
        .replace('\n', "\\n")
}

/// A parsed DTSTART/DTEND-like value.
struct ParsedTime {
    utc: DateTime<Utc>,
    date_only: bool,
    zone: Option<String>,
}

fn parse_time(property: &Property) -> Result<ParsedTime> {
    let value = property.value.trim();
    let bad = || Error::Invalid(format!("ics: bad date `{value}`"));
    let date_only = property
        .param("VALUE")
        .is_some_and(|v| v.eq_ignore_ascii_case("DATE"))
        || (value.len() == 8 && !value.contains('T'));
    if date_only {
        let date = NaiveDate::parse_from_str(value, "%Y%m%d").map_err(|_| bad())?;
        return Ok(ParsedTime {
            utc: crate::calendar::all_day_start(date),
            date_only: true,
            zone: None,
        });
    }
    let (naive, is_utc) = match value.strip_suffix('Z') {
        Some(rest) => (rest, true),
        None => (value, false),
    };
    let local = NaiveDateTime::parse_from_str(naive, "%Y%m%dT%H%M%S").map_err(|_| bad())?;
    if is_utc {
        return Ok(ParsedTime {
            utc: local.and_utc(),
            date_only: false,
            zone: None,
        });
    }
    let zone = property.param("TZID").and_then(resolve_zone);
    let utc = match zone.as_deref().and_then(parse_tz) {
        Some(tz) => to_utc(&tz, local),
        None => to_utc(&Local, local),
    };
    Ok(ParsedTime {
        utc,
        date_only: false,
        zone,
    })
}

/// IANA name for a TZID (IANA as is, common Windows names mapped).
fn resolve_zone(tzid: &str) -> Option<String> {
    let tzid = tzid.trim().trim_start_matches('/');
    if parse_tz(tzid).is_some() {
        return Some(tzid.to_owned());
    }
    const WINDOWS: &[(&str, &str)] = &[
        ("UTC", "UTC"),
        ("GMT Standard Time", "Europe/London"),
        ("Romance Standard Time", "Europe/Paris"),
        ("W. Europe Standard Time", "Europe/Berlin"),
        ("Central Europe Standard Time", "Europe/Budapest"),
        ("Central European Standard Time", "Europe/Warsaw"),
        ("E. Europe Standard Time", "Europe/Chisinau"),
        ("FLE Standard Time", "Europe/Kiev"),
        ("GTB Standard Time", "Europe/Bucharest"),
        ("Russian Standard Time", "Europe/Moscow"),
        ("Eastern Standard Time", "America/New_York"),
        ("Central Standard Time", "America/Chicago"),
        ("Mountain Standard Time", "America/Denver"),
        ("Pacific Standard Time", "America/Los_Angeles"),
        ("Atlantic Standard Time", "America/Halifax"),
        ("E. South America Standard Time", "America/Sao_Paulo"),
        ("India Standard Time", "Asia/Kolkata"),
        ("China Standard Time", "Asia/Shanghai"),
        ("Tokyo Standard Time", "Asia/Tokyo"),
        ("Singapore Standard Time", "Asia/Singapore"),
        ("AUS Eastern Standard Time", "Australia/Sydney"),
        ("New Zealand Standard Time", "Pacific/Auckland"),
    ];
    WINDOWS
        .iter()
        .find(|(windows, _)| windows.eq_ignore_ascii_case(tzid))
        .map(|(_, iana)| (*iana).to_owned())
}

/// Parses an iCalendar duration (`PT1H30M`, `-P1D`, `P2W`).
pub fn parse_duration(value: &str) -> Option<Duration> {
    let value = value.trim();
    let (negative, rest) = match value.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, value.strip_prefix('+').unwrap_or(value)),
    };
    let rest = rest.strip_prefix('P')?;
    let mut total = Duration::zero();
    let mut number = String::new();
    let mut in_time = false;
    for ch in rest.chars() {
        match ch {
            'T' => in_time = true,
            '0'..='9' => number.push(ch),
            unit => {
                let n: i64 = number.parse().ok()?;
                number.clear();
                total += match (unit, in_time) {
                    ('W', false) => Duration::weeks(n),
                    ('D', false) => Duration::days(n),
                    ('H', true) => Duration::hours(n),
                    ('M', true) => Duration::minutes(n),
                    ('S', true) => Duration::seconds(n),
                    _ => return None,
                };
            }
        }
    }
    if !number.is_empty() {
        return None;
    }
    Some(if negative { -total } else { total })
}

fn format_duration(duration: Duration) -> String {
    let minutes = duration.num_minutes();
    let sign = if minutes < 0 { "-" } else { "" };
    let minutes = minutes.abs();
    if minutes == 0 {
        "PT0M".into()
    } else if minutes % (24 * 60) == 0 {
        format!("{sign}P{}D", minutes / (24 * 60))
    } else {
        format!("{sign}PT{}M", minutes)
    }
}

fn parse_address(property: &Property) -> Address {
    let email = property.value.trim();
    let email = email
        .strip_prefix("mailto:")
        .or_else(|| email.strip_prefix("MAILTO:"))
        .unwrap_or(email);
    Address::new(property.param("CN").unwrap_or_default(), email)
}

fn parse_status(value: Option<&str>) -> ResponseStatus {
    match value.map(str::to_ascii_uppercase).as_deref() {
        Some("ACCEPTED") => ResponseStatus::Accepted,
        Some("TENTATIVE") => ResponseStatus::Tentative,
        Some("DECLINED") => ResponseStatus::Declined,
        _ => ResponseStatus::NeedsAction,
    }
}

fn status_code(status: ResponseStatus) -> &'static str {
    match status {
        ResponseStatus::NeedsAction => "NEEDS-ACTION",
        ResponseStatus::Accepted => "ACCEPTED",
        ResponseStatus::Tentative => "TENTATIVE",
        ResponseStatus::Declined => "DECLINED",
    }
}

fn build_event(
    properties: &[Property],
    alarms: &[Vec<Property>],
    method: Option<Method>,
) -> Result<EventSnapshot> {
    let get = |name: &str| properties.iter().find(|p| p.name == name);
    let text = |name: &str| get(name).map(|p| unescape(&p.value)).unwrap_or_default();

    let start = parse_time(
        get("DTSTART").ok_or_else(|| Error::Invalid("ics: VEVENT without DTSTART".into()))?,
    )?;
    let end = match (get("DTEND"), get("DURATION")) {
        (Some(end), _) => parse_time(end)?.utc,
        (None, Some(duration)) => {
            start.utc + parse_duration(&duration.value).unwrap_or_else(Duration::zero)
        }
        (None, None) if start.date_only => start.utc + Duration::days(1),
        (None, None) => start.utc,
    };
    let mut exdates = Vec::new();
    for property in properties.iter().filter(|p| p.name == "EXDATE") {
        for value in property.value.split(',') {
            let single = Property {
                value: value.to_owned(),
                ..property.clone()
            };
            exdates.push(parse_time(&single)?.utc);
        }
    }
    let recurrence_id = get("RECURRENCE-ID")
        .map(parse_time)
        .transpose()?
        .map(|time| time.utc);
    let attendees = properties
        .iter()
        .filter(|p| p.name == "ATTENDEE")
        .map(|p| Attendee {
            address: parse_address(p),
            status: parse_status(p.param("PARTSTAT")),
            optional: p
                .param("ROLE")
                .is_some_and(|role| role.eq_ignore_ascii_case("OPT-PARTICIPANT")),
        })
        .collect();
    let reminders = alarms
        .iter()
        .filter_map(|alarm| {
            let trigger = alarm.iter().find(|p| p.name == "TRIGGER")?;
            parse_duration(&trigger.value).map(|offset| -offset.num_minutes())
        })
        .filter(|minutes| *minutes >= 0)
        .collect();
    let categories = properties
        .iter()
        .filter(|p| p.name == "CATEGORIES")
        .flat_map(|p| split_unquoted(&p.value, ','))
        .map(|c| unescape(c.trim()))
        .filter(|c| !c.is_empty())
        .collect();
    let busy_status = get("X-MICROSOFT-CDO-BUSYSTATUS").map(|p| p.value.to_ascii_uppercase());
    let show_as = match busy_status.as_deref() {
        Some("FREE") => ShowAs::Free,
        Some("TENTATIVE") => ShowAs::Tentative,
        Some("OOF") => ShowAs::OutOfOffice,
        Some("WORKINGELSEWHERE") => ShowAs::WorkingElsewhere,
        Some(_) => ShowAs::Busy,
        None => match (
            get("TRANSP").map(|p| p.value.to_ascii_uppercase()),
            get("STATUS").map(|p| p.value.to_ascii_uppercase()),
        ) {
            (Some(transp), _) if transp == "TRANSPARENT" => ShowAs::Free,
            (_, Some(status)) if status == "TENTATIVE" => ShowAs::Tentative,
            _ => ShowAs::Busy,
        },
    };
    let sequence = get("SEQUENCE")
        .and_then(|p| p.value.trim().parse().ok())
        .unwrap_or(0);
    let uid = get("UID")
        .map(|p| p.value.trim().to_owned())
        .filter(|uid| !uid.is_empty())
        .unwrap_or_else(crate::calendar::new_uid);

    let event = CalendarEvent {
        id: 0,
        calendar_id: 0,
        title: text("SUMMARY"),
        location: text("LOCATION"),
        description: text("DESCRIPTION"),
        start: start.utc,
        end: end.max(start.utc),
        all_day: start.date_only,
        recurrence: get("RRULE").map(|p| p.value.trim().to_owned()),
        attendees,
        reminders,
        categories,
    };
    let details = EventDetails {
        event_id: 0,
        uid,
        time_zone: start.zone,
        show_as,
        private: get("CLASS").is_some_and(|p| {
            matches!(
                p.value.to_ascii_uppercase().as_str(),
                "PRIVATE" | "CONFIDENTIAL"
            )
        }),
        organizer: get("ORGANIZER").map(parse_address),
        exdates,
        master_id: None,
        recurrence_id,
        invitation: method
            .filter(|m| matches!(m, Method::Request | Method::Cancel))
            .map(|method| Invitation { method, sequence }),
    };
    Ok(EventSnapshot {
        event,
        details: Some(details),
    })
}

// ---------- export ----------

/// Serializes events into a VCALENDAR (with `METHOD` when given).
pub fn export(events: &[(CalendarEvent, EventDetails)], method: Option<Method>) -> String {
    let mut out = Writer::default();
    out.line("BEGIN:VCALENDAR");
    out.line("VERSION:2.0");
    out.line(&format!("PRODID:{PRODID}"));
    out.line("CALSCALE:GREGORIAN");
    if let Some(method) = method {
        out.line(&format!("METHOD:{}", method.as_str()));
    }
    for (event, details) in events {
        write_event(&mut out, event, details, method);
    }
    out.line("END:VCALENDAR");
    out.text
}

/// An invitation (`METHOD:REQUEST`) for an event the user organizes.
pub fn request(event: &CalendarEvent, details: &EventDetails) -> String {
    export(&[(event.clone(), details.clone())], Some(Method::Request))
}

/// The user's answer (`METHOD:REPLY`) to an invitation, from attendee `email`.
pub fn reply(
    event: &CalendarEvent,
    details: &EventDetails,
    email: &str,
    status: ResponseStatus,
) -> String {
    let mut answer = event.clone();
    let me = event
        .attendees
        .iter()
        .find(|a| a.address.email.eq_ignore_ascii_case(email))
        .map(|a| a.address.clone())
        .unwrap_or_else(|| Address::new("", email));
    answer.attendees = vec![Attendee {
        address: me,
        status,
        optional: false,
    }];
    export(&[(answer, details.clone())], Some(Method::Reply))
}

#[derive(Default)]
struct Writer {
    text: String,
}

impl Writer {
    /// Writes one content line folded at 75 octets (never splitting a UTF-8 character).
    fn line(&mut self, line: &str) {
        let mut width = 0;
        for ch in line.chars() {
            let len = ch.len_utf8();
            if width + len > 75 {
                self.text.push_str("\r\n ");
                width = 1;
            }
            self.text.push(ch);
            width += len;
        }
        self.text.push_str("\r\n");
    }
}

fn utc_stamp(time: DateTime<Utc>) -> String {
    time.format("%Y%m%dT%H%M%SZ").to_string()
}

fn write_time(
    out: &mut Writer,
    name: &str,
    time: DateTime<Utc>,
    all_day: bool,
    zone: Option<&str>,
) {
    if all_day {
        out.line(&format!("{name};VALUE=DATE:{}", time.format("%Y%m%d")));
        return;
    }
    match zone.and_then(|z| parse_tz(z).map(|tz| (z, tz))) {
        Some((name_tz, tz)) => out.line(&format!(
            "{name};TZID={name_tz}:{}",
            tz.from_utc_datetime(&time.naive_utc())
                .format("%Y%m%dT%H%M%S")
        )),
        None => out.line(&format!("{name}:{}", utc_stamp(time))),
    }
}

fn address_line(name: &str, params: &str, address: &Address) -> String {
    let cn = if address.name.is_empty() {
        String::new()
    } else {
        format!(";CN=\"{}\"", address.name.replace('"', "'"))
    };
    format!("{name}{cn}{params}:mailto:{}", address.email)
}

fn write_event(
    out: &mut Writer,
    event: &CalendarEvent,
    details: &EventDetails,
    method: Option<Method>,
) {
    let zone = details.time_zone.as_deref();
    out.line("BEGIN:VEVENT");
    out.line(&format!("UID:{}", details.uid));
    out.line(&format!("DTSTAMP:{}", utc_stamp(Utc::now())));
    out.line(&format!(
        "SEQUENCE:{}",
        details.invitation.as_ref().map_or(0, |i| i.sequence)
    ));
    write_time(out, "DTSTART", event.start, event.all_day, zone);
    write_time(out, "DTEND", event.end, event.all_day, zone);
    if let Some(recurrence_id) = details.recurrence_id {
        write_time(out, "RECURRENCE-ID", recurrence_id, event.all_day, zone);
    }
    if let Some(rule) = &event.recurrence {
        out.line(&format!("RRULE:{rule}"));
        for exdate in &details.exdates {
            write_time(out, "EXDATE", *exdate, event.all_day, zone);
        }
    }
    out.line(&format!("SUMMARY:{}", escape(&event.title)));
    if !event.location.is_empty() {
        out.line(&format!("LOCATION:{}", escape(&event.location)));
    }
    if !event.description.is_empty() {
        out.line(&format!("DESCRIPTION:{}", escape(&event.description)));
    }
    if !event.categories.is_empty() {
        let categories: Vec<String> = event.categories.iter().map(|c| escape(c)).collect();
        out.line(&format!("CATEGORIES:{}", categories.join(",")));
    }
    if details.private {
        out.line("CLASS:PRIVATE");
    }
    out.line(if details.show_as == ShowAs::Free {
        "TRANSP:TRANSPARENT"
    } else {
        "TRANSP:OPAQUE"
    });
    let busy = match details.show_as {
        ShowAs::Free => "FREE",
        ShowAs::Tentative => "TENTATIVE",
        ShowAs::Busy => "BUSY",
        ShowAs::OutOfOffice => "OOF",
        ShowAs::WorkingElsewhere => "WORKINGELSEWHERE",
    };
    out.line(&format!("X-MICROSOFT-CDO-BUSYSTATUS:{busy}"));
    if method == Some(Method::Cancel) {
        out.line("STATUS:CANCELLED");
    }
    if let Some(organizer) = &details.organizer {
        out.line(&address_line("ORGANIZER", "", organizer));
    }
    for attendee in &event.attendees {
        let role = if attendee.optional {
            "OPT-PARTICIPANT"
        } else {
            "REQ-PARTICIPANT"
        };
        let params = format!(
            ";ROLE={role};PARTSTAT={};RSVP=TRUE",
            status_code(attendee.status)
        );
        out.line(&address_line("ATTENDEE", &params, &attendee.address));
    }
    if method != Some(Method::Reply) {
        for minutes in &event.reminders {
            out.line("BEGIN:VALARM");
            out.line("ACTION:DISPLAY");
            out.line(&format!("DESCRIPTION:{}", escape(&event.title)));
            out.line(&format!(
                "TRIGGER:{}",
                format_duration(-Duration::minutes(*minutes))
            ));
            out.line("END:VALARM");
        }
    }
    out.line("END:VEVENT");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    const OUTLOOK_INVITE: &str = "BEGIN:VCALENDAR\r\n\
METHOD:REQUEST\r\n\
PRODID:Microsoft Exchange Server 2010\r\n\
VERSION:2.0\r\n\
BEGIN:VTIMEZONE\r\n\
TZID:Romance Standard Time\r\n\
BEGIN:STANDARD\r\n\
DTSTART:16010101T030000\r\n\
TZOFFSETFROM:+0200\r\n\
TZOFFSETTO:+0100\r\n\
END:STANDARD\r\n\
END:VTIMEZONE\r\n\
BEGIN:VEVENT\r\n\
ORGANIZER;CN=\"Grace Hopper\":mailto:grace@example.com\r\n\
ATTENDEE;ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE;CN=Me:mailto:me@\r\n example.com\r\n\
ATTENDEE;ROLE=OPT-PARTICIPANT;PARTSTAT=ACCEPTED;CN=\"Lin, Ada\":mailto:ada@example.com\r\n\
DESCRIPTION:Agenda:\\n1. Budget\\; 2. Hiring\\, maybe\r\n\
RRULE:FREQ=WEEKLY;BYDAY=TU;COUNT=6\r\n\
EXDATE;TZID=Romance Standard Time:20261124T100000\r\n\
UID:040000008200E00074C5B7101A82E008@example.com\r\n\
SUMMARY;LANGUAGE=en-US:Budget review\r\n\
DTSTART;TZID=Romance Standard Time:20261117T100000\r\n\
DTEND;TZID=Romance Standard Time:20261117T110000\r\n\
CLASS:PRIVATE\r\n\
SEQUENCE:2\r\n\
LOCATION:Room 4\r\n\
CATEGORIES:Work,Finance\r\n\
X-MICROSOFT-CDO-BUSYSTATUS:TENTATIVE\r\n\
BEGIN:VALARM\r\n\
ACTION:DISPLAY\r\n\
TRIGGER:-PT15M\r\n\
END:VALARM\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

    #[test]
    fn parses_outlook_invitation() {
        let calendar = parse(OUTLOOK_INVITE).unwrap();
        assert_eq!(calendar.method, Some(Method::Request));
        assert_eq!(calendar.events.len(), 1);
        let EventSnapshot { event, details } = &calendar.events[0];
        let details = details.as_ref().unwrap();
        assert_eq!(event.title, "Budget review");
        assert_eq!(event.location, "Room 4");
        assert_eq!(event.description, "Agenda:\n1. Budget; 2. Hiring, maybe");
        // 10:00 Paris in November = 09:00 UTC.
        assert_eq!(event.start, utc(2026, 11, 17, 9, 0));
        assert_eq!(event.end, utc(2026, 11, 17, 10, 0));
        assert_eq!(
            event.recurrence.as_deref(),
            Some("FREQ=WEEKLY;BYDAY=TU;COUNT=6")
        );
        assert_eq!(details.exdates, [utc(2026, 11, 24, 9, 0)]);
        assert_eq!(details.time_zone.as_deref(), Some("Europe/Paris"));
        assert_eq!(details.show_as, ShowAs::Tentative);
        assert!(details.private);
        assert_eq!(event.reminders, [15]);
        assert_eq!(event.categories, ["Work", "Finance"]);
        assert_eq!(
            details.organizer,
            Some(Address::new("Grace Hopper", "grace@example.com"))
        );
        assert_eq!(event.attendees.len(), 2);
        assert_eq!(event.attendees[0].address.email, "me@example.com");
        assert_eq!(event.attendees[1].address.name, "Lin, Ada");
        assert!(event.attendees[1].optional);
        assert_eq!(event.attendees[1].status, ResponseStatus::Accepted);
        assert_eq!(
            details.invitation,
            Some(Invitation {
                method: Method::Request,
                sequence: 2
            })
        );
    }

    #[test]
    fn export_then_parse_roundtrips() {
        let calendar = parse(OUTLOOK_INVITE).unwrap();
        let EventSnapshot { event, details } = calendar.events[0].clone();
        let details = details.unwrap();
        let text = export(&[(event.clone(), details.clone())], Some(Method::Request));
        assert!(text.lines().all(|line| line.len() <= 76));
        let again = parse(&text).unwrap();
        assert_eq!(again.method, Some(Method::Request));
        let EventSnapshot {
            event: event2,
            details: details2,
        } = again.events[0].clone();
        assert_eq!(event2, event);
        assert_eq!(details2.unwrap(), details);
    }

    #[test]
    fn all_day_and_duration() {
        let text = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:a\nSUMMARY:Holiday\nDTSTART;VALUE=DATE:20261225\nEND:VEVENT\nBEGIN:VEVENT\nUID:b\nSUMMARY:Call\nDTSTART:20261110T080000Z\nDURATION:PT1H30M\nTRANSP:TRANSPARENT\nEND:VEVENT\nEND:VCALENDAR\n";
        let calendar = parse(text).unwrap();
        assert_eq!(calendar.method, None);
        let holiday = &calendar.events[0].event;
        assert!(holiday.all_day);
        assert_eq!(holiday.start, utc(2026, 12, 25, 0, 0));
        assert_eq!(holiday.end, utc(2026, 12, 26, 0, 0));
        let call = &calendar.events[1];
        assert_eq!(call.event.end - call.event.start, Duration::minutes(90));
        assert_eq!(call.details.as_ref().unwrap().show_as, ShowAs::Free);
        assert!(call.details.as_ref().unwrap().invitation.is_none());
    }

    #[test]
    fn reply_contains_only_my_answer() {
        let calendar = parse(OUTLOOK_INVITE).unwrap();
        let EventSnapshot { event, details } = calendar.events[0].clone();
        let text = reply(
            &event,
            &details.unwrap(),
            "ME@example.com",
            ResponseStatus::Accepted,
        );
        assert!(text.contains("METHOD:REPLY"));
        let parsed = parse(&text).unwrap();
        let attendees = &parsed.events[0].event.attendees;
        assert_eq!(attendees.len(), 1);
        assert_eq!(attendees[0].address.email, "me@example.com");
        assert_eq!(attendees[0].status, ResponseStatus::Accepted);
        assert!(!text.contains("BEGIN:VALARM"));
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("PT15M"), Some(Duration::minutes(15)));
        assert_eq!(parse_duration("-PT1H"), Some(Duration::hours(-1)));
        assert_eq!(parse_duration("P1DT2H"), Some(Duration::hours(26)));
        assert_eq!(parse_duration("P2W"), Some(Duration::weeks(2)));
        assert_eq!(parse_duration("PT"), Some(Duration::zero()));
        assert_eq!(parse_duration("15M"), None);
        assert_eq!(parse_duration("PT5"), None);
        assert_eq!(format_duration(-Duration::minutes(15)), "-PT15M");
        assert_eq!(format_duration(-Duration::days(1)), "-P1D");
    }

    #[test]
    fn rejects_broken_files() {
        assert!(parse("hello").is_err());
        assert!(parse("BEGIN:VEVENT\nEND:VEVENT\n").is_err());
        assert!(parse("BEGIN:VCALENDAR\nBEGIN:VEVENT\nEND:VCALENDAR\n").is_err());
        assert!(
            parse("BEGIN:VCALENDAR\nBEGIN:VEVENT\nSUMMARY:x\nEND:VEVENT\nEND:VCALENDAR\n").is_err()
        );
    }

    #[test]
    fn folding_keeps_utf8_intact() {
        let mut writer = Writer::default();
        let long = format!("SUMMARY:{}", "é".repeat(60));
        writer.line(&long);
        assert!(writer.text.split("\r\n").all(|line| line.len() <= 75));
        let unfolded = unfold(&writer.text);
        assert_eq!(unfolded, [long]);
    }
}
