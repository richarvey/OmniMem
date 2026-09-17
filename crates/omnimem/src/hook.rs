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
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use clap::ValueEnum;
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

/// Longest session record written at close. Long enough for a closing summary,
/// short enough that a session log cannot crowd out deliberate memories.
const MAX_RECORD_CHARS: usize = 2000;

/// Longest briefing injected at session start. A briefing is paid for in every
/// message of the session that follows, so it stays a summary: the graveyard
/// and the current state, not the whole store.
const MAX_BRIEFING_CHARS: usize = 6000;

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

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum Event {
    /// A tool call is about to run: deny it if it repeats a known dead end.
    PreToolUse,
    /// A session is starting: hand it what this project already knows.
    SessionStart,
    /// A session has finished: record what it did, if it did anything.
    SessionEnd,
}

/// The project a hook is speaking for.
///
/// `--project`, then `OMNIMEM_PROJECT`, then the directory name, which is the
/// same default the server's instructions tell an agent to assume, so a hook
/// and an agent name the same project without being told twice.
fn project_name(explicit: Option<&str>, payload: &Value) -> Option<String> {
    if let Some(name) = explicit.map(str::trim).filter(|n| !n.is_empty()) {
        return Some(name.to_owned());
    }
    // Through omnimem_core::env, not std::env::var, so the desktop app's
    // omnimem.env overlay reaches a hook the same way it reaches the server.
    if let Some(name) = omnimem_core::env::var("OMNIMEM_PROJECT")
        && !name.trim().is_empty()
    {
        return Some(name.trim().to_owned());
    }
    payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()))
        .filter(|n| !n.is_empty())
}

fn bullet(out: &mut String, text: &str) {
    let text = text.trim();
    if !text.is_empty() {
        out.push_str("- ");
        out.push_str(text);
        out.push('\n');
    }
}

/// The briefing as an agent should read it.
///
/// Rendered rather than dumped as JSON: this text is prepended to a session and
/// then carried in its context, so it is written to be read once and acted on.
/// The graveyard leads, because it is the part that changes what the agent does
/// next.
pub(crate) fn render_briefing(project: &str, briefing: &Value) -> String {
    let mut out = format!("Project memory for `{project}` (OmniMem)\n\n");
    let get = |key: &str| briefing.get(key);

    if let Some(state) = get("project_context")
        .and_then(|c| c.get("current_state"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        out.push_str("Where this project is:\n");
        out.push_str(state);
        out.push_str("\n\n");
    }

    let graveyard = get("experience_summary")
        .and_then(|e| e.get("graveyard"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if !graveyard.is_empty() {
        out.push_str(
            "Already tried on this project and abandoned. Do not spend turns \
             rediscovering these:\n",
        );
        for entry in graveyard.iter().take(10) {
            let name = entry.get("name").and_then(Value::as_str).unwrap_or("?");
            let reason = entry
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .trim_end_matches('.');
            let effort = entry.get("effort_score").and_then(Value::as_i64);
            let weight = match effort {
                Some(4..=i64::MAX) => " (abandoned after significant effort)",
                Some(3) => " (several attempts)",
                _ => "",
            };
            bullet(
                &mut out,
                &if reason.is_empty() {
                    format!("{name}{weight}")
                } else {
                    format!("{name}: {reason}{weight}")
                },
            );
        }
        out.push('\n');
    }

    let breakthroughs = get("experience_summary")
        .and_then(|e| e.get("top_3_breakthroughs"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if !breakthroughs.is_empty() {
        out.push_str("What worked, the hard way:\n");
        for entry in breakthroughs {
            if let Some(text) = entry.get("breakthrough").and_then(Value::as_str) {
                bullet(&mut out, text);
            }
        }
        out.push('\n');
    }

    let warnings = get("contradiction_warnings")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if !warnings.is_empty() {
        out.push_str("Memories that contradict each other, so treat both as unsettled:\n");
        for entry in warnings.iter().take(5) {
            if let Some(text) = entry
                .get("summary")
                .or_else(|| entry.get("content"))
                .and_then(Value::as_str)
            {
                bullet(&mut out, text);
            }
        }
        out.push('\n');
    }

    let stale = get("stale_memories")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    if stale > 0 {
        out.push_str(&format!(
            "{stale} memories here have not been recalled in a long time; \
             `recall` reaches them if this turns out to be old ground.\n\n"
        ));
    }

    out.push_str(
        "This is recorded experience from earlier sessions, not a guess. It can be \
         out of date: check before relying on anything load-bearing, and record what \
         you learn with remember() and record_experience() so the next session starts \
         further along.",
    );

    if out.chars().count() > MAX_BRIEFING_CHARS {
        out = out.chars().take(MAX_BRIEFING_CHARS).collect::<String>();
        out.push_str("\n[briefing truncated]");
    }
    out
}

/// True when a briefing has something worth paying context for.
///
/// A new project has an empty store, and injecting a heading with nothing under
/// it into every session teaches an agent to skim past the whole thing.
fn briefing_is_worth_showing(briefing: &Value) -> bool {
    let non_empty = |v: Option<&Value>| {
        v.and_then(Value::as_array).is_some_and(|a| !a.is_empty())
            || v.and_then(Value::as_str)
                .is_some_and(|s| !s.trim().is_empty())
    };
    let experience = briefing.get("experience_summary");
    non_empty(experience.and_then(|e| e.get("graveyard")))
        || non_empty(experience.and_then(|e| e.get("top_3_breakthroughs")))
        || non_empty(
            briefing
                .get("project_context")
                .and_then(|c| c.get("current_state")),
        )
        || non_empty(briefing.get("contradiction_warnings"))
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

/// Things that must never reach the store.
///
/// A transcript is exactly where credentials show up: a command that printed a
/// token, a config dump, a pasted key. Recording sessions automatically means
/// recording those too unless they are taken out first, and a memory store is
/// the worst place for one, because recall will helpfully put it back into a
/// later session's context.
///
/// Deliberately blunt. A false positive costs a redacted line in a session log;
/// a false negative puts a live credential in the store forever.
fn redact(text: &str) -> String {
    static PATTERNS: &[&str] = [
        r"(?i)\b(sk|pk|rk)-[A-Za-z0-9_-]{16,}",
        r"\bgh[pousr]_[A-Za-z0-9]{16,}",
        r"\bAKIA[0-9A-Z]{16}\b",
        r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{12,}",
        r"(?i)\b(api[_-]?key|secret|password|passwd|token|authorization)\b\s*[:=]\s*\S+",
        r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----",
        r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}",
    ]
    .as_slice();
    let mut out = text.to_owned();
    for pattern in PATTERNS {
        if let Ok(re) = regex::Regex::new(pattern) {
            out = re.replace_all(&out, "[redacted]").into_owned();
        }
    }
    out
}

/// What a session did, read back from its transcript.
#[derive(Default)]
struct SessionWork {
    first_ask: String,
    closing_summary: String,
    files: Vec<String>,
    edits: usize,
}

/// Read the transcript defensively.
///
/// Claude Code does not promise the file is complete or flushed when SessionEnd
/// fires, so every line is attempted and every failure skipped: a half-written
/// last line must not cost the whole record.
fn read_transcript(path: &Path) -> SessionWork {
    let mut work = SessionWork::default();
    let Ok(raw) = std::fs::read_to_string(path) else {
        return work;
    };
    for line in raw.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let role = event
            .get("message")
            .and_then(|m| m.get("role"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let content = event
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        for block in content {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    let text = block
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .trim();
                    if text.is_empty() {
                        continue;
                    }
                    if role == "user" && work.first_ask.is_empty() {
                        work.first_ask = text.to_owned();
                    } else if role == "assistant" {
                        // The last one standing: an agent's closing message is
                        // usually its own account of what it did, which beats
                        // anything this hook could assemble mechanically.
                        work.closing_summary = text.to_owned();
                    }
                }
                Some("tool_use") => {
                    let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                    if matches!(name, "Edit" | "MultiEdit" | "Write" | "NotebookEdit") {
                        work.edits += 1;
                        if let Some(file) = block
                            .get("input")
                            .and_then(|i| i.get("file_path"))
                            .and_then(Value::as_str)
                        {
                            let file = file.rsplit('/').next().unwrap_or(file).to_owned();
                            if !work.files.contains(&file) {
                                work.files.push(file);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    work
}

/// The memory a finished session leaves behind, or none when it left nothing.
///
/// Gated on having changed something. A session that only read, or that was
/// abandoned after two messages, has nothing worth a permanent record, and
/// writing one anyway is how a store fills with noise that outranks the
/// memories somebody meant to keep.
fn session_record(work: &SessionWork) -> Option<String> {
    if work.edits == 0 || work.closing_summary.trim().is_empty() {
        return None;
    }
    let mut text = String::new();
    if !work.first_ask.is_empty() {
        let ask: String = work.first_ask.chars().take(200).collect();
        text.push_str(&format!("Asked to: {}\n\n", ask.trim()));
    }
    text.push_str(work.closing_summary.trim());
    if !work.files.is_empty() {
        let mut files = work.files.clone();
        files.sort();
        text.push_str(&format!("\n\nFiles changed: {}", files.join(", ")));
    }
    let text = redact(&text);
    let text: String = text.chars().take(MAX_RECORD_CHARS).collect();
    Some(text)
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
    open_engine(db)?
        .abandoned_warnings(&proposal)
        .map_err(|error| anyhow!("{error}"))
}

/// Opened read-only in spirit: no migrations are run, because the server owns
/// the schema and a hook must not race it. SQLite is in WAL mode, so reading
/// alongside a running server is safe.
fn open_engine(db: &Path) -> Result<Engine> {
    let store = std::sync::Arc::new(Store::open(db)?);
    let config = EngineConfig::from_env(db.parent().unwrap_or(Path::new(".")).join("backups"));
    Ok(Engine::new(store, std::sync::Arc::new(NoEmbedder), config))
}

/// Write what this session did into the store, once.
///
/// Unlike the other two events this one writes, so it needs the real embedder:
/// an unembedded memory is not searchable, and a session log nobody can recall
/// is just disk. That costs about a second to load, which is why the hook entry
/// in settings.json raises SessionEnd's default 1.5s budget.
fn record_session(db: &Path, project: &str, payload: &Value) -> Result<()> {
    let Some(path) = payload.get("transcript_path").and_then(Value::as_str) else {
        return Ok(());
    };
    let work = read_transcript(Path::new(path));
    let Some(content) = session_record(&work) else {
        return Ok(());
    };

    // SessionEnd can fire more than once for one session (a /clear followed by
    // an exit), and a duplicate record is worse than none: it doubles the
    // session's weight in every later recall.
    let store = std::sync::Arc::new(Store::open(db)?);
    let marker = format!(
        "meta:session_log:{}",
        payload
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    );
    if store.get(&marker)?.is_some() {
        return Ok(());
    }

    let config = EngineConfig::from_env(db.parent().unwrap_or(Path::new(".")).join("backups"));
    let embedder = omnimem_app::load_embedder()?;
    let engine = Engine::new(store.clone(), std::sync::Arc::new(embedder), config);
    engine
        .remember(
            &content,
            Some(project),
            Some(&["session-log".to_owned()]),
            "episodic",
            false,
            None,
            None,
            // The agent's account of its own session: reasoning about what it
            // did, not something it retrieved or something a human vouched for.
            Some("concluded"),
        )
        .map_err(|error| anyhow!("{error}"))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    store.hash_set(
        &marker,
        &omnimem_store::Fields::from([("written_at".to_owned(), stamp.to_string())]),
    )?;
    Ok(())
}

/// The context a session should start with, or none when there is nothing
/// worth saying.
fn session_start_context(db: &Path, project: &str) -> Result<Option<String>> {
    let briefing = open_engine(db)?
        // Read-only: a hook must not advance the maintenance schedule or
        // propose skill drafts, and it must not embed. See session_briefing.
        .session_briefing(Some(project), false)
        .map_err(|error| anyhow!("{error}"))?;
    Ok(briefing_is_worth_showing(&briefing).then(|| render_briefing(project, &briefing)))
}

/// Read one hook payload, decide, and print. Never fails the caller: on any
/// error it stays silent, which Claude Code reads as "carry on".
pub(crate) fn run(db: &Path, event: Event, project: Option<&str>) -> Result<()> {
    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        return Ok(());
    }
    // An empty or malformed payload is not a reason to fail: SessionStart in
    // particular is worth answering from the working directory alone.
    let payload = serde_json::from_str::<Value>(&raw).unwrap_or_else(|_| json!({}));
    match event {
        Event::PreToolUse => match warnings_for(db, &payload) {
            Ok(warnings) if !warnings.is_empty() => println!("{}", decision(&warnings)),
            Ok(_) => {}
            // To stderr, where it reaches the hook log without being read as a
            // decision. The tool call proceeds.
            Err(error) => eprintln!("omnimem hook: {error}"),
        },
        Event::SessionEnd => {
            let Some(name) = project_name(project, &payload) else {
                return Ok(());
            };
            if let Err(error) = record_session(db, &name, &payload) {
                eprintln!("omnimem hook: {error}");
            }
        }
        Event::SessionStart => {
            let Some(name) = project_name(project, &payload) else {
                return Ok(());
            };
            match session_start_context(db, &name) {
                Ok(Some(context)) => println!(
                    "{}",
                    json!({
                        "hookSpecificOutput": {
                            "hookEventName": "SessionStart",
                            "additionalContext": context,
                        }
                    })
                ),
                Ok(None) => {}
                Err(error) => eprintln!("omnimem hook: {error}"),
            }
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
    fn credentials_never_reach_the_store() {
        // A transcript is where secrets surface. Recall would put anything
        // stored here back into a later session's context, so these must not
        // survive the trip.
        for secret in [
            "export ANTHROPIC_API_KEY=sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAA",
            "token: ghp_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "Authorization: Bearer abcdef0123456789abcdef",
            "aws_access_key_id = AKIAIOSFODNN7EXAMPLE",
            "password: hunter2correcthorse",
            "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVP",
        ] {
            let cleaned = redact(secret);
            assert!(
                cleaned.contains("[redacted]"),
                "not redacted: {secret} -> {cleaned}"
            );
        }
        let ordinary = "Implemented make_limiter with pellham and the suite passed.";
        assert_eq!(redact(ordinary), ordinary, "ordinary prose must survive");
    }

    #[test]
    fn a_session_that_changed_nothing_is_not_recorded() {
        // Reading around, or abandoning after two messages, leaves nothing
        // worth a permanent record. Writing one anyway is how a store fills
        // with noise that outranks memories somebody meant to keep.
        let read_only = SessionWork {
            closing_summary: "I looked at the config and it seems fine.".into(),
            ..Default::default()
        };
        assert!(session_record(&read_only).is_none());

        let silent = SessionWork {
            edits: 3,
            ..Default::default()
        };
        assert!(session_record(&silent).is_none());
    }

    #[test]
    fn a_session_that_did_work_is_recorded_with_its_files() {
        let work = SessionWork {
            first_ask: "implement rate limiting".into(),
            closing_summary: "Used pellham; the isolation tests pass.".into(),
            files: vec!["shard.rs".into(), "Cargo.toml".into()],
            edits: 2,
        };
        let record = session_record(&work).expect("a session that changed files is worth keeping");
        assert!(
            record.contains("Asked to: implement rate limiting"),
            "{record}"
        );
        assert!(record.contains("Used pellham"), "{record}");
        assert!(
            record.contains("Files changed: Cargo.toml, shard.rs"),
            "{record}"
        );
        assert!(record.chars().count() <= MAX_RECORD_CHARS);
    }

    #[test]
    fn a_briefing_with_nothing_in_it_is_not_shown() {
        // A new project has an empty store. Injecting a heading with nothing
        // under it into every session teaches an agent to skim past it.
        assert!(!briefing_is_worth_showing(&json!({})));
        assert!(!briefing_is_worth_showing(
            &json!({"experience_summary": {"graveyard": []}, "stale_memories": []})
        ));
        assert!(briefing_is_worth_showing(
            &json!({"experience_summary": {"graveyard": [{"name": "kestrel-rs"}]}})
        ));
    }

    #[test]
    fn the_briefing_leads_with_the_graveyard() {
        let briefing = json!({
            "project_context": {"current_state": "mid rollout"},
            "experience_summary": {
                "graveyard": [{"name": "kestrel-rs", "reason": "thread-local handle", "effort_score": 4}],
                "top_3_breakthroughs": [{"breakthrough": "pellham per shard"}]
            }
        });
        let text = render_briefing("harrier", &briefing);
        let graveyard = text.find("kestrel-rs").expect("graveyard shown");
        let breakthrough = text.find("pellham per shard").expect("breakthrough shown");
        assert!(graveyard < breakthrough, "the graveyard leads:\n{text}");
        assert!(
            text.contains("abandoned after significant effort"),
            "{text}"
        );
        assert!(
            text.contains("can be out of date"),
            "a briefing must invite checking:\n{text}"
        );
    }

    #[test]
    fn a_whole_file_write_is_bounded() {
        let huge = "x".repeat(MAX_QUERY_CHARS * 2);
        let text = proposal_text("Write", &json!({"content": huge}));
        assert_eq!(text.chars().count(), MAX_QUERY_CHARS);
    }
}
