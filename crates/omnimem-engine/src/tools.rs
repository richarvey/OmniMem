//! The core tool behaviours (`tools/core.py`, `tools/backup.py`,
//! `tools/queue.py` and the `health` tool), returning 6.x's result shapes.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use chrono::{DateTime, Local, Utc};
use omnimem_core::{MemoryKey, Namespace};
use omnimem_store::{BackupFile, Fields};
use regex::Regex;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tracing::{info, warn};
use ulid::Ulid;

use crate::chunking::{self, VALID_STRATEGIES};
use crate::classification::{classification_fields, licence_for_write, provenance_for_write};
use crate::domains::DomainInput;
use crate::error::invalid;
use crate::lifecycle::{MemoryState, SUPPRESSED_KEY};
use crate::pyfmt::{compact, now_str, py_json, round_to, take_chars};
use crate::recall::{RecallResult, tags_truthy};
use crate::tags::validate_tags;
use crate::{Engine, EngineError, Result};

pub const MAX_CONTENT_LENGTH: usize = 50_000;
pub const MAX_TOP_K: i64 = 50;
const MAX_BACKUP_FILE_SIZE: u64 = 100 * 1024 * 1024;
const BACKUP_PREFIXES: [&str; 4] = ["mem:", "topics:", "log:recall:", "meta:"];
const WRITABLE: [&str; 4] = ["episodic", "knowledge", "preference", "project"];

static SAFE_NAME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9_\-. ]+$").expect("valid"));
static SAFE_FILENAME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9_][a-zA-Z0-9_.\-]*\.json$").expect("valid"));

/// `domain_filter`: one domain (or a comma list in a string), or a list.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum DomainFilter {
    One(String),
    Many(Vec<String>),
}

impl DomainFilter {
    fn is_empty(&self) -> bool {
        match self {
            DomainFilter::One(s) => s.is_empty(),
            DomainFilter::Many(v) => v.is_empty(),
        }
    }

    fn input(&self) -> DomainInput<'_> {
        match self {
            DomainFilter::One(s) => DomainInput::Text(s),
            DomainFilter::Many(v) => DomainInput::List(v),
        }
    }

    fn as_list(&self) -> Vec<String> {
        match self {
            DomainFilter::One(s) => vec![s.clone()],
            DomainFilter::Many(v) => v.clone(),
        }
    }
}

pub(crate) fn validate_namespace(namespace: &str) -> Result<()> {
    if WRITABLE.contains(&namespace) {
        Ok(())
    } else {
        Err(invalid(format!(
            "Invalid namespace '{namespace}'. Must be one of: {}",
            WRITABLE.join(", ")
        )))
    }
}

pub(crate) fn validate_project_name(project: Option<&str>) -> Result<()> {
    let Some(project) = project else {
        return Ok(());
    };
    if project.is_empty() || project.chars().count() > 200 {
        return Err(invalid("Project name must be 1-200 characters"));
    }
    if !SAFE_NAME_RE.is_match(project) {
        return Err(invalid(
            "Project name contains invalid characters. Only alphanumeric, hyphens, underscores, dots, and spaces are allowed.",
        ));
    }
    Ok(())
}

fn validate_content(content: &str) -> Result<()> {
    if content.trim().is_empty() {
        return Err(invalid("Content cannot be empty"));
    }
    let length = content.chars().count();
    if length > MAX_CONTENT_LENGTH {
        return Err(invalid(format!(
            "Content too long ({length} chars). Maximum is {MAX_CONTENT_LENGTH}."
        )));
    }
    Ok(())
}

fn nonempty(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|v| !v.is_empty())
}

/// The recall-output shape of one result.
fn recall_entry(r: &RecallResult) -> Value {
    let mut e = Map::new();
    e.insert("key".into(), r.key.as_str().into());
    e.insert("namespace".into(), r.namespace.as_str().into());
    e.insert("content".into(), r.content.as_str().into());
    e.insert("score".into(), json!(r.adjusted_score));
    e.insert("state".into(), r.state.as_str().into());
    if let Some(p) = nonempty(&r.project) {
        e.insert("project".into(), p.into());
    }
    if r.result_type != "memory" {
        e.insert("result_type".into(), r.result_type.into());
    }
    if r.weak_match {
        e.insert("weak_match".into(), true.into());
    }
    if tags_truthy(&r.tags) {
        e.insert("tags".into(), r.tags.clone());
    }
    if r.reinstate_candidate {
        e.insert("reinstate_candidate".into(), true.into());
        if let Some(reason) = nonempty(&r.deprioritised_reason) {
            e.insert("deprioritised_reason".into(), reason.into());
        }
    }
    if let Some(effort) = r.effort_score {
        e.insert("effort_score".into(), effort.into());
    }
    for (name, value) in [
        ("outcome", &r.outcome),
        ("breakthrough", &r.breakthrough),
        ("lesson", &r.lesson),
    ] {
        if let Some(v) = nonempty(value) {
            e.insert(name.into(), v.into());
        }
    }
    if !r.contradictions.is_empty() {
        e.insert("contradictions".into(), r.contradictions.len().into());
    }
    if let Some(url) = nonempty(&r.source_url) {
        e.insert("source_url".into(), url.into());
    }
    if let Some(date) = r.event_date {
        e.insert("event_date".into(), json!(date));
    }
    for (name, value) in [
        ("enriched_from", &r.enriched_from),
        ("licence", &r.licence),
        ("licence_note", &r.licence_note),
        ("provenance", &r.provenance),
    ] {
        if let Some(v) = nonempty(value) {
            e.insert(name.into(), v.into());
        }
    }
    Value::Object(e)
}

/// A trailing notice listing results whose licence is still unknown.
fn licence_notice(entries: &[Value]) -> Option<Value> {
    let unclassified: Vec<&str> = entries
        .iter()
        .filter(|e| e.get("licence").and_then(Value::as_str) == Some("unknown"))
        .filter(|e| {
            matches!(
                e.get("result_type")
                    .and_then(Value::as_str)
                    .unwrap_or("memory"),
                "memory" | "knowledge"
            )
        })
        .filter_map(|e| e.get("key").and_then(Value::as_str))
        .collect();
    if unclassified.is_empty() {
        return None;
    }
    let n = unclassified.len();
    let (s, have) = if n == 1 { ("", "has") } else { ("s", "have") };
    Some(json!({
        "result_type": "licence_notice",
        "unclassified": unclassified,
        "note": format!(
            "{n} result{s} above {have} no recorded redistribution licence. If the human can say \
             whether the source may be redistributed, record it with set_licence(keys=[...], \
             licence='open'|'restricted'), or set_licence(feed_name=...) to classify every \
             article from one feed."
        ),
    }))
}

impl Engine {
    fn resolve_mode(&self, mode: Option<&str>) -> Result<String> {
        let mode = match mode {
            Some(m) => m.to_owned(),
            None => self.config.ingest_mode.trim().to_lowercase(),
        };
        if mode == "full" || mode == "raw" {
            Ok(mode)
        } else {
            Err(invalid(format!(
                "Invalid mode '{mode}'. Must be 'full' or 'raw'."
            )))
        }
    }

    pub fn version(&self) -> Value {
        json!({"version": env!("CARGO_PKG_VERSION")})
    }

    #[allow(clippy::too_many_arguments)]
    pub fn remember(
        &self,
        content: &str,
        project: Option<&str>,
        tags: Option<&[String]>,
        namespace: &str,
        force: bool,
        mode: Option<&str>,
        licence: Option<&str>,
        provenance: Option<&str>,
    ) -> Result<Value> {
        validate_namespace(namespace)?;
        validate_content(content)?;
        validate_project_name(project)?;
        validate_tags(tags)?;
        let mode = self.resolve_mode(mode)?;
        let licence_fields = licence_for_write(licence, namespace)?;
        let provenance_class = provenance_for_write(provenance, namespace)?;
        let ns: Namespace = namespace.parse().map_err(|e| invalid(format!("{e}")))?;
        let enrich_after = mode == "full" && namespace != "knowledge" && !force;

        let key = MemoryKey::generate(ns).to_string();
        let now = now_str();
        let vector = self.embed(content)?;

        if !force && let Some(dup) = self.check_duplicate(ns, &vector, project)? {
            info!(similarity = dup.similarity, existing = %dup.key, "duplicate detected");
            return Ok(json!({
                "status": "duplicate_found",
                "existing_key": dup.key,
                "existing_content": take_chars(&dup.content, 80),
                "similarity": round_to(dup.similarity, 4),
            }));
        }
        let contradiction = if force {
            None
        } else {
            self.check_contradiction_heuristic(ns, &vector, content, project)?
        };

        let mut fields = Fields::from([
            ("content".to_owned(), content.to_owned()),
            ("state".to_owned(), "active".to_owned()),
            ("surface_score".to_owned(), "1.0".to_owned()),
            ("experience_weight".to_owned(), "1.0".to_owned()),
            ("created_at".to_owned(), now.clone()),
            ("updated_at".to_owned(), now.clone()),
            ("tags".to_owned(), py_json(&json!(tags.unwrap_or_default()))),
            ("provenance".to_owned(), provenance_class.to_owned()),
        ]);
        fields.extend(licence_fields.clone());
        if let Some(p) = project.filter(|p| !p.is_empty()) {
            fields.insert("project".to_owned(), p.to_owned());
            if namespace == "project" {
                fields.insert("project_name".to_owned(), p.to_owned());
            }
        }
        self.store.upsert(&key, &fields, Some(&vector))?;
        info!(%key, namespace, "stored memory");

        let classification = classification_payload(&licence_fields, provenance_class);
        if enrich_after {
            self.store.enqueue_enrichment(&json!({
                "key": key,
                "namespace": namespace,
                "project": project,
                "tags": tags,
                "doc_id": null,
                "created_at": now,
                "classification": classification,
            }))?;
        }

        let mut result = Map::new();
        result.insert("key".into(), key.into());
        result.insert("namespace".into(), namespace.into());
        result.insert("licence".into(), licence_fields["licence"].as_str().into());
        result.insert("provenance".into(), provenance_class.into());
        if enrich_after {
            result.insert("enrichment".into(), "queued".into());
        }
        if let Some(c) = contradiction {
            result.insert(
                "contradiction_warning".into(),
                json!({
                    "existing_key": c.existing_key,
                    "existing_content": c.existing_content,
                    "similarity": round_to(c.similarity, 4),
                    "explanation": crate::contradiction::HEURISTIC_EXPLANATION,
                }),
            );
        }
        Ok(Value::Object(result))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn remember_document(
        &self,
        content: &str,
        chunk_strategy: &str,
        project: Option<&str>,
        tags: Option<&[String]>,
        namespace: &str,
        chunk_size: Option<i64>,
        mode: Option<&str>,
        licence: Option<&str>,
        provenance: Option<&str>,
    ) -> Result<Value> {
        validate_namespace(namespace)?;
        validate_content(content)?;
        validate_project_name(project)?;
        validate_tags(tags)?;
        let mode = self.resolve_mode(mode)?;
        let licence_fields = licence_for_write(licence, namespace)?;
        let provenance_class = provenance_for_write(provenance, namespace)?;
        if !VALID_STRATEGIES.contains(&chunk_strategy) {
            return Err(invalid(format!(
                "Invalid chunk_strategy '{chunk_strategy}'. Must be one of: {}",
                VALID_STRATEGIES.join(", ")
            )));
        }
        let ns: Namespace = namespace.parse().map_err(|e| invalid(format!("{e}")))?;

        let chunks = chunking::chunk(content, chunk_strategy, chunk_size)?;
        if chunks.is_empty() {
            return Ok(json!({"doc_id": null, "keys": [], "chunks_stored": 0}));
        }
        let doc_id = Ulid::generate().to_string();
        let now = now_str();
        let enrich_after = mode == "full" && namespace != "knowledge";
        let batch_mode = self.config.enrichment_batch_mode;

        let texts: Vec<String> = chunks
            .iter()
            .map(|c| take_chars(c, MAX_CONTENT_LENGTH))
            .collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let vectors = self.embed_many(&refs)?;

        let mut keys: Vec<String> = Vec::new();
        let mut skipped = 0;
        for (idx, (text, vector)) in texts.iter().zip(&vectors).enumerate() {
            if self.check_duplicate(ns, vector, project)?.is_some() {
                skipped += 1;
                continue;
            }
            let key = MemoryKey::generate(ns).to_string();
            let mut fields = Fields::from([
                ("content".to_owned(), text.clone()),
                ("state".to_owned(), "active".to_owned()),
                ("surface_score".to_owned(), "1.0".to_owned()),
                ("experience_weight".to_owned(), "1.0".to_owned()),
                ("created_at".to_owned(), now.clone()),
                ("updated_at".to_owned(), now.clone()),
                ("tags".to_owned(), py_json(&json!(tags.unwrap_or_default()))),
                ("doc_id".to_owned(), doc_id.clone()),
                ("chunk_index".to_owned(), idx.to_string()),
                ("chunk_strategy".to_owned(), chunk_strategy.to_owned()),
                ("provenance".to_owned(), provenance_class.to_owned()),
            ]);
            fields.extend(licence_fields.clone());
            if let Some(p) = project.filter(|p| !p.is_empty()) {
                fields.insert("project".to_owned(), p.to_owned());
            }
            self.store.upsert(&key, &fields, Some(vector))?;
            keys.push(key);
        }

        let classification = classification_payload(&licence_fields, provenance_class);
        if enrich_after && !keys.is_empty() {
            if batch_mode {
                self.store.enqueue_enrichment(&json!({
                    "key": keys[0],
                    "namespace": namespace,
                    "project": project,
                    "tags": tags,
                    "doc_id": doc_id,
                    "created_at": now,
                    "classification": classification,
                    "batch_mode": true,
                    "batch_content": take_chars(&chunks.join("\n\n"), 24_000),
                }))?;
            } else {
                for key in &keys {
                    self.store.enqueue_enrichment(&json!({
                        "key": key,
                        "namespace": namespace,
                        "project": project,
                        "tags": tags,
                        "doc_id": doc_id,
                        "created_at": now,
                        "classification": classification,
                    }))?;
                }
            }
        }
        info!(%doc_id, %mode, chunks = chunks.len(), stored = keys.len(), skipped, "stored document");

        let enrichment = match (enrich_after, batch_mode) {
            (true, true) => "batch_queued",
            (true, false) => "queued",
            _ => "none",
        };
        Ok(json!({
            "doc_id": doc_id,
            "keys": keys,
            "chunks_stored": keys.len(),
            "chunks_total": chunks.len(),
            "duplicates_skipped": skipped,
            "namespace": namespace,
            "mode": mode,
            "licence": licence_fields["licence"],
            "provenance": provenance_class,
            "enrichment": enrichment,
        }))
    }

    /// Project and domain filters to the project list recall takes, with the
    /// notice that reports a filter which could not be applied honestly.
    fn resolve_recall_scope(
        &self,
        project_filter: Option<&str>,
        domain_filter: Option<&DomainFilter>,
    ) -> Result<(Vec<String>, Option<Value>, bool)> {
        let project_filter = project_filter.filter(|p| !p.is_empty());
        validate_project_name(project_filter)?;
        let Some(domain_filter) = domain_filter.filter(|d| !d.is_empty()) else {
            return Ok((
                project_filter
                    .map(|p| vec![p.to_owned()])
                    .unwrap_or_default(),
                None,
                false,
            ));
        };
        let resolution = self.resolve_projects_for_domains(domain_filter.input())?;
        let mut projects = resolution.projects();

        if let Some(project) = project_filter {
            projects.retain(|p| p == project);
            if projects.is_empty() {
                let which = if resolution.requested.len() == 1 {
                    "this domain"
                } else {
                    "any of these domains"
                };
                return Ok((
                    Vec::new(),
                    Some(json!({
                        "result_type": "domain_filter_notice",
                        "domain_filter": resolution.requested,
                        "project_filter": project,
                        "applied": true,
                        "projects": [],
                        "note": format!("Project '{project}' does not declare {which}, so nothing matches both filters."),
                    })),
                    true,
                ));
            }
        }

        let mut notice = None;
        if resolution.fully_unmatched() {
            let which = if resolution.unmatched.len() == 1 {
                "this domain"
            } else {
                "any of these domains"
            };
            notice = Some(json!({
                "result_type": "domain_filter_notice",
                "domain_filter": resolution.requested,
                "unmatched_domains": resolution.unmatched,
                "applied": false,
                "note": format!(
                    "No project declares {which}, so the domain filter was not applied and these \
                     results span every project. Run compile_project_domains(project_name) or \
                     list_projects(domain=...) to see what is declared."
                ),
            }));
            projects.clear();
        } else if !resolution.unmatched.is_empty() {
            notice = Some(json!({
                "result_type": "domain_filter_notice",
                "domain_filter": resolution.requested,
                "unmatched_domains": resolution.unmatched,
                "applied": true,
                "projects": projects,
                "note": format!(
                    "Filtered on the domains that matched; no project declares {}.",
                    resolution.unmatched.join(", ")
                ),
            }));
        }
        Ok((projects, notice, false))
    }

    fn validate_recall_namespaces(namespaces: Option<&[String]>) -> Result<()> {
        for ns in namespaces.unwrap_or_default() {
            validate_namespace(ns)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn recall(
        &self,
        query: &str,
        top_k: i64,
        namespaces: Option<&[String]>,
        project_filter: Option<&str>,
        _expand_queries: Option<bool>,
        domain_filter: Option<&DomainFilter>,
    ) -> Result<Value> {
        let top_k = top_k.clamp(1, MAX_TOP_K);
        Self::validate_recall_namespaces(namespaces)?;
        let (projects, notice, empty_scope) =
            self.resolve_recall_scope(project_filter, domain_filter)?;
        if empty_scope {
            return Ok(Value::Array(notice.into_iter().collect()));
        }
        let results = self.recall_results(query, namespaces, Some(top_k), &projects, None)?;
        let mut output: Vec<Value> = notice.into_iter().collect();
        output.extend(results.iter().map(recall_entry));
        if let Some(n) = licence_notice(&output) {
            output.push(n);
        }
        Ok(Value::Array(output))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn recall_index(
        &self,
        query: &str,
        top_k: i64,
        namespaces: Option<&[String]>,
        project_filter: Option<&str>,
        snippet_length: i64,
        _expand_queries: Option<bool>,
        domain_filter: Option<&DomainFilter>,
    ) -> Result<Value> {
        let top_k = top_k.clamp(1, MAX_TOP_K);
        Self::validate_recall_namespaces(namespaces)?;
        let (projects, notice, empty_scope) =
            self.resolve_recall_scope(project_filter, domain_filter)?;
        if empty_scope {
            return Ok(json!({
                "results": [],
                "token_estimate": {"index": 0, "full": 0},
                "domain_filter": notice,
            }));
        }
        let snippet_length = snippet_length.clamp(50, 500) as usize;
        let results = self.recall_results(query, namespaces, Some(top_k), &projects, None)?;

        let mut output = Vec::new();
        let (mut full_tokens, mut index_tokens) = (0usize, 0usize);
        for r in &results {
            let length = r.content.chars().count();
            let estimate = length / 4;
            full_tokens += estimate;
            let mut snippet = take_chars(&r.content, snippet_length);
            if length > snippet_length {
                snippet.push_str("...");
            }
            let mut e = Map::new();
            e.insert("key".into(), r.key.as_str().into());
            e.insert("namespace".into(), r.namespace.as_str().into());
            e.insert("snippet".into(), snippet.as_str().into());
            e.insert("score".into(), json!(r.adjusted_score));
            e.insert("estimated_tokens".into(), estimate.into());
            if let Some(p) = nonempty(&r.project) {
                e.insert("project".into(), p.into());
            }
            if r.result_type != "memory" {
                e.insert("result_type".into(), r.result_type.into());
            }
            if r.weak_match {
                e.insert("weak_match".into(), true.into());
            }
            if tags_truthy(&r.tags) {
                e.insert("tags".into(), r.tags.clone());
            }
            if r.reinstate_candidate {
                e.insert("reinstate_candidate".into(), true.into());
            }
            if let Some(l) = nonempty(&r.licence) {
                e.insert("licence".into(), l.into());
            }
            if let Some(p) = nonempty(&r.provenance) {
                e.insert("provenance".into(), p.into());
            }
            index_tokens += snippet.chars().count() / 4 + 10;
            output.push(Value::Object(e));
        }

        let mut payload = Map::new();
        let notice_for_licence = licence_notice(&output);
        payload.insert("results".into(), Value::Array(output));
        payload.insert(
            "token_estimate".into(),
            json!({"index": index_tokens, "full": full_tokens}),
        );
        if let Some(n) = notice_for_licence {
            payload.insert("licence_notice".into(), n);
        }
        if let Some(n) = notice {
            payload.insert("domain_filter".into(), n);
        } else if let Some(d) = domain_filter.filter(|d| !d.is_empty() && !projects.is_empty()) {
            payload.insert(
                "domain_filter".into(),
                json!({"domain_filter": d.as_list(), "applied": true, "projects": projects}),
            );
        }
        Ok(Value::Object(payload))
    }

    pub fn recall_detail(&self, keys: &[String]) -> Result<Value> {
        let keys: Vec<String> = keys
            .iter()
            .take(MAX_TOP_K as usize)
            .filter(|k| k.starts_with("mem:"))
            .cloned()
            .collect();
        if keys.is_empty() {
            return Ok(json!([]));
        }
        let rows = self.store.get_multi(&keys)?;
        let mut output = Vec::new();
        for (key, row) in keys.iter().zip(rows) {
            let Some(data) = row else {
                output.push(json!({"key": key, "status": "not_found"}));
                continue;
            };
            let namespace = key.split(':').nth(1).unwrap_or("unknown");
            let mut e = Map::new();
            e.insert("key".into(), key.as_str().into());
            e.insert(
                "content".into(),
                data.get("content").map_or("", String::as_str).into(),
            );
            e.insert("namespace".into(), namespace.into());
            e.insert(
                "state".into(),
                data.get("state").map_or("active", String::as_str).into(),
            );
            if let Some(p) = data.get("project").filter(|p| !p.is_empty()) {
                e.insert("project".into(), p.as_str().into());
            }
            if let Some(tags) = data
                .get("tags")
                .filter(|t| !t.is_empty())
                .and_then(|t| serde_json::from_str::<Value>(t).ok())
                .filter(tags_truthy)
            {
                e.insert("tags".into(), tags);
            }
            if let Some(url) = data.get("source_url").filter(|u| !u.is_empty()) {
                e.insert("source_url".into(), url.as_str().into());
            }
            e.extend(classification_fields(&data, namespace, Some(key)));
            for name in ["breakthrough", "lesson"] {
                if let Some(v) = data.get(name).filter(|v| !v.is_empty()) {
                    e.insert(name.into(), v.as_str().into());
                }
            }
            if let Some(effort) = data
                .get("effort_score")
                .filter(|v| !v.is_empty())
                .and_then(|v| v.trim().parse::<f64>().ok())
            {
                e.insert("effort_score".into(), (effort as i64).into());
            }
            if let Some(outcome) = data.get("outcome").filter(|v| !v.is_empty()) {
                e.insert("outcome".into(), outcome.as_str().into());
            }
            output.push(Value::Object(e));
        }
        Ok(Value::Array(output))
    }

    /// Recall with the default namespaces, for the key-or-query lifecycle tools.
    fn query_targets(&self, query: &str) -> Result<Vec<RecallResult>> {
        self.recall_results(query, None, Some(3), &[], None)
    }

    pub fn deprioritise(
        &self,
        key_or_query: &str,
        reason: &str,
        hints: Option<&[String]>,
    ) -> Result<Value> {
        let mut affected = Vec::new();
        let hints = hints.filter(|h| !h.is_empty());
        if key_or_query.starts_with("mem:") {
            affected.push(Value::Object(self.transition(
                key_or_query,
                MemoryState::Deprioritised,
                Some(reason),
            )?));
            if let Some(hints) = hints {
                self.add_reinstate_hints(key_or_query, hints)?;
            }
        } else {
            for r in self.query_targets(key_or_query)? {
                if r.adjusted_score > 0.85 && r.state == "active" {
                    affected.push(Value::Object(self.transition(
                        &r.key,
                        MemoryState::Deprioritised,
                        Some(reason),
                    )?));
                    if let Some(hints) = hints {
                        self.add_reinstate_hints(&r.key, hints)?;
                    }
                }
            }
        }
        Ok(json!({"affected": affected}))
    }

    pub fn archive(&self, key_or_query: &str, reason: Option<&str>) -> Result<Value> {
        let mut affected = Vec::new();
        if key_or_query.starts_with("mem:") {
            affected.push(Value::Object(self.transition(
                key_or_query,
                MemoryState::Archived,
                reason,
            )?));
        } else {
            for r in self.query_targets(key_or_query)? {
                if r.adjusted_score > 0.85 {
                    match self.transition(&r.key, MemoryState::Archived, reason) {
                        Ok(t) => affected.push(Value::Object(t)),
                        Err(EngineError::Invalid(e)) => {
                            warn!(key = %r.key, error = %e, "cannot archive")
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
        }
        Ok(json!({"affected": affected}))
    }

    fn clear_deprioritisation(&self, key: &str) -> Result<()> {
        let fields = Fields::from([
            ("deprioritised_reason".to_owned(), String::new()),
            ("surface_score".to_owned(), "1.0".to_owned()),
        ]);
        Ok(self.store.set_fields(key, &fields)?)
    }

    pub fn reinstate(&self, key_or_query: &str) -> Result<Value> {
        let mut affected = Vec::new();
        if key_or_query.starts_with("mem:") {
            let t = self.transition(key_or_query, MemoryState::Active, None)?;
            self.clear_deprioritisation(key_or_query)?;
            affected.push(Value::Object(t));
        } else {
            for r in self.query_targets(key_or_query)? {
                if matches!(r.state.as_str(), "deprioritised" | "archived") {
                    match self.transition(&r.key, MemoryState::Active, None) {
                        Ok(t) => {
                            self.clear_deprioritisation(&r.key)?;
                            affected.push(Value::Object(t));
                        }
                        Err(EngineError::Invalid(e)) => {
                            warn!(key = %r.key, error = %e, "cannot reinstate")
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
        }
        Ok(json!({"affected": affected}))
    }

    pub fn forget(&self, key_or_query: &str, confirm: bool) -> Result<Value> {
        let mut targets: Vec<(String, String)> = Vec::new();
        if key_or_query.starts_with("mem:") {
            if let Some(data) = self.store.get(key_or_query)?.filter(|d| !d.is_empty()) {
                let content = data.get("content").map_or("", String::as_str);
                targets.push((key_or_query.to_owned(), take_chars(content, 80)));
            }
        } else {
            for r in self.query_targets(key_or_query)? {
                if r.adjusted_score > 0.85 {
                    targets.push((r.key.clone(), take_chars(&r.content, 80)));
                }
            }
        }
        if targets.is_empty() {
            return Ok(json!({"status": "not_found"}));
        }
        if !confirm {
            let preview: Vec<Value> = targets
                .iter()
                .map(|(k, c)| json!({"key": k, "content": c}))
                .collect();
            return Ok(json!({"status": "preview", "targets": preview}));
        }
        let mut deleted = Vec::new();
        for (key, _) in &targets {
            if self.transition(key, MemoryState::Deleted, None).is_err() {
                self.store.delete(key)?;
            }
            deleted.push(key.clone());
        }
        self.invalidate_abandoned_cache();
        Ok(json!({"status": "deleted", "deleted_keys": deleted}))
    }

    pub fn suppress_topic(&self, topic: &str, reason: Option<&str>) -> Result<Value> {
        if topic.trim().is_empty() {
            return Err(invalid("Topic cannot be empty"));
        }
        if topic.chars().count() > 200 {
            return Err(invalid("Topic too long (max 200 characters)"));
        }
        self.store
            .set_add(SUPPRESSED_KEY, &[topic.to_lowercase()])?;
        info!(topic, "suppressed topic");
        let mut m = Map::new();
        m.insert("topic".into(), topic.into());
        m.insert("reason".into(), reason.map_or(Value::Null, Value::from));
        Ok(compact(m))
    }

    pub fn unsuppress_topic(&self, topic: &str) -> Result<Value> {
        self.store
            .set_remove(SUPPRESSED_KEY, &[topic.to_lowercase()])?;
        Ok(json!({"topic": topic}))
    }

    pub fn list_suppressions(&self) -> Result<Value> {
        Ok(json!({"suppressed_topics": self.suppressed_topics()?}))
    }

    pub fn find_duplicates(
        &self,
        namespace: &str,
        threshold: Option<f64>,
        project_filter: Option<&str>,
    ) -> Result<Value> {
        validate_namespace(namespace)?;
        let project_filter = project_filter.filter(|p| !p.is_empty());
        validate_project_name(project_filter)?;
        let ns: Namespace = namespace.parse().map_err(|e| invalid(format!("{e}")))?;
        let clusters = self.find_all_duplicates(ns, threshold, project_filter)?;
        Ok(json!({"namespace": namespace, "clusters": clusters}))
    }

    /// Store and model status. 6.x reported Valkey connectivity and index
    /// drift; there is no separate index to drift from the data any more.
    pub fn health(&self) -> Result<Value> {
        let mut records = Map::new();
        let mut vectors = Map::new();
        for (ns, count) in self.store.count_all_records()? {
            records.insert(ns.as_str().into(), count.into());
            vectors.insert(ns.as_str().into(), self.store.vector_count(ns).into());
        }
        Ok(json!({
            "database_connected": true,
            "records": records,
            "vectors": vectors,
            "model_loaded": true,
            "uptime_seconds": round_to(self.started.elapsed().as_secs_f64(), 1),
        }))
    }

    pub fn queue_status(&self) -> Result<Value> {
        Ok(json!({"pending": self.store.enrichment_pending()?}))
    }

    fn backup_path(&self, filename: &str) -> std::result::Result<PathBuf, String> {
        if filename.is_empty() {
            return Err("Filename cannot be empty".into());
        }
        if !SAFE_FILENAME_RE.is_match(filename) {
            return Err("Invalid filename. Only alphanumeric characters, underscores, hyphens, and dots are allowed, and the file must end in .json".into());
        }
        if filename.len() > 255 {
            return Err("Filename too long (max 255 characters)".into());
        }
        Ok(self.config.backup_dir.join(filename))
    }

    pub fn dump_to_file(&self, filename: Option<&str>) -> Result<Value> {
        let filename = filename.map(str::to_owned).unwrap_or_else(|| {
            format!(
                "memory_backup_{}.json",
                Local::now().format("%Y%m%d_%H%M%S")
            )
        });
        let path = match self.backup_path(&filename) {
            Ok(p) => p,
            Err(message) => return Ok(json!({"status": "error", "message": message})),
        };
        let dump = self.store.dump()?;
        if let Err(e) = omnimem_store::write_backup(&path, &dump) {
            warn!(error = %e, "failed to write backup file");
            return Ok(json!({"status": "error", "message": "Failed to write backup file"}));
        }
        info!(path = %path.display(), keys = dump.data.len(), "backup written");
        Ok(json!({
            "filename": filename,
            "path": path.display().to_string(),
            "total_keys": dump.data.len(),
        }))
    }

    pub fn restore_from_file(&self, filename: &str, dry_run: bool) -> Result<Value> {
        let error = |message: &str| Ok(json!({"status": "error", "message": message}));
        let path = match self.backup_path(filename) {
            Ok(p) => p,
            Err(message) => return error(&message),
        };
        let Ok(meta) = fs::metadata(&path) else {
            return error("Backup file not found");
        };
        if meta.len() > MAX_BACKUP_FILE_SIZE {
            return error(&format!(
                "Backup file too large ({} MB). Maximum is {} MB.",
                meta.len() / 1024 / 1024,
                MAX_BACKUP_FILE_SIZE / 1024 / 1024
            ));
        }
        let Ok(raw) = fs::read_to_string(&path) else {
            return error("Failed to read backup file");
        };
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            return error("Invalid JSON");
        };
        if !value.is_object() {
            return error("Invalid backup format");
        }
        if value.get("data").is_some_and(|d| !d.is_object()) {
            return error("Invalid backup data format");
        }
        let backup = BackupFile::from_value(value).unwrap_or(BackupFile {
            metadata: Value::Null,
            data: Map::new(),
        });
        if let Some(bad) = backup
            .data
            .keys()
            .find(|k| !BACKUP_PREFIXES.iter().any(|p| k.starts_with(p)))
        {
            return error(&format!(
                "Backup contains invalid key prefix: {}",
                take_chars(bad, 50)
            ));
        }
        if dry_run {
            let metadata = if backup.metadata.is_null() {
                json!({})
            } else {
                backup.metadata.clone()
            };
            return Ok(json!({
                "status": "dry_run",
                "total_keys_in_backup": backup.data.len(),
                "metadata": metadata,
            }));
        }
        let report =
            match self
                .store
                .restore_backup(&backup, Some(self.embedder.as_ref()), &mut |_, _| {})
            {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %e, "restore failed");
                    return error("Restore operation failed");
                }
            };
        self.invalidate_abandoned_cache();
        self.invalidate_domain_cache();
        Ok(json!({
            "status": "restored",
            "restored_keys": report.memories + report.other_records + report.sets,
            "skipped_keys": report.skipped_older + report.skipped_invalid + report.skipped_expired,
            "re_embedded": report.embedded,
            "filename": filename,
        }))
    }

    pub fn list_backups(&self) -> Result<Value> {
        let dir: &Path = &self.config.backup_dir;
        let Ok(entries) = fs::read_dir(dir) else {
            return Ok(json!({"backups": []}));
        };
        let mut backups: Vec<(String, Value)> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "json") && e.path().is_file())
            .filter_map(|e| {
                let meta = e.metadata().ok()?;
                let created = meta
                    .modified()
                    .ok()
                    .map(|t| {
                        DateTime::<Utc>::from(t)
                            .format("%Y-%m-%dT%H:%M:%SZ")
                            .to_string()
                    })
                    .unwrap_or_default();
                Some((
                    created.clone(),
                    json!({
                        "filename": e.file_name().to_string_lossy(),
                        "size_kb": round_to(meta.len() as f64 / 1024.0, 2),
                        "created_at": created,
                    }),
                ))
            })
            .collect();
        backups.sort_by(|a, b| b.0.cmp(&a.0));
        Ok(json!({"backups": backups.into_iter().map(|(_, v)| v).collect::<Vec<_>>()}))
    }
}

/// The `classification` object an enrichment job carries: licence fields
/// then provenance, as 6.x's payload dict was ordered.
fn classification_payload(licence: &Fields, provenance: &str) -> Value {
    let mut m = Map::new();
    m.insert("licence".into(), licence["licence"].as_str().into());
    if let Some(note) = licence.get("licence_note") {
        m.insert("licence_note".into(), note.as_str().into());
    }
    m.insert("provenance".into(), provenance.into());
    Value::Object(m)
}
