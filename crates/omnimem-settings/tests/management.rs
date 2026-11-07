//! Duplicates, contradictions, suppressions, telemetry and token overhead
//! through `Panel::handle`.

use std::sync::Arc;

use axum::http::{Request, Response};
use omnimem_core::{EmbeddingError, TextEmbedder, VECTOR_DIM};
use omnimem_engine::{Engine, EngineConfig, ResolveRequest, Retire};
use omnimem_settings::{Panel, StaticOverhead};
use omnimem_store::{Fields, Store};
use serde_json::Value;

/// Texts of the same length embed identically, so duplicates are easy to make.
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

fn setup() -> (Panel, Arc<Engine>) {
    let engine = Arc::new(Engine::new(
        Arc::new(Store::open_in_memory().unwrap()),
        Arc::new(Flat),
        EngineConfig::default(),
    ));
    let panel = Panel::new();
    panel.set_engine(engine.clone());
    (panel, engine)
}

fn remember(engine: &Engine, content: &str, project: &str) -> String {
    engine
        .remember(
            content,
            Some(project),
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

fn maintenance_ran(engine: &Engine) {
    engine
        .store()
        .hash_set(
            "meta:maintenance:omnimem",
            &Fields::from([
                ("last_maintenance_at".to_owned(), "1757000000.0".to_owned()),
                (
                    "last_maintenance_summary".to_owned(),
                    r#"{"duplicates_archived": 2, "contradictions_found": 1}"#.to_owned(),
                ),
            ]),
        )
        .unwrap();
}

#[tokio::test]
async fn duplicates_scan_and_maintenance_is_noted() {
    let (panel, engine) = setup();
    maintenance_ran(&engine);
    let first = remember(&engine, "Twins share a length: one", "omnimem");
    let second = remember(&engine, "Twins share a length: two", "omnimem");
    remember(
        &engine,
        "Something else entirely, a different length",
        "omnimem",
    );

    let page = text(&get(&panel, "/duplicates").await);
    assert!(page.contains("04 Sep 2025 15:33 UTC"), "{page}");
    assert!(
        page.contains("project omnimem,\n  2 duplicates archived"),
        "{page}"
    );
    assert!(page.contains("aria-current=\"page\"><span class=\"nav-label\">Duplicates"));

    let scan = text(&get(&panel, "/duplicates/scan?namespace=episodic").await);
    assert!(
        scan.contains("1 cluster of near-identical episodic memories"),
        "{scan}"
    );
    assert!(scan.contains(&first) && scan.contains(&second));
    assert!(scan.contains("similarity 1.0"), "{scan}");

    // A scan has an address, and its Archive and memory links come back to it.
    let here = "/duplicates?namespace=episodic&scan=1";
    let page = text(&get(&panel, here).await);
    assert!(
        page.contains("1 cluster of near-identical episodic memories"),
        "{page}"
    );
    assert!(
        page.contains(&format!(
            r#"name="next" value="{}""#,
            here.replace('&', "&amp;")
        )),
        "{page}"
    );
    assert!(
        page.contains("?back=/duplicates%3Fnamespace%3Depisodic%26scan%3D1"),
        "{page}"
    );
    assert!(
        text(&get(&panel, "/duplicates/scan?namespace=knowledge").await)
            .contains("No duplicates found in knowledge.")
    );
}

#[tokio::test]
async fn contradictions_list_each_pair_once() {
    let (panel, engine) = setup();
    maintenance_ran(&engine);
    let a = remember(&engine, "Always squash merge", "omnimem");
    let b = remember(&engine, "Never squash merge", "omnimem");
    engine
        .store()
        .set_field(
            &a,
            "contradictions",
            &format!(r#"[{{"key": "{b}", "explanation": "They disagree", "similarity": 0.81}}]"#),
        )
        .unwrap();
    engine
        .store()
        .set_field(&b, "contradictions", &format!(r#"[{{"key": "{a}"}}]"#))
        .unwrap();

    let page = text(&get(&panel, "/contradictions").await);
    assert!(
        page.contains(r#"Confirmed by the model <span class="count-chip">1</span>"#),
        "{page}"
    );
    assert!(!page.contains("pairs"));
    assert!(page.contains("They disagree"));
    assert!(page.contains("Always squash merge") && page.contains("Never squash merge"));
    assert!(page.contains("1 pair linked in that run."));
}

#[tokio::test]
async fn a_pair_shows_its_explanation_whichever_side_recorded_it() {
    // Two memories made in the same millisecond can sort either way, so the
    // side holding the explanation may be met first or second: both orders.
    for explained_first in [true, false] {
        let (panel, engine) = setup();
        let a = remember(&engine, "Deploy on Fridays", "omnimem");
        let b = remember(&engine, "Never deploy on a Friday", "omnimem");
        let (mut first, mut second) = if a < b { (a, b) } else { (b, a) };
        if !explained_first {
            std::mem::swap(&mut first, &mut second);
        }
        engine
            .store()
            .set_field(
                &first,
                "contradictions",
                &format!(r#"[{{"key": "{second}", "explanation": "Opposite advice", "similarity": 0.77}}]"#),
            )
            .unwrap();
        engine
            .store()
            .set_field(
                &second,
                "contradictions",
                &format!(r#"[{{"key": "{first}"}}]"#),
            )
            .unwrap();

        let page = text(&get(&panel, "/contradictions").await);
        assert!(
            page.contains(r#"Confirmed by the model <span class="count-chip">1</span>"#),
            "{page}"
        );
        assert!(
            page.contains("Opposite advice"),
            "explanation on the {} side lost: {page}",
            if explained_first { "first" } else { "second" }
        );
    }
}

/// Two memories linked as contradicting each other, both rows.
fn linked_pair(engine: &Engine) -> (String, String) {
    let a = remember(engine, "Always squash merge", "omnimem");
    let b = remember(engine, "Never squash merge", "omnimem");
    for (key, other) in [(&a, &b), (&b, &a)] {
        engine
            .store()
            .set_field(
                key,
                "contradictions",
                &format!(r#"[{{"key": "{other}", "explanation": "They disagree"}}]"#),
            )
            .unwrap();
    }
    (a, b)
}

fn form_keys(a: &str, b: &str) -> String {
    format!(
        "key_a={}&key_b={}",
        a.replace(':', "%3A"),
        b.replace(':', "%3A")
    )
}

fn redirect(response: &Response<Vec<u8>>) -> String {
    response.headers()["x-omnimem-redirect"]
        .to_str()
        .unwrap()
        .to_owned()
}

fn resolution_of(engine: &Engine, key: &str) -> Value {
    let raw = engine.store().get(key).unwrap().unwrap()["contradictions"].clone();
    serde_json::from_str::<Vec<Value>>(&raw).unwrap()[0]["resolution"].clone()
}

#[tokio::test]
async fn the_page_offers_resolve_actions_in_place_of_the_archive_buttons() {
    let (panel, engine) = setup();
    linked_pair(&engine);
    let page = text(&get(&panel, "/contradictions").await);
    for label in [
        "A is current",
        "B is current",
        "Both stand",
        "Not a contradiction",
    ] {
        assert!(page.contains(label), "{label} missing: {page}");
    }
    assert!(page.contains("action=\"/admin/contradictions/resolve\""));
    assert!(page.contains("name=\"reason\""));
    assert!(page.contains("name=\"retire_loser\""));
    assert!(!page.contains("Archive A") && !page.contains("/lifecycle/archive"));
}

#[tokio::test]
async fn a_click_resolves_the_pair_and_it_leaves_the_page() {
    let (panel, engine) = setup();
    let (a, b) = linked_pair(&engine);
    let form = format!(
        "{}&choice=b&reason=we+moved&retire_loser=1",
        form_keys(&a, &b)
    );
    let response = post(&panel, "/contradictions/resolve", &form).await;
    assert_eq!(redirect(&response), "/contradictions?message=Resolved.");

    for key in [&a, &b] {
        let r = resolution_of(&engine, key);
        assert_eq!(r["outcome"], "supersedes");
        assert_eq!(r["winner"], b.as_str());
        assert_eq!(r["reason"], "we moved");
        assert_eq!(r["resolved_by"], "panel");
    }
    assert_eq!(
        engine.store().get(&a).unwrap().unwrap()["state"],
        "deprioritised"
    );
    assert_eq!(engine.store().get(&b).unwrap().unwrap()["state"], "active");
    let page = text(&get(&panel, "/contradictions").await);
    assert!(page.contains("No contradictions detected."), "{page}");

    // The memory's page shows what was decided.
    let detail = text(&get(&panel, &format!("/memory/{a}")).await);
    assert!(
        detail.contains("Resolved:") && detail.contains("supersedes"),
        "{detail}"
    );
    assert!(detail.contains("Replaced by"), "{detail}");
}

#[tokio::test]
async fn both_stand_and_not_a_contradiction_keep_both_memories_as_they_are() {
    for (choice, outcome) in [("both", "both_valid"), ("not", "false_positive")] {
        let (panel, engine) = setup();
        let (a, b) = linked_pair(&engine);
        // The box is for a side that lost, so it does nothing here.
        let form = format!("{}&choice={choice}&retire_loser=1", form_keys(&a, &b));
        post(&panel, "/contradictions/resolve", &form).await;
        assert_eq!(resolution_of(&engine, &a)["outcome"], outcome);
        assert_eq!(engine.store().get(&b).unwrap().unwrap()["state"], "active");
        assert_eq!(engine.store().get(&a).unwrap().unwrap()["state"], "active");
    }
    let (panel, engine) = setup();
    let (a, b) = linked_pair(&engine);
    let response = post(&panel, "/contradictions/resolve", &form_keys(&a, &b)).await;
    assert!(
        redirect(&response).starts_with("/contradictions?error="),
        "{}",
        redirect(&response)
    );
}

#[tokio::test]
async fn pending_resolutions_are_listed_to_accept_or_discard() {
    let (panel, engine) = setup();
    let (a, b) = linked_pair(&engine);
    let request = ResolveRequest::new(
        &a,
        &b,
        "supersedes",
        Some(&a),
        Some("agent says so"),
        Retire::No,
    )
    .unwrap();
    engine.propose_resolution(&request, "mcp").unwrap();

    let page = text(&get(&panel, "/contradictions").await);
    assert!(
        page.contains("Proposed by an agent") && page.contains("agent says so"),
        "{page}"
    );
    assert!(page.contains("action=\"/admin/contradictions/accept\""));
    assert!(page.contains("action=\"/admin/contradictions/discard\""));

    // Discard writes nothing.
    let response = post(&panel, "/contradictions/discard", &form_keys(&a, &b)).await;
    assert_eq!(
        redirect(&response),
        "/contradictions?message=Proposal%20discarded."
    );
    assert!(engine.pending_resolutions().unwrap().is_empty());
    assert!(!text(&get(&panel, "/contradictions").await).contains("agent says so"));

    // Accept writes exactly the proposal; the click is the approval.
    engine.propose_resolution(&request, "mcp").unwrap();
    let response = post(&panel, "/contradictions/accept", &form_keys(&a, &b)).await;
    assert_eq!(redirect(&response), "/contradictions?message=Resolved.");
    let r = resolution_of(&engine, &b);
    assert_eq!(
        (r["outcome"].as_str(), r["winner"].as_str()),
        (Some("supersedes"), Some(a.as_str()))
    );
    assert_eq!(r["reason"], "agent says so");
    assert_eq!(r["resolved_by"], "panel");

    // Once it has gone, accepting says so.
    let response = post(&panel, "/contradictions/accept", &form_keys(&a, &b)).await;
    assert!(
        redirect(&response).contains("already"),
        "{}",
        redirect(&response)
    );
}

#[tokio::test]
async fn topics_are_suppressed_and_released() {
    let (panel, engine) = setup();
    assert!(
        text(&get(&panel, "/suppressions").await).contains("No topics are currently suppressed.")
    );
    let added = text(&post(&panel, "/suppressions/add", "topic=Kubernetes").await);
    assert!(added.to_lowercase().contains("kubernetes"), "{added}");
    let listed = engine.list_suppressions().unwrap()["suppressed_topics"].to_string();
    assert!(listed.to_lowercase().contains("kubernetes"));
    assert!(text(&post(&panel, "/suppressions/add", "topic=++").await).contains("<table"));

    let removed = text(&post(&panel, "/suppressions/remove", "topic=kubernetes").await);
    assert!(
        removed.contains("No topics are currently suppressed."),
        "{removed}"
    );
}

#[tokio::test]
async fn the_suppressions_page_shows_each_topics_reason() {
    let (panel, engine) = setup();
    engine
        .suppress_topic("kubernetes", Some("far too noisy"))
        .unwrap();
    engine.suppress_topic("docker", None).unwrap();

    let page = text(&get(&panel, "/suppressions").await);
    assert!(page.contains("far too noisy"), "{page}");
    assert!(page.contains("docker"), "{page}");

    let removed = text(&post(&panel, "/suppressions/remove", "topic=kubernetes").await);
    assert!(!removed.contains("far too noisy"), "{removed}");
}

#[tokio::test]
async fn telemetry_ranks_recalls_and_finds_what_went_cold() {
    let (panel, engine) = setup();
    let hot = remember(&engine, "Recalled often", "omnimem");
    let cold = remember(&engine, "Recalled long ago", "omnimem");
    remember(&engine, "Never recalled at all", "other");
    let now = omnimem_engine::pyfmt::now_str();
    engine
        .store()
        .bump_recall_counts(std::slice::from_ref(&hot), &now)
        .unwrap();
    engine
        .store()
        .bump_recall_counts(std::slice::from_ref(&hot), &now)
        .unwrap();
    engine
        .store()
        .bump_recall_counts(std::slice::from_ref(&cold), "1000.0")
        .unwrap();

    let page = text(&get(&panel, "/telemetry").await);
    assert!(
        page.contains(r#"<div class="stat-total" title="3">3</div>"#),
        "{page}"
    );
    let most = page.find("Most recalled").unwrap();
    let gone = page.find("Gone cold").unwrap();
    let never = page.find("Never recalled <span").unwrap();
    assert!(page[most..gone].find("Recalled often") < page[most..gone].find("Recalled long ago"));
    assert!(page[gone..never].contains("Recalled long ago"));
    assert!(page[never..].contains("Never recalled at all"));
    assert!(page.contains("aria-current=\"page\"><span class=\"nav-label\">Telemetry"));

    let filtered = text(&get(&panel, "/telemetry/refresh?project=other").await);
    assert!(!filtered.contains("<html"));
    assert!(filtered.contains("Never recalled at all"));
    assert!(!filtered.contains("Recalled often"));
}

#[tokio::test]
async fn token_overhead_measures_and_resets() {
    let (panel, engine) = setup();
    panel.set_static_overhead(StaticOverhead {
        instructions_chars: 14_162,
        tool_count: 50,
        tool_schemas_chars: 40_000,
        deferred_names_chars: 1_000,
    });
    remember(&engine, "Twelve chars", "omnimem");
    let metrics = "meta:tool_metrics:recall";
    engine.store().hash_incr(metrics, "call_count", 3).unwrap();
    engine
        .store()
        .hash_incr(metrics, "total_duration_ms", 100)
        .unwrap();
    engine
        .store()
        .hash_incr(metrics, "total_response_chars", 1000)
        .unwrap();

    let page = text(&get(&panel, "/token-overhead").await);
    assert!(page.contains("55,162"), "static characters: {page}");
    assert!(page.contains("~13,790"), "static tokens: {page}");
    assert!(page.contains("Tool schemas (50 tools)"));
    assert!(page.contains(r#"<td class="mono">recall</td>"#));
    assert!(page.contains("33.3ms"), "{page}");
    assert!(page.contains("~83"), "average tokens");
    assert!(page.contains("aria-current=\"page\"><span class=\"nav-label\">Telemetry"));

    let reset = post(&panel, "/token-overhead/reset", "").await;
    assert_eq!(reset.headers()["x-omnimem-redirect"], "/token-overhead");
    let after = text(&get(&panel, "/token-overhead/refresh").await);
    assert!(
        after.contains("No tool calls recorded since the counters were reset"),
        "{after}"
    );
    assert!(engine.store().hash_get_all(metrics).unwrap().is_none());
}

#[tokio::test]
async fn the_apps_notice_tops_every_page() {
    let (panel, _engine) = setup();
    assert!(!text(&get(&panel, "/").await).contains("flash-warning"));
    panel.set_notice("No tray icon here: install the AppIndicator extension".to_owned());
    for path in ["/", "/memories", "/configuration"] {
        let page = text(&get(&panel, path).await);
        assert!(
            page.contains("No tray icon here: install the AppIndicator extension"),
            "{path}: {page}"
        );
    }
}

fn link(engine: &Engine, a: &str, b: &str, explanation: &str) {
    engine
        .store()
        .set_field(
            a,
            "contradictions",
            &format!(r#"[{{"key": "{b}", "explanation": "{explanation}"}}]"#),
        )
        .unwrap();
    engine
        .store()
        .set_field(b, "contradictions", &format!(r#"[{{"key": "{a}"}}]"#))
        .unwrap();
}

fn encode(text: &str) -> String {
    text.replace(':', "%3A").replace('|', "%7C")
}

#[tokio::test]
async fn wording_only_pairs_are_counted_apart_and_dismissed_only_as_shown() {
    let (panel, engine) = setup();
    let (a, b) = (
        remember(&engine, "Always rebase", "omnimem"),
        remember(&engine, "Never rebase", "omnimem"),
    );
    let (c, d) = (
        remember(&engine, "Always tag releases", "omnimem"),
        remember(&engine, "Never tag releases", "omnimem"),
    );
    let (e, f) = (
        remember(&engine, "Ship on Fridays", "omnimem"),
        remember(&engine, "Don't ship on Fridays", "omnimem"),
    );
    link(&engine, &a, &b, omnimem_store::HEURISTIC_LINK_EXPLANATION);
    link(&engine, &c, &d, omnimem_store::HEURISTIC_LINK_EXPLANATION);
    link(&engine, &e, &f, "A model read both: they disagree");

    // The menu counts what needs a decision, not wording-only pairs.
    let counts = text(&get(&panel, "/nav-counts").await);
    assert!(
        counts.contains(r#"id="nav-count-contradictions" hx-swap-oob="true"><span class="nav-count" title="1 contradiction to settle">1</span>"#),
        "{counts}"
    );
    let page = text(&get(&panel, "/contradictions").await);
    assert!(
        page.contains(r#"Contradictions <span class="count-chip">1</span>"#),
        "{page}"
    );
    assert!(page.contains("and 2 wording only"), "{page}");

    // Dismissing posts the pairs the page showed. One that wasn't posted,
    // and the model-confirmed one even if posted, stay open.
    let (lo, hi) = if a < b { (&a, &b) } else { (&b, &a) };
    let (mlo, mhi) = if e < f { (&e, &f) } else { (&f, &e) };
    let form = format!(
        "pair={}&pair={}",
        encode(&format!("{lo}|{hi}")),
        encode(&format!("{mlo}|{mhi}"))
    );
    let done = post(&panel, "/contradictions/dismiss-wording", &form).await;
    let location = done.headers()["location"].to_str().unwrap().to_owned();
    assert!(
        location.contains("Dismissed%201%20wording-only%20pair"),
        "{location}"
    );
    let open = engine.open_contradictions().unwrap();
    assert_eq!(open.len(), 2, "{open:?}");
    let still = |x: &str| open.iter().any(|p| p["key_a"] == x || p["key_b"] == x);
    assert!(!still(&a) && still(&c) && still(&e));
}

#[tokio::test]
async fn keeping_a_duplicate_archives_only_its_scanned_cluster() {
    let (panel, engine) = setup();
    let first = remember(&engine, "Twins share a length: one", "omnimem");
    let second = remember(&engine, "Twins share a length: two", "omnimem");
    let bystander = remember(
        &engine,
        "Something else entirely, a different length",
        "omnimem",
    );

    // Without a scan behind it, nothing is archived.
    let refused = post(
        &panel,
        "/duplicates/keep",
        &format!(
            "key={}&other={}&next=%2Fduplicates",
            encode(&first),
            encode(&bystander)
        ),
    )
    .await;
    assert!(
        refused.headers()["location"]
            .to_str()
            .unwrap()
            .contains("error=")
    );

    // After a scan, the form's `other` is ignored: the cluster decides.
    let scan = text(&get(&panel, "/duplicates?namespace=episodic&scan=1").await);
    assert!(scan.contains("Keep this one"), "{scan}");
    // The dashboard reports the scan, with its age, until its clusters are settled.
    let dashboard = text(&get(&panel, "/?refresh=1").await);
    assert!(
        dashboard.contains("duplicate cluster</span>")
            && dashboard.contains("scan of episodic just now"),
        "{dashboard}"
    );
    let kept = post(
        &panel,
        "/duplicates/keep",
        &format!(
            "key={}&other={}&next=%2Fduplicates%3Fnamespace%3Depisodic%26scan%3D1",
            encode(&first),
            encode(&bystander)
        ),
    )
    .await;
    let location = kept.headers()["location"].to_str().unwrap().to_owned();
    assert!(
        location.contains("Kept%20one%2C%20archived%201."),
        "{location}"
    );
    let state = |key: &str| {
        engine
            .store()
            .get(key)
            .unwrap()
            .unwrap()
            .get("state")
            .cloned()
    };
    assert_eq!(state(&second).as_deref(), Some("archived"));
    assert_ne!(state(&bystander).as_deref(), Some("archived"));
    assert_ne!(state(&first).as_deref(), Some("archived"));

    // Back on the scan, the cluster is settled without a rescan.
    let again = text(&get(&panel, "/duplicates?namespace=episodic&scan=1").await);
    assert!(
        again.contains("No duplicates found in episodic."),
        "{again}"
    );
    // Settled from anywhere, the cluster leaves the dashboard too.
    let dashboard = text(&get(&panel, "/?refresh=1").await);
    assert!(!dashboard.contains("duplicate cluster"), "{dashboard}");
}
