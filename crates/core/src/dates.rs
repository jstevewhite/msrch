//! Date-argument parsing for `--after`/`--before`.
//!
//! Accepted forms: ISO `YYYY-MM-DD` (resolves to that day's 00:00 local time)
//! and relative `Nd`/`Nw`/`Nm` (days/weeks/months ago; month ≈ 30 days).
//! Inclusive/exclusive semantics are applied by core at comparison time —
//! both flags parse identically here.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

const FORMS: &str = "accepted forms: YYYY-MM-DD, or relative 7d / 2w / 3m (days/weeks/months ago)";

/// Date-argument parser shared by the CLI (clap value_parser) and MCP front-ends.
pub fn parse_date_arg(s: &str) -> Result<SystemTime, String> {
    resolve_with_now(s, SystemTime::now())
}

fn resolve_with_now(s: &str, now: SystemTime) -> Result<SystemTime, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err(format!("empty date; {FORMS}"));
    }

    // Relative: digits followed by a single unit char.
    if let Some(unit) = s.chars().last().filter(|c| matches!(c, 'd' | 'w' | 'm')) {
        let digits = &s[..s.len() - 1];
        if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
            let n: u64 = digits
                .parse()
                .map_err(|_| format!("'{s}' is out of range; {FORMS}"))?;
            let days = match unit {
                'd' => n,
                'w' => n * 7,
                'm' => n * 30, // documented approximation
                _ => unreachable!(),
            };
            let delta = Duration::from_secs(days.saturating_mul(86_400));
            // Check if subtracting delta would go before UNIX_EPOCH.
            let elapsed_from_epoch = now
                .duration_since(UNIX_EPOCH)
                .map_err(|_| format!("'{s}' is too far in the past"))?;
            if delta > elapsed_from_epoch {
                return Err(format!("'{s}' is too far in the past"));
            }
            return now
                .checked_sub(delta)
                .ok_or_else(|| format!("'{s}' is too far in the past"));
        }
    }

    // ISO date → local midnight.
    if let Ok(date) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        use chrono::TimeZone;
        let midnight = date
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| format!("'{s}' has no midnight (?); {FORMS}"))?;
        return chrono::Local
            .from_local_datetime(&midnight)
            .single()
            .map(SystemTime::from)
            .ok_or_else(|| format!("'{s}' is ambiguous in local time (DST edge); {FORMS}"));
    }

    Err(format!("could not parse date '{s}'; {FORMS}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn t(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn relative_forms_subtract_from_now() {
        let now = t(100 * 86_400); // fixed "now": 100 days after epoch
        assert_eq!(resolve_with_now("7d", now).unwrap(), t(93 * 86_400));
        assert_eq!(resolve_with_now("2w", now).unwrap(), t(86 * 86_400));
        assert_eq!(resolve_with_now("3m", now).unwrap(), t(10 * 86_400)); // 3 × 30d
        assert_eq!(resolve_with_now("1d", now).unwrap(), t(99 * 86_400));
    }

    #[test]
    fn iso_dates_resolve_to_local_midnight() {
        use chrono::{Local, TimeZone};
        let got = resolve_with_now("2026-07-01", t(0)).unwrap();
        let expected: SystemTime = Local
            .with_ymd_and_hms(2026, 7, 1, 0, 0, 0)
            .single()
            .expect("unambiguous local midnight")
            .into();
        assert_eq!(got, expected);
    }

    #[test]
    fn garbage_is_rejected_with_helpful_message() {
        for bad in ["tomorrow", "2026-13-40", "", "7", "d7", "7y", "07/01/2026"] {
            let err = parse_date_arg(bad).unwrap_err();
            assert!(
                err.contains("YYYY-MM-DD") && err.contains("7d"),
                "error must list accepted forms, got: {err}"
            );
        }
    }

    #[test]
    fn relative_larger_than_now_is_rejected_not_panicking() {
        // now − N would underflow SystemTime; must error, not panic.
        let err = resolve_with_now("999999d", t(86_400)).unwrap_err();
        assert!(err.contains("too far in the past"), "{err}");
    }
}
