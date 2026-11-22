//! OmniMem's hooks for OpenCode, as an OpenCode plugin.
//!
//! OpenCode has no shell-command hooks like Claude Code's, but its plugins
//! (JavaScript run inside OpenCode) can stop a tool call by throwing before it
//! runs and add to the system prompt. The plugin written here does both by
//! running `omnimem hook` with the JSON Claude Code would send, so the dead-end
//! guard and the briefing are the same code for both agents. It speaks both
//! plugin APIs from one default export: 2.x's `setup()` (`tool` `execute.before`
//! and `session` `context`) and 1.x's `server()` (`tool.execute.before` and
//! `experimental.chat.system.transform`, from OpenCode 1.18.29).
//!
//! OpenCode finds plugins in `{plugin,plugins}/*.{js,ts}` under its config
//! folder, `$XDG_CONFIG_HOME/opencode` or `~/.config/opencode` on every
//! platform, and under a project's `.opencode`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The plugin, with `__OMNIMEM_COMMAND__` where the program goes.
const PLUGIN_TEMPLATE: &str = include_str!("opencode-plugin.js");

/// What the plugin's file is called.
pub const PLUGIN_FILE: &str = "omnimem.js";

/// A line every plugin OmniMem wrote starts with, so one can be told from
/// somebody else's file of the same name.
const PLUGIN_MARK: &str = "// OmniMem for OpenCode";

/// OpenCode's global config folder.
pub fn config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|home| home.join(".config")))?;
    Some(base.join("opencode"))
}

/// Where the plugin goes: OpenCode's global plugins folder, or with
/// `project`, that project's `.opencode/plugins`.
pub fn plugin_path(project: Option<&Path>) -> Option<PathBuf> {
    let dir = match project {
        Some(project) => project.join(".opencode"),
        None => config_dir()?,
    };
    Some(dir.join("plugins").join(PLUGIN_FILE))
}

/// The `opencode` command: where its installers put it, then the PATH. An
/// app started from the Dock or Finder doesn't get the shell's PATH, so the
/// usual places come first.
pub fn find_cli() -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["opencode.exe", "opencode.cmd"]
    } else {
        &["opencode"]
    };
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::home_dir() {
        for dir in [".opencode/bin", ".local/bin", ".bun/bin", ".npm-global/bin"] {
            dirs.push(home.join(dir));
        }
    }
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from));
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs.iter()
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|candidate| candidate.is_file())
}

/// Whether OpenCode is here: its command, its desktop app on macOS, or the
/// config folder it makes the first time it runs.
pub fn installed() -> bool {
    if find_cli().is_some() || config_dir().is_some_and(|dir| dir.is_dir()) {
        return true;
    }
    #[cfg(target_os = "macos")]
    {
        let mut apps = vec![PathBuf::from("/Applications/OpenCode.app")];
        if let Some(home) = std::env::home_dir() {
            apps.push(home.join("Applications/OpenCode.app"));
        }
        if apps.iter().any(|app| app.is_dir()) {
            return true;
        }
    }
    false
}

/// This program, as a path another program can run. On Windows
/// `canonicalize` gives the verbatim form, `\\?\C:\...`, which most programs
/// won't start, so that prefix comes off.
pub fn this_program() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("finding this program")?;
    Ok(exe.canonicalize().map_or(exe, |c| without_verbatim(&c)))
}

/// A path without Windows' verbatim prefix (`\\?\UNC\server\share` becomes
/// `\\server\share`). A path without one is returned as it was, so this is a
/// no-op elsewhere.
pub fn without_verbatim(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(share) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{share}"));
    }
    match text.strip_prefix(r"\\?\") {
        // Only a drive path: other verbatim forms (`\\?\Volume{...}`) have no
        // plainer spelling, so they're left for the shell to refuse loudly.
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path.to_path_buf(),
    }
}

/// The program and arguments the plugin runs before `hook`: always the full
/// path, since OpenCode's desktop app doesn't get the shell's PATH, and
/// `--db` when the hooks should use a database other than the default.
pub fn hook_command(program: &Path, db: Option<&Path>) -> Vec<String> {
    let mut command = vec![program.to_string_lossy().into_owned()];
    if let Some(db) = db {
        let db = std::path::absolute(db).unwrap_or_else(|_| db.to_path_buf());
        command.push("--db".to_owned());
        command.push(db.to_string_lossy().into_owned());
    }
    command
}

/// The plugin's source, running `command`.
pub fn plugin_source(command: &[String]) -> String {
    let command = serde_json::to_string(command).unwrap_or_else(|_| "[\"omnimem\"]".to_owned());
    PLUGIN_TEMPLATE.replace("__OMNIMEM_COMMAND__", &command)
}

/// What's at the plugin's path, compared with what would be written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginState {
    /// Nothing there.
    Missing,
    /// OmniMem's plugin, exactly as it would be written.
    Current,
    /// OmniMem's plugin, from another version or running another program.
    Outdated,
    /// Some other file of the same name, which is left alone.
    Foreign,
    /// It couldn't be read, for this reason.
    Unreadable(String),
}

pub fn plugin_state(path: &Path, source: &str) -> PluginState {
    match std::fs::read_to_string(path) {
        Ok(text) if text == source => PluginState::Current,
        Ok(text) if text.starts_with(PLUGIN_MARK) => PluginState::Outdated,
        Ok(_) => PluginState::Foreign,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => PluginState::Missing,
        Err(e) => PluginState::Unreadable(e.to_string()),
    }
}

/// Write the plugin, replacing an older one of OmniMem's. Somebody else's
/// file of the same name is refused, not overwritten.
pub fn write_plugin(path: &Path, source: &str) -> Result<PluginState> {
    let before = plugin_state(path, source);
    match &before {
        PluginState::Current => return Ok(before),
        PluginState::Foreign => anyhow::bail!(
            "{} is somebody else's, so it was left alone",
            path.display()
        ),
        PluginState::Unreadable(why) => anyhow::bail!("couldn't read {}: {why}", path.display()),
        PluginState::Missing | PluginState::Outdated => {}
    }
    // Written beside it and renamed into place, so OpenCode, which reloads
    // its config folder when a file changes, never loads half a plugin.
    crate::client_files::replace(path, source, false)?;
    Ok(before)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(test: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("omnimem-opencode-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_plugin_runs_this_program_with_its_database() {
        let command = hook_command(
            Path::new("/opt/OmniMem/omnimem"),
            Some(Path::new("/d/x.db")),
        );
        assert_eq!(command, ["/opt/OmniMem/omnimem", "--db", "/d/x.db"]);
        let source = plugin_source(&command);
        assert!(source.starts_with(PLUGIN_MARK));
        assert!(source.contains(r#"const OMNIMEM = ["/opt/OmniMem/omnimem","--db","/d/x.db"];"#));
        assert!(!source.contains("__OMNIMEM_COMMAND__"));
        // A Windows path survives as a JavaScript string.
        let windows = plugin_source(&[r"C:\Program Files\OmniMem\omnimem.exe".to_owned()]);
        assert!(windows.contains(r#"["C:\\Program Files\\OmniMem\\omnimem.exe"]"#));
    }

    #[test]
    fn the_plugin_has_one_export_for_both_opencode_apis() {
        // OpenCode refuses a plugin file with an export it can't load: 2.x
        // wants a default export with `id` and `setup`, 1.x takes its `server`.
        let source = plugin_source(&["omnimem".to_owned()]);
        let exports: Vec<&str> = source
            .lines()
            .filter(|line| line.starts_with("export "))
            .collect();
        assert_eq!(exports, ["export default {"]);
        for part in [
            "  id: \"omnimem\",",
            "  async setup(ctx) {",
            "  async server({ directory }) {",
        ] {
            assert!(source.contains(part), "{part}");
        }
    }

    #[test]
    fn writing_replaces_our_own_plugin_and_never_somebody_elses() {
        let dir = scratch("write");
        let path = plugin_path(Some(&dir)).unwrap();
        assert_eq!(path, dir.join(".opencode/plugins/omnimem.js"));
        let old = plugin_source(&["/old/omnimem".to_owned()]);
        let new = plugin_source(&["/new/omnimem".to_owned()]);

        assert_eq!(plugin_state(&path, &new), PluginState::Missing);
        assert_eq!(write_plugin(&path, &old).unwrap(), PluginState::Missing);
        assert_eq!(plugin_state(&path, &new), PluginState::Outdated);
        assert_eq!(write_plugin(&path, &new).unwrap(), PluginState::Outdated);
        assert_eq!(plugin_state(&path, &new), PluginState::Current);
        assert_eq!(write_plugin(&path, &new).unwrap(), PluginState::Current);

        std::fs::write(&path, "export const Mine = async () => ({})\n").unwrap();
        assert_eq!(plugin_state(&path, &new), PluginState::Foreign);
        assert!(write_plugin(&path, &new).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "export const Mine = async () => ({})\n"
        );
    }
}
