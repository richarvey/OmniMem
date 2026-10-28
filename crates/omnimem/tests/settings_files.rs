//! The settings files people start from list every setting the configuration
//! reference documents, so nothing has to be looked up to be found.

use std::path::Path;

fn repo_file(path: &str) -> String {
    let full = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path);
    std::fs::read_to_string(&full).unwrap_or_else(|e| panic!("reading {}: {e}", full.display()))
}

/// The names in the first column of docs/configuration.md's tables, up to
/// the settings that are gone since 6.x.
fn documented_settings() -> Vec<String> {
    let docs = repo_file("docs/configuration.md");
    let current = docs.split("## Gone since 6.x").next().unwrap();
    let mut names = Vec::new();
    for line in current.lines() {
        let Some(first_cell) = line.strip_prefix("| `") else {
            continue;
        };
        let first_cell = first_cell.split(" |").next().unwrap();
        for name in first_cell.split(", ") {
            let name = name.trim_matches('`');
            // Names the settings file, so it can't be set inside one.
            if name != "OMNIMEM_ENV_FILE" {
                names.push(name.to_owned());
            }
        }
    }
    assert!(names.len() > 50, "found only {names:?}");
    names
}

#[test]
fn the_settings_files_list_every_documented_setting() {
    for file in ["packaging/linux/omnimem.env", ".env.example"] {
        let text = repo_file(file);
        let missing: Vec<_> = documented_settings()
            .into_iter()
            .filter(|name| {
                !text.lines().any(|line| {
                    line.trim_start_matches('#')
                        .starts_with(&format!("{name}="))
                })
            })
            .collect();
        assert!(missing.is_empty(), "{file} doesn't list {missing:?}");
    }
}

#[test]
fn the_settings_files_parse_to_what_they_show() {
    // Every line that sets something sets exactly what it says, comments
    // and all: the file the package installs is read by omnimem_core::env.
    let values = omnimem_core::env::parse_settings(&repo_file("packaging/linux/omnimem.env"));
    let get = |name: &str| values.get(name).map(String::as_str);
    assert_eq!(get("OMNIMEM_DB"), Some("/var/lib/omnimem/omnimem.db"));
    assert_eq!(get("HF_HOME"), Some("/var/lib/omnimem/huggingface"));
    assert_eq!(get("MCP_HOST"), Some("127.0.0.1"));
    assert_eq!(get("MCP_PORT"), Some("8765"));
    assert_eq!(values.len(), 4, "{values:?}");
}
