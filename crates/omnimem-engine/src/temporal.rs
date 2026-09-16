//! Dates in recall queries and the event_date boost (`memory/temporal.py`).
//!
//! 6.x handed queries to Python's `dateparser`. No Rust crate reads prose
//! the same way, so this parses the forms people put in memory queries:
//! relative days, "N units ago", "last/this/next week|month|year", weekdays,
//! ISO and US numeric dates, month names with or without day and year, and
//! bare years. Like dateparser with `PREFER_DATES_FROM=past`, an ambiguous
//! date resolves to the most recent past one, and missing day or time parts
//! are taken from now. Times are naive local, as 6.x compared them.

use std::sync::LazyLock;

use chrono::{
    DateTime, Datelike, Days, Local, Months, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Weekday,
};
use regex::Regex;

const FULL_MATCH_WINDOW_DAYS: f64 = 7.0;
const FALLOFF_WINDOW_DAYS: f64 = 60.0;
const MAX_BOOST: f64 = 1.5;

static HINT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)\b(",
        r"yesterday|today|tomorrow|tonight|",
        r"last|this|next|past|previous|recent|recently|",
        r"ago|earlier|later|before|after|since|until|",
        r"monday|tuesday|wednesday|thursday|friday|saturday|sunday|",
        r"january|february|march|april|may|june|july|",
        r"august|september|october|november|december|",
        r"jan|feb|mar|apr|jun|jul|aug|sep|oct|nov|dec|",
        r"\d{4}|\d{1,2}/\d{1,2}|\d{1,2}-\d{1,2}",
        r")\b"
    ))
    .expect("valid regex")
});

const MONTHS: &str = "january|february|march|april|may|june|july|august|september|october|november|december|jan|feb|mar|apr|jun|jul|aug|sept|sep|oct|nov|dec";
const WEEKDAYS: &str = "monday|tuesday|wednesday|thursday|friday|saturday|sunday";

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid regex")
}

static ISO_RE: LazyLock<Regex> = LazyLock::new(|| re(r"\b(\d{4})-(\d{1,2})-(\d{1,2})\b"));
static US_FULL_RE: LazyLock<Regex> = LazyLock::new(|| re(r"\b(\d{1,2})/(\d{1,2})/(\d{4})\b"));
static US_SHORT_RE: LazyLock<Regex> = LazyLock::new(|| re(r"\b(\d{1,2})/(\d{1,2})\b"));
static RELATIVE_DAY_RE: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)\b(yesterday|today|tonight|tomorrow)\b"));
static AGO_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)\b(\d+|an?|one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve)\s+(day|week|month|year)s?\s+ago\b",
    )
});
static RELATIVE_UNIT_RE: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)\b(last|past|previous|this|next)\s+(week|month|year)\b"));
static WEEKDAY_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"(?i)\b(?:(last|past|previous|this|next)\s+)?({WEEKDAYS})\b"
    ))
});
static DAY_MONTH_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"(?i)\b(\d{{1,2}})(?:st|nd|rd|th)?\s+({MONTHS})\.?,?(?:\s+(\d{{4}}))?\b"
    ))
});
static MONTH_DAY_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"(?i)\b({MONTHS})\.?\s+(\d{{1,2}})(?:st|nd|rd|th)?\b(?:,?\s+(\d{{4}})\b)?"
    ))
});
static MONTH_YEAR_RE: LazyLock<Regex> =
    LazyLock::new(|| re(&format!(r"(?i)\b({MONTHS})\.?,?\s+(\d{{4}})\b")));
static MONTH_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)\b(january|february|march|april|may|june|july|august|september|october|november|december)\b",
    )
});
static YEAR_RE: LazyLock<Regex> = LazyLock::new(|| re(r"\b(19\d{2}|20\d{2})\b"));

pub fn looks_temporal(query: &str) -> bool {
    !query.is_empty() && HINT_RE.is_match(query)
}

fn month_number(name: &str) -> Option<u32> {
    let n = name.to_lowercase();
    let n = n.trim_end_matches('.');
    Some(match &n[..n.len().min(3)] {
        "jan" => 1,
        "feb" => 2,
        "mar" => 3,
        "apr" => 4,
        "may" => 5,
        "jun" => 6,
        "jul" => 7,
        "aug" => 8,
        "sep" => 9,
        "oct" => 10,
        "nov" => 11,
        "dec" => 12,
        _ => return None,
    })
}

fn number_word(word: &str) -> Option<u32> {
    let w = word.to_lowercase();
    if let Ok(n) = w.parse() {
        return Some(n);
    }
    Some(match w.as_str() {
        "a" | "an" | "one" => 1,
        "two" => 2,
        "three" => 3,
        "four" => 4,
        "five" => 5,
        "six" => 6,
        "seven" => 7,
        "eight" => 8,
        "nine" => 9,
        "ten" => 10,
        "eleven" => 11,
        "twelve" => 12,
        _ => return None,
    })
}

fn weekday(name: &str) -> Option<Weekday> {
    name.to_lowercase().parse().ok()
}

fn midnight(date: NaiveDate) -> NaiveDateTime {
    date.and_time(NaiveTime::MIN)
}

/// A date with the day clamped into the month.
fn date_clamped(year: i32, month: u32, day: u32) -> Option<NaiveDate> {
    (1..=day)
        .rev()
        .find_map(|d| NaiveDate::from_ymd_opt(year, month, d))
}

/// Largest count a relative expression may carry ("1000 years ago"); beyond
/// it the phrase is not a date, and the arithmetic below stays in range.
const MAX_SHIFT_MAGNITUDE: u64 = 1000;

fn shift(now: NaiveDateTime, unit: &str, n: i64) -> Option<NaiveDateTime> {
    let unit = unit.to_lowercase();
    let magnitude = n.unsigned_abs();
    if magnitude > MAX_SHIFT_MAGNITUDE {
        return None;
    }
    let months = u32::try_from(magnitude).ok()?;
    let years_in_months = months.checked_mul(12)?;
    match (unit.as_str(), n >= 0) {
        ("day", true) => now.checked_add_days(Days::new(magnitude)),
        ("day", false) => now.checked_sub_days(Days::new(magnitude)),
        ("week", true) => now.checked_add_days(Days::new(magnitude.checked_mul(7)?)),
        ("week", false) => now.checked_sub_days(Days::new(magnitude.checked_mul(7)?)),
        ("month", true) => now.checked_add_months(Months::new(months)),
        ("month", false) => now.checked_sub_months(Months::new(months)),
        ("year", true) => now.checked_add_months(Months::new(years_in_months)),
        ("year", false) => now.checked_sub_months(Months::new(years_in_months)),
        _ => None,
    }
}

/// Every date-shaped span in the query, as (start, end, date); the earliest
/// (and at a tie, the longest) wins.
fn candidates(query: &str, now: NaiveDateTime) -> Vec<(usize, usize, NaiveDateTime)> {
    let today = now.date();
    let mut found = Vec::new();
    let mut push = |m: regex::Match<'_>, dt: Option<NaiveDateTime>| {
        if let Some(dt) = dt {
            found.push((m.start(), m.end(), dt));
        }
    };

    for c in ISO_RE.captures_iter(query) {
        let date = NaiveDate::from_ymd_opt(
            c[1].parse().unwrap_or(0),
            c[2].parse().unwrap_or(0),
            c[3].parse().unwrap_or(0),
        );
        push(c.get(0).unwrap(), date.map(midnight));
    }
    for c in US_FULL_RE.captures_iter(query) {
        let date = NaiveDate::from_ymd_opt(
            c[3].parse().unwrap_or(0),
            c[1].parse().unwrap_or(0),
            c[2].parse().unwrap_or(0),
        );
        push(c.get(0).unwrap(), date.map(midnight));
    }
    for c in US_SHORT_RE.captures_iter(query) {
        let (month, day) = (c[1].parse().unwrap_or(0), c[2].parse().unwrap_or(0));
        let date = NaiveDate::from_ymd_opt(today.year(), month, day).and_then(|d| {
            if d > today {
                NaiveDate::from_ymd_opt(today.year() - 1, month, day)
            } else {
                Some(d)
            }
        });
        push(c.get(0).unwrap(), date.map(midnight));
    }
    for c in RELATIVE_DAY_RE.captures_iter(query) {
        let dt = match c[1].to_lowercase().as_str() {
            "yesterday" => shift(now, "day", -1),
            "tomorrow" => shift(now, "day", 1),
            _ => Some(now),
        };
        push(c.get(0).unwrap(), dt);
    }
    for c in AGO_RE.captures_iter(query) {
        let dt = number_word(&c[1]).and_then(|n| shift(now, &c[2], -i64::from(n)));
        push(c.get(0).unwrap(), dt);
    }
    for c in RELATIVE_UNIT_RE.captures_iter(query) {
        let dt = match c[1].to_lowercase().as_str() {
            "this" => Some(now),
            "next" => shift(now, &c[2], 1),
            _ => shift(now, &c[2], -1),
        };
        push(c.get(0).unwrap(), dt);
    }
    for c in WEEKDAY_RE.captures_iter(query) {
        let Some(target) = weekday(&c[2]) else {
            continue;
        };
        let back = (7 + today.weekday().num_days_from_monday() - target.num_days_from_monday()) % 7;
        let days_back = match c.get(1).map(|m| m.as_str().to_lowercase()) {
            Some(q) if q == "next" => {
                let ahead = (7 - back) % 7;
                -(if ahead == 0 { 7 } else { ahead } as i64)
            }
            Some(q) if q != "this" => {
                if back == 0 {
                    7
                } else {
                    back as i64
                }
            }
            _ => back as i64,
        };
        push(c.get(0).unwrap(), shift(now, "day", -days_back));
    }
    for c in DAY_MONTH_RE.captures_iter(query) {
        let month = month_number(&c[2]);
        let day: u32 = c[1].parse().unwrap_or(0);
        let date = month.and_then(|m| match c.get(3) {
            Some(y) => NaiveDate::from_ymd_opt(y.as_str().parse().unwrap_or(0), m, day),
            None => past_date(today, m, day),
        });
        push(c.get(0).unwrap(), date.map(midnight));
    }
    for c in MONTH_DAY_RE.captures_iter(query) {
        let month = month_number(&c[1]);
        let day: u32 = c[2].parse().unwrap_or(0);
        let date = month.and_then(|m| match c.get(3) {
            Some(y) => NaiveDate::from_ymd_opt(y.as_str().parse().unwrap_or(0), m, day),
            None => past_date(today, m, day),
        });
        push(c.get(0).unwrap(), date.map(midnight));
    }
    for c in MONTH_YEAR_RE.captures_iter(query) {
        let date = month_number(&c[1])
            .and_then(|m| date_clamped(c[2].parse().unwrap_or(0), m, today.day()));
        push(c.get(0).unwrap(), date.map(midnight));
    }
    for c in MONTH_RE.captures_iter(query) {
        let date = month_number(&c[1]).and_then(|m| {
            let year = if m > today.month() {
                today.year() - 1
            } else {
                today.year()
            };
            date_clamped(year, m, today.day())
        });
        push(c.get(0).unwrap(), date.map(midnight));
    }
    for c in YEAR_RE.captures_iter(query) {
        let date = date_clamped(c[1].parse().unwrap_or(0), today.month(), today.day());
        push(c.get(0).unwrap(), date.map(midnight));
    }
    found
}

/// This year's month/day, or last year's if that is still to come.
fn past_date(today: NaiveDate, month: u32, day: u32) -> Option<NaiveDate> {
    let this_year = NaiveDate::from_ymd_opt(today.year(), month, day)?;
    if this_year > today {
        NaiveDate::from_ymd_opt(today.year() - 1, month, day)
    } else {
        Some(this_year)
    }
}

/// The first date the query mentions, or `None`.
pub fn parse_query_date_at(query: &str, now: NaiveDateTime) -> Option<NaiveDateTime> {
    if !looks_temporal(query) {
        return None;
    }
    candidates(query, now)
        .into_iter()
        .min_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)))
        .map(|(_, _, dt)| dt)
}

pub fn parse_query_date(query: &str) -> Option<NaiveDateTime> {
    parse_query_date_at(query, Local::now().naive_local())
}

/// 1.5 within a week of the query date, falling linearly to 1.0 at 60 days.
pub fn temporal_boost(query_date: NaiveDateTime, event_date_ts: f64) -> f64 {
    let secs = event_date_ts.floor() as i64;
    let nanos = ((event_date_ts - event_date_ts.floor()) * 1e9) as u32;
    let Some(event) = Local
        .timestamp_opt(secs, nanos)
        .single()
        .map(|d: DateTime<Local>| d.naive_local())
    else {
        return 1.0;
    };
    let delta_days = (event - query_date).num_milliseconds().abs() as f64 / 86_400_000.0;
    if delta_days <= FULL_MATCH_WINDOW_DAYS {
        return MAX_BOOST;
    }
    if delta_days >= FALLOFF_WINDOW_DAYS {
        return 1.0;
    }
    let progress =
        (delta_days - FULL_MATCH_WINDOW_DAYS) / (FALLOFF_WINDOW_DAYS - FULL_MATCH_WINDOW_DAYS);
    MAX_BOOST - (MAX_BOOST - 1.0) * progress
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tuesday 15 September 2026, 10:30.
    fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, 15)
            .unwrap()
            .and_hms_opt(10, 30, 0)
            .unwrap()
    }

    fn date(q: &str) -> Option<NaiveDate> {
        parse_query_date_at(q, now()).map(|d| d.date())
    }

    fn ymd(y: i32, m: u32, d: u32) -> Option<NaiveDate> {
        NaiveDate::from_ymd_opt(y, m, d)
    }

    #[test]
    fn no_hint_no_date() {
        assert_eq!(date("valkey tag filter quirks"), None);
        assert!(!looks_temporal(""));
    }

    #[test]
    fn relative_forms() {
        assert_eq!(date("what did I do yesterday"), ymd(2026, 9, 14));
        assert_eq!(date("notes from 3 days ago"), ymd(2026, 9, 12));
        assert_eq!(date("a week ago"), ymd(2026, 9, 8));
        assert_eq!(date("two months ago"), ymd(2026, 7, 15));
        assert_eq!(date("decisions last week"), ymd(2026, 9, 8));
        assert_eq!(date("last year"), ymd(2025, 9, 15));
    }

    #[test]
    fn weekdays_prefer_the_past() {
        assert_eq!(date("last tuesday"), ymd(2026, 9, 8));
        assert_eq!(date("on tuesday"), ymd(2026, 9, 15));
        assert_eq!(date("friday's deploy"), ymd(2026, 9, 11));
        assert_eq!(date("next monday"), ymd(2026, 9, 21));
    }

    #[test]
    fn absolute_forms() {
        assert_eq!(date("since 2026-03-04"), ymd(2026, 3, 4));
        assert_eq!(
            date("on 10/15"),
            ymd(2025, 10, 15),
            "a future month/day means last year"
        );
        assert_eq!(date("on 3/4/2025"), ymd(2025, 3, 4));
        assert_eq!(date("March 3rd, 2024"), ymd(2024, 3, 3));
        assert_eq!(date("3 March 2024"), ymd(2024, 3, 3));
        assert_eq!(date("in March 2026"), ymd(2026, 3, 15));
        assert_eq!(date("in December"), ymd(2025, 12, 15));
        assert_eq!(date("what happened in 2024"), ymd(2024, 9, 15));
    }

    #[test]
    fn the_earliest_span_wins() {
        assert_eq!(date("yesterday not 2020"), ymd(2026, 9, 14));
    }

    #[test]
    fn boost_window() {
        let q = now();
        let ts =
            |dt: NaiveDateTime| Local.from_local_datetime(&dt).single().unwrap().timestamp() as f64;
        assert_eq!(temporal_boost(q, ts(q)), 1.5);
        assert_eq!(temporal_boost(q, ts(q - chrono::Duration::days(90))), 1.0);
        let mid = temporal_boost(
            q,
            ts(q - chrono::Duration::days(33) - chrono::Duration::hours(12)),
        );
        assert!((mid - 1.25).abs() < 1e-9, "{mid}");
    }
}
