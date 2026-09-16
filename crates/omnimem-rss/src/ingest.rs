//! Feed ingestion and the scheduler (`rss_worker/ingester.py`, `worker.py`).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

use chrono::{Local, TimeZone};
use feed_rs::model::Entry;
use omnimem_core::LanguageModel;
use omnimem_core::classification::{LICENCE_UNKNOWN, PROVENANCE_RETRIEVED, RSS_PROJECT_LABEL};
use omnimem_engine::Engine;
use omnimem_engine::classification::{MAX_LICENCE_NOTE, resolve_licence};
use omnimem_engine::pyfmt::{now_secs, py_float, py_json, take_chars};
use omnimem_engine::skills::{py_str, py_truthy};
use omnimem_store::Fields;
use regex::Regex;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tracing::{error, info, warn};

use crate::RssConfig;
use crate::fetch::{Fetcher, strip_markup};
use crate::summariser::{DigestItem, extract_items, summarise};

/// Below this many characters a digest entry is a teaser: fetch the page.
const MIN_CONTENT_LENGTH: usize = 500;
const EMBED_CHUNK: usize = 32;
/// The most a stored article's content may hold. This is the engine's
/// `MAX_CONTENT_LENGTH` for `remember`; the constant is private to its
/// tools module, so it is repeated here rather than exposed for one use.
const MAX_CONTENT_CHARS: usize = 50_000;
/// Titles come from the feed body, so they are stripped and capped.
const MAX_TITLE_CHARS: usize = 500;
/// A feed's name and each of its topics, from `feeds.yml`.
const MAX_LABEL_CHARS: usize = 200;
const MAX_TOPICS: usize = 50;

static PROJECT_SAFE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9_\-. ]+$").expect("valid"));

/// Plain text from feed HTML: script, style and comment blocks dropped,
/// tags removed, entities decoded and whitespace collapsed.
pub fn strip_html(text: &str) -> String {
    strip_markup(text, "")
}

/// The feed's display name: `name`, else its URL, capped.
fn feed_name(feed: &Map<String, Value>, url: &str) -> String {
    let name = feed.get("name").map_or_else(|| url.to_owned(), py_str);
    take_chars(&name, MAX_LABEL_CHARS)
}

/// The feed's topics as stored: at most `MAX_TOPICS` entries, each string
/// capped. Anything that isn't a list is dropped rather than stored as is.
fn feed_topics(feed: &Map<String, Value>) -> Value {
    let topics = feed
        .get("topics")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(MAX_TOPICS)
        .map(|t| match t {
            Value::String(s) => Value::String(take_chars(s, MAX_LABEL_CHARS)),
            other => Value::String(take_chars(&py_str(other), MAX_LABEL_CHARS)),
        })
        .collect();
    Value::Array(topics)
}

/// The first 16 hex characters of the URL's sha256: an article's key.
pub fn url_hash(url: &str) -> String {
    Sha256::digest(url.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()[..16]
        .to_owned()
}

pub fn format_item(item: &DigestItem) -> String {
    format!(
        "# {}\n\n**Who:** {}\n**What:** {}\n**Why:** {}",
        item.title, item.who, item.what, item.why
    )
}

/// The entry's title as text: markup stripped and capped, `Untitled` when
/// the feed gives none (or nothing but markup).
fn entry_title(entry: &Entry) -> String {
    let title = entry
        .title
        .as_ref()
        .map(|t| take_chars(&strip_html(&t.content), MAX_TITLE_CHARS))
        .unwrap_or_default();
    if title.is_empty() {
        "Untitled".to_owned()
    } else {
        title
    }
}

/// feedparser's `link`: the alternate link, else the first.
fn entry_link(entry: &Entry) -> String {
    entry
        .links
        .iter()
        .find(|l| l.rel.as_deref().is_none_or(|r| r == "alternate"))
        .or_else(|| entry.links.first())
        .map(|l| l.href.clone())
        .unwrap_or_default()
}

/// The entry's full content when it has any, else its summary, as text.
fn entry_content(entry: &Entry) -> String {
    let raw = match &entry.content {
        Some(content) => content.body.clone().unwrap_or_default(),
        None => entry
            .summary
            .as_ref()
            .map(|s| s.content.clone())
            .unwrap_or_default(),
    };
    strip_html(&raw)
}

/// `str(time.mktime(entry.published_parsed))`: feedparser hands over UTC,
/// and `mktime` reads it as local time. On a UTC host that is the exact
/// timestamp; the quirk is kept so stored values match 6.x.
fn published_at(entry: &Entry) -> String {
    entry
        .published
        .and_then(|t| Local.from_local_datetime(&t.naive_utc()).earliest())
        .map(|t| py_float(t.timestamp() as f64))
        .unwrap_or_default()
}

/// The text of a panic payload, for the log line.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_else(|| "non-string panic payload".to_owned())
}

fn mtime(path: &Path) -> f64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0.0, |d| d.as_secs_f64())
}

/// Per-feed counts, as the 6.x worker reported them.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FeedStats {
    pub added: usize,
    pub skipped: usize,
    pub errors: usize,
    pub refused: usize,
}

impl FeedStats {
    pub fn to_value(self) -> Value {
        json!({"added": self.added, "skipped": self.skipped, "errors": self.errors, "refused": self.refused})
    }

    fn add(&mut self, other: FeedStats) {
        self.added += other.added;
        self.skipped += other.skipped;
        self.errors += other.errors;
        self.refused += other.refused;
    }
}

struct NewArticle {
    key: String,
    title: String,
    summary: String,
    url: String,
    published_at: String,
}

impl NewArticle {
    /// Title and content capped: the summary is model output or page text,
    /// and either could run past what the store should hold.
    fn new(key: String, title: &str, summary: &str, url: &str, published_at: String) -> Self {
        Self {
            key,
            title: take_chars(title, MAX_TITLE_CHARS),
            summary: take_chars(summary, MAX_CONTENT_CHARS),
            url: url.to_owned(),
            published_at,
        }
    }
}

pub struct Ingester {
    engine: Arc<Engine>,
    config: RssConfig,
    fetcher: Fetcher,
}

impl Ingester {
    pub fn new(engine: Arc<Engine>, config: RssConfig) -> Result<Self, String> {
        let fetcher =
            Fetcher::with_private_hosts(config.max_page_bytes, config.allow_private_hosts)?;
        Ok(Self {
            engine,
            config,
            fetcher,
        })
    }

    pub fn config(&self) -> &RssConfig {
        &self.config
    }

    fn llm(&self) -> Option<&dyn LanguageModel> {
        self.engine.llm().map(|m| m.as_ref() as &dyn LanguageModel)
    }

    /// The project label: the feed's `project:` if it is a usable name.
    fn project(feed: &Map<String, Value>, name: &str) -> String {
        let project = feed
            .get("project")
            .filter(|v| py_truthy(v))
            .map(py_str)
            .unwrap_or_default()
            .trim()
            .to_owned();
        if project.is_empty() {
            return RSS_PROJECT_LABEL.to_owned();
        }
        if !PROJECT_SAFE.is_match(&project) {
            warn!(
                feed = name,
                project, "project label has unsupported characters, using {RSS_PROJECT_LABEL}"
            );
            return RSS_PROJECT_LABEL.to_owned();
        }
        project
    }

    /// The licence fields for a feed's articles, or `None` to refuse it.
    fn licence(&self, feed: &Map<String, Value>, name: &str) -> Option<Fields> {
        let declared = match feed.get("licence") {
            None | Some(Value::Null) => String::new(),
            Some(raw) => py_str(raw),
        };
        let (class, derived_note) = resolve_licence(&declared).unwrap_or_else(|_| {
            warn!(feed = name, licence = %declared, "unrecognised licence, treating as unknown");
            (LICENCE_UNKNOWN, None)
        });
        if class == LICENCE_UNKNOWN && self.config.require_licence {
            error!(
                feed = name,
                licence = %declared,
                "feed refused: RSS_REQUIRE_LICENCE is on and the feed declares no usable licence"
            );
            return None;
        }
        let explicit = feed
            .get("licence_note")
            .filter(|v| py_truthy(v))
            .map(py_str)
            .unwrap_or_default()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let note = if explicit.is_empty() {
            derived_note.unwrap_or("").to_owned()
        } else {
            explicit
        };
        let mut fields = Fields::from([("licence".to_owned(), class.to_owned())]);
        let note = take_chars(&note, MAX_LICENCE_NOTE);
        if !note.is_empty() {
            fields.insert("licence_note".to_owned(), note);
        }
        Some(fields)
    }

    /// Digest mode: the page behind a teaser, then one article per item.
    fn digest_articles(&self, entry: &Entry, url: &str) -> Option<Vec<NewArticle>> {
        let title = entry_title(entry);
        let mut content = entry_content(entry);
        if content.chars().count() < MIN_CONTENT_LENGTH {
            info!(
                title,
                chars = content.chars().count(),
                "RSS content too short, fetching page"
            );
            match self
                .fetcher
                .fetch_page_content(url)
                .filter(|p| !p.is_empty())
            {
                Some(page) => content = page,
                None if content.chars().count() < 50 => {
                    warn!(title, "no usable content, skipping");
                    return None;
                }
                None => {}
            }
        }
        let items = extract_items(self.llm(), &title, url, &content)?;
        let published_at = published_at(entry);
        Some(
            items
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    NewArticle::new(
                        format!("mem:knowledge:{}", url_hash(&format!("{url}:{i}"))),
                        &item.title,
                        &format_item(item),
                        url,
                        published_at.clone(),
                    )
                })
                .collect(),
        )
    }

    /// Fetch and ingest one feed. An `Err` is a failure the cycle counts as
    /// one error for the feed.
    pub fn ingest_feed(&self, feed: &Map<String, Value>) -> Result<FeedStats, String> {
        let url = match feed.get("url") {
            None | Some(Value::Null) => return Err("feed has no url".to_owned()),
            Some(url) => py_str(url),
        };
        let name = feed_name(feed, &url);
        let topics = feed_topics(feed);
        let digest = feed.get("mode").map(py_str).as_deref() == Some("digest");
        let project = Self::project(feed, &name);
        let mut stats = FeedStats::default();

        // The licence gate runs before any network I/O: a refused feed costs
        // nothing and stores nothing.
        let Some(licence) = self.licence(feed, &name) else {
            stats.refused = 1;
            return Ok(stats);
        };

        let parsed = match self.fetcher.fetch_feed(&url) {
            Ok(parsed) => parsed,
            Err(e) => {
                warn!(feed = name, error = %e, "no entries in feed: fetch or parse failed");
                return Ok(stats);
            }
        };
        if parsed.entries.is_empty() {
            warn!(feed = name, "no entries in feed");
            return Ok(stats);
        }

        let limit = if digest {
            self.config.max_digest_entries
        } else {
            self.config.max_articles_per_feed
        };
        let store = self.engine.store();
        let mut articles: Vec<NewArticle> = Vec::new();
        for entry in parsed.entries.iter().take(limit) {
            let link = entry_link(entry);
            if link.is_empty() {
                continue;
            }
            let dedup_key = if digest {
                format!("mem:knowledge:{}", url_hash(&format!("{link}:0")))
            } else {
                format!("mem:knowledge:{}", url_hash(&link))
            };
            if store.get(&dedup_key).map_err(|e| e.to_string())?.is_some() {
                stats.skipped += 1;
                continue;
            }
            if digest {
                match self.digest_articles(entry, &link) {
                    Some(items) => articles.extend(items),
                    None => {
                        info!(
                            title = entry_title(entry),
                            "skipping: digest extraction failed"
                        );
                        stats.skipped += 1;
                    }
                }
                continue;
            }
            let title = entry_title(entry);
            let content = take_chars(&entry_content(entry), 2000);
            let Some(summary) = summarise(self.llm(), &title, &link, &content) else {
                info!(title, "skipping: summarisation refused");
                stats.skipped += 1;
                continue;
            };
            articles.push(NewArticle::new(
                dedup_key,
                &title,
                &summary,
                &link,
                published_at(entry),
            ));
        }

        let now = now_secs();
        let created = py_float(now);
        let expires = py_float(now + self.config.max_knowledge_age_days as f64 * 86_400.0);
        let topics_json = py_json(&topics);
        for chunk in articles.chunks(EMBED_CHUNK) {
            let texts: Vec<&str> = chunk.iter().map(|a| a.summary.as_str()).collect();
            let vectors = match self.engine.embed_texts(&texts) {
                Ok(vectors) => vectors,
                Err(e) => {
                    error!(feed = name, error = %e, "embedding failed for a chunk");
                    stats.errors += chunk.len();
                    continue;
                }
            };
            for (article, vector) in chunk.iter().zip(vectors) {
                let mut fields = Fields::from([
                    ("content".to_owned(), article.summary.clone()),
                    ("title".to_owned(), article.title.clone()),
                    ("source_url".to_owned(), article.url.clone()),
                    ("feed_name".to_owned(), name.clone()),
                    ("project".to_owned(), project.clone()),
                    ("published_at".to_owned(), article.published_at.clone()),
                    ("topics".to_owned(), topics_json.clone()),
                    ("state".to_owned(), "active".to_owned()),
                    ("surface_score".to_owned(), "1.0".to_owned()),
                    ("experience_weight".to_owned(), "1.0".to_owned()),
                    ("created_at".to_owned(), created.clone()),
                    ("updated_at".to_owned(), created.clone()),
                    ("expires_at".to_owned(), expires.clone()),
                    ("provenance".to_owned(), PROVENANCE_RETRIEVED.to_owned()),
                ]);
                fields.extend(licence.clone());
                match store.upsert(&article.key, &fields, Some(&vector)) {
                    Ok(()) => stats.added += 1,
                    Err(e) => {
                        error!(feed = name, key = article.key, error = %e, "store failed");
                        stats.errors += 1;
                    }
                }
            }
        }
        info!(
            feed = name,
            added = stats.added,
            skipped = stats.skipped,
            errors = stats.errors,
            "feed ingested"
        );
        Ok(stats)
    }

    /// The `feeds:` list, with each entry's name and topics capped so the
    /// influence mirror and every article store the same bounded values.
    fn load_feeds(&self) -> Result<Vec<Value>, String> {
        let text = std::fs::read_to_string(&self.config.feeds_path).map_err(|e| e.to_string())?;
        let config: Value = serde_yaml_ng::from_str(&text).map_err(|e| e.to_string())?;
        let mut feeds = config
            .get("feeds")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for feed in feeds.iter_mut().filter_map(Value::as_object_mut) {
            if let Some(name) = feed.get("name").map(py_str) {
                feed.insert("name".to_owned(), json!(take_chars(&name, MAX_LABEL_CHARS)));
            }
            if feed.contains_key("topics") {
                let topics = feed_topics(feed);
                feed.insert("topics".to_owned(), topics);
            }
        }
        Ok(feeds)
    }

    /// One cycle over every feed.
    pub fn ingest_all(&self) -> Value {
        self.ingest_all_until(&AtomicBool::new(false))
    }

    /// One cycle, stopping between feeds once `stop` is set.
    pub fn ingest_all_until(&self, stop: &AtomicBool) -> Value {
        let path = self.config.feeds_path.display().to_string();
        let feeds = match self.load_feeds() {
            Ok(feeds) => feeds,
            Err(e) => {
                error!(path, error = %e, "failed to load feeds config");
                return json!({"status": "error", "message": "Failed to load feeds configuration"});
            }
        };
        // Keep the skill compiler's view current even for hand edits.
        if let Err(e) = self.engine.sync_feed_influences(&feeds) {
            warn!(error = %e, "could not mirror feed influence");
        }
        if feeds.is_empty() {
            warn!(path, "no feeds configured");
            return json!({"status": "no_feeds", "feeds_processed": 0});
        }

        let mut total = FeedStats::default();
        for feed in &feeds {
            if stop.load(Ordering::Relaxed) {
                info!("stopping the RSS cycle between feeds");
                break;
            }
            // A panic in one feed (a parser edge case, a bad model reply)
            // must not take the scheduler thread down with it, so it is
            // caught and counted as that feed's error.
            let result = feed
                .as_object()
                .ok_or_else(|| "feed entry is not a mapping".to_owned())
                .and_then(|f| {
                    catch_unwind(AssertUnwindSafe(|| self.ingest_feed(f))).unwrap_or_else(
                        |payload| Err(format!("ingest panicked: {}", panic_message(&payload))),
                    )
                });
            match result {
                Ok(stats) => total.add(stats),
                Err(e) => {
                    let name = feed.get("name").map_or_else(|| "?".to_owned(), py_str);
                    error!(feed = name, error = %e, "failed to ingest feed");
                    total.errors += 1;
                }
            }
        }
        info!(
            feeds = feeds.len(),
            added = total.added,
            skipped = total.skipped,
            errors = total.errors,
            refused = total.refused,
            "ingestion complete"
        );
        json!({
            "status": "complete",
            "feeds_processed": feeds.len(),
            "added": total.added,
            "skipped": total.skipped,
            "errors": total.errors,
            "refused": total.refused,
        })
    }

    /// What a cycle would ingest, without summarising or storing: each
    /// feed's entries with the key, title, link, publication time and the
    /// start of the text 6.x would summarise.
    pub fn preview(&self) -> Value {
        let feeds = match self.load_feeds() {
            Ok(feeds) => feeds,
            Err(e) => return json!({"status": "error", "message": e}),
        };
        let mut entries = Vec::new();
        let mut problems = Vec::new();
        for feed in feeds.iter().filter_map(Value::as_object) {
            let Some(url) = feed.get("url").filter(|u| !u.is_null()).map(py_str) else {
                problems.push(json!({"feed": feed.get("name").map(py_str), "problem": "no url"}));
                continue;
            };
            let name = feed_name(feed, &url);
            if self.licence(feed, &name).is_none() {
                problems.push(json!({"feed": name, "problem": "refused: no usable licence"}));
                continue;
            }
            let digest = feed.get("mode").map(py_str).as_deref() == Some("digest");
            let parsed = match self.fetcher.fetch_feed(&url) {
                Ok(parsed) => parsed,
                Err(e) => {
                    problems.push(json!({"feed": name, "problem": e}));
                    continue;
                }
            };
            let limit = if digest {
                self.config.max_digest_entries
            } else {
                self.config.max_articles_per_feed
            };
            for entry in parsed.entries.iter().take(limit) {
                let link = entry_link(entry);
                if link.is_empty() {
                    continue;
                }
                let hashed = if digest {
                    format!("{link}:0")
                } else {
                    link.clone()
                };
                entries.push(json!({
                    "feed": name,
                    "key": format!("mem:knowledge:{}", url_hash(&hashed)),
                    "title": entry_title(entry),
                    "url": link,
                    "published_at": published_at(entry),
                    "content": take_chars(&entry_content(entry), 200),
                }));
            }
        }
        json!({"entries": entries, "problems": problems})
    }

    /// The scheduler: a cycle at start, then one every `schedule`, and one
    /// whenever `feeds.yml` changes, until `stop` is set.
    pub fn run(&self, stop: &AtomicBool) {
        let path = self.config.feeds_path.clone();
        info!(path = %path.display(), "RSS scheduler started");
        let mut last_mtime = mtime(&path);
        self.ingest_all_until(stop);
        // `None` when there is no schedule, or when adding it to the clock
        // would overflow (a schedule too far out to ever fire).
        let next_after = |now: Instant| {
            (!self.config.schedule.is_zero())
                .then(|| now.checked_add(self.config.schedule))
                .flatten()
        };
        let mut next_run = next_after(Instant::now());
        let mut last_check = Instant::now();
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(100));
            if next_run.is_some_and(|due| Instant::now() >= due) {
                info!("starting scheduled RSS ingestion");
                self.ingest_all_until(stop);
                next_run = next_after(Instant::now());
                continue;
            }
            if last_check.elapsed() >= self.config.watch_interval {
                last_check = Instant::now();
                let current = mtime(&path);
                if current > last_mtime {
                    info!(path = %path.display(), "feeds.yml changed, triggering re-ingestion");
                    last_mtime = current;
                    self.ingest_all_until(stop);
                }
            }
        }
        info!("RSS scheduler stopped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_strips_to_single_spaced_text() {
        assert_eq!(
            strip_html("<p>Hello <b>world</b></p>\n\n  x "),
            "Hello world x"
        );
    }

    #[test]
    fn html_stripping_drops_script_style_and_comment_contents() {
        assert_eq!(
            strip_html("<SCRIPT>var s = 1;</SCRIPT><style>p{}</style><!-- c -->A &amp; B"),
            "A & B"
        );
    }

    /// The one entry of an RSS document whose item title is `title`.
    fn titled(title: &str) -> Entry {
        let doc = format!(
            "<rss version=\"2.0\"><channel><title>F</title><item><title><![CDATA[{title}]]></title>\
             </item></channel></rss>"
        );
        feed_rs::parser::parse(doc.as_bytes())
            .unwrap()
            .entries
            .remove(0)
    }

    #[test]
    fn titles_are_stripped_and_capped() {
        assert_eq!(
            entry_title(&titled("<b>Bold</b> &amp; plain")),
            "Bold & plain"
        );
        assert_eq!(entry_title(&titled("<script>x</script>")), "Untitled");
        assert_eq!(entry_title(&Entry::default()), "Untitled");
        let long = "é".repeat(MAX_TITLE_CHARS + 10);
        assert_eq!(entry_title(&titled(&long)).chars().count(), MAX_TITLE_CHARS);
    }

    #[test]
    fn feed_labels_are_capped() {
        let feed = json!({
            "name": "n".repeat(300),
            "topics": ["t".repeat(300), 7, {"x": 1}],
        });
        let feed = feed.as_object().unwrap();
        assert_eq!(feed_name(feed, "u").chars().count(), MAX_LABEL_CHARS);
        let topics = feed_topics(feed);
        let topics = topics.as_array().unwrap();
        assert_eq!(topics.len(), 3);
        assert_eq!(topics[0].as_str().unwrap().chars().count(), MAX_LABEL_CHARS);
        assert_eq!(topics[1], json!("7"));
        let many: Vec<Value> = (0..100).map(|i| json!(i.to_string())).collect();
        let feed = json!({"topics": many});
        assert_eq!(
            feed_topics(feed.as_object().unwrap())
                .as_array()
                .unwrap()
                .len(),
            MAX_TOPICS
        );
        let none = json!({"name": "x", "topics": "not a list"});
        assert_eq!(feed_topics(none.as_object().unwrap()), json!([]));
    }

    #[test]
    fn stored_content_is_capped() {
        let article = NewArticle::new(
            "k".into(),
            &"t".repeat(1000),
            &"c".repeat(MAX_CONTENT_CHARS + 5),
            "u",
            String::new(),
        );
        assert_eq!(article.title.chars().count(), MAX_TITLE_CHARS);
        assert_eq!(article.summary.chars().count(), MAX_CONTENT_CHARS);
    }

    #[test]
    fn panic_payloads_read_as_text() {
        let payload = catch_unwind(|| panic!("boom {}", 1)).unwrap_err();
        assert_eq!(panic_message(&*payload), "boom 1");
        let payload = catch_unwind(|| std::panic::panic_any(7u8)).unwrap_err();
        assert_eq!(panic_message(&*payload), "non-string panic payload");
    }

    #[test]
    fn url_hashes_are_sixteen_hex_characters() {
        let hash = url_hash("https://example.org/post");
        assert_eq!(hash.len(), 16);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(hash, url_hash("https://example.org/post:0"));
    }

    #[test]
    fn digest_items_format_like_6x() {
        let item = DigestItem {
            title: "T".into(),
            who: "W".into(),
            what: "X".into(),
            why: "Y".into(),
        };
        assert_eq!(
            format_item(&item),
            "# T\n\n**Who:** W\n**What:** X\n**Why:** Y"
        );
    }
}
