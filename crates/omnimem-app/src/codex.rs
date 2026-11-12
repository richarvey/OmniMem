//! OmniMem in OpenAI's Codex CLI: its MCP entry and its hooks.
//!
//! Codex keeps its settings in its home folder, `$CODEX_HOME` or `~/.codex`
//! on every platform: MCP servers as `[mcp_servers.<name>]` tables in
//! `config.toml`, and hooks in `hooks.json` (a project's own in its
//! `.codex/hooks.json`, read once the project is trusted). Codex's hooks
//! speak Claude Code's protocol, events, `hookSpecificOutput` and all, so
//! OmniMem's are the same `omnimem hook` commands Claude Code runs: the
//! `PreToolUse` guard and the `SessionStart` briefing. Codex reports its
//! shell as `Bash` and its file edits as `apply_patch`, with the patch in
//! `tool_input.command`.
//!
//! Codex runs a hook only once a person has reviewed and trusted it, in the
//! prompt it shows at start or under `/hooks`, so writing them here is half
//! the job by design: nothing here records that trust.
//!
//! `config.toml` is edited with `toml_edit`, so its comments survive, and
//! both files are copied beside themselves before they change.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Map, Value, json};
use toml_edit::{InlineTable, Item, Table, value};

pub use crate::client_files::{HooksState, McpServer};
use crate::client_files::{read_toml, replace};
pub use crate::shell::hook_base;

/// The name OmniMem's MCP entry goes under.
pub const SERVER_NAME: &str = "omnimem";

/// One of OmniMem's hooks, as it goes in `hooks.json`.
pub struct Hook {
    /// Codex's event.
    pub event: &'static str,
    /// Codex's matcher, a regex: tool names for `PreToolUse`, the session's
    /// source for `SessionStart`.
    pub matcher: &'static str,
    /// The `omnimem hook` event argument, if not the default guard.
    pub argument: Option<&'static str>,
    pub timeout: u64,
}

/// The hooks. The guard matches Codex's shell (`Bash`, which its unified
/// exec reports as too) and `apply_patch`, its one way of editing files;
/// reads never start omnimem. The briefing runs for a new conversation, after
/// `/clear`, and after a compaction, which drops what the session was told.
pub const HOOKS: [Hook; 2] = [
    Hook {
        event: "SessionStart",
        matcher: "^(startup|clear|compact)$",
        argument: Some("session-start"),
        timeout: 15,
    },
    Hook {
        event: "PreToolUse",
        matcher: "^(Bash|apply_patch)$",
        argument: None,
        timeout: 15,
    },
];

/// Codex's home folder.
pub fn home() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|home| home.join(".codex")))
}

/// Codex's user settings, which hold its MCP servers.
pub fn config_path() -> Option<PathBuf> {
    home().map(|dir| dir.join("config.toml"))
}

/// Where the hooks go: Codex's home folder, or with `project`, that
/// project's `.codex` folder.
pub fn hooks_path(project: Option<&Path>) -> Option<PathBuf> {
    let dir = match project {
        Some(project) => project.join(".codex"),
        None => home()?,
    };
    Some(dir.join("hooks.json"))
}

/// The `codex` command: where npm, Bun, Homebrew and the standalone installer
/// put it, then the PATH. An app started from the Dock or Finder doesn't get
/// the shell's PATH, so the usual places come first.
pub fn find_cli() -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["codex.exe", "codex.cmd"]
    } else {
        &["codex"]
    };
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::home_dir() {
        for dir in [".local/bin", ".npm-global/bin", ".bun/bin", ".cargo/bin"] {
            dirs.push(home.join(dir));
        }
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        dirs.push(PathBuf::from(appdata).join("npm"));
    }
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from));
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs.iter()
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|candidate| candidate.is_file())
}

/// Whether Codex is here: its command, its desktop app on macOS, or the home
/// folder it makes the first time it runs.
pub fn installed() -> bool {
    if find_cli().is_some() || home().is_some_and(|dir| dir.is_dir()) {
        return true;
    }
    #[cfg(target_os = "macos")]
    {
        let mut apps = vec![PathBuf::from("/Applications/Codex.app")];
        if let Some(home) = std::env::home_dir() {
            apps.push(home.join("Applications/Codex.app"));
        }
        if apps.iter().any(|app| app.is_dir()) {
            return true;
        }
    }
    false
}

/// One of OmniMem's hooks' command, after `base`.
pub fn hook_command(base: &str, hook: &Hook) -> String {
    match hook.argument {
        Some(argument) => format!("{base} hook {argument}"),
        None => format!("{base} hook"),
    }
}

/// Whether a handler is one of OmniMem's, whoever wrote it: the same test
/// `install-hooks` uses for Claude Code's settings.
fn is_ours(handler: &Value) -> bool {
    handler
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|command| command.contains("omnimem") && command.contains(" hook"))
}

fn group(base: &str, hook: &Hook) -> Value {
    json!({
        "matcher": hook.matcher,
        "hooks": [{
            "type": "command",
            "command": hook_command(base, hook),
            "timeout": hook.timeout,
        }],
    })
}

/// What `hooks.json` holds of OmniMem's hooks, against what would be written.
pub fn hooks_state(path: &Path, base: &str) -> HooksState {
    let config = match read_json(path) {
        Ok(config) => config,
        Err(e) => return HooksState::Unreadable(format!("{e:#}")),
    };
    let events = match events_of(&config) {
        Ok(Some(events)) => events,
        Ok(None) => return HooksState::Missing,
        Err(e) => return HooksState::Unreadable(format!("{e:#}")),
    };
    // Every group holding one of ours, by event.
    let ours: Vec<(&String, &Value)> = events
        .iter()
        .flat_map(|(event, groups)| {
            groups
                .as_array()
                .into_iter()
                .flatten()
                .map(move |group| (event, group))
        })
        .filter(|(_, group)| {
            group
                .get("hooks")
                .and_then(Value::as_array)
                .is_some_and(|handlers| handlers.iter().any(is_ours))
        })
        .collect();
    if ours.is_empty() {
        return HooksState::Missing;
    }
    let current = ours.len() == HOOKS.len()
        && HOOKS.iter().all(|hook| {
            let want = group(base, hook);
            ours.iter()
                .any(|(event, have)| *event == hook.event && same_group(have, &want))
        });
    if current {
        HooksState::Current
    } else {
        HooksState::Outdated
    }
}

/// A group is OmniMem's as written when it has the same matcher and exactly
/// the one handler, however the JSON is laid out.
fn same_group(have: &Value, want: &Value) -> bool {
    let handler = |group: &Value| {
        group
            .get("hooks")
            .and_then(Value::as_array)
            .filter(|handlers| handlers.len() == 1)
            .map(|handlers| {
                let h = &handlers[0];
                (
                    h.get("type").cloned(),
                    h.get("command").cloned(),
                    h.get("timeout").and_then(Value::as_f64),
                    h.get("async").and_then(Value::as_bool).unwrap_or(false),
                )
            })
    };
    have.get("matcher") == want.get("matcher")
        && handler(have).is_some()
        && handler(have) == handler(want)
}

/// Put OmniMem's hooks in `hooks.json`, taking out any of OmniMem's it held
/// before and keeping everything else as it was, with a copy beside it.
pub fn write_hooks(path: &Path, base: &str) -> Result<HooksState> {
    let before = hooks_state(path, base);
    match &before {
        HooksState::Current => return Ok(before),
        HooksState::Unreadable(why) => bail!("couldn't use {}: {why}", path.display()),
        HooksState::Missing | HooksState::Outdated => {}
    }
    let mut config = read_json(path)?;
    let events = config
        .as_object_mut()
        .ok_or_else(|| anyhow!("hooks.json isn't a JSON object"))?
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow!("\"hooks\" in hooks.json isn't a JSON object"))?;
    // An event goes only if taking ours out is what emptied it.
    let mut emptied = Vec::new();
    for (event, groups) in events.iter_mut() {
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };
        let had_groups = !groups.is_empty();
        groups.retain_mut(|group| {
            let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                return true;
            };
            let had = handlers.len();
            handlers.retain(|handler| !is_ours(handler));
            // A group that only held ours goes with them.
            !(handlers.is_empty() && had > 0)
        });
        if had_groups && groups.is_empty() {
            emptied.push(event.clone());
        }
    }
    events.retain(|event, _| !emptied.contains(event));
    for hook in &HOOKS {
        events
            .entry(hook.event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| anyhow!("hooks.{} in hooks.json isn't a list", hook.event))?
            .push(group(base, hook));
    }
    let text = serde_json::to_string_pretty(&config)? + "\n";
    replace(path, &text, false)?;
    Ok(before)
}

/// The `hooks` object of a hooks file, if it has one, refused if it isn't
/// the shape Codex reads.
fn events_of(config: &Value) -> Result<Option<&Map<String, Value>>> {
    let object = config
        .as_object()
        .ok_or_else(|| anyhow!("hooks.json isn't a JSON object"))?;
    match object.get("hooks") {
        None => Ok(None),
        Some(Value::Object(events)) => {
            if let Some((event, _)) = events.iter().find(|(_, groups)| !groups.is_array()) {
                bail!("hooks.{event} in hooks.json isn't a list");
            }
            Ok(Some(events))
        }
        Some(_) => bail!("\"hooks\" in hooks.json isn't a JSON object"),
    }
}

/// The MCP servers in Codex's `config.toml`: none when the file is missing.
pub fn mcp_servers(path: &Path) -> Result<Vec<McpServer>> {
    let doc = read_toml(path)?;
    let Some(item) = doc.get("mcp_servers") else {
        return Ok(Vec::new());
    };
    let servers = item
        .as_table_like()
        .ok_or_else(|| anyhow!("config.toml's mcp_servers isn't a table of servers"))?;
    Ok(servers
        .iter()
        .filter_map(|(name, entry)| {
            let entry = entry.as_table_like()?;
            let text = |key: &str| entry.get(key).and_then(Item::as_str).map(str::to_owned);
            Some(McpServer {
                name: name.to_owned(),
                url: text("url"),
                command: text("command"),
                args: entry
                    .get("args")
                    .and_then(Item::as_array)
                    .map(|args| {
                        args.iter()
                            .filter_map(|a| a.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect())
}

/// Add this server to Codex's `config.toml`, or point its `omnimem` entry
/// here (keeping its name and its place), with the token as a header when
/// one is set. Everything else stays as it was, with a copy beside it.
pub fn write_mcp_server(path: &Path, mcp_url: &str, token: Option<&str>) -> Result<()> {
    let mut doc = read_toml(path)?;
    let servers = doc
        .entry("mcp_servers")
        .or_insert_with(|| {
            // Written as `[mcp_servers.omnimem]` alone, with no empty
            // `[mcp_servers]` header above it.
            let mut table = Table::new();
            table.set_implicit(true);
            Item::Table(table)
        })
        .as_table_mut()
        .ok_or_else(|| {
            anyhow!("config.toml's mcp_servers isn't written as [mcp_servers] tables")
        })?;
    let name = servers
        .iter()
        .map(|(name, _)| name)
        .find(|name| name.eq_ignore_ascii_case(SERVER_NAME))
        .unwrap_or(SERVER_NAME)
        .to_owned();
    let entry = servers
        .entry(&name)
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .ok_or_else(|| anyhow!("mcp_servers.{name} in config.toml isn't a table"))?;
    // Whatever the entry said about reaching another server goes, so
    // nothing left over from it contradicts what's written now.
    for key in [
        "command",
        "args",
        "env",
        "env_vars",
        "cwd",
        "bearer_token_env_var",
        "http_headers",
        "env_http_headers",
        "http_headers_helper",
    ] {
        entry.remove(key);
    }
    entry["url"] = value(mcp_url);
    if let Some(token) = token {
        let mut headers = InlineTable::new();
        headers.insert("Authorization", format!("Bearer {token}").into());
        entry["http_headers"] = value(headers);
    }
    // With a token in it, the file is its owner's alone.
    replace(path, &doc.to_string(), token.is_some()).map(|_| ())
}

/// The file as JSON: an empty object when it's missing or empty.
fn read_json(path: &Path) -> Result<Value> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(json!({})),
        Ok(text) => {
            serde_json::from_str(&text).map_err(|e| anyhow!("hooks.json isn't valid JSON ({e})"))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(e).with_context(|| format!("couldn't read {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("omnimem-codex-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn hooks_are_added_once_updated_in_place_and_others_kept() {
        let dir = scratch("hooks");
        let path = hooks_path(Some(&dir)).unwrap();
        assert_eq!(path, dir.join(".codex/hooks.json"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"hooks": {"PreToolUse": [
                {"matcher": "Bash", "hooks": [
                    {"type": "command", "command": "lint.sh"},
                    {"type": "command", "command": "/old/omnimem hook"}
                ]}
            ], "Stop": [{"hooks": [{"type": "command", "command": "notify.sh"}]}],
            "Notification": [],
            "PostToolUse": [{"hooks": [{"type": "command", "command": "omnimem hook x"}]}]}}"#,
        )
        .unwrap();

        // A hand-written omnimem hook counts as ours, but not as current.
        assert_eq!(hooks_state(&path, "/opt/omnimem"), HooksState::Outdated);
        assert_eq!(
            write_hooks(&path, "/opt/omnimem").unwrap(),
            HooksState::Outdated
        );
        assert_eq!(hooks_state(&path, "/opt/omnimem"), HooksState::Current);
        assert_eq!(
            write_hooks(&path, "/opt/omnimem").unwrap(),
            HooksState::Current
        );

        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let pre = written["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 2);
        assert_eq!(
            pre[0]["hooks"],
            json!([{"type": "command", "command": "lint.sh"}])
        );
        assert_eq!(pre[1]["matcher"], "^(Bash|apply_patch)$");
        assert_eq!(pre[1]["hooks"][0]["command"], "/opt/omnimem hook");
        assert_eq!(
            written["hooks"]["SessionStart"][0]["hooks"][0]["command"],
            "/opt/omnimem hook session-start"
        );
        assert_eq!(
            written["hooks"]["Stop"][0]["hooks"][0]["command"],
            "notify.sh"
        );
        // An event that only held ours goes; one that was empty already stays.
        assert_eq!(written["hooks"].get("PostToolUse"), None);
        assert_eq!(written["hooks"]["Notification"], json!([]));
        assert!(dir.join(".codex/hooks.json.omnimem-backup").is_file());

        // Another program, or one gone, is no longer current.
        assert_eq!(hooks_state(&path, "/usr/bin/omnimem"), HooksState::Outdated);
        let mut fewer = written.clone();
        fewer["hooks"]
            .as_object_mut()
            .unwrap()
            .remove("SessionStart");
        std::fs::write(&path, fewer.to_string()).unwrap();
        assert_eq!(hooks_state(&path, "/opt/omnimem"), HooksState::Outdated);

        // A file of the wrong shape is reported, and left as it is.
        std::fs::write(&path, r#"{"hooks": {"PreToolUse": {}}}"#).unwrap();
        assert!(matches!(
            hooks_state(&path, "/opt/omnimem"),
            HooksState::Unreadable(_)
        ));
        assert!(write_hooks(&path, "/opt/omnimem").is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"{"hooks": {"PreToolUse": {}}}"#
        );

        let fresh = dir.join("fresh/hooks.json");
        assert_eq!(hooks_state(&fresh, "/opt/omnimem"), HooksState::Missing);
        write_hooks(&fresh, "/opt/omnimem").unwrap();
        assert_eq!(hooks_state(&fresh, "/opt/omnimem"), HooksState::Current);
    }

    #[test]
    fn the_mcp_entry_is_added_or_pointed_here_and_the_rest_kept() {
        let dir = scratch("mcp");
        let path = dir.join("config.toml");
        assert_eq!(mcp_servers(&path).unwrap(), []);
        std::fs::write(
            &path,
            "model = \"gpt-5.5\" # mine\n\n[mcp_servers.fetch]\ncommand = \"uvx\"\nargs = [\"mcp-server-fetch\"]\n\n[mcp_servers.OmniMem]\ncommand = \"npx\"\nargs = [\"-y\", \"supergateway\", \"--sse\", \"http://old:8765/sse\"]\n\n[projects.\"/home/me/alpha\"]\ntrust_level = \"trusted\"\n",
        )
        .unwrap();
        let servers = mcp_servers(&path).unwrap();
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[1].name, "OmniMem");
        assert_eq!(servers[1].command.as_deref(), Some("npx"));

        write_mcp_server(&path, "http://127.0.0.1:8765/mcp", Some("t0k")).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("model = \"gpt-5.5\" # mine\n"), "{text}");
        assert!(text.contains("[mcp_servers.fetch]\ncommand = \"uvx\""));
        assert!(!text.contains("supergateway"), "{text}");
        assert!(text.contains("[mcp_servers.OmniMem]\nurl = \"http://127.0.0.1:8765/mcp\"\nhttp_headers = { Authorization = \"Bearer t0k\" }"), "{text}");
        assert!(text.contains("trust_level = \"trusted\""));
        let servers = mcp_servers(&path).unwrap();
        assert_eq!(servers[1].url.as_deref(), Some("http://127.0.0.1:8765/mcp"));
        assert_eq!(servers[1].command, None);
        assert!(dir.join("config.toml.omnimem-backup").is_file());

        write_mcp_server(&path, "http://127.0.0.1:8765/mcp", None).unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("t0k"));
        let fresh = dir.join("fresh/config.toml");
        write_mcp_server(&fresh, "http://127.0.0.1:8765/mcp", None).unwrap();
        assert_eq!(
            std::fs::read_to_string(&fresh).unwrap(),
            "[mcp_servers.omnimem]\nurl = \"http://127.0.0.1:8765/mcp\"\n"
        );

        // An inline table is read too.
        std::fs::write(&path, "mcp_servers = { x = { url = \"http://x/mcp\" } }\n").unwrap();
        assert_eq!(
            mcp_servers(&path).unwrap()[0].url.as_deref(),
            Some("http://x/mcp")
        );
        std::fs::write(&path, "mcp_servers = 3\n").unwrap();
        assert!(mcp_servers(&path).is_err());
        assert!(write_mcp_server(&path, "http://127.0.0.1:8765/mcp", None).is_err());
    }
}
