//! iCalendar recurrence rules (RFC 5545 `RRULE` subset) and occurrence expansion.
//!
//! Supported: `FREQ` (daily, weekly, monthly, yearly), `INTERVAL`, `COUNT`, `UNTIL`,
//! `BYDAY` (with ordinals such as `2TU` / `-1FR`), `BYMONTHDAY`, `BYMONTH`, `BYSETPOS`, `WKST`.
//! Expansion runs on wall-clock time in the event's time zone so a 09:00 weekly meeting stays
//! at 09:00 across daylight-saving changes.

use std::fmt;

use chrono::{
    DateTime, Datelike, Duration, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Utc, Weekday,
};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Safety cap: never generate more occurrences than this per expansion.
const MAX_OCCURRENCES: usize = 5_000;
/// Safety cap on the number of periods (days/weeks/months/years) scanned.
const MAX_PERIODS: i64 = 50_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Frequency {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

impl Frequency {
    pub const ALL: [Frequency; 4] = [Self::Daily, Self::Weekly, Self::Monthly, Self::Yearly];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Daily => "DAILY",
            Self::Weekly => "WEEKLY",
            Self::Monthly => "MONTHLY",
            Self::Yearly => "YEARLY",
        }
    }

    fn unit(self, plural: bool) -> &'static str {
        match (self, plural) {
            (Self::Daily, false) => "day",
            (Self::Daily, true) => "days",
            (Self::Weekly, false) => "week",
            (Self::Weekly, true) => "weeks",
            (Self::Monthly, false) => "month",
            (Self::Monthly, true) => "months",
            (Self::Yearly, false) => "year",
            (Self::Yearly, true) => "years",
        }
    }
}

/// A `BYDAY` entry: a weekday, optionally the n-th of the month/year (`2TU`, `-1FR`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WeekdayNum {
    pub ordinal: Option<i8>,
    pub weekday: Weekday,
}

impl WeekdayNum {
    pub fn every(weekday: Weekday) -> Self {
        Self {
            ordinal: None,
            weekday,
        }
    }

    pub fn nth(ordinal: i8, weekday: Weekday) -> Self {
        Self {
            ordinal: Some(ordinal),
            weekday,
        }
    }
}

/// A parsed recurrence rule. Build one directly or with [`RRule::parse`]; `to_string()` gives
/// the `RRULE` value back (without the `RRULE:` prefix), which is what
/// [`crate::CalendarEvent::recurrence`] stores.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RRule {
    pub freq: Frequency,
    /// Every n-th period, >= 1.
    pub interval: u32,
    /// Total number of occurrences, including the first.
    pub count: Option<u32>,
    /// Last allowed occurrence start (inclusive).
    pub until: Option<DateTime<Utc>>,
    pub by_day: Vec<WeekdayNum>,
    /// Days of the month, negative counts from the end (`-1` = last day).
    pub by_month_day: Vec<i8>,
    /// Months 1..=12.
    pub by_month: Vec<u32>,
    /// Picks the n-th candidate of each period (`BYSETPOS`), negative from the end.
    pub by_set_pos: Vec<i32>,
    pub week_start: Weekday,
}

impl RRule {
    pub fn new(freq: Frequency) -> Self {
        Self {
            freq,
            interval: 1,
            count: None,
            until: None,
            by_day: Vec::new(),
            by_month_day: Vec::new(),
            by_month: Vec::new(),
            by_set_pos: Vec::new(),
            week_start: Weekday::Mon,
        }
    }

    pub fn interval(mut self, interval: u32) -> Self {
        self.interval = interval.max(1);
        self
    }

    pub fn count(mut self, count: u32) -> Self {
        self.count = Some(count);
        self
    }

    pub fn until(mut self, until: DateTime<Utc>) -> Self {
        self.until = Some(until);
        self
    }

    pub fn by_day(mut self, days: impl IntoIterator<Item = WeekdayNum>) -> Self {
        self.by_day = days.into_iter().collect();
        self
    }

    pub fn by_month_day(mut self, days: impl IntoIterator<Item = i8>) -> Self {
        self.by_month_day = days.into_iter().collect();
        self
    }

    pub fn by_month(mut self, months: impl IntoIterator<Item = u32>) -> Self {
        self.by_month = months.into_iter().collect();
        self
    }

    /// Parses an `RRULE` value; a leading `RRULE:` is accepted. Unknown parts are ignored.
    pub fn parse(value: &str) -> Result<Self> {
        let value = value.trim();
        let value = value.strip_prefix("RRULE:").unwrap_or(value);
        let invalid = |what: &str| Error::Invalid(format!("recurrence rule `{value}`: {what}"));
        let mut freq = None;
        let mut rule = Self::new(Frequency::Daily);
        for part in value.split(';').filter(|part| !part.is_empty()) {
            let (key, val) = part
                .split_once('=')
                .ok_or_else(|| invalid("expected KEY=VALUE"))?;
            let list = || val.split(',').filter(|item| !item.is_empty());
            match key.to_ascii_uppercase().as_str() {
                "FREQ" => {
                    freq = Some(match val.to_ascii_uppercase().as_str() {
                        "DAILY" => Frequency::Daily,
                        "WEEKLY" => Frequency::Weekly,
                        "MONTHLY" => Frequency::Monthly,
                        "YEARLY" => Frequency::Yearly,
                        _ => return Err(invalid("unsupported FREQ")),
                    })
                }
                "INTERVAL" => {
                    rule.interval = val
                        .parse::<u32>()
                        .map_err(|_| invalid("bad INTERVAL"))?
                        .max(1)
                }
                "COUNT" => rule.count = Some(val.parse().map_err(|_| invalid("bad COUNT"))?),
                "UNTIL" => rule.until = Some(parse_until(val).ok_or_else(|| invalid("bad UNTIL"))?),
                "BYDAY" => {
                    rule.by_day = list()
                        .map(|item| parse_weekday_num(item).ok_or_else(|| invalid("bad BYDAY")))
                        .collect::<Result<_>>()?
                }
                "BYMONTHDAY" => {
                    rule.by_month_day = list()
                        .map(|item| {
                            item.parse::<i8>()
                                .ok()
                                .filter(|day| *day != 0 && (-31..=31).contains(day))
                                .ok_or_else(|| invalid("bad BYMONTHDAY"))
                        })
                        .collect::<Result<_>>()?
                }
                "BYMONTH" => {
                    rule.by_month = list()
                        .map(|item| {
                            item.parse::<u32>()
                                .ok()
                                .filter(|month| (1..=12).contains(month))
                                .ok_or_else(|| invalid("bad BYMONTH"))
                        })
                        .collect::<Result<_>>()?
                }
                "BYSETPOS" => {
                    rule.by_set_pos = list()
                        .map(|item| item.parse::<i32>().map_err(|_| invalid("bad BYSETPOS")))
                        .collect::<Result<_>>()?
                }
                "WKST" => {
                    rule.week_start = parse_weekday(val).ok_or_else(|| invalid("bad WKST"))?
                }
                _ => {}
            }
        }
        rule.freq = freq.ok_or_else(|| invalid("missing FREQ"))?;
        Ok(rule)
    }

    /// Human description, e.g. "Every 2 weeks on Mon, Wed, 10 times".
    pub fn describe(&self) -> String {
        let mut text = if self.interval == 1 {
            match self.freq {
                Frequency::Daily => "Daily".to_owned(),
                Frequency::Weekly => "Weekly".to_owned(),
                Frequency::Monthly => "Monthly".to_owned(),
                Frequency::Yearly => "Yearly".to_owned(),
            }
        } else {
            format!("Every {} {}", self.interval, self.freq.unit(true))
        };
        let weekdays = [
            Weekday::Mon,
            Weekday::Tue,
            Weekday::Wed,
            Weekday::Thu,
            Weekday::Fri,
        ];
        let plain_days: Vec<Weekday> = self
            .by_day
            .iter()
            .filter(|day| day.ordinal.is_none())
            .map(|day| day.weekday)
            .collect();
        let set_pos = match self.by_set_pos.as_slice() {
            [pos] => i8::try_from(*pos).ok(),
            _ => None,
        };
        if !self.by_day.is_empty() && self.by_day.iter().all(|day| day.ordinal.is_none()) {
            if set_pos.is_some() && plain_days.len() == 1 {
                text.push_str(&format!(
                    " on the {} {}",
                    ordinal_name(set_pos.unwrap_or(1)),
                    weekday_long(plain_days[0])
                ));
            } else if plain_days.len() == 5 && weekdays.iter().all(|day| plain_days.contains(day)) {
                text.push_str(" on weekdays");
            } else {
                let names: Vec<&str> = plain_days.iter().map(|day| weekday_short(*day)).collect();
                text.push_str(&format!(" on {}", names.join(", ")));
            }
        } else if !self.by_day.is_empty() {
            let names: Vec<String> = self
                .by_day
                .iter()
                .map(|day| match day.ordinal {
                    Some(n) => format!("{} {}", ordinal_name(n), weekday_long(day.weekday)),
                    None => weekday_long(day.weekday).to_owned(),
                })
                .collect();
            text.push_str(&format!(" on the {}", names.join(", ")));
        } else if !self.by_month_day.is_empty() {
            let names: Vec<String> = self
                .by_month_day
                .iter()
                .map(|day| {
                    if *day == -1 {
                        "last day".to_owned()
                    } else {
                        format!("day {day}")
                    }
                })
                .collect();
            text.push_str(&format!(" on {}", names.join(", ")));
        }
        if !self.by_month.is_empty() && self.freq == Frequency::Yearly {
            let names: Vec<&str> = self.by_month.iter().map(|m| month_name(*m)).collect();
            text.push_str(&format!(" in {}", names.join(", ")));
        }
        if let Some(count) = self.count {
            text.push_str(&format!(
                ", {count} {}",
                if count == 1 { "time" } else { "times" }
            ));
        }
        if let Some(until) = self.until {
            text.push_str(&format!(", until {}", until.format("%-d %b %Y")));
        }
        text
    }

    /// Occurrence starts (UTC) of a series starting at `start` whose instances overlap
    /// `[from, to)`. `duration` is the length of one instance; `exdates` are skipped.
    /// Wall-clock arithmetic happens in `tz`.
    pub fn occurrences<Z: TimeZone>(
        &self,
        start: DateTime<Utc>,
        duration: Duration,
        tz: &Z,
        exdates: &[DateTime<Utc>],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Vec<DateTime<Utc>> {
        let mut found = Vec::new();
        self.expand(start, tz, |occurrence| {
            if occurrence >= to {
                return false;
            }
            if occurrence + duration > from
                && !exdates.contains(&occurrence)
                && found.len() < MAX_OCCURRENCES
            {
                found.push(occurrence);
            }
            found.len() < MAX_OCCURRENCES
        });
        found
    }

    /// True when the series yields an instance starting exactly at `at`.
    pub fn includes<Z: TimeZone>(&self, start: DateTime<Utc>, tz: &Z, at: DateTime<Utc>) -> bool {
        let mut hit = false;
        self.expand(start, tz, |occurrence| {
            hit = occurrence == at;
            occurrence < at
        });
        hit
    }

    /// Calls `visit` with every occurrence start in order until it returns false or the
    /// series ends (`COUNT`/`UNTIL`/safety caps).
    pub fn expand<Z: TimeZone>(
        &self,
        start: DateTime<Utc>,
        tz: &Z,
        mut visit: impl FnMut(DateTime<Utc>) -> bool,
    ) {
        let local_start = start.with_timezone(tz).naive_local();
        let time = local_start.time();
        let start_date = local_start.date();
        let interval = i64::from(self.interval.max(1));
        let mut emitted: u32 = 0;

        for period in 0..MAX_PERIODS {
            let Some(mut dates) = self.period_dates(start_date, period * interval) else {
                return;
            };
            dates.sort();
            dates.dedup();
            if !self.by_set_pos.is_empty() {
                dates = select_positions(&dates, &self.by_set_pos);
            }
            for date in dates {
                if date < start_date {
                    continue;
                }
                let occurrence = to_utc(tz, date.and_time(time));
                if occurrence < start {
                    continue;
                }
                if self.until.is_some_and(|until| occurrence > until) {
                    return;
                }
                if self.count.is_some_and(|count| emitted >= count) {
                    return;
                }
                emitted += 1;
                if !visit(occurrence) || emitted as usize >= MAX_OCCURRENCES * 4 {
                    return;
                }
            }
        }
    }

    /// Candidate dates of the period `offset` periods after the one containing `start`.
    /// `None` when the date arithmetic overflows (ends the expansion).
    fn period_dates(&self, start: NaiveDate, offset: i64) -> Option<Vec<NaiveDate>> {
        let month_ok =
            |date: &NaiveDate| self.by_month.is_empty() || self.by_month.contains(&date.month());
        let day_ok = |date: &NaiveDate| {
            self.by_day.is_empty()
                || self
                    .by_day
                    .iter()
                    .any(|day| day.ordinal.is_none() && day.weekday == date.weekday())
        };
        let month_day_ok = |date: &NaiveDate| {
            self.by_month_day.is_empty()
                || self
                    .by_month_day
                    .iter()
                    .any(|day| resolve_month_day(date.year(), date.month(), *day) == Some(*date))
        };
        Some(match self.freq {
            Frequency::Daily => {
                let date = start.checked_add_signed(Duration::days(offset))?;
                if month_ok(&date) && day_ok(&date) && month_day_ok(&date) {
                    vec![date]
                } else {
                    Vec::new()
                }
            }
            Frequency::Weekly => {
                let back = i64::from(
                    (7 + start.weekday().num_days_from_monday()
                        - self.week_start.num_days_from_monday())
                        % 7,
                );
                let week = start.checked_add_signed(Duration::days(offset * 7 - back))?;
                let weekdays: Vec<Weekday> = if self.by_day.is_empty() {
                    vec![start.weekday()]
                } else {
                    self.by_day.iter().map(|day| day.weekday).collect()
                };
                (0..7)
                    .filter_map(|i| week.checked_add_signed(Duration::days(i)))
                    .filter(|date| weekdays.contains(&date.weekday()) && month_ok(date))
                    .collect()
            }
            Frequency::Monthly => {
                let (year, month) = add_months(start.year(), start.month(), offset)?;
                if !self.by_month.is_empty() && !self.by_month.contains(&month) {
                    return Some(Vec::new());
                }
                self.month_dates(year, month, start.day())
            }
            Frequency::Yearly => {
                let year = start.year().checked_add(i32::try_from(offset).ok()?)?;
                let months = if self.by_month.is_empty() {
                    vec![start.month()]
                } else {
                    self.by_month.clone()
                };
                if self.by_month.is_empty() && !self.by_day.is_empty() {
                    // Yearly by weekday without a month: ordinals count within the year.
                    year_weekdays(year, &self.by_day)
                } else {
                    months
                        .into_iter()
                        .flat_map(|month| self.month_dates(year, month, start.day()))
                        .collect()
                }
            }
        })
    }

    /// Candidate dates inside one month for MONTHLY/YEARLY rules.
    fn month_dates(&self, year: i32, month: u32, default_day: u32) -> Vec<NaiveDate> {
        let mut dates: Vec<NaiveDate> = if !self.by_day.is_empty() {
            self.by_day
                .iter()
                .flat_map(|day| month_weekdays(year, month, *day))
                .collect()
        } else if !self.by_month_day.is_empty() {
            self.by_month_day
                .iter()
                .filter_map(|day| resolve_month_day(year, month, *day))
                .collect()
        } else {
            NaiveDate::from_ymd_opt(year, month, default_day)
                .into_iter()
                .collect()
        };
        if !self.by_day.is_empty() && !self.by_month_day.is_empty() {
            dates.retain(|date| {
                self.by_month_day
                    .iter()
                    .any(|day| resolve_month_day(year, month, *day) == Some(*date))
            });
        }
        dates
    }
}

impl fmt::Display for RRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FREQ={}", self.freq.as_str())?;
        if self.interval > 1 {
            write!(f, ";INTERVAL={}", self.interval)?;
        }
        if let Some(count) = self.count {
            write!(f, ";COUNT={count}")?;
        }
        if let Some(until) = self.until {
            write!(f, ";UNTIL={}", until.format("%Y%m%dT%H%M%SZ"))?;
        }
        if !self.by_day.is_empty() {
            let days: Vec<String> = self
                .by_day
                .iter()
                .map(|day| match day.ordinal {
                    Some(n) => format!("{n}{}", weekday_code(day.weekday)),
                    None => weekday_code(day.weekday).to_owned(),
                })
                .collect();
            write!(f, ";BYDAY={}", days.join(","))?;
        }
        if !self.by_month_day.is_empty() {
            write!(f, ";BYMONTHDAY={}", join(&self.by_month_day))?;
        }
        if !self.by_month.is_empty() {
            write!(f, ";BYMONTH={}", join(&self.by_month))?;
        }
        if !self.by_set_pos.is_empty() {
            write!(f, ";BYSETPOS={}", join(&self.by_set_pos))?;
        }
        if self.week_start != Weekday::Mon {
            write!(f, ";WKST={}", weekday_code(self.week_start))?;
        }
        Ok(())
    }
}

fn join<T: ToString>(items: &[T]) -> String {
    items
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// Converts a wall-clock time to UTC; times skipped by a DST jump move forward one hour,
/// ambiguous times take the earlier instant.
pub fn to_utc<Z: TimeZone>(tz: &Z, local: NaiveDateTime) -> DateTime<Utc> {
    match tz.from_local_datetime(&local) {
        LocalResult::Single(time) => time.with_timezone(&Utc),
        LocalResult::Ambiguous(earliest, _) => earliest.with_timezone(&Utc),
        LocalResult::None => {
            let shifted = local + Duration::hours(1);
            match tz.from_local_datetime(&shifted) {
                LocalResult::Single(time) | LocalResult::Ambiguous(time, _) => {
                    time.with_timezone(&Utc)
                }
                LocalResult::None => local.and_utc(),
            }
        }
    }
}

fn select_positions(dates: &[NaiveDate], positions: &[i32]) -> Vec<NaiveDate> {
    let len = dates.len() as i32;
    let mut picked: Vec<NaiveDate> = positions
        .iter()
        .filter_map(|pos| {
            let index = if *pos > 0 { pos - 1 } else { len + pos };
            usize::try_from(index)
                .ok()
                .and_then(|i| dates.get(i).copied())
        })
        .collect();
    picked.sort();
    picked.dedup();
    picked
}

fn add_months(year: i32, month: u32, offset: i64) -> Option<(i32, u32)> {
    let total = i64::from(year) * 12 + i64::from(month) - 1 + offset;
    let year = i32::try_from(total.div_euclid(12)).ok()?;
    let month = u32::try_from(total.rem_euclid(12) + 1).ok()?;
    Some((year, month))
}

/// Number of days in a month.
pub fn days_in_month(year: i32, month: u32) -> u32 {
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    NaiveDate::from_ymd_opt(next_year, next_month, 1)
        .and_then(|first| first.pred_opt())
        .map_or(28, |last| last.day())
}

fn resolve_month_day(year: i32, month: u32, day: i8) -> Option<NaiveDate> {
    let len = days_in_month(year, month) as i32;
    let day = i32::from(day);
    let resolved = if day > 0 { day } else { len + day + 1 };
    if resolved < 1 || resolved > len {
        return None;
    }
    NaiveDate::from_ymd_opt(year, month, u32::try_from(resolved).ok()?)
}

fn month_weekdays(year: i32, month: u32, day: WeekdayNum) -> Vec<NaiveDate> {
    let all: Vec<NaiveDate> = (1..=days_in_month(year, month))
        .filter_map(|d| NaiveDate::from_ymd_opt(year, month, d))
        .filter(|date| date.weekday() == day.weekday)
        .collect();
    pick_ordinal(all, day.ordinal)
}

fn year_weekdays(year: i32, days: &[WeekdayNum]) -> Vec<NaiveDate> {
    let Some(first) = NaiveDate::from_ymd_opt(year, 1, 1) else {
        return Vec::new();
    };
    let all: Vec<NaiveDate> = first.iter_days().take_while(|d| d.year() == year).collect();
    days.iter()
        .flat_map(|day| {
            let matching = all
                .iter()
                .copied()
                .filter(|date| date.weekday() == day.weekday)
                .collect();
            pick_ordinal(matching, day.ordinal)
        })
        .collect()
}

fn pick_ordinal(dates: Vec<NaiveDate>, ordinal: Option<i8>) -> Vec<NaiveDate> {
    match ordinal {
        None => dates,
        Some(n) if n > 0 => dates.get(n as usize - 1).copied().into_iter().collect(),
        Some(n) => {
            let index = dates.len() as i64 + i64::from(n);
            usize::try_from(index)
                .ok()
                .and_then(|i| dates.get(i).copied())
                .into_iter()
                .collect()
        }
    }
}

fn parse_until(value: &str) -> Option<DateTime<Utc>> {
    let value = value.trim();
    if let Ok(date) = NaiveDate::parse_from_str(value, "%Y%m%d") {
        // A date-only UNTIL includes that whole day.
        return Some(date.and_hms_opt(23, 59, 59)?.and_utc());
    }
    let naive = value.trim_end_matches('Z');
    NaiveDateTime::parse_from_str(naive, "%Y%m%dT%H%M%S")
        .ok()
        .map(|time| time.and_utc())
}

fn parse_weekday_num(value: &str) -> Option<WeekdayNum> {
    let value = value.trim();
    if value.len() < 2 {
        return None;
    }
    let (ordinal, code) = value.split_at(value.len() - 2);
    let weekday = parse_weekday(code)?;
    let ordinal = if ordinal.is_empty() {
        None
    } else {
        let n = ordinal.trim_start_matches('+').parse::<i8>().ok()?;
        (n != 0).then_some(n)
    };
    Some(WeekdayNum { ordinal, weekday })
}

/// Parses a two-letter iCalendar weekday code (`MO`..`SU`).
pub fn parse_weekday(code: &str) -> Option<Weekday> {
    Some(match code.to_ascii_uppercase().as_str() {
        "MO" => Weekday::Mon,
        "TU" => Weekday::Tue,
        "WE" => Weekday::Wed,
        "TH" => Weekday::Thu,
        "FR" => Weekday::Fri,
        "SA" => Weekday::Sat,
        "SU" => Weekday::Sun,
        _ => return None,
    })
}

/// Two-letter iCalendar weekday code.
pub fn weekday_code(day: Weekday) -> &'static str {
    match day {
        Weekday::Mon => "MO",
        Weekday::Tue => "TU",
        Weekday::Wed => "WE",
        Weekday::Thu => "TH",
        Weekday::Fri => "FR",
        Weekday::Sat => "SA",
        Weekday::Sun => "SU",
    }
}

pub fn weekday_short(day: Weekday) -> &'static str {
    match day {
        Weekday::Mon => "Mon",
        Weekday::Tue => "Tue",
        Weekday::Wed => "Wed",
        Weekday::Thu => "Thu",
        Weekday::Fri => "Fri",
        Weekday::Sat => "Sat",
        Weekday::Sun => "Sun",
    }
}

pub fn weekday_long(day: Weekday) -> &'static str {
    match day {
        Weekday::Mon => "Monday",
        Weekday::Tue => "Tuesday",
        Weekday::Wed => "Wednesday",
        Weekday::Thu => "Thursday",
        Weekday::Fri => "Friday",
        Weekday::Sat => "Saturday",
        Weekday::Sun => "Sunday",
    }
}

/// "first", "second", ..., "last".
pub fn ordinal_name(n: i8) -> &'static str {
    match n {
        1 => "first",
        2 => "second",
        3 => "third",
        4 => "fourth",
        5 => "fifth",
        -1 => "last",
        -2 => "second to last",
        _ => "n-th",
    }
}

pub fn month_name(month: u32) -> &'static str {
    match month {
        1 => "January",
        2 => "February",
        3 => "March",
        4 => "April",
        5 => "May",
        6 => "June",
        7 => "July",
        8 => "August",
        9 => "September",
        10 => "October",
        11 => "November",
        _ => "December",
    }
}

/// Which occurrence of its weekday `date` is in its month: (n-th from start, is last).
pub fn weekday_position(date: NaiveDate) -> (i8, bool) {
    let nth = ((date.day() - 1) / 7 + 1) as i8;
    let is_last = date.day() + 7 > days_in_month(date.year(), date.month());
    (nth, is_last)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_tz::Europe::Paris;

    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    fn all(rule: &str, start: DateTime<Utc>, limit: usize) -> Vec<DateTime<Utc>> {
        let rule = RRule::parse(rule).unwrap();
        let mut out = Vec::new();
        rule.expand(start, &Utc, |at| {
            out.push(at);
            out.len() < limit
        });
        out
    }

    fn dates(times: &[DateTime<Utc>]) -> Vec<String> {
        times
            .iter()
            .map(|t| t.format("%Y-%m-%d").to_string())
            .collect()
    }

    #[test]
    fn parse_and_format_roundtrip() {
        for value in [
            "FREQ=DAILY",
            "FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE",
            "FREQ=MONTHLY;BYDAY=2TU",
            "FREQ=MONTHLY;BYMONTHDAY=-1",
            "FREQ=YEARLY;COUNT=5;BYDAY=-1SU;BYMONTH=3",
            "FREQ=WEEKLY;UNTIL=20261231T235959Z;BYDAY=FR",
            "FREQ=MONTHLY;BYDAY=TU;BYSETPOS=2",
        ] {
            assert_eq!(RRule::parse(value).unwrap().to_string(), value);
        }
        assert_eq!(
            RRule::parse("RRULE:freq=weekly;byday=mo").unwrap(),
            RRule::new(Frequency::Weekly).by_day([WeekdayNum::every(Weekday::Mon)])
        );
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(RRule::parse("").is_err());
        assert!(RRule::parse("FREQ=HOURLY").is_err());
        assert!(RRule::parse("FREQ=DAILY;COUNT=x").is_err());
        assert!(RRule::parse("FREQ=WEEKLY;BYDAY=XX").is_err());
        assert!(RRule::parse("FREQ=MONTHLY;BYMONTHDAY=0").is_err());
    }

    #[test]
    fn date_only_until_includes_the_day() {
        let rule = RRule::parse("FREQ=DAILY;UNTIL=20261105").unwrap();
        let start = utc(2026, 11, 3, 9, 0);
        let mut out = Vec::new();
        rule.expand(start, &Utc, |at| {
            out.push(at);
            true
        });
        assert_eq!(dates(&out), ["2026-11-03", "2026-11-04", "2026-11-05"]);
    }

    #[test]
    fn daily_with_interval_and_count() {
        let out = all("FREQ=DAILY;INTERVAL=3;COUNT=4", utc(2026, 1, 30, 8, 0), 100);
        assert_eq!(
            dates(&out),
            ["2026-01-30", "2026-02-02", "2026-02-05", "2026-02-08"]
        );
        assert!(out.iter().all(|t| t.format("%H:%M").to_string() == "08:00"));
    }

    #[test]
    fn weekly_on_chosen_days() {
        // 2026-11-02 is a Monday.
        let out = all(
            "FREQ=WEEKLY;BYDAY=MO,WE,FR;COUNT=5",
            utc(2026, 11, 2, 9, 0),
            100,
        );
        assert_eq!(
            dates(&out),
            [
                "2026-11-02",
                "2026-11-04",
                "2026-11-06",
                "2026-11-09",
                "2026-11-11"
            ]
        );
    }

    #[test]
    fn weekly_every_other_week_starting_mid_week() {
        // Starts Wednesday; Monday of the first week is before the start and skipped.
        let out = all(
            "FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE",
            utc(2026, 11, 4, 9, 0),
            4,
        );
        assert_eq!(
            dates(&out),
            ["2026-11-04", "2026-11-16", "2026-11-18", "2026-11-30"]
        );
    }

    #[test]
    fn weekly_without_byday_uses_start_weekday() {
        let out = all("FREQ=WEEKLY", utc(2026, 11, 5, 9, 0), 3);
        assert_eq!(dates(&out), ["2026-11-05", "2026-11-12", "2026-11-19"]);
    }

    #[test]
    fn monthly_by_date_skips_short_months() {
        let out = all("FREQ=MONTHLY", utc(2026, 1, 31, 9, 0), 4);
        assert_eq!(
            dates(&out),
            ["2026-01-31", "2026-03-31", "2026-05-31", "2026-07-31"]
        );
    }

    #[test]
    fn monthly_last_day() {
        let out = all("FREQ=MONTHLY;BYMONTHDAY=-1", utc(2026, 1, 31, 9, 0), 3);
        assert_eq!(dates(&out), ["2026-01-31", "2026-02-28", "2026-03-31"]);
    }

    #[test]
    fn monthly_second_tuesday_and_last_friday() {
        let out = all("FREQ=MONTHLY;BYDAY=2TU", utc(2026, 11, 10, 9, 0), 3);
        assert_eq!(dates(&out), ["2026-11-10", "2026-12-08", "2027-01-12"]);
        let out = all("FREQ=MONTHLY;BYDAY=-1FR", utc(2026, 11, 27, 9, 0), 2);
        assert_eq!(dates(&out), ["2026-11-27", "2026-12-25"]);
        let out = all(
            "FREQ=MONTHLY;BYDAY=TU;BYSETPOS=2",
            utc(2026, 11, 10, 9, 0),
            2,
        );
        assert_eq!(dates(&out), ["2026-11-10", "2026-12-08"]);
    }

    #[test]
    fn yearly_variants() {
        let out = all("FREQ=YEARLY", utc(2024, 2, 29, 0, 0), 2);
        assert_eq!(dates(&out), ["2024-02-29", "2028-02-29"]);
        let out = all(
            "FREQ=YEARLY;BYMONTH=11;BYDAY=4TH",
            utc(2026, 11, 26, 12, 0),
            2,
        );
        assert_eq!(dates(&out), ["2026-11-26", "2027-11-25"]);
        let out = all("FREQ=YEARLY;INTERVAL=2", utc(2026, 6, 1, 12, 0), 2);
        assert_eq!(dates(&out), ["2026-06-01", "2028-06-01"]);
    }

    #[test]
    fn until_is_inclusive() {
        let out = all(
            "FREQ=DAILY;UNTIL=20261105T090000Z",
            utc(2026, 11, 3, 9, 0),
            100,
        );
        assert_eq!(dates(&out), ["2026-11-03", "2026-11-04", "2026-11-05"]);
    }

    #[test]
    fn window_and_exdates() {
        let rule = RRule::parse("FREQ=DAILY").unwrap();
        let start = utc(2026, 11, 1, 9, 0);
        let skipped = utc(2026, 11, 11, 9, 0);
        let found = rule.occurrences(
            start,
            Duration::hours(1),
            &Utc,
            &[skipped],
            utc(2026, 11, 10, 9, 30),
            utc(2026, 11, 13, 0, 0),
        );
        // The 10th overlaps the window start (09:00-10:00 vs 09:30).
        assert_eq!(dates(&found), ["2026-11-10", "2026-11-12"]);
        assert!(rule.includes(start, &Utc, skipped));
        assert!(!rule.includes(start, &Utc, skipped + Duration::minutes(1)));
    }

    #[test]
    fn keeps_wall_clock_across_dst() {
        // 09:00 Paris: UTC+2 in October, UTC+1 after the 25 Oct 2026 change.
        let start = Paris
            .with_ymd_and_hms(2026, 10, 22, 9, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        let rule = RRule::parse("FREQ=WEEKLY;COUNT=2").unwrap();
        let mut out = Vec::new();
        rule.expand(start, &Paris, |at| {
            out.push(at);
            true
        });
        assert_eq!(out[0], utc(2026, 10, 22, 7, 0));
        assert_eq!(out[1], utc(2026, 10, 29, 8, 0));
    }

    #[test]
    fn describe_reads_naturally() {
        let describe = |rule: &str| RRule::parse(rule).unwrap().describe();
        assert_eq!(describe("FREQ=DAILY"), "Daily");
        assert_eq!(
            describe("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR"),
            "Weekly on weekdays"
        );
        assert_eq!(
            describe("FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE;COUNT=10"),
            "Every 2 weeks on Mon, Wed, 10 times"
        );
        assert_eq!(
            describe("FREQ=MONTHLY;BYDAY=2TU"),
            "Monthly on the second Tuesday"
        );
        assert_eq!(
            describe("FREQ=MONTHLY;BYMONTHDAY=-1"),
            "Monthly on last day"
        );
    }

    #[test]
    fn weekday_position_helper() {
        let date = NaiveDate::from_ymd_opt(2026, 11, 24).unwrap();
        assert_eq!(weekday_position(date), (4, true));
        let date = NaiveDate::from_ymd_opt(2026, 11, 10).unwrap();
        assert_eq!(weekday_position(date), (2, false));
    }
}
