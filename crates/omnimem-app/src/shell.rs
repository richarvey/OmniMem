//! Command lines for the hooks other agents run: Vibe's (`/bin/sh`, or on
//! Windows Git Bash or PowerShell) and Codex's (`sh -lc`, or on Windows
//! `cmd.exe /C`, which Codex wraps in the outer quotes cmd wants).

use std::path::Path;

/// The command line a hook starts with: this program by its full path (an
/// app's own PATH can't be counted on) and `--db` when the hooks should use a
/// database other than the default. A Windows path is written with forward
/// slashes, which cmd, PowerShell and Git Bash all take.
pub fn hook_base(program: &Path, db: Option<&Path>) -> String {
    let mut line = shell_word(&program.to_string_lossy());
    if let Some(db) = db {
        let db = std::path::absolute(db).unwrap_or_else(|_| db.to_path_buf());
        line.push_str(" --db ");
        line.push_str(&shell_word(&db.to_string_lossy()));
    }
    line
}

/// A word the hook's shell reads back as `word`: bare when it can be, else
/// in single quotes for `sh`, or double quotes on Windows, where cmd and
/// PowerShell have no single-quote escape that Git Bash shares. A Windows
/// path can't hold a `"`, so nothing inside needs escaping there.
pub(crate) fn shell_word(word: &str) -> String {
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
        format!("\"{word}\"")
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}
