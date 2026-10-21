//! Skills through `Panel::handle`: the compile partial, list and detail,
//! delete, and an export and import round trip.

use std::path::PathBuf;
use std::sync::Arc;

use axum::http::{Request, Response};
use omnimem_core::{EmbeddingError, TextEmbedder, VECTOR_DIM};
use omnimem_engine::{Engine, EngineConfig};
use omnimem_settings::Panel;
use omnimem_store::{Fields, Store};

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

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("omnimem-{name}-{}", ulid::Ulid::generate()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn setup() -> (Panel, Arc<Engine>, PathBuf) {
    let engine = Arc::new(Engine::new(
        Arc::new(Store::open_in_memory().unwrap()),
        Arc::new(Flat),
        EngineConfig::default(),
    ));
    let panel = Panel::new();
    panel.set_engine(engine.clone());
    let dir = scratch("skills");
    panel.set_downloads_dir(dir.join("Downloads"));
    panel.set_feeds_path(dir.join("feeds.yml"));
    (panel, engine, dir)
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

async fn upload(panel: &Panel, filename: &str, data: &[u8]) -> Response<Vec<u8>> {
    let mut body = format!(
        "--XBOUNDARY\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/zip\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(data);
    body.extend_from_slice(b"\r\n--XBOUNDARY--\r\n");
    panel
        .handle(
            Request::post("omnimem://localhost/skills/import")
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

/// A compiled skill as `compile_skill(mode="write")` stores one, with one
/// source memory.
fn compiled_skill(engine: &Engine) -> (String, String) {
    let source = engine
        .remember(
            "Run clippy with warnings denied before every commit",
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
    let key = "mem:skill:gen:rust-local".to_owned();
    let fields = Fields::from([
        ("name".to_owned(), "rust-local".to_owned()),
        (
            "description".to_owned(),
            "How rust work is done here".to_owned(),
        ),
        ("domain".to_owned(), "rust".to_owned()),
        ("user".to_owned(), "local".to_owned()),
        (
            "body".to_owned(),
            "---\nname: rust-local\n---\n\n# Rust\n\n- Run clippy\n".to_owned(),
        ),
        ("generated".to_owned(), "true".to_owned()),
        ("state".to_owned(), "active".to_owned()),
        ("surface_score".to_owned(), "1.0".to_owned()),
        ("contract_version".to_owned(), "1".to_owned()),
        ("compiled_at".to_owned(), "1757000000.0".to_owned()),
        ("created_at".to_owned(), "1757000000.0".to_owned()),
        ("updated_at".to_owned(), "1757000000.0".to_owned()),
        ("tags".to_owned(), r#"["rust"]"#.to_owned()),
        ("source_manifest".to_owned(), format!(r#"["{source}"]"#)),
        (
            "rule_manifest".to_owned(),
            format!(
                r#"[{{"kind": "do", "text": "Run clippy with warnings denied.", "sources": ["{source}"], "reinforcement": 2}}]"#
            ),
        ),
    ]);
    let vector = engine.embed_texts(&["rust-local"]).unwrap().remove(0);
    engine.store().upsert(&key, &fields, Some(&vector)).unwrap();
    (key, source)
}

#[tokio::test]
async fn the_new_skill_modal_refuses_what_it_should() {
    let (panel, engine, dir) = setup();
    compiled_skill(&engine);

    assert!(
        text(&post(&panel, "/skills/compile", "domain=+").await)
            .contains("Enter a domain, e.g. python.")
    );
    assert!(
        text(&post(&panel, "/skills/compile", "domain=Not+Valid%21").await)
            .contains("Invalid domain.")
    );
    let exists = text(&post(&panel, "/skills/compile", "domain=rust").await);
    assert!(exists.contains("already exists"), "{exists}");
    assert!(exists.contains(r#"<a href="/skills/mem:skill:gen:rust-local">rust-local</a>"#));

    // An alias resolves first: golang is reported as go.
    let aliased = text(&post(&panel, "/skills/compile", "domain=golang").await);
    assert!(
        aliased.contains("No active episodic memories are tagged <strong>go</strong>"),
        "{aliased}"
    );
    let none = text(&post(&panel, "/skills/compile", "domain=haskell").await);
    assert!(
        none.contains("No active episodic memories are tagged <strong>haskell</strong>"),
        "{none}"
    );
    assert!(
        none.contains("rust (1)"),
        "known domains are listed: {none}"
    );

    let no_proposal = text(&post(&panel, "/skills/commit", "domain=haskell").await);
    assert!(no_proposal.contains("flash-error"), "{no_proposal}");
    assert!(no_proposal.contains("Nothing proposed for this domain"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn skills_list_show_and_delete() {
    let (panel, engine, dir) = setup();
    let (key, source) = compiled_skill(&engine);

    let list = text(&get(&panel, "/skills").await);
    assert!(
        list.contains(r#"<span class="badge" style="font-size:0.75rem">1</span>"#),
        "{list}"
    );
    assert!(list.contains(&format!(
        r#"<a href="/skills/{key}" class="project-name">rust-local</a>"#
    )));
    assert!(list.contains("1 active"));
    assert!(list.contains("class=\"nav-link active\">Compiled Skills"));

    let detail = text(&get(&panel, &format!("/skills/{key}")).await);
    assert!(
        detail.contains("Run clippy with warnings denied."),
        "{detail}"
    );
    assert!(
        detail.contains("(1 do · 0 watch · 0 don&#39;t)")
            || detail.contains("(1 do · 0 watch · 0 don't)"),
        "{detail}"
    );
    assert!(detail.contains(&format!(r#"<a href="/memory/{source}""#)));
    assert!(detail.contains("# Rust"));
    assert_eq!(
        get(&panel, "/skills/mem:skill:gen:nothing").await.status(),
        404
    );
    assert_eq!(get(&panel, "/skills/mem:episodic:x").await.status(), 404);

    assert_eq!(
        landed(&post(&panel, "/skills/delete", &format!("key={key}")).await),
        "/skills"
    );
    assert!(engine.store().get(&key).unwrap().is_none());
    assert!(
        engine.store().get(&source).unwrap().is_some(),
        "sources survive"
    );
    assert_eq!(
        post(&panel, "/skills/delete", &format!("key={key}"))
            .await
            .status(),
        404
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn a_bundle_exports_to_downloads_and_imports_after_confirming() {
    let (panel, engine, dir) = setup();
    let (key, source) = compiled_skill(&engine);

    let exported = landed(&get(&panel, &format!("/skills/export/{key}")).await);
    assert!(
        exported.starts_with("/skills?message=Exported%20to%20"),
        "{exported}"
    );
    let downloads = dir.join("Downloads");
    let files: Vec<PathBuf> = std::fs::read_dir(&downloads)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    let bundle = std::fs::read(&files[0]).unwrap();
    landed(&get(&panel, &format!("/skills/export/{key}")).await);
    assert_eq!(
        std::fs::read_dir(&downloads).unwrap().count(),
        2,
        "a second export doesn't overwrite"
    );
    assert_eq!(
        get(&panel, "/skills/export/mem:skill:gen:nothing")
            .await
            .status(),
        404
    );

    // Import into a store that has neither the skill nor its source.
    engine.store().delete(&key).unwrap();
    engine.store().delete(&source).unwrap();

    assert!(text(&upload(&panel, "notes.txt", b"hello").await).contains("Only .zip bundles"));
    assert!(text(&upload(&panel, "broken.zip", b"not a zip").await).contains("flash-error"));

    let preview = text(&upload(&panel, "rust-local.zip", &bundle).await);
    assert!(preview.contains("Bundle validated"), "{preview}");
    assert!(preview.contains("Skill will be created at"));
    assert!(
        preview.contains("1 source\n    memory will be added")
            || preview.contains("memory will be added"),
        "{preview}"
    );
    assert!(
        engine.store().get(&key).unwrap().is_none(),
        "a preview writes nothing"
    );
    let token = preview
        .split(r#"name="token" value=""#)
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap()
        .to_owned();

    let confirmed = panel
        .handle(
            Request::post("omnimem://localhost/skills/import/confirm")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(format!("token={token}").into_bytes())
                .unwrap(),
        )
        .await;
    assert_eq!(
        confirmed.headers()["HX-Redirect"],
        "/skills?message=Skill%20imported%3B%201%20memory%20added."
    );
    assert!(engine.store().get(&key).unwrap().is_some());
    assert!(engine.store().get(&source).unwrap().is_some());

    let again = text(&post(&panel, "/skills/import/confirm", &format!("token={token}")).await);
    assert!(again.contains("has expired"), "a token works once: {again}");
    assert!(
        text(&post(&panel, "/skills/import/confirm", "token=%3Cx%3E").await)
            .contains("Invalid import token.")
    );

    let shown = text(
        &get(
            &panel,
            "/skills?message=Skill%20imported%3B%201%20memory%20added.",
        )
        .await,
    );
    assert!(shown.contains(
        r#"<div class="flash flash-success"><span>Skill imported; 1 memory added.</span></div>"#
    ));
    std::fs::remove_dir_all(dir).unwrap();
}
