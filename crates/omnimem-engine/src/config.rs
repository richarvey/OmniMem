//! Tunables, with 6.x's environment variables and defaults.

use std::env;
use std::path::PathBuf;
use std::time::Duration;

use tracing::warn;

#[derive(Debug, Clone, PartialEq)]
pub struct EngineConfig {
    /// `FACT_EXTRACTION_MODEL`
    pub fact_extraction_model: String,
    /// `QUERY_EXPANSION_MODEL`
    pub query_expansion_model: String,
    /// `RECALL_EXPAND_COUNT`: variants per expanded query, 1 to 10.
    pub recall_expand_count: i64,
    /// `MEMORY_RECALL_TOP_K`
    pub recall_top_k: i64,
    /// `RECALL_MIN_SCORE`: floor on raw similarity; 0 disables.
    pub recall_min_score: f64,
    /// `RECALL_WEAK_SCORE`: below this a kept result is `weak_match`; 0 disables.
    pub recall_weak_score: f64,
    /// `RECENCY_DECAY_DAYS`
    pub recency_decay_days: f64,
    /// `ABANDONED_CACHE_TTL_SECONDS`; zero disables the cache.
    pub abandoned_cache_ttl: Duration,
    /// `DEPRIORITISED_WEIGHT`
    pub deprioritised_weight: f64,
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
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            fact_extraction_model: "claude-haiku-4-5-20251001".to_owned(),
            query_expansion_model: "claude-haiku-4-5-20251001".to_owned(),
            recall_expand_count: 3,
            recall_top_k: 5,
            recall_min_score: 0.15,
            recall_weak_score: 0.35,
            recency_decay_days: 90.0,
            abandoned_cache_ttl: Duration::from_secs(60),
            deprioritised_weight: 0.2,
            dedup_threshold: 0.92,
            contradiction_threshold: 0.7,
            ingest_mode: "full".to_owned(),
            enrichment_batch_mode: false,
            domain_cache_ttl: Duration::from_secs(60),
            backup_dir: PathBuf::from("backups"),
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
        }
    }
}

fn text(name: &str) -> Option<String> {
    env::var(name)
        .ok()
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

fn flag(name: &str) -> bool {
    text(name).is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "true" | "1" | "yes"))
}

impl EngineConfig {
    /// Read the environment. `default_backup_dir` is used when `BACKUP_DIR`
    /// is unset (the server puts backups beside the database).
    pub fn from_env(default_backup_dir: PathBuf) -> Self {
        let d = Self::default();
        let backup_dir = text("BACKUP_DIR")
            .map(PathBuf::from)
            .unwrap_or(default_backup_dir);
        let skill_user = text("OMNIMEM_USER")
            .map(|u| crate::domains::normalise_domain(&u))
            .filter(|u| crate::domains::is_valid_domain(u))
            .unwrap_or(d.skill_user);
        Self {
            fact_extraction_model: text("FACT_EXTRACTION_MODEL").unwrap_or(d.fact_extraction_model),
            query_expansion_model: text("QUERY_EXPANSION_MODEL").unwrap_or(d.query_expansion_model),
            recall_expand_count: number("RECALL_EXPAND_COUNT", d.recall_expand_count),
            recall_top_k: number("MEMORY_RECALL_TOP_K", d.recall_top_k),
            recall_min_score: number("RECALL_MIN_SCORE", d.recall_min_score).max(0.0),
            recall_weak_score: number("RECALL_WEAK_SCORE", d.recall_weak_score).max(0.0),
            recency_decay_days: number("RECENCY_DECAY_DAYS", d.recency_decay_days),
            abandoned_cache_ttl: Duration::from_secs(number("ABANDONED_CACHE_TTL_SECONDS", 60)),
            deprioritised_weight: number("DEPRIORITISED_WEIGHT", d.deprioritised_weight),
            dedup_threshold: number("DEDUP_SIMILARITY_THRESHOLD", d.dedup_threshold),
            contradiction_threshold: number(
                "CONTRADICTION_SIMILARITY_THRESHOLD",
                d.contradiction_threshold,
            ),
            ingest_mode: text("INGEST_MODE")
                .map(|m| m.to_ascii_lowercase())
                .unwrap_or(d.ingest_mode),
            enrichment_batch_mode: flag("ENRICHMENT_BATCH_MODE"),
            domain_cache_ttl: Duration::from_secs(number("PROJECT_DOMAIN_CACHE_TTL_SECONDS", 60)),
            skill_export_dir: text("SKILL_EXPORT_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| backup_dir.join("skills")),
            backup_dir,
            expand_queries: flag("RECALL_EXPAND_QUERIES"),
            stale_memory_days: number("STALE_MEMORY_DAYS", d.stale_memory_days),
            auto_maintenance_interval: number(
                "AUTO_MAINTENANCE_INTERVAL",
                d.auto_maintenance_interval,
            )
            .max(0),
            skill_user,
            skill_min_score: number("SKILL_MIN_SCORE", d.skill_min_score).max(0.0),
            skill_cluster_threshold: number("SKILL_CLUSTER_THRESHOLD", d.skill_cluster_threshold),
            skill_domain_suggest_threshold: number(
                "SKILL_DOMAIN_SUGGEST_THRESHOLD",
                d.skill_domain_suggest_threshold,
            ),
            skill_proposal_ttl: Duration::from_secs(number("SKILL_PROPOSAL_TTL_SECONDS", 86_400)),
            skill_feed_max_articles: number("SKILL_FEED_MAX_ARTICLES", d.skill_feed_max_articles),
            skill_knowledge_watch_days: number(
                "SKILL_KNOWLEDGE_WATCH_DAYS",
                d.skill_knowledge_watch_days,
            ),
            skill_knowledge_watch_threshold: number(
                "SKILL_KNOWLEDGE_WATCH_THRESHOLD",
                d.skill_knowledge_watch_threshold,
            ),
            skill_suggest_min_similarity: number(
                "SKILL_SUGGEST_MIN_SIMILARITY",
                d.skill_suggest_min_similarity,
            ),
            skill_scan_interval_hours: number(
                "SKILL_SCAN_INTERVAL_HOURS",
                d.skill_scan_interval_hours,
            ),
            skill_scan_max_proposals: number(
                "SKILL_SCAN_MAX_PROPOSALS",
                d.skill_scan_max_proposals,
            )
            .max(0),
            skill_scan_min_pool: number("SKILL_SCAN_MIN_POOL", d.skill_scan_min_pool).max(1),
            skill_scan_cross_project: text("SKILL_SCAN_CROSS_PROJECT")
                .is_none_or(|v| !matches!(v.to_ascii_lowercase().as_str(), "false" | "0" | "no")),
        }
    }
}
