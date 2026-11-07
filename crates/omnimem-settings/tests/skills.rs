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
    assert_eq!(response.status(), 303, "{}", text(response));
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
    assert!(exists.contains(r#"<a href="/admin/skills/mem:skill:gen:rust-local">rust-local</a>"#));

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
        list.contains(r#"<span class="count-chip">1</span>"#),
        "{list}"
    );
    assert!(list.contains(&format!(
        r#"<a href="/admin/skills/{key}" class="project-name" title="rust-local">rust-local</a>"#
    )));
    assert!(list.contains("1 active"));
    assert!(list.contains("aria-current=\"page\"><span class=\"nav-label\">Skills"));

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
    assert!(detail.contains(&format!(r#"<a href="/admin/memory/{}?back="#, source)));
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
async fn a_bundle_downloads_and_imports_after_confirming() {
    let (panel, engine, dir) = setup();
    let (key, source) = compiled_skill(&engine);

    let exported = get(&panel, &format!("/skills/export/{key}")).await;
    assert_eq!(exported.status(), 200);
    assert_eq!(exported.headers()["content-type"], "application/zip");
    let disposition = exported.headers()["content-disposition"].to_str().unwrap();
    assert!(
        disposition.starts_with("attachment; filename=\"") && disposition.ends_with(".zip\""),
        "{disposition}"
    );
    let bundle = exported.body().clone();
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
        r#"<div class="flash flash-success" role="status"><span>Skill imported; 1 memory added.</span></div>"#
    ));
    std::fs::remove_dir_all(dir).unwrap();
}

const PROPOSAL: &str = "meta:skill:proposal:rust-local";
const DRAFT: &str = "---\nname: rust-local\n---\n\n# Rust\n\n- Run clippy\n- Run the tests\n";

/// A proposal as `compile_skill(mode="propose")` stashes one, which is all
/// the panel ever sees of an agent's: no memories behind it are needed.
fn stash_proposal(engine: &Engine, based_on: &str) {
    let fields = Fields::from([
        ("body".to_owned(), DRAFT.to_owned()),
        ("description".to_owned(), "How rust work is done".to_owned()),
        ("domain".to_owned(), "rust".to_owned()),
        ("user".to_owned(), "local".to_owned()),
        ("based_on".to_owned(), based_on.to_owned()),
        ("created_at".to_owned(), "1757000000.0".to_owned()),
        ("rule_manifest".to_owned(), r#"[{"kind": "do"}]"#.to_owned()),
        ("source_manifest".to_owned(), "[]".to_owned()),
    ]);
    engine.store().hash_set(PROPOSAL, &fields).unwrap();
}

#[tokio::test]
async fn pending_proposals_list_with_their_draft_and_count() {
    let (panel, engine, dir) = setup();
    let none = text(&get(&panel, "/skills").await);
    assert!(!none.contains("Pending proposals"), "{none}");

    // Stashed straight into the store, as an agent's proposal is: the menu's
    // count is cached for a short while, so it isn't read before this.
    stash_proposal(&engine, "");
    let list = text(&get(&panel, "/skills").await);
    assert!(list.contains("Pending proposals"), "{list}");
    assert!(list.contains("new skill"), "{list}");
    assert!(list.contains("# Rust"), "the draft is shown: {list}");
    assert!(list.contains("Accept &amp; write skill"));
    assert!(list.contains(r#"action="/admin/skills/discard""#));
    // The sidebar loads every review count in one request, out of band.
    assert!(list.contains(r#"hx-get="/admin/nav-counts""#));
    let counts = text(&get(&panel, "/nav-counts").await);
    assert!(
        counts.contains(r#"id="nav-count-skills" hx-swap-oob="true"><span class="nav-count""#)
            && counts.contains(">1</span>"),
        "{counts}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn accepting_a_pending_proposal_commits_the_stash() {
    let (panel, engine, dir) = setup();
    stash_proposal(&engine, "");

    let accepted = post(&panel, "/skills/commit", "domain=rust").await;
    assert_eq!(
        accepted.headers()["HX-Redirect"],
        "/skills/mem:skill:gen:rust-local"
    );
    let stored = engine
        .store()
        .get("mem:skill:gen:rust-local")
        .unwrap()
        .unwrap();
    assert_eq!(stored["body"], DRAFT, "byte for byte");
    assert!(engine.store().hash_get_all(PROPOSAL).unwrap().is_none());
    assert!(
        !text(&get(&panel, "/nav-counts").await)
            .contains(r#"nav-count-skills" hx-swap-oob="true"><span"#),
        "no proposal, no count"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn discarding_a_pending_proposal_writes_nothing() {
    let (panel, engine, dir) = setup();
    stash_proposal(&engine, "");

    let gone = landed(&post(&panel, "/skills/discard", "domain=rust").await);
    assert!(gone.starts_with("/skills?message="), "{gone}");
    assert!(engine.store().hash_get_all(PROPOSAL).unwrap().is_none());
    assert!(
        engine
            .store()
            .get("mem:skill:gen:rust-local")
            .unwrap()
            .is_none()
    );

    // Discarding what has already gone is said, not an error page.
    let again = landed(&post(&panel, "/skills/discard", "domain=rust").await);
    assert!(again.starts_with("/skills?error="), "{again}");
    let invalid = landed(&post(&panel, "/skills/discard", "domain=Not+Valid!").await);
    assert!(invalid.starts_with("/skills?error="), "{invalid}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn a_stale_pending_proposal_cannot_be_accepted() {
    let (panel, engine, dir) = setup();
    compiled_skill(&engine);
    // Proposed against a body the stored skill no longer has.
    stash_proposal(&engine, "0123abcd");

    let list = text(&get(&panel, "/skills").await);
    assert!(list.contains("changed after this was proposed"), "{list}");
    assert!(!list.contains("Accept &amp; write skill"), "{list}");
    assert!(
        !text(&get(&panel, "/nav-counts").await)
            .contains(r#"nav-count-skills" hx-swap-oob="true"><span"#),
        "no proposal, no count"
    );

    let refused = text(&post(&panel, "/skills/commit", "domain=rust").await);
    assert!(refused.contains("stored skill changed"), "{refused}");
    let stored = engine
        .store()
        .get("mem:skill:gen:rust-local")
        .unwrap()
        .unwrap();
    assert_ne!(stored["body"], DRAFT);
    std::fs::remove_dir_all(dir).unwrap();
}
