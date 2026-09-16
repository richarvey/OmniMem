//! Project context tools and domain suggestion (`tools/project.py`, and the
//! suggestion half of `memory/project_domains.py`).

use std::collections::{BTreeMap, HashSet};

use omnimem_core::classification::{LICENCE_OWN, PROVENANCE_ASSERTED, PROVENANCE_CONCLUDED};
use omnimem_store::{Fields, Store};
use serde_json::{Map, Value, json};
use tracing::{info, warn};

use crate::domains::{
    DomainInput, is_valid_domain, normalise_domains, parse_domains, resolve_domain,
};
use crate::error::invalid;
use crate::lifecycle::MemoryState;
use crate::pyfmt::{compact, now_str, py_float, take_chars};
use crate::tools::{
    DomainFilter, MAX_LONG_TEXT, MAX_SHORT_TEXT, validate_project_name, validate_text,
};
use crate::{Engine, Result};

const STACK_STOPWORDS: [&str; 28] = [
    "and", "or", "the", "a", "an", "with", "using", "etc", "etc.", "plus", "via", "for", "on",
    "in", "of", "some", "various", "misc", "other", "others", "custom", "cli", "app", "apps",
    "stack", "based", "", "",
];
const MIN_TAG_OCCURRENCES: usize = 2;
const MAX_SUGGEST_SCAN_KEYS: usize = 5000;
const PROJECT_NAMESPACES: [&str; 4] = ["episodic", "project", "knowledge", "preference"];

/// A project's memories per namespace, as (key, fields).
type ProjectRows = Vec<(&'static str, Vec<(String, Fields)>)>;

/// (changed per namespace, total to change, skipped per state, changed).
type BulkOutcome = (Map<String, Value>, usize, Map<String, Value>, usize);

fn is_stopword(item: &str) -> bool {
    let item = item.trim().to_lowercase();
    !item.is_empty() && STACK_STOPWORDS.contains(&item.as_str())
}

fn project_name(name: &str) -> Result<()> {
    validate_project_name(Some(name))
}

fn text(fields: &Fields, name: &str) -> Option<String> {
    fields.get(name).filter(|v| !v.is_empty()).cloned()
}

fn doc_project(fields: &Fields) -> Option<&str> {
    fields
        .get("project")
        .filter(|p| !p.is_empty())
        .or_else(|| fields.get("project_name").filter(|p| !p.is_empty()))
        .map(String::as_str)
}

fn read_domains(fields: Option<&Fields>) -> Vec<String> {
    fields
        .and_then(|f| f.get("domains"))
        .map(|d| normalise_domains(DomainInput::Text(d)).domains)
        .unwrap_or_default()
}

fn domain_list(domains: &[String]) -> Value {
    if domains.is_empty() {
        Value::Null
    } else {
        json!(domains)
    }
}

/// Tags as stored on episodic memories: a JSON array, sometimes a string.
fn parse_tag_field(raw: Option<&str>) -> Vec<String> {
    let Some(raw) = raw.filter(|r| !r.is_empty()) else {
        return Vec::new();
    };
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Array(items)) => items
            .into_iter()
            .filter(|v| !matches!(v, Value::Null | Value::Bool(false)) && v.as_str() != Some(""))
            .map(|v| match v {
                Value::String(s) => s,
                other => other.to_string(),
            })
            .collect(),
        Ok(_) => Vec::new(),
        Err(_) => raw
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
            .collect(),
    }
}

pub struct Suggestion {
    pub existing: Vec<String>,
    pub suggested: Vec<String>,
    pub merged: Vec<String>,
    pub evidence: Vec<(String, Vec<String>)>,
}

/// Seed domains from each project's stack, once (6.6 migration). Returns
/// (seeded, marked empty).
pub fn migrate_project_domains(store: &Store) -> omnimem_store::Result<(usize, usize)> {
    let keys = store.scan_prefix("mem:project:")?;
    let rows = store.get_fields_multi(&keys, &["stack", "domains", "goals"])?;
    let (mut seeded, mut marked) = (0, 0);
    for (key, row) in keys.iter().zip(rows) {
        let Some(row) = row else { continue };
        if row.contains_key("domains") {
            continue;
        }
        if text(&row, "stack").is_none() && text(&row, "goals").is_none() {
            continue;
        }
        let candidates: Vec<String> = parse_domains(DomainInput::Text(
            row.get("stack").map_or("", String::as_str),
        ))
        .into_iter()
        .filter(|item| !is_stopword(item))
        .collect();
        let domains = normalise_domains(DomainInput::List(&candidates)).domains;
        store.set_field(key, "domains", &domains.join(","))?;
        if domains.is_empty() {
            marked += 1;
        } else {
            seeded += 1;
        }
    }
    if seeded + marked > 0 {
        info!(
            seeded,
            marked, "migration: seeded project domains from stack"
        );
    }
    Ok((seeded, marked))
}

impl Engine {
    /// Propose domains from the project's stack and recurring tags, with the
    /// evidence for each. Nothing is stored.
    pub fn suggest_domains_for_project(&self, name: &str, limit: usize) -> Result<Suggestion> {
        let row = self.store.get(&format!("mem:project:{name}"))?;
        let existing = read_domains(row.as_ref());
        let stack = row
            .as_ref()
            .and_then(|r| text(r, "stack"))
            .unwrap_or_default();

        let mut evidence: Vec<(String, Vec<String>)> = Vec::new();
        let mut add =
            |domain: String, why: String| match evidence.iter_mut().find(|(d, _)| *d == domain) {
                Some((_, reasons)) => {
                    if !reasons.contains(&why) {
                        reasons.push(why);
                    }
                }
                None => evidence.push((domain, vec![why])),
            };

        for item in parse_domains(DomainInput::Text(&stack)) {
            if is_stopword(&item) {
                continue;
            }
            let (canonical, _) = resolve_domain(&item);
            if is_valid_domain(&canonical) {
                add(canonical, "stack".to_owned());
            }
        }

        let mut tag_counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut keys = self.store.scan_prefix("mem:episodic:")?;
        if keys.len() > MAX_SUGGEST_SCAN_KEYS {
            warn!(
                cap = MAX_SUGGEST_SCAN_KEYS,
                total = keys.len(),
                "domain suggestion scan capped"
            );
            keys.truncate(MAX_SUGGEST_SCAN_KEYS);
        }
        for row in self
            .store
            .get_fields_multi(&keys, &["project", "project_name", "tags", "state"])?
            .into_iter()
            .flatten()
        {
            if !matches!(row.get("state").map(String::as_str), None | Some("active")) {
                continue;
            }
            if doc_project(&row) != Some(name) {
                continue;
            }
            for tag in parse_tag_field(row.get("tags").map(String::as_str)) {
                let (canonical, _) = resolve_domain(&tag);
                if is_valid_domain(&canonical) {
                    *tag_counts.entry(canonical).or_default() += 1;
                }
            }
        }
        let mut counted: Vec<(String, usize)> = tag_counts.into_iter().collect();
        counted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        for (domain, count) in counted {
            if count >= MIN_TAG_OCCURRENCES {
                add(domain, format!("tagged on {count} memories"));
            }
        }

        let suggested: Vec<String> = evidence
            .iter()
            .map(|(d, _)| d.clone())
            .filter(|d| !existing.contains(d))
            .take(limit)
            .collect();
        let combined: Vec<String> = existing.iter().chain(&suggested).cloned().collect();
        let merged = normalise_domains(DomainInput::List(&combined)).domains;
        let evidence = evidence
            .into_iter()
            .filter(|(d, _)| suggested.contains(d))
            .collect();
        Ok(Suggestion {
            existing,
            suggested,
            merged,
            evidence,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn set_project_context(
        &self,
        name: &str,
        description: &str,
        stack: &str,
        goals: &str,
        current_state: &str,
        notes: Option<&str>,
        domains: Option<&DomainFilter>,
    ) -> Result<Value> {
        project_name(name)?;
        validate_text("description", description, MAX_SHORT_TEXT)?;
        validate_text("stack", stack, MAX_SHORT_TEXT)?;
        validate_text("goals", goals, MAX_LONG_TEXT)?;
        validate_text("current_state", current_state, MAX_LONG_TEXT)?;
        if let Some(notes) = notes {
            validate_text("notes", notes, MAX_LONG_TEXT)?;
        }
        let key = format!("mem:project:{name}");
        let now = now_str();
        let vector = self.embed(&format!("{description} {goals} {current_state}"))?;
        let mut fields = Fields::from([
            ("content".to_owned(), description.to_owned()),
            ("project_name".to_owned(), name.to_owned()),
            ("description".to_owned(), description.to_owned()),
            ("stack".to_owned(), stack.to_owned()),
            ("goals".to_owned(), goals.to_owned()),
            ("current_state".to_owned(), current_state.to_owned()),
            ("state".to_owned(), "active".to_owned()),
            ("surface_score".to_owned(), "1.0".to_owned()),
            ("created_at".to_owned(), now.clone()),
            ("updated_at".to_owned(), now),
            ("licence".to_owned(), LICENCE_OWN.to_owned()),
            ("provenance".to_owned(), PROVENANCE_ASSERTED.to_owned()),
        ]);
        if let Some(n) = notes.filter(|n| !n.is_empty()) {
            fields.insert("notes".to_owned(), n.to_owned());
        }
        let (resolved, aliased, rejected) = match domains {
            None => {
                let existing = self.store.get(&key)?;
                (
                    existing.as_ref().map(|e| read_domains(Some(e))),
                    Vec::new(),
                    Vec::new(),
                )
            }
            Some(filter) => {
                let n = match filter {
                    DomainFilter::One(s) => normalise_domains(DomainInput::Text(s)),
                    DomainFilter::Many(v) => normalise_domains(DomainInput::List(v)),
                };
                (Some(n.domains), n.aliased, n.rejected)
            }
        };
        if let Some(resolved) = &resolved {
            fields.insert("domains".to_owned(), resolved.join(","));
        }
        self.store.upsert(&key, &fields, Some(&vector))?;
        self.invalidate_domain_cache();
        info!(project = name, "saved project context");

        let mut m = Map::new();
        m.insert("project_name".into(), name.into());
        m.insert(
            "domains".into(),
            domain_list(resolved.as_deref().unwrap_or_default()),
        );
        m.insert(
            "resolved_aliases".into(),
            Value::Object(aliased.into_iter().map(|(a, c)| (a, c.into())).collect()),
        );
        m.insert("rejected_domains".into(), json!(rejected));
        Ok(compact(m))
    }

    pub fn get_project_context(&self, name: &str) -> Result<Value> {
        project_name(name)?;
        let Some(data) = self.store.get(&format!("mem:project:{name}"))? else {
            return Ok(json!({"status": "not_found"}));
        };
        let field = |n: &str, default: &str| data.get(n).map_or(default, String::as_str).to_owned();
        let mut m = Map::new();
        m.insert("status".into(), "found".into());
        m.insert("project_name".into(), field("project_name", name).into());
        m.insert("description".into(), field("description", "").into());
        m.insert("stack".into(), field("stack", "").into());
        m.insert("domains".into(), domain_list(&read_domains(Some(&data))));
        m.insert("goals".into(), field("goals", "").into());
        m.insert("current_state".into(), field("current_state", "").into());
        m.insert(
            "notes".into(),
            data.get("notes").map_or(Value::Null, |n| n.as_str().into()),
        );
        m.insert("state".into(), field("state", "active").into());
        m.insert(
            "updated_at".into(),
            data.get("updated_at")
                .map_or(Value::Null, |u| u.as_str().into()),
        );
        Ok(compact(m))
    }

    pub fn list_projects(&self, domain: Option<&str>) -> Result<Value> {
        let wanted = match domain.filter(|d| !d.is_empty()) {
            None => None,
            Some(d) => {
                let n = normalise_domains(DomainInput::Text(d));
                if !n.rejected.is_empty() || n.domains.is_empty() {
                    return Err(invalid(format!(
                        "Invalid domain filter: '{d}'. Use 1-64 characters of lowercase letters, \
                         digits, hyphens, underscores or dots."
                    )));
                }
                Some(n.domains[0].clone())
            }
        };
        let keys = self.store.scan_prefix("mem:project:")?;
        let rows = self.store.get_fields_multi(
            &keys,
            &[
                "project_name",
                "project",
                "goals",
                "stack",
                "description",
                "state",
                "domains",
            ],
        )?;
        let mut projects: Vec<(String, Map<String, Value>, Vec<String>)> = Vec::new();
        for (key, row) in keys.iter().zip(rows) {
            let Some(data) = row else { continue };
            let name = text(&data, "project_name")
                .or_else(|| text(&data, "project"))
                .unwrap_or_else(|| key.rsplit(':').next().unwrap_or("").to_owned());
            let is_context = text(&data, "goals").is_some() || text(&data, "stack").is_some();
            let index = if let Some(i) = projects.iter().position(|(n, _, _)| *n == name) {
                i
            } else {
                let mut m = Map::new();
                m.insert("project_name".into(), name.as_str().into());
                m.insert("description".into(), "".into());
                m.insert("state".into(), "active".into());
                m.insert("memory_count".into(), 0.into());
                projects.push((name.clone(), m, Vec::new()));
                projects.len() - 1
            };
            let (_, entry, domains) = &mut projects[index];
            if is_context {
                entry.insert(
                    "description".into(),
                    take_chars(data.get("description").map_or("", String::as_str), 80).into(),
                );
                entry.insert(
                    "state".into(),
                    data.get("state").map_or("active", String::as_str).into(),
                );
                *domains = read_domains(Some(&data));
            } else {
                let n = entry["memory_count"].as_i64().unwrap_or(0) + 1;
                entry.insert("memory_count".into(), n.into());
            }
        }
        projects.sort_by_key(|(n, _, _)| n.to_lowercase());
        if let Some(w) = &wanted {
            projects.retain(|(_, _, d)| d.contains(w));
        }
        let empty = projects.is_empty();
        let list: Vec<Value> = projects
            .into_iter()
            .map(|(_, mut m, d)| {
                if !d.is_empty() {
                    m.insert("domains".into(), json!(d));
                }
                Value::Object(m)
            })
            .collect();
        let mut result = Map::new();
        result.insert("projects".into(), Value::Array(list));
        if let Some(w) = wanted {
            result.insert("domain".into(), w.as_str().into());
            if empty {
                result.insert(
                    "note".into(),
                    format!(
                        "No project declares the domain '{w}'. Run compile_project_domains(project_name) \
                         to suggest domains for a project from its stack and memories."
                    )
                    .into(),
                );
            }
        }
        Ok(Value::Object(result))
    }

    pub fn compile_project_domains(&self, name: &str, auto_save: bool) -> Result<Value> {
        project_name(name)?;
        let key = format!("mem:project:{name}");
        if self.store.get(&key)?.is_none() {
            return Ok(json!({
                "status": "not_found",
                "project_name": name,
                "note": "No project context stored. Create one with set_project_context() or compile_project_context() first.",
            }));
        }
        let s = self.suggest_domains_for_project(name, 10)?;
        let mut saved = false;
        if auto_save && s.merged != s.existing {
            let updates = Fields::from([
                ("domains".to_owned(), s.merged.join(",")),
                ("updated_at".to_owned(), now_str()),
            ]);
            self.store.set_fields(&key, &updates)?;
            self.invalidate_domain_cache();
            saved = true;
        }
        let mut m = Map::new();
        m.insert("status".into(), "compiled".into());
        m.insert("project_name".into(), name.into());
        m.insert("existing_domains".into(), json!(s.existing));
        m.insert("suggested_domains".into(), json!(s.suggested));
        m.insert("merged_domains".into(), json!(s.merged));
        if !s.evidence.is_empty() {
            m.insert(
                "evidence".into(),
                Value::Object(s.evidence.into_iter().map(|(d, e)| (d, json!(e))).collect()),
            );
        }
        if saved {
            m.insert("auto_saved".into(), true.into());
        } else if s.suggested.is_empty() {
            m.insert(
                "note".into(),
                "Nothing new to suggest. Domains are read from the project's stack field and from tags \
                 that recur across its memories — set them by hand with set_project_context(domains=[...]) \
                 if neither carries the signal."
                    .into(),
            );
        } else {
            m.insert(
                "note".into(),
                "Call again with auto_save=True to store these.".into(),
            );
        }
        let known = self.domain_map()?;
        if !known.is_empty() {
            let mut in_use: Vec<(String, usize)> =
                known.iter().map(|(d, p)| (d.clone(), p.len())).collect();
            in_use.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            m.insert(
                "domains_in_use".into(),
                Value::Object(in_use.into_iter().map(|(d, n)| (d, n.into())).collect()),
            );
        }
        Ok(Value::Object(m))
    }

    pub fn update_project_state(
        &self,
        name: &str,
        current_state: &str,
        notes: Option<&str>,
    ) -> Result<Value> {
        project_name(name)?;
        validate_text("current_state", current_state, MAX_LONG_TEXT)?;
        if let Some(notes) = notes {
            validate_text("notes", notes, MAX_LONG_TEXT)?;
        }
        let key = format!("mem:project:{name}");
        if self.store.get(&key)?.is_none() {
            return Ok(json!({"status": "not_found"}));
        }
        let mut updates = Fields::from([
            ("current_state".to_owned(), current_state.to_owned()),
            ("updated_at".to_owned(), now_str()),
        ]);
        if let Some(n) = notes {
            updates.insert("notes".to_owned(), n.to_owned());
        }
        self.store.set_fields(&key, &updates)?;
        Ok(json!({"project_name": name}))
    }

    /// Keys per namespace belonging to a project, the context entry only when asked.
    fn project_keys(&self, name: &str, include_context: bool) -> Result<ProjectRows> {
        let context_key = format!("mem:project:{name}");
        let mut out = Vec::new();
        for ns in PROJECT_NAMESPACES {
            let keys = self.store.scan_prefix(&format!("mem:{ns}:"))?;
            let rows = self
                .store
                .get_fields_multi(&keys, &["project", "project_name", "state"])?;
            let matched: Vec<(String, Fields)> = keys
                .into_iter()
                .zip(rows)
                .filter_map(|(k, r)| r.map(|r| (k, r)))
                .filter(|(_, r)| doc_project(r) == Some(name))
                .filter(|(k, _)| include_context || *k != context_key)
                .collect();
            if !matched.is_empty() {
                out.push((ns, matched));
            }
        }
        Ok(out)
    }

    pub fn delete_project(
        &self,
        name: &str,
        confirm: bool,
        include_context: bool,
    ) -> Result<Value> {
        project_name(name)?;
        let groups = self.project_keys(name, include_context)?;
        let counts: Map<String, Value> = groups
            .iter()
            .map(|(ns, k)| ((*ns).to_owned(), k.len().into()))
            .collect();
        let total: usize = groups.iter().map(|(_, k)| k.len()).sum();
        if total == 0 {
            return Ok(json!({"status": "not_found", "project_name": name}));
        }
        if !confirm {
            return Ok(json!({
                "status": "preview",
                "project_name": name,
                "would_delete": counts,
                "total": total,
                "note": "Call again with confirm=True to delete.",
            }));
        }
        let mut deleted = 0;
        for (_, keys) in &groups {
            let keys: Vec<String> = keys.iter().map(|(k, _)| k.clone()).collect();
            deleted += self.store.delete_many(&keys)?;
        }
        self.invalidate_domain_cache();
        self.invalidate_abandoned_cache();
        info!(project = name, deleted, "deleted project memories");
        Ok(json!({"status": "deleted", "project_name": name, "deleted": counts, "total": deleted}))
    }

    /// Move every matching memory of a project to `new_state`.
    fn bulk_transition_project(
        &self,
        name: &str,
        new_state: MemoryState,
        apply: bool,
        reason: Option<&str>,
        include_context: bool,
    ) -> Result<BulkOutcome> {
        let mut counts = Map::new();
        let mut skipped: Map<String, Value> = Map::new();
        let mut to_change: Vec<String> = Vec::new();
        for (ns, rows) in self.project_keys(name, include_context)? {
            let mut n = 0;
            for (key, row) in rows {
                let current = MemoryState::parse(row.get("state").map_or("active", |s| {
                    if s.is_empty() { "active" } else { s.as_str() }
                }))
                .unwrap_or(MemoryState::Active);
                if current == new_state || !current.can_become(new_state) {
                    let slot = skipped.entry(current.as_str()).or_insert(0.into());
                    *slot = (slot.as_i64().unwrap_or(0) + 1).into();
                    continue;
                }
                to_change.push(key);
                n += 1;
            }
            if n > 0 {
                counts.insert(ns.to_owned(), n.into());
            }
        }
        let total = to_change.len();
        let mut changed = 0;
        if apply && total > 0 {
            let mut updates = Fields::from([
                ("state".to_owned(), new_state.as_str().to_owned()),
                (
                    "surface_score".to_owned(),
                    py_float(self.surface_score(new_state)),
                ),
                ("updated_at".to_owned(), now_str()),
            ]);
            if new_state == MemoryState::Deprioritised
                && let Some(r) = reason.filter(|r| !r.is_empty())
            {
                updates.insert("deprioritised_reason".to_owned(), r.to_owned());
            }
            changed = self.store.set_fields_multi(&to_change, &updates)?;
        }
        Ok((counts, total, skipped, changed))
    }

    pub fn deprioritise_project(
        &self,
        name: &str,
        confirm: bool,
        reason: Option<&str>,
        include_context: bool,
    ) -> Result<Value> {
        project_name(name)?;
        if let Some(reason) = reason {
            validate_text("reason", reason, MAX_SHORT_TEXT)?;
        }
        let (counts, total, skipped, changed) = self.bulk_transition_project(
            name,
            MemoryState::Deprioritised,
            confirm,
            reason,
            include_context,
        )?;
        let mut m = Map::new();
        if total == 0 {
            m.insert(
                "status".into(),
                if skipped.is_empty() {
                    "not_found"
                } else {
                    "nothing_to_change"
                }
                .into(),
            );
            m.insert("project_name".into(), name.into());
            m.insert("already_inactive".into(), Value::Object(skipped));
            return Ok(compact(m));
        }
        if !confirm {
            m.insert("status".into(), "preview".into());
            m.insert("project_name".into(), name.into());
            m.insert("would_deprioritise".into(), Value::Object(counts));
            m.insert("total".into(), total.into());
            m.insert("already_inactive".into(), Value::Object(skipped));
            m.insert(
                "note".into(),
                "Call again with confirm=True to deprioritise.".into(),
            );
            return Ok(compact(m));
        }
        m.insert("status".into(), "deprioritised".into());
        m.insert("project_name".into(), name.into());
        m.insert("deprioritised".into(), Value::Object(counts));
        m.insert("total".into(), changed.into());
        m.insert("already_inactive".into(), Value::Object(skipped));
        Ok(compact(m))
    }

    pub fn reinstate_project(
        &self,
        name: &str,
        confirm: bool,
        include_context: bool,
    ) -> Result<Value> {
        project_name(name)?;
        let (counts, total, skipped, changed) = self.bulk_transition_project(
            name,
            MemoryState::Active,
            confirm,
            None,
            include_context,
        )?;
        let mut m = Map::new();
        if total == 0 {
            m.insert(
                "status".into(),
                if skipped.is_empty() {
                    "not_found"
                } else {
                    "nothing_to_change"
                }
                .into(),
            );
            m.insert("project_name".into(), name.into());
            m.insert("already_active".into(), Value::Object(skipped));
            return Ok(compact(m));
        }
        if !confirm {
            m.insert("status".into(), "preview".into());
            m.insert("project_name".into(), name.into());
            m.insert("would_reinstate".into(), Value::Object(counts));
            m.insert("total".into(), total.into());
            m.insert("already_active".into(), Value::Object(skipped));
            m.insert(
                "note".into(),
                "Call again with confirm=True to reinstate.".into(),
            );
            return Ok(compact(m));
        }
        m.insert("status".into(), "reinstated".into());
        m.insert("project_name".into(), name.into());
        m.insert("reinstated".into(), Value::Object(counts));
        m.insert("total".into(), changed.into());
        m.insert("already_active".into(), Value::Object(skipped));
        Ok(compact(m))
    }

    pub fn compile_project_context(&self, name: &str, auto_save: bool) -> Result<Value> {
        project_name(name)?;
        let existing_key = format!("mem:project:{name}");
        let existing_data = self.store.get(&existing_key)?;
        let existing_context = existing_data
            .as_ref()
            .filter(|d| text(d, "goals").is_some() || text(d, "stack").is_some())
            .map(|d| {
                let mut m = Map::new();
                for f in ["description", "stack"] {
                    m.insert(f.into(), d.get(f).map_or("", String::as_str).into());
                }
                m.insert("domains".into(), domain_list(&read_domains(Some(d))));
                for f in ["goals", "current_state"] {
                    m.insert(f.into(), d.get(f).map_or("", String::as_str).into());
                }
                m.insert(
                    "notes".into(),
                    d.get("notes").map_or(Value::Null, |n| n.as_str().into()),
                );
                m.insert(
                    "updated_at".into(),
                    d.get("updated_at")
                        .map_or(Value::Null, |u| u.as_str().into()),
                );
                compact(m)
            });

        let keys = self.store.scan_prefix("mem:episodic:")?;
        let rows = self.store.get_multi(&keys)?;
        let mut memories: Vec<(f64, Value)> = Vec::new();
        let mut tag_counts: Vec<(String, usize)> = Vec::new();
        let (mut breakthroughs, mut gotchas) = (Vec::new(), Vec::new());
        let mut abandoned: Vec<Value> = Vec::new();
        let mut seen_abandoned: HashSet<String> = HashSet::new();

        for (key, row) in keys.iter().zip(rows) {
            let Some(data) = row else { continue };
            if !matches!(data.get("state").map(String::as_str), None | Some("active")) {
                continue;
            }
            if doc_project(&data) != Some(name) {
                continue;
            }
            let updated_at = data
                .get("updated_at")
                .cloned()
                .unwrap_or_else(|| "0".to_owned());
            let tags: Value = data
                .get("tags")
                .and_then(|t| serde_json::from_str(t).ok())
                .unwrap_or(json!([]));
            if let Value::Array(items) = &tags {
                for t in items
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|t| !t.is_empty())
                {
                    match tag_counts.iter_mut().find(|(n, _)| n == t) {
                        Some(slot) => slot.1 += 1,
                        None => tag_counts.push((t.to_owned(), 1)),
                    }
                }
            }
            let mut entry = Map::new();
            entry.insert("key".into(), key.as_str().into());
            entry.insert(
                "content".into(),
                data.get("content").map_or("", String::as_str).into(),
            );
            entry.insert("updated_at".into(), updated_at.as_str().into());
            if tags.as_array().is_some_and(|a| !a.is_empty()) {
                entry.insert("tags".into(), tags);
            }
            if let Some(effort) = data
                .get("effort_score")
                .and_then(|e| e.trim().parse::<f64>().ok())
            {
                entry.insert("effort_score".into(), (effort as i64).into());
            }
            if let Some(outcome) = text(&data, "outcome") {
                entry.insert("outcome".into(), outcome.into());
            }
            memories.push((updated_at.parse().unwrap_or(0.0), Value::Object(entry)));
            if let Some(bt) = text(&data, "breakthrough") {
                breakthroughs.push(bt);
            }
            if let Some(g) = text(&data, "gotchas") {
                gotchas.push(g);
            }
            if let Some(Value::Array(approaches)) = data
                .get("abandoned_approaches")
                .and_then(|a| serde_json::from_str(a).ok())
            {
                for a in approaches {
                    let Some(n) = a
                        .get("name")
                        .and_then(Value::as_str)
                        .filter(|n| !n.is_empty())
                    else {
                        continue;
                    };
                    if !seen_abandoned.insert(n.to_lowercase()) {
                        continue;
                    }
                    abandoned.push(json!({
                        "name": n,
                        "type": a.get("type").and_then(Value::as_str).unwrap_or(""),
                        "reason": a.get("reason").and_then(Value::as_str).unwrap_or(""),
                    }));
                }
            }
        }
        memories.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut ranked_tags = tag_counts;
        ranked_tags.sort_by_key(|t| std::cmp::Reverse(t.1));
        let top_tags: Vec<String> = ranked_tags.into_iter().take(20).map(|(t, _)| t).collect();

        let suggestion = self.suggest_domains_for_project(name, 10)?;
        let mut notes_parts = Vec::new();
        if !breakthroughs.is_empty() {
            notes_parts.push(format!("Breakthroughs: {}", breakthroughs.join("; ")));
        }
        if !gotchas.is_empty() {
            notes_parts.push(format!("Gotchas: {}", gotchas.join("; ")));
        }
        if !abandoned.is_empty() {
            let dead_ends: Vec<String> = abandoned
                .iter()
                .map(|a| {
                    format!(
                        "{} ({})",
                        a["name"].as_str().unwrap_or(""),
                        a["reason"].as_str().unwrap_or("")
                    )
                })
                .collect();
            notes_parts.push(format!("Abandoned approaches: {}", dead_ends.join("; ")));
        }
        let compiled_state: Vec<String> = memories
            .iter()
            .take(5)
            .map(|(_, m)| take_chars(m["content"].as_str().unwrap_or(""), 200))
            .collect();

        let existing_str = |field: &str| {
            existing_context
                .as_ref()
                .and_then(|c| c.get(field))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        let mut draft = Map::new();
        draft.insert(
            "description".into(),
            existing_str("description").unwrap_or_default().into(),
        );
        draft.insert(
            "stack".into(),
            existing_str("stack")
                .unwrap_or_else(|| top_tags.join(", "))
                .into(),
        );
        draft.insert("domains".into(), domain_list(&suggestion.merged));
        draft.insert(
            "goals".into(),
            existing_str("goals").unwrap_or_default().into(),
        );
        draft.insert(
            "current_state".into(),
            compiled_state.join("\n---\n").into(),
        );
        draft.insert("notes".into(), notes_parts.join("\n").into());
        let draft = compact(draft);

        let mut saved = false;
        if auto_save && (!memories.is_empty() || existing_context.is_some()) {
            let now = now_str();
            let field = |f: &str| {
                draft
                    .get(f)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned()
            };
            let (description, goals, current_state) =
                (field("description"), field("goals"), field("current_state"));
            let vector = self.embed(&format!("{description} {goals} {current_state}"))?;
            let mut fields = Fields::from([
                ("content".to_owned(), description.clone()),
                ("project_name".to_owned(), name.to_owned()),
                ("description".to_owned(), description),
                ("stack".to_owned(), field("stack")),
                ("goals".to_owned(), goals),
                ("current_state".to_owned(), current_state),
                ("notes".to_owned(), field("notes")),
                ("domains".to_owned(), suggestion.merged.join(",")),
                ("state".to_owned(), "active".to_owned()),
                ("surface_score".to_owned(), "1.0".to_owned()),
                ("updated_at".to_owned(), now.clone()),
                ("licence".to_owned(), LICENCE_OWN.to_owned()),
                (
                    "provenance".to_owned(),
                    existing_data
                        .as_ref()
                        .and_then(|d| text(d, "provenance"))
                        .unwrap_or_else(|| PROVENANCE_CONCLUDED.to_owned()),
                ),
            ]);
            if existing_data.is_none() {
                fields.insert("created_at".to_owned(), now);
            }
            self.store.upsert(&existing_key, &fields, Some(&vector))?;
            self.invalidate_domain_cache();
            saved = true;
        }

        let mut result = Map::new();
        result.insert("project_name".into(), name.into());
        result.insert("memory_count".into(), memories.len().into());
        result.insert("draft".into(), draft);
        if let Some(c) = existing_context {
            result.insert("existing_context".into(), c);
        }
        if !top_tags.is_empty() {
            result.insert("top_tags".into(), json!(top_tags));
        }
        if !suggestion.evidence.is_empty() {
            result.insert(
                "domain_evidence".into(),
                Value::Object(
                    suggestion
                        .evidence
                        .into_iter()
                        .map(|(d, e)| (d, json!(e)))
                        .collect(),
                ),
            );
        }
        if !abandoned.is_empty() {
            result.insert("abandoned_approaches".into(), Value::Array(abandoned));
        }
        if !breakthroughs.is_empty() {
            result.insert("breakthroughs".into(), json!(breakthroughs));
        }
        if !gotchas.is_empty() {
            result.insert("gotchas".into(), json!(gotchas));
        }
        if !memories.is_empty() {
            result.insert(
                "memories".into(),
                memories.into_iter().map(|(_, m)| m).collect(),
            );
        }
        if saved {
            result.insert("auto_saved".into(), true.into());
        }
        Ok(Value::Object(result))
    }

    /// Run the domain migration and drop the cached map it may have changed.
    pub fn migrate_project_domains(&self) -> Result<(usize, usize)> {
        let outcome = migrate_project_domains(&self.store)?;
        self.invalidate_domain_cache();
        Ok(outcome)
    }
}
