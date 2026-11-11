//! OmniMem in Mistral's Vibe CLI: its MCP entry and its hooks.
//!
//! Vibe keeps everything in its home folder, `$VIBE_HOME` or `~/.vibe` on
//! every platform: MCP servers as `[[mcp_servers]]` tables in `config.toml`,
//! hooks as `[[hooks]]` tables in `hooks.toml` (a project's own are in its
//! `.vibe/hooks.toml`, read once the folder is trusted). Hooks are shell
//! commands, like Claude Code's, that read one JSON object on stdin and may
//! print one back, so OmniMem's are `omnimem hook ... --agent vibe`: a
//! `pre_tool` guard that denies a call repeating a dead end, and, from Vibe
//! 3.1.1, a `session_start` hook that hands the session its briefing.
//!
//! Both files are edited with `toml_edit`, so comments and layout survive,
//! after a copy of the file as it was is kept beside it. OmniMem's hooks are
//! the ones named `omnimem-*`; nothing else in the file is touched.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use toml_edit::{ArrayOfTables, DocumentMut, InlineTable, Item, Table, value};

/// The name OmniMem's MCP entry goes under.
pub const SERVER_NAME: &str = "omnimem";

/// What every hook OmniMem writes is called, so its own can be told apart.
const HOOK_PREFIX: &str = "omnimem-";

/// One of OmniMem's hooks, as it goes in `hooks.toml`.
pub struct Hook {
    pub name: &'static str,
    /// Vibe's hook type.
    pub kind: &'static str,
    /// Vibe's matcher: tool names for `pre_tool`, the session's source for
    /// `session_start`.
    pub matcher: &'static str,
    /// The `omnimem hook` event it answers.
    pub event: &'static str,
    pub timeout: i64,
    pub description: &'static str,
}

/// The hooks. The guard matches Vibe's tools that change something (its
/// short names: `file_system.edit` is `edit`), and `process_start` because
/// background commands go through it rather than `bash`. Reads are never
/// matched, so they don't start omnimem at all. The briefing runs for a new
/// conversation (`startup`, or `/clear`), not a resumed or forked one, which
/// already has it.
pub const HOOKS: [Hook; 2] = [
    Hook {
        name: "omnimem-briefing",
        kind: "session_start",
        matcher: "re:startup|clear",
        event: "session-start",
        timeout: 15,
        description: "OmniMem: what this project already knows",
    },
    Hook {
        name: "omnimem-guard",
        kind: "pre_tool",
        matcher: "re:edit|write_file|bash|process_start",
        event: "pre-tool-use",
        timeout: 15,
        description: "OmniMem: stop an approach this project already abandoned",
    },
];

/// Vibe's home folder.
pub fn home() -> Option<PathBuf> {
    std::env::var_os("VIBE_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|home| home.join(".vibe")))
}

/// Vibe's user settings, which hold its MCP servers.
pub fn config_path() -> Option<PathBuf> {
    home().map(|dir| dir.join("config.toml"))
}

/// Where the hooks go: Vibe's home folder, or with `project`, that project's
/// `.vibe` folder.
pub fn hooks_path(project: Option<&Path>) -> Option<PathBuf> {
    let dir = match project {
        Some(project) => project.join(".vibe"),
        None => home()?,
    };
    Some(dir.join("hooks.toml"))
}

/// The `vibe` command: where its installer (`uv tool`) and Homebrew put it,
/// then the PATH. An app started from the Dock or Finder doesn't get the
/// shell's PATH, so the usual places come first.
pub fn find_cli() -> Option<PathBuf> {
    let name = if cfg!(windows) { "vibe.exe" } else { "vibe" };
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::home_dir() {
        dirs.push(home.join(".local/bin"));
    }
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from));
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs.into_iter()
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// Whether Vibe is here: its command, or the home folder it makes the first
/// time it runs.
pub fn installed() -> bool {
    find_cli().is_some() || home().is_some_and(|dir| dir.is_dir())
}

/// The command line a hook starts with: this program by its full path (an
/// app's own PATH can't be counted on) and `--db` when the hooks should use a
/// database other than the default. Vibe runs a hook through `/bin/sh`, or on
/// Windows through Git Bash or PowerShell, so a Windows path is written with
/// forward slashes, which all three take.
pub fn hook_base(program: &Path, db: Option<&Path>) -> String {
    let mut line = shell_word(&program.to_string_lossy());
    if let Some(db) = db {
        let db = std::path::absolute(db).unwrap_or_else(|_| db.to_path_buf());
        line.push_str(" --db ");
        line.push_str(&shell_word(&db.to_string_lossy()));
    }
    line
}

/// One of OmniMem's hooks' command, after `base`.
pub fn hook_command(base: &str, hook: &Hook) -> String {
    format!("{base} hook {} --agent vibe", hook.event)
}

/// A word the hook's shell reads back as `word`: bare when it can be, else
/// in single quotes for `/bin/sh`, or double quotes on Windows, where
/// PowerShell has no single-quote escape that Git Bash shares.
fn shell_word(word: &str) -> String {
    let word = if cfg!(windows) {
        word.replace('\\', "/")
    } else {
        word.to_owned()
    };
    if !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/:._-+".contains(c))
    {
        word
    } else if cfg!(windows) {
        format!("\"{}\"", word.replace('"', "\\\""))
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// What `hooks.toml` holds of OmniMem's hooks, against what would be written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HooksState {
    /// None of them.
    Missing,
    /// All of them, exactly as they'd be written.
    Current,
    /// Some, or from another version, or running another program.
    Outdated,
    /// The file couldn't be read, for this reason.
    Unreadable(String),
}

pub fn hooks_state(path: &Path, base: &str) -> HooksState {
    let doc = match read_doc(path) {
        Ok(doc) => doc,
        Err(e) => return HooksState::Unreadable(format!("{e:#}")),
    };
    let tables = match doc.get("hooks") {
        None => return HooksState::Missing,
        Some(item) => match item.as_array_of_tables() {
            Some(tables) => tables,
            None => {
                return HooksState::Unreadable(
                    "hooks.toml's hooks aren't [[hooks]] tables".to_owned(),
                );
            }
        },
    };
    let ours: Vec<&Table> = tables
        .iter()
        .filter(|table| hook_name(table).is_some_and(|name| name.starts_with(HOOK_PREFIX)))
        .collect();
    if ours.is_empty() {
        return HooksState::Missing;
    }
    let wanted: Vec<Table> = HOOKS.iter().map(|hook| hook_table(base, hook)).collect();
    let same = ours.len() == wanted.len()
        && wanted.iter().all(|want| {
            ours.iter()
                .find(|have| hook_name(have) == hook_name(want))
                .is_some_and(|have| same_hook(have, want))
        });
    if same {
        HooksState::Current
    } else {
        HooksState::Outdated
    }
}

/// Put OmniMem's hooks in `hooks.toml`, replacing any it wrote before and
/// keeping everything else as it was, with a copy of the file beside it.
pub fn write_hooks(path: &Path, base: &str) -> Result<HooksState> {
    let before = hooks_state(path, base);
    match &before {
        HooksState::Current => return Ok(before),
        HooksState::Unreadable(why) => bail!("couldn't use {}: {why}", path.display()),
        HooksState::Missing | HooksState::Outdated => {}
    }
    let mut doc = read_doc(path)?;
    let tables = doc
        .entry("hooks")
        .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()))
        .as_array_of_tables_mut()
        .ok_or_else(|| anyhow!("hooks.toml's hooks aren't [[hooks]] tables"))?;
    tables.retain(|table| !hook_name(table).is_some_and(|name| name.starts_with(HOOK_PREFIX)));
    for hook in &HOOKS {
        tables.push(hook_table(base, hook));
    }
    replace_file(path, &doc.to_string())?;
    Ok(before)
}

fn hook_name(table: &Table) -> Option<&str> {
    table.get("name").and_then(Item::as_str)
}

fn hook_table(base: &str, hook: &Hook) -> Table {
    let mut table = Table::new();
    table["name"] = value(hook.name);
    table["type"] = value(hook.kind);
    table["match"] = value(hook.matcher);
    table["command"] = value(hook_command(base, hook));
    table["timeout"] = value(hook.timeout);
    table["description"] = value(hook.description);
    table
}

/// Whether two hook tables say the same thing, whatever their layout.
fn same_hook(have: &Table, want: &Table) -> bool {
    let text = |table: &Table, key: &str| table.get(key).and_then(Item::as_str).map(str::to_owned);
    let seconds = |table: &Table| {
        let item = table.get("timeout")?;
        item.as_float()
            .or_else(|| item.as_integer().map(|n| n as f64))
    };
    ["name", "type", "match", "command"]
        .iter()
        .all(|key| text(have, key) == text(want, key))
        && seconds(have) == seconds(want)
        && !have.get("strict").and_then(Item::as_bool).unwrap_or(false)
}

/// One of Vibe's MCP servers, as much of it as says where it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServer {
    pub name: String,
    pub url: Option<String>,
    /// A stdio server's program and its arguments.
    pub command: Option<String>,
    pub args: Vec<String>,
}

/// The MCP servers in Vibe's `config.toml`: none when the file is missing.
pub fn mcp_servers(path: &Path) -> Result<Vec<McpServer>> {
    let doc = read_doc(path)?;
    let Some(item) = doc.get("mcp_servers") else {
        return Ok(Vec::new());
    };
    let text = |get: Option<&toml_edit::Value>| get.and_then(|v| v.as_str()).map(str::to_owned);
    let mut servers = Vec::new();
    let mut add = |name: Option<String>, url, command, args: Option<&toml_edit::Array>| {
        servers.push(McpServer {
            name: name.unwrap_or_default(),
            url,
            command,
            args: args
                .map(|args| {
                    args.iter()
                        .filter_map(|a| a.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
        });
    };
    if let Some(tables) = item.as_array_of_tables() {
        for table in tables {
            let get = |key: &str| table.get(key).and_then(Item::as_value);
            add(
                text(get("name")),
                text(get("url")),
                text(get("command")),
                get("args").and_then(|v| v.as_array()),
            );
        }
    } else if let Some(array) = item.as_array() {
        for table in array.iter().filter_map(|v| v.as_inline_table()) {
            add(
                text(table.get("name")),
                text(table.get("url")),
                text(table.get("command")),
                table.get("args").and_then(|v| v.as_array()),
            );
        }
    } else {
        bail!("config.toml's mcp_servers isn't a list of servers");
    }
    Ok(servers)
}

/// Add this server to Vibe's `config.toml`, or point its `omnimem` entry
/// here (keeping its name and its place), with the token as a header when
/// one is set. Everything else stays as it was, with a copy beside it.
pub fn write_mcp_server(path: &Path, mcp_url: &str, token: Option<&str>) -> Result<()> {
    let mut doc = read_doc(path)?;
    let tables = doc
        .entry("mcp_servers")
        .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()))
        .as_array_of_tables_mut()
        .ok_or_else(|| {
            anyhow!("config.toml's mcp_servers isn't written as [[mcp_servers]] tables")
        })?;
    let existing = tables.iter_mut().find(|table| {
        table
            .get("name")
            .and_then(Item::as_str)
            .is_some_and(|name| name.eq_ignore_ascii_case(SERVER_NAME))
    });
    let table = match existing {
        Some(table) => table,
        None => {
            let mut table = Table::new();
            table["name"] = value(SERVER_NAME);
            tables.push(table);
            tables
                .iter_mut()
                .last()
                .ok_or_else(|| anyhow!("the new entry went missing"))?
        }
    };
    // Whatever the entry said about reaching another server goes, so
    // nothing left over from it contradicts what's written now.
    for key in [
        "command",
        "args",
        "env",
        "headers",
        "api_key_env",
        "api_key_header",
        "api_key_format",
        "auth",
    ] {
        table.remove(key);
    }
    table["transport"] = value("streamable-http");
    table["url"] = value(mcp_url);
    if let Some(token) = token {
        let mut headers = InlineTable::new();
        headers.insert("Authorization", format!("Bearer {token}").into());
        let mut auth = Table::new();
        auth["type"] = value("static");
        auth["headers"] = value(headers);
        table.insert("auth", Item::Table(auth));
    }
    replace_file(path, &doc.to_string())
}

/// The file as TOML: empty when it's missing. A file that isn't TOML is an
/// error, so it's left alone rather than replaced.
fn read_doc(path: &Path) -> Result<DocumentMut> {
    let file = path
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    match std::fs::read_to_string(path) {
        Ok(text) => text
            .parse::<DocumentMut>()
            .map_err(|e| anyhow!("{file} isn't valid TOML ({})", e.message())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(e) => Err(e).with_context(|| format!("couldn't read {file}")),
    }
}

/// Keep a copy of the file as it was, then write beside it and rename into
/// place, so Vibe never reads half a file.
fn replace_file(path: &Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    if path.exists() {
        std::fs::copy(path, with_suffix(path, ".omnimem-backup"))
            .with_context(|| format!("backing up {}", path.display()))?;
    }
    let partial = with_suffix(path, ".omnimem-partial");
    std::fs::write(&partial, text).with_context(|| format!("writing {}", path.display()))?;
    std::fs::rename(&partial, path).with_context(|| format!("replacing {}", path.display()))
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name: OsString = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("omnimem-vibe-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_hooks_run_this_program_with_its_database() {
        let base = hook_base(
            Path::new("/opt/OmniMem/omnimem"),
            Some(Path::new("/d/x.db")),
        );
        assert_eq!(base, "/opt/OmniMem/omnimem --db /d/x.db");
        assert_eq!(
            hook_command(&base, &HOOKS[1]),
            "/opt/OmniMem/omnimem --db /d/x.db hook pre-tool-use --agent vibe"
        );
        if !cfg!(windows) {
            assert_eq!(
                hook_base(Path::new("/Users/a b/omnimem"), None),
                "'/Users/a b/omnimem'"
            );
            assert_eq!(shell_word("it's"), r"'it'\''s'");
        }
    }

    #[test]
    fn hooks_are_added_once_updated_in_place_and_others_kept() {
        let dir = scratch("hooks");
        let path = hooks_path(Some(&dir)).unwrap();
        assert_eq!(path, dir.join(".vibe/hooks.toml"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "# my hooks\n[[hooks]]\nname = \"lint\"\ntype = \"post_agent\"\ncommand = \"make lint\"\n",
        )
        .unwrap();

        assert_eq!(hooks_state(&path, "/old/omnimem"), HooksState::Missing);
        assert_eq!(
            write_hooks(&path, "/old/omnimem").unwrap(),
            HooksState::Missing
        );
        assert_eq!(hooks_state(&path, "/old/omnimem"), HooksState::Current);
        assert_eq!(hooks_state(&path, "/new/omnimem"), HooksState::Outdated);
        assert_eq!(
            write_hooks(&path, "/new/omnimem").unwrap(),
            HooksState::Outdated
        );
        assert_eq!(
            write_hooks(&path, "/new/omnimem").unwrap(),
            HooksState::Current
        );

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# my hooks\n"), "{text}");
        assert!(text.contains("command = \"make lint\""));
        assert_eq!(text.matches("name = \"omnimem-guard\"").count(), 1);
        assert!(
            text.contains(
                "command = \"/new/omnimem hook pre-tool-use --agent vibe\"\ntimeout = 15"
            )
        );
        assert!(text.contains("match = \"re:edit|write_file|bash|process_start\""));
        assert!(text.contains("type = \"session_start\""));
        assert!(dir.join(".vibe/hooks.toml.omnimem-backup").is_file());

        // One of ours gone, or made strict, is no longer current.
        let doc = text.replace("timeout = 15\n", "timeout = 15.0\n");
        std::fs::write(&path, &doc).unwrap();
        assert_eq!(hooks_state(&path, "/new/omnimem"), HooksState::Current);
        std::fs::write(
            &path,
            doc.replacen("timeout = 15.0\n", "timeout = 15.0\nstrict = true\n", 1),
        )
        .unwrap();
        assert_eq!(hooks_state(&path, "/new/omnimem"), HooksState::Outdated);

        std::fs::write(&path, "hooks = 3\n").unwrap();
        assert!(matches!(
            hooks_state(&path, "/new/omnimem"),
            HooksState::Unreadable(_)
        ));
        assert!(write_hooks(&path, "/new/omnimem").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hooks = 3\n");
        std::fs::write(&path, "[[hooks]\n").unwrap();
        assert!(matches!(
            hooks_state(&path, "/new/omnimem"),
            HooksState::Unreadable(_)
        ));
    }

    #[test]
    fn the_mcp_entry_is_added_or_pointed_here_and_the_rest_kept() {
        let dir = scratch("mcp");
        let path = dir.join("config.toml");
        assert_eq!(mcp_servers(&path).unwrap(), []);
        std::fs::write(
            &path,
            "active_model = \"devstral\" # mine\n\n[[mcp_servers]]\nname = \"fetch\"\ntransport = \"stdio\"\ncommand = \"uvx\"\nargs = [\"mcp-server-fetch\"]\n\n[[mcp_servers]]\nname = \"OmniMem\"\ntransport = \"stdio\"\ncommand = \"npx\"\nargs = [\"mcp-remote\", \"http://old:8765/mcp\"]\n",
        )
        .unwrap();
        let servers = mcp_servers(&path).unwrap();
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[1].name, "OmniMem");
        assert_eq!(servers[1].args, ["mcp-remote", "http://old:8765/mcp"]);

        write_mcp_server(&path, "http://127.0.0.1:8765/mcp", Some("t0k")).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("active_model = \"devstral\" # mine\n"),
            "{text}"
        );
        assert!(text.contains("command = \"uvx\""));
        assert!(!text.contains("npx"), "{text}");
        assert!(text.contains("name = \"OmniMem\""));
        assert!(text.contains("transport = \"streamable-http\""));
        assert!(
            text.contains("[mcp_servers.auth]\ntype = \"static\""),
            "{text}"
        );
        assert!(text.contains("Authorization = \"Bearer t0k\""));
        let servers = mcp_servers(&path).unwrap();
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[1].url.as_deref(), Some("http://127.0.0.1:8765/mcp"));
        assert_eq!(servers[1].command, None);
        assert!(dir.join("config.toml.omnimem-backup").is_file());

        // Without a token the old one goes, and a new file gets one entry.
        write_mcp_server(&path, "http://127.0.0.1:8765/mcp", None).unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("t0k"));
        let fresh = dir.join("fresh/config.toml");
        write_mcp_server(&fresh, "http://127.0.0.1:8765/mcp", None).unwrap();
        assert_eq!(
            std::fs::read_to_string(&fresh).unwrap(),
            "[[mcp_servers]]\nname = \"omnimem\"\ntransport = \"streamable-http\"\nurl = \"http://127.0.0.1:8765/mcp\"\n"
        );

        // An inline list is read, but not rewritten.
        std::fs::write(
            &path,
            "mcp_servers = [{ name = \"x\", transport = \"http\", url = \"http://x/mcp\" }]\n",
        )
        .unwrap();
        assert_eq!(
            mcp_servers(&path).unwrap()[0].url.as_deref(),
            Some("http://x/mcp")
        );
        assert!(write_mcp_server(&path, "http://127.0.0.1:8765/mcp", None).is_err());
    }
}
