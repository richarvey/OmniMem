//! Skill tools and their briefing surfaces (`tools/skills.py`), plus
//! `promote_knowledge` (`tools/knowledge.py`).

use std::collections::HashSet;

use omnimem_core::Namespace;
use omnimem_store::{Fields, SearchFilter};
use serde_json::{Map, Value, json};
use tracing::{info, warn};

use crate::contradiction::has_negation_pair;
use crate::domains::{is_valid_domain, normalise_domain, resolve_domain};
use crate::error::invalid;
use crate::pyfmt::{compact, now_secs, now_str, py_json, round_to, take_chars};
use crate::skills::{
    GENERATED_SKILL_PREFIX, INVALID_DOMAIN, SKILL_KEY_PREFIX, generated_skill_key, lesson_bearing,
    parse_skill_domains, parse_string_list, py_str, py_truthy, safe_float,
    validate_reference_rules,
};
use crate::{Engine, Result};

/// Above this a semantic match is as trustworthy as an exact domain hit.
const SKILL_HIGH_CONFIDENCE: f64 = 0.45;

fn state_active(row: &Fields) -> bool {
    row.get("state").is_none_or(|s| s == "active")
}

fn text(row: &Fields, name: &str) -> String {
    row.get(name).cloned().unwrap_or_default()
}

fn active_filter() -> SearchFilter {
    SearchFilter {
        states: vec!["active".to_owned()],
        ..SearchFilter::default()
    }
}

fn skill_entry(key: &str, row: &Fields, score: f64, matched: &str) -> Value {
    let mut m = Map::new();
    m.insert("skill_id".into(), key.into());
    m.insert("name".into(), text(row, "name").into());
    m.insert("description".into(), text(row, "description").into());
    m.insert("domain".into(), text(row, "domain").into());
    m.insert(
        "generated".into(),
        (row.get("generated").map(String::as_str) == Some("true")).into(),
    );
    m.insert("score".into(), json!(round_to(score, 4)));
    m.insert("match".into(), matched.into());
    // A skill loads whole, so the caller needs to tell a good winner from
    // the least-bad of a weak field (#31).
    let confidence = if matched == "domain" || score >= SKILL_HIGH_CONFIDENCE {
        "high"
    } else {
        "low"
    };
    m.insert("confidence".into(), confidence.into());
    compact(m)
}

impl Engine {
    pub fn find_skills(&self, query_or_domain: &str) -> Result<Value> {
        if query_or_domain.trim().is_empty() {
            return Err(invalid("query_or_domain cannot be empty"));
        }
        let (canonical, _) = resolve_domain(query_or_domain);
        let all_keys = self.store.scan_prefix(SKILL_KEY_PREFIX)?;
        if all_keys.is_empty() {
            return Ok(json!({
                "skills": [],
                "note": "No skills stored yet. compile_skill(domain=...) creates one.",
            }));
        }
        let rows = self.store.get_fields_multi(
            &all_keys,
            &[
                "name",
                "description",
                "domain",
                "state",
                "generated",
                "compiled_at",
            ],
        )?;

        // Exact domain hits lead; authored before generated on a tie.
        let mut domain_hits: Vec<(&String, &Fields)> = all_keys
            .iter()
            .zip(&rows)
            .filter_map(|(k, r)| r.as_ref().map(|r| (k, r)))
            .filter(|(_, r)| state_active(r) && r.get("domain") == Some(&canonical))
            .collect();
        domain_hits.sort_by(|a, b| {
            let generated = |r: &Fields| r.get("generated").map(String::as_str) == Some("true");
            generated(a.1)
                .cmp(&generated(b.1))
                .then_with(|| a.0.cmp(b.0))
        });
        let mut results: Vec<Value> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        for (key, row) in &domain_hits {
            results.push(skill_entry(key, row, 1.0, "domain"));
            seen.insert(key.as_str());
        }

        let query_vector = self.embed(query_or_domain)?;
        let hits =
            self.store
                .search(Namespace::Skill, &query_vector, 10, &active_filter(), None)?;
        let mut semantic: Vec<(f64, bool, &str, &Fields)> = hits
            .iter()
            .filter(|h| !seen.contains(h.key.as_str()) && state_active(&h.fields))
            .map(|h| {
                (
                    f64::from(h.similarity()).max(0.0),
                    h.fields.get("generated").map(String::as_str) == Some("true"),
                    h.key.as_str(),
                    &h.fields,
                )
            })
            .collect();
        semantic.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.1.cmp(&b.1))
                .then_with(|| a.2.cmp(b.2))
        });
        let floor = self.config.skill_min_score;
        let mut below_floor = 0;
        for (similarity, _, key, row) in semantic {
            if similarity < floor {
                below_floor += 1;
                continue;
            }
            results.push(skill_entry(key, row, similarity, "semantic"));
        }

        if results.is_empty() {
            let note = if below_floor > 0 {
                format!(
                    "No skill cleared the relevance floor ({floor:.2}); {below_floor} scored below it. \
                     Nothing stored covers this work — compile_skill(domain=...) creates a skill for it."
                )
            } else {
                "No skill matched. Known skills exist for other domains — call find_skills with a \
                 broader query or compile_skill to create one."
                    .to_owned()
            };
            // Not compacted: the empty list is the answer.
            return Ok(json!({"skills": [], "note": note}));
        }
        results.truncate(10);
        let all_low = results.iter().all(|e| e["confidence"] == "low");
        let mut out = Map::new();
        out.insert("skills".into(), results.into());
        if all_low {
            out.insert(
                "note".into(),
                "Best match is a weak one — read the description before loading it, and treat 'no \
                 relevant skill' as a valid answer."
                    .into(),
            );
        }
        Ok(Value::Object(out))
    }

    pub fn get_skill(&self, skill_id: &str) -> Result<Value> {
        let skill_id = skill_id.trim();
        if skill_id.is_empty() {
            return Err(invalid("skill_id cannot be empty"));
        }
        let candidates: Vec<String> = if skill_id.starts_with(SKILL_KEY_PREFIX) {
            vec![skill_id.to_owned()]
        } else {
            let short = normalise_domain(skill_id);
            vec![
                format!("{SKILL_KEY_PREFIX}{short}"),
                format!("{GENERATED_SKILL_PREFIX}{short}"),
                generated_skill_key(&resolve_domain(&short).0, &self.config.skill_user),
            ]
        };

        for key in &candidates {
            let Some(data) = self.store.get(key).ok().flatten() else {
                continue;
            };
            if data.get("body").is_none_or(|b| b.is_empty()) {
                continue;
            }
            // The same counters recall keeps, so telemetry sees skill loads.
            if let Err(e) = self
                .store
                .bump_recall_counts(std::slice::from_ref(key), &now_str())
            {
                warn!(key, error = %e, "could not bump skill counters");
            }
            let manifest = data
                .get("source_manifest")
                .and_then(|m| serde_json::from_str::<Value>(m).ok())
                .unwrap_or_else(|| json!([]));
            let optional = |name: &str| data.get(name).map_or(Value::Null, |v| v.as_str().into());
            let mut m = Map::new();
            m.insert("status".into(), "found".into());
            m.insert("skill_id".into(), key.as_str().into());
            m.insert("name".into(), text(&data, "name").into());
            m.insert("description".into(), text(&data, "description").into());
            m.insert("domain".into(), text(&data, "domain").into());
            m.insert(
                "generated".into(),
                (data.get("generated").map(String::as_str) == Some("true")).into(),
            );
            m.insert("contract_version".into(), optional("contract_version"));
            m.insert("compiled_at".into(), optional("compiled_at"));
            m.insert(
                "state".into(),
                data.get("state").map_or("active", String::as_str).into(),
            );
            m.insert("source_manifest".into(), manifest);
            m.insert("body".into(), text(&data, "body").into());
            return Ok(compact(m));
        }

        let all_keys = self.store.scan_prefix(SKILL_KEY_PREFIX)?;
        let rows = self
            .store
            .get_fields_multi(&all_keys, &["name", "domain"])?;
        let available: Vec<Value> = all_keys
            .iter()
            .zip(rows)
            .take(10)
            .map(|(k, r)| {
                let r = r.unwrap_or_default();
                json!({"skill_id": k, "name": text(&r, "name"), "domain": text(&r, "domain")})
            })
            .collect();
        let mut m = Map::new();
        m.insert("status".into(), "not_found".into());
        m.insert("tried".into(), json!(candidates));
        m.insert("available".into(), available.into());
        Ok(compact(m))
    }

    pub fn bless(&self, memory_key: &str) -> Result<Value> {
        if !memory_key.starts_with("mem:episodic:") {
            return Err(invalid(
                "bless() takes an episodic memory key (mem:episodic:...) — skills compile from the \
                 episodic experience pool.",
            ));
        }
        let Some(data) = self.store.get(memory_key)? else {
            return Ok(json!({"status": "not_found", "key": memory_key}));
        };
        let already = data.get("blessed").map(String::as_str) == Some("1");
        if !already {
            self.store.set_fields(
                memory_key,
                &Fields::from([
                    ("blessed".to_owned(), "1".to_owned()),
                    ("blessed_at".to_owned(), now_str()),
                ]),
            )?;
            info!(memory_key, "blessed memory as skill-eligible");
        }
        let tags: Vec<String> = data
            .get("tags")
            .and_then(|t| serde_json::from_str::<Value>(t).ok())
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter(|t| py_truthy(t))
            .map(|t| py_str(t).to_lowercase())
            .collect();
        let mut m = Map::new();
        m.insert(
            "status".into(),
            if already {
                "already_blessed"
            } else {
                "blessed"
            }
            .into(),
        );
        m.insert("key".into(), memory_key.into());
        m.insert("domains".into(), json!(tags));
        m.insert(
            "note".into(),
            "Its lessons now clear the reinforcement gate on the next compile_skill() for its \
             tagged domains. Compiling still requires the propose-and-accept flow."
                .into(),
        );
        Ok(compact(m))
    }

    /// Keep a knowledge item from expiring, and optionally make it
    /// skill-eligible for a domain (with extracted rules).
    pub fn promote_knowledge(
        &self,
        key: &str,
        domain: Option<&str>,
        demote: bool,
        rules: Option<&Value>,
    ) -> Result<Value> {
        if !key.starts_with("mem:knowledge:") {
            return Ok(json!({"error": format!("Key must be in the knowledge namespace: {key}")}));
        }
        let domain_given = domain.is_some_and(|d| !d.is_empty());
        if demote && !domain_given {
            return Ok(json!({"error": "demote requires a domain to remove"}));
        }
        if rules.is_some() && (!domain_given || demote) {
            return Ok(json!({"error": "rules only apply when promoting to a domain"}));
        }
        let Some(data) = self.store.get(key)? else {
            return Ok(json!({"error": format!("Key not found: {key}")}));
        };
        if data.get("state").map(String::as_str) == Some("archived") {
            return Ok(json!({"error": format!("Cannot promote archived item: {key}")}));
        }
        let Some(domain) = domain else {
            self.store.set_field(key, "expires_at", "")?;
            return Ok(json!({"key": key, "promoted": true}));
        };
        let (canonical, _) = resolve_domain(domain);
        if !is_valid_domain(&canonical) {
            return Ok(json!({"error": INVALID_DOMAIN}));
        }

        let now = now_str();
        let mut domains = parse_skill_domains(data.get("skill_domains"));
        if demote {
            let Some(position) = domains.iter().position(|d| *d == canonical) else {
                return Ok(
                    json!({"error": format!("{key} is not promoted to domain '{canonical}'")}),
                );
            };
            domains.remove(position);
            self.store.set_fields(
                key,
                &Fields::from([
                    ("skill_domains".to_owned(), py_json(&json!(domains))),
                    ("updated_at".to_owned(), now),
                ]),
            )?;
            let mut m = Map::new();
            m.insert("key".into(), key.into());
            m.insert("demoted_from".into(), canonical.as_str().into());
            m.insert("skill_domains".into(), json!(domains));
            m.insert(
                "note".into(),
                format!(
                    "Recompile with compile_skill(domain='{canonical}') to drop its Reference rule from the skill."
                )
                .into(),
            );
            return Ok(compact(m));
        }

        let validated = match rules.map(validate_reference_rules).transpose() {
            Ok(v) => v,
            Err(e) => return Ok(json!({"error": e})),
        };
        let already = domains.contains(&canonical);
        let mut fields = Fields::new();
        if !already {
            domains.push(canonical.clone());
            domains.sort();
            fields.insert("skill_domains".to_owned(), py_json(&json!(domains)));
            fields.insert("promoted_at".to_owned(), now.clone());
            fields.insert("expires_at".to_owned(), String::new());
        }
        if let Some(rules) = &validated {
            let rules: Vec<Value> = rules.iter().map(|r| r.to_value()).collect();
            fields.insert("skill_rules".to_owned(), py_json(&Value::Array(rules)));
        }
        let changed = !fields.is_empty();
        if changed {
            fields.insert("updated_at".to_owned(), now);
            self.store.set_fields(key, &fields)?;
        }

        let mut m = Map::new();
        m.insert("key".into(), key.into());
        m.insert("promoted".into(), true.into());
        m.insert("skill_domains".into(), json!(domains));
        let note = if already && !changed {
            "Already promoted to this domain.".to_owned()
        } else {
            format!(
                "Compiles into the '{canonical}' skill's Reference section at the next \
                 compile_skill(domain='{canonical}') — the propose-and-accept gate still applies."
            )
        };
        m.insert("note".into(), note.into());
        if let Some(rules) = validated {
            if rules.is_empty() {
                m.insert(
                    "note".into(),
                    "Extracted rules cleared — the article reverts to a single summary Reference \
                     rule at the next compile."
                        .into(),
                );
            }
            m.insert(
                "reference_rules".into(),
                rules.iter().map(|r| r.to_value()).collect(),
            );
        }
        Ok(compact(m))
    }

    // -- briefing surfaces ----------------------------------------------------

    /// The briefing's skill sections for a project: suggestions, pending
    /// updates, the auto scan and the knowledge watch. On a greenfield
    /// project the suggestions move to the top, because a compiled skill is
    /// the only thing carrying the user's conventions there.
    pub(crate) fn briefing_skill_sections(
        &self,
        project: &str,
        result: &mut Map<String, Value>,
    ) -> Result<()> {
        let project_data = self.store.get(&format!("mem:project:{project}"))?;
        let context_text = project_data.as_ref().map(|data| {
            let joined = ["description", "stack", "goals", "current_state"]
                .iter()
                .map(|f| text(data, f))
                .collect::<Vec<_>>()
                .join(" ");
            let joined = joined.trim();
            if joined.is_empty() {
                project.to_owned()
            } else {
                joined.to_owned()
            }
        });
        let suggestions = self.suggest_skills_for_briefing(context_text.as_deref(), 3)?;
        if !suggestions.is_empty() {
            if project_data.is_none() {
                let mut reordered = Map::new();
                reordered.insert(
                    "skill_suggestions".into(),
                    json!({
                        "note": "Greenfield project (no context yet). A compiled skill carries your conventions — pick by description and load with get_skill().",
                        "skills": suggestions,
                    }),
                );
                reordered.extend(std::mem::take(result));
                *result = reordered;
            } else {
                result.insert("skill_suggestions".into(), json!({"skills": suggestions}));
            }
        }

        let updates = self.pending_skill_updates()?;
        if !updates.is_empty() {
            result.insert("skill_updates".into(), Value::Array(updates.clone()));
        }

        // Stash only: a human still reviews and commits every draft.
        let scan = || -> Result<Option<Value>> {
            if !self.skill_scan_due()? {
                return Ok(None);
            }
            let domains: Vec<String> = updates
                .iter()
                .filter_map(|u| u["domain"].as_str().map(str::to_owned))
                .collect();
            self.run_skill_scan(&domains).map(Some)
        };
        match scan() {
            Ok(Some(scan)) if scan["proposals"].as_array().is_some_and(|p| !p.is_empty()) => {
                result.insert(
                    "auto_proposed_skills".into(),
                    json!({
                        "note": "Drafts proposed automatically from recurring lessons — nothing is written until a human reviews and accepts each one. Ignoring a draft declines it.",
                        "proposals": scan["proposals"],
                    }),
                );
            }
            Ok(_) => {}
            Err(e) => tracing::error!(error = %e, "auto skill scan failed"),
        }

        let watch = self.knowledge_watch()?;
        if !watch.is_empty() {
            result.insert("skill_knowledge_watch".into(), Value::Array(watch));
        }
        Ok(())
    }

    /// Skill recommendations: semantic top-k with project context, the whole
    /// catalogue on a greenfield project. Suggests, never loads.
    pub(crate) fn suggest_skills_for_briefing(
        &self,
        context_text: Option<&str>,
        top_k: usize,
    ) -> Result<Vec<Value>> {
        let keys = self.store.scan_prefix(SKILL_KEY_PREFIX)?;
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let rows = self.store.get_fields_multi(
            &keys,
            &["name", "description", "domain", "state", "generated"],
        )?;
        let active: Vec<(String, Fields)> = keys
            .into_iter()
            .zip(rows)
            .filter_map(|(k, r)| r.map(|r| (k, r)))
            .filter(|(_, r)| state_active(r))
            .collect();
        if active.is_empty() {
            return Ok(Vec::new());
        }
        let suggestion = |key: &str, row: &Fields, similarity: Option<f64>| {
            let mut m = Map::new();
            m.insert("skill_id".into(), key.into());
            m.insert("name".into(), text(row, "name").into());
            m.insert("description".into(), text(row, "description").into());
            m.insert("domain".into(), text(row, "domain").into());
            m.insert(
                "similarity".into(),
                similarity.map_or(Value::Null, |s| json!(round_to(s, 4))),
            );
            m.insert("load_with".into(), format!("get_skill('{key}')").into());
            compact(m)
        };

        let Some(context_text) = context_text.filter(|c| !c.is_empty()) else {
            let mut entries: Vec<&(String, Fields)> = active.iter().collect();
            entries.sort_by_key(|(_, r)| text(r, "name"));
            return Ok(entries
                .into_iter()
                .take(10)
                .map(|(k, r)| suggestion(k, r, None))
                .collect());
        };

        let min_similarity = self.config.skill_suggest_min_similarity;
        let hits = self.store.search(
            Namespace::Skill,
            &self.embed(context_text)?,
            top_k.max(10),
            &active_filter(),
            None,
        )?;
        let mut scored: Vec<(f64, &str, &Fields)> = hits
            .iter()
            .filter_map(|h| {
                let (_, row) = active.iter().find(|(k, _)| *k == h.key)?;
                let similarity = f64::from(h.similarity()).max(0.0);
                (similarity >= min_similarity).then_some((similarity, h.key.as_str(), row))
            })
            .collect();
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.cmp(b.1))
        });
        Ok(scored
            .into_iter()
            .take(top_k)
            .map(|(s, k, r)| suggestion(k, r, Some(s)))
            .collect())
    }

    /// Per-skill gists of source changes since the last compile. Updated or
    /// removed sources are high risk; new lesson-bearing memories low.
    pub(crate) fn pending_skill_updates(&self) -> Result<Vec<Value>> {
        let skill_keys = self.store.scan_prefix(GENERATED_SKILL_PREFIX)?;
        if skill_keys.is_empty() {
            return Ok(Vec::new());
        }
        let rows = self.store.get_fields_multi(
            &skill_keys,
            &[
                "name",
                "domain",
                "state",
                "compiled_at",
                "source_manifest",
                "rule_manifest",
            ],
        )?;
        let skills: Vec<(String, Fields)> = skill_keys
            .into_iter()
            .zip(rows)
            .filter_map(|(k, r)| r.map(|r| (k, r)))
            .filter(|(_, r)| state_active(r) && r.get("domain").is_some_and(|d| !d.is_empty()))
            .collect();
        if skills.is_empty() {
            return Ok(Vec::new());
        }
        let mut domains: Vec<String> = skills.iter().map(|(_, r)| text(r, "domain")).collect();
        domains.sort();
        domains.dedup();
        let pools = self.gather_domain_pools(&domains)?;
        let promoted_pools = self.gather_promoted_knowledge(&domains)?;

        let mut updates = Vec::new();
        for (key, row) in &skills {
            let domain = text(row, "domain");
            let compiled_at = safe_float(row.get("compiled_at"));
            let manifest = parse_string_list(row.get("source_manifest"));
            let rule_manifest: Vec<Value> = row
                .get("rule_manifest")
                .and_then(|m| serde_json::from_str::<Value>(m).ok())
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default();
            let rules_fed_by = |source: &str| -> Vec<String> {
                rule_manifest
                    .iter()
                    .filter(|r| {
                        r.get("sources")
                            .and_then(Value::as_array)
                            .is_some_and(|s| s.iter().any(|v| v.as_str() == Some(source)))
                    })
                    .map(|r| {
                        let name = r.get("name").and_then(Value::as_str).unwrap_or("");
                        let label = if r.get("kind").and_then(Value::as_str) == Some("dont")
                            && !name.is_empty()
                        {
                            format!("Avoid {name}")
                        } else {
                            r.get("text")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned()
                        };
                        take_chars(&label, 60)
                    })
                    .collect()
            };

            let mut changes: Vec<Value> = Vec::new();
            if !manifest.is_empty() {
                let source_rows = self
                    .store
                    .get_fields_multi(&manifest, &["updated_at", "state"])?;
                for (source_key, source) in manifest.iter().zip(source_rows) {
                    let change = match &source {
                        None => Some((
                            "source_removed",
                            "source memory gone — its rules may be removed",
                        )),
                        Some(s)
                            if matches!(
                                s.get("state").map(String::as_str),
                                Some("archived" | "deleted")
                            ) =>
                        {
                            Some((
                                "source_removed",
                                "source memory gone — its rules may be removed",
                            ))
                        }
                        Some(s) if safe_float(s.get("updated_at")) > compiled_at => Some((
                            "source_updated",
                            "source memory updated — its rules may be rewritten",
                        )),
                        _ => None,
                    };
                    if let Some((change, gist)) = change {
                        let mut m = Map::new();
                        m.insert("change".into(), change.into());
                        m.insert("risk".into(), "high".into());
                        m.insert("source".into(), source_key.as_str().into());
                        m.insert("feeds_rules".into(), json!(rules_fed_by(source_key)));
                        m.insert("gist".into(), gist.into());
                        changes.push(compact(m));
                    }
                }
            }

            let in_manifest = |k: &str| manifest.iter().any(|m| m == k);
            let fresh: Vec<_> = pools
                .get(&domain)
                .map(|p| {
                    p.iter()
                        .filter(|m| {
                            !in_manifest(&m.key) && m.created_at > compiled_at && lesson_bearing(m)
                        })
                        .collect()
                })
                .unwrap_or_default();
            for mem in fresh.iter().take(3) {
                changes.push(json!({
                    "change": "new_source",
                    "risk": "low",
                    "source": mem.key,
                    "gist": take_chars(&mem.content, 60),
                }));
            }
            if fresh.len() > 3 {
                changes.push(json!({
                    "change": "new_source",
                    "risk": "low",
                    "gist": format!("+{} more new lesson-bearing memories", fresh.len() - 3),
                }));
            }

            let fresh_refs: Vec<_> = promoted_pools
                .get(&domain)
                .map(|p| {
                    p.iter()
                        .filter(|i| !in_manifest(&i.key) && i.promoted_at > compiled_at)
                        .collect()
                })
                .unwrap_or_default();
            for item in fresh_refs.iter().take(3) {
                let label = if item.title.is_empty() {
                    &item.content
                } else {
                    &item.title
                };
                changes.push(json!({
                    "change": "new_reference",
                    "risk": "low",
                    "source": item.key,
                    "gist": take_chars(label, 60),
                }));
            }
            if fresh_refs.len() > 3 {
                changes.push(json!({
                    "change": "new_reference",
                    "risk": "low",
                    "gist": format!("+{} more promoted articles", fresh_refs.len() - 3),
                }));
            }

            if !changes.is_empty() {
                let batch = changes.iter().all(|c| c["risk"] == "low");
                updates.push(json!({
                    "skill_id": key,
                    "name": text(row, "name"),
                    "domain": domain,
                    "changes": changes,
                    "batch_accept_eligible": batch,
                    "full_diff": format!("compile_skill(domain='{domain}', mode='propose')"),
                }));
            }
        }
        Ok(updates)
    }

    /// Recent knowledge close to a compiled skill, flagged as a possible
    /// contradiction when the negation heuristic fires against one of its
    /// rules. Awareness only; nothing here changes a skill.
    pub(crate) fn knowledge_watch(&self) -> Result<Vec<Value>> {
        let watch_days = self.config.skill_knowledge_watch_days;
        if watch_days <= 0 {
            return Ok(Vec::new());
        }
        let threshold = self.config.skill_knowledge_watch_threshold;
        let skill_keys = self.store.scan_prefix(SKILL_KEY_PREFIX)?;
        if skill_keys.is_empty() {
            return Ok(Vec::new());
        }
        let skill_rows = self.store.get_fields_multi(
            &skill_keys,
            &[
                "name",
                "domain",
                "state",
                "compiled_at",
                "rule_manifest",
                "source_manifest",
            ],
        )?;
        let skills: Vec<(String, Fields)> = skill_keys
            .into_iter()
            .zip(skill_rows)
            .filter_map(|(k, r)| r.map(|r| (k, r)))
            .filter(|(_, r)| state_active(r) && r.get("domain").is_some_and(|d| !d.is_empty()))
            .collect();
        if skills.is_empty() {
            return Ok(Vec::new());
        }

        let cutoff = now_secs() - watch_days as f64 * 86_400.0;
        let knowledge_keys = self.store.scan_prefix("mem:knowledge:")?;
        let knowledge_rows = self.store.get_fields_multi(
            &knowledge_keys,
            &[
                "state",
                "created_at",
                "content",
                "title",
                "feed_name",
                "source_url",
                "skill_domains",
            ],
        )?;
        let recent: Vec<(String, Fields)> = knowledge_keys
            .into_iter()
            .zip(knowledge_rows)
            .filter_map(|(k, r)| r.map(|r| (k, r)))
            .filter(|(_, r)| {
                r.get("state").map(String::as_str) == Some("active")
                    && r.get("created_at")
                        .map_or(Some(0.0), |c| c.trim().parse::<f64>().ok())
                        .is_some_and(|c| c >= cutoff)
            })
            .collect();
        if recent.is_empty() {
            return Ok(Vec::new());
        }
        let article_keys: Vec<String> = recent.iter().map(|(k, _)| k.clone()).collect();
        let article_vectors = self.store.get_vectors_multi(&article_keys);
        let skill_vector_keys: Vec<String> = skills.iter().map(|(k, _)| k.clone()).collect();
        let skill_vectors = self.store.get_vectors_multi(&skill_vector_keys);

        let mut watch = Vec::new();
        for ((skill_key, skill_row), skill_vector) in skills.iter().zip(skill_vectors) {
            let Some(skill_vector) = skill_vector else {
                continue;
            };
            let domain = text(skill_row, "domain");
            let manifest = parse_string_list(skill_row.get("source_manifest"));
            let rules: Vec<Value> = skill_row
                .get("rule_manifest")
                .and_then(|m| serde_json::from_str::<Value>(m).ok())
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default();

            let mut matches: Vec<Value> = Vec::new();
            for ((article_key, article), article_vector) in recent.iter().zip(&article_vectors) {
                let Some(article_vector) = article_vector else {
                    continue;
                };
                if manifest.contains(article_key)
                    || parse_skill_domains(article.get("skill_domains")).contains(&domain)
                {
                    continue;
                }
                let dot: f32 = skill_vector
                    .iter()
                    .zip(article_vector)
                    .map(|(a, b)| a * b)
                    .sum();
                let similarity = f64::from(dot);
                if similarity < threshold {
                    continue;
                }
                let content = text(article, "content");
                let conflicts: Vec<String> = rules
                    .iter()
                    .filter_map(|r| {
                        r.get("text")
                            .and_then(Value::as_str)
                            .filter(|t| !t.is_empty())
                    })
                    .filter(|t| has_negation_pair(&content, t))
                    .map(|t| take_chars(t, 60))
                    .collect();
                let label = article
                    .get("title")
                    .filter(|t| !t.is_empty())
                    .unwrap_or(&content);
                let mut m = Map::new();
                m.insert("key".into(), article_key.as_str().into());
                m.insert("gist".into(), take_chars(label, 80).into());
                m.insert(
                    "feed_name".into(),
                    article
                        .get("feed_name")
                        .map_or(Value::Null, |v| v.as_str().into()),
                );
                m.insert(
                    "source_url".into(),
                    article
                        .get("source_url")
                        .map_or(Value::Null, |v| v.as_str().into()),
                );
                m.insert("similarity".into(), json!(round_to(similarity, 4)));
                m.insert(
                    "possible_contradiction".into(),
                    if conflicts.is_empty() {
                        Value::Null
                    } else {
                        true.into()
                    },
                );
                m.insert("conflicts_with_rules".into(), json!(conflicts));
                matches.push(compact(m));
            }
            if matches.is_empty() {
                continue;
            }
            matches.sort_by(|a, b| {
                let flagged = |m: &Value| m.get("possible_contradiction").is_some();
                flagged(b).cmp(&flagged(a)).then_with(|| {
                    let s = |m: &Value| m["similarity"].as_f64().unwrap_or(0.0);
                    s(b).partial_cmp(&s(a)).unwrap_or(std::cmp::Ordering::Equal)
                })
            });
            matches.truncate(3);
            watch.push(json!({
                "skill_id": skill_key,
                "name": text(skill_row, "name"),
                "domain": domain,
                "articles": matches,
                "note": format!(
                    "Relevant recent knowledge — review, and promote_knowledge(key, domain='{domain}') to compile an article into the skill's Reference section."
                ),
            }));
        }
        Ok(watch)
    }
}
