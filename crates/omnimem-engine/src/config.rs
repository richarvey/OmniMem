//! Tunables, with 6.x's environment variables and defaults.

use std::env;
use std::path::PathBuf;
use std::time::Duration;

use tracing::warn;

#[derive(Debug, Clone, PartialEq)]
pub struct EngineConfig {
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
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
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
        Self {
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
            backup_dir: text("BACKUP_DIR")
                .map(PathBuf::from)
                .unwrap_or(default_backup_dir),
            expand_queries: flag("RECALL_EXPAND_QUERIES"),
            stale_memory_days: number("STALE_MEMORY_DAYS", d.stale_memory_days),
            auto_maintenance_interval: number(
                "AUTO_MAINTENANCE_INTERVAL",
                d.auto_maintenance_interval,
            )
            .max(0),
        }
    }
}
