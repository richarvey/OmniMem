//! RSS ingestion (`rss_worker/`): fetch the feeds in `feeds.yml`, summarise
//! each new article with Claude Haiku (or pull the distinct items out of a
//! digest), embed it and store it in the knowledge namespace, stamped with
//! its feed's licence and project label.
//!
//! In 6.x this was its own container with its own copy of the model and a
//! Valkey connection; here it is a thread in `serve` sharing the engine. The
//! cycle runs at start, every `RSS_SCHEDULE_HOURS`, and whenever `feeds.yml`
//! changes. `feeds.yml` stays the source of truth, and each cycle mirrors it
//! into `meta:feed:influence` for the skill compiler.

mod fetch;
mod ingest;
mod summariser;

use std::path::PathBuf;
use std::time::Duration;

use tracing::warn;

pub use fetch::{Fetcher, page_text};
pub use ingest::{FeedStats, Ingester, format_item, strip_html, url_hash};
pub use summariser::{DigestItem, extract_items, fallback_summary, is_refusal, summarise};

/// The 6.x worker's settings, from the same environment variables.
#[derive(Debug, Clone, PartialEq)]
pub struct RssConfig {
    /// `FEEDS_CONFIG_PATH`
    pub feeds_path: PathBuf,
    /// `RSS_SCHEDULE_HOURS` (6). Zero leaves only the start-up run and runs
    /// triggered by editing `feeds.yml`.
    pub schedule: Duration,
    /// `FEEDS_WATCH_INTERVAL` seconds (10): how often `feeds.yml` is checked.
    pub watch_interval: Duration,
    /// `RSS_MAX_ARTICLES_PER_FEED` (20)
    pub max_articles_per_feed: usize,
    /// `RSS_MAX_DIGEST_ENTRIES` (2)
    pub max_digest_entries: usize,
    /// `RSS_MAX_PAGE_BYTES` (10 MB): the most read from a page or a feed.
    pub max_page_bytes: usize,
    /// `MAX_KNOWLEDGE_AGE_DAYS` (30): articles expire this long after ingest.
    pub max_knowledge_age_days: i64,
    /// `RSS_REQUIRE_LICENCE`: refuse feeds that declare no usable licence.
    pub require_licence: bool,
}

fn number<T: std::str::FromStr>(name: &str, default: T) -> T {
    match std::env::var(name).map(|v| v.trim().to_owned()) {
        Ok(raw) if !raw.is_empty() => raw.parse().unwrap_or_else(|_| {
            warn!("{name}={raw:?} is not a number; using the default");
            default
        }),
        _ => default,
    }
}

impl RssConfig {
    /// `default_feeds_path` is used when `FEEDS_CONFIG_PATH` is unset.
    pub fn from_env(default_feeds_path: PathBuf) -> Self {
        let feeds_path = std::env::var("FEEDS_CONFIG_PATH")
            .ok()
            .filter(|p| !p.trim().is_empty())
            .map_or(default_feeds_path, PathBuf::from);
        Self {
            feeds_path,
            schedule: Duration::from_secs(number::<u64>("RSS_SCHEDULE_HOURS", 6) * 3600),
            watch_interval: Duration::from_secs(number("FEEDS_WATCH_INTERVAL", 10).max(1)),
            max_articles_per_feed: number("RSS_MAX_ARTICLES_PER_FEED", 20),
            max_digest_entries: number("RSS_MAX_DIGEST_ENTRIES", 2),
            max_page_bytes: number("RSS_MAX_PAGE_BYTES", 10 * 1024 * 1024),
            max_knowledge_age_days: number("MAX_KNOWLEDGE_AGE_DAYS", 30),
            require_licence: std::env::var("RSS_REQUIRE_LICENCE").is_ok_and(|v| {
                matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes")
            }),
        }
    }
}
