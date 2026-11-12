//! OmniMem in Kiro, AWS's agentic IDE and CLI: its MCP entry and its hooks.
//!
//! Kiro keeps its settings in `~/.kiro` on every platform: MCP servers under
//! `mcpServers` in `settings/mcp.json` (a project's own in its
//! `.kiro/settings/mcp.json`), shaped like Claude Desktop's but taking a
//! `url` and `headers` directly, and hooks as standalone files in `hooks/`
//! (a project's own in `.kiro/hooks/`), each a `{"version": "v1", "hooks":
//! [...]}` document. The CLI reads the global folder; the IDE reads a
//! workspace's, so the IDE needs `install-hooks --kiro --project`.
//!
//! A command hook gets the event as JSON on stdin (`hook_event_name`, `cwd`,
//! `tool_name`, `tool_input`). What it prints on success goes into the
//! agent's context, which is how the session-start briefing arrives, and a
//! `PreToolUse` hook that exits 2 blocks the call, its stderr going to the
//! model as the reason. So `omnimem hook --agent kiro` prints nothing for a
//! call it lets through.
//!
//! OmniMem's hooks are a file of their own, `omnimem.json`, so writing them
//! never touches anybody else's; the file as it was is copied beside it.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Value, json};

pub use crate::client_files::HooksState;
use crate::client_files::replace;
pub use crate::shell::hook_base;

/// The name OmniMem's MCP entry goes under.
pub const SERVER_NAME: &str = "omnimem";

/// The file OmniMem's hooks are in, in a hooks folder.
pub const HOOKS_FILE: &str = "omnimem.json";

/// One of OmniMem's hooks, as it goes in the hooks file.
pub struct Hook {
    pub name: &'static str,
    /// Kiro's trigger.
    pub trigger: &'static str,
    /// The `omnimem hook` event it answers.
    pub event: &'static str,
    /// Seconds.
    pub timeout: u64,
    pub description: &'static str,
}

/// The hooks. The guard has no matcher, so it sees every tool call: Kiro's
/// tool names differ between its IDE and its CLI versions, and `omnimem
/// hook` lets anything it doesn't guard through without opening the store,
/// so a read costs a process start and nothing more.
pub const HOOKS: [Hook; 2] = [
    Hook {
        name: "omnimem-briefing",
        trigger: "SessionStart",
        event: "session-start",
        timeout: 15,
        description: "OmniMem: what this project already knows, at the start of a session",
    },
    Hook {
        name: "omnimem-guard",
        trigger: "PreToolUse",
        event: "pre-tool-use",
        timeout: 15,
        description: "OmniMem: refuse a change that repeats an approach this project abandoned",
    },
];

/// Kiro's folder in the home folder.
pub fn home() -> Option<PathBuf> {
    std::env::home_dir().map(|home| home.join(".kiro"))
}

/// Kiro's user MCP settings.
pub fn config_path() -> Option<PathBuf> {
    home().map(|dir| dir.join("settings").join("mcp.json"))
}

/// Where the hooks go: Kiro's folder, or with `project`, that project's
/// `.kiro` folder.
pub fn hooks_path(project: Option<&Path>) -> Option<PathBuf> {
    let dir = match project {
        Some(project) => project.join(".kiro"),
        None => home()?,
    };
    Some(dir.join("hooks").join(HOOKS_FILE))
}

/// Whether Kiro is here: the folder it makes the first time it runs, its
/// CLI, or on macOS its app.
pub fn installed() -> bool {
    if home().is_some_and(|dir| dir.is_dir()) {
        return true;
    }
    let names: &[&str] = if cfg!(windows) {
        &["kiro-cli.exe", "kiro.exe", "kiro.cmd"]
    } else {
        &["kiro-cli", "kiro"]
    };
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::home_dir() {
        dirs.push(home.join(".local/bin"));
    }
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from));
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    if dirs
        .iter()
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .any(|candidate| candidate.is_file())
    {
        return true;
    }
    #[cfg(target_os = "macos")]
    {
        let mut apps = vec![PathBuf::from("/Applications/Kiro.app")];
        if let Some(home) = std::env::home_dir() {
            apps.push(home.join("Applications/Kiro.app"));
        }
        if apps.iter().any(|app| app.is_dir()) {
            return true;
        }
    }
    false
}

/// One of OmniMem's hooks' command, after `base`.
pub fn hook_command(base: &str, hook: &Hook) -> String {
    format!("{base} hook {} --agent kiro", hook.event)
}

/// The hooks file as it would be written.
pub fn hooks_document(base: &str) -> Value {
    let hooks: Vec<Value> = HOOKS
        .iter()
        .map(|hook| {
            json!({
                "name": hook.name,
                "description": hook.description,
                "trigger": hook.trigger,
                "action": {"type": "command", "command": hook_command(base, hook)},
                "timeout": hook.timeout,
                "enabled": true,
            })
        })
        .collect();
    json!({"version": "v1", "hooks": hooks})
}

/// What OmniMem's hooks file holds, against what would be written.
pub fn hooks_state(path: &Path, base: &str) -> HooksState {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return HooksState::Missing,
        Err(e) => return HooksState::Unreadable(format!("couldn't read {}: {e}", path.display())),
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(have) if have == hooks_document(base) => HooksState::Current,
        // OmniMem's own file, from another version or pointing at another
        // program, or edited: it's rewritten whole either way.
        _ => HooksState::Outdated,
    }
}

/// Write OmniMem's hooks file, with a copy of the old one beside it.
pub fn write_hooks(path: &Path, base: &str) -> Result<HooksState> {
    let before = hooks_state(path, base);
    if before != HooksState::Current {
        let text = serde_json::to_string_pretty(&hooks_document(base))? + "\n";
        replace(path, &text, false)?;
    }
    Ok(before)
}

/// Kiro's MCP entry for this server, as guides/kiro.md gives it: the URL
/// itself, and the token as a header when one is set.
pub fn mcp_entry(mcp_url: &str, token: Option<&str>) -> Value {
    let mut entry = json!({"type": "http", "url": mcp_url});
    if let Some(token) = token {
        entry["headers"] = json!({"Authorization": format!("Bearer {token}")});
    }
    entry
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("omnimem-kiro-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_hooks_file_is_written_whole_and_recognised() {
        let dir = scratch("hooks");
        let path = hooks_path(Some(&dir)).unwrap();
        assert_eq!(path, dir.join(".kiro/hooks/omnimem.json"));
        let base = "/opt/omnimem/bin/omnimem";
        assert_eq!(hooks_state(&path, base), HooksState::Missing);
        assert_eq!(write_hooks(&path, base).unwrap(), HooksState::Missing);
        assert_eq!(hooks_state(&path, base), HooksState::Current);

        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc["version"], "v1");
        assert_eq!(doc["hooks"][0]["trigger"], "SessionStart");
        assert_eq!(
            doc["hooks"][0]["action"]["command"],
            "/opt/omnimem/bin/omnimem hook session-start --agent kiro"
        );
        assert_eq!(doc["hooks"][1]["trigger"], "PreToolUse");
        assert!(doc["hooks"][1].get("matcher").is_none());

        // Another program, or a hand edit, is out of date and rewritten.
        assert_eq!(
            hooks_state(&path, "/elsewhere/omnimem"),
            HooksState::Outdated
        );
        assert_eq!(
            write_hooks(&path, "/elsewhere/omnimem").unwrap(),
            HooksState::Outdated
        );
        assert_eq!(
            hooks_state(&path, "/elsewhere/omnimem"),
            HooksState::Current
        );
        assert!(crate::client_files::backup_path(&path).exists());
    }

    #[test]
    fn the_mcp_entry_carries_the_token_as_a_header() {
        assert_eq!(
            mcp_entry("http://127.0.0.1:8765/mcp", None),
            json!({"type": "http", "url": "http://127.0.0.1:8765/mcp"})
        );
        assert_eq!(
            mcp_entry("http://127.0.0.1:8765/mcp", Some("t0k")),
            json!({
                "type": "http",
                "url": "http://127.0.0.1:8765/mcp",
                "headers": {"Authorization": "Bearer t0k"}
            })
        );
    }
}
