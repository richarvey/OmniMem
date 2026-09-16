//! Audit tools (`tools/audit.py`).

use omnimem_core::Namespace;
use omnimem_store::{Fields, MemoryFilter};
use serde_json::{Map, Value, json};

use crate::classification::classification_fields;
use crate::error::invalid;
use crate::pyfmt::{compact, round_to, take_chars};
use crate::{Engine, Result};

const AUDIT_NAMESPACES: [&str; 4] = ["episodic", "knowledge", "preference", "project"];
const REINDEX_ORDER: [&str; 5] = ["episodic", "project", "knowledge", "preference", "skill"];
const AUDIT_MAX_LIMIT: i64 = 500;

fn safe_json(raw: Option<&String>) -> Value {
    raw.filter(|r| !r.is_empty())
        .and_then(|r| serde_json::from_str(r).ok())
        .unwrap_or(json!([]))
}

fn doc_project(fields: &Fields) -> Option<&str> {
    fields
        .get("project")
        .filter(|p| !p.is_empty())
        .or_else(|| fields.get("project_name").filter(|p| !p.is_empty()))
        .map(String::as_str)
}

impl Engine {
    pub fn memory_audit(
        &self,
        project: Option<&str>,
        namespace: Option<&str>,
        include_archived: bool,
        limit: i64,
        offset: i64,
    ) -> Result<Value> {
        let limit = limit.clamp(1, AUDIT_MAX_LIMIT) as usize;
        let offset = offset.max(0) as usize;
        let project = project.filter(|p| !p.is_empty());
        let namespaces: Vec<&str> = match namespace.filter(|n| !n.is_empty()) {
            Some(ns) => {
                if !AUDIT_NAMESPACES.contains(&ns) {
                    return Err(invalid(format!(
                        "Invalid namespace '{ns}'. Must be one of: {}",
                        AUDIT_NAMESPACES.join(", ")
                    )));
                }
                vec![ns]
            }
            None => AUDIT_NAMESPACES.to_vec(),
        };

        let mut counts: Vec<(String, i64)> = ["active", "deprioritised", "archived", "deleted"]
            .iter()
            .map(|s| ((*s).to_owned(), 0))
            .collect();
        let bump = |counts: &mut Vec<(String, i64)>, state: &str| match counts
            .iter_mut()
            .find(|(s, _)| s == state)
        {
            Some(slot) => slot.1 += 1,
            None => counts.push((state.to_owned(), 1)),
        };
        let mut entries = Vec::new();
        let mut matching = 0usize;
        for ns in namespaces {
            let Ok(namespace) = ns.parse::<Namespace>() else {
                continue;
            };
            // Every row counts towards the summary, archived ones before the
            // project filter applies, so the listing is unfiltered; the
            // projection alone keeps the read to the six fields shown.
            let rows = self.store.list_memories(
                namespace,
                &MemoryFilter::default(),
                &[
                    "state",
                    "content",
                    "effort_score",
                    "outcome",
                    "project",
                    "project_name",
                ],
            )?;
            for (key, data) in &rows {
                let state = data.get("state").map_or("active", String::as_str);
                if state == "archived" && !include_archived {
                    bump(&mut counts, "archived");
                    continue;
                }
                if let Some(p) = project
                    && doc_project(data) != Some(p)
                {
                    continue;
                }
                bump(&mut counts, state);
                matching += 1;
                if matching - 1 < offset || entries.len() >= limit {
                    continue;
                }
                let mut e = Map::new();
                e.insert("key".into(), key.as_str().into());
                e.insert(
                    "content".into(),
                    take_chars(data.get("content").map_or("", String::as_str), 80).into(),
                );
                e.insert("state".into(), state.into());
                e.insert(
                    "effort_score".into(),
                    data.get("effort_score")
                        .and_then(|v| v.trim().parse::<f64>().ok())
                        .map_or(Value::Null, |v| (v as i64).into()),
                );
                e.insert(
                    "outcome".into(),
                    data.get("outcome")
                        .map_or(Value::Null, |o| o.as_str().into()),
                );
                e.insert(
                    "project".into(),
                    doc_project(data).map_or(Value::Null, Value::from),
                );
                entries.push(compact(e));
            }
        }
        let total: i64 = counts.iter().map(|(_, n)| n).sum();
        Ok(json!({
            "summary": Value::Object(counts.into_iter().map(|(s, n)| (s, n.into())).collect()),
            "total": total,
            "matching_total": matching,
            "offset": offset,
            "limit": limit,
            "returned": entries.len(),
            "has_more": offset + entries.len() < matching,
            "entries": entries,
        }))
    }

    pub fn why_did_you_mention(&self, query: &str) -> Result<Value> {
        let mut log_keys = self.store.scan_prefix("log:recall:")?;
        log_keys.sort_by(|a, b| b.cmp(a));
        log_keys.truncate(50);
        if log_keys.is_empty() {
            return Ok(json!({"status": "not_found"}));
        }
        let rows = self.store.get_multi(&log_keys)?;
        let query_lower = query.to_lowercase();
        let found = |data: &Fields, match_type: &str, similarity: Option<f64>| {
            let mut m = Map::new();
            m.insert("status".into(), "found".into());
            m.insert("match_type".into(), match_type.into());
            if let Some(s) = similarity {
                m.insert("similarity".into(), json!(round_to(s, 4)));
            }
            m.insert(
                "log_query".into(),
                data.get("query").map_or("", String::as_str).into(),
            );
            m.insert(
                "timestamp".into(),
                data.get("timestamp")
                    .map_or(Value::Null, |t| t.as_str().into()),
            );
            m.insert("result_keys".into(), safe_json(data.get("result_keys")));
            compact(m)
        };
        let mut others: Vec<Fields> = Vec::new();
        for data in rows.into_iter().flatten() {
            let log_query = data.get("query").map_or("", String::as_str).to_lowercase();
            if log_query.contains(&query_lower) || query_lower.contains(&log_query) {
                return Ok(found(&data, "keyword", None));
            }
            others.push(data);
        }
        if others.is_empty() {
            return Ok(json!({"status": "not_found"}));
        }
        let query_vector = self.embed(query)?;
        let texts: Vec<&str> = others
            .iter()
            .map(|d| d.get("query").map_or("", String::as_str))
            .collect();
        let vectors = self.embed_many(&texts)?;
        let mut best: Option<(f64, usize)> = None;
        for (i, v) in vectors.iter().enumerate() {
            let sim = f64::from(query_vector.iter().zip(v).map(|(a, b)| a * b).sum::<f32>());
            if sim > best.map_or(0.0, |b| b.0) {
                best = Some((sim, i));
            }
        }
        match best {
            Some((sim, i)) if sim > 0.5 => Ok(found(&others[i], "semantic", Some(sim))),
            _ => Ok(json!({"status": "not_found"})),
        }
    }

    pub fn explain_memory(&self, key: &str) -> Result<Value> {
        if !key.starts_with("mem:") && !key.starts_with("log:recall:") {
            return Err(invalid(
                "Key must start with 'mem:' or 'log:recall:' prefix",
            ));
        }
        let Some(data) = self.store.get(key)? else {
            return Ok(json!({"status": "not_found"}));
        };
        let opt = |name: &str| data.get(name).map_or(Value::Null, |v| v.as_str().into());
        let mut m = Map::new();
        m.insert("status".into(), "found".into());
        m.insert("key".into(), key.into());
        m.insert("content".into(), opt("content"));
        m.insert(
            "state".into(),
            data.get("state").map_or("active", String::as_str).into(),
        );
        m.insert(
            "project".into(),
            doc_project(&data).map_or(Value::Null, Value::from),
        );
        m.insert("tags".into(), safe_json(data.get("tags")));
        m.insert("created_at".into(), opt("created_at"));
        m.insert("updated_at".into(), opt("updated_at"));
        m.insert(
            "effort_score".into(),
            data.get("effort_score")
                .and_then(|v| v.trim().parse::<f64>().ok())
                .map_or(Value::Null, |v| (v as i64).into()),
        );
        m.insert("outcome".into(), opt("outcome"));
        m.insert("experience_weight".into(), opt("experience_weight"));
        m.insert(
            "abandoned_approaches".into(),
            safe_json(data.get("abandoned_approaches")),
        );
        m.insert("breakthrough".into(), opt("breakthrough"));
        m.insert("lesson".into(), opt("lesson"));
        m.insert("gotchas".into(), opt("gotchas"));
        m.insert("deprioritised_reason".into(), opt("deprioritised_reason"));
        m.insert(
            "reinstate_hints".into(),
            safe_json(data.get("reinstate_hints")),
        );
        m.insert(
            "contradictions".into(),
            safe_json(data.get("contradictions")),
        );
        m.insert(
            "recall_count".into(),
            data.get("recall_count")
                .and_then(|c| c.trim().parse::<i64>().ok())
                .unwrap_or(0)
                .into(),
        );
        m.insert("last_recalled".into(), opt("last_recalled"));
        for name in ["source_url", "feed_name"] {
            if let Some(v) = data.get(name).filter(|v| !v.is_empty()) {
                m.insert(name.into(), v.as_str().into());
            }
        }
        // v7 identity, as the conformance spec asks explain_memory to expose.
        for name in ["origin_id", "content_hash", "epoch", "classification"] {
            if let Some(v) = data.get(name).filter(|v| !v.is_empty()) {
                m.insert(name.into(), v.as_str().into());
            }
        }
        if key.starts_with("mem:") {
            let ns = key.split(':').nth(1).unwrap_or("");
            m.extend(classification_fields(&data, ns, Some(key)));
        }
        Ok(compact(m))
    }

    /// Rebuild the in-memory vector matrix from the database, then embed
    /// any memory that has no vector: what a restore cut short, or an
    /// `import --no-embed`, leaves unsearchable, and nothing else would
    /// ever go back for. There is no separate index to fall out of step any
    /// more, so nothing is ever "phantom"; the shape of 6.x's report is
    /// kept, with `embedded` and `unembeddable` added.
    pub fn reindex(&self, namespace: Option<&str>) -> Result<Value> {
        let namespace = namespace.filter(|n| !n.is_empty());
        if let Some(ns) = namespace
            && !REINDEX_ORDER.contains(&ns)
        {
            let mut allowed = REINDEX_ORDER.to_vec();
            allowed.sort_unstable();
            return Err(invalid(format!(
                "Invalid namespace '{ns}'. Must be one of: {}",
                allowed.join(", ")
            )));
        }
        let report = self.store.reload_vectors()?;
        let targets: Vec<&str> = namespace.map_or_else(|| REINDEX_ORDER.to_vec(), |n| vec![n]);
        let vectorless: Vec<String> = self
            .store
            .memories_without_vectors()?
            .into_iter()
            .filter(|key| {
                targets
                    .iter()
                    .any(|ns| key.starts_with(&format!("mem:{ns}:")))
            })
            .collect();
        let (embedded, unembeddable) =
            self.store
                .embed_memories(&vectorless, self.embedder.as_ref(), &mut |_, _| {})?;
        if embedded > 0 {
            tracing::info!(
                embedded,
                unembeddable,
                "reindex embedded vectorless memories"
            );
        }
        let counts = self.store.count_all_records()?;
        let results: Vec<Value> = targets
            .iter()
            .filter_map(|ns| ns.parse::<Namespace>().ok())
            .map(|ns| {
                let (before, _) = report[&ns];
                json!({
                    "namespace": ns.as_str(),
                    "before_num_docs": before,
                    "after_num_docs": self.store.vector_count(ns),
                    "actual_records": counts[&ns],
                    "removed_phantoms": 0,
                })
            })
            .collect();
        Ok(json!({
            "status": "ok",
            "reindexed": results,
            "total_phantoms_removed": 0,
            "embedded": embedded,
            "unembeddable": unembeddable,
        }))
    }
}
