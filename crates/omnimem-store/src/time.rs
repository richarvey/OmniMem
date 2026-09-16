//! Wall-clock helpers. Times are Unix seconds as `f64`, as 6.x stored them.

use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// `YYYY-MM-DDTHH:MM:SSZ`, the format of a 6.x backup's `exported_at`.
pub(crate) fn iso8601_utc(secs: f64) -> String {
    let secs = secs.max(0.0) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Howard Hinnant's days-to-civil algorithm (proleptic Gregorian, UTC).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_known_instants() {
        assert_eq!(iso8601_utc(0.0), "1970-01-01T00:00:00Z");
        // The production backup used while porting.
        assert_eq!(iso8601_utc(1_788_802_536.0), "2026-09-07T17:35:36Z");
        assert_eq!(iso8601_utc(951_782_400.0), "2000-02-29T00:00:00Z");
    }
}
