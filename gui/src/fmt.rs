//! Human-readable dates, counts, speeds and status labels. Pure functions, unit tested.

use clouddrop::db::{FolderStats, State};
use clouddrop::util::human_bytes;

/// A calendar moment in local time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Civil {
    pub year: i32,
    /// 1-12
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
}

const MONTHS: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September", "October",
    "November", "December",
];
const WEEKDAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];

impl Civil {
    pub fn new(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> Self {
        Civil { year, month, day, hour, minute }
    }

    /// Days since 1970-01-01.
    pub fn days(&self) -> i64 {
        let y = i64::from(self.year) - i64::from(self.month <= 2);
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let m = i64::from(self.month);
        let mp = (m + 9) % 12;
        let doy = (153 * mp + 2) / 5 + i64::from(self.day) - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    /// 0 = Monday.
    pub fn weekday(&self) -> usize {
        (self.days() + 3).rem_euclid(7) as usize
    }

    pub fn clock(&self) -> String {
        format!("{:02}:{:02}", self.hour, self.minute)
    }

    fn month_short(&self) -> &'static str {
        &MONTHS[(self.month as usize - 1) % 12][..3]
    }
}

/// Unix milliseconds in the user's local time zone.
pub fn civil_from_ms(ms: i64) -> Civil {
    gtk::glib::DateTime::from_unix_local(ms.div_euclid(1000))
        .map(|d| {
            Civil::new(
                d.year(),
                d.month() as u32,
                d.day_of_month() as u32,
                d.hour() as u32,
                d.minute() as u32,
            )
        })
        .unwrap_or(Civil::new(1970, 1, 1, 0, 0))
}

pub fn now_civil() -> Civil {
    civil_from_ms(clouddrop::util::now_ms())
}

/// "Today 12:09", "Yesterday", "12 Oct", "12 Oct 2025".
pub fn short_when(then: Civil, now: Civil) -> String {
    match now.days() - then.days() {
        0 => format!("Today {}", then.clock()),
        1 => "Yesterday".to_string(),
        _ if then.year == now.year => format!("{} {}", then.day, then.month_short()),
        _ => format!("{} {} {}", then.day, then.month_short(), then.year),
    }
}

/// Section heading for a day in the activity list.
pub fn day_heading(day: Civil, today: Civil) -> String {
    match today.days() - day.days() {
        0 => "Today".to_string(),
        1 => "Yesterday".to_string(),
        2..=6 => WEEKDAYS[day.weekday()].to_string(),
        _ if day.year == today.year => format!("{} {}", day.day, MONTHS[day.month as usize - 1]),
        _ => format!("{} {} {}", day.day, MONTHS[day.month as usize - 1], day.year),
    }
}

/// Groups consecutive items that fall on the same day, keeping their order.
pub fn group_by_day<T>(
    items: &[T],
    at: impl Fn(&T) -> i64,
    to_civil: impl Fn(i64) -> Civil,
    today: Civil,
) -> Vec<(String, Vec<&T>)> {
    let mut groups: Vec<(String, i64, Vec<&T>)> = Vec::new();
    for item in items {
        let c = to_civil(at(item));
        let key = c.days();
        match groups.last_mut() {
            Some((_, k, v)) if *k == key => v.push(item),
            _ => groups.push((day_heading(c, today), key, vec![item])),
        }
    }
    groups.into_iter().map(|(h, _, v)| (h, v)).collect()
}

/// Upload speed: "850.0 KB/s", "2.4 MB/s".
pub fn speed(bytes_per_second: f64) -> String {
    format!("{}/s", human_bytes(bytes_per_second.round() as u64))
}

/// Time left at the current speed: "less than a minute left", "about 5 minutes left",
/// "about 2 hours left", "more than a day left".
pub fn time_left(seconds: u64) -> String {
    if seconds < 60 {
        return "less than a minute left".into();
    }
    let minutes = (seconds as f64 / 60.0).round() as u64;
    if minutes < 60 {
        return if minutes == 1 { "about a minute left".into() } else { format!("about {minutes} minutes left") };
    }
    let hours = (seconds as f64 / 3600.0).round() as u64;
    if hours < 24 {
        return if hours == 1 { "about an hour left".into() } else { format!("about {hours} hours left") };
    }
    "more than a day left".into()
}

pub fn files(n: i64) -> String {
    if n == 1 { "1 file".into() } else { format!("{n} files") }
}

pub fn items(n: usize) -> String {
    if n == 1 { "1 item".into() } else { format!("{n} items") }
}

/// How a file or folder is doing, for the status column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Uploaded,
    CloudOnly,
    Waiting,
    Uploading,
    Failed,
}

impl Status {
    pub fn of(state: State) -> Status {
        match state {
            State::Synced => Status::Uploaded,
            State::CloudOnly => Status::CloudOnly,
            State::Pending => Status::Waiting,
            State::Uploading => Status::Uploading,
            State::Error => Status::Failed,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Status::Uploaded => "Uploaded",
            Status::CloudOnly => "Cloud-only",
            Status::Waiting => "Waiting",
            Status::Uploading => "Uploading",
            Status::Failed => "Failed",
        }
    }

    pub fn css(self) -> &'static str {
        match self {
            Status::Uploaded => "status-ok",
            Status::CloudOnly => "status-cloud",
            Status::Waiting => "status-wait",
            Status::Uploading => "status-busy",
            Status::Failed => "status-fail",
        }
    }

    /// Order for sorting: problems first.
    pub fn rank(self) -> u8 {
        match self {
            Status::Failed => 0,
            Status::Uploading => 1,
            Status::Waiting => 2,
            Status::CloudOnly => 3,
            Status::Uploaded => 4,
        }
    }
}

/// "12 files · 3 cloud-only", "Empty", "5 files · 1 failed".
pub fn folder_summary(s: &FolderStats) -> String {
    if s.files == 0 {
        return "Empty".into();
    }
    let mut parts = vec![files(s.files)];
    if s.failed > 0 {
        parts.push(format!("{} failed", s.failed));
    }
    if s.waiting > 0 {
        parts.push(format!("{} waiting", s.waiting));
    }
    if s.cloud_only == s.files {
        parts.push("all cloud-only".into());
    } else if s.cloud_only > 0 {
        parts.push(format!("{} cloud-only", s.cloud_only));
    }
    parts.join(" · ")
}

/// The most urgent thing going on inside a folder (None = all uploaded or empty).
pub fn folder_status(s: &FolderStats) -> Option<Status> {
    if s.failed > 0 {
        Some(Status::Failed)
    } else if s.waiting > 0 {
        Some(Status::Waiting)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_and_time_left() {
        assert_eq!(speed(2.4 * 1024.0 * 1024.0), "2.4 MB/s");
        assert_eq!(speed(850.0 * 1024.0), "850.0 KB/s");
        assert_eq!(time_left(20), "less than a minute left");
        assert_eq!(time_left(80), "about a minute left");
        assert_eq!(time_left(5 * 60 + 10), "about 5 minutes left");
        assert_eq!(time_left(3600 + 600), "about an hour left");
        assert_eq!(time_left(5 * 3600), "about 5 hours left");
        assert_eq!(time_left(30 * 3600), "more than a day left");
    }

    /// Test stand-in for the local time zone: UTC.
    pub fn utc(ms: i64) -> Civil {
        let secs = ms.div_euclid(1000);
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        // Inverse of Civil::days (Howard Hinnant's civil_from_days).
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        let y = (yoe + era * 400) as i32 + i32::from(m <= 2);
        Civil::new(y, m, d, (rem / 3600) as u32, (rem % 3600 / 60) as u32)
    }

    #[test]
    fn days_and_weekdays() {
        assert_eq!(Civil::new(1970, 1, 1, 0, 0).days(), 0);
        assert_eq!(Civil::new(1970, 1, 1, 0, 0).weekday(), 3); // Thursday
        assert_eq!(Civil::new(2026, 10, 14, 0, 0).weekday(), 2); // Wednesday
        assert_eq!(Civil::new(2024, 3, 1, 0, 0).days() - Civil::new(2024, 2, 28, 0, 0).days(), 2);
        assert_eq!(utc(1_791_913_161_130), Civil::new(2026, 10, 13, 17, 39));
    }

    #[test]
    fn relative_dates() {
        let now = Civil::new(2026, 10, 14, 12, 30);
        assert_eq!(short_when(Civil::new(2026, 10, 14, 9, 5), now), "Today 09:05");
        assert_eq!(short_when(Civil::new(2026, 10, 13, 23, 59), now), "Yesterday");
        assert_eq!(short_when(Civil::new(2026, 10, 2, 8, 0), now), "2 Oct");
        assert_eq!(short_when(Civil::new(2025, 12, 31, 8, 0), now), "31 Dec 2025");
        // a clock that runs ahead still shows a date, not "Today"
        assert_eq!(short_when(Civil::new(2026, 10, 15, 8, 0), now), "15 Oct");
    }

    #[test]
    fn day_headings() {
        let today = Civil::new(2026, 10, 14, 12, 0); // Wednesday
        assert_eq!(day_heading(Civil::new(2026, 10, 14, 1, 0), today), "Today");
        assert_eq!(day_heading(Civil::new(2026, 10, 13, 1, 0), today), "Yesterday");
        assert_eq!(day_heading(Civil::new(2026, 10, 12, 1, 0), today), "Monday");
        assert_eq!(day_heading(Civil::new(2026, 10, 8, 1, 0), today), "Thursday");
        assert_eq!(day_heading(Civil::new(2026, 10, 7, 1, 0), today), "7 October");
        assert_eq!(day_heading(Civil::new(2025, 3, 1, 1, 0), today), "1 March 2025");
    }

    #[test]
    fn grouping_keeps_order_and_splits_days() {
        let day = 86_400_000_i64;
        let base = 1_791_990_000_000_i64; // 2026-10-14 ~14:20 UTC
        let today = utc(base);
        let at = [base, base - 3_600_000, base - day, base - day - 60_000, base - 9 * day];
        let groups = group_by_day(&at, |t| *t, utc, today);
        let heads: Vec<&str> = groups.iter().map(|(h, _)| h.as_str()).collect();
        assert_eq!(heads, ["Today", "Yesterday", "5 October"]);
        assert_eq!(groups[0].1.len(), 2);
        assert_eq!(groups[1].1.len(), 2);
    }

    #[test]
    fn folder_labels() {
        let s = FolderStats { files: 12, cloud_only: 3, ..Default::default() };
        assert_eq!(folder_summary(&s), "12 files · 3 cloud-only");
        assert_eq!(folder_summary(&FolderStats::default()), "Empty");
        let all = FolderStats { files: 2, cloud_only: 2, ..Default::default() };
        assert_eq!(folder_summary(&all), "2 files · all cloud-only");
        let bad = FolderStats { files: 5, failed: 1, waiting: 2, ..Default::default() };
        assert_eq!(folder_summary(&bad), "5 files · 1 failed · 2 waiting");
        assert_eq!(folder_status(&bad), Some(Status::Failed));
        assert_eq!(folder_status(&s), None);
        assert_eq!(files(1), "1 file");
        assert_eq!(items(0), "0 items");
    }

    #[test]
    fn status_labels() {
        assert_eq!(Status::of(State::Synced).label(), "Uploaded");
        assert_eq!(Status::of(State::CloudOnly).label(), "Cloud-only");
        assert_eq!(Status::of(State::Pending).label(), "Waiting");
        assert_eq!(Status::of(State::Uploading).label(), "Uploading");
        assert_eq!(Status::of(State::Error).label(), "Failed");
        assert!(Status::Failed.rank() < Status::Uploaded.rank());
    }
}
