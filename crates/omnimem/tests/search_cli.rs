//! `omnimem search --valid-at --known-at` (#36) on a real database file.
//! Needs the embedding model, so it skips where the cache doesn't have it.

use std::path::PathBuf;
use std::process::Command;

use omnimem_core::TextEmbedder;
use omnimem_embed::{EmbedConfig, Embedder};
use omnimem_store::{Fields, Store};

fn fields(pairs: &[(&str, &str)]) -> Fields {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[test]
fn search_filters_on_valid_at_and_known_at() {
    let embedder = match Embedder::load(&EmbedConfig::default()) {
        Ok(e) => e,
        Err(err) if err.is_model_unavailable() => {
            eprintln!("skipping: {err}");
            return;
        }
        Err(err) => panic!("model present but failed to load: {err}"),
    };
    let dir: PathBuf = std::env::temp_dir().join(format!("omnimem-cli-s-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("empty.env"), "").unwrap();
    let db = dir.join("omnimem.db");
    {
        let store = Store::open(&db).unwrap();
        // Berlin: recorded 10 Jan 2026, true until 1 April, retired 10 May.
        // Lisbon: recorded 10 May, true from 1 April.
        let rows = [
            (
                "mem:episodic:berlin",
                "Sam lives in Berlin",
                vec![
                    ("created_at", "1768003200.0"),
                    ("valid_to", "1775001600.0"),
                    ("valid_to_recorded_at", "1778371200.0"),
                    ("retired_at", "1778371200.0"),
                    ("trust", "superseded"),
                ],
            ),
            (
                "mem:episodic:lisbon",
                "Sam moved to Lisbon",
                vec![
                    ("created_at", "1778371200.0"),
                    ("valid_from", "1775001600.0"),
                ],
            ),
        ];
        for (key, content, extra) in rows {
            let mut f = fields(&[("content", content), ("state", "active")]);
            f.extend(fields(&extra));
            let vector = embedder.embed_texts(&[content]).unwrap().pop().unwrap();
            store.upsert(key, &f, Some(&vector)).unwrap();
        }
    }
    let run = |args: &[&str]| -> Vec<String> {
        let out = Command::new(env!("CARGO_BIN_EXE_omnimem"))
            .env("OMNIMEM_ENV_FILE", dir.join("empty.env"))
            .env_remove("OMNIMEM_DB")
            .arg("--db")
            .arg(&db)
            .args(["search", "where does Sam live", "--json"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let mut keys: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| {
                serde_json::from_str::<serde_json::Value>(l).unwrap()["key"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        keys.sort();
        keys
    };
    assert_eq!(run(&[]), ["mem:episodic:berlin", "mem:episodic:lisbon"]);
    assert_eq!(run(&["--valid-at", "2026-03-01"]), ["mem:episodic:berlin"]);
    assert_eq!(run(&["--valid-at", "2026-05-01"]), ["mem:episodic:lisbon"]);
    assert_eq!(run(&["--known-at", "2026-04-15"]), ["mem:episodic:berlin"]);
    assert_eq!(
        run(&["--valid-at", "2026-04-15", "--known-at", "2026-04-15"]),
        ["mem:episodic:berlin"]
    );
    // A date that can't be read is refused.
    let bad = Command::new(env!("CARGO_BIN_EXE_omnimem"))
        .env("OMNIMEM_ENV_FILE", dir.join("empty.env"))
        .arg("--db")
        .arg(&db)
        .args(["search", "Sam", "--valid-at", "whenever"])
        .output()
        .unwrap();
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("valid_at"));
    let _ = std::fs::remove_dir_all(&dir);
}
