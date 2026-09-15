//! The 6.x startup migrations, run after an import.
//!
//! Ported from `mcp_server/memory/migrations.py`, each as a pure function over
//! every memory's fields so the rules can be tested directly, then persisted
//! in one transaction. All are idempotent: they only fill in what is missing,
//! and never bump `updated_at` (the skill compiler reads that as "source
//! changed").
//!
//! Not here yet: `migrate_project_domains`, which needs the domain
//! normalisation the engine owns (phase 3).

use std::collections::{BTreeMap, HashMap};

use omnimem_core::classification::{
    DEFAULT_CLASSIFICATION, LICENCE_OWN, LICENCE_UNKNOWN, PROVENANCE_ASSERTED,
    PROVENANCE_CONCLUDED, PROVENANCE_RETRIEVED, RSS_PROJECT_LABEL,
};
use omnimem_core::content_hash;
use serde::Serialize;
use tracing::info;

use crate::store::{Fields, memory_namespace, merge_memory, parse_fields};
use crate::{Result, Store};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct MigrationReport {
    pub state: usize,
    pub project_names: usize,
    pub rss_projects: usize,
    pub licence: usize,
    pub provenance: usize,
    /// Memories given v7 identity fields they lacked.
    pub identity: usize,
    /// Content hashes shared by more than one memory. Expected, not an
    /// error: a local dedup opportunity, and it affects network collapse.
    pub content_hash_collisions: usize,
}

/// key -> fields, for every memory, in key order.
type Records = BTreeMap<String, Fields>;

fn namespace_of(key: &str) -> &str {
    key.split(':').nth(1).unwrap_or("")
}

fn nonempty<'a>(fields: &'a Fields, name: &str) -> Option<&'a str> {
    fields
        .get(name)
        .map(String::as_str)
        .filter(|v| !v.is_empty())
}

/// Sets `name` to `value` on `key`, recording that the record changed.
fn stamp(
    records: &mut Records,
    changed: &mut BTreeMap<String, Fields>,
    key: &str,
    name: &str,
    value: &str,
) {
    if let Some(fields) = records.get_mut(key) {
        fields.insert(name.to_owned(), value.to_owned());
    }
    changed
        .entry(key.to_owned())
        .or_default()
        .insert(name.to_owned(), value.to_owned());
}

pub(crate) fn missing_state(
    records: &mut Records,
    changed: &mut BTreeMap<String, Fields>,
) -> usize {
    let keys: Vec<String> = records
        .iter()
        .filter(|(k, f)| {
            matches!(
                namespace_of(k),
                "episodic" | "project" | "knowledge" | "preference"
            ) && nonempty(f, "state").is_none()
        })
        .map(|(k, _)| k.clone())
        .collect();
    for key in &keys {
        stamp(records, changed, key, "state", "active");
    }
    keys.len()
}

pub(crate) fn project_names(
    records: &mut Records,
    changed: &mut BTreeMap<String, Fields>,
) -> usize {
    let updates: Vec<(String, String)> = records
        .iter()
        .filter(|(k, f)| namespace_of(k) == "project" && nonempty(f, "project_name").is_none())
        .filter_map(|(k, f)| nonempty(f, "project").map(|p| (k.clone(), p.to_owned())))
        .collect();
    for (key, project) in &updates {
        stamp(records, changed, key, "project_name", project);
    }
    updates.len()
}

pub(crate) fn rss_article_projects(
    records: &mut Records,
    changed: &mut BTreeMap<String, Fields>,
) -> usize {
    let keys: Vec<String> = records
        .iter()
        .filter(|(k, f)| {
            namespace_of(k) == "knowledge"
                && nonempty(f, "feed_name").is_some()
                && nonempty(f, "project").is_none()
        })
        .map(|(k, _)| k.clone())
        .collect();
    for key in &keys {
        stamp(records, changed, key, "project", RSS_PROJECT_LABEL);
    }
    keys.len()
}

/// `migrate_licence`: honest defaults. Conversation namespaces are own;
/// articles, imports and untraceable knowledge are unknown; extracted facts
/// inherit their source, resolved from what this pass stamped first and the
/// stored value second, and own when the source is gone.
pub(crate) fn licence(records: &mut Records, changed: &mut BTreeMap<String, Fields>) -> usize {
    let mut own = Vec::new();
    let mut imported = Vec::new();
    let mut pending: Vec<(String, String)> = Vec::new();
    for (key, f) in records.iter() {
        if !matches!(namespace_of(key), "episodic" | "project" | "preference")
            || nonempty(f, "licence").is_some()
        {
            continue;
        }
        if nonempty(f, "imported_at").is_some() {
            imported.push(key.clone());
        } else if let Some(source) = nonempty(f, "enriched_from") {
            pending.push((key.clone(), source.to_owned()));
        } else {
            own.push(key.clone());
        }
    }
    let mut unknown_knowledge = Vec::new();
    for (key, f) in records.iter() {
        if namespace_of(key) != "knowledge" || nonempty(f, "licence").is_some() {
            continue;
        }
        match nonempty(f, "enriched_from") {
            Some(source)
                if nonempty(f, "feed_name").is_none() && nonempty(f, "imported_at").is_none() =>
            {
                pending.push((key.clone(), source.to_owned()));
            }
            _ => unknown_knowledge.push(key.clone()),
        }
    }

    // What a fact's source reads as: this pass's own/imported stamps, else
    // the value already stored. Taken before any fact is assigned, as the
    // Python migration reads the store before writing the facts.
    let mut resolved: HashMap<String, Option<String>> = HashMap::new();
    for key in &own {
        resolved.insert(key.clone(), Some(LICENCE_OWN.to_owned()));
    }
    for key in &imported {
        resolved.insert(key.clone(), Some(LICENCE_UNKNOWN.to_owned()));
    }
    for (_, source) in &pending {
        resolved.entry(source.clone()).or_insert_with(|| {
            records
                .get(source)
                .and_then(|f| nonempty(f, "licence"))
                .map(str::to_owned)
        });
    }

    let mut total = 0;
    for key in own {
        stamp(records, changed, &key, "licence", LICENCE_OWN);
        total += 1;
    }
    for key in imported.into_iter().chain(unknown_knowledge) {
        stamp(records, changed, &key, "licence", LICENCE_UNKNOWN);
        total += 1;
    }
    for (key, source) in pending {
        let value = resolved
            .get(&source)
            .cloned()
            .flatten()
            .unwrap_or_else(|| LICENCE_OWN.to_owned());
        stamp(records, changed, &key, "licence", &value);
        total += 1;
    }
    total
}

fn is_project_context(key: &str, f: &Fields) -> bool {
    if let Some(name) = nonempty(f, "project_name")
        && key == format!("mem:project:{name}")
    {
        return true;
    }
    nonempty(f, "stack").is_some() || nonempty(f, "goals").is_some()
}

/// `migrate_provenance`: articles retrieved, preferences and project context
/// asserted, facts inherit (concluded when the source is gone), everything
/// else, every episodic memory included, concluded.
pub(crate) fn provenance(records: &mut Records, changed: &mut BTreeMap<String, Fields>) -> usize {
    let mut assigned: BTreeMap<String, &'static str> = BTreeMap::new();
    let mut pending: Vec<(String, String)> = Vec::new();
    for ns in ["episodic", "project", "preference", "knowledge"] {
        for (key, f) in records.iter().filter(|(k, _)| namespace_of(k) == ns) {
            if nonempty(f, "provenance").is_some() {
                continue;
            }
            if nonempty(f, "feed_name").is_some() {
                assigned.insert(key.clone(), PROVENANCE_RETRIEVED);
            } else if let Some(source) = nonempty(f, "enriched_from") {
                pending.push((key.clone(), source.to_owned()));
            } else if ns == "preference" || (ns == "project" && is_project_context(key, f)) {
                assigned.insert(key.clone(), PROVENANCE_ASSERTED);
            } else {
                assigned.insert(key.clone(), PROVENANCE_CONCLUDED);
            }
        }
    }
    let mut facts: Vec<(String, String)> = Vec::new();
    for (key, source) in pending {
        let value = assigned
            .get(&source)
            .map(|v| (*v).to_owned())
            .or_else(|| {
                records
                    .get(&source)
                    .and_then(|f| nonempty(f, "provenance"))
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| PROVENANCE_CONCLUDED.to_owned());
        facts.push((key, value));
    }
    let total = assigned.len() + facts.len();
    for (key, value) in assigned {
        stamp(records, changed, &key, "provenance", value);
    }
    for (key, value) in facts {
        stamp(records, changed, &key, "provenance", &value);
    }
    total
}

/// v7 spec §7: content_hash, epoch 1, origin_id, default classification.
pub(crate) fn identity(
    records: &mut Records,
    changed: &mut BTreeMap<String, Fields>,
    origin_id: &str,
) -> (usize, usize) {
    let mut touched = 0;
    let keys: Vec<String> = records.keys().cloned().collect();
    for key in &keys {
        let f = &records[key];
        let mut updates: Vec<(&str, String)> = Vec::new();
        if let Some(content) = nonempty(f, "content")
            && nonempty(f, "content_hash").is_none()
        {
            updates.push(("content_hash", content_hash(content)));
        }
        if nonempty(f, "epoch").is_none() {
            updates.push(("epoch", "1".to_owned()));
        }
        if nonempty(f, "origin_id").is_none() {
            updates.push(("origin_id", origin_id.to_owned()));
        }
        if nonempty(f, "classification").is_none() {
            updates.push(("classification", DEFAULT_CLASSIFICATION.to_owned()));
        }
        if !updates.is_empty() {
            touched += 1;
        }
        for (name, value) in updates {
            stamp(records, changed, key, name, &value);
        }
    }
    let mut hashes: HashMap<&str, usize> = HashMap::new();
    for f in records.values() {
        if let Some(h) = nonempty(f, "content_hash") {
            *hashes.entry(h).or_default() += 1;
        }
    }
    let collisions = hashes.values().filter(|n| **n > 1).count();
    (touched, collisions)
}

impl Store {
    /// Run every migration over the whole store.
    pub fn run_migrations(&self) -> Result<MigrationReport> {
        let mut records: Records = BTreeMap::new();
        {
            let conn = self.conn();
            let mut stmt = conn.prepare("SELECT key, fields FROM memories ORDER BY key")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let key: String = row.get(0)?;
                let raw: String = row.get(1)?;
                let fields = parse_fields(&key, &raw)?;
                records.insert(key, fields);
            }
        }

        let mut changed: BTreeMap<String, Fields> = BTreeMap::new();
        let mut report = MigrationReport {
            state: missing_state(&mut records, &mut changed),
            project_names: project_names(&mut records, &mut changed),
            rss_projects: rss_article_projects(&mut records, &mut changed),
            ..MigrationReport::default()
        };
        report.licence = licence(&mut records, &mut changed);
        report.provenance = provenance(&mut records, &mut changed);
        (report.identity, report.content_hash_collisions) =
            identity(&mut records, &mut changed, &self.origin_id);

        if !changed.is_empty() {
            let mut conn = self.conn();
            let tx = conn.transaction()?;
            for (key, updates) in &changed {
                merge_memory(&tx, key, memory_namespace(key)?, updates, &self.origin_id)?;
            }
            tx.commit()?;
        }
        if report != MigrationReport::default() {
            info!(?report, "migrations applied");
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn records(entries: &[(&str, &[(&str, &str)])]) -> Records {
        entries
            .iter()
            .map(|(k, fields)| {
                (
                    (*k).to_owned(),
                    fields
                        .iter()
                        .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
                        .collect(),
                )
            })
            .collect()
    }

    fn get<'a>(r: &'a Records, key: &str, field: &str) -> Option<&'a str> {
        r.get(key).and_then(|f| f.get(field)).map(String::as_str)
    }

    #[test]
    fn state_names_and_rss_labels() {
        let mut r = records(&[
            ("mem:episodic:a", &[("content", "x")]),
            ("mem:episodic:b", &[("state", "archived")]),
            ("mem:project:c", &[("project", "omnimem")]),
            ("mem:knowledge:d", &[("feed_name", "n8n Blog")]),
            (
                "mem:knowledge:e",
                &[("feed_name", "Other"), ("project", "Mine")],
            ),
        ]);
        let mut changed = BTreeMap::new();
        assert_eq!(missing_state(&mut r, &mut changed), 4);
        assert_eq!(get(&r, "mem:episodic:b", "state"), Some("archived"));
        assert_eq!(project_names(&mut r, &mut changed), 1);
        assert_eq!(get(&r, "mem:project:c", "project_name"), Some("omnimem"));
        assert_eq!(rss_article_projects(&mut r, &mut changed), 1);
        assert_eq!(get(&r, "mem:knowledge:d", "project"), Some("RSS"));
        assert_eq!(get(&r, "mem:knowledge:e", "project"), Some("Mine"));
    }

    #[test]
    fn licence_is_honest() {
        let mut r = records(&[
            ("mem:episodic:own", &[("content", "a decision")]),
            ("mem:episodic:imp", &[("imported_at", "1")]),
            ("mem:episodic:set", &[("licence", "open")]),
            ("mem:knowledge:article", &[("feed_name", "Feed")]),
            ("mem:knowledge:plain", &[("content", "no origin")]),
            (
                "mem:knowledge:fact-of-own",
                &[("enriched_from", "mem:episodic:own")],
            ),
            (
                "mem:knowledge:fact-of-set",
                &[("enriched_from", "mem:episodic:set")],
            ),
            (
                "mem:knowledge:fact-of-imported",
                &[("enriched_from", "mem:episodic:imp")],
            ),
            (
                "mem:preference:fact-of-gone",
                &[("enriched_from", "mem:episodic:deleted")],
            ),
            (
                "mem:knowledge:imported-fact",
                &[("enriched_from", "mem:episodic:own"), ("imported_at", "1")],
            ),
        ]);
        licence(&mut r, &mut BTreeMap::new());
        let l = |k: &str| get(&r, k, "licence");
        assert_eq!(l("mem:episodic:own"), Some("own"));
        assert_eq!(l("mem:episodic:imp"), Some("unknown"));
        assert_eq!(
            l("mem:episodic:set"),
            Some("open"),
            "a set value is never revisited"
        );
        assert_eq!(l("mem:knowledge:article"), Some("unknown"));
        assert_eq!(l("mem:knowledge:plain"), Some("unknown"));
        assert_eq!(l("mem:knowledge:fact-of-own"), Some("own"));
        assert_eq!(l("mem:knowledge:fact-of-set"), Some("open"));
        assert_eq!(l("mem:knowledge:fact-of-imported"), Some("unknown"));
        assert_eq!(l("mem:preference:fact-of-gone"), Some("own"));
        assert_eq!(l("mem:knowledge:imported-fact"), Some("unknown"));
    }

    #[test]
    fn provenance_classes() {
        let mut r = records(&[
            ("mem:episodic:e", &[("content", "work")]),
            ("mem:preference:p", &[("content", "always")]),
            ("mem:project:omnimem", &[("project_name", "omnimem")]),
            ("mem:project:01ULID", &[("project_name", "omnimem")]),
            ("mem:project:legacy", &[("stack", "python")]),
            ("mem:knowledge:a", &[("feed_name", "Feed")]),
            (
                "mem:knowledge:fact",
                &[("enriched_from", "mem:preference:p")],
            ),
            (
                "mem:knowledge:orphan",
                &[("enriched_from", "mem:episodic:gone")],
            ),
            ("mem:knowledge:kept", &[("provenance", "asserted")]),
        ]);
        provenance(&mut r, &mut BTreeMap::new());
        let p = |k: &str| get(&r, k, "provenance");
        assert_eq!(p("mem:episodic:e"), Some("concluded"));
        assert_eq!(p("mem:preference:p"), Some("asserted"));
        assert_eq!(p("mem:project:omnimem"), Some("asserted"));
        assert_eq!(p("mem:project:01ULID"), Some("concluded"));
        assert_eq!(p("mem:project:legacy"), Some("asserted"));
        assert_eq!(p("mem:knowledge:a"), Some("retrieved"));
        assert_eq!(p("mem:knowledge:fact"), Some("asserted"));
        assert_eq!(p("mem:knowledge:orphan"), Some("concluded"));
        assert_eq!(p("mem:knowledge:kept"), Some("asserted"));
    }

    #[test]
    fn identity_fills_gaps_and_counts_collisions() {
        let mut r = records(&[
            ("mem:episodic:a", &[("content", "same text")]),
            ("mem:episodic:b", &[("content", "same text  ")]),
            ("mem:skill:gen:x-local", &[("name", "x")]),
            (
                "mem:episodic:c",
                &[
                    ("content", "done"),
                    ("content_hash", "sha256:kept"),
                    ("epoch", "3"),
                    ("origin_id", "o"),
                    ("classification", "{}"),
                ],
            ),
        ]);
        let (touched, collisions) = identity(&mut r, &mut BTreeMap::new(), "node");
        assert_eq!(touched, 3);
        assert_eq!(collisions, 1, "trailing whitespace hashes the same");
        assert_eq!(get(&r, "mem:episodic:a", "epoch"), Some("1"));
        assert_eq!(get(&r, "mem:episodic:a", "origin_id"), Some("node"));
        assert_eq!(get(&r, "mem:skill:gen:x-local", "content_hash"), None);
        assert_eq!(get(&r, "mem:episodic:c", "epoch"), Some("3"));
    }

    #[test]
    fn migrations_are_idempotent_on_a_store() {
        let store = Store::open_in_memory().unwrap();
        store
            .set_fields(
                "mem:knowledge:k",
                &Fields::from([("feed_name".to_owned(), "F".to_owned())]),
            )
            .unwrap();
        let first = store.run_migrations().unwrap();
        assert_eq!(first.licence, 1);
        assert_eq!(first.rss_projects, 1);
        let second = store.run_migrations().unwrap();
        assert_eq!(
            second.licence
                + second.provenance
                + second.state
                + second.rss_projects
                + second.identity,
            0
        );
        let f = store.get("mem:knowledge:k").unwrap().unwrap();
        assert_eq!(f["licence"], "unknown");
        assert_eq!(f["provenance"], "retrieved");
    }
}
