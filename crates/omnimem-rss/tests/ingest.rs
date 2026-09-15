//! Ingestion end to end: a local HTTP server stands in for the feeds and
//! article pages, a scripted model for Claude, and SQLite runs in memory.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use omnimem_core::{EmbeddingError, LanguageModel, LlmError, TextEmbedder, VECTOR_DIM};
use omnimem_engine::{Engine, EngineConfig};
use omnimem_rss::{FeedStats, Fetcher, Ingester, RssConfig, url_hash};
use omnimem_store::Store;
use serde_json::{Map, Value, json};

struct Words;

impl TextEmbedder for Words {
    fn dimension(&self) -> usize {
        VECTOR_DIM
    }
    fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        Ok(texts
            .iter()
            .map(|t| {
                let mut v = vec![0.0f32; VECTOR_DIM];
                v[t.len() % VECTOR_DIM] = 1.0;
                v
            })
            .collect())
    }
}

#[derive(Default)]
struct Scripted {
    replies: Mutex<VecDeque<String>>,
    prompts: Mutex<Vec<(u32, String)>>,
}

impl Scripted {
    fn with(replies: &[&str]) -> Arc<Self> {
        let model = Self::default();
        model
            .replies
            .lock()
            .unwrap()
            .extend(replies.iter().map(|r| (*r).to_owned()));
        Arc::new(model)
    }

    fn prompts(&self) -> Vec<(u32, String)> {
        self.prompts.lock().unwrap().clone()
    }
}

impl LanguageModel for Scripted {
    fn complete(&self, _model: &str, prompt: &str, max_tokens: u32) -> Result<String, LlmError> {
        self.prompts
            .lock()
            .unwrap()
            .push((max_tokens, prompt.to_owned()));
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| "no reply scripted".into())
    }
}

type Routes = Arc<Mutex<Vec<(String, String)>>>;

/// Serves the routes (path, body) until the test ends; records every path.
fn server() -> (String, Routes, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let routes: Routes = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (routes_in, seen_in) = (routes.clone(), seen.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            if reader.read_line(&mut request).is_err() {
                continue;
            }
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) if line.trim().is_empty() => break,
                    Ok(_) => {}
                }
            }
            let path = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
            seen_in.lock().unwrap().push(path.clone());
            let body = routes_in
                .lock()
                .unwrap()
                .iter()
                .find(|(p, _)| *p == path)
                .map(|(_, b)| b.clone());
            let (status, body) = match body {
                Some(body) => ("200 OK", body),
                None => ("404 Not Found", String::new()),
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\ncontent-type: text/xml\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (base, routes, seen)
}

fn route(routes: &Routes, path: &str, body: &str) {
    routes
        .lock()
        .unwrap()
        .push((path.to_owned(), body.to_owned()));
}

/// An RSS 2.0 document; an empty path leaves the item without a link.
fn rss(base: &str, items: &[(&str, &str, &str)]) -> String {
    let items: String = items
        .iter()
        .map(|(path, title, description)| {
            let link = if path.is_empty() {
                String::new()
            } else {
                format!("<link>{base}{path}</link>")
            };
            format!(
                "<item><title>{title}</title>{link}<description><![CDATA[{description}]]></description>\
                 <pubDate>Wed, 01 Jul 2026 12:00:00 GMT</pubDate></item>"
            )
        })
        .collect();
    format!(
        "<?xml version=\"1.0\"?><rss version=\"2.0\"><channel><title>Example</title>\
         <link>{base}</link><description>d</description>{items}</channel></rss>"
    )
}

fn config(feeds_path: PathBuf) -> RssConfig {
    RssConfig {
        feeds_path,
        schedule: Duration::ZERO,
        watch_interval: Duration::from_millis(50),
        max_articles_per_feed: 20,
        max_digest_entries: 2,
        max_page_bytes: 1024 * 1024,
        max_knowledge_age_days: 30,
        require_licence: false,
    }
}

/// An ingester over a fresh in-memory store the test can read.
fn ingester(model: Option<Arc<Scripted>>, config: RssConfig) -> (Ingester, Arc<Store>) {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let engine = Engine::new(store.clone(), Arc::new(Words), EngineConfig::default());
    let engine = match model {
        Some(m) => engine.with_llm(m),
        None => engine,
    };
    (Ingester::new(Arc::new(engine), config).unwrap(), store)
}

fn feed(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

fn no_feeds_file() -> PathBuf {
    PathBuf::from("/nonexistent/feeds.yml")
}

fn stats(value: FeedStats) -> Value {
    value.to_value()
}

/// `str(time.mktime(...))` of the test feed's pubDate, as 6.x stored it.
fn local_mktime_2026_07_01_noon() -> String {
    use chrono::{Local, NaiveDate, TimeZone};
    let naive = NaiveDate::from_ymd_opt(2026, 7, 1)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap();
    omnimem_engine::pyfmt::py_float(
        Local
            .from_local_datetime(&naive)
            .earliest()
            .unwrap()
            .timestamp() as f64,
    )
}

fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("omnimem-rss-{label}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_feeds(path: &Path, yaml: &str) {
    std::fs::write(path, yaml).unwrap();
}

#[test]
fn summary_mode_stores_articles_and_skips_them_next_time() {
    let (base, routes, _) = server();
    route(
        &routes,
        "/feed.xml",
        &rss(
            &base,
            &[
                ("/post", "A Post", "<p>Hello <b>world</b></p>"),
                ("", "No link", "x"),
            ],
        ),
    );
    let model = Scripted::with(&["  A summary.  "]);
    let (ingester, store) = ingester(Some(model.clone()), config(no_feeds_file()));
    let feed =
        feed(json!({"url": format!("{base}/feed.xml"), "name": "Example", "topics": ["rust"]}));

    let first = ingester.ingest_feed(&feed).unwrap();
    assert_eq!(
        stats(first),
        json!({"added": 1, "skipped": 0, "errors": 0, "refused": 0})
    );
    let key = format!("mem:knowledge:{}", url_hash(&format!("{base}/post")));
    let article = store.get(&key).unwrap().unwrap();
    assert_eq!(article["content"], "A summary.");
    assert_eq!(article["title"], "A Post");
    assert_eq!(article["source_url"], format!("{base}/post"));
    assert_eq!(article["feed_name"], "Example");
    assert_eq!(article["project"], "RSS");
    assert_eq!(article["topics"], r#"["rust"]"#);
    assert_eq!(article["licence"], "unknown");
    assert_eq!(article["provenance"], "retrieved");
    assert_eq!(article["published_at"], local_mktime_2026_07_01_noon());
    let created: f64 = article["created_at"].parse().unwrap();
    let expires: f64 = article["expires_at"].parse().unwrap();
    assert_eq!(expires - created, 30.0 * 86_400.0);
    let (max_tokens, prompt) = &model.prompts()[0];
    assert_eq!(*max_tokens, 256);
    assert!(prompt.ends_with(&format!("Title: A Post\nURL: {base}/post\n\nHello world")));

    let second = ingester.ingest_feed(&feed).unwrap();
    assert_eq!(
        stats(second),
        json!({"added": 0, "skipped": 1, "errors": 0, "refused": 0})
    );
    assert_eq!(model.prompts().len(), 1);
}

#[test]
fn without_a_model_summaries_fall_back_and_refusals_are_skipped() {
    let (base, routes, _) = server();
    route(
        &routes,
        "/feed.xml",
        &rss(&base, &[("/post", "A Post", "<p>Hello <b>world</b></p>")]),
    );
    let feed = feed(json!({"url": format!("{base}/feed.xml"), "name": "Example"}));
    let key = format!("mem:knowledge:{}", url_hash(&format!("{base}/post")));

    let (offline, store) = ingester(None, config(no_feeds_file()));
    assert_eq!(offline.ingest_feed(&feed).unwrap().added, 1);
    assert_eq!(
        store.get(&key).unwrap().unwrap()["content"],
        "A Post. Hello world"
    );

    let (refusing, store) = ingester(
        Some(Scripted::with(&["I'm unable to access that URL."])),
        config(no_feeds_file()),
    );
    let result = refusing.ingest_feed(&feed).unwrap();
    assert_eq!(
        stats(result),
        json!({"added": 0, "skipped": 1, "errors": 0, "refused": 0})
    );
    assert!(store.get(&key).unwrap().is_none());
}

#[test]
fn licences_and_project_labels_come_from_the_feed() {
    let (base, routes, _) = server();
    route(
        &routes,
        "/feed.xml",
        &rss(&base, &[("/post", "A Post", "text")]),
    );
    let url = format!("{base}/feed.xml");
    let key = format!("mem:knowledge:{}", url_hash(&format!("{base}/post")));
    let ingest = |extra: Value| {
        let (ingester, store) = ingester(None, config(no_feeds_file()));
        let mut f = feed(json!({"url": url, "name": "Example"}));
        f.extend(feed(extra));
        assert_eq!(ingester.ingest_feed(&f).unwrap().added, 1);
        store.get(&key).unwrap().unwrap()
    };

    let declared = ingest(json!({"licence": "CC_BY 4.0", "project": "research"}));
    assert_eq!(declared["licence"], "open");
    assert_eq!(declared["licence_note"], "CC BY 4.0");
    assert_eq!(declared["project"], "research");

    let noted = ingest(
        json!({"licence": "ogl-3.0", "licence_note": format!("  checked   on {}", "x".repeat(300))}),
    );
    assert!(noted["licence_note"].starts_with("checked on xxx"));
    assert_eq!(noted["licence_note"].chars().count(), 200);

    let mistyped = ingest(json!({"licence": false, "project": "bad/label"}));
    assert_eq!(mistyped["licence"], "unknown");
    assert!(!mistyped.contains_key("licence_note"));
    assert_eq!(mistyped["project"], "RSS");
}

#[test]
fn require_licence_refuses_before_any_fetch() {
    let (base, routes, seen) = server();
    route(
        &routes,
        "/feed.xml",
        &rss(&base, &[("/post", "A Post", "text")]),
    );
    let strict = RssConfig {
        require_licence: true,
        ..config(no_feeds_file())
    };
    let url = format!("{base}/feed.xml");

    let (gated, _) = ingester(None, strict.clone());
    let undeclared = gated
        .ingest_feed(&feed(json!({"url": url, "name": "E"})))
        .unwrap();
    assert_eq!(
        stats(undeclared),
        json!({"added": 0, "skipped": 0, "errors": 0, "refused": 1})
    );
    let explicit = gated
        .ingest_feed(&feed(
            json!({"url": url, "name": "E", "licence": "unknown"}),
        ))
        .unwrap();
    assert_eq!(explicit.refused, 1);
    assert!(seen.lock().unwrap().is_empty(), "nothing was fetched");

    let (declared, _) = ingester(None, strict);
    let accepted = declared
        .ingest_feed(&feed(json!({"url": url, "name": "E", "licence": "open"})))
        .unwrap();
    assert_eq!(accepted.added, 1);
}

#[test]
fn digest_mode_fetches_teaser_pages_and_stores_each_item() {
    let (base, routes, seen) = server();
    route(
        &routes,
        "/feed.xml",
        &rss(
            &base,
            &[(
                "/issue",
                "Weekly",
                "tiny but over fifty characters of teaser text here",
            )],
        ),
    );
    route(
        &routes,
        "/issue",
        "<html><script>var secret = 1;</script><p>full page of news</p></html>",
    );
    let reply = "```json\n[{\"title\": \"Item A\", \"who\": \"A\", \"what\": \"did\", \"why\": \"matters\"},\
                 {\"title\": \"No why\", \"who\": \"B\", \"what\": \"did\"}]\n```";
    let model = Scripted::with(&[reply]);
    let (digester, store) = ingester(Some(model.clone()), config(no_feeds_file()));
    let digest =
        feed(json!({"url": format!("{base}/feed.xml"), "name": "Weekly", "mode": "digest"}));

    let first = digester.ingest_feed(&digest).unwrap();
    assert_eq!(
        stats(first),
        json!({"added": 1, "skipped": 0, "errors": 0, "refused": 0})
    );
    assert!(seen.lock().unwrap().contains(&"/issue".to_owned()));
    let (max_tokens, prompt) = &model.prompts()[0];
    assert_eq!(*max_tokens, 4096);
    assert!(prompt.contains(&format!(
        "Article title: Weekly\nArticle URL: {base}/issue\n\nfull page of news"
    )));
    assert!(!prompt.contains("secret"));
    let key = format!("mem:knowledge:{}", url_hash(&format!("{base}/issue:0")));
    let item = store.get(&key).unwrap().unwrap();
    assert_eq!(
        item["content"],
        "# Item A\n\n**Who:** A\n**What:** did\n**Why:** matters"
    );
    assert_eq!(item["title"], "Item A");
    assert_eq!(
        digester.ingest_feed(&digest).unwrap().skipped,
        1,
        "the :0 key dedupes the issue"
    );

    let (offline, _) = ingester(None, config(no_feeds_file()));
    let skipped = offline.ingest_feed(&digest).unwrap();
    assert_eq!(
        stats(skipped),
        json!({"added": 0, "skipped": 1, "errors": 0, "refused": 0})
    );
}

#[test]
fn a_cycle_mirrors_influence_and_rolls_up_stats() {
    let (base, routes, _) = server();
    route(
        &routes,
        "/feed.xml",
        &rss(&base, &[("/post", "A Post", "text")]),
    );
    let dir = temp_dir("cycle");
    let path = dir.join("feeds.yml");
    let (cycler, store) = ingester(None, config(path.clone()));

    assert_eq!(cycler.ingest_all()["status"], "error", "no feeds.yml yet");

    write_feeds(
        &path,
        &format!(
            "feeds:\n  - url: {base}/feed.xml\n    name: A\n    topics: [python]\n    skills:\n      python: 7\n  - name: NoUrl\n"
        ),
    );
    assert_eq!(
        cycler.ingest_all(),
        json!({"status": "complete", "feeds_processed": 2, "added": 1, "skipped": 0, "errors": 1, "refused": 0})
    );
    let mirror = store.hash_get_all("meta:feed:influence").unwrap().unwrap();
    let entry: Value = serde_json::from_str(&mirror["A"]).unwrap();
    assert_eq!(
        entry,
        json!({"skills": {"python": 7}, "topics": ["python"], "url": format!("{base}/feed.xml")})
    );
    assert!(!mirror.contains_key("NoUrl"));

    write_feeds(&path, "feeds: []\n");
    assert_eq!(
        cycler.ingest_all(),
        json!({"status": "no_feeds", "feeds_processed": 0})
    );
    assert!(
        store.hash_get_all("meta:feed:influence").unwrap().is_none(),
        "the mirror is replaced, not merged"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_scheduler_runs_at_start_and_when_feeds_yml_changes() {
    let (base, routes, _) = server();
    route(&routes, "/a.xml", &rss(&base, &[("/a", "First", "text a")]));
    route(
        &routes,
        "/b.xml",
        &rss(&base, &[("/b", "Second", "text b")]),
    );
    let dir = temp_dir("watch");
    let path = dir.join("feeds.yml");
    write_feeds(
        &path,
        &format!("feeds:\n  - url: {base}/a.xml\n    name: A\n"),
    );
    let (scheduler, store) = ingester(None, config(path.clone()));
    let scheduler = Arc::new(scheduler);
    let stop = Arc::new(AtomicBool::new(false));
    let handle = {
        let (scheduler, stop) = (scheduler.clone(), stop.clone());
        std::thread::spawn(move || scheduler.run(&stop))
    };
    let wait_for = |url: String| {
        let key = format!("mem:knowledge:{}", url_hash(&url));
        (0..100).any(|_| {
            let found = store.get(&key).unwrap().is_some();
            if !found {
                std::thread::sleep(Duration::from_millis(50));
            }
            found
        })
    };
    assert!(wait_for(format!("{base}/a")), "the start-up cycle ran");

    write_feeds(
        &path,
        &format!(
            "feeds:\n  - url: {base}/a.xml\n    name: A\n  - url: {base}/b.xml\n    name: B\n"
        ),
    );
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(SystemTime::now() + Duration::from_secs(10))
        .unwrap();
    assert!(
        wait_for(format!("{base}/b")),
        "editing feeds.yml triggered a cycle"
    );

    stop.store(true, Ordering::Relaxed);
    handle.join().unwrap();
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn pages_are_capped_and_only_fetched_over_http() {
    let (base, routes, _) = server();
    route(&routes, "/big", &format!("<p>{}</p>", "a".repeat(100)));
    let fetcher = Fetcher::new(20).unwrap();
    let text = fetcher.fetch_page_content(&format!("{base}/big")).unwrap();
    assert_eq!(
        text,
        "a".repeat(17),
        "20 bytes, then the opening tag stripped"
    );
    assert!(fetcher.fetch_page_content("ftp://example.org/x").is_none());
    assert!(
        fetcher
            .fetch_page_content(&format!("{base}/missing"))
            .is_none()
    );
}
