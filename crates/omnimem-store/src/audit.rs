//! The audit log: one row per change, written in the transaction of the
//! change it records, so the log can't say something happened that didn't
//! and can't miss something that did.
//!
//! Three things come together here.
//!
//! - **The level** (`AUDIT_LOG`) lives on the [`Store`]. At `off`, the
//!   default, a write costs one atomic load more than it did.
//! - **Who is calling** is an ambient, per-thread context ([`enter`]). Every
//!   store and engine call is synchronous, and the places that make them
//!   (the MCP handler, the panel, the workers, the CLI) each run on one
//!   thread for the length of a call, so a guard on that thread is all the
//!   attribution there is. Nothing here may be held across an `.await`.
//! - **What the change was** is an optional [`Op`] the engine sets around a
//!   write (`transition`, `retag`, `set_licence`...). Without one the store
//!   still records the write, as `remember` (a new memory), `update` or
//!   `delete`, so a write nobody annotated is never missing from the log.
//!
//! The store hooks sit in the write paths that carry a person's or an
//! agent's change (`upsert`, `set_fields`, `set_fields_multi`,
//! `set_fields_each`, `delete_many`, suppression's `set_add` and
//! `set_remove`, and a restore). Counters, recall logs, caches and
//! migrations don't go through them, so a recall never writes an audit row.

use std::cell::RefCell;
use std::sync::atomic::Ordering;

use rusqlite::{Connection, params, params_from_iter};
use serde_json::{Map, Value, json};

use crate::store::Fields;
use crate::time::now;
use crate::{Result, Store};

/// The key of the suppressed-topics set, the one non-memory record whose
/// changes are audited (as `suppress` and `unsuppress`).
pub(crate) const SUPPRESSED_TOPICS: &str = "topics:suppressed";

/// The longest client name kept. It comes from the client, so it is bounded
/// and stripped of control characters before it reaches the log.
const MAX_CLIENT_CHARS: usize = 200;

/// How much of the log to keep: what `AUDIT_LOG` selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum AuditLevel {
    /// Nothing is written.
    #[default]
    Off,
    /// State changes and deletes, suppression and skill writes.
    Lifecycle,
    /// Lifecycle plus every write, without memory text.
    Writes,
    /// Writes plus the content before and after. The only level that holds
    /// memory text.
    Full,
}

impl AuditLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Lifecycle => "lifecycle",
            Self::Writes => "writes",
            Self::Full => "full",
        }
    }

    /// `off`, `lifecycle`, `writes` or `full`, in any case. Anything else is
    /// not a level.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "lifecycle" => Some(Self::Lifecycle),
            "writes" => Some(Self::Writes),
            "full" => Some(Self::Full),
            _ => None,
        }
    }

    fn to_u8(self) -> u8 {
        self as u8
    }

    fn from_u8(n: u8) -> Self {
        match n {
            1 => Self::Lifecycle,
            2 => Self::Writes,
            3 => Self::Full,
            _ => Self::Off,
        }
    }
}

/// Who made a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Actor {
    /// An MCP client (an agent), named in the row's `client`.
    Mcp,
    /// The settings window.
    Panel,
    /// A Claude Code hook.
    Hook,
    /// The maintenance pass, and the startup seeding.
    Maintenance,
    /// The enrichment worker (fact extraction).
    Enrichment,
    /// The RSS scheduler, or `omnimem rss`.
    Rss,
    /// An `omnimem` command run by hand.
    Cli,
    /// `omnimem import`.
    Import,
    /// A write nobody attributed. Every entry point sets an actor, so this
    /// only shows a path that was missed.
    Unknown,
}

impl Actor {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mcp => "mcp",
            Self::Panel => "panel",
            Self::Hook => "hook",
            Self::Maintenance => "maintenance",
            Self::Enrichment => "enrichment",
            Self::Rss => "rss",
            Self::Cli => "cli",
            Self::Import => "import",
            Self::Unknown => "unknown",
        }
    }

    /// An actor name as it is spelt in the log, for filters.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw.trim().to_ascii_lowercase().as_str() {
            "mcp" => Self::Mcp,
            "panel" => Self::Panel,
            "hook" => Self::Hook,
            "maintenance" => Self::Maintenance,
            "enrichment" => Self::Enrichment,
            "rss" => Self::Rss,
            "cli" => Self::Cli,
            "import" => Self::Import,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }
}

/// What the engine says a write is, for the write it wraps.
#[derive(Debug, Clone, Default)]
pub struct Op {
    action: &'static str,
    level: Option<AuditLevel>,
    reason: Option<String>,
    detail: Option<Map<String, Value>>,
    fold_into: Option<String>,
}

impl Op {
    /// An action named as it is logged: `transition`, `retag`, `forget`...
    pub fn new(action: &'static str) -> Self {
        Self {
            action,
            ..Self::default()
        }
    }

    /// The lowest level this action is recorded at. Without one, a write that
    /// changes `state` or deletes is `lifecycle` and any other is `writes`.
    #[must_use]
    pub fn at(mut self, level: AuditLevel) -> Self {
        self.level = Some(level);
        self
    }

    /// The reason the caller gave. An empty one is no reason.
    #[must_use]
    pub fn reason(mut self, reason: Option<&str>) -> Self {
        self.reason = reason.filter(|r| !r.is_empty()).map(str::to_owned);
        self
    }

    /// Facts about the change that the field diff can't show, merged into
    /// the row's `to` (`linked_to` for a contradiction, say).
    #[must_use]
    pub fn detail(mut self, detail: Value) -> Self {
        if let Value::Object(map) = detail {
            self.detail = Some(map);
        }
        self
    }

    /// Fold every memory this write touches into one entry against `source`,
    /// listing them: the derived-fact cascade is one row per source, not one
    /// per fact.
    #[must_use]
    pub fn fold_into(mut self, source: &str) -> Self {
        self.fold_into = Some(source.to_owned());
        self
    }
}

#[derive(Debug, Clone, Default)]
struct Ambient {
    actor: Option<Actor>,
    client: String,
    op: Option<Op>,
}

thread_local! {
    static AMBIENT: RefCell<Ambient> = RefCell::new(Ambient::default());
}

/// Restores the previous context when dropped.
#[must_use = "the context ends when the guard is dropped"]
pub struct ScopeGuard {
    previous: Ambient,
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        AMBIENT.with(|a| *a.borrow_mut() = std::mem::take(&mut self.previous));
    }
}

/// Attribute the changes this thread makes to `actor`, until the guard is
/// dropped. `client` is the MCP client's name (and OAuth client id), empty
/// for the internal actors.
pub fn enter(actor: Actor, client: &str) -> ScopeGuard {
    let client: String = client
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_CLIENT_CHARS)
        .collect();
    let previous = AMBIENT.with(|a| {
        a.replace(Ambient {
            actor: Some(actor),
            client,
            op: None,
        })
    });
    ScopeGuard { previous }
}

/// Restores the previous operation when dropped.
#[must_use = "the operation ends when the guard is dropped"]
pub struct OpGuard {
    previous: Option<Op>,
}

impl Drop for OpGuard {
    fn drop(&mut self) {
        let previous = self.previous.take();
        AMBIENT.with(|a| a.borrow_mut().op = previous);
    }
}

/// Name the writes this thread makes until the guard is dropped. An
/// operation with no reason of its own takes the enclosing one's, so a
/// caller can say why (an approval, say) around the engine's own `Op`.
pub fn op(mut op: Op) -> OpGuard {
    let previous = AMBIENT.with(|a| {
        let mut ambient = a.borrow_mut();
        if op.reason.is_none() {
            op.reason = ambient.op.as_ref().and_then(|o| o.reason.clone());
        }
        ambient.op.replace(op)
    });
    OpGuard { previous }
}

/// One row of the log.
#[derive(Debug, Clone, PartialEq)]
pub struct AuditRow {
    pub id: i64,
    pub at: f64,
    pub actor: String,
    pub client: String,
    pub action: String,
    pub key: String,
    pub from: Value,
    pub to: Value,
    pub reason: String,
}

impl AuditRow {
    /// The row as the CLI prints it and a backup carries it.
    pub fn to_json(&self) -> Value {
        json!({
            "at": self.at,
            "actor": self.actor,
            "client": self.client,
            "action": self.action,
            "key": self.key,
            "from": self.from,
            "to": self.to,
            "reason": self.reason,
        })
    }
}

/// Which rows to read, newest first. Empty means no restriction.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AuditQuery {
    /// Exactly this key (or topic).
    pub key: Option<String>,
    /// Keys containing this text.
    pub key_contains: Option<String>,
    pub actor: Option<String>,
    pub action: Option<String>,
    /// At or after this instant, in Unix seconds.
    pub since: Option<f64>,
    /// Before this instant.
    pub until: Option<f64>,
    /// At most this many rows; 0 means 100.
    pub limit: usize,
    pub offset: usize,
}

// Fields whose old and new values are kept below `full`: how a memory is
// classified, filed and in what state, never what it says.
const KEPT: [&str; 20] = [
    "state",
    "surface_score",
    "tags",
    "project",
    "project_name",
    "licence",
    "licence_note",
    "provenance",
    "feed_name",
    "enriched_from",
    "source_doc_id",
    "doc_id",
    "content_hash",
    "name",
    "domain",
    "outcome",
    "effort_score",
    "experience_weight",
    "superseded_by",
    "supersedes",
];

// Never recorded at any level: they change on every touch (or every recall)
// and say nothing a reader wants, and the rest is identity the key shows.
const NEVER: [&str; 6] = [
    "updated_at",
    "recall_count",
    "last_recalled",
    "origin_id",
    "epoch",
    "vector",
];

fn wanted(name: &str, full: bool) -> bool {
    !NEVER.contains(&name) && (full || KEPT.contains(&name))
}

// What a row says about the change rather than the memory, kept when the
// memory is forgotten: the namespace, which fields changed (by name), the
// facts that moved, a suppression, a contradiction link's other side, a
// resolution's choice, a project and the scrub count itself. Everything
// else in a row that isn't in `KEPT` is a field only `full` records.
const BOOKKEEPING: [&str; 14] = [
    "namespace",
    "other_fields",
    "facts",
    "suppressed",
    "linked_to",
    "key_a",
    "key_b",
    "outcome",
    "resolved_by",
    "winner",
    "retire",
    "project",
    "derived_facts",
    "scrubbed",
];

fn scrub_map(map: &mut Map<String, Value>) -> bool {
    let before = map.len();
    map.retain(|name, _| KEPT.contains(&name.as_str()) || BOOKKEEPING.contains(&name.as_str()));
    map.len() != before
}

/// #19: a forget erases what a memory said from the log, not only from the
/// store. Every row for each key loses the fields only `full` keeps (its
/// text, and anything else of the memory's), and a contradiction link on
/// another memory that points at it loses the explanation, which may quote
/// it. Who, when, the action, the reason and the state stay, so the log
/// still shows that it happened. Runs whatever the level is now, since
/// rows written at `full` outlive a change of level, and inside the
/// delete's own transaction, so it rolls back with it. Returns how many
/// rows changed for each key.
pub(crate) fn scrub_forgotten(
    conn: &Connection,
    keys: &[&str],
) -> Result<std::collections::HashMap<String, usize>> {
    let mut counts = std::collections::HashMap::new();
    if keys.is_empty() {
        return Ok(counts);
    }
    let marks = std::iter::repeat_n("?", keys.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT id, key, changed_from, changed_to FROM audit
         WHERE key IN ({marks}) OR json_extract(changed_to, '$.linked_to') IN ({marks})"
    );
    let rows: Vec<(i64, String, String, String)> = {
        let mut stmt = conn.prepare(&sql)?;
        stmt.query_map(params_from_iter(keys.iter().chain(keys)), |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?
    };
    let object = |raw: &str| match serde_json::from_str(raw) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    };
    for (id, key, from, to) in rows {
        let (mut from, mut to) = (object(&from), object(&to));
        let own = keys.contains(&key.as_str());
        // A link row is counted against the forgotten memory it points at.
        let counted = if own {
            key
        } else {
            to_linked(&to).unwrap_or(key)
        };
        let changed = if own {
            scrub_map(&mut from) | scrub_map(&mut to)
        } else {
            // Another memory's link to the forgotten one: only the model's
            // explanation of the conflict can quote it.
            to.remove("explanation").is_some()
        };
        if !changed {
            continue;
        }
        conn.prepare_cached("UPDATE audit SET changed_from = ?1, changed_to = ?2 WHERE id = ?3")?
            .execute(params![
                Value::Object(from).to_string(),
                Value::Object(to).to_string(),
                id
            ])?;
        *counts.entry(counted).or_insert(0) += 1;
    }
    Ok(counts)
}

fn to_linked(to: &Map<String, Value>) -> Option<String> {
    to.get("linked_to")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// A stored value as JSON: a tag list is the array it spells, anything else
/// the text.
fn value_of(name: &str, raw: &str) -> Value {
    if name == "tags"
        && let Ok(parsed @ Value::Array(_)) = serde_json::from_str(raw)
    {
        return parsed;
    }
    Value::String(raw.to_owned())
}

fn namespace_of(key: &str) -> Option<&str> {
    key.strip_prefix("mem:")?.split(':').next()
}

fn state_of(fields: &Fields) -> &str {
    fields.get("state").map_or("active", String::as_str)
}

/// What changed between `old` and `new`, as the `from` and `to` objects.
/// A new memory has no `from`; a deleted one no fields in `to` beyond its
/// state.
fn diff(
    key: &str,
    old: Option<&Fields>,
    new: Option<&Fields>,
    full: bool,
) -> (Map<String, Value>, Map<String, Value>) {
    let mut from = Map::new();
    let mut to = Map::new();
    let present = |fields: &Fields, out: &mut Map<String, Value>| {
        for (name, raw) in fields {
            if wanted(name, full) && !raw.is_empty() {
                out.insert(name.clone(), value_of(name, raw));
            }
        }
    };
    match (old, new) {
        (None, Some(new)) => {
            present(new, &mut to);
            if let Some(ns) = namespace_of(key) {
                to.insert("namespace".into(), ns.into());
            }
        }
        (Some(old), None) => {
            present(old, &mut from);
            if let Some(ns) = namespace_of(key) {
                from.insert("namespace".into(), ns.into());
            }
            to.insert("state".into(), "deleted".into());
        }
        (Some(old), Some(new)) => {
            let names: std::collections::BTreeSet<&String> = old.keys().chain(new.keys()).collect();
            let mut others = Vec::new();
            for name in names {
                if NEVER.contains(&name.as_str()) {
                    continue;
                }
                let (before, after) = (old.get(name), new.get(name));
                // A record with no state is active, as the engine reads it,
                // so its first transition reads "active -> archived".
                let implied = (name == "state" && before.is_none()).then(|| "active".to_owned());
                let before = before.or(implied.as_ref());
                if before.map(String::as_str).unwrap_or_default()
                    == after.map(String::as_str).unwrap_or_default()
                {
                    continue;
                }
                if !wanted(name, full) {
                    // Changed, but not something the level shows: say which
                    // field, never what it held.
                    others.push(Value::String(name.clone()));
                    continue;
                }
                from.insert(
                    name.clone(),
                    before.map_or(Value::Null, |raw| value_of(name, raw)),
                );
                to.insert(
                    name.clone(),
                    after.map_or(Value::Null, |raw| value_of(name, raw)),
                );
            }
            if !others.is_empty() {
                to.insert("other_fields".into(), Value::Array(others));
            }
        }
        (None, None) => {}
    }
    (from, to)
}

/// The log's writer for one store call: reads the level and the ambient
/// context once, then records each change into the call's own transaction.
pub(crate) struct Recorder {
    level: AuditLevel,
    actor: Actor,
    client: String,
    op: Option<Op>,
    at: f64,
    /// Memories folded into one entry (see [`Op::fold_into`]): key, old
    /// state, new state.
    folded: Vec<(String, Value, Value)>,
    /// Rows [`scrub_forgotten`] emptied, per deleted key, for the delete's
    /// own entry to say.
    pub(crate) scrubbed: std::collections::HashMap<String, usize>,
}

impl Store {
    /// A writer for one call. Nothing is looked up when the log is off.
    pub(crate) fn recorder(&self) -> Recorder {
        let level = self.audit_level();
        let ambient = if level == AuditLevel::Off {
            Ambient::default()
        } else {
            AMBIENT.with(|a| a.borrow().clone())
        };
        Recorder {
            level,
            actor: ambient.actor.unwrap_or(Actor::Unknown),
            client: ambient.client,
            op: ambient.op,
            at: now(),
            folded: Vec::new(),
            scrubbed: std::collections::HashMap::new(),
        }
    }

    /// Set what `AUDIT_LOG` selects. The engine does this when it opens;
    /// a command that opens the store alone does it itself.
    pub fn set_audit_level(&self, level: AuditLevel) {
        self.audit_level.store(level.to_u8(), Ordering::Relaxed);
    }

    pub fn audit_level(&self) -> AuditLevel {
        AuditLevel::from_u8(self.audit_level.load(Ordering::Relaxed))
    }
}

impl Recorder {
    /// True when changes are being recorded at all.
    pub(crate) fn enabled(&self) -> bool {
        self.level != AuditLevel::Off
    }

    fn insert(
        &self,
        conn: &Connection,
        action: &str,
        key: &str,
        from: &Map<String, Value>,
        to: &Map<String, Value>,
    ) -> Result<()> {
        let reason = self
            .op
            .as_ref()
            .and_then(|o| o.reason.as_deref())
            .unwrap_or_default();
        conn.prepare_cached(
            "INSERT INTO audit (at, actor, client, action, key, changed_from, changed_to, reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?
        .execute(params![
            self.at,
            self.actor.as_str(),
            self.client,
            action,
            key,
            Value::Object(from.clone()).to_string(),
            Value::Object(to.clone()).to_string(),
            reason,
        ])?;
        Ok(())
    }

    /// A memory went from `old` to `new` (`None` for not there). Records it
    /// when the level asks for this kind of change and something worth
    /// showing changed.
    pub(crate) fn memory(
        &mut self,
        conn: &Connection,
        key: &str,
        old: Option<&Fields>,
        new: Option<&Fields>,
    ) -> Result<()> {
        if !self.enabled() {
            return Ok(());
        }
        let state_changed = matches!((old, new), (Some(o), Some(n)) if state_of(o) != state_of(n));
        let lifecycle = new.is_none() || state_changed;
        let explicit = self.op.as_ref().and_then(|o| o.level);
        let required = explicit.unwrap_or(if lifecycle {
            AuditLevel::Lifecycle
        } else {
            AuditLevel::Writes
        });
        if self.level < required {
            return Ok(());
        }
        // A delete's own row keeps no text even at `full`: the point of a
        // forget is that what the memory said goes (#19).
        let keep_text = self.level == AuditLevel::Full && new.is_some();
        let (from, mut to) = diff(key, old, new, keep_text);
        if new.is_none()
            && let Some(&n) = self.scrubbed.get(key)
        {
            to.insert("scrubbed".into(), n.into());
        }
        let created = old.is_none();
        if !created && new.is_some() && from.is_empty() && to.is_empty() {
            // Nothing moved but the fields that change on every touch.
            return Ok(());
        }
        if self.op.as_ref().is_some_and(|o| o.fold_into.is_some()) {
            let before = from.get("state").cloned().unwrap_or(Value::Null);
            let after = to
                .get("state")
                .cloned()
                .unwrap_or_else(|| new.map_or("deleted".into(), |n| state_of(n).into()));
            self.folded.push((key.to_owned(), before, after));
            return Ok(());
        }
        if let Some(detail) = self.op.as_ref().and_then(|o| o.detail.clone()) {
            to.extend(detail);
        }
        let action = match self.op.as_ref().map(|o| o.action).filter(|a| !a.is_empty()) {
            Some(action) => action,
            None if new.is_none() => "delete",
            None if created => "remember",
            None if state_changed => "transition",
            None => "update",
        };
        self.insert(conn, action, key, &from, &to)
    }

    /// Topics added to or removed from the suppressed set, one row each.
    pub(crate) fn topics(
        &self,
        conn: &Connection,
        key: &str,
        added: &[String],
        removed: &[String],
    ) -> Result<()> {
        if !self.enabled() || key != SUPPRESSED_TOPICS {
            return Ok(());
        }
        if self.level
            < self
                .op
                .as_ref()
                .and_then(|o| o.level)
                .unwrap_or(AuditLevel::Lifecycle)
        {
            return Ok(());
        }
        let custom = self.op.as_ref().map(|o| o.action).filter(|a| !a.is_empty());
        let rows = added
            .iter()
            .map(|t| (t, "suppress", false, true))
            .chain(removed.iter().map(|t| (t, "unsuppress", true, false)));
        for (topic, default, was, is) in rows {
            let from = Map::from_iter([("suppressed".to_owned(), Value::Bool(was))]);
            let to = Map::from_iter([("suppressed".to_owned(), Value::Bool(is))]);
            self.insert(conn, custom.unwrap_or(default), topic, &from, &to)?;
        }
        Ok(())
    }

    /// Write the folded entry, if the call touched anything it folds.
    pub(crate) fn finish(self, conn: &Connection) -> Result<()> {
        let Some(source) = self.op.as_ref().and_then(|o| o.fold_into.clone()) else {
            return Ok(());
        };
        if self.folded.is_empty() {
            return Ok(());
        }
        let mut before = Map::new();
        let mut after = Map::new();
        for (key, was, is) in &self.folded {
            before.insert(key.clone(), was.clone());
            after.insert(key.clone(), is.clone());
        }
        let from = Map::from_iter([("facts".to_owned(), Value::Object(before))]);
        let mut to = Map::from_iter([("facts".to_owned(), Value::Object(after))]);
        let scrubbed: usize = self
            .folded
            .iter()
            .filter_map(|(key, _, _)| self.scrubbed.get(key))
            .sum();
        if scrubbed > 0 {
            to.insert("scrubbed".into(), scrubbed.into());
        }
        let action = self.op.as_ref().map_or("cascade", |o| o.action);
        self.insert(conn, action, &source, &from, &to)
    }
}

fn row_of(row: &rusqlite::Row<'_>) -> rusqlite::Result<AuditRow> {
    let json_of = |raw: String| serde_json::from_str(&raw).unwrap_or(Value::Null);
    Ok(AuditRow {
        id: row.get(0)?,
        at: row.get(1)?,
        actor: row.get(2)?,
        client: row.get(3)?,
        action: row.get(4)?,
        key: row.get(5)?,
        from: json_of(row.get(6)?),
        to: json_of(row.get(7)?),
        reason: row.get(8)?,
    })
}

const ROW_COLUMNS: &str =
    "id, at, actor, client, action, key, changed_from, changed_to, reason FROM audit";

impl Store {
    /// The rows a query asks for, newest first.
    pub fn audit_entries(&self, query: &AuditQuery) -> Result<Vec<AuditRow>> {
        let mut sql = format!("SELECT {ROW_COLUMNS} WHERE 1");
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        let mut text = |sql: &mut String, clause: &str, value: &Option<String>| {
            if let Some(value) = value.as_ref().filter(|v| !v.is_empty()) {
                sql.push_str(clause);
                args.push(value.clone().into());
            }
        };
        text(&mut sql, " AND key = ?", &query.key);
        text(&mut sql, " AND instr(key, ?) > 0", &query.key_contains);
        text(&mut sql, " AND actor = ?", &query.actor);
        text(&mut sql, " AND action = ?", &query.action);
        if let Some(since) = query.since {
            sql.push_str(" AND at >= ?");
            args.push(since.into());
        }
        if let Some(until) = query.until {
            sql.push_str(" AND at < ?");
            args.push(until.into());
        }
        let limit = if query.limit == 0 { 100 } else { query.limit };
        sql.push_str(" ORDER BY at DESC, id DESC LIMIT ? OFFSET ?");
        args.push((limit as i64).into());
        args.push((query.offset as i64).into());
        let conn = self.conn();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), row_of)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn audit_count(&self) -> Result<usize> {
        let n: i64 = self
            .conn()
            .query_row("SELECT COUNT(*) FROM audit", [], |r| r.get(0))?;
        Ok(usize::try_from(n).unwrap_or(0))
    }

    /// Delete rows older than `days`; 0 keeps everything. Returns how many went.
    pub fn prune_audit(&self, days: u32) -> Result<usize> {
        if days == 0 {
            return Ok(0);
        }
        let cutoff = now() - f64::from(days) * 86_400.0;
        Ok(self
            .conn()
            .execute("DELETE FROM audit WHERE at < ?1", params![cutoff])?)
    }

    /// Every row, oldest first, as a backup carries them.
    pub fn export_audit(&self) -> Result<Vec<Value>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!("SELECT {ROW_COLUMNS} ORDER BY at, id"))?;
        let rows = stmt.query_map([], row_of)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?.to_json());
        }
        Ok(out)
    }

    /// Add a backup's rows to the log, skipping any already there: a row is
    /// the same row when it has the same time, actor, client, action, key,
    /// change and reason, so restoring a backup twice, or onto the store it
    /// came from, adds nothing. Returns (added, skipped).
    pub fn merge_audit(&self, rows: &[Value]) -> Result<(usize, usize)> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let (mut added, mut skipped) = (0, 0);
        for row in rows {
            let text = |name: &str| row.get(name).and_then(Value::as_str);
            let (Some(at), Some(actor), Some(action), Some(key)) = (
                row.get("at")
                    .and_then(Value::as_f64)
                    .filter(|a| a.is_finite()),
                text("actor"),
                text("action"),
                text("key"),
            ) else {
                skipped += 1;
                continue;
            };
            let changed = |name: &str| {
                row.get(name)
                    .filter(|v| v.is_object())
                    .map_or_else(|| "{}".to_owned(), Value::to_string)
            };
            let (client, reason) = (
                text("client").unwrap_or_default(),
                text("reason").unwrap_or_default(),
            );
            let (from, to) = (changed("from"), changed("to"));
            // `at` is a float that went through JSON, and serde_json (without
            // float_roundtrip) can read it back a unit in the last place off,
            // so the same row is matched within ten microseconds, not exactly.
            let inserted = tx.execute(
                "INSERT INTO audit (at, actor, client, action, key, changed_from, changed_to, reason)
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8
                 WHERE NOT EXISTS (
                    SELECT 1 FROM audit WHERE abs(at - ?1) < 0.00001 AND actor = ?2 AND client = ?3
                      AND action = ?4 AND key = ?5 AND changed_from = ?6
                      AND changed_to = ?7 AND reason = ?8)",
                params![at, actor, client, action, key, from, to, reason],
            )?;
            if inserted == 0 {
                skipped += 1;
            } else {
                added += 1;
            }
        }
        tx.commit()?;
        Ok((added, skipped))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(pairs: &[(&str, &str)]) -> Fields {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    /// Every row's from and to for `key`, as text, oldest first.
    fn logged(store: &Store, key: &str) -> Vec<(String, Value, Value)> {
        let mut rows = store
            .audit_entries(&AuditQuery {
                key: Some(key.to_owned()),
                limit: 100,
                ..AuditQuery::default()
            })
            .unwrap();
        rows.reverse();
        rows.into_iter().map(|r| (r.action, r.from, r.to)).collect()
    }

    fn full_store() -> Store {
        let store = Store::open_in_memory().unwrap();
        store.set_audit_level(AuditLevel::Full);
        store
    }

    const KEY: &str = "mem:episodic:01SCRUB";

    /// #19: at `full`, remember, update, then forget. The rows are all still
    /// there and still say who did what, but none holds what it said.
    #[test]
    fn a_forget_scrubs_the_text_from_every_earlier_row() {
        let store = full_store();
        let _who = enter(Actor::Mcp, "claude-code");
        store
            .set_fields(
                KEY,
                &fields(&[
                    ("content", "the launch code is 7291"),
                    ("project", "apollo"),
                ]),
            )
            .unwrap();
        store
            .set_fields(KEY, &fields(&[("content", "the launch code is 8302")]))
            .unwrap();
        let before = logged(&store, KEY);
        assert_eq!(before.len(), 2);
        assert!(
            before[1].1.to_string().contains("7291"),
            "full keeps the text: {before:?}"
        );

        assert_eq!(store.delete_many(&[KEY.to_owned()]).unwrap(), 1);
        let after = logged(&store, KEY);
        assert_eq!(after.len(), 3, "the rows stay, and the forget adds one");
        for (action, from, to) in &after {
            let text = format!("{from}{to}");
            assert!(
                !text.contains("7291") && !text.contains("8302"),
                "{action}: {text}"
            );
        }
        assert_eq!(after[0].0, "remember");
        assert_eq!(after[0].2["project"], "apollo", "where it was filed stays");
        assert_eq!(after[2].0, "delete");
        assert_eq!(after[2].2["state"], "deleted");
        assert_eq!(after[2].2["scrubbed"], 2);
    }

    #[test]
    fn the_scrub_rolls_back_with_a_delete_that_fails() {
        let store = full_store();
        let _who = enter(Actor::Mcp, "claude-code");
        store
            .set_fields(KEY, &fields(&[("content", "the launch code is 7291")]))
            .unwrap();
        // The delete fails inside its transaction, after the scrub ran.
        store
            .conn()
            .execute_batch(
                "CREATE TRIGGER refuse BEFORE DELETE ON memories
                 BEGIN SELECT RAISE(ABORT, 'refused'); END;",
            )
            .unwrap();
        assert!(store.delete_many(&[KEY.to_owned()]).is_err());
        let rows = logged(&store, KEY);
        assert_eq!(rows.len(), 1);
        assert!(
            rows[0].2.to_string().contains("7291"),
            "nothing was scrubbed"
        );
        assert!(store.get(KEY).unwrap().is_some(), "nor deleted");
    }

    /// A contradiction link on another memory keeps the model's explanation
    /// at `full`, which may quote the one being forgotten.
    #[test]
    fn a_link_explanation_that_may_quote_the_forgotten_memory_goes_too() {
        let store = full_store();
        let _who = enter(Actor::Mcp, "claude-code");
        let other = "mem:episodic:01OTHER";
        store
            .set_fields(KEY, &fields(&[("content", "port 9000")]))
            .unwrap();
        store
            .set_fields(other, &fields(&[("content", "port 8000")]))
            .unwrap();
        {
            let _op = op(Op::new("contradiction_link").detail(json!({
                "linked_to": KEY,
                "explanation": "one says port 9000, the other 8000",
            })));
            store
                .set_fields(other, &fields(&[("contradictions", r#"[{"key": "x"}]"#)]))
                .unwrap();
        }
        store.delete_many(&[KEY.to_owned()]).unwrap();
        let link = logged(&store, other)
            .into_iter()
            .find(|(action, _, _)| action == "contradiction_link")
            .unwrap();
        assert_eq!(link.2["linked_to"], KEY);
        assert!(link.2.get("explanation").is_none(), "{:?}", link.2);
        // The other memory's own text is its own business, until it goes.
        assert!(logged(&store, other)[0].2.to_string().contains("port 8000"));
        let forgot = logged(&store, KEY).pop().unwrap();
        assert_eq!(forgot.2["scrubbed"], 2, "its own remember and the link");
    }

    /// Rows written at `full` outlive a move to a lower level, so the scrub
    /// runs whatever the level is now, even off.
    #[test]
    fn the_scrub_runs_at_every_level() {
        let store = full_store();
        let _who = enter(Actor::Mcp, "claude-code");
        store
            .set_fields(KEY, &fields(&[("content", "the launch code is 7291")]))
            .unwrap();
        store.set_audit_level(AuditLevel::Off);
        store.delete_many(&[KEY.to_owned()]).unwrap();
        let rows = logged(&store, KEY);
        assert_eq!(rows.len(), 1, "off writes no row for the forget");
        assert!(!rows[0].2.to_string().contains("7291"));
    }

    #[test]
    fn levels_order_and_parse() {
        assert!(AuditLevel::Off < AuditLevel::Lifecycle);
        assert!(AuditLevel::Lifecycle < AuditLevel::Writes);
        assert!(AuditLevel::Writes < AuditLevel::Full);
        assert_eq!(AuditLevel::parse(" FULL "), Some(AuditLevel::Full));
        assert_eq!(AuditLevel::parse("verbose"), None);
        assert_eq!(AuditLevel::default(), AuditLevel::Off);
        for level in [
            AuditLevel::Off,
            AuditLevel::Lifecycle,
            AuditLevel::Writes,
            AuditLevel::Full,
        ] {
            assert_eq!(AuditLevel::from_u8(level.to_u8()), level);
            assert_eq!(AuditLevel::parse(level.as_str()), Some(level));
        }
    }

    #[test]
    fn a_new_memory_shows_where_it_is_filed_not_what_it_says() {
        let new = fields(&[
            ("content", "the secret plan"),
            ("project", "apollo"),
            ("tags", r#"["a", "b"]"#),
            ("updated_at", "1.0"),
        ]);
        let (from, to) = diff("mem:episodic:1", None, Some(&new), false);
        assert!(from.is_empty());
        assert_eq!(to["project"], "apollo");
        assert_eq!(to["tags"], json!(["a", "b"]));
        assert_eq!(to["namespace"], "episodic");
        assert!(!to.contains_key("content"));
        assert!(!to.contains_key("updated_at"));
        let (_, full) = diff("mem:episodic:1", None, Some(&new), true);
        assert_eq!(full["content"], "the secret plan");
    }

    #[test]
    fn an_edit_lists_only_what_changed() {
        let old = fields(&[("state", "active"), ("content", "a"), ("project", "p")]);
        let new = fields(&[("state", "archived"), ("content", "b"), ("project", "p")]);
        let (from, to) = diff("mem:episodic:1", Some(&old), Some(&new), false);
        assert_eq!(
            from,
            json!({"state": "active"}).as_object().cloned().unwrap()
        );
        // The content changed too, which this level names but doesn't show.
        assert_eq!(
            to,
            json!({"state": "archived", "other_fields": ["content"]})
                .as_object()
                .cloned()
                .unwrap()
        );
        let (from, to) = diff("mem:episodic:1", Some(&old), Some(&new), true);
        assert_eq!(from["content"], "a");
        assert_eq!(to["content"], "b");
        assert!(!to.contains_key("project"));
    }

    fn store_at(level: AuditLevel) -> Store {
        let store = Store::open_in_memory().unwrap();
        store.set_audit_level(level);
        store
    }

    #[test]
    fn a_row_is_written_with_its_change_and_not_without_it() {
        let store = store_at(AuditLevel::Writes);
        let _who = enter(Actor::Cli, "");
        // Make the write fail after the memory and its audit row are in the
        // transaction: the vector insert is the last thing an upsert does.
        store
            .conn()
            .execute_batch(
                "CREATE TRIGGER refuse BEFORE INSERT ON vectors
                 BEGIN SELECT RAISE(ABORT, 'refused'); END;",
            )
            .unwrap();
        let vector = vec![0.5; store.dimension()];
        let failed = store.upsert(
            "mem:episodic:a",
            &fields(&[("content", "x"), ("project", "p")]),
            Some(&vector),
        );
        assert!(failed.is_err());
        assert!(store.get("mem:episodic:a").unwrap().is_none());
        assert_eq!(store.audit_count().unwrap(), 0, "no change, so no row");

        store.conn().execute_batch("DROP TRIGGER refuse").unwrap();
        store
            .upsert(
                "mem:episodic:a",
                &fields(&[("content", "x")]),
                Some(&vector),
            )
            .unwrap();
        assert_eq!(store.audit_count().unwrap(), 1, "the change and its row");
    }

    #[test]
    fn a_failed_delete_or_suppression_leaves_no_row_either() {
        let store = store_at(AuditLevel::Lifecycle);
        let _who = enter(Actor::Cli, "");
        store
            .set_fields("mem:episodic:a", &fields(&[("content", "x")]))
            .unwrap();
        store
            .conn()
            .execute_batch(
                "CREATE TRIGGER keep BEFORE DELETE ON memories
                 BEGIN SELECT RAISE(ABORT, 'kept'); END;",
            )
            .unwrap();
        assert!(store.delete("mem:episodic:a").is_err());
        assert!(store.get("mem:episodic:a").unwrap().is_some());
        assert_eq!(store.audit_count().unwrap(), 0);

        // A set that already holds the member changes nothing, so records nothing.
        store
            .set_add(SUPPRESSED_TOPICS, &["topic one".to_owned()])
            .unwrap();
        store
            .set_add(SUPPRESSED_TOPICS, &["topic one".to_owned()])
            .unwrap();
        assert_eq!(store.audit_count().unwrap(), 1);
    }

    #[test]
    fn nothing_is_read_or_written_while_the_log_is_off() {
        let store = store_at(AuditLevel::Off);
        let _who = enter(Actor::Cli, "");
        store
            .set_fields("mem:episodic:a", &fields(&[("content", "x")]))
            .unwrap();
        store.delete("mem:episodic:a").unwrap();
        assert_eq!(store.audit_count().unwrap(), 0);
    }

    #[test]
    fn counters_that_change_on_every_recall_are_never_changes() {
        let store = store_at(AuditLevel::Full);
        let _who = enter(Actor::Mcp, "c");
        store
            .set_fields("mem:episodic:a", &fields(&[("content", "x")]))
            .unwrap();
        store
            .bump_recall_counts(&["mem:episodic:a".to_owned()], "1.0")
            .unwrap();
        // A touch that moves only `updated_at` is no change either.
        store
            .set_fields("mem:episodic:a", &fields(&[("updated_at", "99.0")]))
            .unwrap();
        assert_eq!(store.audit_count().unwrap(), 1);
    }

    #[test]
    fn rows_come_back_newest_first_and_merge_without_duplicating() {
        let store = store_at(AuditLevel::Writes);
        let _who = enter(Actor::Mcp, "c");
        for n in 0..3 {
            store
                .set_fields("mem:episodic:a", &fields(&[("project", &format!("p{n}"))]))
                .unwrap();
        }
        let rows = store.audit_entries(&AuditQuery::default()).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].to["project"], "p2");
        assert_eq!(rows[0].from["project"], "p1");

        let exported = store.export_audit().unwrap();
        let other = Store::open_in_memory().unwrap();
        assert_eq!(other.merge_audit(&exported).unwrap(), (3, 0));
        assert_eq!(other.merge_audit(&exported).unwrap(), (0, 3));
        assert_eq!(other.audit_count().unwrap(), 3);
        // A malformed row is skipped, not fatal.
        assert_eq!(other.merge_audit(&[json!({"at": "soon"})]).unwrap(), (0, 1));

        // A time read back from JSON a unit in the last place off is still the
        // same row (serde_json's float parsing isn't exact), as CI found when
        // a restore added a row it already had.
        let mut nudged = exported.clone();
        for row in &mut nudged {
            let at = row["at"].as_f64().unwrap();
            row["at"] = json!(f64::from_bits(at.to_bits() + 1));
        }
        assert_eq!(other.merge_audit(&nudged).unwrap(), (0, 3));
        assert_eq!(other.audit_count().unwrap(), 3);
    }

    #[test]
    fn a_scope_ends_when_its_guard_does() {
        let store = store_at(AuditLevel::Writes);
        {
            let _outer = enter(Actor::Panel, "");
            {
                let _inner = enter(Actor::Hook, "h");
                store
                    .set_fields("mem:episodic:a", &fields(&[("project", "x")]))
                    .unwrap();
            }
            store
                .set_fields("mem:episodic:a", &fields(&[("project", "y")]))
                .unwrap();
        }
        store
            .set_fields("mem:episodic:a", &fields(&[("project", "z")]))
            .unwrap();
        let rows = store.audit_entries(&AuditQuery::default()).unwrap();
        let actors: Vec<_> = rows.iter().rev().map(|r| r.actor.as_str()).collect();
        assert_eq!(actors, ["hook", "panel", "unknown"]);
        assert_eq!(rows[2].client, "h");
    }

    #[test]
    fn a_client_name_is_bounded_and_clean() {
        let store = store_at(AuditLevel::Writes);
        let _who = enter(Actor::Mcp, &format!("evil\n{}", "x".repeat(500)));
        store
            .set_fields("mem:episodic:a", &fields(&[("project", "x")]))
            .unwrap();
        let row = &store.audit_entries(&AuditQuery::default()).unwrap()[0];
        assert_eq!(row.client.chars().count(), MAX_CLIENT_CHARS);
        assert!(!row.client.contains('\n'));
    }
}
