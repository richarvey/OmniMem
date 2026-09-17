//! `omnimem hook`: stop a known dead end before the agent runs it.
//!
//! Recall only helps an agent that asks. Measured on the dead-end benchmark,
//! agents with the server attached asked nothing at all: across ten runs they
//! made zero memory calls and went straight to the crate the project had
//! already abandoned. What changed the outcome was catching the proposal at
//! the point of action.
//!
//! Claude Code calls a PreToolUse hook with one JSON object on stdin and reads
//! one on stdout. Returning `permissionDecision: "deny"` stops the call and
//! hands the reason to the model, which is where the memory goes: what was
//! abandoned, why, and what worked instead. A refusal that says only "not
//! that" leaves the agent to rediscover the answer, which is the work the
//! graveyard exists to save.
//!
//! Two rules keep this from making stale memory unfalsifiable:
//!
//! * Only tools that change something are guarded. Read, Glob and Grep are
//!   never blocked, because reading is how an agent checks whether a warning
//!   still holds, and a Bash command is guarded only when it mutates: listing
//!   `vendor/kestrel-rs` is investigation, `cargo add kestrel-rs` is a proposal.
//! * It fails open. Any error, any unreadable store, anything unexpected, and
//!   the call proceeds. A memory system that blocks work when it is unwell is
//!   worse than one that forgets.

use std::io::Read;
use std::path::Path;

use anyhow::{Result, anyhow};
use omnimem_core::{EmbeddingError, TextEmbedder};
use omnimem_engine::{Engine, EngineConfig};
use omnimem_store::Store;
use serde_json::{Value, json};

/// Tools that put something in place. Read/Glob/Grep are deliberately absent.
const GUARDED: [&str; 5] = ["Edit", "MultiEdit", "Write", "NotebookEdit", "Bash"];

/// Shell fragments that mean a command changes something rather than looks.
const MUTATING: [&str; 11] = [
    "cargo add",
    "cargo install",
    "sed -i",
    "tee ",
    ">",
    "mv ",
    "cp ",
    "patch",
    "git apply",
    "perl -i",
    "npm install",
];

/// Longest proposal we search on. Whole-file writes are common and the
/// graveyard scan is keyword-based, so more than this buys nothing.
const MAX_QUERY_CHARS: usize = 4000;

/// Never embeds anything. The graveyard scan is keyword-based, and a hook runs
/// on every tool call, so loading the real model here would cost about a
/// second each time for a vector nothing reads.
struct NoEmbedder;

impl TextEmbedder for NoEmbedder {
    fn dimension(&self) -> usize {
        0
    }

    fn embed_texts(&self, _texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        Err("the hook does not embed; it only scans the graveyard".into())
    }
}

pub(crate) fn bash_mutates(command: &str) -> bool {
    MUTATING.iter().any(|marker| command.contains(marker))
}

/// What this call would put in place, flattened into one searchable string.
///
/// `old_string` is deliberately not read: taking a dead end *out* of a file is
/// the opposite of proposing it, and reading it would block the very fix the
/// warning asks for.
pub(crate) fn proposal_text(tool: &str, input: &Value) -> String {
    let field = |key: &str| input.get(key).and_then(Value::as_str).unwrap_or_default();
    let text = match tool {
        "Edit" => field("new_string").to_owned(),
        "Write" => field("content").to_owned(),
        "NotebookEdit" => field("new_source").to_owned(),
        "Bash" => {
            let command = field("command");
            if bash_mutates(command) {
                command.to_owned()
            } else {
                String::new()
            }
        }
        "MultiEdit" => input
            .get("edits")
            .and_then(Value::as_array)
            .map(|edits| {
                edits
                    .iter()
                    .filter_map(|e| e.get("new_string").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default(),
        _ => String::new(),
    };
    text.chars().take(MAX_QUERY_CHARS).collect()
}

/// The refusal an agent reads. Carries the way out, not just the wall.
pub(crate) fn deny_reason(warnings: &[String]) -> String {
    let body = warnings
        .iter()
        .map(|w| format!("- {w}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Stopped by project memory: this approach was already tried on this project \
         and abandoned.\n\n{body}\n\nThis is recorded experience from earlier work on \
         this codebase, not a guess. Do not spend turns re-establishing that it fails. \
         Take the approach that worked instead, and only revisit the abandoned one if \
         you have a specific reason to believe the situation has changed."
    )
}

fn decision(warnings: &[String]) -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": deny_reason(warnings),
        }
    })
}

/// The warnings for one hook payload, or none if the call should proceed.
fn warnings_for(db: &Path, payload: &Value) -> Result<Vec<String>> {
    let tool = payload
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !GUARDED.contains(&tool) {
        return Ok(Vec::new());
    }
    let empty = json!({});
    let input = payload.get("tool_input").unwrap_or(&empty);
    let proposal = proposal_text(tool, input);
    if proposal.trim().is_empty() {
        return Ok(Vec::new());
    }
    // Opened read-only in spirit: no migrations are run, because the server
    // owns the schema and a hook must not race it. SQLite is in WAL mode, so
    // reading alongside a running server is safe.
    let store = std::sync::Arc::new(Store::open(db)?);
    let config = EngineConfig::from_env(db.parent().unwrap_or(Path::new(".")).join("backups"));
    let engine = Engine::new(store, std::sync::Arc::new(NoEmbedder), config);
    engine
        .abandoned_warnings(&proposal)
        .map_err(|error| anyhow!("{error}"))
}

/// Read one hook payload, decide, and print. Never fails the caller: on any
/// error it stays silent, which Claude Code reads as "carry on".
pub(crate) fn run(db: &Path) -> Result<()> {
    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        return Ok(());
    }
    let Ok(payload) = serde_json::from_str::<Value>(&raw) else {
        return Ok(());
    };
    match warnings_for(db, &payload) {
        Ok(warnings) if !warnings.is_empty() => {
            println!("{}", decision(&warnings));
        }
        Ok(_) => {}
        Err(error) => {
            // To stderr, where it reaches the hook log without being read as a
            // decision. The tool call proceeds.
            eprintln!("omnimem hook: {error}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reading_is_never_a_proposal() {
        // The tools that only look are not guarded at all, and a Bash command
        // that inspects a dead end must stay allowed: blocking investigation
        // is how stale memory becomes unfalsifiable.
        for tool in ["Read", "Glob", "Grep"] {
            assert!(!GUARDED.contains(&tool), "{tool} should not be guarded");
        }
        for command in [
            "ls -la vendor/kestrel-rs/",
            "cargo test --offline",
            "grep -rn install_global vendor/",
        ] {
            assert!(
                proposal_text("Bash", &json!({"command": command})).is_empty(),
                "{command} should read as investigation"
            );
        }
    }

    #[test]
    fn acting_is_a_proposal() {
        assert_eq!(
            proposal_text("Bash", &json!({"command": "cargo add kestrel-rs"})),
            "cargo add kestrel-rs"
        );
        assert_eq!(
            proposal_text("Edit", &json!({"new_string": "kestrel_rs::Governor"})),
            "kestrel_rs::Governor"
        );
        assert_eq!(
            proposal_text(
                "MultiEdit",
                &json!({"edits": [{"new_string": "a"}, {"new_string": "b"}]})
            ),
            "a\nb"
        );
    }

    #[test]
    fn removing_a_dead_end_is_not_proposing_it() {
        // old_string is what is being taken out. Searching it would block the
        // very edit that removes the abandoned approach.
        let input = json!({"old_string": "kestrel_rs::Governor", "new_string": "pellham::Limiter"});
        assert_eq!(proposal_text("Edit", &input), "pellham::Limiter");
    }

    #[test]
    fn the_refusal_carries_the_way_out() {
        let reason = deny_reason(&["Abandoned approach: kestrel-rs — global handle. \
                                    What worked instead: pellham"
            .to_owned()]);
        assert!(reason.contains("What worked instead: pellham"), "{reason}");
        assert!(reason.contains("specific reason"), "{reason}");
    }

    #[test]
    fn a_whole_file_write_is_bounded() {
        let huge = "x".repeat(MAX_QUERY_CHARS * 2);
        let text = proposal_text("Write", &json!({"content": huge}));
        assert_eq!(text.chars().count(), MAX_QUERY_CHARS);
    }
}
