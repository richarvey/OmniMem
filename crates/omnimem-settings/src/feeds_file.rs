//! The reading list on disk: `feeds.yml`, as a list of feed mappings under
//! `feeds:` (`_load_feeds` and `_save_feeds` in `web_ui/routes/feeds.py`).

use std::path::Path;

use serde_json::{Map, Value, json};

/// Every feed, in file order. A missing file is an empty list, as 6.x read
/// it; a file that doesn't parse is an error, so a save can't wipe it.
pub(crate) fn load(path: &Path) -> Result<Vec<Map<String, Value>>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("could not read {}: {e}", path.display())),
    };
    let config: Value = serde_yaml_ng::from_str(&text)
        .map_err(|e| format!("{} is not valid YAML: {e}", path.display()))?;
    Ok(config
        .get("feeds")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|feed| feed.as_object().cloned())
        .collect())
}

/// Write the list back, keeping each feed's keys in order. The file is
/// written beside the original and renamed over it, so a reader never sees
/// half a file.
pub(crate) fn save(path: &Path, feeds: &[Map<String, Value>]) -> Result<(), String> {
    let text = serde_yaml_ng::to_string(&json!({ "feeds": feeds }))
        .map_err(|e| format!("could not write the reading list: {e}"))?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    let partial = path.with_extension("yml.partial");
    std::fs::write(&partial, text)
        .and_then(|()| std::fs::rename(&partial, path))
        .map_err(|e| format!("could not write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reading_list_round_trips_in_order() {
        let dir = std::env::temp_dir().join(format!("omnimem-feeds-{}", ulid::Ulid::generate()));
        let path = dir.join("feeds.yml");
        assert!(load(&path).unwrap().is_empty(), "a missing file is empty");

        let feed = |name: &str| {
            json!({"url": format!("https://example.com/{name}"), "name": name, "skills": {"rust": 3}})
                .as_object()
                .cloned()
                .unwrap()
        };
        save(&path, &[feed("b"), feed("a")]).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded, vec![feed("b"), feed("a")]);
        assert_eq!(
            loaded[0].keys().collect::<Vec<_>>(),
            ["url", "name", "skills"]
        );

        std::fs::write(&path, "feeds: [").unwrap();
        assert!(load(&path).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
