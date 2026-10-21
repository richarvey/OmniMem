//! The reading list editor and backups through `Panel::handle`: the pages
//! that read and write files.

use std::path::PathBuf;
use std::sync::Arc;

use axum::http::{Request, Response};
use omnimem_core::{EmbeddingError, TextEmbedder, VECTOR_DIM};
use omnimem_engine::{Engine, EngineConfig};
use omnimem_settings::Panel;
use omnimem_store::Store;

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

fn setup() -> Fixture {
    let dir = std::env::temp_dir().join(format!("omnimem-files-{}", ulid::Ulid::generate()));
    let engine = Arc::new(Engine::new(
        Arc::new(Store::open_in_memory().unwrap()),
        Arc::new(Flat),
        EngineConfig {
            backup_dir: dir.join("backups"),
            ..EngineConfig::default()
        },
    ));
    let panel = Panel::new();
    panel.set_engine(engine.clone());
    panel.set_feeds_path(dir.join("feeds.yml"));
    panel.set_downloads_dir(dir.join("Downloads"));
    Fixture { panel, engine, dir }
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

async fn upload(panel: &Panel, uri: &str, filename: &str, data: &[u8]) -> Response<Vec<u8>> {
    let mut body = format!(
        "--XBOUNDARY\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(data);
    body.extend_from_slice(b"\r\n--XBOUNDARY--\r\n");
    panel
        .handle(
            Request::post(format!("omnimem://localhost{uri}"))
                .header("content-type", "multipart/form-data; boundary=XBOUNDARY")
                .body(body)
                .unwrap(),
        )
        .await
}

fn text(response: &Response<Vec<u8>>) -> String {
    String::from_utf8_lossy(response.body()).into_owned()
}

fn landed(response: &Response<Vec<u8>>) -> String {
    assert_eq!(response.status(), 200, "{}", text(response));
    response.headers()["x-omnimem-redirect"]
        .to_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn the_reading_list_is_edited_mirrored_and_replaced() {
    let f = setup();
    let feeds = f.dir.join("feeds.yml");
    assert!(text(&get(&f.panel, "/feeds").await).contains("No RSS feeds configured."));
    assert!(text(&get(&f.panel, "/feeds/new").await).contains("Add Feed"));

    assert_eq!(
        landed(&post(&f.panel, "/feeds/new", "name=&url=").await),
        "/feeds/new"
    );
    let refused = landed(
        &post(
            &f.panel,
            "/feeds/new",
            "name=Rust&url=https%3A%2F%2Fexample.com%2Ffeed&skill_domain=rust&skill_influence=11",
        )
        .await,
    );
    assert!(refused.starts_with("/feeds/new?error="), "{refused}");

    let url = "https://blog.rust-lang.org/feed.xml";
    let form = format!(
        "name=Rust+Blog&url={}&topics=rust%2C+language&digest=on&licence=open&licence_note=CC+BY+4.0&skill_domain=py&skill_influence=8&skill_domain=&skill_influence=5",
        url.replace(':', "%3A").replace('/', "%2F")
    );
    assert_eq!(landed(&post(&f.panel, "/feeds/new", &form).await), "/feeds");
    let written = std::fs::read_to_string(&feeds).unwrap();
    assert!(written.contains("mode: digest"), "{written}");
    assert!(written.contains("python: 8"), "{written}");
    let mirrored = f.engine.load_feed_influences();
    assert!(
        mirrored
            .values()
            .any(|feed| feed.url == url && feed.skills == [("python".to_owned(), 8)]),
        "{mirrored:?}"
    );

    let list = text(&get(&f.panel, "/feeds").await);
    assert!(list.contains("python (8)"), "{list}");
    assert!(list.contains(r#"<span class="badge ns-knowledge">language</span>"#));

    let edit = text(&get(&f.panel, "/feeds/0/edit").await);
    assert!(edit.contains(r#"value="CC BY 4.0""#), "{edit}");
    assert!(edit.contains(r#"<option value="open" selected>"#));
    assert_eq!(get(&f.panel, "/feeds/7/edit").await.status(), 404);
    // Switching the class with the pre-filled note left alone drops the note.
    let switched = form.replace("licence=open", "licence=restricted");
    assert_eq!(
        landed(&post(&f.panel, "/feeds/0/edit", &switched).await),
        "/feeds"
    );
    let written = std::fs::read_to_string(&feeds).unwrap();
    assert!(
        written.contains("licence: restricted") && !written.contains("CC BY"),
        "{written}"
    );

    let saved = landed(&get(&f.panel, "/feeds/download").await);
    assert!(
        saved.starts_with("/feeds?message=Saved%20a%20copy%20to%20"),
        "{saved}"
    );
    assert!(f.dir.join("Downloads").join("feeds.yml").is_file());

    assert!(
        landed(&upload(&f.panel, "/feeds/upload", "feeds.txt", b"feeds: []").await)
            .contains("Only%20.yml")
    );
    assert!(
        landed(&upload(&f.panel, "/feeds/upload", "feeds.yml", b"feeds: [").await)
            .contains("Invalid%20YAML")
    );
    assert!(
        landed(&upload(&f.panel, "/feeds/upload", "feeds.yml", b"other: 1").await)
            .contains("top-level")
    );
    let replacement = b"feeds:\n  - url: https://example.com/a.xml\n    name: A\n";
    assert!(
        landed(&upload(&f.panel, "/feeds/upload", "feeds.yaml", replacement).await)
            .starts_with("/feeds?message=")
    );
    let written = std::fs::read_to_string(&feeds).unwrap();
    assert!(written.contains("https://example.com/a.xml"), "{written}");
    assert!(written.contains("name: A"), "{written}");
    // A bad entry refuses the whole upload, and says which one.
    let smuggled = b"feeds:\n  - url: file:///etc/passwd\n    name: X\n";
    assert!(
        landed(&upload(&f.panel, "/feeds/upload", "feeds.yml", smuggled).await)
            .contains("Feed%201")
    );

    assert_eq!(
        landed(&post(&f.panel, "/feeds/0/delete", "").await),
        "/feeds"
    );
    assert!(text(&get(&f.panel, "/feeds").await).contains("No RSS feeds configured."));
}

#[tokio::test]
async fn backups_are_made_previewed_restored_and_removed() {
    let f = setup();
    let key = f
        .engine
        .remember(
            "A memory worth keeping",
            Some("omnimem"),
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
        .to_owned();
    assert!(text(&get(&f.panel, "/backups").await).contains("No backup files found."));

    let created = landed(&post(&f.panel, "/backups/create", "").await);
    assert!(
        created.starts_with("/backups?message=Backup%20created%3A%20memory_backup_"),
        "{created}"
    );
    let filename = std::fs::read_dir(f.dir.join("backups"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .file_name()
        .to_string_lossy()
        .into_owned();
    assert!(text(&get(&f.panel, "/backups").await).contains(&filename));

    let preview = text(&get(&f.panel, &format!("/backups/{filename}/preview")).await);
    assert!(
        preview.contains(&format!("Restore Preview: {filename}")),
        "{preview}"
    );
    assert!(preview.contains("Total Keys"));
    assert_eq!(
        get(&f.panel, "/backups/spaced%20name.json/preview")
            .await
            .status(),
        400
    );
    assert_eq!(
        get(&f.panel, "/backups/missing.json/preview")
            .await
            .status(),
        404
    );

    f.engine.store().delete(&key).unwrap();
    let restored = landed(&post(&f.panel, &format!("/backups/{filename}/restore"), "").await);
    assert!(
        restored.starts_with("/backups?message=Restored%20"),
        "{restored}"
    );
    assert!(
        f.engine.store().get(&key).unwrap().is_some(),
        "the memory came back"
    );

    let saved = landed(&get(&f.panel, &format!("/backups/{filename}/download")).await);
    assert!(saved.contains("Saved%20a%20copy"), "{saved}");
    assert!(f.dir.join("Downloads").join(&filename).is_file());

    assert!(
        landed(&upload(&f.panel, "/backups/upload", "notes.txt", b"{}").await)
            .contains("Only%20.json")
    );
    assert!(
        landed(&upload(&f.panel, "/backups/upload", "bad.json", b"{").await)
            .contains("not%20valid%20JSON")
    );
    assert_eq!(
        landed(
            &upload(
                &f.panel,
                "/backups/upload",
                "../../elsewhere/mine.json",
                br#"{"data": {}}"#
            )
            .await
        ),
        "/backups?message=Uploaded%20mine.json"
    );
    assert!(f.dir.join("backups").join("mine.json").is_file());

    assert_eq!(
        landed(&post(&f.panel, "/backups/mine.json/delete", "").await),
        "/backups?message=Deleted%20mine.json"
    );
    assert!(!f.dir.join("backups").join("mine.json").exists());
    assert!(landed(&post(&f.panel, "/backups/mine.json/delete", "").await).contains("not%20found"));
}
