//! Lifecycle states and transitions, topic suppression and reinstate hints
//! (`memory/lifecycle.py`).

use omnimem_store::Fields;
use serde_json::{Map, Value, json};
use tracing::info;

use crate::error::invalid;
use crate::pyfmt::{now_str, py_float, py_json};
use crate::recall::mentions;
use crate::{Engine, Result};

pub(crate) const SUPPRESSED_KEY: &str = "topics:suppressed";
/// Shortest topic, abandoned-approach name or reinstate hint that may match
/// against text: anything shorter appears inside most words.
pub(crate) const MIN_TOPIC_CHARS: usize = 3;
pub(crate) const MAX_TOPIC_CHARS: usize = 200;

/// Trim a topic-like term (a suppressed topic, an abandoned approach name or
/// a reinstate hint) and check it is something that can sensibly be matched.
pub(crate) fn validate_term(what: &str, value: &str) -> Result<String> {
    let trimmed = value.trim();
    let length = trimmed.chars().count();
    if !(MIN_TOPIC_CHARS..=MAX_TOPIC_CHARS).contains(&length) {
        return Err(invalid(format!(
            "{what} must be {MIN_TOPIC_CHARS}-{MAX_TOPIC_CHARS} characters"
        )));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(invalid(format!("{what} contains control characters")));
    }
    Ok(trimmed.to_owned())
}

/// Reinstate hints, trimmed. A blank or one-letter hint would flag the
/// memory as a candidate for every query.
pub(crate) fn validate_reinstate_hints(hints: &[String]) -> Result<Vec<String>> {
    hints
        .iter()
        .map(|h| validate_term("Reinstate hint", h))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryState {
    Active,
    Deprioritised,
    Archived,
    Deleted,
}

impl MemoryState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Deprioritised => "deprioritised",
            Self::Archived => "archived",
            Self::Deleted => "deleted",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "active" => Self::Active,
            "deprioritised" => Self::Deprioritised,
            "archived" => Self::Archived,
            "deleted" => Self::Deleted,
            _ => return None,
        })
    }

    pub(crate) fn can_become(self, next: Self) -> bool {
        self.allowed().contains(&next)
    }

    fn allowed(self) -> &'static [Self] {
        use MemoryState::{Active, Archived, Deleted, Deprioritised};
        match self {
            Active => &[Deprioritised, Archived, Deleted],
            Deprioritised => &[Active, Archived, Deleted],
            Archived => &[Active, Deleted],
            Deleted => &[],
        }
    }
}

impl Engine {
    pub(crate) fn surface_score(&self, state: MemoryState) -> f64 {
        match state {
            MemoryState::Active => 1.0,
            MemoryState::Deprioritised => self.config.deprioritised_weight,
            MemoryState::Archived | MemoryState::Deleted => 0.0,
        }
    }

    /// Validate and apply a state transition.
    pub fn transition(
        &self,
        key: &str,
        new_state: MemoryState,
        reason: Option<&str>,
    ) -> Result<Map<String, Value>> {
        let Some(data) = self.store.get(key)? else {
            return Err(invalid(format!("Memory key not found: {key}")));
        };
        let raw_state = data.get("state").map_or("active", String::as_str);
        let current = MemoryState::parse(raw_state)
            .ok_or_else(|| invalid(format!("'{raw_state}' is not a valid MemoryState")))?;
        if !current.allowed().contains(&new_state) {
            let allowed: Vec<String> = current
                .allowed()
                .iter()
                .map(|s| format!("'{}'", s.as_str()))
                .collect();
            return Err(invalid(format!(
                "Invalid transition: {} -> {}. Allowed: [{}]",
                current.as_str(),
                new_state.as_str(),
                allowed.join(", ")
            )));
        }

        let surface = self.surface_score(new_state);
        if new_state != MemoryState::Deleted {
            let mut updates = Fields::from([
                ("state".to_owned(), new_state.as_str().to_owned()),
                ("surface_score".to_owned(), py_float(surface)),
                ("updated_at".to_owned(), now_str()),
            ]);
            if new_state == MemoryState::Deprioritised
                && let Some(reason) = reason.filter(|r| !r.is_empty())
            {
                updates.insert("deprioritised_reason".to_owned(), reason.to_owned());
            }
            self.store.set_fields(key, &updates)?;
        }

        let mut result = Map::new();
        result.insert("key".into(), key.into());
        result.insert("previous_state".into(), current.as_str().into());
        result.insert("new_state".into(), new_state.as_str().into());
        result.insert("surface_score".into(), json!(surface));

        if new_state == MemoryState::Deprioritised
            && let Some(effort) = data
                .get("effort_score")
                .and_then(|e| e.trim().parse::<f64>().ok())
                .map(|e| e as i64)
            && effort >= 4
        {
            result.insert(
                "warning".into(),
                format!(
                    "This memory has an effort score of {effort}/5. It represents hard-won \
                     knowledge. Deprioritised as requested, but consider archiving rather than \
                     suppressing it entirely."
                )
                .into(),
            );
        }

        if new_state == MemoryState::Deleted {
            self.store.delete(key)?;
        }
        let derived = self.cascade_to_derived(key, new_state)?;
        if derived > 0 {
            result.insert("derived_facts".into(), derived.into());
        }
        info!(
            key,
            from = current.as_str(),
            to = new_state.as_str(),
            derived,
            "transitioned memory"
        );
        Ok(result)
    }

    /// Facts and preferences extracted from `key` follow it: deprioritised,
    /// archived or deleted with it, and back to active when it's reinstated.
    /// A fact the source took with it is marked `retired_with`, so reinstating
    /// the source brings back only those, never one turned down on its own.
    /// Facts outlive nothing they were drawn from, which is how "the server
    /// is at <ip>" stopped being recalled as true after its source went.
    fn cascade_to_derived(&self, key: &str, new_state: MemoryState) -> Result<usize> {
        if key.starts_with("mem:skill:") {
            return Ok(0);
        }
        let mut derived = Vec::new();
        for ns in ["knowledge", "preference"] {
            let keys = self.store.scan_prefix(&format!("mem:{ns}:"))?;
            let rows = self.store.get_fields_multi(
                &keys,
                &[
                    "enriched_from",
                    "source_doc_id",
                    "state",
                    "surface_score",
                    "retired_with",
                    "retired_surface",
                    "retired_state",
                ],
            )?;
            for (fact, row) in keys.into_iter().zip(rows) {
                let Some(row) = row else { continue };
                let from_source = |field: &str| row.get(field).is_some_and(|v| v == key);
                if fact != key && (from_source("enriched_from") || from_source("source_doc_id")) {
                    derived.push((fact, row));
                }
            }
        }
        let mut changed = 0;
        let now = now_str();
        for (fact, row) in derived {
            let state = row.get("state").map_or("active", String::as_str);
            let retired_by_source = row.get("retired_with").is_some_and(|v| v == key);
            let follow = match new_state {
                MemoryState::Deprioritised => state == "active",
                MemoryState::Archived => matches!(state, "active" | "deprioritised"),
                MemoryState::Deleted => true,
                MemoryState::Active => retired_by_source,
            };
            if !follow {
                continue;
            }
            if new_state == MemoryState::Deleted {
                self.store.delete(&fact)?;
                changed += 1;
                continue;
            }
            let mut updates = Fields::from([
                ("state".to_owned(), new_state.as_str().to_owned()),
                ("updated_at".to_owned(), now.clone()),
            ]);
            if new_state == MemoryState::Active {
                // Back to where it was before the source took it.
                let saved = |name: &str, default: &str| {
                    row.get(name)
                        .filter(|v| !v.is_empty())
                        .cloned()
                        .unwrap_or_else(|| default.to_owned())
                };
                updates.insert("state".to_owned(), saved("retired_state", "active"));
                updates.insert("surface_score".to_owned(), saved("retired_surface", "0.5"));
                for name in ["retired_with", "retired_surface", "retired_state"] {
                    updates.insert(name.to_owned(), String::new());
                }
            } else {
                updates.insert(
                    "surface_score".to_owned(),
                    py_float(self.surface_score(new_state)),
                );
                updates.insert("retired_with".to_owned(), key.to_owned());
                if !retired_by_source {
                    let surface = row
                        .get("surface_score")
                        .cloned()
                        .unwrap_or_else(|| "0.5".to_owned());
                    updates.insert("retired_surface".to_owned(), surface);
                    updates.insert("retired_state".to_owned(), state.to_owned());
                }
            }
            self.store.set_fields(&fact, &updates)?;
            changed += 1;
        }
        Ok(changed)
    }

    pub(crate) fn suppressed_topics(&self) -> Result<Vec<String>> {
        Ok(self.store.set_members(SUPPRESSED_KEY)?)
    }

    pub(crate) fn add_reinstate_hints(&self, key: &str, hints: &[String]) -> Result<()> {
        let Some(data) = self.store.get(key)? else {
            return Err(invalid(format!("Memory key not found: {key}")));
        };
        let mut existing: Vec<Value> = data
            .get("reinstate_hints")
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default();
        existing.extend(hints.iter().map(|h| Value::from(h.as_str())));
        self.store
            .set_field(key, "reinstate_hints", &py_json(&Value::Array(existing)))?;
        Ok(())
    }
}

/// True if a deprioritised memory has a reinstate hint the query mentions,
/// or that mentions the query, matched on word boundaries. A blank query
/// matches nothing, and hints too short to be meaningful (from data stored
/// before hints were validated) are ignored.
pub(crate) fn check_reinstate_eligibility(doc: &Fields, query: &str) -> bool {
    if doc.get("state").map_or("active", String::as_str) != "deprioritised" {
        return false;
    }
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return false;
    }
    let Some(hints) = doc
        .get("reinstate_hints")
        .and_then(|raw| serde_json::from_str::<Vec<Value>>(raw).ok())
    else {
        return false;
    };
    hints.iter().filter_map(Value::as_str).any(|hint| {
        let hint = hint.trim().to_lowercase();
        hint.chars().count() >= MIN_TOPIC_CHARS
            && (mentions(&query, &hint) || mentions(&hint, &query))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deprioritised(hints: &str) -> Fields {
        Fields::from([
            ("state".to_owned(), "deprioritised".to_owned()),
            ("reinstate_hints".to_owned(), hints.to_owned()),
        ])
    }

    #[test]
    fn reinstate_hints_are_validated_and_trimmed() {
        assert_eq!(
            validate_reinstate_hints(&["  file watching ".to_owned()]).unwrap(),
            ["file watching"]
        );
        for bad in ["", " ", "ab", "a\nb"] {
            assert!(
                validate_reinstate_hints(&[bad.to_owned()]).is_err(),
                "{bad:?} should be rejected"
            );
        }
        assert!(validate_reinstate_hints(&["x".repeat(201)]).is_err());
    }

    #[test]
    fn blank_queries_and_short_hints_never_match() {
        let doc = deprioritised(r#"["", "e", "file watching"]"#);
        assert!(!check_reinstate_eligibility(&doc, ""));
        assert!(!check_reinstate_eligibility(&doc, "   "));
        assert!(!check_reinstate_eligibility(&doc, "elephants"));
        assert!(check_reinstate_eligibility(&doc, "File watching again"));
        assert!(check_reinstate_eligibility(&doc, "watching"));
        assert!(
            !check_reinstate_eligibility(&doc, "profile watch"),
            "hints match whole words only"
        );
    }
}
