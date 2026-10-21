//! Numbers and times as the 6.x pages printed them.

use chrono::{Local, TimeZone};

/// A stored number, or 0 when it is missing or malformed: one bad field must
/// not fail a whole page.
pub(crate) fn number(raw: Option<&String>) -> f64 {
    raw.and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite())
        .unwrap_or(0.0)
}

/// `%Y-%m-%d %H:%M:%S` in local time, or a dash.
pub(crate) fn timestamp(raw: Option<&String>) -> String {
    raw.and_then(|r| r.trim().parse::<f64>().ok())
        .filter(|ts| ts.is_finite())
        .and_then(|ts| Local.timestamp_opt(ts as i64, 0).single())
        .map_or_else(
            || "—".to_owned(),
            |t| t.format("%Y-%m-%d %H:%M:%S").to_string(),
        )
}

/// `%Y-%m-%d %H:%M` in local time, or a dash: the shorter stamp lists use.
pub(crate) fn minutes(raw: Option<&String>) -> String {
    raw.and_then(|r| r.trim().parse::<f64>().ok())
        .filter(|ts| ts.is_finite())
        .and_then(|ts| Local.timestamp_opt(ts as i64, 0).single())
        .map_or_else(
            || "—".to_owned(),
            |t| t.format("%Y-%m-%d %H:%M").to_string(),
        )
}

/// A table's split date cell: `7 Sep 2026` and `14:05`, or a dash.
pub(crate) fn date_and_time(ts: f64) -> (String, String) {
    match Local.timestamp_opt(ts as i64, 0).single() {
        Some(t) if ts > 0.0 => (
            t.format("%-d %b %Y").to_string(),
            t.format("%H:%M").to_string(),
        ),
        _ => ("—".to_owned(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `number` parses or falls back to 0.0; nothing is computed, so the
    // values compare exactly.
    #[allow(clippy::float_cmp)]
    #[test]
    fn missing_values_print_as_dashes() {
        assert_eq!(number(Some(&" 2.5".to_owned())), 2.5);
        assert_eq!(number(Some(&"nan".to_owned())), 0.0);
        assert_eq!(timestamp(None), "—");
        assert_eq!(timestamp(Some(&"soon".to_owned())), "—");
        assert_eq!(date_and_time(0.0), ("—".to_owned(), String::new()));
        assert_eq!(timestamp(Some(&"0".to_owned())).len(), 19);
    }
}
