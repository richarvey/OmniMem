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

pub use fetch::{Fetcher, is_public_host, page_text};
pub use ingest::{FeedStats, Ingester, format_item, strip_html, url_hash};
pub use summariser::{DigestItem, extract_items, fallback_summary, is_refusal, summarise};

/// The longest schedule accepted: a year. Anything longer is a typo, and
/// the clamp keeps the seconds arithmetic well inside a `Duration`.
const MAX_SCHEDULE_HOURS: u64 = 24 * 365;

/// The 6.x worker's settings, from the same environment variables. The
/// fetcher also reads `RSS_ALLOW_PRIVATE_HOSTS`, which lets feeds on a LAN
/// or on this host be fetched; it is off by default.
#[derive(Debug, Clone, PartialEq)]
pub struct RssConfig {
    /// `FEEDS_CONFIG_PATH`
    pub feeds_path: PathBuf,
    /// `RSS_SCHEDULE_HOURS` (6, at most a year). Zero leaves only the
    /// start-up run and runs triggered by editing `feeds.yml`.
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
    /// `RSS_ALLOW_PRIVATE_HOSTS`: fetch feeds and pages on loopback, private
    /// and link-local addresses. Off by default, so a feed entry can't point
    /// OmniMem at the machine it runs on or the network behind it.
    pub allow_private_hosts: bool,
}

/// A `true`, `1` or `yes` setting.
fn flag(name: &str) -> bool {
    omnimem_core::env::var(name)
        .is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes"))
}

fn number<T: std::str::FromStr>(name: &str, default: T) -> T {
    match omnimem_core::env::var(name).map(|v| v.trim().to_owned()) {
        Some(raw) if !raw.is_empty() => raw.parse().unwrap_or_else(|_| {
            warn!("{name}={raw:?} is not a number; using the default");
            default
        }),
        _ => default,
    }
}

impl RssConfig {
    /// `default_feeds_path` is used when `FEEDS_CONFIG_PATH` is unset.
    pub fn from_env(default_feeds_path: PathBuf) -> Self {
        let feeds_path = omnimem_core::env::var("FEEDS_CONFIG_PATH")
            .filter(|p| !p.trim().is_empty())
            .map_or(default_feeds_path, PathBuf::from);
        let hours = number::<u64>("RSS_SCHEDULE_HOURS", 6);
        if hours > MAX_SCHEDULE_HOURS {
            warn!("RSS_SCHEDULE_HOURS={hours} is more than a year; using {MAX_SCHEDULE_HOURS}");
        }
        Self {
            feeds_path,
            schedule: Duration::from_secs(hours.min(MAX_SCHEDULE_HOURS).saturating_mul(3600)),
            watch_interval: Duration::from_secs(number("FEEDS_WATCH_INTERVAL", 10).max(1)),
            max_articles_per_feed: number("RSS_MAX_ARTICLES_PER_FEED", 20),
            max_digest_entries: number("RSS_MAX_DIGEST_ENTRIES", 2),
            max_page_bytes: number("RSS_MAX_PAGE_BYTES", 10 * 1024 * 1024),
            max_knowledge_age_days: number("MAX_KNOWLEDGE_AGE_DAYS", 30),
            require_licence: flag("RSS_REQUIRE_LICENCE"),
            allow_private_hosts: flag("RSS_ALLOW_PRIVATE_HOSTS"),
        }
    }
}
