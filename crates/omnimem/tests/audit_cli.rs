//! `omnimem audit`, `export --audit` and `import` on a real database file.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use omnimem_store::{Actor, AuditLevel, Fields, Store, enter};

/// A scratch folder with an empty settings file, so no setting on the
/// machine running the tests reaches the binary.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("omnimem-cli-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("empty.env"), "").unwrap();
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn run(&self, db: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_omnimem"))
            .env("OMNIMEM_ENV_FILE", self.path("empty.env"))
            .env_remove("AUDIT_LOG")
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

fn fields(pairs: &[(&str, &str)]) -> Fields {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

/// A database with a memory that was written, archived by an agent and
/// retagged from the window.
fn populated(scratch: &Scratch, name: &str) -> PathBuf {
    let db = scratch.path(name);
    let store = Store::open(&db).unwrap();
    store.set_audit_level(AuditLevel::Writes);
    {
        let _who = enter(Actor::Mcp, "claude-code");
        store
            .set_fields(
                "mem:episodic:one",
                &fields(&[("content", "the secret plan"), ("project", "alpha")]),
            )
            .unwrap();
        store
            .set_fields(
                "mem:episodic:one",
                &fields(&[("state", "archived"), ("archived_reason", "done")]),
            )
            .unwrap();
    }
    {
        let _who = enter(Actor::Panel, "");
        store
            .set_fields("mem:episodic:two", &fields(&[("content", "another")]))
            .unwrap();
    }
    db
}

fn stdout(output: &Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn audit_prints_the_log_newest_first_and_filters_it() {
    let scratch = Scratch::new("print");
    let db = populated(&scratch, "a.db");

    let all = stdout(&scratch.run(&db, &["audit"]));
    let lines: Vec<_> = all.lines().collect();
    assert_eq!(lines.len(), 3, "{all}");
    assert!(
        lines[0].contains("panel")
            && lines[0].contains("remember")
            && lines[0].contains("mem:episodic:two")
    );
    assert!(lines[1].contains("mcp (claude-code)") && lines[1].contains("transition"));
    assert!(lines[1].contains("state: active -> archived"), "{all}");
    assert!(!all.contains("the secret plan"), "writes holds no text");

    let key = stdout(&scratch.run(&db, &["audit", "--key", "mem:episodic:one"]));
    assert_eq!(key.lines().count(), 2);
    let actor = stdout(&scratch.run(&db, &["audit", "--actor", "panel"]));
    assert_eq!(actor.lines().count(), 1);
    let action = stdout(&scratch.run(&db, &["audit", "--action", "transition"]));
    assert_eq!(action.lines().count(), 1);
    let since = stdout(&scratch.run(&db, &["audit", "--since", "1h"]));
    assert_eq!(since.lines().count(), 3);
    let future = scratch.run(&db, &["audit", "--since", "2999-01-01"]);
    assert_eq!(stdout(&future).lines().count(), 0);
    assert!(String::from_utf8_lossy(&future.stderr).contains("no audit rows match"));
    let limited = stdout(&scratch.run(&db, &["audit", "--limit", "1"]));
    assert_eq!(limited.lines().count(), 1);
}

#[test]
fn audit_json_is_one_object_per_line() {
    let scratch = Scratch::new("json");
    let db = populated(&scratch, "a.db");
    let out = stdout(&scratch.run(&db, &["audit", "--json", "--key", "mem:episodic:one"]));
    let rows: Vec<serde_json::Value> = out
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["action"], "transition");
    assert_eq!(rows[0]["actor"], "mcp");
    assert_eq!(rows[0]["client"], "claude-code");
    assert_eq!(rows[0]["from"]["state"], "active");
    assert_eq!(rows[0]["to"]["state"], "archived");
    assert!(rows[0]["at"].is_number());
}

#[test]
fn audit_refuses_what_it_cannot_read() {
    let scratch = Scratch::new("bad");
    let db = populated(&scratch, "a.db");
    let since = scratch.run(&db, &["audit", "--since", "yesterday-ish"]);
    assert!(!since.status.success());
    assert!(String::from_utf8_lossy(&since.stderr).contains("--since"));
    let actor = scratch.run(&db, &["audit", "--actor", "somebody"]);
    assert!(!actor.status.success());
}

#[test]
fn an_empty_log_says_how_to_start_one() {
    let scratch = Scratch::new("empty");
    let db = scratch.path("e.db");
    drop(Store::open(&db).unwrap());
    let out = scratch.run(&db, &["audit"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("AUDIT_LOG is off"));
}

#[test]
fn export_carries_the_log_only_with_audit_and_import_adds_it_once() {
    let scratch = Scratch::new("export");
    let db = populated(&scratch, "a.db");

    let plain = scratch.path("plain.json");
    stdout(&scratch.run(&db, &["export", plain.to_str().unwrap()]));
    let plain: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(plain).unwrap()).unwrap();
    assert!(plain.get("audit").is_none());

    let with = scratch.path("with.json");
    stdout(&scratch.run(&db, &["export", with.to_str().unwrap(), "--audit"]));
    let with_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&with).unwrap()).unwrap();
    assert_eq!(with_json["audit"].as_array().unwrap().len(), 3);

    // Into a fresh database: the rows arrive, and a second import adds none.
    let fresh = scratch.path("fresh.db");
    let args = ["import", with.to_str().unwrap(), "--no-embed", "--create"];
    let first = scratch.run(&fresh, &args);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        String::from_utf8_lossy(&first.stderr).contains("audit log: 3 rows added, 0 already there")
    );
    let second = scratch.run(&fresh, &args);
    assert!(
        String::from_utf8_lossy(&second.stderr)
            .contains("audit log: 0 rows added, 3 already there")
    );
    let shown = stdout(&scratch.run(&fresh, &["audit", "--actor", "mcp"]));
    assert_eq!(shown.lines().count(), 2, "{shown}");
}

#[test]
fn an_import_is_recorded_as_the_importer_when_the_log_is_on() {
    let scratch = Scratch::new("import");
    let db = populated(&scratch, "a.db");
    let backup = scratch.path("b.json");
    stdout(&scratch.run(&db, &["export", backup.to_str().unwrap()]));

    let fresh = scratch.path("fresh.db");
    let output = Command::new(env!("CARGO_BIN_EXE_omnimem"))
        .env("OMNIMEM_ENV_FILE", scratch.path("empty.env"))
        .env("AUDIT_LOG", "writes")
        .arg("--db")
        .arg(&fresh)
        .args(["import", backup.to_str().unwrap(), "--no-embed", "--create"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let shown = stdout(&scratch.run(&fresh, &["audit", "--json"]));
    let rows: Vec<serde_json::Value> = shown
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(rows.len(), 2, "{shown}");
    assert!(
        rows.iter()
            .all(|r| r["actor"] == "import" && r["action"] == "restore")
    );
}
