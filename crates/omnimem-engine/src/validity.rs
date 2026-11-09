//! Bi-temporal validity (#26, #27): when a memory was true, and when OmniMem
//! believed it.
//!
//! | Field | Axis | Empty means |
//! |---|---|---|
//! | `valid_from` | valid time, start | true from when it was recorded (`created_at`) |
//! | `valid_to` | valid time, end | still true |
//! | `created_at` | record time, start | (always written) |
//! | `retired_at` | record time, end | still believed |
//!
//! Times are seconds since the epoch, written the way `created_at` is. The
//! store's generated columns and its readers ([`omnimem_store::valid_at`],
//! [`omnimem_store::known_at`]) read a superseded or rejected record with no
//! `retired_at` as retired at its `updated_at`, so nothing older needs a
//! rewrite.
//!
//! Who writes them: `remember` takes `valid_from` and `valid_to`; a
//! resolution closes a superseded memory's validity and retires it, and
//! `both_wrong` retires both sides; a person's reject retires and a restore
//! un-retires ([`follow_trust`] keeps `retired_at` in step with trust, so
//! every trust writer gets it); `reopened` undoes what the resolution it
//! reopens did. An extracted fact takes its source's `valid_to` and
//! `retired_at` ([`Engine::cascade_interval`]).

use omnimem_store::{AuditLevel, Fields, MemoryFilter, Op, op, time_of, trust_of};

use crate::error::invalid;
use crate::pyfmt::{py_float, take_chars};
use crate::trust;
use crate::{Engine, Result};

pub const VALID_FROM: &str = "valid_from";
pub const VALID_TO: &str = "valid_to";
pub const RETIRED_AT: &str = "retired_at";

/// The field on a fact that says its `name` came from its source, so
/// clearing the source's clears only what it gave: `valid_to_from` and
/// `retired_at_from`, each holding the source's key.
pub(crate) fn inherited_marker(name: &str) -> String {
    format!("{name}_from")
}

/// The validity a caller gives a memory, as they wrote it: each is parsed
/// by [`parse_instant`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Validity {
    pub valid_from: Option<String>,
    pub valid_to: Option<String>,
}

/// A time a caller gave: seconds since the epoch, an ISO 8601 date or
/// timestamp (a date alone is local midnight), or the prose forms recall
/// understands in a query ("last March", "3 weeks ago", "1 April 2026").
pub fn parse_instant(raw: &str) -> Option<f64> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(n) = raw.parse::<f64>() {
        return n.is_finite().then_some(n);
    }
    crate::llm::parse_event_date(&serde_json::Value::String(raw.to_owned()))
        .or_else(|| crate::temporal::parse_query_date(raw).and_then(crate::llm::local_timestamp))
        .filter(|t| t.is_finite())
}

/// [`parse_instant`] for a named parameter, refusing what it can't read.
pub(crate) fn instant_arg(name: &str, raw: Option<&str>) -> Result<Option<f64>> {
    match raw.map(str::trim).filter(|r| !r.is_empty()) {
        None => Ok(None),
        Some(raw) => parse_instant(raw).map(Some).ok_or_else(|| {
            invalid(format!(
                "Could not read {name} '{}' as a date. Use an ISO date such as 2026-03-01, \
                 a timestamp, or a phrase such as \"last March\".",
                take_chars(raw, 60)
            ))
        }),
    }
}

/// The fields `remember` writes for a validity a caller gave, checked.
pub(crate) fn validity_fields(valid_from: Option<f64>, valid_to: Option<f64>) -> Result<Fields> {
    if let (Some(from), Some(to)) = (valid_from, valid_to)
        && to <= from
    {
        return Err(invalid("valid_to must be after valid_from".to_owned()));
    }
    let mut fields = Fields::new();
    if let Some(from) = valid_from {
        fields.insert(VALID_FROM.to_owned(), py_float(from));
    }
    if let Some(to) = valid_to {
        fields.insert(VALID_TO.to_owned(), py_float(to));
    }
    Ok(fields)
}

fn retired(trust: &str) -> bool {
    trust == trust::SUPERSEDED || trust == trust::REJECTED
}

/// Keep `retired_at` in step with a trust change already in `updates`: a
/// memory that stops being believed (superseded or rejected) is retired
/// `now`, and one that is believed again loses its `retired_at`. A memory
/// already retired keeps the time it first was.
pub(crate) fn follow_trust(row: &Fields, updates: &mut Fields, now: &str) {
    let Some(next) = updates.get("trust") else {
        return;
    };
    let mut after = row.clone();
    after.insert("trust".to_owned(), next.clone());
    let (was, is) = (retired(trust_of(row)), retired(trust_of(&after)));
    if is && !was {
        updates.insert(RETIRED_AT.to_owned(), now.to_owned());
    } else if !is && time_of(row, RETIRED_AT).is_some() {
        updates.insert(RETIRED_AT.to_owned(), String::new());
    }
}

impl Engine {
    /// An extracted fact or preference takes its source's `valid_to` and
    /// `retired_at` (#27), as it follows its state: "the server is at
    /// <ip>" stops being true when the memory it came from does. A fact
    /// with an end of its own keeps it; one that took its source's loses it
    /// when the source's is cleared. Returns how many facts moved.
    pub(crate) fn cascade_interval(&self, source: &str) -> Result<usize> {
        if source.starts_with("mem:skill:") {
            return Ok(0);
        }
        let Some(src) = self.store.get(source)?.filter(|r| !r.is_empty()) else {
            return Ok(0);
        };
        let give = |name: &str| time_of(&src, name).map(py_float);
        let (valid_to, retired_at) = (give(VALID_TO), give(RETIRED_AT));
        let mut writes: Vec<(String, Fields)> = Vec::new();
        for ns in [
            omnimem_core::Namespace::Knowledge,
            omnimem_core::Namespace::Preference,
        ] {
            let filter = MemoryFilter {
                present_any: vec!["enriched_from".to_owned(), "source_doc_id".to_owned()],
                ..MemoryFilter::default()
            };
            for (fact, row) in self.store.list_memories(
                ns,
                &filter,
                &[
                    "enriched_from",
                    "source_doc_id",
                    VALID_TO,
                    RETIRED_AT,
                    "valid_to_from",
                    "retired_at_from",
                ],
            )? {
                let from_source = |field: &str| row.get(field).is_some_and(|v| v == source);
                if fact == source || !(from_source("enriched_from") || from_source("source_doc_id"))
                {
                    continue;
                }
                let mut updates = Fields::new();
                for (name, value) in [(VALID_TO, &valid_to), (RETIRED_AT, &retired_at)] {
                    let marker = inherited_marker(name);
                    let inherited = row.get(&marker).is_some_and(|v| v == source);
                    let own = row.get(name).filter(|v| !v.is_empty());
                    match value {
                        Some(v) if own.is_none() || inherited => {
                            if own != Some(v) {
                                updates.insert(name.to_owned(), v.clone());
                            }
                            if !inherited {
                                updates.insert(marker, source.to_owned());
                            }
                        }
                        None if inherited => {
                            updates.insert(name.to_owned(), String::new());
                            updates.insert(marker, String::new());
                        }
                        _ => {}
                    }
                }
                if !updates.is_empty() {
                    writes.push((fact, updates));
                }
            }
        }
        let moved = writes.len();
        if moved > 0 {
            let facts: Vec<&str> = writes.iter().map(|(k, _)| k.as_str()).collect();
            let _audit = op(Op::new("cascade")
                .at(AuditLevel::Lifecycle)
                .detail(serde_json::json!({"facts": facts})));
            self.store.set_fields_each(&writes)?;
        }
        Ok(moved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instants_parse_from_numbers_iso_and_prose() {
        assert_eq!(parse_instant("1700000000"), Some(1_700_000_000.0));
        assert_eq!(parse_instant("2026-03-01T00:00:00Z"), Some(1_772_323_200.0));
        assert!(parse_instant("2026-03-01").is_some());
        assert!(parse_instant("3 weeks ago").is_some());
        assert_eq!(parse_instant(""), None);
        assert_eq!(parse_instant("whenever"), None);
        assert_eq!(parse_instant("inf"), None);
        assert!(instant_arg("valid_at", Some("whenever")).is_err());
        assert_eq!(instant_arg("valid_at", Some("  ")).unwrap(), None);
    }

    #[test]
    fn validity_must_end_after_it_starts() {
        assert!(validity_fields(Some(10.0), Some(5.0)).is_err());
        assert!(validity_fields(Some(10.0), Some(10.0)).is_err());
        let f = validity_fields(Some(10.0), None).unwrap();
        assert_eq!(f[VALID_FROM], "10.0");
        assert!(!f.contains_key(VALID_TO));
    }

    #[test]
    fn retirement_follows_trust() {
        let row = Fields::new();
        let mut up = Fields::from([("trust".to_owned(), "superseded".to_owned())]);
        follow_trust(&row, &mut up, "5.0");
        assert_eq!(up[RETIRED_AT], "5.0");

        let row = Fields::from([
            ("trust".to_owned(), "rejected".to_owned()),
            (RETIRED_AT.to_owned(), "3.0".to_owned()),
        ]);
        let mut up = Fields::from([("trust".to_owned(), String::new())]);
        follow_trust(&row, &mut up, "5.0");
        assert_eq!(up[RETIRED_AT], "");

        // Already retired: the first time stands.
        let mut up = Fields::from([("trust".to_owned(), "superseded".to_owned())]);
        follow_trust(&row, &mut up, "5.0");
        assert!(!up.contains_key(RETIRED_AT));

        // No trust change, nothing to follow.
        let mut up = Fields::new();
        follow_trust(&row, &mut up, "5.0");
        assert!(up.is_empty());
    }
}
