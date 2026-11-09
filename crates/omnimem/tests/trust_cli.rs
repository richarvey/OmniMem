//! `omnimem trust` on a real database file.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use omnimem_store::{Actor, Fields, Store, enter};

mod common;

const SOURCE: &str = "mem:knowledge:01SRC";
const FACT_A: &str = "mem:knowledge:01FA";
const FACT_B: &str = "mem:knowledge:01FB";

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("omnimem-cli-t-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("empty.env"), "").unwrap();
        Self(dir)
    }

    fn run(&self, db: &Path, args: &[&str]) -> Output {
        // A person-only command runs at a terminal, answered as a person.
        if let Some(key) = common::person_only_key(args) {
            let mut all: Vec<std::ffi::OsString> = vec!["--db".into(), db.into()];
            all.extend(args.iter().map(Into::into));
            return common::as_output(common::in_terminal(
                Path::new(env!("CARGO_BIN_EXE_omnimem")),
                &all,
                &[
                    ("AUDIT_LOG", "lifecycle".into()),
                    ("OMNIMEM_ENV_FILE", self.0.join("empty.env").into()),
                ],
                &["OMNIMEM_DB"],
                &common::tail(key),
            ));
        }
        self.piped(db, args)
    }

    /// With stdin and stdout piped, as an agent's shell runs it.
    fn piped(&self, db: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_omnimem"))
            .env("AUDIT_LOG", "lifecycle")
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

fn row(content: &str, from: Option<&str>) -> Fields {
    let mut fields: Fields = [
        ("content", content),
        ("state", "active"),
        ("created_at", "1.0"),
        ("updated_at", "1.0"),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
    .collect();
    if let Some(from) = from {
        fields.insert("enriched_from".to_owned(), from.to_owned());
    }
    fields
}

/// A source and two facts extracted from it, so both are candidates.
fn database(scratch: &Scratch) -> PathBuf {
    let db = scratch.0.join("omnimem.db");
    let store = Store::open(&db).unwrap();
    let _who = enter(Actor::Cli, "");
    store
        .upsert(SOURCE, &row("an article", None), None)
        .unwrap();
    store
        .upsert(FACT_A, &row("the sky is blue", Some(SOURCE)), None)
        .unwrap();
    store
        .upsert(FACT_B, &row("grass is green", Some(SOURCE)), None)
        .unwrap();
    db
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn trust(db: &Path, key: &str) -> Option<String> {
    Store::open(db)
        .unwrap()
        .get(key)
        .unwrap()
        .unwrap()
        .get("trust")
        .cloned()
}

#[test]
fn list_shows_candidates_and_filters_by_source() {
    let scratch = Scratch::new("list");
    let db = database(&scratch);
    let listed = scratch.run(&db, &["trust", "list"]);
    assert!(listed.status.success(), "{}", err(&listed));
    let text = out(&listed);
    assert!(
        text.contains(FACT_A) && text.contains("grass is green"),
        "{text}"
    );
    assert!(!text.contains("an article"), "the source isn't a candidate");

    let json = scratch.run(
        &db,
        &[
            "trust", "list", "--source", SOURCE, "--limit", "1", "--json",
        ],
    );
    let text = out(&json);
    assert_eq!(text.lines().count(), 1, "{text}");
    let row: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert_eq!(row["enriched_from"], SOURCE);

    let none = scratch.run(&db, &["trust", "list", "--source", "mem:knowledge:nothing"]);
    assert!(out(&none).is_empty());
}

#[test]
fn confirm_reject_and_restore() {
    let scratch = Scratch::new("act");
    let db = database(&scratch);

    let done = scratch.run(&db, &["trust", "confirm", FACT_A, "--reason", "checked"]);
    assert!(done.status.success(), "{}", err(&done));
    assert_eq!(trust(&db, FACT_A).as_deref(), Some("accepted"));

    // A second confirm is refused with the engine's message.
    let again = scratch.run(&db, &["trust", "confirm", FACT_A]);
    assert!(!again.status.success());
    assert!(
        err(&again).contains("only a candidate can be confirmed"),
        "{}",
        err(&again)
    );

    let rejected = scratch.run(&db, &["trust", "reject", FACT_B, "--reason", "not true"]);
    assert!(rejected.status.success(), "{}", err(&rejected));
    assert_eq!(trust(&db, FACT_B).as_deref(), Some("rejected"));

    let restored = scratch.run(&db, &["trust", "restore", FACT_B]);
    assert!(restored.status.success(), "{}", err(&restored));
    assert_ne!(trust(&db, FACT_B).as_deref(), Some("rejected"));

    let log = scratch.run(&db, &["audit", "--action", "trust", "--json"]);
    let text = out(&log);
    assert!(
        text.contains("\"cli\"") && text.contains("not true"),
        "{text}"
    );
}

#[test]
fn confirm_source_moves_every_candidate_from_it() {
    let scratch = Scratch::new("source");
    let db = database(&scratch);
    let done = scratch.run(&db, &["trust", "confirm-source", SOURCE]);
    assert!(done.status.success(), "{}", err(&done));
    assert!(
        out(&done).contains("confirmed 2 candidates"),
        "{}",
        out(&done)
    );
    assert_eq!(trust(&db, FACT_A).as_deref(), Some("accepted"));
    assert_eq!(trust(&db, FACT_B).as_deref(), Some("accepted"));
}

/// #37: without a terminal, or with the wrong characters typed, a
/// person-only command changes nothing.
#[test]
fn a_person_only_command_needs_a_terminal_and_the_key() {
    let scratch = Scratch::new("tty");
    let db = database(&scratch);
    for args in [
        vec!["trust", "confirm", FACT_A],
        vec!["trust", "reject", FACT_A],
        vec!["trust", "confirm-source", SOURCE],
    ] {
        let piped = scratch.piped(&db, &args);
        assert!(!piped.status.success(), "{args:?} ran without a terminal");
        assert!(
            err(&piped).contains("only from a terminal"),
            "{}",
            err(&piped)
        );
    }
    assert_ne!(trust(&db, FACT_A).as_deref(), Some("accepted"));
    assert_ne!(trust(&db, FACT_A).as_deref(), Some("rejected"));

    let mut all: Vec<std::ffi::OsString> = vec!["--db".into(), db.clone().into()];
    all.extend(["trust", "confirm", FACT_A].iter().map(Into::into));
    let wrong = common::in_terminal(
        Path::new(env!("CARGO_BIN_EXE_omnimem")),
        &all,
        &[("OMNIMEM_ENV_FILE", scratch.0.join("empty.env").into())],
        &["OMNIMEM_DB"],
        "XXXXXX",
    );
    assert!(!wrong.success, "{}", wrong.output);
    assert!(wrong.output.contains("didn't match"), "{}", wrong.output);
    assert_ne!(trust(&db, FACT_A).as_deref(), Some("accepted"));

    // Listing is anyone's: no terminal needed.
    assert!(scratch.piped(&db, &["trust", "list"]).status.success());
}
