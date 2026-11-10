//! Contradiction scanning and auto-maintenance (`tools/contradiction.py`,
//! `memory/maintenance.py`).

use omnimem_core::Namespace;
use omnimem_store::{AuditLevel, Fields, Op, SearchFilter, op as audit_op};
use serde_json::{Value, json};
use tracing::{debug, error, info};

use crate::contradiction::signals_conflict;
use crate::error::invalid;
use crate::lifecycle::MemoryState;
use crate::pyfmt::{now_secs, now_str, py_json, take_chars};
use crate::resolution::row_resolved_with;
use crate::skills::py_str;
use crate::tools::validate_namespace;
use crate::{Engine, Result};

const SCAN_CAP: usize = 200;
const COMPARISON_CAP: usize = 2000;
const SIMILARITY_THRESHOLD: f64 = 0.5;
const RESULTS_CAP: usize = 10;

/// A model-reported confidence as a number in 0..=1; anything else is 0.
fn clamped_confidence(raw: Option<&Value>) -> f64 {
    raw.and_then(Value::as_f64)
        .filter(|c| c.is_finite())
        .map_or(0.0, |c| c.clamp(0.0, 1.0))
}

fn doc_project(fields: &Fields) -> Option<&str> {
    fields
        .get("project")
        .filter(|p| !p.is_empty())
        .or_else(|| fields.get("project_name").filter(|p| !p.is_empty()))
        .map(String::as_str)
}

fn dot(a: &[f32], b: &[f32]) -> f64 {
    f64::from(a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>())
}

impl Engine {
    /// Stored vectors for `entries`, embedding content only where one is missing.
    fn vectors_for(&self, entries: &[(String, Fields)]) -> Result<Vec<Vec<f32>>> {
        let keys: Vec<String> = entries.iter().map(|(k, _)| k.clone()).collect();
        let mut vectors = self.store.get_vectors_multi(&keys);
        let missing: Vec<usize> = (0..vectors.len())
            .filter(|i| vectors[*i].is_none())
            .collect();
        if !missing.is_empty() {
            let texts: Vec<&str> = missing
                .iter()
                .map(|i| entries[*i].1.get("content").map_or("", String::as_str))
                .collect();
            for (i, v) in missing.iter().zip(self.embed_many(&texts)?) {
                vectors[*i] = Some(v);
            }
        }
        Ok(vectors.into_iter().map(Option::unwrap_or_default).collect())
    }

    /// Cross-reference two memories as contradicting each other. A link
    /// the model confirmed (`verified`) also disputes both sides (#23); a
    /// heuristic one only warns, since shared words and a negation aren't
    /// evidence enough to rank a memory down.
    fn link_contradiction(
        &self,
        key_a: &str,
        key_b: &str,
        explanation: &str,
        verified: bool,
    ) -> Result<bool> {
        let now = now_str();
        let mut linked = false;
        for (src, other) in [(key_a, key_b), (key_b, key_a)] {
            let Some(data) = self.store.get(src)? else {
                continue;
            };
            let mut existing: Vec<Value> = data
                .get("contradictions")
                .and_then(|c| serde_json::from_str(c).ok())
                .unwrap_or_default();
            if existing
                .iter()
                .any(|c| c.get("key").and_then(Value::as_str) == Some(other))
            {
                continue;
            }
            let mut link = json!({"key": other, "explanation": explanation, "detected_at": now});
            if verified {
                link["verified"] = true.into();
            }
            existing.push(link);
            let mut updates = Fields::from([(
                "contradictions".to_owned(),
                py_json(&Value::Array(existing)),
            )]);
            // A confirmed link changes what the memory is worth, so it is an
            // update. A heuristic one is a note in the margin: it leaves
            // `updated_at` alone, as a trust change does, so the maintenance
            // pass can't push a memory up Recent activity or make a skill's
            // sources look changed by linking it.
            if verified {
                updates.insert("updated_at".to_owned(), now.clone());
                crate::trust::dispute(&data, &mut updates);
            }
            // The explanation is model output that may quote either memory,
            // so only `full` keeps it.
            let mut detail = json!({"linked_to": other});
            if self.store.audit_level() == AuditLevel::Full {
                detail["explanation"] = json!(explanation);
            }
            let _audit = audit_op(Op::new("contradiction_link").detail(detail));
            self.store.set_fields(src, &updates)?;
            linked = true;
        }
        if linked {
            info!(key_a, key_b, "linked contradiction");
        }
        Ok(linked)
    }

    /// Tier 1 heuristic scan, similarity-gated. With `use_api`, tier 2 asks
    /// Claude about each heuristic match and keeps only the confirmed ones;
    /// with no API key that confirms nothing, as in 6.x.
    pub fn check_contradictions(
        &self,
        query: Option<&str>,
        namespace: &str,
        project_filter: Option<&str>,
        use_api: bool,
    ) -> Result<Value> {
        // Compiled skills are build output and never contradict each other in
        // a way worth linking, so only the writable namespaces are scanned.
        validate_namespace(namespace)?;
        let ns: Namespace = namespace
            .parse()
            .map_err(|_| invalid(format!("Invalid namespace: {namespace}")))?;
        let project_filter = project_filter.filter(|p| !p.is_empty());
        let in_scope = |f: &Fields| {
            !matches!(
                f.get("state").map(String::as_str),
                Some("archived" | "deleted")
            ) && project_filter.is_none_or(|p| doc_project(f) == Some(p))
        };

        let entries: Vec<(String, Fields)> = if let Some(q) = query.filter(|q| !q.is_empty()) {
            let vector = self.embed_query(q)?;
            self.store
                .search(ns, &vector, 20, &SearchFilter::default(), None)?
                .into_iter()
                .filter(|h| in_scope(&h.fields))
                .map(|h| (h.key, h.fields))
                .collect()
        } else {
            // Keys come back in ULID (creation) order. Filter by scope first,
            // then keep the newest SCAN_CAP: a new memory is the one most
            // likely to contradict something, and capping before the project
            // filter would compare only the store's oldest slice. The project,
            // when there is one, is cut in SQL on its indexes, which reads
            // the project as `doc_project` does; `in_scope` still decides.
            let filter = omnimem_store::MemoryFilter {
                project: project_filter.map(str::to_owned),
                ..omnimem_store::MemoryFilter::default()
            };
            let mut rows = self.store.list_memories(
                ns,
                &filter,
                &[
                    "state",
                    "project",
                    "project_name",
                    "content",
                    "contradictions",
                ],
            )?;
            rows.reverse();
            rows.into_iter()
                .filter(|(_, r)| in_scope(r))
                .take(SCAN_CAP)
                .collect()
        };
        if entries.is_empty() {
            return Ok(json!({"contradictions": []}));
        }
        let vectors = self.vectors_for(&entries)?;

        let mut found = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut comparisons = 0;
        'outer: for i in 0..entries.len() {
            let (key_a, doc_a) = &entries[i];
            let content_a = doc_a.get("content").map_or("", String::as_str);
            if content_a.is_empty() {
                continue;
            }
            for j in (i + 1)..entries.len() {
                if comparisons >= COMPARISON_CAP || found.len() >= RESULTS_CAP {
                    break 'outer;
                }
                let (key_b, doc_b) = &entries[j];
                let content_b = doc_b.get("content").map_or("", String::as_str);
                if content_b.is_empty() {
                    continue;
                }
                // A person has settled this pair, whichever row says so.
                if row_resolved_with(doc_a, key_b) || row_resolved_with(doc_b, key_a) {
                    continue;
                }
                let pair = if key_a < key_b {
                    format!("{key_a}:{key_b}")
                } else {
                    format!("{key_b}:{key_a}")
                };
                if !seen.insert(pair) {
                    continue;
                }
                comparisons += 1;
                if dot(&vectors[i], &vectors[j]) < SIMILARITY_THRESHOLD
                    || !signals_conflict(content_a, content_b)
                {
                    continue;
                }
                let mut entry = json!({
                    "key_a": key_a,
                    "key_b": key_b,
                    "content_a": take_chars(content_a, 80),
                    "content_b": take_chars(content_b, 80),
                    "method": "heuristic",
                });
                let mut explanation = omnimem_store::HEURISTIC_LINK_EXPLANATION.to_owned();
                if use_api {
                    let verdict = self.check_contradiction_api(content_a, content_b);
                    // The verdict is model output: only a JSON `true` confirms,
                    // not a truthy string such as "false" or "maybe".
                    if verdict.get("is_contradiction") != Some(&Value::Bool(true)) {
                        continue;
                    }
                    let stated = verdict
                        .get("explanation")
                        .cloned()
                        .unwrap_or_else(|| json!(""));
                    explanation = stated
                        .as_str()
                        .map_or_else(|| py_str(&stated), str::to_owned);
                    entry["method"] = "api_confirmed".into();
                    entry["confidence"] = json!(clamped_confidence(verdict.get("confidence")));
                    entry["explanation"] = stated;
                }
                self.link_contradiction(key_a, key_b, &explanation, use_api)?;
                found.push(entry);
            }
            if comparisons >= COMPARISON_CAP || found.len() >= RESULTS_CAP {
                break;
            }
        }
        Ok(json!({"contradictions": found}))
    }

    /// Archive RSS articles whose `expires_at` has passed.
    fn expire_knowledge_items(&self) -> Result<Vec<String>> {
        let now = now_secs();
        let keys = self.store.scan_prefix("mem:knowledge:")?;
        let rows = self
            .store
            .get_fields_multi(&keys, &["state", "feed_name", "expires_at"])?;
        let mut expired = Vec::new();
        for (key, row) in keys.iter().zip(rows) {
            let Some(data) = row else { continue };
            if data.get("state").map(String::as_str) != Some("active")
                || data
                    .get("feed_name")
                    .is_none_or(std::string::String::is_empty)
            {
                continue;
            }
            let Some(at) = data
                .get("expires_at")
                .filter(|e| !e.is_empty())
                .and_then(|e| e.parse::<f64>().ok())
            else {
                continue;
            };
            if at <= now {
                match self.transition(
                    key,
                    MemoryState::Archived,
                    Some("auto-maintenance: knowledge item expired"),
                ) {
                    Ok(_) => expired.push(key.clone()),
                    Err(e) => debug!(key, error = %e, "skipped archiving an expired article"),
                }
            }
        }
        Ok(expired)
    }

    /// Dedup archive, contradiction scan and article expiry for a project.
    pub(crate) fn run_maintenance(&self, project: &str) -> Value {
        // Whoever's briefing set this off, the pass is maintenance's own.
        let _audit = omnimem_store::enter(omnimem_store::Actor::Maintenance, "");
        let ran_at = now_str();
        if let Err(e) = self.prune_audit() {
            error!(error = %e, "could not prune the audit log");
        }
        let mut archived = Vec::new();
        match self.find_all_duplicates(Namespace::Episodic, None, Some(project)) {
            Ok(clusters) => {
                for cluster in clusters {
                    let mut members: Vec<Value> =
                        cluster["memories"].as_array().cloned().unwrap_or_default();
                    members.sort_by(|a, b| {
                        let t = |m: &Value| {
                            m["created_at"]
                                .as_str()
                                .and_then(|c| c.parse::<f64>().ok())
                                .unwrap_or(0.0)
                        };
                        t(a).partial_cmp(&t(b)).unwrap_or(std::cmp::Ordering::Equal)
                    });
                    let Some(newest) = members
                        .last()
                        .and_then(|m| m["key"].as_str())
                        .map(str::to_owned)
                    else {
                        continue;
                    };
                    for m in &members[..members.len() - 1] {
                        let key = m["key"].as_str().unwrap_or("");
                        let reason = format!("auto-maintenance: duplicate of {newest}");
                        if self
                            .transition(key, MemoryState::Archived, Some(&reason))
                            .is_ok()
                        {
                            archived.push(key.to_owned());
                        }
                    }
                }
            }
            Err(e) => error!(project, error = %e, "maintenance dedup phase failed"),
        }

        if let Err(e) = self.settle_dangling_links() {
            error!(error = %e, "could not settle links to memories that no longer exist");
        }
        let mut contradictions = Vec::new();
        let mut scan = || -> Result<()> {
            // Newest first, so a busy project's latest memories are compared.
            // State and project are cut in SQL on their indexes rather than
            // by reading every episodic memory's content; the store's cut is a
            // superset (a missing state passes), so the test below decides.
            let filter = omnimem_store::MemoryFilter {
                states: vec!["active".to_owned()],
                project: Some(project.to_owned()),
                ..omnimem_store::MemoryFilter::default()
            };
            let mut rows = self.store.list_memories(
                Namespace::Episodic,
                &filter,
                &[
                    "state",
                    "project",
                    "project_name",
                    "content",
                    "contradictions",
                ],
            )?;
            rows.reverse();
            let entries: Vec<(String, Fields)> = rows
                .into_iter()
                .filter(|(_, r)| {
                    r.get("state").map(String::as_str) == Some("active")
                        && doc_project(r) == Some(project)
                })
                .take(SCAN_CAP)
                .collect();
            if entries.len() < 2 {
                return Ok(());
            }
            let vectors = self.vectors_for(&entries)?;
            let mut comparisons = 0;
            'outer: for i in 0..entries.len() {
                for j in (i + 1)..entries.len() {
                    if comparisons >= COMPARISON_CAP || contradictions.len() >= RESULTS_CAP {
                        break 'outer;
                    }
                    if row_resolved_with(&entries[i].1, &entries[j].0)
                        || row_resolved_with(&entries[j].1, &entries[i].0)
                    {
                        continue;
                    }
                    comparisons += 1;
                    if dot(&vectors[i], &vectors[j]) < SIMILARITY_THRESHOLD {
                        continue;
                    }
                    let a = entries[i].1.get("content").map_or("", String::as_str);
                    let b = entries[j].1.get("content").map_or("", String::as_str);
                    if signals_conflict(a, b) {
                        // Linked as the tool's heuristic tier links them, so
                        // the pair the summary counts is one the panel lists
                        // and a person can settle; 6.x only counted it, and
                        // found it again on every pass.
                        // Found means new: a pair linked on an earlier pass
                        // is still listed, but not counted again.
                        if !self.link_contradiction(
                            &entries[i].0,
                            &entries[j].0,
                            omnimem_store::HEURISTIC_LINK_EXPLANATION,
                            false,
                        )? {
                            continue;
                        }
                        contradictions.push(json!({
                            "key_a": entries[i].0,
                            "key_b": entries[j].0,
                            "content_a": take_chars(a, 80),
                            "content_b": take_chars(b, 80),
                        }));
                    }
                }
            }
            Ok(())
        };
        if let Err(e) = scan() {
            error!(project, error = %e, "maintenance contradiction phase failed");
        }

        let expired = self.expire_knowledge_items().unwrap_or_else(|e| {
            error!(error = %e, "maintenance knowledge expiry phase failed");
            Vec::new()
        });
        json!({
            "project": project,
            "ran_at": ran_at,
            "duplicates_archived": archived,
            "contradictions_found": contradictions,
            "knowledge_expired": expired,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confidence_is_a_clamped_number() {
        assert_eq!(clamped_confidence(Some(&json!(0.7))), 0.7);
        assert_eq!(clamped_confidence(Some(&json!(3))), 1.0);
        assert_eq!(clamped_confidence(Some(&json!(-1.5))), 0.0);
        assert_eq!(clamped_confidence(Some(&json!("high"))), 0.0);
        assert_eq!(clamped_confidence(None), 0.0);
    }
}
