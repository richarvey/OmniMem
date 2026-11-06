//! The gated compile-to-skill flow (`memory/skill_compiler.py`).
//!
//! Experience and graveyard writes flow freely, but nothing writes to a skill
//! silently: a propose surfaces a diff, a human accepts it, and a write
//! commits exactly the draft that was reviewed. The MCP tool and the web UI
//! both run this one flow.

use omnimem_store::{AuditLevel, Op, op as audit_op};
use std::path::PathBuf;
use std::sync::LazyLock;

use omnimem_store::Fields;
use regex::Regex;
use serde_json::{Map, Value, json};
use tracing::{error, info};

use crate::domains::{is_valid_domain, normalise_domain, resolve_domain};
use crate::error::invalid;
use crate::feeds::feeds_for_domain;
use crate::pyfmt::{compact, now_secs, now_str, py_float, py_json};
use crate::skills::{
    CONTRACT_VERSION, INVALID_DOMAIN, PoolMemory, Rule, bodies_equivalent, body_sha,
    build_feed_rules, build_reference_rules, draft_description, extract_lessons,
    generated_skill_key, render_skill_md, render_unified_diff, summarise_rule_changes,
};
use crate::tools::{MAX_SHORT_TEXT, validate_text};
use crate::{Engine, Result};

#[cfg(test)]
#[path = "skills_golden_tests.rs"]
mod golden_tests;

const MAX_SKILL_BODY: usize = 100_000;

/// A cap on held-back rule text for pathological rules only (#32).
const HELD_BACK_MAX_CHARS: usize = 500;

static SAFE_EXPORT_SEGMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9_][a-zA-Z0-9_.\-]*$").expect("valid"));

const PROPOSAL_PREFIX: &str = "meta:skill:proposal:";

pub(crate) fn proposal_key(domain: &str, user: &str) -> String {
    format!("{PROPOSAL_PREFIX}{domain}-{user}")
}

/// A stashed proposal as a person sees it: enough to decide on, and a
/// fingerprint so an approval can only commit the draft that was shown.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillReview {
    pub domain: String,
    pub skill_id: String,
    /// No skill is stored for this domain yet, so `shown` is the whole draft.
    pub new_skill: bool,
    pub description: String,
    /// The unified diff against the stored skill, or the whole draft for a new one.
    pub shown: String,
    /// Lines the diff adds and removes (a new skill's lines all count as added).
    pub added: usize,
    pub removed: usize,
    /// Rule counts by kind, from the proposal's manifest.
    pub rules: Value,
    /// When the proposal was made (seconds since the epoch), 0 if unknown.
    pub created_at: f64,
    /// SHA-256 of the proposed body: what an approval has to name.
    pub body_sha: String,
    /// The stored skill changed after this was proposed, so a commit would be refused.
    pub stale: bool,
}

/// What a write would face right now.
#[derive(Debug, Clone, PartialEq)]
pub enum ProposalCheck {
    /// A proposal waits to be accepted.
    Ready(SkillReview),
    /// A write would end here, with this result (no proposal, stale, refused).
    Blocked(Value),
}

fn review_of(stash: &Fields, skill_id: &str, existing: Option<&Fields>) -> SkillReview {
    let text = |name: &str| stash.get(name).cloned().unwrap_or_default();
    let body = text("body");
    let existing_body = existing
        .and_then(|e| e.get("body"))
        .filter(|b| !b.is_empty())
        .cloned()
        .unwrap_or_default();
    let new_skill = existing_body.is_empty();
    let (shown, added, removed) = if new_skill {
        (body.clone(), body.lines().count(), 0)
    } else {
        let diff = render_unified_diff(&existing_body, &body, skill_id);
        let counted = |sign: char, header: &str| {
            diff.lines()
                .filter(|l| l.starts_with(sign) && !l.starts_with(header))
                .count()
        };
        let (added, removed) = (counted('+', "+++"), counted('-', "---"));
        (diff, added, removed)
    };
    let manifest: Vec<Value> = serde_json::from_str::<Value>(&text("rule_manifest"))
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    let mut rules = Map::new();
    for rule in &manifest {
        let kind = rule.get("kind").and_then(Value::as_str).unwrap_or("rule");
        let n = rules.get(kind).and_then(Value::as_i64).unwrap_or(0) + 1;
        rules.insert(kind.to_owned(), n.into());
    }
    let current_sha = if new_skill {
        String::new()
    } else {
        body_sha(&existing_body)
    };
    SkillReview {
        domain: text("domain"),
        skill_id: skill_id.to_owned(),
        new_skill,
        description: text("description"),
        shown,
        added,
        removed,
        rules: Value::Object(rules),
        created_at: text("created_at").trim().parse().unwrap_or(0.0),
        body_sha: body_sha(&body),
        stale: text("based_on") != current_sha,
    }
}

/// A stored object that isn't the compiler's own output is never overwritten.
fn refusal_for(existing: Option<&Fields>, skill_id: &str) -> Option<Value> {
    let existing = existing?;
    (existing.get("generated").map(String::as_str) != Some("true")).then(|| {
        json!({
            "status": "refused",
            "skill_id": skill_id,
            "reason": "Existing object is not flagged generated:true — the compiler only overwrites its own output.",
        })
    })
}

fn no_proposal(domain: &str) -> Value {
    json!({
        "status": "no_proposal",
        "domain": domain,
        "note": "Nothing proposed for this domain (or the proposal expired). Run compile_skill(mode='propose') and review the diff first — write only commits an accepted proposal.",
    })
}

fn stale_proposal(skill_id: &str) -> Value {
    json!({
        "status": "stale_proposal",
        "skill_id": skill_id,
        "note": "The stored skill changed after this diff was proposed. Re-run compile_skill(mode='propose') and review again.",
    })
}

fn rule_counts(rules: &[Rule]) -> Value {
    let mut counts = Map::new();
    for rule in rules {
        let n = counts.get(rule.kind).and_then(Value::as_i64).unwrap_or(0) + 1;
        counts.insert(rule.kind.to_owned(), n.into());
    }
    Value::Object(counts)
}

/// Rule text for review, trimmed on a word boundary only if pathologically long.
pub(crate) fn held_back_text(text: &str) -> (String, bool) {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= HELD_BACK_MAX_CHARS {
        return (text.to_owned(), false);
    }
    let mut cut = chars[..HELD_BACK_MAX_CHARS].to_vec();
    if let Some(boundary) = cut.iter().rposition(|c| *c == ' ')
        && boundary > HELD_BACK_MAX_CHARS / 2
    {
        cut.truncate(boundary);
    }
    let cut: String = cut.into_iter().collect();
    (format!("{}…", cut.trim_end()), true)
}

/// How much of a domain's pool comes from its busiest project (#33).
pub(crate) fn pool_concentration(pool: &[PoolMemory]) -> Option<Value> {
    if pool.is_empty() {
        return None;
    }
    let mut counts: Vec<(String, usize)> = Vec::new();
    for item in pool {
        let name = item.project.as_deref().unwrap_or("").trim();
        if name.is_empty() {
            continue;
        }
        match counts.iter_mut().find(|(n, _)| n == name) {
            Some(slot) => slot.1 += 1,
            None => counts.push((name.to_owned(), 1)),
        }
    }
    let (top, count) = counts
        .iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)))?;
    Some(json!({
        "projects": counts.len(),
        "top_project": top,
        "top_project_share": format!("{count}/{}", pool.len()),
    }))
}

/// Why nothing compiled, pointing at the lever that helps. Deliberately
/// never suggests lowering `min_reinforcement` (#33).
pub(crate) fn insufficient_note(domain: &str, pool: &[PoolMemory], held_back: &[Rule]) -> String {
    let mut parts = vec![
        "No lesson recurs across enough memories to earn a rule. A single episode is a memory; a \
         pattern earns a skill rule."
            .to_owned(),
    ];
    if !held_back.is_empty() && held_back.iter().all(|r| r.reinforcement <= 1) {
        parts.push(format!(
            "All {} candidates appear exactly once, so nothing was close to the gate.",
            held_back.len()
        ));
    }
    if let Some(concentration) = pool_concentration(pool)
        && concentration["projects"] == 1
    {
        parts.push(format!(
            "Every candidate comes from one project ({}), so '{domain}' and that project's history \
             are the same set of memories here. A narrower domain that spans real work in several \
             projects is more likely to clear the gate than a broad one.",
            concentration["top_project"].as_str().unwrap_or("")
        ));
    }
    parts.push(
        "Read the held_back text: candidates written as narrative of what happened can never \
         become rules however often they recur, while a generalisable claim can. bless() the ones \
         that already read as rules."
            .to_owned(),
    );
    parts.join(" ")
}

pub(crate) fn held_back_preview(held_back: &[Rule], limit: usize) -> Vec<Value> {
    held_back
        .iter()
        .take(limit)
        .map(|rule| {
            let (text, truncated) = match rule.name.as_deref().filter(|n| !n.is_empty()) {
                Some(name) if rule.kind == "dont" => (format!("Avoid {name}"), false),
                _ => held_back_text(&rule.text),
            };
            let mut m = Map::new();
            m.insert("kind".into(), rule.kind.into());
            m.insert("rule".into(), text.into());
            if truncated {
                m.insert("truncated".into(), true.into());
            }
            m.insert("reinforcement".into(), rule.reinforcement.into());
            m.insert("sources".into(), json!(rule.sources));
            compact(m)
        })
        .collect()
}

impl Engine {
    /// `export_path` resolved inside `SKILL_EXPORT_DIR`, or why not. Segments
    /// can't start with a dot, so `..` can't appear and the join stays inside.
    pub(crate) fn safe_export_path(
        &self,
        export_path: &str,
    ) -> std::result::Result<PathBuf, String> {
        let export_path = export_path.trim();
        if export_path.is_empty() {
            return Err("export_path cannot be empty".to_owned());
        }
        let root = &self.config.skill_export_dir;
        if export_path.starts_with('/')
            || export_path.starts_with('~')
            || std::path::Path::new(export_path).is_absolute()
        {
            return Err(format!(
                "export_path must be relative — it is written inside SKILL_EXPORT_DIR ({})",
                root.display()
            ));
        }
        let normalised = export_path.replace('\\', "/");
        let segments: Vec<&str> = normalised.split('/').filter(|s| !s.is_empty()).collect();
        if segments.is_empty() {
            return Err("export_path cannot be empty".to_owned());
        }
        if segments.iter().any(|s| !SAFE_EXPORT_SEGMENT.is_match(s)) {
            return Err(
                "Invalid export_path segment. Use alphanumerics, underscores, hyphens, and dots \
                 (no leading dot); e.g. 'python-ric/SKILL.md'"
                    .to_owned(),
            );
        }
        if !segments.last().is_some_and(|s| s.ends_with(".md")) {
            return Err("export_path must end in .md".to_owned());
        }
        Ok(segments.iter().fold(root.clone(), |path, s| path.join(s)))
    }

    /// `compile_skill`: propose a reviewable draft, or write the one proposed.
    pub fn compile_skill(
        &self,
        domain: &str,
        mode: &str,
        min_reinforcement: i64,
        include_graveyard: bool,
        export_path: Option<&str>,
        description: Option<&str>,
    ) -> Result<Value> {
        if mode != "propose" && mode != "write" {
            return Err(invalid(format!(
                "mode must be 'propose' or 'write', got '{mode}'"
            )));
        }
        if let Some(description) = description {
            validate_text("description", description, MAX_SHORT_TEXT)?;
        }
        let min_reinforcement = min_reinforcement.clamp(1, 10) as usize;
        let (canonical, aliased) = resolve_domain(domain);
        if !is_valid_domain(&canonical) {
            return Err(invalid(INVALID_DOMAIN));
        }
        let user = self.config.skill_user.clone();
        let skill_id = generated_skill_key(&canonical, &user);

        let existing = self.store.get(&skill_id)?;
        if let Some(refused) = refusal_for(existing.as_ref(), &skill_id) {
            return Ok(refused);
        }

        if mode == "write" {
            return self.commit_proposal(
                &canonical,
                &user,
                &skill_id,
                existing.as_ref(),
                export_path,
                None,
            );
        }
        let aliased_from = aliased.then(|| normalise_domain(domain));
        self.propose_skill(
            &canonical,
            &user,
            &skill_id,
            existing.as_ref(),
            min_reinforcement,
            include_graveyard,
            description,
            aliased_from,
        )
    }

    /// What a write for `domain` would meet right now: the proposal to put in
    /// front of a person, or the result a write would end with. Writes
    /// nothing. The MCP layer reads this before asking for approval.
    pub fn review_skill_proposal(&self, domain: &str) -> Result<ProposalCheck> {
        let (canonical, _) = resolve_domain(domain);
        if !is_valid_domain(&canonical) {
            return Err(invalid(INVALID_DOMAIN));
        }
        let user = self.config.skill_user.clone();
        let skill_id = generated_skill_key(&canonical, &user);
        let existing = self.store.get(&skill_id)?;
        if let Some(refused) = refusal_for(existing.as_ref(), &skill_id) {
            return Ok(ProposalCheck::Blocked(refused));
        }
        let Some(stash) = self.live_proposal(&canonical, &user)? else {
            return Ok(ProposalCheck::Blocked(no_proposal(&canonical)));
        };
        let review = review_of(&stash, &skill_id, existing.as_ref());
        if review.stale {
            return Ok(ProposalCheck::Blocked(stale_proposal(&skill_id)));
        }
        Ok(ProposalCheck::Ready(review))
    }

    /// Commit the proposal a person has approved: `compile_skill(mode="write")`
    /// for a caller that has already got the approval, which it proves by
    /// naming the body it showed (`reviewed_sha`). If the proposal was
    /// replaced while the person was deciding, nothing is written.
    ///
    /// The engine doesn't know who approved; the MCP layer calls this only
    /// after elicitation. The settings panel uses plain
    /// `compile_skill(mode="write")`, since a click there is the approval.
    pub fn commit_approved_skill(
        &self,
        domain: &str,
        export_path: Option<&str>,
        reviewed_sha: &str,
    ) -> Result<Value> {
        let (canonical, _) = resolve_domain(domain);
        if !is_valid_domain(&canonical) {
            return Err(invalid(INVALID_DOMAIN));
        }
        let user = self.config.skill_user.clone();
        let skill_id = generated_skill_key(&canonical, &user);
        let existing = self.store.get(&skill_id)?;
        if let Some(refused) = refusal_for(existing.as_ref(), &skill_id) {
            return Ok(refused);
        }
        self.commit_proposal(
            &canonical,
            &user,
            &skill_id,
            existing.as_ref(),
            export_path,
            Some(reviewed_sha),
        )
    }

    /// Every stashed proposal, any domain and whoever made it, oldest first.
    /// Proposals live under a TTL, so what is listed can still be committed
    /// unless it has gone stale.
    pub fn pending_skill_proposals(&self) -> Result<Vec<SkillReview>> {
        let mut reviews = Vec::new();
        for key in self.store.scan_prefix(PROPOSAL_PREFIX)? {
            let Some(stash) = self
                .store
                .hash_get_all(&key)?
                .filter(|s| s.get("body").is_some_and(|b| !b.is_empty()))
            else {
                continue;
            };
            let domain = stash.get("domain").cloned().unwrap_or_default();
            let user = stash
                .get("user")
                .cloned()
                .unwrap_or_else(|| self.config.skill_user.clone());
            let skill_id = generated_skill_key(&domain, &user);
            let existing = self.store.get(&skill_id)?;
            reviews.push(review_of(&stash, &skill_id, existing.as_ref()));
        }
        reviews.sort_by(|a, b| a.created_at.total_cmp(&b.created_at));
        Ok(reviews)
    }

    /// Throw a stashed proposal away. True if there was one.
    pub fn discard_skill_proposal(&self, domain: &str) -> Result<bool> {
        let (canonical, _) = resolve_domain(domain);
        if !is_valid_domain(&canonical) {
            return Err(invalid(INVALID_DOMAIN));
        }
        let key = proposal_key(&canonical, &self.config.skill_user);
        Ok(self.store.kv_delete(&key)?)
    }

    /// The stash for a domain, if it still holds a draft.
    fn live_proposal(&self, domain: &str, user: &str) -> Result<Option<Fields>> {
        Ok(self
            .store
            .hash_get_all(&proposal_key(domain, user))?
            .filter(|s| s.get("body").is_some_and(|b| !b.is_empty())))
    }

    #[allow(clippy::too_many_arguments)]
    fn propose_skill(
        &self,
        domain: &str,
        user: &str,
        skill_id: &str,
        existing: Option<&Fields>,
        min_reinforcement: usize,
        include_graveyard: bool,
        description_override: Option<&str>,
        aliased_from: Option<String>,
    ) -> Result<Value> {
        let domains = [domain.to_owned()];
        let pool = self
            .gather_domain_pools(&domains)?
            .remove(domain)
            .unwrap_or_default();
        let promoted = self
            .gather_promoted_knowledge(&domains)?
            .remove(domain)
            .unwrap_or_default();
        let ref_rules = build_reference_rules(&promoted);

        // Feeds supplement a skill but never bootstrap one.
        let domain_feeds = feeds_for_domain(&self.load_feed_influences(), domain);
        let feed_rules = if domain_feeds.is_empty() {
            Vec::new()
        } else {
            build_feed_rules(&self.gather_feed_knowledge(domain, &domain_feeds)?)
        };

        if pool.is_empty() && ref_rules.is_empty() {
            let known = self.known_domains()?;
            let names: Vec<String> = known.iter().map(|(d, _)| d.clone()).collect();
            let suggestion = self.suggest_similar_domain(domain, &names)?;
            let mut top = known;
            top.sort_by_key(|t| std::cmp::Reverse(t.1));
            let mut m = Map::new();
            m.insert("status".into(), "no_candidates".into());
            m.insert("domain".into(), domain.into());
            m.insert(
                "did_you_mean".into(),
                suggestion.map_or(Value::Null, |(d, s)| json!({"domain": d, "similarity": s})),
            );
            m.insert(
                "known_domains".into(),
                top.into_iter()
                    .take(10)
                    .map(|(d, n)| json!({"domain": d, "memories": n}))
                    .collect(),
            );
            m.insert(
                "note".into(),
                "No active episodic memories are tagged with this domain and no knowledge is \
                 promoted to it. Domains are tags — tag memories at remember() time, or \
                 promote_knowledge(key, domain=...) to feed reference material in."
                    .into(),
            );
            return Ok(compact(m));
        }

        let lessons = extract_lessons(&pool, include_graveyard);
        if lessons.is_empty() && ref_rules.is_empty() {
            return Ok(json!({
                "status": "no_lessons",
                "domain": domain,
                "pool_size": pool.len(),
                "note": "Memories exist for this domain but none carry lessons yet. record_experience() lessons/gotchas, log_abandoned() dead ends, or bless() a memory to make it skill-eligible.",
            }));
        }

        let (mut eligible, held_back) = self.build_rules(&lessons, min_reinforcement)?;
        if eligible.is_empty() && ref_rules.is_empty() {
            let mut m = Map::new();
            m.insert("status".into(), "insufficient_reinforcement".into());
            m.insert("domain".into(), domain.into());
            m.insert("pool_size".into(), pool.len().into());
            m.insert("min_reinforcement".into(), min_reinforcement.into());
            m.insert(
                "pool_concentration".into(),
                pool_concentration(&pool).unwrap_or(Value::Null),
            );
            m.insert("held_back".into(), held_back_preview(&held_back, 10).into());
            m.insert(
                "note".into(),
                insufficient_note(domain, &pool, &held_back).into(),
            );
            return Ok(compact(m));
        }
        // Promotion and feed association are the vetting, so these join
        // after the gate and neither need nor consume reinforcement.
        eligible.extend(ref_rules);
        eligible.extend(feed_rules);

        // The description is human-owned and pinned once stored.
        let existing_description = existing
            .and_then(|e| e.get("description"))
            .cloned()
            .unwrap_or_default();
        let skill_description = match description_override
            .map(str::trim)
            .filter(|d| !d.is_empty())
        {
            Some(d) => d.to_owned(),
            None if !existing_description.is_empty() => existing_description.clone(),
            None => draft_description(domain, user),
        };
        let description_pinned = !existing_description.is_empty() && description_override.is_none();

        let now = now_secs();
        let body = render_skill_md(
            domain,
            user,
            &skill_description,
            &eligible,
            now,
            min_reinforcement,
        );
        let body_len = body.chars().count();
        if body_len > MAX_SKILL_BODY {
            return Ok(json!({
                "status": "error",
                "reason": format!("Compiled body too large ({body_len} chars, max {MAX_SKILL_BODY})."),
            }));
        }

        let existing_body = existing
            .and_then(|e| e.get("body"))
            .cloned()
            .unwrap_or_default();
        if !existing_body.is_empty() && bodies_equivalent(&existing_body, &body) {
            return Ok(json!({
                "status": "unchanged",
                "skill_id": skill_id,
                "domain": domain,
                "note": "Compiled output matches the stored skill — nothing to propose.",
            }));
        }

        let new_manifest: Vec<Value> = eligible.iter().map(Rule::to_value).collect();
        let old_manifest: Vec<Value> = existing
            .and_then(|e| e.get("rule_manifest"))
            .and_then(|m| serde_json::from_str::<Value>(m).ok())
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default();
        let changes = summarise_rule_changes(&old_manifest, &new_manifest);

        // Stash the proposal so write commits exactly what was reviewed.
        let sources: std::collections::BTreeSet<&String> =
            eligible.iter().flat_map(|r| r.sources.iter()).collect();
        let proposal = Fields::from([
            ("body".to_owned(), body.clone()),
            ("description".to_owned(), skill_description.clone()),
            ("domain".to_owned(), domain.to_owned()),
            ("user".to_owned(), user.to_owned()),
            (
                "based_on".to_owned(),
                if existing_body.is_empty() {
                    String::new()
                } else {
                    body_sha(&existing_body)
                },
            ),
            ("created_at".to_owned(), py_float(now)),
            (
                "min_reinforcement".to_owned(),
                min_reinforcement.to_string(),
            ),
            (
                "rule_manifest".to_owned(),
                py_json(&Value::Array(new_manifest)),
            ),
            ("source_manifest".to_owned(), py_json(&json!(sources))),
        ]);
        let key = proposal_key(domain, user);
        let ttl = self.config.skill_proposal_ttl;
        self.store.hash_set(&key, &proposal)?;
        self.store.expire(&key, ttl)?;

        let is_new = existing.is_none();
        let mut result = Map::new();
        result.insert("status".into(), "proposal".into());
        result.insert("skill_id".into(), skill_id.into());
        result.insert("domain".into(), domain.into());
        result.insert("new_skill".into(), is_new.into());
        result.insert("description".into(), skill_description.into());
        result.insert("description_pinned".into(), description_pinned.into());
        result.insert("rules".into(), rule_counts(&eligible));
        result.insert("changes".into(), changes.into());
        result.insert(
            "note".into(),
            format!(
                "Review, then commit with compile_skill(domain='{domain}', mode='write'). Proposal expires in {}s.",
                ttl.as_secs()
            )
            .into(),
        );
        if let Some(from) = aliased_from {
            result.insert("domain_resolved_from".into(), from.into());
        }
        if is_new {
            result.insert("draft".into(), body.into());
        } else {
            result.insert(
                "diff".into(),
                render_unified_diff(&existing_body, &body, skill_id).into(),
            );
        }
        if !held_back.is_empty() {
            result.insert("held_back".into(), held_back_preview(&held_back, 10).into());
        }
        Ok(compact(result))
    }

    fn commit_proposal(
        &self,
        domain: &str,
        user: &str,
        skill_id: &str,
        existing: Option<&Fields>,
        export_path: Option<&str>,
        reviewed_sha: Option<&str>,
    ) -> Result<Value> {
        let key = proposal_key(domain, user);
        let Some(stash) = self.live_proposal(domain, user)? else {
            return Ok(no_proposal(domain));
        };

        let existing_body = existing
            .and_then(|e| e.get("body"))
            .filter(|b| !b.is_empty());
        let current_sha = existing_body.map(|b| body_sha(b)).unwrap_or_default();
        if stash.get("based_on").map_or("", String::as_str) != current_sha {
            return Ok(stale_proposal(skill_id));
        }
        if let Some(reviewed) = reviewed_sha
            && stash.get("body").map(|b| body_sha(b)).as_deref() != Some(reviewed)
        {
            return Ok(json!({
                "status": "proposal_changed",
                "skill_id": skill_id,
                "note": "The proposal was replaced after the person was shown it, so nothing was written. Re-run compile_skill(mode='propose') and ask again.",
            }));
        }

        let now = now_str();
        let stashed = |name: &str, default: &str| {
            stash
                .get(name)
                .cloned()
                .unwrap_or_else(|| default.to_owned())
        };
        let body = stashed("body", "");
        let description = stashed("description", "");
        let name = format!("{domain}-{user}");
        let fields = Fields::from([
            ("name".to_owned(), name.clone()),
            ("description".to_owned(), description.clone()),
            ("domain".to_owned(), domain.to_owned()),
            ("user".to_owned(), user.to_owned()),
            ("body".to_owned(), body.clone()),
            ("generated".to_owned(), "true".to_owned()),
            ("state".to_owned(), "active".to_owned()),
            ("surface_score".to_owned(), "1.0".to_owned()),
            ("contract_version".to_owned(), CONTRACT_VERSION.to_string()),
            ("compiled_at".to_owned(), stashed("created_at", &now)),
            (
                "created_at".to_owned(),
                existing
                    .and_then(|e| e.get("created_at"))
                    .filter(|c| !c.is_empty())
                    .cloned()
                    .unwrap_or_else(|| now.clone()),
            ),
            ("updated_at".to_owned(), now.clone()),
            ("tags".to_owned(), py_json(&json!([domain]))),
            (
                "source_manifest".to_owned(),
                stashed("source_manifest", "[]"),
            ),
            ("rule_manifest".to_owned(), stashed("rule_manifest", "[]")),
        ]);
        let vector = self.embed(&omnimem_store::discovery_text(&name, &description, domain))?;
        {
            let _audit = audit_op(Op::new("skill_write").at(AuditLevel::Lifecycle));
            self.store.upsert(skill_id, &fields, Some(&vector))?;
        }
        self.store.kv_delete(&key)?;
        info!(skill_id, chars = body.chars().count(), "committed skill");

        let mut result = Map::new();
        result.insert("status".into(), "written".into());
        result.insert("skill_id".into(), skill_id.into());
        result.insert("domain".into(), domain.into());
        result.insert("description".into(), description.into());
        result.insert("new_skill".into(), existing.is_none().into());
        if let Some(export_path) = export_path.filter(|p| !p.is_empty()) {
            match self.safe_export_path(export_path) {
                Err(e) => {
                    result.insert("export_error".into(), e.into());
                }
                Ok(path) => {
                    let written = path
                        .parent()
                        .map_or(Ok(()), std::fs::create_dir_all)
                        .and_then(|()| std::fs::write(&path, &body));
                    match written {
                        Ok(()) => {
                            result.insert("exported_to".into(), path.display().to_string().into());
                        }
                        Err(e) => {
                            error!(error = %e, "skill export failed");
                            result.insert(
                                "export_error".into(),
                                "Failed to write export file".into(),
                            );
                        }
                    }
                }
            }
        }
        Ok(compact(result))
    }
}
