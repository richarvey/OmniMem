//! The audit log through the engine: which changes each level records, who
//! made them, and that the log holds memory text at `full` and nowhere else.

use std::path::PathBuf;
use std::sync::Arc;

use omnimem_core::{EmbeddingError, TextEmbedder, VECTOR_DIM};
use omnimem_engine::{AuditLevel, Engine, EngineConfig};
use omnimem_store::{Actor, AuditQuery, AuditRow, Fields, Store, enter};
use serde_json::json;

/// The same bag-of-words embedder the other engine tests use.
struct Words;

fn vector(text: &str) -> Vec<f32> {
    let mut v = vec![0.0f32; VECTOR_DIM];
    for word in text.split_whitespace() {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in word.to_lowercase().bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        v[(h % VECTOR_DIM as u64) as usize] += 1.0;
    }
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        for x in &mut v {
            *x /= n;
        }
    } else {
        v[0] = 1.0;
    }
    v
}

impl TextEmbedder for Words {
    fn dimension(&self) -> usize {
        VECTOR_DIM
    }
    fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        Ok(texts.iter().map(|t| vector(t)).collect())
    }
}

fn engine_with(config: EngineConfig) -> Engine {
    Engine::new(
        Arc::new(Store::open_in_memory().unwrap()),
        Arc::new(Words),
        config,
    )
}

fn engine_at(level: AuditLevel) -> Engine {
    engine_with(EngineConfig {
        audit_log: level,
        ..EngineConfig::default()
    })
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("omnimem-audit-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Every row, newest first.
fn rows(e: &Engine) -> Vec<AuditRow> {
    e.store()
        .audit_entries(&AuditQuery {
            limit: 1000,
            ..AuditQuery::default()
        })
        .unwrap()
}

/// The distinct actions in the log, sorted.
fn actions(e: &Engine) -> Vec<String> {
    let mut seen: Vec<String> = rows(e).into_iter().map(|r| r.action).collect();
    seen.sort();
    seen.dedup();
    seen
}

fn remember(e: &Engine, content: &str, project: Option<&str>) -> String {
    let result = e
        .remember(
            content,
            project,
            Some(&["tag-one".to_owned()]),
            "episodic",
            true,
            Some("raw"),
            None,
            None,
        )
        .unwrap();
    result["key"].as_str().unwrap().to_owned()
}

/// Every change of every kind the engine can make, so a level's set can be
/// read off the log. Returns the memory the changes were made to.
fn exercise(e: &Engine) -> String {
    let _who = enter(Actor::Mcp, "test-client");
    let key = remember(e, "the staging server runs on port 9000", Some("alpha"));
    e.retag(&key, None, Some(vec!["extra".into()]), None)
        .unwrap();
    e.set_licence("open", Some(std::slice::from_ref(&key)), None, None)
        .unwrap();
    e.set_provenance("asserted", std::slice::from_ref(&key))
        .unwrap();
    e.record_experience(&key, 3, "succeeded", 2, None, Some("a fix"), None, None)
        .unwrap();
    e.deprioritise(&key, "not needed for now", None).unwrap();
    e.reinstate(&key).unwrap();
    e.archive(&key, Some("superseded")).unwrap();
    e.suppress_topic("staging", Some("noisy")).unwrap();
    e.unsuppress_topic("staging").unwrap();
    e.forget(&key, true).unwrap();
    key
}

/// #18: with `AUDIT_LOG` unset the log keeps lifecycle rows. An archive is
/// recorded, with its reason and no memory text; a plain remember is not.
#[test]
fn the_default_keeps_a_lifecycle_log() {
    let e = engine_with(EngineConfig::default());
    assert_eq!(e.audit_level(), AuditLevel::Lifecycle);
    let _who = enter(Actor::Mcp, "test-client");
    let key = remember(&e, "the staging server runs on port 9000", None);
    assert_eq!(
        e.store().audit_count().unwrap(),
        0,
        "a remember is a write, not a lifecycle change"
    );
    e.archive(&key, Some("superseded")).unwrap();
    let logged = rows(&e);
    assert_eq!(logged.len(), 1, "{logged:?}");
    assert_eq!(logged[0].action, "transition");
    assert_eq!(logged[0].key, key);
    assert_eq!(logged[0].reason, "superseded");
    let written = serde_json::to_string(&logged[0].to_json()).unwrap();
    assert!(!written.contains("port 9000"), "no memory text: {written}");
}

#[test]
fn off_writes_nothing() {
    let e = engine_at(AuditLevel::Off);
    let key = exercise(&e);
    assert_eq!(e.store().audit_count().unwrap(), 0);
    // The 6.x result: no history key at all.
    let again = remember(&e, "another memory entirely", None);
    let explained = e.explain_memory(&again).unwrap();
    assert!(explained.get("history").is_none(), "{explained}");
    assert!(!key.is_empty());
}

#[test]
fn lifecycle_records_state_changes_deletes_and_suppression_only() {
    let e = engine_at(AuditLevel::Lifecycle);
    exercise(&e);
    assert_eq!(
        actions(&e),
        ["forget", "suppress", "transition", "unsuppress"],
        "{:?}",
        rows(&e).iter().map(|r| &r.action).collect::<Vec<_>>()
    );
    // deprioritise, reinstate and archive: three transitions.
    let transitions = rows(&e)
        .into_iter()
        .filter(|r| r.action == "transition")
        .count();
    assert_eq!(transitions, 3);
}

#[test]
fn writes_adds_every_other_write() {
    let e = engine_at(AuditLevel::Writes);
    exercise(&e);
    assert_eq!(
        actions(&e),
        [
            "forget",
            "record_experience",
            "remember",
            "retag",
            "set_licence",
            "set_provenance",
            "suppress",
            "transition",
            "unsuppress"
        ]
    );
}

#[test]
fn a_new_memory_is_recorded_without_its_text_below_full() {
    for level in [AuditLevel::Lifecycle, AuditLevel::Writes] {
        let e = engine_at(level);
        let key = remember(&e, "the launch code is hunter2", Some("alpha"));
        e.archive(&key, Some("done")).unwrap();
        e.forget(&key, true).unwrap();
        let everything =
            serde_json::to_string(&rows(&e).iter().map(AuditRow::to_json).collect::<Vec<_>>())
                .unwrap();
        assert!(
            !everything.contains("hunter2"),
            "{level:?} must not hold memory text: {everything}"
        );
    }
    let e = engine_at(AuditLevel::Writes);
    let key = remember(&e, "the launch code is hunter2", Some("alpha"));
    let created = &rows(&e)[0];
    assert_eq!(created.action, "remember");
    assert_eq!(created.key, key);
    assert_eq!(created.to["namespace"], "episodic");
    assert_eq!(created.to["project"], "alpha");
    assert_eq!(created.to["tags"], json!(["tag-one"]));
    assert!(created.to.get("content").is_none());
}

#[test]
fn full_holds_the_text_before_and_after() {
    let e = engine_at(AuditLevel::Full);
    let key = remember(&e, "the launch code is hunter2", None);
    let _who = enter(Actor::Panel, "");
    e.store()
        .set_field(&key, "content", "the launch code is correcthorse")
        .unwrap();

    let log = rows(&e);
    let created = log.iter().find(|r| r.action == "remember").unwrap();
    assert_eq!(created.to["content"], "the launch code is hunter2");
    let edited = log.iter().find(|r| r.action == "update").unwrap();
    assert_eq!(edited.from["content"], "the launch code is hunter2");
    assert_eq!(edited.to["content"], "the launch code is correcthorse");

    // Until it's forgotten (#19): then the history stays and the text goes,
    // including from the forget's own row.
    e.forget(&key, true).unwrap();
    let log = rows(&e);
    assert_eq!(log.iter().filter(|r| r.key == key).count(), 3);
    for row in log.iter().filter(|r| r.key == key) {
        assert!(
            row.from.get("content").is_none() && row.to.get("content").is_none(),
            "{row:?}"
        );
    }
    let forgotten = log.iter().find(|r| r.action == "forget").unwrap();
    assert_eq!(forgotten.to["scrubbed"], 2);
}

#[test]
fn a_change_to_a_field_the_level_does_not_show_names_it_without_its_value() {
    let e = engine_at(AuditLevel::Writes);
    let key = remember(&e, "the launch code is hunter2", None);
    e.store()
        .set_field(&key, "content", "the launch code is correcthorse")
        .unwrap();
    let edited = &rows(&e)[0];
    assert_eq!(edited.action, "update");
    assert_eq!(edited.to["other_fields"], json!(["content"]));
    assert!(edited.to.get("content_hash").is_some());
}

#[test]
fn rows_say_who_made_the_change() {
    let e = engine_at(AuditLevel::Writes);
    let key = {
        let _who = enter(Actor::Mcp, "claude-code (abc-123)");
        remember(&e, "an agent wrote this", None)
    };
    {
        let _who = enter(Actor::Panel, "");
        e.retag(&key, None, Some(vec!["by-hand".into()]), None)
            .unwrap();
    }
    {
        let _who = enter(Actor::Hook, "");
        remember(&e, "a hook wrote this", None);
    }
    let log = rows(&e);
    let by = |action: &str, actor: &str| {
        log.iter()
            .find(|r| r.action == action && r.actor == actor)
            .unwrap_or_else(|| panic!("no {action} by {actor}"))
    };
    assert_eq!(by("remember", "mcp").client, "claude-code (abc-123)");
    assert_eq!(by("retag", "panel").client, "");
    assert_eq!(by("remember", "hook").client, "");
    // A write nobody attributed says so, rather than borrowing a name.
    let unscoped = std::thread::scope(|s| {
        s.spawn(|| remember(&e, "nobody claimed this", None))
            .join()
            .unwrap()
    });
    let row = rows(&e).into_iter().find(|r| r.key == unscoped).unwrap();
    assert_eq!(row.actor, "unknown");
}

#[test]
fn the_cascade_is_one_entry_per_source_listing_the_facts() {
    let e = engine_at(AuditLevel::Lifecycle);
    let _who = enter(Actor::Mcp, "test");
    let source = remember(&e, "the build server is at 10.0.0.5", None);
    let mut facts = Vec::new();
    for (i, text) in ["build server address", "build server owner"]
        .iter()
        .enumerate()
    {
        let fact = format!("mem:knowledge:fact{i}");
        let f = Fields::from([
            ("content".to_owned(), (*text).to_owned()),
            ("state".to_owned(), "active".to_owned()),
            ("surface_score".to_owned(), "1.0".to_owned()),
            ("enriched_from".to_owned(), source.clone()),
        ]);
        e.store().upsert(&fact, &f, Some(&vector(text))).unwrap();
        facts.push(fact);
    }
    e.archive(&source, Some("replaced")).unwrap();

    let log = rows(&e);
    let cascade: Vec<_> = log.iter().filter(|r| r.action == "cascade").collect();
    assert_eq!(cascade.len(), 1, "{log:#?}");
    assert_eq!(cascade[0].key, source);
    for fact in &facts {
        assert_eq!(cascade[0].to["facts"][fact], "archived");
        assert_eq!(cascade[0].from["facts"][fact], "active");
        assert!(
            log.iter().all(|r| &r.key != fact),
            "a fact moved by the cascade has no entry of its own"
        );
    }
    let own = log
        .iter()
        .find(|r| r.action == "transition" && r.key == source)
        .unwrap();
    assert_eq!(own.reason, "replaced");
    assert_eq!(own.from["state"], "active");
    assert_eq!(own.to["state"], "archived");

    // And back again.
    e.reinstate(&source).unwrap();
    let back = rows(&e)
        .into_iter()
        .find(|r| r.action == "cascade" && r.to["facts"][&facts[0]] == "active")
        .expect("reinstating records the facts coming back");
    assert_eq!(back.key, source);

    // Deleting the source deletes its facts, again as one entry.
    e.forget(&source, true).unwrap();
    let gone = rows(&e)
        .into_iter()
        .find(|r| r.action == "cascade" && r.to["facts"][&facts[1]] == "deleted")
        .expect("forgetting records the facts going with it");
    assert_eq!(gone.key, source);
}

/// #19 through the engine: at `full`, forgetting a memory scrubs its text
/// from the log, and its extracted facts' text with it, while every row
/// still says what happened. `delete_project` goes the same way.
#[test]
fn forget_and_delete_project_scrub_text_from_the_log() {
    let e = engine_at(AuditLevel::Full);
    let _who = enter(Actor::Mcp, "test");
    let source = remember(&e, "the build server is at 10.0.0.5", Some("alpha"));
    let fact = "mem:knowledge:factscrub".to_owned();
    let f = Fields::from([
        (
            "content".to_owned(),
            "build server address 10.0.0.5".to_owned(),
        ),
        ("state".to_owned(), "active".to_owned()),
        ("surface_score".to_owned(), "1.0".to_owned()),
        ("enriched_from".to_owned(), source.clone()),
    ]);
    e.store()
        .upsert(&fact, &f, Some(&vector("build server address")))
        .unwrap();
    e.retag(&source, None, Some(vec!["infra".into()]), None)
        .unwrap();
    let text_in = |e: &Engine, key: &str, text: &str| {
        e.store()
            .audit_entries(&AuditQuery {
                key: Some(key.to_owned()),
                limit: 100,
                ..AuditQuery::default()
            })
            .unwrap()
            .iter()
            .any(|r| format!("{}{}", r.from, r.to).contains(text))
    };
    assert!(
        text_in(&e, &source, "10.0.0.5") && text_in(&e, &fact, "10.0.0.5"),
        "full keeps text"
    );

    e.forget(&source, true).unwrap();
    for key in [&source, &fact] {
        assert!(
            !text_in(&e, key, "10.0.0.5"),
            "{key} still has text in the log"
        );
    }
    let log = rows(&e);
    assert!(
        log.iter()
            .any(|r| r.key == source && r.action == "remember")
    );
    let forgot = log
        .iter()
        .find(|r| r.key == source && r.action == "forget")
        .expect("the forget is still recorded");
    // Its remember held text; the retag only ever held tags.
    assert_eq!(forgot.to["scrubbed"], 1, "{:?}", forgot.to);
    let cascade = log
        .iter()
        .find(|r| r.action == "cascade" && r.to["facts"][&fact] == "deleted")
        .unwrap();
    assert_eq!(cascade.to["scrubbed"], 1, "the fact's own row");

    // A project-wide delete scrubs too.
    let doomed = remember(&e, "beta knows the vault code 4471", Some("beta"));
    assert!(text_in(&e, &doomed, "4471"));
    e.delete_project("beta", true, true).unwrap();
    assert!(!text_in(&e, &doomed, "4471"));
    assert!(
        rows(&e)
            .iter()
            .any(|r| r.key == doomed && r.action == "remember")
    );
}

#[test]
fn a_project_wide_transition_is_recorded_for_each_memory() {
    let e = engine_at(AuditLevel::Lifecycle);
    let _who = enter(Actor::Mcp, "test");
    let first = remember(&e, "alpha one", Some("alpha"));
    let second = remember(&e, "alpha two", Some("alpha"));
    let other = remember(&e, "beta one", Some("beta"));
    e.deprioritise_project("alpha", true, Some("project paused"), false)
        .unwrap();
    let log = rows(&e);
    let moved: Vec<_> = log
        .iter()
        .filter(|r| r.action == "project_transition")
        .collect();
    assert_eq!(moved.len(), 2, "{log:#?}");
    for key in [&first, &second] {
        let row = moved.iter().find(|r| &r.key == key).unwrap();
        assert_eq!(row.reason, "project paused");
        assert_eq!(row.to["state"], "deprioritised");
        assert_eq!(row.to["project"], "alpha");
    }
    assert!(log.iter().all(|r| r.key != other));

    e.delete_project("alpha", true, false).unwrap();
    let deleted: Vec<_> = rows(&e)
        .into_iter()
        .filter(|r| r.action == "project_delete")
        .collect();
    assert_eq!(deleted.len(), 2);
    assert!(deleted.iter().all(|r| r.to["state"] == "deleted"));
}

#[test]
fn suppression_is_recorded_against_the_topic_with_its_reason() {
    let e = engine_at(AuditLevel::Lifecycle);
    let _who = enter(Actor::Mcp, "test");
    e.suppress_topic("Staging Server", Some("too noisy"))
        .unwrap();
    e.suppress_topic("Staging Server", None).unwrap();
    e.unsuppress_topic("Staging Server").unwrap();
    let log = rows(&e);
    // The second suppress changed nothing, so it has no row.
    assert_eq!(log.len(), 2, "{log:#?}");
    assert_eq!(log[1].action, "suppress");
    assert_eq!(log[1].key, "staging server");
    assert_eq!(log[1].reason, "too noisy");
    assert_eq!(log[0].action, "unsuppress");
}

#[test]
fn maintenance_archives_are_the_maintenances_not_the_callers() {
    let e = engine_with(EngineConfig {
        audit_log: AuditLevel::Lifecycle,
        auto_maintenance_interval: 1,
        ..EngineConfig::default()
    });
    let _who = enter(Actor::Mcp, "an-agent");
    let old = remember(&e, "the same thing written twice over", Some("alpha"));
    // A second copy, written later so the first is the one archived.
    std::thread::sleep(std::time::Duration::from_millis(20));
    remember(&e, "the same thing written twice over", Some("alpha"));
    e.briefing(Some("alpha"), false).unwrap();
    let archived = rows(&e)
        .into_iter()
        .find(|r| r.key == old && r.action == "transition")
        .expect("the duplicate was archived");
    assert_eq!(archived.actor, "maintenance");
    assert_eq!(archived.client, "");
    assert!(
        archived.reason.starts_with("auto-maintenance"),
        "{archived:?}"
    );
}

#[test]
fn explain_memory_lists_its_history_when_there_is_one() {
    let e = engine_at(AuditLevel::Writes);
    let _who = enter(Actor::Mcp, "test");
    let key = remember(&e, "something worth explaining", None);
    e.archive(&key, Some("enough")).unwrap();
    let explained = e.explain_memory(&key).unwrap();
    let history = explained["history"].as_array().unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0]["action"], "transition");
    assert_eq!(history[0]["actor"], "mcp");
    assert_eq!(history[0]["reason"], "enough");
    assert_eq!(history[1]["action"], "remember");
    assert!(history[0]["at"].as_str().unwrap().ends_with('Z'));
    // The log is on but this memory has no rows: no key, not an empty list.
    e.store().set_audit_level(AuditLevel::Off);
    let quiet = remember(&e, "written while the log was off", None);
    e.store().set_audit_level(AuditLevel::Writes);
    assert!(e.explain_memory(&quiet).unwrap().get("history").is_none());
}

#[test]
fn retention_prunes_old_rows_and_zero_keeps_them() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let old_row = |age_days: f64, key: &str| {
        json!({
            "at": now - age_days * 86_400.0,
            "actor": "mcp", "client": "", "action": "update", "key": key,
            "from": {}, "to": {}, "reason": "",
        })
    };
    for (days, left) in [(90, 1), (0, 2)] {
        let e = engine_with(EngineConfig {
            audit_retention_days: days,
            ..EngineConfig::default()
        });
        e.store()
            .merge_audit(&[
                old_row(120.0, "mem:episodic:old"),
                old_row(1.0, "mem:episodic:new"),
            ])
            .unwrap();
        e.prune_audit().unwrap();
        assert_eq!(
            e.store().audit_count().unwrap(),
            left,
            "retention of {days} days"
        );
    }
    // The log can be off and still age out.
    let e = engine_with(EngineConfig {
        audit_log: AuditLevel::Off,
        ..EngineConfig::default()
    });
    assert_eq!(e.audit_level(), AuditLevel::Off);
    e.store()
        .merge_audit(&[old_row(120.0, "mem:episodic:old")])
        .unwrap();
    e.prune_audit().unwrap();
    assert_eq!(e.store().audit_count().unwrap(), 0);
}

#[test]
fn an_export_carries_the_log_only_when_asked_and_a_restore_adds_what_is_missing() {
    let dir = scratch("backup");
    let config = || EngineConfig {
        audit_log: AuditLevel::Writes,
        backup_dir: dir.clone(),
        ..EngineConfig::default()
    };
    let e = engine_with(config());
    let _who = enter(Actor::Mcp, "test");
    let key = remember(&e, "worth backing up", Some("alpha"));
    e.archive(&key, Some("done")).unwrap();
    let count = e.store().audit_count().unwrap();
    assert_eq!(count, 2);

    let plain = e.dump_to_file(Some("plain.json")).unwrap();
    assert!(plain.get("audit").is_none());
    let without = omnimem_store::read_backup(&dir.join("plain.json")).unwrap();
    assert!(without.audit.is_none());

    let with = e.dump_to_file_with(Some("with.json"), false, true).unwrap();
    assert_eq!(with["audit"], count);
    let backup = omnimem_store::read_backup(&dir.join("with.json")).unwrap();
    assert_eq!(backup.audit.as_ref().unwrap().len(), count);

    // Onto the store it came from: nothing is added twice.
    let preview = e.restore_from_file("with.json", true).unwrap();
    assert_eq!(preview["audit_in_backup"], count);
    let same = e.restore_from_file("with.json", false).unwrap();
    assert_eq!(same["audit"]["added"], 0);
    assert_eq!(same["audit"]["already_present"], count);
    // (The restore itself is a write, which the log records.)
    assert!(e.store().audit_count().unwrap() >= count);

    // Onto a fresh store: the rows arrive, and the memory with them.
    let fresh = engine_with(config());
    let restored = fresh.restore_from_file("with.json", false).unwrap();
    assert_eq!(restored["audit"]["added"], count);
    let log = rows(&fresh);
    assert!(log.iter().any(|r| r.action == "transition" && r.key == key));
    let again = fresh.restore_from_file("with.json", false).unwrap();
    assert_eq!(again["audit"]["added"], 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_restore_is_recorded_as_a_restore() {
    let dir = scratch("restore");
    let source = engine_with(EngineConfig {
        backup_dir: dir.clone(),
        ..EngineConfig::default()
    });
    remember(&source, "restore me", None);
    source.dump_to_file(Some("b.json")).unwrap();

    let target = engine_with(EngineConfig {
        audit_log: AuditLevel::Writes,
        backup_dir: dir.clone(),
        ..EngineConfig::default()
    });
    let _who = enter(Actor::Panel, "");
    target.restore_from_file("b.json", false).unwrap();
    let log = rows(&target);
    let restored: Vec<_> = log.iter().filter(|r| r.action == "restore").collect();
    assert_eq!(restored.len(), 1, "{log:#?}");
    assert_eq!(restored[0].actor, "panel");
    assert!(restored[0].to.get("content").is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn contradiction_links_are_recorded_against_both_memories() {
    let e = engine_at(AuditLevel::Writes);
    let _who = enter(Actor::Mcp, "test");
    let a = remember(&e, "always use the cache for staging", Some("alpha"));
    let b = remember(&e, "never use the cache for staging", Some("alpha"));
    let found = e
        .check_contradictions(None, "episodic", Some("alpha"), false)
        .unwrap();
    assert!(
        found["contradictions"]
            .as_array()
            .is_some_and(|c| !c.is_empty()),
        "{found}"
    );
    let links: Vec<_> = rows(&e)
        .into_iter()
        .filter(|r| r.action == "contradiction_link")
        .collect();
    assert_eq!(links.len(), 2);
    let on_a = links.iter().find(|r| r.key == a).unwrap();
    assert_eq!(on_a.to["linked_to"], b);
    assert!(on_a.to.get("explanation").is_none(), "only full keeps it");
}

#[test]
fn an_approval_note_around_a_write_becomes_its_reason() {
    // What a caller that asked the user first does: say why around the
    // engine's own write, and the row carries it.
    let e = engine_at(AuditLevel::Lifecycle);
    let _who = enter(Actor::Mcp, "test");
    let key = remember(&e, "to be archived with a note", None);
    {
        let _note = omnimem_store::op(
            omnimem_store::Op::new("approved").reason(Some("approved by the user")),
        );
        e.archive(&key, None).unwrap();
    }
    let row = &rows(&e)[0];
    assert_eq!(row.action, "transition");
    assert_eq!(row.reason, "approved by the user");
}

#[test]
fn the_log_survives_filtering() {
    let e = engine_at(AuditLevel::Writes);
    let _who = enter(Actor::Mcp, "test");
    let key = remember(&e, "findable", None);
    remember(&e, "other", None);
    e.archive(&key, None).unwrap();
    let ask = |q: AuditQuery| e.store().audit_entries(&q).unwrap();
    assert_eq!(
        ask(AuditQuery {
            key: Some(key.clone()),
            ..Default::default()
        })
        .len(),
        2
    );
    assert_eq!(
        ask(AuditQuery {
            action: Some("transition".into()),
            ..Default::default()
        })
        .len(),
        1
    );
    assert_eq!(
        ask(AuditQuery {
            actor: Some("panel".into()),
            ..Default::default()
        })
        .len(),
        0
    );
    assert_eq!(
        ask(AuditQuery {
            key_contains: Some("mem:episodic".into()),
            ..Default::default()
        })
        .len(),
        3
    );
    assert_eq!(
        ask(AuditQuery {
            since: Some(f64::MAX),
            ..Default::default()
        })
        .len(),
        0
    );
    assert_eq!(
        ask(AuditQuery {
            limit: 1,
            ..Default::default()
        })
        .len(),
        1
    );
    assert_eq!(
        ask(AuditQuery {
            limit: 1,
            offset: 2,
            ..Default::default()
        })
        .len(),
        1
    );
}
