//! Settings lookup for every crate that reads the environment.
//!
//! The headless server is configured as 6.x was, with environment variables.
//! The desktop app has no shell to set them in, so at start it installs an
//! overlay built from `omnimem.env` in its data folder and the secrets it
//! keeps in the OS keychain. A real environment variable always wins, so an
//! override from the shell still works.
//!
//! The overlay is installed once, before any service reads a setting, and
//! never changes afterwards: settings apply at the next start, as environment
//! variables always did. Nothing here writes to the process environment,
//! which isn't sound once other threads are running (reading the keychain
//! can start some).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::OnceLock;

static OVERLAY: OnceLock<BTreeMap<String, String>> = OnceLock::new();

/// Install the overlay. Returns false, changing nothing, if one is already
/// installed.
pub fn install_overlay(values: BTreeMap<String, String>) -> bool {
    OVERLAY.set(values).is_ok()
}

/// A setting: the environment variable when it is set, otherwise the
/// overlay's value. Like `std::env::var(..).ok()`, a value that isn't valid
/// UTF-8 reads as unset.
pub fn var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .or_else(|| OVERLAY.get().and_then(|overlay| overlay.get(name).cloned()))
}

/// Where a setting's value comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Environment,
    Overlay,
    Unset,
}

pub fn source(name: &str) -> Source {
    if std::env::var_os(name).is_some() {
        Source::Environment
    } else if OVERLAY
        .get()
        .is_some_and(|overlay| overlay.contains_key(name))
    {
        Source::Overlay
    } else {
        Source::Unset
    }
}

/// True for a name an environment file can hold: `A-Z`, digits and `_`,
/// not starting with a digit.
pub fn is_setting_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_uppercase() || c == '_')
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn unquote(raw: &str) -> String {
    let raw = raw.trim();
    if raw.len() >= 2 && raw.starts_with('\'') && raw.ends_with('\'') {
        return raw[1..raw.len() - 1].to_owned();
    }
    if raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"') {
        let mut out = String::new();
        let mut chars = raw[1..raw.len() - 1].chars();
        while let Some(c) = chars.next() {
            match (c, chars.clone().next()) {
                ('\\', Some(next @ ('"' | '\\'))) => {
                    out.push(next);
                    chars.next();
                }
                (c, _) => out.push(c),
            }
        }
        return out;
    }
    raw.to_owned()
}

/// Read `KEY=value` lines, as a systemd `EnvironmentFile` or a `.env` file
/// holds them.
///
/// Blank lines, `#` comments, an `export ` prefix and names that aren't
/// setting names are skipped; values may be single- or double-quoted; a
/// later line wins.
pub fn parse_settings(text: &str) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if is_setting_name(name) {
            values.insert(name.to_owned(), unquote(value));
        }
    }
    values
}

/// Write settings back as `KEY=value` lines under a header, sorted by name.
/// A value with spaces, quotes, `#` or a backslash is double-quoted.
pub fn render_settings(values: &BTreeMap<String, String>) -> String {
    let mut out = String::from(
        "# OmniMem settings, written by the settings panel.\n\
         # An environment variable of the same name takes precedence.\n",
    );
    for (name, value) in values {
        // The file is read a line at a time, so a value holding a line break
        // would come back as two settings. Control characters are dropped
        // here rather than trusted to every caller's validation.
        let cleaned: String = value.chars().filter(|c| !c.is_control()).collect();
        let value = &cleaned;
        let plain = !value.is_empty()
            && value
                .chars()
                .all(|c| !c.is_whitespace() && !matches!(c, '"' | '\'' | '#' | '\\'));
        // Writing into a String cannot fail, so the results are dropped.
        if plain {
            let _ = writeln!(out, "{name}={value}");
        } else {
            let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
            let _ = writeln!(out, "{name}=\"{escaped}\"");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_files_round_trip() {
        let values = BTreeMap::from([
            ("MCP_PORT".to_owned(), "8765".to_owned()),
            ("OMNIMEM_USER".to_owned(), "ric".to_owned()),
            (
                "MCP_ALLOWED_ORIGINS".to_owned(),
                "https://a.example, https://b.example".to_owned(),
            ),
            (
                "TRICKY".to_owned(),
                r#"say "hi" \ # not a comment"#.to_owned(),
            ),
            ("EMPTY".to_owned(), String::new()),
        ]);
        let text = render_settings(&values);
        assert!(text.contains("MCP_PORT=8765\n"), "{text}");
        assert_eq!(parse_settings(&text), values);
    }

    #[test]
    fn hand_written_files_are_read_leniently() {
        let text = "\n# comment\nexport RSS_SCHEDULE_HOURS = 6\nlower=skipped\nINGEST_MODE='raw'\nnot a setting\nINGEST_MODE=full\n";
        let values = parse_settings(text);
        assert_eq!(
            values.get("RSS_SCHEDULE_HOURS").map(String::as_str),
            Some("6")
        );
        assert_eq!(
            values.get("INGEST_MODE").map(String::as_str),
            Some("full"),
            "a later line wins"
        );
        assert_eq!(values.len(), 2);
        assert!(!is_setting_name("9LIVES"));
        assert!(is_setting_name("_PRIVATE_1"));
    }

    #[test]
    fn the_environment_wins_over_the_overlay() {
        let overlay = BTreeMap::from([
            ("PATH".to_owned(), "from the overlay".to_owned()),
            (
                "OMNIMEM_OVERLAY_TEST_ONLY".to_owned(),
                "overlay value".to_owned(),
            ),
        ]);
        assert!(install_overlay(overlay));
        assert!(!install_overlay(BTreeMap::new()), "installed once");
        assert_ne!(var("PATH").as_deref(), Some("from the overlay"));
        assert_eq!(source("PATH"), Source::Environment);
        assert_eq!(
            var("OMNIMEM_OVERLAY_TEST_ONLY").as_deref(),
            Some("overlay value")
        );
        assert_eq!(source("OMNIMEM_OVERLAY_TEST_ONLY"), Source::Overlay);
        assert_eq!(source("OMNIMEM_NEVER_SET_ANYWHERE"), Source::Unset);
    }
}
