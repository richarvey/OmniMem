//! `omnimem contradictions` on a real database file.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use omnimem_store::{Actor, AuditLevel, Fields, Store, enter};
use serde_json::{Value, json};

const KEY_A: &str = "mem:episodic:01A";
const KEY_B: &str = "mem:episodic:01B";

/// A scratch folder with an empty settings file, so no setting on the
/// machine running the tests reaches the binary.
mod common;

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("omnimem-cli-c-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("empty.env"), "").unwrap();
        Self(dir)
    }

    fn run(&self, db: &Path, args: &[&str]) -> Output {
        self.run_logged(db, args, None)
    }

    /// As `run`, with `AUDIT_LOG` set to `level`.
    fn run_logged(&self, db: &Path, args: &[&str], level: Option<&str>) -> Output {
        // A person-only command runs at a terminal, answered as a person.
        if let Some(key) = common::person_only_key(args) {
            let mut all: Vec<std::ffi::OsString> = vec!["--db".into(), db.into()];
            all.extend(args.iter().map(Into::into));
            let mut envs = vec![(
                "OMNIMEM_ENV_FILE",
                self.0.join("empty.env").into_os_string(),
            )];
            let mut remove = vec!["OMNIMEM_DB"];
            match level {
                Some(level) => envs.push(("AUDIT_LOG", level.into())),
                None => remove.push("AUDIT_LOG"),
            }
            return common::as_output(common::in_terminal(
                Path::new(env!("CARGO_BIN_EXE_omnimem")),
                &all,
                &envs,
                &remove,
                &common::tail(key),
            ));
        }
        self.piped(db, args, level)
    }

    /// With stdin and stdout piped, as an agent's shell runs it.
    fn piped(&self, db: &Path, args: &[&str], level: Option<&str>) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_omnimem"));
        if let Some(level) = level {
            command.env("AUDIT_LOG", level);
        } else {
            command.env_remove("AUDIT_LOG");
        }
        command
            .env("OMNIMEM_ENV_FILE", self.0.join("empty.env"))
            .env_remove("OMNIMEM_DB")
            .arg("--db")
            .arg(db)
            .args(args)
            .output()
            .unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A database with two linked memories.
fn linked(scratch: &Scratch) -> PathBuf {
    let db = scratch.0.join("omnimem.db");
    let store = Store::open(&db).unwrap();
    store.set_audit_level(AuditLevel::Lifecycle);
    let _who = enter(Actor::Cli, "");
    for (key, other, content) in [
        (KEY_A, KEY_B, "always squash merge"),
        (KEY_B, KEY_A, "never squash merge"),
    ] {
        let links = json!([{"key": other, "explanation": "Opposite advice", "detected_at": "1"}]);
        let fields: Fields = [
            ("content", content),
            ("state", "active"),
            ("project", "alpha"),
            ("created_at", "1.0"),
            ("updated_at", "1.0"),
            ("contradictions", links.to_string().as_str()),
        ]
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
        store.upsert(key, &fields, None).unwrap();
    }
    db
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn links(db: &Path, key: &str) -> Vec<Value> {
    let store = Store::open(db).unwrap();
    serde_json::from_str(&store.get(key).unwrap().unwrap()["contradictions"]).unwrap()
}

#[test]
fn the_list_shows_open_pairs_and_resolve_settles_one() {
    let scratch = Scratch::new("resolve");
    let db = linked(&scratch);

    let listed = scratch.run(&db, &["contradictions"]);
    assert!(listed.status.success());
    let text = stdout(&listed);
    assert!(
        text.contains(KEY_A) && text.contains("never squash merge"),
        "{text}"
    );

    let done = scratch.run_logged(
        &db,
        &[
            "contradictions",
            "resolve",
            KEY_A,
            KEY_B,
            "--outcome",
            "supersedes",
            "--winner",
            KEY_B,
            "--reason",
            "moved to rebase",
            "--retire-loser",
        ],
        Some("lifecycle"),
    );
    assert!(
        done.status.success(),
        "{}",
        String::from_utf8_lossy(&done.stderr)
    );
    assert_eq!(stdout(&done).trim(), "resolved");
    for key in [KEY_A, KEY_B] {
        let r = &links(&db, key)[0]["resolution"];
        assert_eq!(r["outcome"], "supersedes");
        assert_eq!(r["resolved_by"], "cli");
        assert_eq!(r["reason"], "moved to rebase");
    }
    let store = Store::open(&db).unwrap();
    assert_eq!(store.get(KEY_A).unwrap().unwrap()["state"], "deprioritised");
    assert_eq!(store.get(KEY_B).unwrap().unwrap()["state"], "active");

    // The audit row names the command line.
    let rows = scratch.run(
        &db,
        &["audit", "--action", "contradiction_resolve", "--json"],
    );
    let rows = stdout(&rows);
    assert!(
        rows.contains("\"actor\":\"cli\"") || rows.contains("\"actor\": \"cli\""),
        "{rows}"
    );
    assert!(rows.contains("moved to rebase"), "{rows}");

    // Nothing is left in dispute.
    let after = scratch.run(&db, &["contradictions", "--json"]);
    let parsed: Value = serde_json::from_str(&stdout(&after)).unwrap();
    assert_eq!(parsed["open"], json!([]));
    assert_eq!(parsed["pending"], json!([]));
}

#[test]
fn a_bad_request_fails_and_writes_nothing() {
    let scratch = Scratch::new("refused");
    let db = linked(&scratch);
    for args in [
        vec![
            "contradictions",
            "resolve",
            KEY_A,
            KEY_B,
            "--outcome",
            "supersedes",
        ],
        vec![
            "contradictions",
            "resolve",
            KEY_A,
            KEY_B,
            "--outcome",
            "won",
        ],
        vec!["contradictions", "accept", KEY_A, KEY_B],
    ] {
        let out = scratch.run(&db, &args);
        assert!(!out.status.success(), "{args:?}");
    }
    assert!(links(&db, KEY_A)[0].get("resolution").is_none());
}

#[test]
fn pending_proposals_are_listed_accepted_and_discarded() {
    let scratch = Scratch::new("pending");
    let db = linked(&scratch);
    {
        let store = std::sync::Arc::new(Store::open(&db).unwrap());
        let engine = omnimem_engine::Engine::new(
            store,
            std::sync::Arc::new(NoEmbedder),
            omnimem_engine::EngineConfig::default(),
        );
        let request = omnimem_engine::ResolveRequest::new(
            KEY_A,
            KEY_B,
            "both_valid",
            None,
            Some("other repos"),
            omnimem_engine::Retire::No,
        )
        .unwrap();
        engine.propose_resolution(&request, "mcp").unwrap();
    }
    let listed = stdout(&scratch.run(&db, &["contradictions"]));
    assert!(
        listed.contains("pending") && listed.contains("both_valid"),
        "{listed}"
    );

    let discarded = scratch.run(&db, &["contradictions", "discard", KEY_B, KEY_A]);
    assert!(discarded.status.success());
    assert!(!stdout(&scratch.run(&db, &["contradictions"])).contains("pending"));

    // Propose again, then accept from the command line.
    {
        let store = std::sync::Arc::new(Store::open(&db).unwrap());
        let engine = omnimem_engine::Engine::new(
            store,
            std::sync::Arc::new(NoEmbedder),
            omnimem_engine::EngineConfig::default(),
        );
        let request = omnimem_engine::ResolveRequest::new(
            KEY_A,
            KEY_B,
            "both_valid",
            None,
            Some("other repos"),
            omnimem_engine::Retire::No,
        )
        .unwrap();
        engine.propose_resolution(&request, "mcp").unwrap();
    }
    let accepted = scratch.run(&db, &["contradictions", "accept", KEY_A, KEY_B]);
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    assert_eq!(links(&db, KEY_A)[0]["resolution"]["reason"], "other repos");
    assert_eq!(links(&db, KEY_A)[0]["resolution"]["resolved_by"], "cli");
}

struct NoEmbedder;

impl omnimem_core::TextEmbedder for NoEmbedder {
    fn dimension(&self) -> usize {
        omnimem_core::VECTOR_DIM
    }
    fn embed_texts(&self, _: &[&str]) -> Result<Vec<Vec<f32>>, omnimem_core::EmbeddingError> {
        Err("not needed here".into())
    }
}

/// #37: `resolve` and `accept` refuse without a terminal and write nothing.
#[test]
fn resolve_and_accept_need_a_terminal() {
    let scratch = Scratch::new("tty");
    let db = linked(&scratch);
    let piped = scratch.piped(
        &db,
        &[
            "contradictions",
            "resolve",
            KEY_A,
            KEY_B,
            "--outcome",
            "both_valid",
        ],
        None,
    );
    assert!(!piped.status.success());
    assert!(
        String::from_utf8_lossy(&piped.stderr).contains("only from a terminal"),
        "{}",
        String::from_utf8_lossy(&piped.stderr)
    );
    let accept = scratch.piped(&db, &["contradictions", "accept", KEY_A, KEY_B], None);
    assert!(!accept.status.success());
    // Still open: nothing was written.
    assert!(stdout(&scratch.run(&db, &["contradictions"])).contains("open"));
    // Discarding a proposal is not a decision about the memories.
    let discard = scratch.piped(&db, &["contradictions", "discard", KEY_A, KEY_B], None);
    assert!(!String::from_utf8_lossy(&discard.stderr).contains("only from a terminal"));
}
