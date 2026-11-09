//! Trust (#23): whether OmniMem believes a memory, beside `state`, which only
//! says whether it is visible.
//!
//! | Value | Meaning |
//! |---|---|
//! | `candidate` | OmniMem's own conclusion (an extracted fact or preference), not yet confirmed |
//! | `accepted` | Believed |
//! | `disputed` | In an unresolved contradiction the model confirmed |
//! | `superseded` | Replaced by a newer memory (`superseded_by` says which) |
//! | `rejected` | Judged wrong by a person |
//!
//! A record with no `trust` field has the one [`omnimem_store::trust_of`]
//! gives it, so nothing is rewritten to add the field. `trust_prior` holds
//! what a disputed, superseded or rejected memory goes back to.
//!
//! Who moves it: a confirmed contradiction disputes both sides and its
//! resolution moves them on; only a person confirms a candidate or rejects a
//! memory, through the settings window or the CLI ([`Engine::set_trust`]),
//! never an MCP call, the same rule as the skill gate.

use omnimem_core::Namespace;
use omnimem_store::{AuditLevel, Fields, MemoryFilter, Op, op, trust_of};
use serde_json::{Value, json};

use crate::resolution::link_is_resolved;
use crate::{Engine, EngineError, Result};

pub const CANDIDATE: &str = "candidate";
pub const ACCEPTED: &str = "accepted";
pub const DISPUTED: &str = "disputed";
pub const SUPERSEDED: &str = "superseded";
pub const REJECTED: &str = "rejected";
pub const TRUSTS: [&str; 5] = [CANDIDATE, ACCEPTED, DISPUTED, SUPERSEDED, REJECTED];

/// Whether an open link is evidence enough to dispute a memory: the model
/// confirmed it (`verified`), or a person reopened it. A heuristic link,
/// which says only that two memories share words and a negation, isn't.
pub(crate) fn link_disputes(link: &Value) -> bool {
    if link_is_resolved(link) {
        return false;
    }
    let reopened = link
        .get("resolution")
        .and_then(|r| r.get("outcome"))
        .and_then(Value::as_str)
        == Some("reopened");
    reopened || link.get("verified") == Some(&Value::Bool(true))
}

/// What a memory's trust returns to: what it was before (`trust_prior`), or
/// for none, its default (empty, which [`trust_of`] reads).
fn prior(row: &Fields) -> String {
    row.get("trust_prior").cloned().unwrap_or_default()
}

/// Updates that dispute `row`, if it is believed (accepted, or a candidate).
/// A memory already superseded or rejected stays as a person left it.
pub(crate) fn dispute(row: &Fields, updates: &mut Fields) {
    let now = trust_of(row);
    if now == ACCEPTED || now == CANDIDATE {
        updates.insert("trust".to_owned(), DISPUTED.to_owned());
        updates.insert("trust_prior".to_owned(), row_trust_field(row));
    }
}

/// The `trust` field as stored: what a later restore writes back, so a
/// memory with none keeps having none.
fn row_trust_field(row: &Fields) -> String {
    row.get("trust").cloned().unwrap_or_default()
}

/// Updates that end a dispute on `row`, unless one of its other links
/// (`links`, as they will be after this change) still disputes it.
pub(crate) fn undispute(row: &Fields, links: &[Value], updates: &mut Fields) {
    if trust_of(row) != DISPUTED || links.iter().any(link_disputes) {
        return;
    }
    updates.insert("trust".to_owned(), prior(row));
    updates.insert("trust_prior".to_owned(), String::new());
}

/// Set a trust a person decided (supersession, both wrong), remembering
/// what to go back to.
pub(crate) fn settle(row: &Fields, trust: &str, updates: &mut Fields) {
    let keep = if trust_of(row) == DISPUTED {
        prior(row)
    } else {
        row_trust_field(row)
    };
    updates.insert("trust".to_owned(), trust.to_owned());
    updates.insert("trust_prior".to_owned(), keep);
}

/// A person reopened the resolution that superseded or rejected `row`: it
/// is in dispute again, and `trust_prior` still says what it goes back to.
pub(crate) fn reopen(row: &Fields, updates: &mut Fields) {
    let now = trust_of(row);
    if now == SUPERSEDED || now == REJECTED {
        updates.insert("trust".to_owned(), DISPUTED.to_owned());
    } else {
        dispute(row, updates);
    }
}

/// The memory a winner was deleted out from under goes back to what it was.
pub(crate) fn unsupersede(row: &Fields, winner: &str, updates: &mut Fields) {
    if trust_of(row) == SUPERSEDED && row.get("superseded_by").map(String::as_str) == Some(winner) {
        updates.insert("trust".to_owned(), prior(row));
        updates.insert("trust_prior".to_owned(), String::new());
    }
}

/// What a person can do to a memory's trust.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustAction {
    /// A candidate is believed after all.
    Confirm,
    /// The memory is wrong.
    Reject,
    /// Undo a rejection.
    Restore,
}

impl TrustAction {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "confirm" | "accept" => Some(Self::Confirm),
            "reject" => Some(Self::Reject),
            "restore" | "unreject" => Some(Self::Restore),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Confirm => "confirm",
            Self::Reject => "reject",
            Self::Restore => "restore",
        }
    }
}

impl Engine {
    /// A person changes a memory's trust. The settings window and the CLI
    /// call this; no MCP tool does, so an agent can't confirm its own
    /// conclusions. Recorded in the audit log at `lifecycle` with the
    /// reason, and `updated_at` is left alone (the skill compiler reads a
    /// bump as "this source changed").
    pub fn set_trust(&self, key: &str, action: TrustAction, reason: Option<&str>) -> Result<Value> {
        let Some(row) = self.store.get(key)?.filter(|r| !r.is_empty()) else {
            return Err(EngineError::Invalid(format!("Memory not found: {key}")));
        };
        let was = trust_of(&row).to_owned();
        let mut updates = Fields::new();
        match action {
            TrustAction::Confirm if was == CANDIDATE => {
                updates.insert("trust".to_owned(), ACCEPTED.to_owned());
            }
            TrustAction::Confirm => {
                return Err(EngineError::Invalid(format!(
                    "{key} is {was}: only a candidate can be confirmed"
                )));
            }
            TrustAction::Reject if was == REJECTED => {
                return Err(EngineError::Invalid(format!("{key} is already rejected")));
            }
            TrustAction::Reject => settle(&row, REJECTED, &mut updates),
            TrustAction::Restore if was == REJECTED => {
                updates.insert("trust".to_owned(), prior(&row));
                updates.insert("trust_prior".to_owned(), String::new());
            }
            TrustAction::Restore => {
                return Err(EngineError::Invalid(format!(
                    "{key} is {was}: only a rejected memory can be restored"
                )));
            }
        }
        // A reject retires the memory and a restore un-retires it (#27).
        crate::validity::follow_trust(&row, &mut updates, &crate::pyfmt::now_str());
        {
            let _audit = op(Op::new("trust")
                .at(AuditLevel::Lifecycle)
                .reason(reason)
                .detail(json!({"trust_action": action.as_str()})));
            self.store.set_fields(key, &updates)?;
        }
        // A rejected memory's dead ends stop warning, and a restored one's
        // start again (#33).
        self.invalidate_abandoned_cache();
        self.cascade_interval(key)?;
        let now = self
            .store
            .get(key)?
            .map_or_else(|| was.clone(), |r| trust_of(&r).to_owned());
        Ok(json!({"key": key, "trust": now, "was": was, "action": action.as_str()}))
    }

    /// Confirm every candidate extracted from `source`, for the settings
    /// window's "confirm all from this memory". Returns how many moved.
    pub fn confirm_from_source(&self, source: &str, reason: Option<&str>) -> Result<usize> {
        let mut confirmed = 0;
        for (key, _) in self.unconfirmed_from(Some(source), usize::MAX)? {
            self.set_trust(&key, TrustAction::Confirm, reason)?;
            confirmed += 1;
        }
        Ok(confirmed)
    }

    /// Candidates, optionally only those extracted from `source`, newest
    /// first by key, with the fields a list needs.
    pub fn unconfirmed_from(
        &self,
        source: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, Fields)>> {
        let mut out = Vec::new();
        // Every namespace a fact can be filed in: project context gets
        // extracted facts too. Skills are compiled output and never are.
        for namespace in Namespace::ALL
            .into_iter()
            .filter(|ns| *ns != Namespace::Skill)
        {
            let filter = MemoryFilter {
                trusts: vec![CANDIDATE.to_owned()],
                states: vec!["active".to_owned(), "deprioritised".to_owned()],
                ..MemoryFilter::default()
            };
            for (key, row) in self.store.list_memories(
                namespace,
                &filter,
                &["content", "enriched_from", "project", "created_at", "state"],
            )? {
                if source.is_none_or(|s| row.get("enriched_from").map(String::as_str) == Some(s)) {
                    out.push((key, row));
                }
            }
        }
        out.sort_by(|a, b| b.0.cmp(&a.0));
        out.truncate(limit);
        Ok(out)
    }
}
