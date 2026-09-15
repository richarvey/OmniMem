//! Classifying memories and cascading to their derived facts
//! (`memory/lineage.py`, `tools/licence.py`, `tools/provenance.py`).

use omnimem_store::Fields;
use serde_json::{Map, Value, json};
use tracing::info;

use crate::classification::{
    is_classifiable_key, resolve_licence, resolve_provenance, validate_licence_note,
};
use crate::pyfmt::compact;
use crate::{Engine, EngineError, Result};

const MAX_KEYS_PER_CALL: usize = 200;

pub(crate) struct Stamped {
    pub classified: Vec<String>,
    pub cascaded: Vec<String>,
    pub not_found: Vec<String>,
}

/// (valid, skipped) or a ready-to-return error object.
fn partition_keys(
    keys: &[String],
    field: &str,
) -> std::result::Result<(Vec<String>, Vec<String>), Value> {
    if keys.is_empty() {
        return Err(json!({"error": "keys is required"}));
    }
    if keys.len() > MAX_KEYS_PER_CALL {
        return Err(
            json!({"error": format!("Too many keys ({}); max {MAX_KEYS_PER_CALL} per call", keys.len())}),
        );
    }
    let skills: Vec<&String> = keys
        .iter()
        .filter(|k| k.starts_with("mem:skill:"))
        .collect();
    if !skills.is_empty() {
        return Err(json!({
            "error": format!("Compiled skills carry no {field} — classify the source memories in the skill's manifest instead"),
            "skill_keys": skills,
        }));
    }
    let (valid, skipped): (Vec<String>, Vec<String>) =
        keys.iter().cloned().partition(|k| is_classifiable_key(k));
    if valid.is_empty() {
        return Err(json!({
            "error": "No valid memory keys given (mem:episodic:, mem:project:, mem:knowledge: or mem:preference:)",
        }));
    }
    Ok((valid, skipped))
}

impl Engine {
    /// Write `fields` onto `keys`, their document siblings and every fact
    /// derived from them. Never touches `updated_at`.
    pub(crate) fn stamp_lineage(&self, keys: &[String], fields: &Fields) -> Result<Stamped> {
        let rows = self
            .store
            .get_fields_multi(keys, &["created_at", "state", "doc_id"])?;
        let mut found: Vec<String> = Vec::new();
        let mut not_found = Vec::new();
        let mut doc_ids: Vec<(String, String)> = Vec::new();
        for (key, row) in keys.iter().zip(&rows) {
            match row {
                Some(r) => {
                    found.push(key.clone());
                    if let Some(doc) = r.get("doc_id").filter(|d| !d.is_empty()) {
                        let ns = key.split(':').nth(1).unwrap_or("").to_owned();
                        if !doc_ids.contains(&(ns.clone(), doc.clone())) {
                            doc_ids.push((ns, doc.clone()));
                        }
                    }
                }
                None => not_found.push(key.clone()),
            }
        }
        if found.is_empty() {
            return Ok(Stamped {
                classified: Vec::new(),
                cascaded: Vec::new(),
                not_found,
            });
        }
        let mut namespaces: Vec<&str> = doc_ids.iter().map(|(ns, _)| ns.as_str()).collect();
        namespaces.dedup();
        for ns in namespaces {
            let sibling_keys = self.store.scan_prefix(&format!("mem:{ns}:"))?;
            let sibling_rows = self.store.get_fields_multi(&sibling_keys, &["doc_id"])?;
            for (k, r) in sibling_keys.into_iter().zip(sibling_rows) {
                let Some(doc) = r.and_then(|r| r.get("doc_id").cloned()) else {
                    continue;
                };
                if doc_ids.iter().any(|(n, d)| n == ns && *d == doc) && !found.contains(&k) {
                    found.push(k);
                }
            }
        }
        let mut sources: Vec<String> = found.clone();
        sources.extend(doc_ids.iter().map(|(_, d)| d.clone()));

        let mut cascaded = Vec::new();
        if found.iter().any(|k| !k.starts_with("mem:knowledge:")) {
            for ns in ["knowledge", "preference"] {
                let fact_keys = self.store.scan_prefix(&format!("mem:{ns}:"))?;
                let fact_rows = self
                    .store
                    .get_fields_multi(&fact_keys, &["enriched_from", "source_doc_id"])?;
                for (k, r) in fact_keys.into_iter().zip(fact_rows) {
                    let Some(r) = r else { continue };
                    if sources.contains(&k) {
                        continue;
                    }
                    let points_at = |field: &str| r.get(field).is_some_and(|v| sources.contains(v));
                    if points_at("enriched_from") || points_at("source_doc_id") {
                        cascaded.push(k);
                    }
                }
            }
        }
        let all: Vec<String> = found.iter().chain(&cascaded).cloned().collect();
        self.store.set_fields_multi(&all, fields)?;
        Ok(Stamped {
            classified: found,
            cascaded,
            not_found,
        })
    }

    pub fn set_licence(
        &self,
        licence: &str,
        keys: Option<&[String]>,
        feed_name: Option<&str>,
        note: Option<&str>,
    ) -> Result<Value> {
        let keys = keys.filter(|k| !k.is_empty());
        let feed_name = feed_name.filter(|f| !f.is_empty());
        if keys.is_some() == feed_name.is_some() {
            return Ok(json!({"error": "Give either keys or feed_name, not both and not neither"}));
        }
        let (class, derived) = match resolve_licence(licence) {
            Ok(r) => r,
            Err(e) => return Ok(json!({"error": e.to_string()})),
        };
        let note = match validate_licence_note(note) {
            Ok(n) => n.or_else(|| derived.map(str::to_owned)),
            Err(EngineError::Invalid(e)) => return Ok(json!({"error": e})),
            Err(e) => return Err(e),
        };
        let fields = Fields::from([
            ("licence".to_owned(), class.to_owned()),
            ("licence_note".to_owned(), note.clone().unwrap_or_default()),
        ]);

        if let Some(feed) = feed_name {
            let all = self.store.scan_prefix("mem:knowledge:")?;
            let rows = self.store.get_fields_multi(&all, &["feed_name"])?;
            let targets: Vec<String> = all
                .into_iter()
                .zip(rows)
                .filter(|(_, r)| {
                    r.as_ref()
                        .and_then(|r| r.get("feed_name"))
                        .map(String::as_str)
                        == Some(feed)
                })
                .map(|(k, _)| k)
                .collect();
            if targets.is_empty() {
                return Ok(
                    json!({"error": format!("No knowledge articles found for feed '{feed}'")}),
                );
            }
            let outcome = self.stamp_lineage(&targets, &fields)?;
            info!(
                feed,
                count = outcome.classified.len(),
                class,
                "classified feed articles"
            );
            let mut m = Map::new();
            m.insert("feed_name".into(), feed.into());
            m.insert("licence".into(), class.into());
            m.insert("licence_note".into(), note.map_or(Value::Null, Value::from));
            m.insert("classified".into(), outcome.classified.len().into());
            m.insert(
                "note".into(),
                "Existing articles only. Set `licence:` on this feed in feeds.yml or the web UI feed editor so future articles arrive classified.".into(),
            );
            return Ok(compact(m));
        }

        let (valid, skipped) = match partition_keys(keys.unwrap_or_default(), "licence") {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let outcome = self.stamp_lineage(&valid, &fields)?;
        let mut m = Map::new();
        m.insert("licence".into(), class.into());
        m.insert("licence_note".into(), note.map_or(Value::Null, Value::from));
        m.insert("classified".into(), outcome.classified.len().into());
        m.insert("keys".into(), json!(outcome.classified));
        m.insert("cascaded_facts".into(), json!(outcome.cascaded));
        m.insert("not_found".into(), json!(outcome.not_found));
        m.insert("skipped".into(), json!(skipped));
        Ok(compact(m))
    }

    pub fn set_provenance(&self, provenance: &str, keys: &[String]) -> Result<Value> {
        let class = match resolve_provenance(provenance) {
            Ok(c) => c,
            Err(e) => return Ok(json!({"error": e.to_string()})),
        };
        let (valid, skipped) = match partition_keys(keys, "provenance") {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let fields = Fields::from([("provenance".to_owned(), class.to_owned())]);
        let outcome = self.stamp_lineage(&valid, &fields)?;
        let mut m = Map::new();
        m.insert("provenance".into(), class.into());
        m.insert("classified".into(), outcome.classified.len().into());
        m.insert("keys".into(), json!(outcome.classified));
        m.insert("cascaded_facts".into(), json!(outcome.cascaded));
        m.insert("not_found".into(), json!(outcome.not_found));
        m.insert("skipped".into(), json!(skipped));
        Ok(compact(m))
    }
}
