//! Dates formatted in the app's language.
//!
//! chrono's `format` always writes English day and month names. [`LocalizedFormat::format_in`]
//! takes the same strftime pattern but swaps `%a`, `%A`, `%b` and `%B` for names in the chosen
//! language first:
//!
//! ```ignore
//! date.format_in(Language::Fr, "%a %-d %b") // "lun. 15 nov."
//! date.format_in(Language::Fr, "%-d %B %Y") // "15 novembre 2026"
//! ```

use std::fmt::Display;

use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, TimeZone, Weekday};

use crate::settings::Language;

const FR_DAYS_SHORT: [&str; 7] = ["lun.", "mar.", "mer.", "jeu.", "ven.", "sam.", "dim."];
const FR_DAYS_LONG: [&str; 7] = [
    "lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi", "dimanche",
];
const FR_MONTHS_SHORT: [&str; 12] = [
    "janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.", "nov.",
    "déc.",
];
const FR_MONTHS_LONG: [&str; 12] = [
    "janvier",
    "février",
    "mars",
    "avril",
    "mai",
    "juin",
    "juillet",
    "août",
    "septembre",
    "octobre",
    "novembre",
    "décembre",
];

/// Short weekday name: "Mon" / "lun.".
pub fn weekday_short(language: Language, day: Weekday) -> &'static str {
    let index = day.num_days_from_monday() as usize;
    match language {
        Language::En => ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"][index],
        Language::Fr => FR_DAYS_SHORT[index],
    }
}

/// Full weekday name: "Monday" / "lundi".
pub fn weekday_long(language: Language, day: Weekday) -> &'static str {
    let index = day.num_days_from_monday() as usize;
    match language {
        Language::En => crate::recurrence::weekday_long(day),
        Language::Fr => FR_DAYS_LONG[index],
    }
}

/// Short month name for `month` in 1..=12: "Nov" / "nov.".
pub fn month_short(language: Language, month: u32) -> &'static str {
    let index = month.clamp(1, 12) as usize - 1;
    match language {
        Language::En => [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ][index],
        Language::Fr => FR_MONTHS_SHORT[index],
    }
}

/// Full month name for `month` in 1..=12: "November" / "novembre".
pub fn month_long(language: Language, month: u32) -> &'static str {
    match language {
        Language::En => crate::recurrence::month_name(month),
        Language::Fr => FR_MONTHS_LONG[month.clamp(1, 12) as usize - 1],
    }
}

/// `pattern` with `%a`, `%A`, `%b` and `%B` replaced by `date`'s names in `language`.
/// Other specifiers (and `%%`) are left for chrono.
fn localize_pattern(language: Language, date: &impl Datelike, pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() + 16);
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('a') => out.push_str(weekday_short(language, date.weekday())),
            Some('A') => out.push_str(weekday_long(language, date.weekday())),
            Some('b') => out.push_str(month_short(language, date.month())),
            Some('B') => out.push_str(month_long(language, date.month())),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

/// chrono's `format`, with day and month names in the app's language.
pub trait LocalizedFormat {
    fn format_in(&self, language: Language, pattern: &str) -> String;
}

impl LocalizedFormat for NaiveDate {
    fn format_in(&self, language: Language, pattern: &str) -> String {
        self.format(&localize_pattern(language, self, pattern))
            .to_string()
    }
}

impl LocalizedFormat for NaiveDateTime {
    fn format_in(&self, language: Language, pattern: &str) -> String {
        self.format(&localize_pattern(language, self, pattern))
            .to_string()
    }
}

impl<Tz: TimeZone> LocalizedFormat for DateTime<Tz>
where
    Tz::Offset: Display,
{
    fn format_in(&self, language: Language, pattern: &str) -> String {
        self.format(&localize_pattern(language, self, pattern))
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, Utc};

    fn monday() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 16).unwrap()
    }

    #[test]
    fn french_names() {
        let day = monday();
        assert_eq!(day.format_in(Language::Fr, "%a %-d %b"), "lun. 16 nov.");
        assert_eq!(day.format_in(Language::Fr, "%-d %B %Y"), "16 novembre 2026");
        assert_eq!(day.format_in(Language::Fr, "%A"), "lundi");
        let august = NaiveDate::from_ymd_opt(2026, 8, 2).unwrap();
        assert_eq!(august.format_in(Language::Fr, "%a %d %b"), "dim. 02 août");
    }

    #[test]
    fn english_matches_chrono() {
        let at = monday().and_hms_opt(14, 5, 0).unwrap();
        for pattern in ["%a %-d %b %Y, %H:%M", "%A %e %B %Y", "%B %Y", "%a %H:%M"] {
            assert_eq!(
                at.format_in(Language::En, pattern),
                at.format(pattern).to_string()
            );
        }
    }

    #[test]
    fn keeps_other_specifiers() {
        let at = Utc.with_ymd_and_hms(2026, 11, 16, 14, 5, 0).unwrap();
        assert_eq!(at.format_in(Language::Fr, "%H:%M"), "14:05");
        assert_eq!(at.format_in(Language::Fr, "100%% %b"), "100% nov.");
    }

    #[test]
    fn every_month_and_day() {
        for month in 1..=12 {
            assert!(!month_short(Language::Fr, month).is_empty());
            assert!(!month_long(Language::Fr, month).is_empty());
        }
        assert_eq!(month_long(Language::Fr, 12), "décembre");
        assert_eq!(weekday_short(Language::En, Weekday::Sun), "Sun");
        assert_eq!(weekday_long(Language::Fr, Weekday::Sun), "dimanche");
    }
}
