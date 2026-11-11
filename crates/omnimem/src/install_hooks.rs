//! `omnimem install-hooks`: put the three Claude Code hooks from
//! docs/claude-code-hook.md into a settings file, leaving alone any event
//! that already has one. With `--opencode`, write OmniMem's OpenCode plugin
//! instead (guides/opencode.md), which runs the same hooks, and with
//! `--vibe`, put them in Mistral Vibe's `hooks.toml` (guides/vibe.md).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use omnimem_app::opencode::{self, PluginState, without_verbatim};
use omnimem_app::vibe::{self, HooksState};
use serde_json::{Map, Value, json};

/// The hooks, as the guide gives them: event, matcher, `hook` argument and
/// timeout in seconds. SessionEnd's timeout is not optional: Claude Code's
/// default 1.5s budget is less than loading the embedding model takes.
const HOOKS: &[(&str, Option<&str>, Option<&str>, u64)] = &[
    ("SessionStart", None, Some("session-start"), 15),
    ("PreToolUse", Some("Edit|MultiEdit|Write|Bash"), None, 15),
    ("SessionEnd", None, Some("session-end"), 30),
];

/// What happened to one event's hook.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Added(String),
    /// Already there, with the command it already has.
    Present(String),
}

/// Which agent the hooks are for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    ClaudeCode,
    OpenCode,
    Vibe,
}

pub(crate) fn run(db: Option<&Path>, project: bool, dry_run: bool, target: Target) -> Result<()> {
    if std::env::var_os("FLATPAK_ID").is_some_and(|id| !id.is_empty()) {
        bail!(match target {
            Target::OpenCode =>
                "the Flatpak's sandbox can't reach OpenCode's config folder, and OpenCode outside \
                 it can't run the Flatpak's omnimem. Install OmniMem's package to use the \
                 OpenCode hooks",
            Target::Vibe =>
                "the Flatpak's sandbox can't reach Vibe's folder, and Vibe outside it can't run \
                 the Flatpak's omnimem. Install OmniMem's package to use the Vibe hooks",
            Target::ClaudeCode =>
                "the Flatpak's sandbox can't reach Claude Code's settings. Add the hooks by hand: \
                 see docs/claude-code-hook.md",
        });
    }
    match target {
        Target::OpenCode => return run_opencode(db, project, dry_run),
        Target::Vibe => return run_vibe(db, project, dry_run),
        Target::ClaudeCode => {}
    }
    let path = if project {
        std::env::current_dir()?
            .join(".claude")
            .join("settings.json")
    } else {
        std::env::home_dir()
            .ok_or_else(|| anyhow!("no home folder to find Claude Code's settings in"))?
            .join(".claude")
            .join("settings.json")
    };
    let base = base_command(db)?;
    let mut settings = read_settings(&path)?;
    let outcomes = merge(&mut settings, &base)?;

    println!("{}", path.display());
    for ((event, ..), outcome) in HOOKS.iter().zip(&outcomes) {
        match outcome {
            Outcome::Added(command) => println!(
                "  {:<9} {event:<13} {command}",
                if dry_run { "would add" } else { "added" }
            ),
            Outcome::Present(command) => println!("  {:<9} {event:<13} {command}", "present"),
        }
    }
    if !outcomes.iter().any(|o| matches!(o, Outcome::Added(_))) {
        println!("Every hook is already there; nothing changed.");
        return Ok(());
    }
    if dry_run {
        println!("Nothing written (--dry-run).");
        return Ok(());
    }
    write_settings(&path, &settings)?;
    println!("Claude Code uses them from its next session. See docs/claude-code-hook.md.");
    Ok(())
}

/// Write the OpenCode plugin, globally or for the project in this folder.
fn run_opencode(db: Option<&Path>, project: bool, dry_run: bool) -> Result<()> {
    let here = std::env::current_dir()?;
    let path = opencode::plugin_path(project.then_some(here.as_path()))
        .ok_or_else(|| anyhow!("no home folder to find OpenCode's config in"))?;
    let source = opencode::plugin_source(&opencode::hook_command(&opencode::this_program()?, db));
    let state = opencode::plugin_state(&path, &source);
    println!("{}", path.display());
    let (verb, done) = match &state {
        PluginState::Current => {
            println!("The OmniMem plugin is already there; nothing changed.");
            return Ok(());
        }
        PluginState::Foreign => bail!(
            "{} isn't OmniMem's plugin, so it was left alone",
            path.display()
        ),
        PluginState::Unreadable(why) => bail!("couldn't read {}: {why}", path.display()),
        PluginState::Missing => ("add", "added"),
        PluginState::Outdated => ("update", "updated"),
    };
    if dry_run {
        println!("  would {verb} the OmniMem plugin\nNothing written (--dry-run).");
        return Ok(());
    }
    opencode::write_plugin(&path, &source)?;
    println!(
        "  {done} the OmniMem plugin\nOpenCode loads it from its next start. See guides/opencode.md."
    );
    Ok(())
}

/// Put OmniMem's hooks in Vibe's hooks.toml, globally or for the project in
/// this folder.
fn run_vibe(db: Option<&Path>, project: bool, dry_run: bool) -> Result<()> {
    let here = std::env::current_dir()?;
    let path = vibe::hooks_path(project.then_some(here.as_path()))
        .ok_or_else(|| anyhow!("no home folder to find Vibe's in"))?;
    let base = vibe::hook_base(&opencode::this_program()?, db);
    println!("{}", path.display());
    let verb = match vibe::hooks_state(&path, &base) {
        HooksState::Current => {
            println!("OmniMem's hooks are already there; nothing changed.");
            return Ok(());
        }
        HooksState::Unreadable(why) => bail!("couldn't use {}: {why}", path.display()),
        HooksState::Missing => {
            if dry_run {
                "would add"
            } else {
                "added"
            }
        }
        HooksState::Outdated => {
            if dry_run {
                "would update"
            } else {
                "updated"
            }
        }
    };
    for hook in &vibe::HOOKS {
        println!(
            "  {verb:<12} {:<14} {}",
            hook.kind,
            vibe::hook_command(&base, hook)
        );
    }
    if dry_run {
        println!("Nothing written (--dry-run).");
        return Ok(());
    }
    vibe::write_hooks(&path, &base)?;
    if project {
        println!("Vibe reads them once this folder is trusted.");
    }
    println!(
        "Vibe uses them from its next session (the briefing needs Vibe 3.1.1 or later). \
         See guides/vibe.md."
    );
    Ok(())
}

/// How a hook runs this binary: `omnimem` when that's what the PATH finds,
/// otherwise its full path (the macOS app's binary is inside its bundle,
/// and a development build isn't on the PATH at all). With `--db`, the hooks
/// keep using that database.
fn base_command(db: Option<&Path>) -> Result<String> {
    let exe = std::env::current_exe().context("finding this program")?;
    let exe = exe.canonicalize().map_or(exe, |c| without_verbatim(&c));
    let on_path = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(format!("omnimem{}", std::env::consts::EXE_SUFFIX)))
            .find(|candidate| candidate.is_file())
    });
    let mut command =
        match on_path.and_then(|found| found.canonicalize().ok().map(|c| without_verbatim(&c))) {
            Some(found) if found == exe => "omnimem".to_owned(),
            _ => quoted(&exe.to_string_lossy()),
        };
    if let Some(db) = db {
        let db = std::path::absolute(db).unwrap_or_else(|_| db.to_path_buf());
        command.push_str(" --db ");
        command.push_str(&quoted(&db.to_string_lossy()));
    }
    Ok(command)
}

/// Double-quoted for the shell Claude Code runs hooks in, when it needs to be.
/// A backslash always does: bare, the shell would drop it.
fn quoted(arg: &str) -> String {
    if !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/:._-+".contains(c))
    {
        arg.to_owned()
    } else {
        format!("\"{}\"", arg.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

/// Add each event's hook unless the event already runs an omnimem hook, in
/// whatever form. Everything else in the settings is kept as it was.
fn merge(settings: &mut Value, base: &str) -> Result<Vec<Outcome>> {
    let hooks = settings
        .as_object_mut()
        .ok_or_else(|| anyhow!("the settings file isn't a JSON object"))?
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow!("\"hooks\" in the settings file isn't a JSON object"))?;
    let mut outcomes = Vec::new();
    for &(event, matcher, argument, timeout) in HOOKS {
        let groups = hooks
            .entry(event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| anyhow!("hooks.{event} in the settings file isn't a list"))?;
        if let Some(existing) = groups.iter().find_map(omnimem_command) {
            outcomes.push(Outcome::Present(existing));
            continue;
        }
        let command = match argument {
            Some(argument) => format!("{base} hook {argument}"),
            None => format!("{base} hook"),
        };
        let mut group = Map::new();
        if let Some(matcher) = matcher {
            group.insert("matcher".into(), json!(matcher));
        }
        group.insert(
            "hooks".into(),
            json!([{ "type": "command", "command": command, "timeout": timeout }]),
        );
        groups.push(Value::Object(group));
        outcomes.push(Outcome::Added(command));
    }
    Ok(outcomes)
}

/// The command of an omnimem hook in one of an event's groups, if any.
fn omnimem_command(group: &Value) -> Option<String> {
    group
        .get("hooks")?
        .as_array()?
        .iter()
        .filter_map(|hook| hook.get("command")?.as_str())
        .find(|command| command.contains("omnimem") && command.contains(" hook"))
        .map(str::to_owned)
}

/// The settings as JSON: an empty object when the file is missing or empty.
/// A file that isn't valid JSON is left alone rather than replaced.
fn read_settings(path: &Path) -> Result<Value> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(json!({})),
        Ok(text) => serde_json::from_str(&text)
            .with_context(|| format!("{} isn't valid JSON, so it was left alone", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Keep a copy of the file as it was, then write beside it and rename into
/// place, so Claude Code never reads half a file.
fn write_settings(path: &Path, settings: &Value) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    if path.exists() {
        let backup = with_suffix(path, ".omnimem-backup");
        std::fs::copy(path, &backup).with_context(|| format!("backing up {}", path.display()))?;
        println!("The file as it was is in {}", backup.display());
    }
    let partial = with_suffix(path, ".omnimem-partial");
    std::fs::write(&partial, serde_json::to_string_pretty(settings)? + "\n")
        .with_context(|| format!("writing {}", partial.display()))?;
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

    #[test]
    fn an_empty_settings_file_gets_all_three() {
        let mut settings = json!({});
        let outcomes = merge(&mut settings, "omnimem").unwrap();
        assert_eq!(
            outcomes,
            [
                Outcome::Added("omnimem hook session-start".into()),
                Outcome::Added("omnimem hook".into()),
                Outcome::Added("omnimem hook session-end".into()),
            ]
        );
        assert_eq!(
            settings["hooks"]["PreToolUse"][0]["matcher"],
            "Edit|MultiEdit|Write|Bash"
        );
        assert_eq!(
            settings["hooks"]["SessionEnd"][0]["hooks"][0]["timeout"],
            30
        );
        assert_eq!(settings["hooks"]["SessionStart"][0].get("matcher"), None);
    }

    #[test]
    fn existing_hooks_and_settings_are_kept() {
        let mut settings = json!({
            "model": "opus",
            "hooks": {
                "PreToolUse": [
                    { "matcher": "Bash", "hooks": [{ "type": "command", "command": "lint.sh" }] },
                    { "hooks": [{ "type": "command", "command": "/opt/omnimem/omnimem --db /x.db hook" }] }
                ],
                "Stop": [{ "hooks": [{ "type": "command", "command": "notify.sh" }] }]
            }
        });
        let outcomes = merge(&mut settings, "omnimem").unwrap();
        assert_eq!(
            outcomes[1],
            Outcome::Present("/opt/omnimem/omnimem --db /x.db hook".into())
        );
        assert!(matches!(outcomes[0], Outcome::Added(_)));
        assert_eq!(settings["model"], "opus");
        assert_eq!(settings["hooks"]["PreToolUse"].as_array().unwrap().len(), 2);
        assert_eq!(
            settings["hooks"]["Stop"][0]["hooks"][0]["command"],
            "notify.sh"
        );

        // A second run finds everything and changes nothing.
        let before = settings.clone();
        let again = merge(&mut settings, "omnimem").unwrap();
        assert!(again.iter().all(|o| matches!(o, Outcome::Present(_))));
        assert_eq!(settings, before);
    }

    #[test]
    fn settings_of_the_wrong_shape_are_refused() {
        assert!(merge(&mut json!([]), "omnimem").is_err());
        assert!(merge(&mut json!({ "hooks": [] }), "omnimem").is_err());
        assert!(merge(&mut json!({ "hooks": { "SessionStart": {} } }), "omnimem").is_err());
    }

    #[test]
    fn a_windows_verbatim_path_loses_its_prefix() {
        let plain = |raw: &str| {
            without_verbatim(Path::new(raw))
                .to_string_lossy()
                .into_owned()
        };
        assert_eq!(
            plain(r"\\?\C:\Users\ric\AppData\Local\Programs\OmniMem\omnimem.exe"),
            r"C:\Users\ric\AppData\Local\Programs\OmniMem\omnimem.exe"
        );
        assert_eq!(
            plain(r"\\?\UNC\nas\tools\omnimem.exe"),
            r"\\nas\tools\omnimem.exe"
        );
        // Nothing to take off, or no plainer spelling: left as it was.
        assert_eq!(plain("/usr/bin/omnimem"), "/usr/bin/omnimem");
        assert_eq!(plain(r"C:\bin\omnimem.exe"), r"C:\bin\omnimem.exe");
        assert_eq!(
            plain(r"\\?\Volume{1234}\omnimem.exe"),
            r"\\?\Volume{1234}\omnimem.exe"
        );
        // And what lands in settings.json is a command a shell can run.
        assert_eq!(
            quoted(&plain(r"\\?\C:\Program Files\OmniMem\omnimem.exe")),
            "\"C:\\\\Program Files\\\\OmniMem\\\\omnimem.exe\""
        );
    }

    #[test]
    fn paths_are_quoted_only_when_they_need_it() {
        assert_eq!(quoted("/usr/bin/omnimem"), "/usr/bin/omnimem");
        assert_eq!(quoted("/Users/a b/omnimem.db"), "\"/Users/a b/omnimem.db\"");
        assert_eq!(
            quoted("C:\\Program Files\\omnimem.exe"),
            "\"C:\\\\Program Files\\\\omnimem.exe\""
        );
        assert_eq!(
            quoted("C:\\bin\\omnimem.exe"),
            "\"C:\\\\bin\\\\omnimem.exe\""
        );
    }
}
