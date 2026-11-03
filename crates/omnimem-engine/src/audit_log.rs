//! Reading and pruning the audit log (`AUDIT_LOG`, `AUDIT_RETENTION_DAYS`).
//! Writing it is the store's job, in the transaction of each change.

use chrono::{NaiveDate, NaiveDateTime, TimeZone, Utc};
use omnimem_store::{AuditLevel, AuditQuery, AuditRow};
use serde_json::{Map, Value};
use tracing::info;

use crate::{Engine, Result};

/// How many entries `explain_memory` lists, newest first.
const EXPLAIN_HISTORY: usize = 20;

/// A row as `explain_memory` shows it: the time in ISO 8601, and no empty
/// client, reason or change.
pub(crate) fn history_entry(row: &AuditRow) -> Value {
    let mut m = Map::new();
    m.insert(
        "at".into(),
        chrono::DateTime::from_timestamp(row.at as i64, 0)
            .map_or_else(String::new, |t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
            .into(),
    );
    m.insert("actor".into(), row.actor.clone().into());
    m.insert("client".into(), row.client.clone().into());
    m.insert("action".into(), row.action.clone().into());
    m.insert("from".into(), row.from.clone());
    m.insert("to".into(), row.to.clone());
    m.insert("reason".into(), row.reason.clone().into());
    crate::pyfmt::compact(m)
}

/// A moment typed by a person, as Unix seconds: how long ago (`90m`, `12h`,
/// `7d`, `2w`), a date (`2026-11-03`, midnight UTC) or a date and time
/// (`2026-11-03T14:30`, `2026-11-03 14:30:05`, UTC). `now` is Unix seconds.
pub fn parse_when(raw: &str, now: f64) -> Option<f64> {
    let raw = raw.trim().trim_end_matches('Z');
    if raw.is_empty() {
        return None;
    }
    if let Some((digits, unit)) = raw.split_at_checked(raw.len() - 1)
        && let Ok(n) = digits.parse::<u32>()
    {
        let seconds = match unit {
            "m" => 60.0,
            "h" => 3_600.0,
            "d" => 86_400.0,
            "w" => 604_800.0,
            _ => return None,
        };
        return Some(now - f64::from(n) * seconds);
    }
    if let Ok(date) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        return date
            .and_hms_opt(0, 0, 0)
            .map(|t| Utc.from_utc_datetime(&t).timestamp() as f64);
    }
    [
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M",
    ]
    .iter()
    .find_map(|format| NaiveDateTime::parse_from_str(raw, format).ok())
    .map(|t| Utc.from_utc_datetime(&t).timestamp() as f64)
}

/// A change in a line: `state: active -> archived; tags: [...] -> [...]`.
/// Values longer than `clip` characters are cut, so a `full` row's content
/// doesn't fill a table (the whole row is still in the JSON).
pub fn change_summary(from: &Value, to: &Value, clip: usize) -> String {
    let shown = |value: Option<&Value>| match value {
        None | Some(Value::Null) => "(none)".to_owned(),
        Some(Value::String(text)) if text.is_empty() => "(empty)".to_owned(),
        Some(Value::String(text)) => crate::pyfmt::take_chars(text, clip),
        Some(other) => crate::pyfmt::take_chars(&other.to_string(), clip),
    };
    let Some(to) = to.as_object() else {
        return String::new();
    };
    let from = from.as_object();
    to.iter()
        .map(|(name, after)| match from.and_then(|f| f.get(name)) {
            Some(before) => format!("{name}: {} -> {}", shown(Some(before)), shown(Some(after))),
            None => format!("{name}: {}", shown(Some(after))),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

impl Engine {
    /// The level the log is written at.
    pub fn audit_level(&self) -> AuditLevel {
        self.store.audit_level()
    }

    /// A memory's recorded history, newest first. Empty when the log is off
    /// or the memory has none.
    pub(crate) fn history_of(&self, key: &str) -> Result<Vec<Value>> {
        if self.store.audit_level() == AuditLevel::Off {
            return Ok(Vec::new());
        }
        let rows = self.store.audit_entries(&AuditQuery {
            key: Some(key.to_owned()),
            limit: EXPLAIN_HISTORY,
            ..AuditQuery::default()
        })?;
        Ok(rows.iter().map(history_entry).collect())
    }

    /// Drop audit rows older than `AUDIT_RETENTION_DAYS`. The maintenance
    /// pass calls this, and so does starting up, so a log switched off
    /// still ages out.
    pub fn prune_audit(&self) -> Result<usize> {
        let pruned = self.store.prune_audit(self.config.audit_retention_days)?;
        if pruned > 0 {
            info!(pruned, "pruned old audit rows");
        }
        Ok(pruned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: f64 = 1_800_000_000.0;

    #[test]
    fn when_reads_how_long_ago_dates_and_times() {
        assert_eq!(parse_when("90m", NOW), Some(NOW - 5_400.0));
        assert_eq!(parse_when("12h", NOW), Some(NOW - 43_200.0));
        assert_eq!(parse_when("7d", NOW), Some(NOW - 604_800.0));
        assert_eq!(parse_when(" 2w ", NOW), Some(NOW - 1_209_600.0));
        assert_eq!(parse_when("2026-11-03", NOW), Some(1_793_664_000.0));
        assert_eq!(parse_when("2026-11-03T14:30", NOW), Some(1_793_716_200.0));
        assert_eq!(
            parse_when("2026-11-03 14:30:05", NOW),
            Some(1_793_716_205.0)
        );
        assert_eq!(
            parse_when("2026-11-03T14:30:05Z", NOW),
            Some(1_793_716_205.0)
        );
        for bad in ["", "soon", "7", "d", "7x", "-3d", "2026-13-40", "2026-11"] {
            assert_eq!(parse_when(bad, NOW), None, "{bad:?}");
        }
    }

    #[test]
    fn a_change_reads_as_a_line() {
        let from = json!({"state": "active", "tags": ["a"]});
        let to = json!({"state": "archived", "tags": ["a", "b"], "namespace": "episodic"});
        assert_eq!(
            change_summary(&from, &to, 80),
            r#"state: active -> archived; tags: ["a"] -> ["a","b"]; namespace: episodic"#
        );
        assert_eq!(change_summary(&json!({}), &json!({}), 80), "");
        assert_eq!(
            change_summary(
                &json!({"content": "x".repeat(200)}),
                &json!({"content": ""}),
                10
            ),
            "content: xxxxxxxxxx -> (empty)"
        );
        assert_eq!(
            change_summary(&json!({"a": null}), &json!({"a": "z"}), 10),
            "a: (none) -> z"
        );
    }
}
