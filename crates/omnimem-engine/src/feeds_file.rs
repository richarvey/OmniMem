//! The reading list on disk: `feeds.yml`, as a list of feed mappings under
//! `feeds:` (`_load_feeds` and `_save_feeds` in `web_ui/routes/feeds.py`),
//! the checks a feed from outside has to pass, and the merge a backup's
//! feeds go through on restore.

use std::path::Path;

use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::classification::{resolve_licence, validate_licence_note};
use crate::feeds::validate_feed_skills;

pub type Feed = Map<String, Value>;

/// Feeds taken from one file someone hands us: an upload or a backup.
pub const MAX_IMPORTED_FEEDS: usize = 500;

/// Every feed, in file order. A missing file is an empty list, as 6.x read
/// it; a file that doesn't parse is an error, so a save can't wipe it.
pub fn load(path: &Path) -> Result<Vec<Feed>, String> {
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
pub fn save(path: &Path, feeds: &[Feed]) -> Result<(), String> {
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

/// True for an `http:` or `https:` URL. Anything else a feed or an article
/// names is shown as text rather than linked, and the desktop app only hands
/// these schemes to the system browser.
pub fn is_web_url(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    let web = scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https");
    web && !rest.is_empty() && !url.chars().any(char::is_control)
}

/// `str(value)` for a YAML scalar.
pub fn scalar(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn text(feed: &Feed, field: &str) -> String {
    feed.get(field).map(scalar).unwrap_or_default()
}

/// A feed from outside (an uploaded `feeds.yml` or a backup) with the form's
/// rules applied: a web URL, a name of at most 200 characters, a licence the
/// classifier knows, a note within its cap, and a skills mapping that
/// validates. Only the keys the reading list uses are kept, so a file can't
/// smuggle other YAML into the list.
pub fn validate_feed(feed: &Feed) -> Result<Feed, String> {
    let url = text(feed, "url");
    if !is_web_url(url.trim()) {
        return Err("url must start with http:// or https://".to_owned());
    }
    let mut out = Feed::new();
    out.insert("url".into(), Value::String(url.trim().to_owned()));
    let name = text(feed, "name");
    if name.chars().count() > 200 {
        return Err("name must be at most 200 characters".to_owned());
    }
    if !name.trim().is_empty() {
        out.insert("name".into(), Value::String(name.trim().to_owned()));
    }
    if let Some(topics) = feed.get("topics") {
        let topics: Vec<Value> = topics
            .as_array()
            .ok_or_else(|| "topics must be a list".to_owned())?
            .iter()
            .take(50)
            .map(|t| Value::String(scalar(t).chars().take(200).collect()))
            .collect();
        out.insert("topics".into(), Value::Array(topics));
    }
    match feed.get("mode").map(scalar) {
        None => {}
        Some(mode) if mode == "summary" || mode == "digest" => {
            out.insert("mode".into(), Value::String(mode));
        }
        Some(_) => return Err("mode must be summary or digest".to_owned()),
    }
    if let Some(project) = feed.get("project") {
        let project = scalar(project);
        if project.chars().count() > 200 {
            return Err("project must be at most 200 characters".to_owned());
        }
        out.insert("project".into(), Value::String(project));
    }
    if let Some(licence) = feed.get("licence") {
        let raw = scalar(licence);
        resolve_licence(&raw).map_err(|e| e.to_string())?;
        out.insert("licence".into(), Value::String(raw));
    }
    let note = text(feed, "licence_note");
    if !note.trim().is_empty() {
        validate_licence_note(Some(&note)).map_err(|e| e.to_string())?;
        out.insert("licence_note".into(), Value::String(note.trim().to_owned()));
    }
    if let Some(skills) = feed.get("skills") {
        let skills = validate_feed_skills(Some(skills))?;
        let mapping: Map<String, Value> = skills
            .into_iter()
            .map(|(domain, score)| (domain, Value::from(score)))
            .collect();
        if !mapping.is_empty() {
            out.insert("skills".into(), Value::Object(mapping));
        }
    }
    Ok(out)
}

/// What merging a backup's feeds into the reading list did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FeedMerge {
    /// Feeds the backup carried.
    pub in_backup: usize,
    /// New to the reading list, so added to its end.
    pub added: usize,
    /// Already on the list by URL, so left exactly as they were.
    pub already_present: usize,
    /// Failed the checks an uploaded feed gets, or past the cap.
    pub invalid: usize,
}

/// Add the backup's feeds to `existing`, matched by URL. A feed the list
/// already has is left alone, settings and all, as a restored memory never
/// overwrites a newer one; a feed it doesn't have is checked as an upload
/// would be and added at the end. Returns the new list and what happened.
pub fn merge(existing: &[Feed], incoming: &[Value]) -> (Vec<Feed>, FeedMerge) {
    let mut merged = existing.to_vec();
    let mut report = FeedMerge {
        in_backup: incoming.len(),
        ..FeedMerge::default()
    };
    let mut known: std::collections::HashSet<String> = existing
        .iter()
        .map(|feed| text(feed, "url").trim().to_owned())
        .collect();
    for (index, feed) in incoming.iter().enumerate() {
        let checked = (index < MAX_IMPORTED_FEEDS)
            .then_some(feed)
            .and_then(Value::as_object)
            .and_then(|feed| validate_feed(feed).ok());
        let Some(feed) = checked else {
            report.invalid += 1;
            continue;
        };
        if known.insert(text(&feed, "url")) {
            merged.push(feed);
            report.added += 1;
        } else {
            report.already_present += 1;
        }
    }
    (merged, report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(value: &Value) -> Feed {
        value.as_object().cloned().unwrap()
    }

    #[test]
    fn the_reading_list_round_trips_in_order() {
        let dir = std::env::temp_dir().join(format!("omnimem-feeds-{}", ulid::Ulid::generate()));
        let path = dir.join("feeds.yml");
        assert!(load(&path).unwrap().is_empty(), "a missing file is empty");

        let named = |name: &str| {
            feed(
                &json!({"url": format!("https://example.com/{name}"), "name": name, "skills": {"rust": 3}}),
            )
        };
        save(&path, &[named("b"), named("a")]).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded, vec![named("b"), named("a")]);
        assert_eq!(
            loaded[0].keys().collect::<Vec<_>>(),
            ["url", "name", "skills"]
        );

        std::fs::write(&path, "feeds: [").unwrap();
        assert!(load(&path).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_merge_adds_what_is_new_and_leaves_what_is_there() {
        let existing = vec![feed(
            &json!({"url": "https://a.example/rss", "name": "Mine", "mode": "digest"}),
        )];
        let incoming = vec![
            json!({"url": " https://a.example/rss ", "name": "Theirs", "mode": "summary"}),
            json!({"url": "https://b.example/rss", "name": "New", "extra": "dropped"}),
            json!({"url": "https://b.example/rss", "name": "New again"}),
            json!({"url": "ftp://c.example/rss"}),
            json!("not a mapping"),
        ];
        let (merged, report) = merge(&existing, &incoming);
        assert_eq!(
            report,
            FeedMerge {
                in_backup: 5,
                added: 1,
                already_present: 2,
                invalid: 2,
            }
        );
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0], existing[0], "a feed already listed is untouched");
        assert_eq!(
            Value::Object(merged[1].clone()),
            json!({"url": "https://b.example/rss", "name": "New"}),
            "an added feed keeps only the keys the list uses"
        );
    }

    #[test]
    fn an_empty_or_missing_list_merges_to_nothing_new() {
        let existing = vec![feed(&json!({"url": "https://a.example/rss"}))];
        let (merged, report) = merge(&existing, &[]);
        assert_eq!(merged, existing);
        assert_eq!(report, FeedMerge::default());
    }
}
