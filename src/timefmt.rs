//! Time formatting for the activity feed and the claim rows.
//!
//! Everything renders from unix seconds. The absolute form stays in UTC —
//! `hhmmss_utc` is lifted from magic-carpet-desktop/src/ui.rs — and the
//! relative form is a plain duration, so no timezone table is carried.

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `HH:MM:SS` in UTC.
pub fn hhmmss_utc(unix_seconds: u64) -> String {
    let within_day = unix_seconds % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        within_day / 3_600,
        (within_day % 3_600) / 60,
        within_day % 60
    )
}

/// A coarse "how long ago", for the meta line beside an absolute time.
pub fn relative(now: u64, then: u64) -> String {
    if then == 0 {
        return "—".into();
    }
    let delta = now.saturating_sub(then);
    match delta {
        0..=4 => "just now".into(),
        5..=59 => format!("{delta} s ago"),
        60..=3_599 => format!("{} min ago", delta / 60),
        3_600..=86_399 => format!("{} h ago", delta / 3_600),
        _ => format!("{} d ago", delta / 86_400),
    }
}

/// `"Aug 11, 2026"` — the calendar date beside the relative form, in UTC.
/// Civil-from-days (Howard Hinnant's algorithm), so no date crate is needed.
pub fn month_day_year(unix_seconds: u64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = (unix_seconds / 86_400) as i64;
    // Days since 1970-01-01 → (year, day-of-year), civil calendar.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{} {}, {}", MONTHS[(month - 1) as usize], day, year)
}

/// `12345` → `"12,345"`. Every sats figure on screen goes through this.
pub fn fmt_sats(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (ix, c) in digits.chars().enumerate() {
        if ix > 0 && (digits.len() - ix).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_format_like_the_desktop_app() {
        // 2026-08-01 16:13:00 UTC, the desktop module's own vector.
        assert_eq!(hhmmss_utc(1_785_600_780), "16:13:00");
        assert_eq!(hhmmss_utc(0), "00:00:00");
    }

    #[test]
    fn relative_times_coarsen_with_distance() {
        assert_eq!(relative(100, 0), "—");
        assert_eq!(relative(100, 98), "just now");
        assert_eq!(relative(100, 79), "21 s ago");
        assert_eq!(relative(1_000, 100), "15 min ago");
        assert_eq!(relative(90_000, 100), "1 d ago");
    }

    #[test]
    fn sats_group_by_thousands() {
        assert_eq!(fmt_sats(0), "0");
        assert_eq!(fmt_sats(100), "100");
        assert_eq!(fmt_sats(5_000), "5,000");
        assert_eq!(fmt_sats(1_234_567), "1,234,567");
    }

    #[test]
    fn calendar_dates_from_unix_days() {
        assert_eq!(month_day_year(1_786_406_400), "Aug 11, 2026");
        assert_eq!(month_day_year(0), "Jan 1, 1970");
        // Leap-day and a year rollover, to pin the civil-from-days math.
        assert_eq!(month_day_year(1_583_001_600), "Feb 29, 2020");
        assert_eq!(month_day_year(1_609_459_200), "Jan 1, 2021");
    }
}
