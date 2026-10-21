//! OmniMem in Claude Desktop and Claude Code, from the tray menu: whether
//! each already has it, adding it, and switching an `omnimem` entry that
//! points at another server (a 6.x install, say) to this one. Nothing is ever
//! added twice: an entry that reaches this server, under any name, counts.
//!
//! Claude Desktop's config file is small and only Claude Desktop writes it, at
//! start, so it's edited here, after a backup. Claude Code's `~/.claude.json`
//! is rewritten all the time by running sessions, so it's only ever read
//! here; changes go through Claude Code's own `claude mcp` commands.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

/// The name OmniMem's entry goes under, as the guides use.
pub const SERVER_NAME: &str = "omnimem";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Client {
    Desktop,
    Code,
}

impl Client {
    pub fn name(self) -> &'static str {
        match self {
            Client::Desktop => "Claude Desktop",
            Client::Code => "Claude Code",
        }
    }
}

/// What a client's configuration says about OmniMem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Presence {
    /// The client isn't installed here.
    NotInstalled,
    /// It has no OmniMem.
    Absent,
    /// An entry already reaches this server.
    Here,
    /// Its entry called `name` (`omnimem`, in any case) reaches `target`.
    Elsewhere { name: String, target: String },
    /// Its configuration couldn't be read, for this reason.
    Unreadable(String),
}

/// What `mcpServers` in a client's configuration says about the server at
/// `mcp_url`.
pub fn presence_in(config: &Value, mcp_url: &str) -> Presence {
    let Some(servers) = config.get("mcpServers").and_then(Value::as_object) else {
        return Presence::Absent;
    };
    let wanted = comparable(mcp_url);
    if servers
        .values()
        .any(|entry| urls_of(entry).any(|url| comparable(url) == wanted))
    {
        return Presence::Here;
    }
    match servers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(SERVER_NAME))
    {
        Some((name, entry)) => Presence::Elsewhere {
            name: name.clone(),
            target: urls_of(entry)
                .next()
                .or_else(|| entry.get("command").and_then(Value::as_str))
                .unwrap_or("another server")
                .to_owned(),
        },
        None => Presence::Absent,
    }
}

/// The URLs an entry reaches: its `url`, or the URL arguments of a bridge
/// command such as `npx mcp-remote <url>`.
fn urls_of(entry: &Value) -> impl Iterator<Item = &str> {
    let args = entry
        .get("args")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|arg| arg.starts_with("http://") || arg.starts_with("https://"));
    entry
        .get("url")
        .and_then(Value::as_str)
        .into_iter()
        .chain(args)
}

/// A URL in a form that compares: lower case, no trailing slash, and
/// `localhost` and `[::1]` read as 127.0.0.1.
fn comparable(url: &str) -> String {
    url.trim()
        .trim_end_matches('/')
        .to_ascii_lowercase()
        .replace("://localhost", "://127.0.0.1")
        .replace("://[::1]", "://127.0.0.1")
}

/// The host of a URL for a menu label, or the target as it is.
pub fn host_of(target: &str) -> String {
    url::Url::parse(target)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| target.to_owned())
}

/// Claude Desktop's config file, on the platforms Claude Desktop runs on.
pub fn desktop_config_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::home_dir()
            .map(|home| home.join("Library/Application Support/Claude/claude_desktop_config.json"))
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA").map(|dir| {
            PathBuf::from(dir)
                .join("Claude")
                .join("claude_desktop_config.json")
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        None
    }
}

/// Whether Claude Desktop, configured at `path`, has OmniMem.
pub fn desktop_presence(path: Option<&Path>, mcp_url: &str) -> Presence {
    let Some(path) = path else {
        return Presence::NotInstalled;
    };
    // Claude Desktop makes its folder on first run: no folder, no app.
    if !path.parent().is_some_and(Path::is_dir) {
        return Presence::NotInstalled;
    }
    match read_json(path) {
        Ok(Some(config)) => presence_in(&config, mcp_url),
        Ok(None) => Presence::Absent,
        Err(e) => Presence::Unreadable(e),
    }
}

/// Claude Desktop's entry for this server, as the guide gives it: Claude
/// Desktop runs servers as commands, so `mcp-remote` bridges to the URL.
pub fn desktop_entry(mcp_url: &str, token: Option<&str>) -> Value {
    let mut args = vec![json!("-y"), json!("mcp-remote"), json!(mcp_url)];
    if let Some(token) = token {
        args.push(json!("--header"));
        args.push(json!(format!("Authorization: Bearer {token}")));
    }
    json!({"command": "npx", "args": args})
}

/// Where the config is copied before it's changed.
pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(OsString::from).unwrap_or_default();
    name.push(".omnimem-backup");
    path.with_file_name(name)
}

/// Add this server to Claude Desktop, or point its `omnimem` entry here,
/// keeping everything else in the file and a copy of the file as it was.
pub fn write_desktop(path: &Path, mcp_url: &str, token: Option<&str>) -> Result<(), String> {
    let mut config = read_json(path)?.unwrap_or_else(|| json!({}));
    let config_object = config
        .as_object_mut()
        .ok_or("the config file isn't a JSON object")?;
    let servers = config_object
        .entry("mcpServers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("mcpServers in the config file isn't a JSON object")?;
    // An existing entry keeps its name and its place in the file.
    let name = servers
        .keys()
        .find(|name| name.eq_ignore_ascii_case(SERVER_NAME))
        .cloned()
        .unwrap_or_else(|| SERVER_NAME.to_owned());
    servers.insert(name, desktop_entry(mcp_url, token));
    let text = serde_json::to_string_pretty(&config)
        .map_err(|e| format!("couldn't write the config: {e}"))?;
    if path.exists() {
        std::fs::copy(path, backup_path(path))
            .map_err(|e| format!("couldn't back up the config: {e}"))?;
    }
    // Written beside it and renamed into place, so Claude Desktop can never
    // read half a file.
    let mut partial = path.as_os_str().to_owned();
    partial.push(".omnimem-partial");
    std::fs::write(&partial, text + "\n").map_err(|e| format!("couldn't write the config: {e}"))?;
    std::fs::rename(&partial, path).map_err(|e| format!("couldn't replace the config: {e}"))
}

/// Claude Code's user settings, which hold its user-scope MCP servers.
pub fn code_config_path() -> Option<PathBuf> {
    std::env::home_dir().map(|home| home.join(".claude.json"))
}

/// Where Claude Code's installers put `claude`, then the PATH. An app
/// started from the Dock or Finder doesn't get the shell's PATH, so the
/// usual places come first.
pub fn find_claude() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    };
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::home_dir() {
        candidates.push(home.join(".local/bin").join(name));
        candidates.push(home.join(".claude/local").join(name));
        candidates.push(home.join(".npm-global/bin").join(name));
    }
    candidates.extend(
        ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(|dir| Path::new(dir).join(name)),
    );
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|dir| dir.join(name)));
    }
    candidates.into_iter().find(|candidate| candidate.is_file())
}

/// Whether Claude Code, with its `claude` command at `cli` and its settings
/// at `config`, has OmniMem in its user scope.
pub fn code_presence(cli: Option<&Path>, config: Option<&Path>, mcp_url: &str) -> Presence {
    if cli.is_none() {
        return Presence::NotInstalled;
    }
    match config.map(read_json) {
        Some(Ok(Some(settings))) => presence_in(&settings, mcp_url),
        Some(Ok(None)) | None => Presence::Absent,
        Some(Err(e)) => Presence::Unreadable(e),
    }
}

/// `claude` arguments that add this server for every project.
pub fn code_add_args(mcp_url: &str, token: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = ["mcp", "add", "--transport", "http", "--scope", "user"]
        .map(str::to_owned)
        .into();
    args.push(SERVER_NAME.to_owned());
    args.push(mcp_url.to_owned());
    if let Some(token) = token {
        args.push("--header".to_owned());
        args.push(format!("Authorization: Bearer {token}"));
    }
    args
}

/// Add this server to Claude Code, first removing the entry called
/// `replacing` when it points somewhere else.
pub fn write_code(
    cli: &Path,
    replacing: Option<&str>,
    mcp_url: &str,
    token: Option<&str>,
) -> Result<(), String> {
    if let Some(name) = replacing {
        run_claude(cli, &["mcp", "remove", "--scope", "user", name])?;
    }
    let args = code_add_args(mcp_url, token);
    run_claude(cli, &args.iter().map(String::as_str).collect::<Vec<_>>())
}

fn run_claude(cli: &Path, args: &[&str]) -> Result<(), String> {
    // A claude installed with npm is a Node script, so `env node` needs the
    // folders node usually lives in.
    let mut dirs: Vec<PathBuf> = cli.parent().map(Path::to_path_buf).into_iter().collect();
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"].map(PathBuf::from));
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    let path = std::env::join_paths(dirs).map_err(|e| e.to_string())?;
    let output = Command::new(cli)
        .args(args)
        .env("PATH", path)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("couldn't run claude: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&output.stderr);
    let said = if said.trim().is_empty() {
        String::from_utf8_lossy(&output.stdout)
    } else {
        said
    };
    Err(said
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("claude mcp failed")
        .to_owned())
}

/// The file's JSON, `None` when it's missing or empty.
fn read_json(path: &Path) -> Result<Option<Value>, String> {
    let file = path
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(None),
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| format!("{file} isn't valid JSON ({e})")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("couldn't read {file}: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "http://127.0.0.1:8765/mcp";

    /// A fresh folder for one test's files.
    fn scratch(test: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("omnimem-clients-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_entry_reaching_this_server_counts_whatever_its_name_or_form() {
        let config = |servers: Value| json!({ "mcpServers": servers });
        assert_eq!(presence_in(&json!({}), URL), Presence::Absent);
        assert_eq!(
            presence_in(&config(json!({"forgejo": {"command": "npx"}})), URL),
            Presence::Absent
        );
        assert_eq!(
            presence_in(
                &config(json!({"memory": {"type": "http", "url": "http://localhost:8765/mcp/"}})),
                URL
            ),
            Presence::Here
        );
        assert_eq!(
            presence_in(
                &config(json!({"omnimem": {"command": "npx", "args": ["-y", "mcp-remote", URL]}})),
                URL
            ),
            Presence::Here
        );
        // Another OmniMem doesn't matter when this one is already there.
        assert_eq!(
            presence_in(
                &config(json!({
                    "OmniMem": {"type": "http", "url": "https://cloudai.example.net/mcp"},
                    "omnimem-local": {"type": "http", "url": URL},
                })),
                URL
            ),
            Presence::Here
        );
    }

    #[test]
    fn an_omnimem_entry_reaching_another_server_is_offered_for_switching() {
        let config = json!({"mcpServers": {"OmniMem": {
            "command": "npx", "args": ["-y", "mcp-remote", "https://cloudai.example.net/mcp"]
        }}});
        assert_eq!(
            presence_in(&config, URL),
            Presence::Elsewhere {
                name: "OmniMem".to_owned(),
                target: "https://cloudai.example.net/mcp".to_owned()
            }
        );
        assert_eq!(
            host_of("https://cloudai.example.net/mcp"),
            "cloudai.example.net"
        );
        assert_eq!(host_of("omnimem-bridge"), "omnimem-bridge");
    }

    #[test]
    fn desktop_presence_needs_the_app_and_a_readable_file() {
        let dir = scratch("desktop-presence");
        assert_eq!(desktop_presence(None, URL), Presence::NotInstalled);
        assert_eq!(
            desktop_presence(Some(&dir.join("missing/claude_desktop_config.json")), URL),
            Presence::NotInstalled
        );
        let path = dir.join("claude_desktop_config.json");
        assert_eq!(desktop_presence(Some(&path), URL), Presence::Absent);
        std::fs::write(&path, "{ not json").unwrap();
        assert!(matches!(
            desktop_presence(Some(&path), URL),
            Presence::Unreadable(_)
        ));
    }

    #[test]
    fn writing_the_desktop_config_adds_once_and_keeps_everything_else() {
        let dir = scratch("desktop-write");
        let path = dir.join("claude_desktop_config.json");
        let before = r#"{
  "mcpServers": {
    "forgejo": { "command": "npx", "args": ["@ric_/forgejo-mcp"] }
  },
  "preferences": { "sidebarMode": "chat" }
}"#;
        std::fs::write(&path, before).unwrap();
        write_desktop(&path, URL, None).unwrap();
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["preferences"]["sidebarMode"], "chat");
        assert_eq!(
            written["mcpServers"]["forgejo"]["args"][0],
            "@ric_/forgejo-mcp"
        );
        assert_eq!(written["mcpServers"]["omnimem"], desktop_entry(URL, None));
        assert_eq!(std::fs::read_to_string(backup_path(&path)).unwrap(), before);
        assert_eq!(desktop_presence(Some(&path), URL), Presence::Here);

        // Writing again leaves one entry, not two.
        write_desktop(&path, URL, None).unwrap();
        let again: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(again["mcpServers"].as_object().unwrap().len(), 2);
    }

    #[test]
    fn switching_repoints_the_existing_entry_in_place() {
        let dir = scratch("desktop-switch");
        let path = dir.join("claude_desktop_config.json");
        std::fs::write(
            &path,
            r#"{"mcpServers": {"a": {"command": "x"}, "OmniMem": {"command": "npx", "args": ["-y", "mcp-remote", "https://cloudai.example.net/mcp"]}, "z": {"command": "y"}}}"#,
        )
        .unwrap();
        write_desktop(&path, URL, Some("s3cret")).unwrap();
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let names: Vec<&String> = written["mcpServers"].as_object().unwrap().keys().collect();
        assert_eq!(names, ["a", "OmniMem", "z"], "same name, same place");
        assert_eq!(
            written["mcpServers"]["OmniMem"],
            desktop_entry(URL, Some("s3cret"))
        );
        assert_eq!(
            written["mcpServers"]["OmniMem"]["args"][4],
            "Authorization: Bearer s3cret"
        );
    }

    #[test]
    fn a_new_desktop_config_is_created_and_a_broken_one_left_alone() {
        let dir = scratch("desktop-new");
        let path = dir.join("claude_desktop_config.json");
        write_desktop(&path, URL, None).unwrap();
        assert_eq!(desktop_presence(Some(&path), URL), Presence::Here);
        assert!(!backup_path(&path).exists(), "nothing to back up");

        std::fs::write(&path, "{ broken").unwrap();
        assert!(write_desktop(&path, URL, None).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ broken");
    }

    #[test]
    fn claude_code_is_read_only_here_and_added_through_its_cli() {
        let dir = scratch("code");
        let settings = dir.join(".claude.json");
        let cli = dir.join("claude");
        assert_eq!(
            code_presence(None, Some(&settings), URL),
            Presence::NotInstalled
        );
        assert_eq!(
            code_presence(Some(&cli), Some(&settings), URL),
            Presence::Absent
        );
        std::fs::write(
            &settings,
            format!(r#"{{"mcpServers": {{"omnimem": {{"type": "http", "url": "{URL}"}}}}}}"#),
        )
        .unwrap();
        assert_eq!(
            code_presence(Some(&cli), Some(&settings), URL),
            Presence::Here
        );
        assert_eq!(
            code_add_args(URL, Some("t")),
            [
                "mcp",
                "add",
                "--transport",
                "http",
                "--scope",
                "user",
                "omnimem",
                URL,
                "--header",
                "Authorization: Bearer t"
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_cli_is_run_with_its_arguments_and_its_complaint_comes_back() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("cli");
        let log = dir.join("calls");
        let cli = dir.join("claude");
        std::fs::write(
            &cli,
            format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\n[ \"$2\" = remove ] && [ \"$5\" = Broken ] && {{ echo 'No MCP server named Broken' >&2; exit 1; }}\nexit 0\n",
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();

        write_code(&cli, Some("OmniMem"), URL, None).unwrap();
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            format!(
                "mcp remove --scope user OmniMem\nmcp add --transport http --scope user omnimem {URL}\n"
            )
        );
        assert_eq!(
            write_code(&cli, Some("Broken"), URL, None),
            Err("No MCP server named Broken".to_owned())
        );
    }
}
