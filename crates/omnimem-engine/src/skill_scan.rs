//! The auto skill scan (`memory/skill_scan.py`).
//!
//! Time-gated inside `briefing()`, it proposes skills for domains whose
//! lessons already clear the gate (by default only when a rule spans more
//! than one project) and drafts for skills with pending changes. Everything
//! it produces is a proposal stash, so the propose-and-accept gate is
//! untouched. A per-domain "seen" marker holds the sha of the last
//! auto-proposed body, so a draft left to expire is not proposed again until
//! the compiled output changes: ignoring a proposal declines it.

use std::collections::HashSet;

use serde_json::{Map, Value, json};
use tracing::info;

use crate::compiler::proposal_key;
use crate::domains::is_valid_domain;
use crate::pyfmt::{now_secs, py_float};
use crate::skills::{
    GENERATED_SKILL_PREFIX, body_sha, extract_lessons, lesson_bearing, strip_volatile,
};
use crate::{Engine, Result};

const LAST_RUN_KEY: &str = "meta:skill_scan:last_run";
const SEEN_PREFIX: &str = "meta:skill_scan:seen:";
const PROPOSAL_PREFIX: &str = "meta:skill:proposal:";

/// Candidates are capped, largest pools first, to bound the work.
const MAX_CANDIDATES: usize = 10;

/// What one project declaring a domain adds to its candidate ranking.
const PROJECT_DOMAIN_WEIGHT: usize = 2;

impl Engine {
    /// True when the time gate has elapsed.
    pub(crate) fn skill_scan_due(&self) -> Result<bool> {
        let hours = self.config.skill_scan_interval_hours;
        if hours <= 0.0 {
            return Ok(false);
        }
        if let Some(last) = self
            .store
            .string_get(LAST_RUN_KEY)?
            .and_then(|raw| raw.trim().parse::<f64>().ok())
            && now_secs() - last < hours * 3600.0
        {
            return Ok(false);
        }
        Ok(true)
    }

    /// Domains with a live proposal, human or auto: hands off either way.
    fn pending_proposal_domains(&self) -> Result<HashSet<String>> {
        let mut domains = HashSet::new();
        for key in self.store.scan_prefix(PROPOSAL_PREFIX)? {
            if let Some(domain) = self
                .store
                .hash_get_all(&key)?
                .and_then(|f| f.get("domain").cloned())
                .filter(|d| !d.is_empty())
            {
                domains.insert(domain);
            }
        }
        Ok(domains)
    }

    fn existing_skill_domains(&self) -> Result<HashSet<String>> {
        let keys = self.store.scan_prefix(GENERATED_SKILL_PREFIX)?;
        Ok(self
            .store
            .get_fields_multi(&keys, &["domain"])?
            .into_iter()
            .flatten()
            .filter_map(|r| r.get("domain").cloned().filter(|d| !d.is_empty()))
            .collect())
    }

    /// Run the shared propose flow and apply the seen-sha noise gate.
    fn scan_propose(&self, domain: &str, new_skill: bool) -> Result<Option<Value>> {
        let result = self.compile_skill(domain, "propose", 2, true, None, None)?;
        if result["status"] != "proposal" {
            return Ok(None);
        }
        let user = &self.config.skill_user;
        let key = proposal_key(domain, user);
        let body = self
            .store
            .hash_get_all(&key)?
            .and_then(|s| s.get("body").cloned())
            .unwrap_or_default();
        let sha = body_sha(&strip_volatile(&body));
        let seen_key = format!("{SEEN_PREFIX}{domain}-{user}");
        if self.store.string_get(&seen_key)?.as_deref() == Some(sha.as_str()) {
            // The same draft the human already let expire: withdraw it.
            self.store.kv_delete(&key)?;
            return Ok(None);
        }
        self.store.string_set(&seen_key, &sha, None)?;

        let mut entry = Map::new();
        entry.insert("domain".into(), domain.into());
        entry.insert("skill_id".into(), result["skill_id"].clone());
        entry.insert("new_skill".into(), new_skill.into());
        entry.insert(
            "review".into(),
            format!(
                "compile_skill(domain='{domain}', mode='propose') to see the {}, mode='write' to accept",
                if new_skill { "draft" } else { "diff" }
            )
            .into(),
        );
        if let Some(rules) = result
            .get("rules")
            .filter(|r| r.as_object().is_some_and(|o| !o.is_empty()))
        {
            entry.insert("rules".into(), rules.clone());
        }
        if !new_skill && let Some(changes) = result.get("changes") {
            entry.insert("changes".into(), changes.clone());
        }
        Ok(Some(Value::Object(entry)))
    }

    /// One scan pass. `update_domains` are skills the briefing already knows
    /// have pending changes.
    pub(crate) fn run_skill_scan(&self, update_domains: &[String]) -> Result<Value> {
        let now = now_secs();
        // Stamp first, so a failing scan waits out the interval.
        self.store.string_set(LAST_RUN_KEY, &py_float(now), None)?;

        let max_proposals = self.config.skill_scan_max_proposals.max(0) as usize;
        let min_pool = self.config.skill_scan_min_pool.max(1) as usize;
        let existing = self.existing_skill_domains()?;
        let pending = self.pending_proposal_domains()?;
        let mut proposals: Vec<Value> = Vec::new();
        let mut checked = 0;

        let mut counts = self.known_domains()?;
        // Boost, never introduce: a domain nothing is tagged with would be
        // dropped at the pool gate anyway.
        let project_domains = self.domain_map()?;
        for (domain, count) in &mut counts {
            if let Some(projects) = project_domains.get(domain) {
                *count += PROJECT_DOMAIN_WEIGHT * projects.len();
            }
        }
        counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let candidates: Vec<String> = counts
            .into_iter()
            .map(|(d, _)| d)
            .filter(|d| !existing.contains(d) && !pending.contains(d) && is_valid_domain(d))
            .take(MAX_CANDIDATES)
            .collect();

        let pools = self.gather_domain_pools(&candidates)?;
        for domain in &candidates {
            if proposals.len() >= max_proposals {
                break;
            }
            let pool = pools.get(domain).map(Vec::as_slice).unwrap_or_default();
            if pool.iter().filter(|m| lesson_bearing(m)).count() < min_pool {
                continue;
            }
            checked += 1;
            let (eligible, _) = self.build_rules(&extract_lessons(pool, true), 2)?;
            if eligible.is_empty() {
                continue;
            }
            if self.config.skill_scan_cross_project
                && !eligible.iter().any(|r| r.projects.len() >= 2)
            {
                continue;
            }
            if let Some(entry) = self.scan_propose(domain, true)? {
                proposals.push(entry);
            }
        }

        for domain in update_domains {
            if proposals.len() >= max_proposals {
                break;
            }
            if pending.contains(domain) || !existing.contains(domain) {
                continue;
            }
            if let Some(entry) = self.scan_propose(domain, false)? {
                proposals.push(entry);
            }
        }

        if !proposals.is_empty() {
            let domains: Vec<&str> = proposals
                .iter()
                .filter_map(|p| p["domain"].as_str())
                .collect();
            info!(count = proposals.len(), domains = %domains.join(", "), "skill scan proposed drafts");
        }
        Ok(json!({
            "ran_at": py_float(now),
            "proposals": proposals,
            "new_skill_candidates_checked": checked,
        }))
    }
}
