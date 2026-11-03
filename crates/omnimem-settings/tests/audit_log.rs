//! The audit log through `Panel::handle`: who the panel is in the log, the
//! Audit Log page and its filters, a memory's History section, and the
//! backup tickbox.

use std::path::PathBuf;
use std::sync::Arc;

use axum::http::{Request, Response};
use omnimem_core::{EmbeddingError, TextEmbedder, VECTOR_DIM};
use omnimem_engine::{AuditLevel, Engine, EngineConfig};
use omnimem_settings::Panel;
use omnimem_store::{Actor, AuditQuery, Store, enter};

struct Flat;

impl TextEmbedder for Flat {
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

struct Fixture {
    panel: Panel,
    engine: Arc<Engine>,
    dir: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn setup(level: AuditLevel) -> Fixture {
    let dir = std::env::temp_dir().join(format!("omnimem-audit-panel-{}", ulid::Ulid::generate()));
    let engine = Arc::new(Engine::new(
        Arc::new(Store::open_in_memory().unwrap()),
        Arc::new(Flat),
        EngineConfig {
            audit_log: level,
            backup_dir: dir.join("backups"),
            ..EngineConfig::default()
        },
    ));
    let panel = Panel::new();
    panel.set_engine(engine.clone());
    panel.set_feeds_path(dir.join("feeds.yml"));
    Fixture { panel, engine, dir }
}

fn remember(engine: &Engine, content: &str) -> String {
    let _who = enter(Actor::Mcp, "claude-code");
    engine
        .remember(
            content,
            Some("alpha"),
            None,
            "episodic",
            true,
            Some("raw"),
            None,
            None,
        )
        .unwrap()["key"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn get(panel: &Panel, uri: &str) -> Response<Vec<u8>> {
    panel
        .handle(
            Request::get(format!("omnimem://localhost{uri}"))
                .body(Vec::new())
                .unwrap(),
        )
        .await
}

async fn post(panel: &Panel, uri: &str, form: &str) -> Response<Vec<u8>> {
    panel
        .handle(
            Request::post(format!("omnimem://localhost{uri}"))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(form.as_bytes().to_vec())
                .unwrap(),
        )
        .await
}

fn text(response: &Response<Vec<u8>>) -> String {
    String::from_utf8_lossy(response.body()).into_owned()
}

#[tokio::test]
async fn what_the_panel_changes_is_the_panels_in_the_log() {
    let f = setup(AuditLevel::Lifecycle);
    let key = remember(&f.engine, "a memory the window will archive");
    post(&f.panel, "/lifecycle/archive", &format!("key={key}")).await;

    let rows = f
        .engine
        .store()
        .audit_entries(&AuditQuery::default())
        .unwrap();
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert_eq!(rows[0].actor, "panel");
    assert_eq!(rows[0].client, "");
    assert_eq!(rows[0].action, "transition");
    assert_eq!(rows[0].reason, "Archived via web UI");
}

#[tokio::test]
async fn the_audit_page_lists_and_filters_the_log() {
    let f = setup(AuditLevel::Writes);
    let first = remember(&f.engine, "first memory");
    let second = remember(&f.engine, "second memory!");
    post(&f.panel, "/lifecycle/archive", &format!("key={second}")).await;

    let page = text(&get(&f.panel, "/audit").await);
    assert!(page.contains("Audit Log"), "{page}");
    assert!(page.contains("Recording at the <strong>writes</strong> level"));
    assert!(page.contains(&first) && page.contains(&second));
    assert!(page.contains("state: active -&gt; archived"), "{page}");
    assert!(page.contains("claude-code"), "the client is shown");
    assert!(
        page.contains("/audit\" class=\"nav-link active"),
        "the sidebar link"
    );

    let panel_only = text(&get(&f.panel, "/audit?actor=panel").await);
    assert!(panel_only.contains(&second));
    assert!(!panel_only.contains(&first));

    let by_key = text(&get(&f.panel, &format!("/audit?key={first}")).await);
    assert!(by_key.contains(&first) && !by_key.contains(&second));

    let by_action = text(&get(&f.panel, "/audit?action=transition").await);
    assert!(by_action.contains(&second) && !by_action.contains(&first));

    let future = text(&get(&f.panel, "/audit?from=2999-01-01").await);
    assert!(future.contains("Nothing matches those filters."));
    let past = text(&get(&f.panel, "/audit?to=2000-01-01").await);
    assert!(past.contains("Nothing matches those filters."));
    let today = text(&get(&f.panel, "/audit?from=1d&to=2999-01-01").await);
    assert!(today.contains(&first));

    let bad = text(&get(&f.panel, "/audit?from=whenever").await);
    assert!(bad.contains("read the From date"), "{bad}");
}

#[tokio::test]
async fn the_audit_page_says_when_the_log_is_off() {
    let f = setup(AuditLevel::Off);
    let page = text(&get(&f.panel, "/audit").await);
    assert!(page.contains("The audit log is off"), "{page}");
    assert!(page.contains("The audit log is empty."));
}

#[tokio::test]
async fn a_memorys_page_has_its_history() {
    let f = setup(AuditLevel::Writes);
    let key = remember(&f.engine, "a memory with a past");
    post(&f.panel, "/lifecycle/archive", &format!("key={key}")).await;

    let page = text(&get(&f.panel, &format!("/memory/{key}")).await);
    assert!(page.contains(">History<"), "{page}");
    assert!(page.contains("remember"));
    assert!(page.contains("transition"));
    assert!(page.contains("Archived via web UI"));
    assert!(page.contains(&format!("/audit?key={key}")));
}

#[tokio::test]
async fn a_memorys_page_has_no_history_section_with_the_log_off() {
    let f = setup(AuditLevel::Off);
    let key = remember(&f.engine, "a memory with no past on record");
    let page = text(&get(&f.panel, &format!("/memory/{key}")).await);
    assert!(!page.contains(">History<"), "{page}");
}

#[tokio::test]
async fn the_backup_page_offers_the_audit_log_and_a_backup_carries_it_when_ticked() {
    let f = setup(AuditLevel::Writes);
    remember(&f.engine, "worth a backup");
    let page = text(&get(&f.panel, "/backups").await);
    assert!(page.contains("name=\"audit\""), "{page}");
    assert!(page.contains("Include audit log"));

    let backups = f.dir.join("backups");
    let newest = || {
        let mut files: Vec<_> = std::fs::read_dir(&backups)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        files.sort_by_key(|p| std::fs::metadata(p).unwrap().modified().unwrap());
        files.pop().unwrap()
    };

    post(&f.panel, "/backups/create", "").await;
    let plain = omnimem_store::read_backup(&newest()).unwrap();
    assert!(plain.audit.is_none(), "not unless asked");

    std::thread::sleep(std::time::Duration::from_millis(1100));
    let made = post(&f.panel, "/backups/create", "audit=1").await;
    let location = made.headers()["x-omnimem-redirect"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(location.contains("1%20audit%20row"), "{location}");
    let with = omnimem_store::read_backup(&newest()).unwrap();
    assert_eq!(with.audit.as_ref().map(Vec::len), Some(1));
}
