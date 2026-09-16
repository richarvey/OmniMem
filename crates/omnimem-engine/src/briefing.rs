//! The session-start briefing (`tools/briefing.py`).
//!
//! The skill sections (suggestions, pending updates, the auto skill scan and
//! knowledge watch) arrive with the skill compiler in phase 4. There is no
//! index drift section: nothing can drift from the records any more.

use std::collections::HashSet;

use omnimem_core::Namespace;
use omnimem_store::MemoryFilter;
use serde_json::{Map, Value, json};
use tracing::{error, warn};

use crate::classification::classification_fields;
use crate::pyfmt::{compact, now_secs, py_json, take_chars};
use crate::tools::validate_project_name;
use crate::{Engine, Result};

impl Engine {
    /// Memory keys that compiled into a skill: exempt from the stale list,
    /// read live from each skill's manifest (issue #34).
    pub(crate) fn skill_source_keys(&self) -> HashSet<String> {
        let read = || -> Result<HashSet<String>> {
            let mut sources = HashSet::new();
            for (_, row) in self.store.list_memories(
                Namespace::Skill,
                &MemoryFilter::default(),
                &["source_manifest"],
            )? {
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
        // Only active and deprioritised rows of the project take part, so
        // the filter runs in SQL and archived memories, other projects'
        // memories and every unread field stay in the database.
        let filter = MemoryFilter {
            states: vec!["active".to_owned(), "deprioritised".to_owned()],
            project: project.map(str::to_owned),
            ..MemoryFilter::default()
        };
        let rows = self.store.list_memories(
            Namespace::Episodic,
            &filter,
            &[
                "state",
                "updated_at",
                "content",
                "contradictions",
                "reinstate_hints",
                "deprioritised_reason",
            ],
        )?;
        let (mut stale, mut reinstate, mut contradictions) = (Vec::new(), Vec::new(), Vec::new());
        for (key, data) in &rows {
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
        // The week's articles out of a knowledge namespace that RSS fills
        // for years: state and age are filtered in SQL, and the age is
        // re-checked below because the store's cut is a superset.
        let filter = MemoryFilter {
            states: vec!["active".to_owned()],
            created_at_min: Some(cutoff),
            ..MemoryFilter::default()
        };
        let rows = self.store.list_memories(
            Namespace::Knowledge,
            &filter,
            &[
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
        for (key, data) in &rows {
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
            m.extend(classification_fields(data, "knowledge", Some(key)));
            articles.push(compact(m));
            if articles.len() >= 10 {
                break;
            }
        }
        Ok(articles)
    }

    pub fn briefing(&self, project: Option<&str>, include_knowledge: bool) -> Result<Value> {
        let project = project.filter(|p| !p.is_empty());
        // The name becomes part of the `meta:maintenance:` key below, so it
        // must be a project name and not an arbitrary key fragment.
        validate_project_name(project)?;
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
        if let Some(p) = project
            && let Err(e) = self.briefing_skill_sections(p, &mut result)
        {
            error!(project = p, error = %e, "skill briefing sections failed");
        }
        Ok(Value::Object(result))
    }
}
