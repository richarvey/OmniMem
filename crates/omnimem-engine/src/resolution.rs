//! Resolving contradictions: which side won, and a link that stops warning.
//!
//! A contradiction link is never deleted, because `link_contradiction` skips
//! a pair that is already linked and a deleted link would be found again by
//! the next scan. A resolution is written onto the link in both rows instead:
//!
//! ```json
//! {"key": "<other>", "explanation": "...", "detected_at": "...",
//!  "resolution": {"outcome": "supersedes", "winner": "<key>", "reason": "...",
//!                 "resolved_at": "...", "resolved_by": "panel", "history": []}}
//! ```
//!
//! Every reader that warns (the briefing, the session-start hook, the panel's
//! contradictions page, both scans) skips a resolved link; recall and
//! `explain_memory` still show it, resolution attached. Resolving a pair
//! again keeps the earlier resolution in `history`, and the `reopened`
//! outcome puts the warning back.
//!
//! The decision is a person's. Over MCP the transport asks them (see
//! `omnimem-mcp`); a proposal nobody could be asked about is stored here as
//! pending, for the panel or the CLI to accept or discard.

use std::time::Duration;

use omnimem_store::{AuditLevel, Fields, Op, op as audit_op};
use serde_json::{Map, Value, json};
use tracing::info;

use crate::error::{EngineError, invalid};
use crate::lifecycle::MemoryState;
use crate::pyfmt::{now_str, py_json, take_chars};
use crate::tools::{MAX_SHORT_TEXT, validate_text, validate_writable_key};
use crate::{Engine, Result};

/// A proposal waiting for a person lasts a week: long enough to span a
/// weekend, short enough that a circumstance which has since changed doesn't
/// get answered by someone who never saw the question.
pub const PENDING_TTL: Duration = Duration::from_hours(168);

const PENDING_PREFIX: &str = "meta:contradiction:pending:";

/// The explanation on a link made by hand, for a pair no scan had flagged.
const RECORDED_BY_HAND: &str = "Recorded by a person.";

/// How a contradiction ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// One memory is current and the other out of date.
    Supersedes,
    /// Not a contradiction after all: different contexts, projects or times.
    BothValid,
    /// Neither holds.
    BothWrong,
    /// The heuristic misfired.
    FalsePositive,
    /// Put the warning back; the earlier resolution stays in the history.
    Reopened,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Supersedes => "supersedes",
            Self::BothValid => "both_valid",
            Self::BothWrong => "both_wrong",
            Self::FalsePositive => "false_positive",
            Self::Reopened => "reopened",
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        match text.trim() {
            "supersedes" => Ok(Self::Supersedes),
            "both_valid" => Ok(Self::BothValid),
            "both_wrong" => Ok(Self::BothWrong),
            "false_positive" => Ok(Self::FalsePositive),
            "reopened" => Ok(Self::Reopened),
            other => Err(invalid(format!(
                "Unknown outcome '{}'. Use supersedes, both_valid, both_wrong, false_positive or reopened.",
                take_chars(other, 40)
            ))),
        }
    }
}

/// What to do with the memories a resolution finds wanting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Retire {
    #[default]
    No,
    Deprioritise,
    Archive,
}

impl Retire {
    /// `true` and `"deprioritise"` stop a memory surfacing; `"archive"`
    /// retires it. Anything else is refused rather than guessed at.
    pub fn parse(text: &str) -> Result<Self> {
        match text.trim().to_lowercase().as_str() {
            "" | "false" | "no" | "none" => Ok(Self::No),
            "true" | "yes" | "deprioritise" | "deprioritize" => Ok(Self::Deprioritise),
            "archive" => Ok(Self::Archive),
            other => Err(invalid(format!(
                "Unknown retire_loser '{}'. Use true, false, deprioritise or archive.",
                take_chars(other, 40)
            ))),
        }
    }

    fn state(self) -> Option<MemoryState> {
        match self {
            Self::No => None,
            Self::Deprioritise => Some(MemoryState::Deprioritised),
            Self::Archive => Some(MemoryState::Archived),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::No => "no",
            Self::Deprioritise => "deprioritise",
            Self::Archive => "archive",
        }
    }
}

/// One proposed resolution of a pair of linked memories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveRequest {
    pub key_a: String,
    pub key_b: String,
    pub outcome: Outcome,
    /// For `supersedes`: the memory that is current. Nothing else takes one.
    pub winner: Option<String>,
    pub reason: Option<String>,
    pub retire: Retire,
}

impl ResolveRequest {
    pub fn new(
        key_a: &str,
        key_b: &str,
        outcome: &str,
        winner: Option<&str>,
        reason: Option<&str>,
        retire: Retire,
    ) -> Result<Self> {
        let blank = |s: Option<&str>| {
            s.map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        Ok(Self {
            key_a: key_a.trim().to_owned(),
            key_b: key_b.trim().to_owned(),
            outcome: Outcome::parse(outcome)?,
            winner: blank(winner),
            reason: blank(reason),
            retire,
        })
    }

    /// The memory a `supersedes` resolution retires, when there is one.
    fn loser(&self) -> Option<&str> {
        let winner = self.winner.as_deref()?;
        if winner == self.key_a {
            Some(&self.key_b)
        } else {
            Some(&self.key_a)
        }
    }
}

/// A request checked against the store, with what a person needs to judge it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionReview {
    pub request: ResolveRequest,
    pub content_a: String,
    pub content_b: String,
}

/// Whether a link entry has been settled (and not since reopened).
pub(crate) fn link_is_resolved(link: &Value) -> bool {
    link.get("resolution")
        .and_then(|r| r.get("outcome"))
        .and_then(Value::as_str)
        .is_some_and(|outcome| outcome != Outcome::Reopened.as_str())
}

/// The links stored on a row (`contradictions`), empty when there are none
/// or the field is not a list.
pub(crate) fn parse_links(raw: Option<&String>) -> Vec<Value> {
    raw.filter(|r| !r.is_empty())
        .and_then(|r| serde_json::from_str::<Vec<Value>>(r).ok())
        .unwrap_or_default()
}

/// Whether `row` holds a settled link to `other`.
pub(crate) fn row_resolved_with(row: &Fields, other: &str) -> bool {
    parse_links(row.get("contradictions"))
        .iter()
        .any(|l| l.get("key").and_then(Value::as_str) == Some(other) && link_is_resolved(l))
}

fn pair_key(a: &str, b: &str) -> String {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    format!("{PENDING_PREFIX}{lo}|{hi}")
}

/// Add `resolution` to the link to `other` in `links`, keeping what it
/// replaces in `history`. A pair linked on one side only gets its missing
/// entry here, so both rows end up the same.
fn put_resolution(links: &mut Vec<Value>, other: &str, mut resolution: Value, now: &str) {
    let at = links
        .iter()
        .position(|l| l.get("key").and_then(Value::as_str) == Some(other));
    let at = at.unwrap_or_else(|| {
        links.push(json!({"key": other, "explanation": RECORDED_BY_HAND, "detected_at": now}));
        links.len() - 1
    });
    let Some(entry) = links[at].as_object_mut() else {
        return;
    };
    if let Some(Value::Object(mut prior)) = entry.remove("resolution") {
        let mut history = match prior.remove("history") {
            Some(Value::Array(h)) => h,
            _ => Vec::new(),
        };
        history.push(Value::Object(prior));
        resolution["history"] = Value::Array(history);
    }
    entry.insert("resolution".to_owned(), resolution);
}

/// The `supersedes` list on a row.
fn supersedes_of(row: &Fields) -> Vec<String> {
    row.get("supersedes")
        .filter(|s| !s.is_empty())
        .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
        .unwrap_or_default()
}

/// The supersession fields a row should hold after `other` stops (or starts)
/// being its counterpart: `superseded_by` cleared if it named `other`, the
/// `supersedes` list without `other`, then the new relation if there is one.
fn supersession_updates(
    row: &Fields,
    other: &str,
    this_wins: bool,
    this_loses: bool,
    updates: &mut Fields,
) {
    let mut superseded_by = row.get("superseded_by").cloned().unwrap_or_default();
    if superseded_by == other {
        superseded_by.clear();
    }
    if this_loses {
        superseded_by = other.to_owned();
    }
    let mut supersedes = supersedes_of(row);
    supersedes.retain(|k| k != other);
    if this_wins {
        supersedes.push(other.to_owned());
    }
    let supersedes = if supersedes.is_empty() {
        String::new()
    } else {
        py_json(&json!(supersedes))
    };
    if row.get("superseded_by").map_or("", String::as_str) != superseded_by {
        updates.insert("superseded_by".to_owned(), superseded_by);
    }
    if row.get("supersedes").map_or("", String::as_str) != supersedes {
        updates.insert("supersedes".to_owned(), supersedes);
    }
}

impl Engine {
    /// Check a request against the store: both memories exist, they are
    /// linked as contradicting, and the outcome, winner and retirement agree
    /// with each other. Returns what a person is shown to decide on.
    pub fn review_resolution(&self, request: &ResolveRequest) -> Result<ResolutionReview> {
        self.load_pair(request).map(|(review, _, _)| review)
    }

    #[allow(clippy::type_complexity)]
    fn load_pair(&self, r: &ResolveRequest) -> Result<(ResolutionReview, Fields, Fields)> {
        for key in [&r.key_a, &r.key_b] {
            validate_writable_key(key, "resolve contradictions of")?;
        }
        if r.key_a == r.key_b {
            return Err(invalid("A memory can't contradict itself: give two keys."));
        }
        let row = |key: &str| -> Result<Fields> {
            self.store
                .get(key)?
                .filter(|d| !d.is_empty())
                .ok_or_else(|| invalid(format!("Memory key not found: {key}")))
        };
        let (row_a, row_b) = (row(&r.key_a)?, row(&r.key_b)?);
        if let Some(reason) = &r.reason {
            validate_text("reason", reason, MAX_SHORT_TEXT)?;
        }
        match (r.outcome, &r.winner) {
            (Outcome::Supersedes, None) => {
                return Err(invalid(
                    "supersedes needs a winner: the key of the memory that is current.",
                ));
            }
            (Outcome::Supersedes, Some(w)) if *w != r.key_a && *w != r.key_b => {
                return Err(invalid("winner must be key_a or key_b."));
            }
            (Outcome::Supersedes, Some(_)) => {}
            (_, Some(_)) => {
                return Err(invalid("Only a supersedes outcome takes a winner."));
            }
            _ => {}
        }
        if r.retire != Retire::No && !matches!(r.outcome, Outcome::Supersedes | Outcome::BothWrong)
        {
            return Err(invalid(
                "retire_loser only applies to supersedes and both_wrong.",
            ));
        }
        let find = |row: &Fields, other: &str| {
            parse_links(row.get("contradictions"))
                .into_iter()
                .find(|l| l.get("key").and_then(Value::as_str) == Some(other))
        };
        let (link_a, link_b) = (find(&row_a, &r.key_b), find(&row_b, &r.key_a));
        if link_a.is_none() && link_b.is_none() {
            return Err(invalid(
                "These two memories aren't linked as contradicting each other. check_contradictions links the pairs it finds.",
            ));
        }
        if r.outcome == Outcome::Reopened && !link_a.iter().chain(&link_b).any(link_is_resolved) {
            return Err(invalid(
                "That pair isn't resolved, so there is nothing to reopen.",
            ));
        }
        let content = |row: &Fields| row.get("content").cloned().unwrap_or_default();
        let review = ResolutionReview {
            request: r.clone(),
            content_a: content(&row_a),
            content_b: content(&row_b),
        };
        Ok((review, row_a, row_b))
    }

    /// Write a resolution to both rows, then retire what it asks to. `by` is
    /// who decided: `panel`, `cli`, `mcp`, or `mcp-elicitation` for an
    /// agent's request a person accepted.
    pub fn resolve_contradiction(&self, request: &ResolveRequest, by: &str) -> Result<Value> {
        let (_, row_a, row_b) = self.load_pair(request)?;
        let now = now_str();
        let mut resolution = json!({
            "outcome": request.outcome.as_str(),
            "resolved_at": now,
            "resolved_by": by,
        });
        if let Some(winner) = &request.winner {
            resolution["winner"] = json!(winner);
        }
        if let Some(reason) = &request.reason {
            resolution["reason"] = json!(reason);
        }

        // The log names what was decided by whom, never what the memories
        // say, so it holds at every level.
        let mut detail = json!({
            "key_a": request.key_a,
            "key_b": request.key_b,
            "outcome": request.outcome.as_str(),
            "resolved_by": by,
        });
        if let Some(winner) = &request.winner {
            detail["winner"] = json!(winner);
        }
        if request.retire != Retire::No {
            detail["retire"] = json!(request.retire.as_str());
        }
        let _audit = audit_op(
            Op::new("contradiction_resolve")
                .at(AuditLevel::Lifecycle)
                .reason(request.reason.as_deref())
                .detail(detail),
        );

        let supersedes = request.outcome == Outcome::Supersedes;
        let mut writes: Vec<(String, Fields)> = Vec::new();
        for (key, other, row) in [
            (&request.key_a, &request.key_b, &row_a),
            (&request.key_b, &request.key_a, &row_b),
        ] {
            let mut links = parse_links(row.get("contradictions"));
            put_resolution(&mut links, other, resolution.clone(), &now);
            let mut updates = Fields::from([
                ("contradictions".to_owned(), py_json(&Value::Array(links))),
                // A newer row is what lets a restore bring this across.
                ("updated_at".to_owned(), now.clone()),
            ]);
            let wins = supersedes && request.winner.as_deref() == Some(key.as_str());
            let loses = supersedes && request.winner.as_deref() != Some(key.as_str());
            supersession_updates(row, other, wins, loses, &mut updates);
            writes.push((key.clone(), updates));
        }
        self.store.set_fields_each(&writes)?;
        self.store
            .kv_delete(&pair_key(&request.key_a, &request.key_b))?;

        let (retired, skipped) = self.retire(request)?;
        info!(
            key_a = %request.key_a,
            key_b = %request.key_b,
            outcome = request.outcome.as_str(),
            by,
            "resolved contradiction"
        );
        let mut result = Map::new();
        result.insert("status".into(), "resolved".into());
        result.insert("key_a".into(), request.key_a.clone().into());
        result.insert("key_b".into(), request.key_b.clone().into());
        result.insert("outcome".into(), request.outcome.as_str().into());
        if let Some(winner) = &request.winner {
            result.insert("winner".into(), winner.clone().into());
        }
        if let Some(reason) = &request.reason {
            result.insert("reason".into(), reason.clone().into());
        }
        result.insert("resolved_by".into(), by.into());
        if !retired.is_empty() {
            result.insert("retired".into(), retired.into());
        }
        if !skipped.is_empty() {
            result.insert("retire_skipped".into(), skipped.into());
        }
        Ok(Value::Object(result))
    }

    /// Deprioritise or archive what the resolution found wanting, through the
    /// ordinary transition so derived facts follow and the reason is kept. A
    /// memory already past that state is reported, not an error.
    fn retire(&self, request: &ResolveRequest) -> Result<(Vec<Value>, Vec<Value>)> {
        let (mut retired, mut skipped) = (Vec::new(), Vec::new());
        let Some(state) = request.retire.state() else {
            return Ok((retired, skipped));
        };
        let targets: Vec<(&str, String)> = match request.outcome {
            Outcome::Supersedes => request
                .loser()
                .into_iter()
                .zip([format!(
                    "superseded by {}",
                    request.winner.as_deref().unwrap_or("")
                )])
                .collect(),
            Outcome::BothWrong => {
                let why = request.reason.as_deref().map_or_else(
                    || "contradiction: both wrong".to_owned(),
                    |r| format!("both wrong: {r}"),
                );
                vec![
                    (request.key_a.as_str(), why.clone()),
                    (request.key_b.as_str(), why),
                ]
            }
            _ => Vec::new(),
        };
        for (key, reason) in targets {
            match self.transition(key, state, Some(&reason)) {
                Ok(done) => retired.push(Value::Object(done)),
                Err(EngineError::Invalid(why)) => {
                    skipped.push(json!({"key": key, "reason": why}));
                }
                Err(e) => return Err(e),
            }
        }
        Ok((retired, skipped))
    }

    /// Keep a proposal for a person to accept or discard, replacing any
    /// earlier one for the pair. Nothing about the memories changes.
    pub fn propose_resolution(&self, request: &ResolveRequest, by: &str) -> Result<Value> {
        let review = self.review_resolution(request)?;
        let key = pair_key(&request.key_a, &request.key_b);
        let fields = Fields::from([
            ("key_a".to_owned(), request.key_a.clone()),
            ("key_b".to_owned(), request.key_b.clone()),
            ("outcome".to_owned(), request.outcome.as_str().to_owned()),
            (
                "winner".to_owned(),
                request.winner.clone().unwrap_or_default(),
            ),
            (
                "reason".to_owned(),
                request.reason.clone().unwrap_or_default(),
            ),
            ("retire".to_owned(), request.retire.as_str().to_owned()),
            ("proposed_by".to_owned(), by.to_owned()),
            ("proposed_at".to_owned(), now_str()),
        ]);
        // A hash merges, so the old proposal goes first or its fields could
        // outlive the ones that replace them.
        self.store.kv_delete(&key)?;
        self.store.hash_set(&key, &fields)?;
        self.store.expire(&key, PENDING_TTL)?;
        let mut result = Map::new();
        result.insert("status".into(), "awaiting_approval".into());
        result.insert("key_a".into(), review.request.key_a.into());
        result.insert("key_b".into(), review.request.key_b.into());
        result.insert("outcome".into(), request.outcome.as_str().into());
        if let Some(winner) = &request.winner {
            result.insert("winner".into(), winner.clone().into());
        }
        Ok(Value::Object(result))
    }

    /// Proposals waiting for a person, oldest first, with both memories'
    /// content. One whose memories have gone, or whose pair no longer fits
    /// its outcome, is left out.
    pub fn pending_resolutions(&self) -> Result<Vec<Value>> {
        let mut pending = Vec::new();
        for key in self.store.scan_prefix(PENDING_PREFIX)? {
            let Some(stored) = self.store.hash_get_all(&key)? else {
                continue;
            };
            let Some(request) = request_from(&stored) else {
                continue;
            };
            let Ok(review) = self.review_resolution(&request) else {
                continue;
            };
            pending.push(json!({
                "key_a": request.key_a,
                "key_b": request.key_b,
                "content_a": take_chars(&review.content_a, 150),
                "content_b": take_chars(&review.content_b, 150),
                "outcome": request.outcome.as_str(),
                "winner": request.winner.unwrap_or_default(),
                "reason": request.reason.unwrap_or_default(),
                "retire": request.retire.as_str(),
                "proposed_by": stored.get("proposed_by").cloned().unwrap_or_default(),
                "proposed_at": stored.get("proposed_at").cloned().unwrap_or_default(),
            }));
        }
        pending.sort_by(|a, b| {
            let at = |v: &Value| {
                v["proposed_at"]
                    .as_str()
                    .and_then(|t| t.parse::<f64>().ok())
                    .unwrap_or(0.0)
            };
            at(a).total_cmp(&at(b))
        });
        Ok(pending)
    }

    /// Accept the pending proposal for a pair: it is written exactly as it
    /// was proposed. `None` when there is none (it expired, or was already
    /// dealt with).
    pub fn accept_pending_resolution(
        &self,
        key_a: &str,
        key_b: &str,
        by: &str,
    ) -> Result<Option<Value>> {
        let Some(stored) = self.store.hash_get_all(&pair_key(key_a, key_b))? else {
            return Ok(None);
        };
        let Some(request) = request_from(&stored) else {
            return Ok(None);
        };
        self.resolve_contradiction(&request, by).map(Some)
    }

    /// Throw a pending proposal away. True if there was one.
    pub fn discard_pending_resolution(&self, key_a: &str, key_b: &str) -> Result<bool> {
        Ok(self.store.kv_delete(&pair_key(key_a, key_b))?)
    }

    /// Every pair still in dispute, once, with both sides' content. A pair
    /// is out of the list once either row carries a settled link to the
    /// other.
    pub fn open_contradictions(&self) -> Result<Vec<Value>> {
        let keys = self.store.scan_prefix("mem:episodic:")?;
        let rows = self
            .store
            .get_fields_multi(&keys, &["content", "contradictions"])?;
        let sorted = |a: &str, b: &str| {
            let mut pair = [a.to_owned(), b.to_owned()];
            pair.sort();
            pair
        };
        let link_key = |l: &Value| {
            l.get("key")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        let mut settled = std::collections::BTreeSet::new();
        for (key, row) in keys.iter().zip(&rows) {
            for link in parse_links(row.as_ref().and_then(|r| r.get("contradictions"))) {
                if link_is_resolved(&link) {
                    settled.insert(sorted(key, &link_key(&link)));
                }
            }
        }

        // Each pair, by its sorted keys, to where it sits in `raw_pairs`.
        let mut seen: std::collections::BTreeMap<[String; 2], usize> = Default::default();
        let mut others: std::collections::BTreeSet<String> = Default::default();
        let mut raw_pairs: Vec<(String, String, Map<String, Value>)> = Vec::new();
        for (key, row) in keys.iter().zip(rows) {
            let Some(row) = row else { continue };
            for entry in parse_links(row.get("contradictions")) {
                let Some(entry) = entry.as_object() else {
                    continue;
                };
                let other = link_key(&Value::Object(entry.clone()));
                let pair = sorted(key, &other);
                if settled.contains(&pair) {
                    continue;
                }
                if let Some(&at) = seen.get(&pair) {
                    // The other side's record of a pair already listed. Only
                    // one side may carry the explanation (a 6.x import, a
                    // link made from one side), and which side comes first
                    // is down to key order, so take what the first sighting
                    // lacked.
                    let listed = &mut raw_pairs[at].2;
                    for field in ["explanation", "similarity"] {
                        if is_blank(listed.get(field))
                            && let Some(value) = entry.get(field).filter(|v| !is_blank(Some(v)))
                        {
                            listed.insert(field.to_owned(), value.clone());
                        }
                    }
                    continue;
                }
                seen.insert(pair, raw_pairs.len());
                if !other.is_empty() {
                    others.insert(other);
                }
                raw_pairs.push((
                    key.clone(),
                    row.get("content").cloned().unwrap_or_default(),
                    entry.clone(),
                ));
            }
        }
        // One batched read for every other side.
        let other_keys: Vec<String> = others.into_iter().collect();
        let other_content: std::collections::HashMap<String, String> = other_keys
            .iter()
            .cloned()
            .zip(self.store.get_fields_multi(&other_keys, &["content"])?)
            .map(|(key, row)| {
                (
                    key,
                    row.and_then(|r| r.get("content").cloned())
                        .unwrap_or_default(),
                )
            })
            .collect();
        Ok(raw_pairs
            .into_iter()
            .map(|(key, content_a, entry)| {
                let other = entry.get("key").and_then(Value::as_str).unwrap_or("");
                let content_b = other_content
                    .get(other)
                    .filter(|c| !c.is_empty())
                    .cloned()
                    .unwrap_or_else(|| {
                        entry
                            .get("content")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned()
                    });
                json!({
                    "key_a": key,
                    "content_a": take_chars(&content_a, 150),
                    "key_b": other,
                    "content_b": take_chars(&content_b, 150),
                    "explanation": entry.get("explanation").cloned().unwrap_or_else(|| json!("")),
                    "similarity": entry.get("similarity").cloned().unwrap_or(Value::Null),
                })
            })
            .collect())
    }

    /// A memory is about to be deleted: every memory that was linked to it
    /// stops pointing at nothing. An unsettled link is settled as
    /// `other_forgotten`, a settled one is left as it was, and any
    /// supersession naming the deleted memory is cleared.
    pub(crate) fn settle_links_to_deleted(&self, key: &str, data: &Fields) -> Result<()> {
        let links = parse_links(data.get("contradictions"));
        let mut writes: Vec<(String, Fields)> = Vec::new();
        let now = now_str();
        for link in &links {
            let Some(other) = link.get("key").and_then(Value::as_str) else {
                continue;
            };
            let Some(row) = self.store.get(other)?.filter(|d| !d.is_empty()) else {
                continue;
            };
            let mut survivor = parse_links(row.get("contradictions"));
            let mut updates = Fields::new();
            let open = survivor
                .iter()
                .any(|l| l.get("key").and_then(Value::as_str) == Some(key) && !link_is_resolved(l));
            if open {
                let resolution = json!({
                    "outcome": "other_forgotten",
                    "reason": "the other memory was forgotten",
                    "resolved_at": now,
                    "resolved_by": "system",
                });
                put_resolution(&mut survivor, key, resolution, &now);
                updates.insert(
                    "contradictions".to_owned(),
                    py_json(&Value::Array(survivor)),
                );
            }
            supersession_updates(&row, key, false, false, &mut updates);
            if !updates.is_empty() {
                updates.insert("updated_at".to_owned(), now.clone());
                writes.push((other.to_owned(), updates));
            }
        }
        if !writes.is_empty() {
            self.store.set_fields_each(&writes)?;
        }
        Ok(())
    }
}

/// Missing, null or an empty string.
fn is_blank(value: Option<&Value>) -> bool {
    value.is_none_or(|v| v.is_null() || v.as_str() == Some(""))
}

fn request_from(stored: &Fields) -> Option<ResolveRequest> {
    let get = |name: &str| stored.get(name).map(String::as_str);
    ResolveRequest::new(
        get("key_a")?,
        get("key_b")?,
        get("outcome")?,
        get("winner"),
        get("reason"),
        Retire::parse(get("retire").unwrap_or("no")).ok()?,
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes_and_retire_modes_parse() {
        assert_eq!(Outcome::parse("both_valid").unwrap(), Outcome::BothValid);
        assert!(Outcome::parse("won").is_err());
        assert_eq!(Retire::parse("true").unwrap(), Retire::Deprioritise);
        assert_eq!(Retire::parse("archive").unwrap(), Retire::Archive);
        assert_eq!(Retire::parse("false").unwrap(), Retire::No);
        assert!(Retire::parse("burn").is_err());
    }

    #[test]
    fn only_a_settled_link_counts_as_resolved() {
        assert!(!link_is_resolved(&json!({"key": "k"})));
        assert!(link_is_resolved(
            &json!({"resolution": {"outcome": "both_valid"}})
        ));
        assert!(!link_is_resolved(
            &json!({"resolution": {"outcome": "reopened"}})
        ));
    }

    #[test]
    fn the_pending_key_is_the_same_either_way_round() {
        assert_eq!(
            pair_key("mem:episodic:b", "mem:episodic:a"),
            pair_key("mem:episodic:a", "mem:episodic:b")
        );
    }

    #[test]
    fn a_replaced_resolution_goes_into_the_history() {
        let mut links = vec![json!({"key": "x", "resolution": {"outcome": "both_valid"}})];
        put_resolution(&mut links, "x", json!({"outcome": "reopened"}), "1");
        assert_eq!(
            links[0]["resolution"]["history"][0]["outcome"],
            "both_valid"
        );
        put_resolution(&mut links, "x", json!({"outcome": "supersedes"}), "2");
        assert_eq!(
            links[0]["resolution"]["history"].as_array().unwrap().len(),
            2
        );
    }
}
