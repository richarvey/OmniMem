//! The session-start briefing (`tools/briefing.py`).
//!
//! The skill sections (suggestions, pending updates, the auto skill scan and
//! knowledge watch) arrive with the skill compiler in phase 4. There is no
//! index drift section: nothing can drift from the records any more.

use std::collections::HashSet;

use serde_json::{Map, Value, json};
use tracing::{error, warn};

use crate::classification::classification_fields;
use crate::pyfmt::{compact, now_secs, py_json, take_chars};
use crate::{Engine, Result};

const SKILL_KEY_PREFIX: &str = "mem:skill:";

impl Engine {
    /// Memory keys that compiled into a skill: exempt from the stale list,
    /// read live from each skill's manifest (issue #34).
    pub(crate) fn skill_source_keys(&self) -> HashSet<String> {
        let read = || -> Result<HashSet<String>> {
            let keys = self.store.scan_prefix(SKILL_KEY_PREFIX)?;
            let mut sources = HashSet::new();
            for row in self
                .store
                .get_fields_multi(&keys, &["source_manifest"])?
                .into_iter()
                .flatten()
            {
                if let Some(Value::Array(items)) = row
                    .get("source_manifest")
                    .and_then(|m| serde_json::from_str(m).ok())
                {
                    sources.extend(
                        items
                            .into_iter()
                            .filter_map(|v| v.as_str().map(str::to_owned)),
                    );
                }
            }
            Ok(sources)
        };
        read().unwrap_or_else(|e| {
            warn!(error = %e, "could not read skill source manifests");
            HashSet::new()
        })
    }

    /// One pass over episodic memories: stale, contradictions, reinstate candidates.
    fn scan_episodic_once(
        &self,
        stale_days: i64,
        project: Option<&str>,
    ) -> Result<(Vec<Value>, Vec<Value>, Vec<Value>)> {
        let now = now_secs();
        let cutoff = now - stale_days as f64 * 86_400.0;
        let skill_sources = self.skill_source_keys();
        let keys = self.store.scan_prefix("mem:episodic:")?;
        let rows = self.store.get_fields_multi(
            &keys,
            &[
                "state",
                "project",
                "project_name",
                "updated_at",
                "content",
                "contradictions",
                "reinstate_hints",
                "deprioritised_reason",
            ],
        )?;
        let (mut stale, mut reinstate, mut contradictions) = (Vec::new(), Vec::new(), Vec::new());
        for (key, row) in keys.iter().zip(rows) {
            let Some(data) = row else { continue };
            if let Some(p) = project {
                let doc_project = data
                    .get("project")
                    .filter(|v| !v.is_empty())
                    .or_else(|| data.get("project_name").filter(|v| !v.is_empty()));
                if doc_project.map(String::as_str) != Some(p) {
                    continue;
                }
            }
            let content = take_chars(data.get("content").map_or("", String::as_str), 80);
            match data.get("state").map(String::as_str) {
                Some("active") => {
                    let updated = data
                        .get("updated_at")
                        .and_then(|u| u.parse::<f64>().ok())
                        .unwrap_or(0.0);
                    if updated < cutoff && !skill_sources.contains(key) {
                        stale.push((
                            ((now - updated) / 86_400.0) as i64,
                            json!({
                                "key": key,
                                "content": content,
                                "days_stale": ((now - updated) / 86_400.0) as i64,
                            }),
                        ));
                    }
                    let links: Vec<Value> = data
                        .get("contradictions")
                        .and_then(|c| serde_json::from_str(c).ok())
                        .unwrap_or_default();
                    if !links.is_empty() {
                        let contradicts: Vec<&str> = links
                            .iter()
                            .filter(|c| c.is_object())
                            .map(|c| c.get("key").and_then(Value::as_str).unwrap_or(""))
                            .collect();
                        contradictions.push(
                            json!({"key": key, "content": content, "contradicts": contradicts}),
                        );
                    }
                }
                Some("deprioritised") => {
                    let hints: Vec<Value> = data
                        .get("reinstate_hints")
                        .and_then(|h| serde_json::from_str(h).ok())
                        .unwrap_or_default();
                    if !hints.is_empty() {
                        reinstate.push(json!({
                            "key": key,
                            "content": content,
                            "reason": data.get("deprioritised_reason").map_or("", String::as_str),
                            "reinstate_hints": hints,
                        }));
                    }
                }
                _ => {}
            }
        }
        stale.sort_by_key(|s| std::cmp::Reverse(s.0));
        Ok((
            stale.into_iter().take(10).map(|s| s.1).collect(),
            reinstate.into_iter().take(5).collect(),
            contradictions.into_iter().take(5).collect(),
        ))
    }

    fn new_knowledge(&self, since_days: i64) -> Result<Vec<Value>> {
        let cutoff = now_secs() - since_days as f64 * 86_400.0;
        let keys = self.store.scan_prefix("mem:knowledge:")?;
        let rows = self.store.get_fields_multi(
            &keys,
            &[
                "state",
                "created_at",
                "content",
                "source_url",
                "feed_name",
                "licence",
                "licence_note",
                "provenance",
                "enriched_from",
                "imported_at",
            ],
        )?;
        let mut articles = Vec::new();
        for (key, row) in keys.iter().zip(rows) {
            let Some(data) = row else { continue };
            if data.get("state").map(String::as_str) != Some("active") {
                continue;
            }
            if data
                .get("created_at")
                .and_then(|c| c.parse::<f64>().ok())
                .unwrap_or(0.0)
                < cutoff
            {
                continue;
            }
            let mut m = Map::new();
            m.insert("key".into(), key.as_str().into());
            m.insert(
                "content".into(),
                take_chars(data.get("content").map_or("", String::as_str), 80).into(),
            );
            m.insert(
                "source_url".into(),
                data.get("source_url")
                    .map_or(Value::Null, |v| v.as_str().into()),
            );
            m.insert(
                "feed_name".into(),
                data.get("feed_name")
                    .map_or(Value::Null, |v| v.as_str().into()),
            );
            m.extend(classification_fields(&data, "knowledge", Some(key)));
            articles.push(compact(m));
            if articles.len() >= 10 {
                break;
            }
        }
        Ok(articles)
    }

    pub fn briefing(&self, project: Option<&str>, include_knowledge: bool) -> Result<Value> {
        let project = project.filter(|p| !p.is_empty());
        let mut result = Map::new();

        if let Some(p) = project {
            match self.store.get(&format!("mem:project:{p}"))? {
                Some(data) => {
                    let mut m = Map::new();
                    m.insert("name".into(), p.into());
                    m.insert(
                        "current_state".into(),
                        data.get("content").map_or("", String::as_str).into(),
                    );
                    m.insert(
                        "updated_at".into(),
                        data.get("updated_at")
                            .map_or(Value::Null, |u| u.as_str().into()),
                    );
                    result.insert("project_context".into(), compact(m));
                }
                None => {
                    result.insert(
                        "project_context".into(),
                        json!({"name": p, "note": "not_found"}),
                    );
                }
            }
        }

        let experience = self.experience_summary(project)?;
        if experience["memories_with_experience"].as_i64().unwrap_or(0) > 0 {
            result.insert("experience_summary".into(), experience);
        }

        let (stale, reinstate, contradictions) =
            self.scan_episodic_once(self.config.stale_memory_days, project)?;
        if !stale.is_empty() {
            result.insert("stale_memories".into(), stale.into());
        }
        if !contradictions.is_empty() {
            result.insert("contradiction_warnings".into(), contradictions.into());
        }
        if !reinstate.is_empty() {
            result.insert("reinstate_candidates".into(), reinstate.into());
        }

        if include_knowledge {
            let articles = self.new_knowledge(7)?;
            if !articles.is_empty() {
                result.insert("new_knowledge".into(), articles.into());
            }
        }

        let suppressed = self.suppressed_topics()?;
        if !suppressed.is_empty() {
            result.insert("suppressed_topics".into(), suppressed.into());
        }

        if let Some(p) = project
            && self.config.auto_maintenance_interval > 0
        {
            let meta_key = format!("meta:maintenance:{p}");
            match self.store.hash_incr(&meta_key, "briefing_count", 1) {
                Ok(count) if count >= self.config.auto_maintenance_interval => {
                    let report = self.run_maintenance(p);
                    let summary = json!({
                        "duplicates_archived": report["duplicates_archived"].as_array().map_or(0, Vec::len),
                        "contradictions_found": report["contradictions_found"].as_array().map_or(0, Vec::len),
                    });
                    let fields = omnimem_store::Fields::from([
                        ("briefing_count".to_owned(), "0".to_owned()),
                        (
                            "last_maintenance_at".to_owned(),
                            report["ran_at"].as_str().unwrap_or("").to_owned(),
                        ),
                        ("last_maintenance_summary".to_owned(), py_json(&summary)),
                    ]);
                    if let Err(e) = self.store.hash_set(&meta_key, &fields) {
                        error!(project = p, error = %e, "could not record maintenance");
                    }
                    result.insert("auto_maintenance".into(), report);
                }
                Ok(_) => {}
                Err(e) => error!(project = p, error = %e, "auto-maintenance failed"),
            }
        }
        Ok(Value::Object(result))
    }
}
