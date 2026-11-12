//! What the editors of other programs' settings share: Claude Desktop's and
//! Claude Code's JSON, OpenCode's plugin, and Vibe's and Codex's TOML and
//! hooks. Every one of them keeps a copy of the file as it was and puts the
//! new one in place whole.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use toml_edit::{DocumentMut, Item};

/// What a hooks file holds of OmniMem's hooks, against what would be written.
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

/// One MCP server in a client's settings, as much of it as says where it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServer {
    pub name: String,
    pub url: Option<String>,
    /// A stdio server's program and its arguments.
    pub command: Option<String>,
    pub args: Vec<String>,
    /// The `Authorization` header it sends, as written.
    pub authorization: Option<String>,
    /// Whether it gets its credentials some way that can't be read here, such
    /// as from an environment variable or a helper program.
    pub other_auth: bool,
}

/// The `Authorization` header in a TOML table of headers, as written.
pub fn authorization_in(headers: Option<&Item>) -> Option<String> {
    headers?
        .as_table_like()?
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
        .and_then(|(_, value)| value.as_str())
        .map(str::to_owned)
}

/// Where a file is copied before it's changed.
pub fn backup_path(path: &Path) -> PathBuf {
    with_suffix(path, ".omnimem-backup")
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name: OsString = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Put `text` in the file at `path`: a copy of the file as it was is kept
/// beside it (its path is returned), and the new one is written beside it
/// and renamed into place, so nothing ever reads half a file.
///
/// The new file keeps the old one's permissions. With `private`, because it
/// now holds an access token, it's readable by its owner alone.
///
/// The folder may be a project's, which is somebody else's until it's read:
/// whatever is already at the copy's and the partial file's paths is removed
/// rather than written to, so a symlink left there can't send the old file's
/// text, or the new one's, into another file.
pub fn replace(path: &Path, text: &str, private: bool) -> Result<Option<PathBuf>> {
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let existing = std::fs::metadata(path).ok();
    let backup = match existing {
        Some(_) => {
            let backup = backup_path(path);
            remove_stale(&backup)?;
            std::fs::copy(path, &backup)
                .with_context(|| format!("backing up {}", path.display()))?;
            Some(backup)
        }
        None => None,
    };
    let partial = with_suffix(path, ".omnimem-partial");
    remove_stale(&partial)?;
    let written = write_new(&partial, text, private).and_then(|()| {
        if let Some(existing) = existing.filter(|_| !private) {
            std::fs::set_permissions(&partial, existing.permissions())?;
        }
        std::fs::rename(&partial, path)
    });
    if let Err(e) = written {
        let _ = std::fs::remove_file(&partial);
        return Err(e).with_context(|| format!("writing {}", path.display()));
    }
    Ok(backup)
}

/// Take away whatever is at `path`. A symlink goes itself, not its target.
fn remove_stale(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("clearing {}", path.display())),
    }
}

/// A new file holding `text`, refused if something is already there (a
/// symlink included), and its owner's alone from the start with `private`.
fn write_new(path: &Path, text: &str, private: bool) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // Windows keeps a new file to its owner by the folder's ACL.
    let _ = private;
    let mut file = options.open(path)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()
}

/// The file as TOML: empty when it's missing. A file that isn't TOML is an
/// error, so it's left alone rather than replaced.
pub(crate) fn read_toml(path: &Path) -> Result<DocumentMut> {
    let file = path
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    match std::fs::read_to_string(path) {
        Ok(text) => text
            .parse::<DocumentMut>()
            .map_err(|e| anyhow!("{file} isn't valid TOML ({})", e.message())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(e) => Err(e).with_context(|| format!("couldn't read {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("omnimem-files-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_file_is_replaced_whole_with_a_copy_of_what_it_was() {
        let dir = scratch("replace");
        let path = dir.join("sub/config.toml");
        assert_eq!(replace(&path, "a = 1\n", false).unwrap(), None);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a = 1\n");

        let backup = replace(&path, "a = 2\n", false).unwrap().unwrap();
        assert_eq!(backup, dir.join("sub/config.toml.omnimem-backup"));
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "a = 1\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a = 2\n");
        assert!(!dir.join("sub/config.toml.omnimem-partial").exists());
    }

    #[cfg(unix)]
    #[test]
    fn permissions_are_kept_and_a_token_makes_the_file_its_owners() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let dir = scratch("modes");
        let path = dir.join("config.toml");
        std::fs::write(&path, "a = 1\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        replace(&path, "a = 2\n", false).unwrap();
        assert_eq!(mode(&path), 0o640);
        replace(&path, "token = \"t0k\"\n", true).unwrap();
        assert_eq!(mode(&path), 0o600);
        // The copy of a file that held a token is no more readable than it.
        replace(&path, "a = 3\n", false).unwrap();
        assert_eq!(mode(&backup_path(&path)), 0o600);
        assert_eq!(mode(&path), 0o600);

        let fresh = dir.join("fresh.toml");
        replace(&fresh, "token = \"t0k\"\n", true).unwrap();
        assert_eq!(mode(&fresh), 0o600);
    }

    /// A project can arrive with symlinks where the copy and the partial
    /// file go. Neither is written through.
    #[cfg(unix)]
    #[test]
    fn symlinks_left_beside_the_file_are_not_written_through() {
        let dir = scratch("symlinks");
        let victim = dir.join("victim");
        std::fs::write(&victim, "mine\n").unwrap();
        let path = dir.join("hooks.toml");
        std::fs::write(&path, "theirs = \"\"\"\nrun me\n\"\"\"\n").unwrap();
        for suffix in [".omnimem-backup", ".omnimem-partial"] {
            std::os::unix::fs::symlink(&victim, with_suffix(&path, suffix)).unwrap();
        }
        replace(&path, "ours = 1\n", false).unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "mine\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "ours = 1\n");
        let backup = backup_path(&path);
        assert!(!backup.symlink_metadata().unwrap().is_symlink());
        assert!(std::fs::read_to_string(&backup).unwrap().contains("run me"));
    }
}
