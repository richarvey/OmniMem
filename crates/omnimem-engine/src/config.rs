//! Tunables, with 6.x's environment variables and defaults.

use std::path::PathBuf;
use std::time::Duration;

use omnimem_store::AuditLevel;
use tracing::warn;

/// Whose dead ends may warn a recall or a hook (`GRAVEYARD_SCOPE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GraveyardScope {
    /// Only dead ends recorded in the projects the caller is working in (and
    /// those with no project at all). A caller with no project sees everything.
    #[default]
    Project,
    /// Every dead end, whichever project recorded it, as 6.x did.
    All,
}

impl GraveyardScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::All => "all",
        }
    }

    /// `project` or `all`, in any case. Anything else is not a scope.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "project" => Some(Self::Project),
            "all" => Some(Self::All),
            _ => None,
        }
    }
}

/// Who has to say yes before an agent's change takes effect (`HUMAN_APPROVAL`).
/// Read by every gate that has one: skill writes today, contradiction
/// resolution next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HumanApproval {
    /// A person accepts it: through the client (MCP elicitation) or the
    /// settings panel. The agent's say-so is never enough.
    #[default]
    Human,
    /// The agent's call is the approval, as 6.x behaved.
    Agent,
}

impl HumanApproval {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Agent => "agent",
        }
    }

    /// `human` or `agent`, in any case. Anything else is not a mode.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "human" => Some(Self::Human),
            "agent" => Some(Self::Agent),
            _ => None,
        }
    }
}

/// `TRUST_ENRICHMENT`: the trust a fact or preference the enrichment worker
/// extracts starts with. `candidate` (the default) waits for a person to
/// confirm it; `accepted` is that person deciding once, as a standing
/// policy, that extractions are believed from the start. Only new
/// extractions follow it: what is already stored keeps its trust.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EnrichmentTrust {
    #[default]
    Candidate,
    Accepted,
}

impl EnrichmentTrust {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Accepted => "accepted",
        }
    }

    /// `candidate` or `accepted`, in any case. Anything else isn't a value.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "candidate" => Some(Self::Candidate),
            "accepted" => Some(Self::Accepted),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EngineConfig {
    /// `HUMAN_APPROVAL`: `human` (the default) makes an agent's skill write
    /// and contradiction resolution wait for a person; `agent` lets the call
    /// commit, as 6.x did. Each gate can be set apart (#38).
    pub human_approval: HumanApproval,
    /// `HUMAN_APPROVAL_SKILLS`: the skill gate's own setting. `None` follows
    /// `human_approval`. Read through [`EngineConfig::skill_approval`].
    pub human_approval_skills: Option<HumanApproval>,
    /// `HUMAN_APPROVAL_CONTRADICTIONS`: the contradiction gate's own
    /// setting. `None` follows `human_approval`. Read through
    /// [`EngineConfig::contradiction_approval`].
    pub human_approval_contradictions: Option<HumanApproval>,
    /// `TRUST_ENRICHMENT`: what an extracted fact or preference starts as.
    pub trust_enrichment: EnrichmentTrust,
    /// `FACT_EXTRACTION_MODEL`
    pub fact_extraction_model: String,
    /// `QUERY_EXPANSION_MODEL`
    pub query_expansion_model: String,
    /// `CONTRADICTION_MODEL`: tier 2 of the contradiction check, which 6.x
    /// hard-coded.
    pub contradiction_model: String,
    /// `RECALL_EXPAND_COUNT`: variants per expanded query, 1 to 10.
    pub recall_expand_count: i64,
    /// `MEMORY_RECALL_TOP_K`
    pub recall_top_k: i64,
    /// `RECALL_MIN_SCORE`: floor on raw similarity; 0 disables.
    pub recall_min_score: f64,
    /// `RECALL_WEAK_SCORE`: below this a kept result is `weak_match`; 0 disables.
    pub recall_weak_score: f64,
    /// `RECALL_EXPERIENCE_SPAN`: 0 multiplies by the experience weight, as
    /// 6.x did; above 0 the weight adds `(weight - 1) * span` instead.
    pub recall_experience_span: f64,
    /// `RECENCY_DECAY_DAYS`
    pub recency_decay_days: f64,
    /// `ABANDONED_CACHE_TTL_SECONDS`; zero disables the cache.
    pub abandoned_cache_ttl: Duration,
    /// `GRAVEYARD_SCOPE`: `project` keeps a scoped recall's and the hook's
    /// dead-end warnings to their own projects; `all` crosses projects.
    pub graveyard_scope: GraveyardScope,
    /// `AUDIT_LOG`: how much of each change is written to the audit log.
    /// `full` is the only level that keeps memory text.
    pub audit_log: AuditLevel,
    /// `AUDIT_RETENTION_DAYS`: audit rows older than this are pruned by the
    /// maintenance pass; 0, the default since 7.0.11, keeps them for ever,
    /// since `known_at` reads history only as far back as the log goes.
    pub audit_retention_days: u32,
    /// `DEPRIORITISED_WEIGHT`
    pub deprioritised_weight: f64,
    /// `TRUST_DISPUTED_WEIGHT`: how much a memory in a confirmed, unresolved
    /// contradiction counts against an accepted one (#24). A multiplier on
    /// the final score, like the recency decay; the relevance floor reads raw
    /// similarity, so it doesn't move.
    pub trust_disputed_weight: f64,
    /// `TRUST_CANDIDATE_WEIGHT`: the same for an extracted fact or preference
    /// no person has confirmed. 0.9 is about what 90 days of recency costs.
    pub trust_candidate_weight: f64,
    /// `VALIDITY_EXPIRED_WEIGHT`: how much a memory whose `valid_to` has
    /// passed counts in an ordinary recall (#29). It still comes back,
    /// carrying its `valid_to`, so it can't read as true today.
    pub validity_expired_weight: f64,
    /// `DEDUP_SIMILARITY_THRESHOLD`
    pub dedup_threshold: f64,
    /// `CONTRADICTION_SIMILARITY_THRESHOLD`
    pub contradiction_threshold: f64,
    /// `INGEST_MODE`: `full` or `raw`.
    pub ingest_mode: String,
    /// `ENRICHMENT_BATCH_MODE`
    pub enrichment_batch_mode: bool,
    /// `PROJECT_DOMAIN_CACHE_TTL_SECONDS`; zero disables the cache.
    pub domain_cache_ttl: Duration,
    /// `BACKUP_DIR`
    pub backup_dir: PathBuf,
    /// The reading list (`FEEDS_CONFIG_PATH`, or `feeds.yml` beside the
    /// database), which backups can carry and restores merge into. Set by
    /// whoever opens the engine; without it backups hold memories only.
    pub feeds_path: Option<PathBuf>,
    /// `RECALL_EXPAND_QUERIES`: the default for `expand_queries`.
    pub expand_queries: bool,
    /// `STALE_MEMORY_DAYS`: briefing lists active memories untouched this long.
    pub stale_memory_days: i64,
    /// `AUTO_MAINTENANCE_INTERVAL`: briefings per project between maintenance runs; 0 disables.
    pub auto_maintenance_interval: i64,
    /// `OMNIMEM_USER`: the user segment of generated skill keys.
    pub skill_user: String,
    /// `SKILL_MIN_SCORE`: relevance floor for `find_skills`; 0 disables.
    pub skill_min_score: f64,
    /// `SKILL_CLUSTER_THRESHOLD`: similarity that makes two lessons one rule.
    pub skill_cluster_threshold: f64,
    /// `SKILL_DOMAIN_SUGGEST_THRESHOLD`: the did-you-mean guard.
    pub skill_domain_suggest_threshold: f64,
    /// `SKILL_PROPOSAL_TTL_SECONDS`
    pub skill_proposal_ttl: Duration,
    /// `SKILL_EXPORT_DIR`: where `compile_skill(export_path=...)` writes.
    pub skill_export_dir: PathBuf,
    /// `SKILL_FEED_MAX_ARTICLES`: cap on the Feed watch section; 0 disables it.
    pub skill_feed_max_articles: i64,
    /// `SKILL_KNOWLEDGE_WATCH_DAYS`; 0 disables the watch.
    pub skill_knowledge_watch_days: i64,
    /// `SKILL_KNOWLEDGE_WATCH_THRESHOLD`
    pub skill_knowledge_watch_threshold: f64,
    /// `SKILL_SUGGEST_MIN_SIMILARITY`
    pub skill_suggest_min_similarity: f64,
    /// `SKILL_SCAN_INTERVAL_HOURS`; 0 disables the auto scan.
    pub skill_scan_interval_hours: f64,
    /// `SKILL_SCAN_MAX_PROPOSALS`
    pub skill_scan_max_proposals: i64,
    /// `SKILL_SCAN_MIN_POOL`
    pub skill_scan_min_pool: i64,
    /// `SKILL_SCAN_CROSS_PROJECT`
    pub skill_scan_cross_project: bool,
    /// `UPDATE_CHECK`: whether the briefing says a newer release is out.
    pub update_check: bool,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            human_approval: HumanApproval::Human,
            human_approval_skills: None,
            human_approval_contradictions: None,
            trust_enrichment: EnrichmentTrust::Candidate,
            fact_extraction_model: "claude-haiku-5-5".to_owned(),
            query_expansion_model: "claude-haiku-5-5".to_owned(),
            contradiction_model: "claude-haiku-5-5".to_owned(),
            recall_expand_count: 3,
            recall_top_k: 5,
            recall_min_score: 0.15,
            recall_weak_score: 0.40,
            recall_experience_span: 0.0,
            recency_decay_days: 90.0,
            abandoned_cache_ttl: Duration::from_secs(60),
            graveyard_scope: GraveyardScope::Project,
            audit_log: AuditLevel::Lifecycle,
            audit_retention_days: 0,
            deprioritised_weight: 0.2,
            trust_disputed_weight: 0.8,
            trust_candidate_weight: 0.9,
            validity_expired_weight: 0.8,
            dedup_threshold: 0.92,
            contradiction_threshold: 0.7,
            ingest_mode: "full".to_owned(),
            enrichment_batch_mode: false,
            domain_cache_ttl: Duration::from_secs(60),
            backup_dir: PathBuf::from("backups"),
            feeds_path: None,
            expand_queries: false,
            stale_memory_days: 30,
            auto_maintenance_interval: 10,
            skill_user: "local".to_owned(),
            skill_min_score: 0.25,
            skill_cluster_threshold: 0.80,
            skill_domain_suggest_threshold: 0.60,
            skill_proposal_ttl: Duration::from_secs(86_400),
            skill_export_dir: PathBuf::from("backups/skills"),
            skill_feed_max_articles: 25,
            skill_knowledge_watch_days: 14,
            skill_knowledge_watch_threshold: 0.35,
            skill_suggest_min_similarity: 0.30,
            skill_scan_interval_hours: 24.0,
            skill_scan_max_proposals: 3,
            skill_scan_min_pool: 3,
            skill_scan_cross_project: true,
            update_check: true,
        }
    }
}

fn text(name: &str) -> Option<String> {
    omnimem_core::env::var(name)
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

fn number<T: std::str::FromStr>(name: &str, default: T) -> T {
    match text(name) {
        None => default,
        Some(raw) => raw.parse().unwrap_or_else(|_| {
            warn!("{name}={raw:?} is not a number; using the default");
            default
        }),
    }
}

/// A float setting clamped into `range`. `NaN` and the infinities parse
/// as floats but poison every comparison they take part in (a `NaN`
/// threshold would let everything or nothing through), so they fall back
/// to the default like a non-number would.
fn float(name: &str, default: f64, range: std::ops::RangeInclusive<f64>) -> f64 {
    let value = number(name, default);
    let value = if value.is_finite() {
        value
    } else {
        warn!("{name}={value} is not a finite number; using the default");
        default
    };
    let clamped = value.clamp(*range.start(), *range.end());
    if clamped != value {
        warn!("{name}={value} is outside {range:?}; using {clamped}");
    }
    clamped
}

const UNIT: std::ops::RangeInclusive<f64> = 0.0..=1.0;
const NON_NEGATIVE: std::ops::RangeInclusive<f64> = 0.0..=f64::MAX;

/// `GRAVEYARD_SCOPE`, falling back to the default (with a warning) on a value
/// that is neither `project` nor `all`, like the numeric settings do.
fn graveyard_scope(default: GraveyardScope) -> GraveyardScope {
    match text("GRAVEYARD_SCOPE") {
        None => default,
        Some(raw) => GraveyardScope::parse(&raw).unwrap_or_else(|| {
            warn!("GRAVEYARD_SCOPE={raw:?} is not project or all; using the default");
            default
        }),
    }
}

/// `HUMAN_APPROVAL`, falling back to the default (`human`, the safe side)
/// with a warning on a value that is neither `human` nor `agent`.
fn human_approval(default: HumanApproval) -> HumanApproval {
    match text("HUMAN_APPROVAL") {
        None => default,
        Some(raw) => HumanApproval::parse(&raw).unwrap_or_else(|| {
            warn!("HUMAN_APPROVAL={raw:?} is not human or agent; using the default");
            default
        }),
    }
}

/// One gate's own setting: `None` when unset, so it follows
/// `HUMAN_APPROVAL`, and `human` (the safe side) on a value that is neither,
/// with a warning. A typo never opens a gate.
fn gate_approval(name: &str) -> Option<HumanApproval> {
    let raw = text(name)?;
    Some(HumanApproval::parse(&raw).unwrap_or_else(|| {
        warn!("{name}={raw:?} is not human or agent; asking a person");
        HumanApproval::Human
    }))
}

impl EngineConfig {
    /// Who approves an agent's skill write: `HUMAN_APPROVAL_SKILLS`, else
    /// `HUMAN_APPROVAL`.
    pub fn skill_approval(&self) -> HumanApproval {
        self.human_approval_skills.unwrap_or(self.human_approval)
    }

    /// Who approves an agent's contradiction resolution:
    /// `HUMAN_APPROVAL_CONTRADICTIONS`, else `HUMAN_APPROVAL`.
    pub fn contradiction_approval(&self) -> HumanApproval {
        self.human_approval_contradictions
            .unwrap_or(self.human_approval)
    }
}

/// `TRUST_ENRICHMENT`, falling back to the default (`candidate`, the side
/// that asks a person) with a warning on a value that isn't one.
fn trust_enrichment(default: EnrichmentTrust) -> EnrichmentTrust {
    match text("TRUST_ENRICHMENT") {
        None => default,
        Some(raw) => EnrichmentTrust::parse(&raw).unwrap_or_else(|| {
            warn!("TRUST_ENRICHMENT={raw:?} is not candidate or accepted; using candidate");
            default
        }),
    }
}

/// `AUDIT_LOG`, falling back to the default (with a warning) on a value that
/// is not a level. The default is `lifecycle` (#18): state changes and
/// deletes, never memory text, so a typo never stops a log. `off` is still
/// there for anyone who wants nothing kept.
pub fn audit_level_from_env() -> AuditLevel {
    match text("AUDIT_LOG") {
        None => AuditLevel::Lifecycle,
        Some(raw) => AuditLevel::parse(&raw).unwrap_or_else(|| {
            warn!("AUDIT_LOG={raw:?} is not off, lifecycle, writes or full; using lifecycle");
            AuditLevel::Lifecycle
        }),
    }
}

fn flag(name: &str) -> bool {
    text(name).is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "true" | "1" | "yes"))
}

impl EngineConfig {
    /// Read the environment. `default_backup_dir` is used when `BACKUP_DIR`
    /// is unset (the server puts backups beside the database).
    pub fn from_env(default_backup_dir: PathBuf) -> Self {
        let d = Self::default();
        let backup_dir = text("BACKUP_DIR").map_or(default_backup_dir, PathBuf::from);
        let skill_user = text("OMNIMEM_USER")
            .map(|u| crate::domains::normalise_domain(&u))
            .filter(|u| crate::domains::is_valid_domain(u))
            .unwrap_or(d.skill_user);
        Self {
            human_approval: human_approval(d.human_approval),
            human_approval_skills: gate_approval("HUMAN_APPROVAL_SKILLS"),
            human_approval_contradictions: gate_approval("HUMAN_APPROVAL_CONTRADICTIONS"),
            trust_enrichment: trust_enrichment(d.trust_enrichment),
            fact_extraction_model: text("FACT_EXTRACTION_MODEL").unwrap_or(d.fact_extraction_model),
            query_expansion_model: text("QUERY_EXPANSION_MODEL").unwrap_or(d.query_expansion_model),
            contradiction_model: text("CONTRADICTION_MODEL").unwrap_or(d.contradiction_model),
            recall_expand_count: number("RECALL_EXPAND_COUNT", d.recall_expand_count).clamp(1, 10),
            recall_top_k: number("MEMORY_RECALL_TOP_K", d.recall_top_k).clamp(1, 50),
            recall_min_score: float("RECALL_MIN_SCORE", d.recall_min_score, UNIT),
            recall_weak_score: float("RECALL_WEAK_SCORE", d.recall_weak_score, UNIT),
            recall_experience_span: float("RECALL_EXPERIENCE_SPAN", d.recall_experience_span, UNIT),
            recency_decay_days: float("RECENCY_DECAY_DAYS", d.recency_decay_days, NON_NEGATIVE),
            abandoned_cache_ttl: Duration::from_secs(number("ABANDONED_CACHE_TTL_SECONDS", 60)),
            graveyard_scope: graveyard_scope(d.graveyard_scope),
            audit_log: audit_level_from_env(),
            audit_retention_days: number("AUDIT_RETENTION_DAYS", d.audit_retention_days),
            deprioritised_weight: float("DEPRIORITISED_WEIGHT", d.deprioritised_weight, UNIT),
            trust_disputed_weight: float("TRUST_DISPUTED_WEIGHT", d.trust_disputed_weight, UNIT),
            trust_candidate_weight: float("TRUST_CANDIDATE_WEIGHT", d.trust_candidate_weight, UNIT),
            validity_expired_weight: float(
                "VALIDITY_EXPIRED_WEIGHT",
                d.validity_expired_weight,
                UNIT,
            ),
            dedup_threshold: float("DEDUP_SIMILARITY_THRESHOLD", d.dedup_threshold, UNIT),
            contradiction_threshold: float(
                "CONTRADICTION_SIMILARITY_THRESHOLD",
                d.contradiction_threshold,
                UNIT,
            ),
            ingest_mode: text("INGEST_MODE")
                .map(|m| m.to_ascii_lowercase())
                .unwrap_or(d.ingest_mode),
            enrichment_batch_mode: flag("ENRICHMENT_BATCH_MODE"),
            domain_cache_ttl: Duration::from_secs(number("PROJECT_DOMAIN_CACHE_TTL_SECONDS", 60)),
            skill_export_dir: text("SKILL_EXPORT_DIR")
                .map_or_else(|| backup_dir.join("skills"), PathBuf::from),
            backup_dir,
            feeds_path: None,
            expand_queries: flag("RECALL_EXPAND_QUERIES"),
            stale_memory_days: number("STALE_MEMORY_DAYS", d.stale_memory_days).max(0),
            auto_maintenance_interval: number(
                "AUTO_MAINTENANCE_INTERVAL",
                d.auto_maintenance_interval,
            )
            .max(0),
            skill_user,
            skill_min_score: float("SKILL_MIN_SCORE", d.skill_min_score, UNIT),
            skill_cluster_threshold: float(
                "SKILL_CLUSTER_THRESHOLD",
                d.skill_cluster_threshold,
                UNIT,
            ),
            skill_domain_suggest_threshold: float(
                "SKILL_DOMAIN_SUGGEST_THRESHOLD",
                d.skill_domain_suggest_threshold,
                UNIT,
            ),
            skill_proposal_ttl: Duration::from_secs(number("SKILL_PROPOSAL_TTL_SECONDS", 86_400)),
            skill_feed_max_articles: number("SKILL_FEED_MAX_ARTICLES", d.skill_feed_max_articles)
                .max(0),
            skill_knowledge_watch_days: number(
                "SKILL_KNOWLEDGE_WATCH_DAYS",
                d.skill_knowledge_watch_days,
            )
            .max(0),
            skill_knowledge_watch_threshold: float(
                "SKILL_KNOWLEDGE_WATCH_THRESHOLD",
                d.skill_knowledge_watch_threshold,
                UNIT,
            ),
            skill_suggest_min_similarity: float(
                "SKILL_SUGGEST_MIN_SIMILARITY",
                d.skill_suggest_min_similarity,
                UNIT,
            ),
            skill_scan_interval_hours: float(
                "SKILL_SCAN_INTERVAL_HOURS",
                d.skill_scan_interval_hours,
                NON_NEGATIVE,
            ),
            skill_scan_max_proposals: number(
                "SKILL_SCAN_MAX_PROPOSALS",
                d.skill_scan_max_proposals,
            )
            .max(0),
            skill_scan_min_pool: number("SKILL_SCAN_MIN_POOL", d.skill_scan_min_pool).max(1),
            skill_scan_cross_project: text("SKILL_SCAN_CROSS_PROJECT")
                .is_none_or(|v| !matches!(v.to_ascii_lowercase().as_str(), "false" | "0" | "no")),
            update_check: crate::release::update_check_enabled(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The env reader goes through `omnimem_core::env`, so the tests set
    /// real process variables; each uses its own name to stay independent.
    fn with_var<T>(name: &str, value: &str, body: impl FnOnce() -> T) -> T {
        // SAFETY: tests in this module touch distinct variable names and the
        // engine reads them only through `from_env`, called inside `body`.
        unsafe { std::env::set_var(name, value) };
        let out = body();
        unsafe { std::env::remove_var(name) };
        out
    }

    #[test]
    fn float_settings_reject_nan_and_clamp() {
        let d = EngineConfig::default();
        assert_eq!(
            with_var("OMNIMEM_TEST_NAN", "NaN", || float(
                "OMNIMEM_TEST_NAN",
                0.5,
                UNIT
            )),
            0.5
        );
        assert_eq!(
            with_var("OMNIMEM_TEST_INF", "inf", || float(
                "OMNIMEM_TEST_INF",
                0.5,
                UNIT
            )),
            0.5
        );
        assert_eq!(
            with_var("OMNIMEM_TEST_NEG", "-3", || float(
                "OMNIMEM_TEST_NEG",
                0.5,
                UNIT
            )),
            0.0
        );
        assert_eq!(
            with_var("OMNIMEM_TEST_BIG", "7", || float(
                "OMNIMEM_TEST_BIG",
                0.5,
                UNIT
            )),
            1.0
        );
        assert_eq!(
            with_var("OMNIMEM_TEST_DAYS", "-90", || float(
                "OMNIMEM_TEST_DAYS",
                d.recency_decay_days,
                NON_NEGATIVE
            )),
            0.0
        );
        assert_eq!(float("OMNIMEM_TEST_UNSET", 0.25, UNIT), 0.25);
    }

    #[test]
    fn graveyard_scope_parses_and_refuses_the_rest() {
        assert_eq!(
            EngineConfig::default().graveyard_scope,
            GraveyardScope::Project
        );
        assert_eq!(
            GraveyardScope::parse("project"),
            Some(GraveyardScope::Project)
        );
        assert_eq!(GraveyardScope::parse(" ALL "), Some(GraveyardScope::All));
        assert_eq!(GraveyardScope::parse("global"), None);
        assert_eq!(GraveyardScope::parse(""), None);

        // Through the environment: a good value is taken, a bad one falls
        // back to the default rather than guessing, and unset is the default.
        let read = |value: &str| {
            with_var("GRAVEYARD_SCOPE", value, || {
                EngineConfig::from_env(PathBuf::from("backups")).graveyard_scope
            })
        };
        assert_eq!(read("all"), GraveyardScope::All);
        assert_eq!(read("Project"), GraveyardScope::Project);
        assert_eq!(read("everywhere"), GraveyardScope::Project);
        assert_eq!(
            EngineConfig::from_env(PathBuf::from("backups")).graveyard_scope,
            GraveyardScope::Project
        );
    }

    #[test]
    fn audit_settings_parse_and_a_bad_level_keeps_the_lifecycle_log() {
        let d = EngineConfig::default();
        assert_eq!(d.audit_log, AuditLevel::Lifecycle);
        assert_eq!(d.audit_retention_days, 0);

        let level = |value: &str| {
            with_var("AUDIT_LOG", value, || {
                EngineConfig::from_env(PathBuf::from("backups")).audit_log
            })
        };
        assert_eq!(level("lifecycle"), AuditLevel::Lifecycle);
        assert_eq!(level("Writes"), AuditLevel::Writes);
        assert_eq!(level(" FULL "), AuditLevel::Full);
        assert_eq!(level("off"), AuditLevel::Off);
        // A typo never stops the log, and never starts one with text in it:
        // it gets the default, lifecycle, as unset does.
        assert_eq!(level("everything"), AuditLevel::Lifecycle);
        assert_eq!(level("true"), AuditLevel::Lifecycle);
        assert_eq!(
            EngineConfig::from_env(PathBuf::from("backups")).audit_log,
            AuditLevel::Lifecycle
        );

        let days = |value: &str| {
            with_var("AUDIT_RETENTION_DAYS", value, || {
                EngineConfig::from_env(PathBuf::from("backups")).audit_retention_days
            })
        };
        assert_eq!(days("30"), 30);
        assert_eq!(days("0"), 0);
        assert_eq!(days("-5"), 0, "not a count of days, so the default");
        assert_eq!(days("soon"), 0);
    }

    #[test]
    fn trust_enrichment_parses_and_a_bad_value_asks_a_person() {
        assert_eq!(
            EngineConfig::default().trust_enrichment,
            EnrichmentTrust::Candidate
        );
        let value = |raw: &str| {
            with_var("TRUST_ENRICHMENT", raw, || {
                EngineConfig::from_env(PathBuf::from("backups")).trust_enrichment
            })
        };
        assert_eq!(value("accepted"), EnrichmentTrust::Accepted);
        assert_eq!(value(" Accepted "), EnrichmentTrust::Accepted);
        assert_eq!(value("candidate"), EnrichmentTrust::Candidate);
        assert_eq!(value("yes"), EnrichmentTrust::Candidate);
    }

    #[test]
    fn human_approval_parses_and_refuses_the_rest() {
        assert_eq!(EngineConfig::default().human_approval, HumanApproval::Human);
        assert_eq!(HumanApproval::parse(" AGENT "), Some(HumanApproval::Agent));
        assert_eq!(HumanApproval::parse("human"), Some(HumanApproval::Human));
        assert_eq!(HumanApproval::parse("nobody"), None);
        assert_eq!(HumanApproval::parse(""), None);

        // A bad value falls back to the safe side, not to the agent.
        let read = |value: &str| {
            with_var("HUMAN_APPROVAL", value, || {
                EngineConfig::from_env(PathBuf::from("backups")).human_approval
            })
        };
        assert_eq!(read("agent"), HumanApproval::Agent);
        assert_eq!(read("Human"), HumanApproval::Human);
        assert_eq!(read("robot"), HumanApproval::Human);
    }

    /// #38: each gate follows HUMAN_APPROVAL unless set on its own, and a
    /// bad value for one gate asks a person rather than opening it.
    #[test]
    fn each_gate_defaults_to_human_approval_and_can_be_set_apart() {
        let d = EngineConfig::default();
        assert_eq!(
            (d.skill_approval(), d.contradiction_approval()),
            (HumanApproval::Human, HumanApproval::Human)
        );
        let read = |vars: &[(&str, &str)]| {
            fn nest(vars: &[(&str, &str)]) -> EngineConfig {
                match vars.split_first() {
                    None => EngineConfig::from_env(PathBuf::from("backups")),
                    Some(((name, value), rest)) => with_var(name, value, || nest(rest)),
                }
            }
            let c = nest(vars);
            (c.skill_approval(), c.contradiction_approval())
        };
        use HumanApproval::{Agent, Human};
        assert_eq!(read(&[("HUMAN_APPROVAL", "agent")]), (Agent, Agent));
        assert_eq!(
            read(&[
                ("HUMAN_APPROVAL", "agent"),
                ("HUMAN_APPROVAL_SKILLS", "human")
            ]),
            (Human, Agent)
        );
        assert_eq!(
            read(&[("HUMAN_APPROVAL_CONTRADICTIONS", "agent")]),
            (Human, Agent)
        );
        assert_eq!(read(&[("HUMAN_APPROVAL_SKILLS", "agent")]), (Agent, Human));
        assert_eq!(
            read(&[
                ("HUMAN_APPROVAL", "agent"),
                ("HUMAN_APPROVAL_CONTRADICTIONS", "maybe")
            ]),
            (Agent, Human)
        );
    }
}
