//! Small UTC date helpers, so the binary needs no date crate.

use std::time::{SystemTime, UNIX_EPOCH};

pub const DAY: i64 = 86_400;

/// Seconds since the Unix epoch, now.
pub fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The `YYYY-MM-DD` form of an epoch.
#[cfg(test)]
pub fn format_date(epoch: i64) -> String {
    let z = epoch.div_euclid(DAY) + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Parses `YYYY-MM-DD` or an ISO `YYYY-MM-DDTHH:MM:SSZ` timestamp to an epoch.
pub fn parse_date(s: &str) -> Option<i64> {
    let s = s.trim();
    let date = s.get(..10)?;
    let mut parts = date.split('-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    let d: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let mut epoch = days_from_civil(y, m, d) * DAY;
    if let Some(time) = s.get(11..19)
        && s.as_bytes().get(10) == Some(&b'T')
    {
        let mut t = time.split(':');
        let h: i64 = t.next()?.parse().ok()?;
        let mi: i64 = t.next()?.parse().ok()?;
        let se: i64 = t.next()?.parse().ok()?;
        epoch += h * 3600 + mi * 60 + se;
    }
    Some(epoch)
}

/// Epoch seconds from a Firstmate generation stamp such as `s1790401206.46113.533`.
pub fn parse_generation(gen_stamp: &str) -> Option<i64> {
    let digits: String = gen_stamp
        .trim()
        .trim_start_matches(|c: char| c.is_ascii_alphabetic())
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok().filter(|&e: &i64| e > 0)
}

/// Compact elapsed time: `45s`, `12m`, `3h05m`, `2d4h`.
pub fn format_elapsed(secs: i64) -> String {
    let secs = secs.max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < DAY {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    } else {
        let days = secs / DAY;
        let hours = (secs % DAY) / 3600;
        if hours == 0 || days >= 10 {
            format!("{days}d")
        } else {
            format!("{days}d{hours}h")
        }
    }
}

/// Whole-day age for date-only stamps: `today`, `1d`, `12d`.
pub fn format_age_days(then: i64, now: i64) -> String {
    let days = now.div_euclid(DAY) - then.div_euclid(DAY);
    if days <= 0 {
        "today".to_owned()
    } else {
        format!("{days}d")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trip() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        let e = parse_date("2026-09-26").unwrap();
        assert_eq!(format_date(e), "2026-09-26");
        assert_eq!(format_date(parse_date("2024-02-29").unwrap()), "2024-02-29");
    }

    #[test]
    fn parses_timestamps() {
        let d = parse_date("2026-09-26").unwrap();
        assert_eq!(
            parse_date("2026-09-26T05:41:13Z"),
            Some(d + 5 * 3600 + 41 * 60 + 13)
        );
        assert_eq!(parse_date("2026-13-01"), None);
        assert_eq!(parse_date("nope"), None);
        assert_eq!(parse_date(""), None);
    }

    #[test]
    fn generation_stamps() {
        assert_eq!(
            parse_generation("s1790401206.46113.533"),
            Some(1_790_401_206)
        );
        assert_eq!(
            parse_generation("g1788881372.2294.22045"),
            Some(1_788_881_372)
        );
        assert_eq!(parse_generation(""), None);
        assert_eq!(parse_generation("s"), None);
    }

    #[test]
    fn elapsed_formats() {
        assert_eq!(format_elapsed(-5), "0s");
        assert_eq!(format_elapsed(59), "59s");
        assert_eq!(format_elapsed(125), "2m");
        assert_eq!(format_elapsed(3 * 3600 + 5 * 60), "3h05m");
        assert_eq!(format_elapsed(2 * DAY + 4 * 3600), "2d4h");
        assert_eq!(format_elapsed(12 * DAY + 4 * 3600), "12d");
    }

    #[test]
    fn age_in_days() {
        let now = parse_date("2026-09-26T10:00:00Z").unwrap();
        assert_eq!(
            format_age_days(parse_date("2026-09-26").unwrap(), now),
            "today"
        );
        assert_eq!(
            format_age_days(parse_date("2026-09-23").unwrap(), now),
            "3d"
        );
    }
}
