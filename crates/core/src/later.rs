//! "Later" mail features: snooze (a message comes back to its folder at a chosen time),
//! schedule send (a saved draft is queued at a chosen time) and muted conversations
//! (new messages of the thread skip the inbox).
//!
//! State is one JSON blob in the settings table ([`KEY`]); persist with [`Later::command`].
//! The UI scheduler calls [`Later::take_due_snoozes`] / [`Later::take_due_sends`].

use std::collections::BTreeSet;

use chrono::{DateTime, Datelike, Duration, NaiveTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::settings::{get_json, set_json_command};
use crate::{Command, Id, Result, Store};

/// Settings key of the [`Later`] blob.
pub const KEY: &str = "mail.later";
/// Custom folder (per account) that holds snoozed messages.
pub const SNOOZED_FOLDER: &str = "Snoozed";

/// A snoozed message: moved to [`SNOOZED_FOLDER`], back to `return_folder` at `until`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snoozed {
    pub message_id: Id,
    pub until: DateTime<Utc>,
    pub return_folder: Id,
}

/// A draft (still in Drafts) to queue for sending at `at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scheduled {
    pub message_id: Id,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Later {
    pub snoozed: Vec<Snoozed>,
    pub scheduled: Vec<Scheduled>,
    /// Thread ids whose new messages are marked read and archived.
    pub muted_threads: BTreeSet<String>,
}

impl Later {
    /// Stored state, empty when never saved.
    pub fn load(store: &Store) -> Result<Self> {
        Ok(get_json(store, KEY)?.unwrap_or_default())
    }

    /// Command that persists this state.
    pub fn command(&self) -> Result<Command> {
        set_json_command(KEY, self)
    }

    /// Snoozes a message (replacing an earlier snooze of the same message).
    pub fn snooze(&mut self, message_id: Id, until: DateTime<Utc>, return_folder: Id) {
        self.snoozed.retain(|entry| entry.message_id != message_id);
        self.snoozed.push(Snoozed {
            message_id,
            until,
            return_folder,
        });
    }

    /// Schedules a draft (replacing an earlier schedule of the same draft).
    pub fn schedule(&mut self, message_id: Id, at: DateTime<Utc>) {
        self.scheduled
            .retain(|entry| entry.message_id != message_id);
        self.scheduled.push(Scheduled { message_id, at });
    }

    pub fn scheduled_at(&self, message_id: Id) -> Option<DateTime<Utc>> {
        self.scheduled
            .iter()
            .find(|entry| entry.message_id == message_id)
            .map(|entry| entry.at)
    }

    /// Removes and returns the snoozes whose time has come.
    pub fn take_due_snoozes(&mut self, now: DateTime<Utc>) -> Vec<Snoozed> {
        let (due, waiting) = std::mem::take(&mut self.snoozed)
            .into_iter()
            .partition(|entry| entry.until <= now);
        self.snoozed = waiting;
        due
    }

    /// Removes and returns the scheduled sends whose time has come.
    pub fn take_due_sends(&mut self, now: DateTime<Utc>) -> Vec<Scheduled> {
        let (due, waiting) = std::mem::take(&mut self.scheduled)
            .into_iter()
            .partition(|entry| entry.at <= now);
        self.scheduled = waiting;
        due
    }

    /// Mutes or unmutes a conversation. Returns whether it is muted now.
    pub fn toggle_mute(&mut self, thread_id: &str) -> bool {
        if self.muted_threads.remove(thread_id) {
            false
        } else {
            self.muted_threads.insert(thread_id.to_owned());
            true
        }
    }

    pub fn is_muted(&self, thread_id: &str) -> bool {
        !thread_id.is_empty() && self.muted_threads.contains(thread_id)
    }
}

/// Quick choices for snooze and send later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaterPreset {
    InOneHour,
    LaterToday,
    TomorrowMorning,
    NextWeek,
}

/// Hour used by "tomorrow" / "next week".
const MORNING_HOUR: u32 = 8;

impl LaterPreset {
    pub const ALL: [LaterPreset; 4] = [
        Self::InOneHour,
        Self::LaterToday,
        Self::TomorrowMorning,
        Self::NextWeek,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::InOneHour => "In 1 hour",
            Self::LaterToday => "Later today (+3 h)",
            Self::TomorrowMorning => "Tomorrow, 08:00",
            Self::NextWeek => "Next Monday, 08:00",
        }
    }

    /// The moment this preset means, from `now` in the user's time zone.
    pub fn at<Tz: TimeZone>(self, now: DateTime<Tz>) -> DateTime<Utc> {
        let morning = |days: i64| {
            let date = now.date_naive() + Duration::days(days);
            let naive = date.and_time(NaiveTime::MIN) + Duration::hours(i64::from(MORNING_HOUR));
            now.timezone()
                .from_local_datetime(&naive)
                .earliest()
                .map(|local| local.with_timezone(&Utc))
                // A DST gap at 08:00 does not exist in practice; fall back to a day later.
                .unwrap_or_else(|| now.with_timezone(&Utc) + Duration::days(days))
        };
        match self {
            Self::InOneHour => now.with_timezone(&Utc) + Duration::hours(1),
            Self::LaterToday => now.with_timezone(&Utc) + Duration::hours(3),
            Self::TomorrowMorning => morning(1),
            Self::NextWeek => {
                let ahead = 7 - i64::from(now.weekday().num_days_from_monday());
                morning(ahead)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn utc(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    #[test]
    fn due_entries_are_taken_once() {
        let mut later = Later::default();
        later.snooze(1, utc("2026-01-01T10:00:00Z"), 7);
        later.snooze(2, utc("2026-01-01T12:00:00Z"), 7);
        later.snooze(1, utc("2026-01-01T09:00:00Z"), 8); // replaces
        later.schedule(5, utc("2026-01-01T11:00:00Z"));

        let due = later.take_due_snoozes(utc("2026-01-01T10:30:00Z"));
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].return_folder, 8);
        assert_eq!(later.snoozed.len(), 1);
        assert!(later.take_due_sends(utc("2026-01-01T10:30:00Z")).is_empty());
        assert_eq!(later.scheduled_at(5), Some(utc("2026-01-01T11:00:00Z")));
        assert_eq!(later.take_due_sends(utc("2026-01-01T11:00:00Z")).len(), 1);
        assert!(later.scheduled.is_empty());
    }

    #[test]
    fn mute_toggles() {
        let mut later = Later::default();
        assert!(later.toggle_mute("t1"));
        assert!(later.is_muted("t1"));
        assert!(!later.is_muted(""));
        assert!(!later.toggle_mute("t1"));
        assert!(!later.is_muted("t1"));
    }

    #[test]
    fn presets_resolve_in_local_time() {
        let paris = FixedOffset::east_opt(3600).unwrap();
        // Wednesday 2026-01-07 22:30 local.
        let now = paris.with_ymd_and_hms(2026, 1, 7, 22, 30, 0).unwrap();
        assert_eq!(LaterPreset::InOneHour.at(now), utc("2026-01-07T22:30:00Z"));
        assert_eq!(
            LaterPreset::TomorrowMorning.at(now),
            utc("2026-01-08T07:00:00Z")
        );
        assert_eq!(LaterPreset::NextWeek.at(now), utc("2026-01-12T07:00:00Z"));
        // On a Monday, "next week" is the following Monday.
        let monday = paris.with_ymd_and_hms(2026, 1, 12, 7, 0, 0).unwrap();
        assert_eq!(
            LaterPreset::NextWeek.at(monday),
            utc("2026-01-19T07:00:00Z")
        );
    }

    #[test]
    fn persists_through_settings() {
        let store = Store::open_in_memory().unwrap();
        let mut later = Later::default();
        later.toggle_mute("thread");
        crate::command::apply(&store, later.command().unwrap()).unwrap();
        assert_eq!(Later::load(&store).unwrap(), later);
    }
}
