//! The recall pipeline (`memory/recall.py`): abandoned fast-path, vector
//! search, scoring, fact collapse, the relevance floor, and recall logging.
//!
//! Query expansion is not here yet (phase 5); `expand_queries` is accepted
//! and has no effect until it is.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use omnimem_core::Namespace;
use omnimem_store::{Fields, SearchFilter};
use serde_json::{Value, json};
use tracing::{debug, warn};

use crate::classification::{effective_licence, effective_provenance};
use crate::lifecycle::check_reinstate_eligibility;
use crate::pyfmt::{now_secs, py_float, py_json, take_chars};
use crate::temporal::{parse_query_date, temporal_boost};
use crate::{Engine, Result};

pub const NAMESPACES: [&str; 4] = ["episodic", "project", "knowledge", "preference"];

const MAX_ABANDONED_SCAN_KEYS: usize = 5000;
const RECALL_LOG_TTL: Duration = Duration::from_secs(30 * 86_400);

#[derive(Debug, Clone)]
pub(crate) struct AbandonedEntry {
    pub memory_key: String,
    pub name_lower: String,
    pub abandoned_name: String,
    pub reason: String,
    pub effort_score: Option<i64>,
    pub project: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecallResult {
    pub key: String,
    pub namespace: String,
    pub content: String,
    /// Raw similarity: what the relevance floor reads.
    pub score: f64,
    pub adjusted_score: f64,
    pub state: String,
    pub project: Option<String>,
    pub source_url: Option<String>,
    pub published_at: Option<String>,
    pub reinstate_candidate: bool,
    pub tags: Value,
    pub deprioritised_reason: Option<String>,
    pub effort_score: Option<i64>,
    pub outcome: Option<String>,
    pub experience_weight: f64,
    /// `memory`, `knowledge` or `abandoned_warning`.
    pub result_type: &'static str,
    pub weak_match: bool,
    pub breakthrough: Option<String>,
    pub lesson: Option<String>,
    pub contradictions: Vec<Value>,
    pub event_date: Option<f64>,
    pub enriched_from: Option<String>,
    pub licence: Option<String>,
    pub licence_note: Option<String>,
    pub provenance: Option<String>,
}

/// Effort multiplies success; it never amplifies an abandoned outcome.
pub fn compute_experience_weight(effort_score: i64, outcome: &str) -> f64 {
    let base = match outcome {
        "pivoted" => 0.7,
        "abandoned" => 0.1,
        _ => 1.0,
    };
    let multiplier = match effort_score {
        2 => 1.1,
        3 => 1.25,
        4 => 1.5,
        5 => 1.8,
        _ => 1.0,
    };
    if outcome == "abandoned" {
        return base;
    }
    (base * multiplier).min(2.0)
}

/// KNN candidates per namespace: enough for `top_k`, and more under a
/// project filter so it still has matches left after discarding.
fn candidate_k(top_k: i64, projects: &[String]) -> usize {
    let mut k = top_k.max(20);
    if !projects.is_empty() {
        k = k.max((50 + 10 * (projects.len() as i64 - 1)).min(100));
    }
    k as usize
}

fn text<'a>(fields: &'a Fields, name: &str) -> Option<&'a str> {
    fields
        .get(name)
        .map(String::as_str)
        .filter(|v| !v.is_empty())
}

fn number(fields: &Fields, name: &str, default: f64) -> f64 {
    fields
        .get(name)
        .and_then(|v| v.trim().parse::<f64>().ok())
        .unwrap_or(default)
}

fn integer(fields: &Fields, name: &str) -> Option<i64> {
    fields
        .get(name)
        .and_then(|v| v.trim().parse::<f64>().ok())
        .map(|v| v as i64)
}

fn parse_tags(raw: Option<&str>) -> Value {
    let Some(raw) = raw.filter(|r| !r.is_empty()) else {
        return json!([]);
    };
    serde_json::from_str(raw).unwrap_or_else(|_| {
        Value::from(
            raw.split(',')
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>(),
        )
    })
}

fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
    }
}

pub(crate) fn tags_truthy(tags: &Value) -> bool {
    is_truthy(tags)
}

impl Engine {
    /// Every abandoned approach across episodic memories, cached.
    pub(crate) fn abandoned_entries(&self) -> Result<Arc<Vec<AbandonedEntry>>> {
        let ttl = self.config.abandoned_cache_ttl;
        {
            let cache = self.abandoned.lock().unwrap_or_else(|p| p.into_inner());
            if let Some((at, entries)) = cache.as_ref()
                && !ttl.is_zero()
                && at.elapsed() < ttl
            {
                return Ok(entries.clone());
            }
        }
        let mut keys = self.store.scan_prefix("mem:episodic:")?;
        if keys.len() > MAX_ABANDONED_SCAN_KEYS {
            warn!(
                cap = MAX_ABANDONED_SCAN_KEYS,
                total = keys.len(),
                "abandoned scan capped"
            );
            keys.truncate(MAX_ABANDONED_SCAN_KEYS);
        }
        let rows = self
            .store
            .get_fields_multi(&keys, &["abandoned_approaches", "effort_score", "project"])?;
        let mut entries = Vec::new();
        for (key, row) in keys.iter().zip(rows) {
            let Some(row) = row else { continue };
            let Some(approaches) = row
                .get("abandoned_approaches")
                .and_then(|raw| serde_json::from_str::<Vec<Value>>(raw).ok())
            else {
                continue;
            };
            let effort = integer(&row, "effort_score");
            for approach in approaches {
                let Some(name) = approach
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|n| !n.is_empty())
                else {
                    continue;
                };
                entries.push(AbandonedEntry {
                    memory_key: key.clone(),
                    name_lower: name.to_lowercase(),
                    abandoned_name: name.to_owned(),
                    reason: approach
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    effort_score: effort,
                    project: row.get("project").cloned(),
                });
            }
        }
        let entries = Arc::new(entries);
        *self.abandoned.lock().unwrap_or_else(|p| p.into_inner()) =
            Some((Instant::now(), entries.clone()));
        Ok(entries)
    }

    /// Abandoned approaches whose name the query mentions, or which mention it.
    pub(crate) fn abandoned_matches(&self, query: &str) -> Result<Vec<AbandonedEntry>> {
        let query = query.to_lowercase();
        let mut seen = Vec::new();
        let mut matches = Vec::new();
        for entry in self.abandoned_entries()?.iter() {
            let name = &entry.name_lower;
            if query.contains(name.as_str()) || name.contains(query.as_str()) {
                let id = format!("{}:{name}", entry.memory_key);
                if seen.contains(&id) {
                    continue;
                }
                seen.push(id);
                matches.push(entry.clone());
            }
        }
        Ok(matches)
    }

    /// The full pipeline. `projects` empty means unscoped. `min_score`
    /// overrides the relevance floor for this call; 0 switches it off.
    pub fn recall_results(
        &self,
        query: &str,
        namespaces: Option<&[String]>,
        top_k: Option<i64>,
        projects: &[String],
        min_score: Option<f64>,
    ) -> Result<Vec<RecallResult>> {
        let top_k = top_k.unwrap_or(self.config.recall_top_k).clamp(1, 50);
        let namespaces: Vec<String> = match namespaces {
            Some(ns) if !ns.is_empty() => ns.to_vec(),
            _ => NAMESPACES.iter().map(|s| (*s).to_owned()).collect(),
        };

        let mut results: Vec<RecallResult> = Vec::new();
        for warning in self.abandoned_matches(query)? {
            results.push(RecallResult {
                key: warning.memory_key,
                namespace: "episodic".into(),
                content: format!(
                    "Abandoned approach: {} — {}",
                    warning.abandoned_name, warning.reason
                ),
                score: 1.0,
                adjusted_score: 1.0,
                state: "active".into(),
                project: warning.project,
                source_url: None,
                published_at: None,
                reinstate_candidate: false,
                tags: json!([]),
                deprioritised_reason: None,
                effort_score: warning.effort_score,
                outcome: None,
                experience_weight: 1.0,
                result_type: "abandoned_warning",
                weak_match: false,
                breakthrough: None,
                lesson: None,
                contradictions: Vec::new(),
                event_date: None,
                enriched_from: None,
                licence: None,
                licence_note: None,
                provenance: None,
            });
        }

        let vector = self.embed(query)?;
        let now = now_secs();
        let suppressed = self.suppressed_topics()?;
        let query_date = parse_query_date(query);
        let filter = SearchFilter {
            states: vec!["active".into(), "deprioritised".into()],
            projects: projects.to_vec(),
        };
        let per_ns_k = candidate_k(top_k, projects);

        for ns in &namespaces {
            let Ok(namespace) = ns.parse::<Namespace>() else {
                continue;
            };
            for hit in self
                .store
                .search(namespace, &vector, per_ns_k, &filter, None)?
            {
                let doc = &hit.fields;
                let state = doc.get("state").map(String::as_str).unwrap_or("active");
                if matches!(state, "archived" | "deleted") {
                    continue;
                }
                let content = doc.get("content").cloned().unwrap_or_default();
                if !suppressed.is_empty() {
                    let lower = content.to_lowercase();
                    if suppressed.iter().any(|t| lower.contains(t.as_str())) {
                        continue;
                    }
                }
                let doc_project = text(doc, "project").or_else(|| text(doc, "project_name"));
                if !projects.is_empty()
                    && !doc_project.is_some_and(|p| projects.iter().any(|q| q == p))
                {
                    continue;
                }

                let raw_score = (1.0 - f64::from(hit.distance)).max(0.0);
                let surface = number(doc, "surface_score", 1.0);
                let created_at = number(doc, "created_at", now);
                let age_days = (now - created_at) / 86_400.0;
                let mut recency = 1.0;
                if age_days > self.config.recency_decay_days {
                    let excess = (age_days - self.config.recency_decay_days) / 30.0;
                    recency = (1.0 - 0.05 * excess).max(0.3);
                }
                let exp_weight = number(doc, "experience_weight", 1.0);
                let event_date = doc
                    .get("event_date")
                    .filter(|v| !v.is_empty())
                    .and_then(|v| v.trim().parse::<f64>().ok());
                let temporal = match (query_date, event_date) {
                    (Some(q), Some(e)) => temporal_boost(q, e),
                    _ => 1.0,
                };
                let mut adjusted = raw_score * surface * recency * exp_weight * temporal;

                let mut reinstate = false;
                if state == "deprioritised" && check_reinstate_eligibility(doc, query) {
                    reinstate = true;
                    adjusted = 0.6;
                }
                let contradictions = doc
                    .get("contradictions")
                    .filter(|v| !v.is_empty())
                    .and_then(|raw| serde_json::from_str::<Vec<Value>>(raw).ok())
                    .unwrap_or_default();

                results.push(RecallResult {
                    key: hit.key.clone(),
                    namespace: ns.clone(),
                    content,
                    score: raw_score,
                    adjusted_score: adjusted,
                    state: state.to_owned(),
                    project: doc_project.map(str::to_owned),
                    source_url: doc.get("source_url").cloned(),
                    published_at: doc.get("published_at").cloned(),
                    reinstate_candidate: reinstate,
                    tags: parse_tags(doc.get("tags").map(String::as_str)),
                    deprioritised_reason: doc.get("deprioritised_reason").cloned(),
                    effort_score: integer(doc, "effort_score"),
                    outcome: doc.get("outcome").cloned(),
                    experience_weight: exp_weight,
                    result_type: if ns == "knowledge" {
                        "knowledge"
                    } else {
                        "memory"
                    },
                    weak_match: false,
                    breakthrough: doc.get("breakthrough").cloned(),
                    lesson: doc.get("lesson").cloned(),
                    contradictions,
                    event_date,
                    enriched_from: doc.get("enriched_from").cloned(),
                    licence: Some(effective_licence(doc, ns).to_owned()),
                    licence_note: doc.get("licence_note").cloned(),
                    provenance: Some(effective_provenance(doc, ns, Some(&hit.key)).to_owned()),
                });
            }
        }

        // Dedupe on (key, result type), keeping the best adjusted score in
        // the position the key first appeared.
        let mut deduped: Vec<RecallResult> = Vec::new();
        let mut index: HashMap<(String, &'static str), usize> = HashMap::new();
        for r in results {
            match index.get(&(r.key.clone(), r.result_type)) {
                Some(&i) => {
                    if r.adjusted_score > deduped[i].adjusted_score {
                        deduped[i] = r;
                    }
                }
                None => {
                    index.insert((r.key.clone(), r.result_type), deduped.len());
                    deduped.push(r);
                }
            }
        }
        let mut results = deduped;

        // An extracted fact and its verbatim source both matched: keep the
        // source, promoted to the fact's scores when the fact ranked higher.
        let memory_by_key: HashMap<String, usize> = results
            .iter()
            .enumerate()
            .filter(|(_, r)| r.result_type != "abandoned_warning")
            .map(|(i, r)| (r.key.clone(), i))
            .collect();
        let mut dropped = vec![false; results.len()];
        for i in 0..results.len() {
            if results[i].result_type == "abandoned_warning" {
                continue;
            }
            let Some(source) = results[i]
                .enriched_from
                .as_deref()
                .filter(|s| !s.is_empty())
                .and_then(|s| memory_by_key.get(s).copied())
            else {
                continue;
            };
            let (fact_adjusted, fact_score) = (results[i].adjusted_score, results[i].score);
            if fact_adjusted > results[source].adjusted_score {
                results[source].adjusted_score = fact_adjusted;
                results[source].score = results[source].score.max(fact_score);
            }
            dropped[i] = true;
        }
        let mut results: Vec<RecallResult> = results
            .into_iter()
            .zip(dropped)
            .filter(|(_, d)| !d)
            .map(|(r, _)| r)
            .collect();
        results.sort_by(|a, b| {
            b.adjusted_score
                .partial_cmp(&a.adjusted_score)
                .unwrap_or(Ordering::Equal)
        });

        let exempt =
            |r: &RecallResult| r.result_type == "abandoned_warning" || r.reinstate_candidate;
        let floor = min_score.map_or(self.config.recall_min_score, |m| m.max(0.0));
        if floor > 0.0 {
            let before = results.len();
            results.retain(|r| r.score >= floor || exempt(r));
            if results.len() < before {
                debug!(
                    floor,
                    dropped = before - results.len(),
                    "relevance floor applied"
                );
            }
        }
        let weak = self.config.recall_weak_score;
        if weak > 0.0 {
            for r in &mut results {
                if r.score < weak && !exempt(r) {
                    r.weak_match = true;
                }
            }
        }
        results.truncate(top_k as usize);
        self.log_recall_event(query, &results);
        Ok(results)
    }

    /// A `log:recall:*` entry with a 30-day expiry, and the per-memory
    /// recall counters. Failures are logged, never raised.
    fn log_recall_event(&self, query: &str, results: &[RecallResult]) {
        let timestamp = py_float(now_secs());
        let top: Vec<&RecallResult> = results.iter().take(10).collect();
        let keys: Vec<String> = top.iter().map(|r| r.key.clone()).collect();
        let scores: Vec<String> = top.iter().map(|r| py_float(r.adjusted_score)).collect();
        let log_key = format!("log:recall:{timestamp}");
        let fields = Fields::from([
            ("query".to_owned(), take_chars(query, 2000)),
            ("timestamp".to_owned(), timestamp.clone()),
            ("result_keys".to_owned(), py_json(&json!(keys))),
            ("result_scores".to_owned(), py_json(&json!(scores))),
        ]);
        let outcome = self
            .store
            .hash_set(&log_key, &fields)
            .and_then(|_| self.store.expire(&log_key, RECALL_LOG_TTL))
            .and_then(|_| self.store.bump_recall_counts(&keys, &timestamp));
        if let Err(e) = outcome {
            warn!(error = %e, "failed to log recall event");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn experience_weights() {
        assert_eq!(compute_experience_weight(3, "succeeded"), 1.25);
        assert_eq!(compute_experience_weight(5, "abandoned"), 0.1);
        assert!((compute_experience_weight(2, "pivoted") - 0.77).abs() < 1e-9);
        assert_eq!(compute_experience_weight(9, "whatever"), 1.0);
    }

    #[test]
    fn candidate_budget() {
        assert_eq!(candidate_k(5, &[]), 20);
        assert_eq!(candidate_k(30, &[]), 30);
        assert_eq!(candidate_k(5, &["a".into()]), 50);
        assert_eq!(candidate_k(5, &vec!["p".to_owned(); 9]), 100);
    }

    #[test]
    fn tags_parse_json_or_commas() {
        assert_eq!(parse_tags(Some(r#"["a","b"]"#)), json!(["a", "b"]));
        assert_eq!(parse_tags(Some("a, b,")), json!(["a", "b"]));
        assert_eq!(parse_tags(None), json!([]));
    }
}
