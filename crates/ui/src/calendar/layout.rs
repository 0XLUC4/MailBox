//! Pure calendar layout helpers: visible ranges, titles, overlap columns, lane packing.
//! No GPUI here, so everything is unit-tested.

use chrono::{
    DateTime, Datelike, Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, Utc, Weekday,
};
use mailbox_core::calendar::Occurrence;
use mailbox_core::recurrence::to_utc;

use crate::components::IconName;

pub const WEEK_START: Weekday = Weekday::Mon;
pub const MINUTES_PER_DAY: i64 = 24 * 60;
/// Drag / click granularity.
pub const SNAP_MINUTES: i64 = 15;
/// Working hours, shaded differently.
pub const WORK_START_HOUR: u32 = 8;
pub const WORK_END_HOUR: u32 = 18;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ViewKind {
    Day,
    WorkWeek,
    Week,
    Month,
    Agenda,
}

impl ViewKind {
    pub const ALL: [ViewKind; 5] = [
        Self::Day,
        Self::WorkWeek,
        Self::Week,
        Self::Month,
        Self::Agenda,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Day => "Day",
            Self::WorkWeek => "Work week",
            Self::Week => "Week",
            Self::Month => "Month",
            Self::Agenda => "Agenda",
        }
    }

    pub fn icon(self) -> IconName {
        match self {
            Self::Day => IconName::CalendarDays,
            Self::WorkWeek => IconName::CalendarRange,
            Self::Week => IconName::CalendarRange,
            Self::Month => IconName::Calendar,
            Self::Agenda => IconName::List,
        }
    }

    /// Keyboard shortcut shown in tooltips.
    pub fn shortcut(self) -> &'static str {
        match self {
            Self::Day => "D",
            Self::WorkWeek => "X",
            Self::Week => "W",
            Self::Month => "M",
            Self::Agenda => "A",
        }
    }

    /// True for the hour-grid views.
    pub fn is_time_grid(self) -> bool {
        matches!(self, Self::Day | Self::WorkWeek | Self::Week)
    }
}

/// Number of days in the agenda list.
pub const AGENDA_DAYS: i64 = 30;

pub fn start_of_week(date: NaiveDate) -> NaiveDate {
    let back = (7 + date.weekday().num_days_from_monday() - WEEK_START.num_days_from_monday()) % 7;
    date - Duration::days(i64::from(back))
}

pub fn first_of_month(date: NaiveDate) -> NaiveDate {
    date.with_day(1).unwrap_or(date)
}

/// Adds months, clamping the day (31 Jan + 1 month = 28 Feb).
pub fn add_months(date: NaiveDate, months: i32) -> NaiveDate {
    let total = date.year() * 12 + date.month0() as i32 + months;
    let (year, month) = (total.div_euclid(12), total.rem_euclid(12) as u32 + 1);
    let day = date
        .day()
        .min(mailbox_core::recurrence::days_in_month(year, month));
    NaiveDate::from_ymd_opt(year, month, day).unwrap_or(date)
}

/// Days shown by a view around `anchor`.
pub fn visible_days(kind: ViewKind, anchor: NaiveDate) -> Vec<NaiveDate> {
    let (start, count) = match kind {
        ViewKind::Day => (anchor, 1),
        ViewKind::WorkWeek => (start_of_week(anchor), 5),
        ViewKind::Week => (start_of_week(anchor), 7),
        ViewKind::Month => (start_of_week(first_of_month(anchor)), 42),
        ViewKind::Agenda => (anchor, AGENDA_DAYS),
    };
    (0..count).map(|i| start + Duration::days(i)).collect()
}

/// The 6x7 days of a month grid (also used by the mini month).
pub fn month_grid(month: NaiveDate) -> Vec<NaiveDate> {
    visible_days(ViewKind::Month, month)
}

/// Previous/next period.
pub fn step(kind: ViewKind, anchor: NaiveDate, forward: bool) -> NaiveDate {
    let sign = if forward { 1 } else { -1 };
    match kind {
        ViewKind::Day => anchor + Duration::days(sign),
        ViewKind::WorkWeek | ViewKind::Week => anchor + Duration::weeks(sign),
        ViewKind::Month => add_months(anchor, sign as i32),
        ViewKind::Agenda => anchor + Duration::days(sign * AGENDA_DAYS),
    }
}

/// Toolbar title for the visible range.
pub fn title(kind: ViewKind, anchor: NaiveDate) -> String {
    match kind {
        ViewKind::Day => anchor.format("%A %-d %B %Y").to_string(),
        ViewKind::Month => anchor.format("%B %Y").to_string(),
        _ => {
            let days = visible_days(kind, anchor);
            let (first, last) = (days[0], days[days.len() - 1]);
            if first.month() == last.month() {
                format!("{} – {}", first.format("%-d"), last.format("%-d %B %Y"))
            } else if first.year() == last.year() {
                format!("{} – {}", first.format("%-d %b"), last.format("%-d %b %Y"))
            } else {
                format!("{} – {}", first.format("%-d %b %Y"), last.format("%-d %b %Y"))
            }
        }
    }
}

/// UTC bounds covering the given local days (inclusive).
pub fn utc_range(first: NaiveDate, last: NaiveDate) -> (DateTime<Utc>, DateTime<Utc>) {
    // All-day events are stored at UTC midnight; widen by a day so they are never missed.
    let from = to_utc(&Local, first.and_time(NaiveTime::MIN)) - Duration::days(1);
    let to = to_utc(&Local, (last + Duration::days(1)).and_time(NaiveTime::MIN)) + Duration::days(1);
    (from, to)
}

/// Local wall-clock start/end of an occurrence. All-day events keep their stored dates.
pub fn local_span(occurrence: &Occurrence) -> (NaiveDateTime, NaiveDateTime) {
    let event = &occurrence.event;
    if event.all_day {
        (event.start.naive_utc(), event.end.naive_utc())
    } else {
        (
            event.start.with_timezone(&Local).naive_local(),
            event.end.with_timezone(&Local).naive_local(),
        )
    }
}

/// Shown in the all-day row (all-day or lasting 24 hours or more).
pub fn is_all_day_like(occurrence: &Occurrence) -> bool {
    occurrence.event.all_day || occurrence.duration() >= Duration::hours(24)
}

/// First and last local day an occurrence covers (an end at midnight is exclusive).
pub fn day_span(occurrence: &Occurrence) -> (NaiveDate, NaiveDate) {
    let (start, end) = local_span(occurrence);
    let last = if end > start && end.time() == NaiveTime::MIN {
        end.date() - Duration::days(1)
    } else {
        end.date()
    };
    (start.date(), last.max(start.date()))
}

/// Minutes from midnight of `day` covered by a timed occurrence, clipped to the day.
pub fn minutes_on(occurrence: &Occurrence, day: NaiveDate) -> Option<(i64, i64)> {
    let (start, end) = local_span(occurrence);
    let midnight = day.and_time(NaiveTime::MIN);
    let from = (start - midnight).num_minutes().max(0);
    let to = (end - midnight).num_minutes().min(MINUTES_PER_DAY);
    let zero_length_here = start == end && start.date() == day;
    (from < to || zero_length_here).then_some((from, to.max(from)))
}

/// Placement of an item among overlapping ones: `column` of `columns`, spanning `span`
/// columns (it grows right into free columns).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub column: usize,
    pub columns: usize,
    pub span: usize,
}

/// Lays out `[start, end)` intervals of one day into side-by-side columns.
/// Items must be sorted by start; zero-length items take `min_len` for overlap purposes.
pub fn layout_columns(items: &[(i64, i64)], min_len: i64) -> Vec<Placement> {
    let mut placements = vec![
        Placement {
            column: 0,
            columns: 1,
            span: 1,
        };
        items.len()
    ];
    let end_of = |index: usize| {
        let (start, end) = items[index];
        end.max(start + min_len)
    };
    let mut cluster: Vec<usize> = Vec::new();
    let mut cluster_end = i64::MIN;
    let mut column_ends: Vec<i64> = Vec::new();

    let flush = |cluster: &mut Vec<usize>, column_ends: &mut Vec<i64>, placements: &mut [Placement]| {
        let columns = column_ends.len().max(1);
        for &index in cluster.iter() {
            placements[index].columns = columns;
        }
        // Grow each item right while the next columns are free during its time.
        for &index in cluster.iter() {
            let (start, end) = (items[index].0, end_of(index));
            let column = placements[index].column;
            let mut span = 1;
            while column + span < columns {
                let blocked = cluster.iter().any(|&other| {
                    placements[other].column == column + span
                        && items[other].0 < end
                        && end_of(other) > start
                });
                if blocked {
                    break;
                }
                span += 1;
            }
            placements[index].span = span;
        }
        cluster.clear();
        column_ends.clear();
    };

    for (index, &(start, _)) in items.iter().enumerate() {
        if !cluster.is_empty() && start >= cluster_end {
            flush(&mut cluster, &mut column_ends, &mut placements);
            cluster_end = i64::MIN;
        }
        let end = end_of(index);
        let column = match column_ends.iter().position(|&column_end| column_end <= start) {
            Some(column) => {
                column_ends[column] = end;
                column
            }
            None => {
                column_ends.push(end);
                column_ends.len() - 1
            }
        };
        placements[index].column = column;
        cluster.push(index);
        cluster_end = cluster_end.max(end);
    }
    flush(&mut cluster, &mut column_ends, &mut placements);
    placements
}

/// Packs inclusive `[first, last]` day-index ranges into lanes (rows) without overlap.
/// Returns the lane of each range; ranges should be sorted by start then length.
pub fn pack_lanes(ranges: &[(usize, usize)]) -> Vec<usize> {
    let mut lane_ends: Vec<usize> = Vec::new();
    ranges
        .iter()
        .map(|&(first, last)| {
            match lane_ends.iter().position(|&end| end < first) {
                Some(lane) => {
                    lane_ends[lane] = last;
                    lane
                }
                None => {
                    lane_ends.push(last);
                    lane_ends.len() - 1
                }
            }
        })
        .collect()
}

/// Rounds minutes to the nearest snap step.
pub fn snap(minutes: i64) -> i64 {
    ((minutes as f64 / SNAP_MINUTES as f64).round() as i64 * SNAP_MINUTES).clamp(0, MINUTES_PER_DAY)
}

/// Rounds minutes down to the snap step.
pub fn snap_down(minutes: i64) -> i64 {
    (minutes.div_euclid(SNAP_MINUTES) * SNAP_MINUTES).clamp(0, MINUTES_PER_DAY)
}

/// Local day + minutes to UTC.
pub fn local_to_utc(day: NaiveDate, minutes: i64) -> DateTime<Utc> {
    to_utc(&Local, day.and_time(NaiveTime::MIN) + Duration::minutes(minutes))
}

/// "09:00", "13:30".
pub fn format_minutes(minutes: i64) -> String {
    format!("{:02}:{:02}", minutes / 60 % 24, minutes % 60)
}

/// "09:00 – 10:30" in local time (or "All day").
pub fn time_range_label(occurrence: &Occurrence) -> String {
    if occurrence.event.all_day {
        return "All day".into();
    }
    let (start, end) = local_span(occurrence);
    if start.date() == end.date() || (end.time() == NaiveTime::MIN && end - start <= Duration::days(1)) {
        format!("{} – {}", start.format("%H:%M"), end.format("%H:%M"))
    } else {
        format!("{} – {}", start.format("%a %H:%M"), end.format("%a %-d %b %H:%M"))
    }
}

/// "Today", "Tomorrow", "Yesterday" or "Mon 16 Nov".
pub fn relative_day(day: NaiveDate, today: NaiveDate) -> String {
    match (day - today).num_days() {
        0 => "Today".into(),
        1 => "Tomorrow".into(),
        -1 => "Yesterday".into(),
        _ if day.year() == today.year() => day.format("%a %-d %b").to_string(),
        _ => day.format("%a %-d %b %Y").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn ranges_and_steps() {
        // 2026-11-18 is a Wednesday.
        let anchor = date(2026, 11, 18);
        assert_eq!(start_of_week(anchor), date(2026, 11, 16));
        assert_eq!(visible_days(ViewKind::Day, anchor), [anchor]);
        let work = visible_days(ViewKind::WorkWeek, anchor);
        assert_eq!((work[0], work[4]), (date(2026, 11, 16), date(2026, 11, 20)));
        assert_eq!(visible_days(ViewKind::Week, anchor).len(), 7);
        let month = visible_days(ViewKind::Month, anchor);
        assert_eq!(month.len(), 42);
        assert_eq!(month[0], date(2026, 10, 26));
        assert_eq!(step(ViewKind::Month, date(2026, 1, 31), true), date(2026, 2, 28));
        assert_eq!(step(ViewKind::Week, anchor, false), date(2026, 11, 11));
        assert_eq!(add_months(date(2026, 12, 15), 1), date(2027, 1, 15));
        assert_eq!(add_months(date(2026, 1, 15), -1), date(2025, 12, 15));
    }

    #[test]
    fn titles() {
        let anchor = date(2026, 11, 18);
        assert_eq!(title(ViewKind::Month, anchor), "November 2026");
        assert_eq!(title(ViewKind::Week, anchor), "16 – 22 November 2026");
        assert_eq!(title(ViewKind::Week, date(2026, 12, 30)), "28 Dec 2026 – 3 Jan 2027");
        assert_eq!(title(ViewKind::Week, date(2026, 11, 30)), "30 Nov – 6 Dec 2026");
    }

    #[test]
    fn columns_for_overlaps() {
        // A 9-11, B 9:30-10, C 10-10:30, D 12-13 (separate cluster).
        let items = [(540, 660), (570, 600), (600, 630), (720, 780)];
        let placements = layout_columns(&items, 15);
        assert_eq!(placements[0], Placement { column: 0, columns: 2, span: 1 });
        assert_eq!(placements[1], Placement { column: 1, columns: 2, span: 1 });
        // C reuses column 1 once B ended.
        assert_eq!(placements[2], Placement { column: 1, columns: 2, span: 1 });
        assert_eq!(placements[3], Placement { column: 0, columns: 1, span: 1 });
    }

    #[test]
    fn columns_expand_into_free_space() {
        // A 9-10, B 9-12, C 9-10, D 10:30-11: D reuses column 0 and B blocks its growth.
        let items = [(540, 600), (540, 720), (540, 600), (630, 660)];
        let placements = layout_columns(&items, 15);
        assert_eq!(placements.iter().map(|p| p.column).collect::<Vec<_>>(), [0, 1, 2, 0]);
        assert!(placements.iter().all(|p| p.columns == 3));
        assert_eq!(placements[3].span, 1);
        let items = [(540, 720), (540, 600), (630, 660)];
        let placements = layout_columns(&items, 15);
        assert_eq!(placements[2], Placement { column: 1, columns: 2, span: 1 });
        let items = [(540, 600), (540, 720), (630, 660)];
        let placements = layout_columns(&items, 15);
        // Third starts after the first ended: column 0, and column 1 is busy (B) -> span 1.
        assert_eq!(placements[2], Placement { column: 0, columns: 2, span: 1 });
    }

    #[test]
    fn zero_length_items_still_get_columns() {
        let placements = layout_columns(&[(600, 600), (600, 600)], 15);
        assert_eq!(placements[0].columns, 2);
        assert_ne!(placements[0].column, placements[1].column);
    }

    #[test]
    fn lanes() {
        assert_eq!(pack_lanes(&[(0, 2), (1, 1), (3, 4), (2, 5)]), [0, 1, 0, 1]);
        assert!(pack_lanes(&[]).is_empty());
    }

    #[test]
    fn snapping_and_labels() {
        assert_eq!(snap(7), 0);
        assert_eq!(snap(8), 15);
        assert_eq!(snap(2000), MINUTES_PER_DAY);
        assert_eq!(snap_down(29), 15);
        assert_eq!(format_minutes(570), "09:30");
        let today = date(2026, 11, 14);
        assert_eq!(relative_day(today, today), "Today");
        assert_eq!(relative_day(date(2026, 11, 15), today), "Tomorrow");
        assert_eq!(relative_day(date(2026, 11, 18), today), "Wed 18 Nov");
        assert_eq!(relative_day(date(2027, 1, 4), today), "Mon 4 Jan 2027");
    }
}
