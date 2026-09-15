//! Experience tools (`tools/experience.py`).

use chrono::Utc;
use omnimem_store::Fields;
use serde_json::{Map, Value, json};
use tracing::info;

use crate::error::invalid;
use crate::lifecycle::SUPPRESSED_KEY;
use crate::pyfmt::{compact, now_str, py_float, py_json, round_to, take_chars};
use crate::recall::compute_experience_weight;
use crate::{Engine, Result};

const OUTCOMES: [&str; 3] = ["succeeded", "pivoted", "abandoned"];
const APPROACH_TYPES: [&str; 5] = ["approach", "library", "pattern", "service", "tool"];

fn validate_memory_key(key: &str) -> Result<()> {
    if key.starts_with("mem:") {
        Ok(())
    } else {
        Err(invalid("Key must start with 'mem:' prefix"))
    }
}

/// Python's repr of a set of strings, for error messages.
fn set_repr(items: &[&str]) -> String {
    let quoted: Vec<String> = items.iter().map(|i| format!("'{i}'")).collect();
    format!("{{{}}}", quoted.join(", "))
}

fn parse_list(raw: Option<&String>) -> Vec<Value> {
    raw.and_then(|r| serde_json::from_str::<Value>(r).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
}

fn effort(fields: &Fields) -> Option<i64> {
    fields
        .get("effort_score")
        .and_then(|e| e.trim().parse::<f64>().ok())
        .map(|e| e as i64)
}

impl Engine {
    #[allow(clippy::too_many_arguments)]
    pub fn record_experience(
        &self,
        key: &str,
        effort_score: i64,
        outcome: &str,
        iterations: i64,
        abandoned_approaches: Option<Vec<Map<String, Value>>>,
        breakthrough: Option<&str>,
        gotchas: Option<&str>,
        lesson: Option<&str>,
    ) -> Result<Value> {
        validate_memory_key(key)?;
        if !(1..=5).contains(&effort_score) {
            return Err(invalid(format!(
                "effort_score must be 1-5, got {effort_score}"
            )));
        }
        if !OUTCOMES.contains(&outcome) {
            return Err(invalid(format!(
                "outcome must be one of {}, got '{outcome}'",
                set_repr(&OUTCOMES)
            )));
        }
        let Some(data) = self.store.get(key)? else {
            return Err(invalid(format!("Memory key not found: {key}")));
        };

        let weight = compute_experience_weight(effort_score, outcome);
        let mut updates = Fields::from([
            ("effort_score".to_owned(), effort_score.to_string()),
            ("outcome".to_owned(), outcome.to_owned()),
            ("iterations".to_owned(), iterations.to_string()),
            ("experience_weight".to_owned(), py_float(weight)),
            ("updated_at".to_owned(), now_str()),
        ]);
        let abandoned = abandoned_approaches.filter(|a| !a.is_empty());
        if let Some(approaches) = &abandoned {
            let mut existing = parse_list(data.get("abandoned_approaches"));
            existing.extend(approaches.iter().cloned().map(Value::Object));
            updates.insert(
                "abandoned_approaches".to_owned(),
                py_json(&Value::Array(existing)),
            );
        }
        for (name, value) in [
            ("breakthrough", breakthrough),
            ("gotchas", gotchas),
            ("lesson", lesson),
        ] {
            if let Some(v) = value.filter(|v| !v.is_empty()) {
                updates.insert(name.to_owned(), v.to_owned());
            }
        }
        self.store.set_fields(key, &updates)?;
        if abandoned.is_some() {
            self.invalidate_abandoned_cache();
        }

        let mut suppressed = Vec::new();
        if effort_score >= 4
            && outcome == "abandoned"
            && let Some(approaches) = &abandoned
        {
            for approach in approaches {
                let name = approach.get("name").and_then(Value::as_str).unwrap_or("");
                if !name.is_empty() {
                    self.store.set_add(SUPPRESSED_KEY, &[name.to_lowercase()])?;
                    info!(
                        topic = name,
                        effort_score, "auto-suppressed abandoned approach"
                    );
                    suppressed.push(name.to_owned());
                }
            }
        }

        let mut result = Map::new();
        result.insert("key".into(), key.into());
        result.insert("effort_score".into(), effort_score.into());
        result.insert("outcome".into(), outcome.into());
        result.insert("experience_weight".into(), json!(weight));
        if !suppressed.is_empty() {
            result.insert("auto_suppressed".into(), suppressed.into());
        }
        Ok(Value::Object(result))
    }

    pub fn log_abandoned(&self, key: &str, name: &str, kind: &str, reason: &str) -> Result<Value> {
        validate_memory_key(key)?;
        if !APPROACH_TYPES.contains(&kind) {
            return Err(invalid(format!(
                "type must be one of {}, got '{kind}'",
                set_repr(&APPROACH_TYPES)
            )));
        }
        let Some(data) = self.store.get(key)? else {
            return Err(invalid(format!("Memory key not found: {key}")));
        };
        let mut existing = parse_list(data.get("abandoned_approaches"));
        let entry = json!({
            "name": name,
            "type": kind,
            "reason": reason,
            "attempted_at": Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        });
        existing.push(entry.clone());
        let updates = Fields::from([
            (
                "abandoned_approaches".to_owned(),
                py_json(&Value::Array(existing.clone())),
            ),
            ("updated_at".to_owned(), now_str()),
        ]);
        self.store.set_fields(key, &updates)?;
        self.invalidate_abandoned_cache();
        Ok(json!({"key": key, "abandoned_count": existing.len(), "latest_entry": entry}))
    }

    pub fn get_experience(&self, key: &str) -> Result<Value> {
        validate_memory_key(key)?;
        let Some(data) = self.store.get(key)? else {
            return Ok(json!({"status": "not_found"}));
        };
        let Some(effort) = effort(&data) else {
            return Ok(json!({"status": "no_experience"}));
        };
        let iterations = data
            .get("iterations")
            .and_then(|i| i.trim().parse::<f64>().ok())
            .map_or(1, |i| i as i64);
        let mut m = Map::new();
        m.insert("status".into(), "found".into());
        m.insert("key".into(), key.into());
        m.insert("effort_score".into(), effort.into());
        m.insert(
            "outcome".into(),
            data.get("outcome").map_or("unknown", String::as_str).into(),
        );
        m.insert("iterations".into(), iterations.into());
        m.insert(
            "abandoned_approaches".into(),
            Value::Array(parse_list(data.get("abandoned_approaches"))),
        );
        for name in ["breakthrough", "lesson", "gotchas"] {
            m.insert(
                name.into(),
                data.get(name)
                    .map_or(Value::Null, |v| Value::from(v.as_str())),
            );
        }
        m.insert(
            "experience_weight".into(),
            data.get("experience_weight")
                .map_or("1.0", String::as_str)
                .into(),
        );
        Ok(compact(m))
    }

    pub fn experience_summary(&self, project: Option<&str>) -> Result<Value> {
        let project = project.filter(|p| !p.is_empty());
        let keys = self.store.scan_prefix("mem:episodic:")?;
        let rows = self.store.get_fields_multi(
            &keys,
            &[
                "effort_score",
                "outcome",
                "content",
                "abandoned_approaches",
                "breakthrough",
                "project",
            ],
        )?;
        let (mut total_effort, mut count) = (0i64, 0i64);
        let mut outcomes: Vec<(&str, i64)> = OUTCOMES.iter().map(|o| (*o, 0)).collect();
        let mut effortful: Vec<(i64, Value)> = Vec::new();
        let mut graveyard: Vec<Value> = Vec::new();
        let mut seen_names: Vec<String> = Vec::new();
        let mut breakthroughs: Vec<(i64, Value)> = Vec::new();

        for (key, row) in keys.iter().zip(rows) {
            let Some(data) = row else { continue };
            if let Some(p) = project
                && data.get("project").map(String::as_str) != Some(p)
            {
                continue;
            }
            let Some(effort) = effort(&data) else {
                continue;
            };
            let outcome = data.get("outcome").map_or("unknown", String::as_str);
            count += 1;
            total_effort += effort;
            if let Some(slot) = outcomes.iter_mut().find(|(o, _)| *o == outcome) {
                slot.1 += 1;
            }
            effortful.push((
                effort,
                json!({
                    "key": key,
                    "content": take_chars(data.get("content").map_or("", String::as_str), 80),
                    "effort_score": effort,
                    "outcome": outcome,
                }),
            ));
            for approach in parse_list(data.get("abandoned_approaches")) {
                if let Value::Object(a) = approach {
                    let name = a
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                        .to_owned();
                    if seen_names.contains(&name.to_lowercase()) {
                        continue;
                    }
                    seen_names.push(name.to_lowercase());
                    graveyard.push(json!({
                        "name": name,
                        "type": a.get("type").and_then(Value::as_str).unwrap_or("?"),
                        "reason": a.get("reason").and_then(Value::as_str).unwrap_or(""),
                        "effort_score": effort,
                    }));
                }
            }
            if let Some(bt) = data.get("breakthrough").filter(|b| !b.is_empty())
                && outcome == "succeeded"
            {
                breakthroughs.push((
                    effort,
                    json!({"key": key, "effort_score": effort, "breakthrough": bt}),
                ));
            }
        }
        effortful.sort_by_key(|e| std::cmp::Reverse(e.0));
        breakthroughs.sort_by_key(|e| std::cmp::Reverse(e.0));

        let mut m = Map::new();
        m.insert("memories_with_experience".into(), count.into());
        m.insert(
            "average_effort_score".into(),
            if count > 0 {
                json!(round_to(total_effort as f64 / count as f64, 2))
            } else {
                json!(0)
            },
        );
        m.insert(
            "outcome_breakdown".into(),
            Value::Object(
                outcomes
                    .into_iter()
                    .map(|(o, n)| (o.to_owned(), n.into()))
                    .collect(),
            ),
        );
        m.insert(
            "top_5_most_effortful".into(),
            effortful.into_iter().take(5).map(|e| e.1).collect(),
        );
        m.insert("graveyard".into(), Value::Array(graveyard));
        m.insert(
            "top_3_breakthroughs".into(),
            breakthroughs.into_iter().take(3).map(|e| e.1).collect(),
        );
        Ok(compact(m))
    }

    pub fn warn_if_abandoned(&self, query: &str) -> Result<Value> {
        let matches = self.abandoned_matches(query)?;
        if matches.is_empty() {
            return Ok(json!({"status": "clear"}));
        }
        let matches: Vec<Value> = matches
            .into_iter()
            .map(|m| {
                json!({
                    "memory_key": m.memory_key,
                    "abandoned_name": m.abandoned_name,
                    "reason": m.reason,
                    "effort_score": m.effort_score,
                    "project": m.project,
                })
            })
            .collect();
        Ok(json!({"status": "warning", "matches": matches}))
    }
}
