//! Feed influence (`memory/feed_influence.py`): RSS feeds tied to skill
//! domains with a 1-10 score, which is how many of a feed's latest articles
//! a recompile pulls into the skill's Feed watch section.
//!
//! 6.x mirrored `feeds.yml` into the `meta:feed:influence` hash so every
//! container could read it, and a backup carries that hash across. Reads are
//! lenient: a damaged entry is dropped, never raised, because a broken feed
//! mapping must not take skill compilation down. Writing the mirror belongs
//! to the RSS port (phase 6).

use std::collections::BTreeMap;

use serde_json::Value;
use tracing::warn;

use crate::Engine;
use crate::domains::{is_valid_domain, resolve_domain};
use crate::skills::{FeedLink, INVALID_DOMAIN, py_repr, py_str, py_truthy};

pub const FEED_INFLUENCE_KEY: &str = "meta:feed:influence";
pub const MIN_INFLUENCE: i64 = 1;
pub const MAX_INFLUENCE: i64 = 10;
const MAX_SKILLS_PER_FEED: usize = 20;

/// A feed's `{domain: influence}` mapping, in its original order.
pub type FeedSkills = Vec<(String, i64)>;

/// One mirrored feed entry.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FeedInfluence {
    pub url: String,
    pub topics: Vec<String>,
    pub skills: FeedSkills,
    pub mode: Option<String>,
    pub project: Option<String>,
    pub licence: Option<String>,
    pub licence_note: Option<String>,
}

/// `int(str(score))`
fn py_int(raw: &Value) -> Option<i64> {
    match raw {
        Value::String(_) | Value::Number(_) => py_str(raw).trim().parse::<i64>().ok(),
        _ => None,
    }
}

/// Strict validation where a human supplies the mapping (forms, bundles).
pub fn validate_feed_skills(raw: Option<&Value>) -> std::result::Result<FeedSkills, String> {
    let Some(Value::Object(map)) = raw else {
        return Err("skills must be a mapping of domain to influence score".to_owned());
    };
    if map.len() > MAX_SKILLS_PER_FEED {
        return Err(format!(
            "skills: max {MAX_SKILLS_PER_FEED} domains per feed"
        ));
    }
    let mut validated: FeedSkills = Vec::new();
    for (domain, score) in map {
        let (canonical, _) = resolve_domain(domain);
        if !is_valid_domain(&canonical) {
            return Err(INVALID_DOMAIN.to_owned());
        }
        let Some(value) = py_int(score) else {
            return Err(format!(
                "Influence for '{canonical}' must be a whole number ({MIN_INFLUENCE}-{MAX_INFLUENCE}), got {}",
                py_repr(score)
            ));
        };
        if !(MIN_INFLUENCE..=MAX_INFLUENCE).contains(&value) {
            return Err(format!(
                "Influence for '{canonical}' must be between {MIN_INFLUENCE} and {MAX_INFLUENCE}, got {value}"
            ));
        }
        if validated.iter().any(|(d, _)| *d == canonical) {
            return Err(format!("Domain '{canonical}' appears more than once"));
        }
        validated.push((canonical, value));
    }
    Ok(validated)
}

/// Read-side parsing: invalid entries are dropped.
fn parse_skills_lenient(raw: Option<&Value>) -> FeedSkills {
    let Some(Value::Object(map)) = raw else {
        return Vec::new();
    };
    let mut skills: FeedSkills = Vec::new();
    for (domain, score) in map {
        let (canonical, _) = resolve_domain(domain);
        let value = py_int(score).filter(|_| is_valid_domain(&canonical));
        match value {
            Some(v) if (MIN_INFLUENCE..=MAX_INFLUENCE).contains(&v) => {
                match skills.iter_mut().find(|(d, _)| *d == canonical) {
                    Some(slot) => slot.1 = v,
                    None => skills.push((canonical, v)),
                }
            }
            _ => warn!(domain, score = %score, "dropping invalid feed skill entry"),
        }
    }
    skills
}

fn optional_text(entry: &serde_json::Map<String, Value>, name: &str) -> Option<String> {
    entry.get(name).filter(|v| py_truthy(v)).map(py_str)
}

impl Engine {
    /// The mirrored feed map, by feed name.
    pub fn load_feed_influences(&self) -> BTreeMap<String, FeedInfluence> {
        let raw = match self.store.hash_get_all(FEED_INFLUENCE_KEY) {
            Ok(raw) => raw.unwrap_or_default(),
            Err(e) => {
                warn!(error = %e, "could not read {FEED_INFLUENCE_KEY}");
                return BTreeMap::new();
            }
        };
        let mut influences = BTreeMap::new();
        for (name, value) in raw {
            let Ok(Value::Object(entry)) = serde_json::from_str::<Value>(&value) else {
                warn!(feed = %name, "dropping unreadable feed influence entry");
                continue;
            };
            let Some(url) = entry.get("url").and_then(Value::as_str) else {
                continue;
            };
            let topics = match entry.get("topics") {
                Some(Value::Array(items)) => {
                    items.iter().filter(|t| py_truthy(t)).map(py_str).collect()
                }
                _ => Vec::new(),
            };
            influences.insert(
                name,
                FeedInfluence {
                    url: url.to_owned(),
                    topics,
                    skills: parse_skills_lenient(entry.get("skills")),
                    mode: optional_text(&entry, "mode"),
                    project: optional_text(&entry, "project"),
                    licence: optional_text(&entry, "licence"),
                    licence_note: optional_text(&entry, "licence_note"),
                },
            );
        }
        influences
    }
}

/// Feeds influencing a domain, strongest first, ties by name.
pub fn feeds_for_domain(
    influences: &BTreeMap<String, FeedInfluence>,
    domain: &str,
) -> Vec<FeedLink> {
    let mut matched: Vec<FeedLink> = influences
        .iter()
        .filter_map(|(name, entry)| {
            entry
                .skills
                .iter()
                .find(|(d, _)| d == domain)
                .map(|(_, influence)| FeedLink {
                    feed_name: name.clone(),
                    influence: *influence,
                    url: entry.url.clone(),
                })
        })
        .collect();
    matched.sort_by(|a, b| {
        b.influence
            .cmp(&a.influence)
            .then_with(|| a.feed_name.cmp(&b.feed_name))
    });
    matched
}

/// One `feeds.yml` entry in the mirrored shape (`normalise_feed_entry`), or
/// `None` when it has no usable name and URL. Only a licence that resolves
/// is mirrored: an unrecognised one would travel in skill bundles and fail
/// validation on the receiving side.
pub fn normalise_feed_entry(feed: &Value) -> Option<(String, Value)> {
    let feed = feed.as_object()?;
    let text = |name: &str| {
        feed.get(name)
            .filter(|v| py_truthy(v))
            .map(py_str)
            .unwrap_or_default()
            .trim()
            .to_owned()
    };
    let (name, url) = (text("name"), text("url"));
    if name.is_empty() || url.is_empty() || name.chars().count() > 200 {
        return None;
    }
    let mut entry: BTreeMap<String, Value> = BTreeMap::new();
    entry.insert("url".into(), url.into());
    let topics: Vec<String> = match feed.get("topics") {
        Some(Value::Array(items)) => items.iter().filter(|t| py_truthy(t)).map(py_str).collect(),
        _ => Vec::new(),
    };
    entry.insert("topics".into(), topics.into());
    let skills: serde_json::Map<String, Value> = parse_skills_lenient(feed.get("skills"))
        .into_iter()
        .map(|(d, s)| (d, s.into()))
        .collect();
    entry.insert("skills".into(), Value::Object(skills));
    for optional in ["mode", "project"] {
        if let Some(value) = feed.get(optional).filter(|v| py_truthy(v)) {
            entry.insert(optional.into(), py_str(value).into());
        }
    }
    if let Some(raw) = feed
        .get("licence")
        .filter(|v| !v.is_null() && v.as_str() != Some(""))
    {
        let declared = py_str(raw);
        if crate::classification::resolve_licence(&declared).is_ok() {
            entry.insert("licence".into(), declared.into());
            let note = feed
                .get("licence_note")
                .filter(|v| py_truthy(v))
                .map(py_str)
                .unwrap_or_default()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if !note.is_empty() {
                let capped: String = note
                    .chars()
                    .take(crate::classification::MAX_LICENCE_NOTE)
                    .collect();
                entry.insert("licence_note".into(), capped.into());
            }
        } else {
            warn!(feed = %name, licence = %declared, "unrecognised licence not mirrored");
        }
    }
    Some((name, Value::Object(entry.into_iter().collect())))
}

impl Engine {
    /// Mirror a reading list into `meta:feed:influence`, replacing what was
    /// there, so removed and renamed feeds leave nothing behind.
    pub fn sync_feed_influences(&self, feeds: &[Value]) -> crate::Result<usize> {
        let mirrored: omnimem_store::Fields = feeds
            .iter()
            .filter_map(normalise_feed_entry)
            .map(|(name, entry)| (name, entry.to_string()))
            .collect();
        self.store.kv_delete(FEED_INFLUENCE_KEY)?;
        if !mirrored.is_empty() {
            self.store.hash_set(FEED_INFLUENCE_KEY, &mirrored)?;
        }
        tracing::info!(
            count = mirrored.len(),
            "mirrored feeds into {FEED_INFLUENCE_KEY}"
        );
        Ok(mirrored.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strict_validation_messages_match_6x() {
        assert_eq!(
            validate_feed_skills(Some(&json!({"py": "11"}))).unwrap_err(),
            "Influence for 'python' must be between 1 and 10, got 11"
        );
        assert_eq!(
            validate_feed_skills(Some(&json!({"rust": 2.5}))).unwrap_err(),
            "Influence for 'rust' must be a whole number (1-10), got 2.5"
        );
        assert_eq!(
            validate_feed_skills(Some(&json!({"py": 3, "python": 4}))).unwrap_err(),
            "Domain 'python' appears more than once"
        );
        assert_eq!(
            validate_feed_skills(Some(&json!({"Rust": " 7 "}))).unwrap(),
            [("rust".to_owned(), 7)]
        );
    }

    #[test]
    fn lenient_parsing_drops_damage() {
        assert_eq!(
            parse_skills_lenient(Some(&json!({"go": 3, "bad domain!": 2, "rust": 99}))),
            [("go".to_owned(), 3)]
        );
    }
}
