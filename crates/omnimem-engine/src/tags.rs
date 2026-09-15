//! Tag validation and editing (`memory/tags.py`).

use serde_json::{Value, json};

use crate::error::invalid;
use crate::pyfmt::{now_str, py_json};
use crate::{Engine, Result};

pub const MAX_TAGS: usize = 20;
pub const MAX_TAG_LENGTH: usize = 100;

const RETAGGABLE_NAMESPACES: [&str; 4] = ["episodic", "knowledge", "preference", "project"];

pub fn validate_tags(tags: Option<&[String]>) -> Result<()> {
    let Some(tags) = tags else { return Ok(()) };
    if tags.len() > MAX_TAGS {
        return Err(invalid(format!(
            "Too many tags ({}). Maximum is {MAX_TAGS}.",
            tags.len()
        )));
    }
    if tags.iter().any(|t| t.chars().count() > MAX_TAG_LENGTH) {
        return Err(invalid(format!(
            "Each tag must be a string of at most {MAX_TAG_LENGTH} characters"
        )));
    }
    Ok(())
}

/// The stored JSON tags field, tolerating missing or malformed data.
pub fn parse_tags_field(raw: Option<&str>) -> Vec<String> {
    let Some(raw) = raw.filter(|r| !r.is_empty()) else {
        return Vec::new();
    };
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Array(items)) => items
            .into_iter()
            .map(|v| match v {
                Value::String(s) => s,
                other => other.to_string(),
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Strip whitespace, drop empties, dedupe preserving order.
fn clean(tags: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tag in tags {
        let t = tag.trim();
        if !t.is_empty() && !out.iter().any(|o| o == t) {
            out.push(t.to_owned());
        }
    }
    out
}

impl Engine {
    /// `retag`: replace the tags, or add and remove, without re-embedding.
    pub fn retag(
        &self,
        key: &str,
        tags: Option<Vec<String>>,
        add: Option<Vec<String>>,
        remove: Option<Vec<String>>,
    ) -> Result<Value> {
        let adding = add.as_ref().is_some_and(|a| !a.is_empty());
        let removing = remove.as_ref().is_some_and(|r| !r.is_empty());
        if tags.is_some() && (adding || removing) {
            return Err(invalid(
                "Pass either tags (full replacement) or add/remove, not both",
            ));
        }
        if tags.is_none() && !adding && !removing {
            return Err(invalid("Nothing to do — pass tags, add, or remove"));
        }
        let parts: Vec<&str> = key.split(':').collect();
        if !key.starts_with("mem:") || parts.len() < 3 {
            return Err(invalid(format!(
                "Invalid memory key: {}",
                crate::pyfmt::take_chars(key, 50)
            )));
        }
        let namespace = parts[1];
        if !RETAGGABLE_NAMESPACES.contains(&namespace) {
            return Err(invalid(format!(
                "Cannot retag '{namespace}' entries. Only {} memories carry editable tags.",
                RETAGGABLE_NAMESPACES.join(", ")
            )));
        }
        validate_tags(tags.as_deref())?;
        validate_tags(add.as_deref())?;
        validate_tags(remove.as_deref())?;

        let Some(data) = self.store.get(key)? else {
            return Ok(json!({"status": "not_found", "key": key}));
        };
        let current = parse_tags_field(data.get("tags").map(String::as_str));
        let new_tags = match &tags {
            Some(tags) => clean(tags),
            None => {
                let mut next = current.clone();
                if let Some(remove) = &remove {
                    let gone: Vec<&str> = remove.iter().map(|t| t.trim()).collect();
                    next.retain(|t| !gone.contains(&t.as_str()));
                }
                if let Some(add) = &add {
                    next.extend(add.iter().cloned());
                    next = clean(&next);
                }
                next
            }
        };
        validate_tags(Some(&new_tags))?;
        if new_tags == current {
            return Ok(json!({"status": "unchanged", "key": key, "tags": new_tags}));
        }
        let fields = omnimem_store::Fields::from([
            ("tags".to_owned(), py_json(&json!(new_tags))),
            ("updated_at".to_owned(), now_str()),
        ]);
        self.store.set_fields(key, &fields)?;
        Ok(json!({
            "status": "updated",
            "key": key,
            "tags": new_tags,
            "previous_tags": current,
        }))
    }
}
