//! `recent_knowledge` (`tools/knowledge.py`). `promote_knowledge` arrives
//! with the skill compiler in phase 4.

use omnimem_core::Namespace;
use omnimem_core::classification::LICENCE_CLASSES;
use omnimem_store::MemoryFilter;
use serde_json::{Map, Value};

use crate::classification::{classification_fields, effective_licence};
use crate::error::invalid;
use crate::pyfmt::{compact, now_secs};
use crate::{Engine, Result};

impl Engine {
    pub fn recent_knowledge(
        &self,
        days: i64,
        feed_name: Option<&str>,
        topics: Option<&[String]>,
        limit: i64,
        licence: Option<&str>,
    ) -> Result<Value> {
        let days = days.clamp(1, 365);
        let limit = limit.clamp(1, 50) as usize;
        let cutoff = now_secs() - days as f64 * 86_400.0;
        let licence = licence.filter(|l| !l.is_empty());
        if let Some(l) = licence
            && !LICENCE_CLASSES.contains(&l)
        {
            return Err(invalid(format!(
                "Invalid licence class '{l}'. Must be one of {}.",
                LICENCE_CLASSES.join(", ")
            )));
        }
        let feed_name = feed_name.filter(|f| !f.is_empty());
        let topics = topics.filter(|t| !t.is_empty());

        // State, age and feed are evaluated in SQL; the age is re-checked
        // below because the store's cut is a superset of this parse.
        let filter = MemoryFilter {
            states: vec!["active".to_owned()],
            feed_names: feed_name.map(|f| vec![f.to_owned()]).unwrap_or_default(),
            created_at_min: Some(cutoff),
            ..MemoryFilter::default()
        };
        let rows = self.store.list_memories(
            Namespace::Knowledge,
            &filter,
            &[
                "created_at",
                "feed_name",
                "topics",
                "title",
                "content",
                "source_url",
                "published_at",
                "expires_at",
                "licence",
                "licence_note",
                "provenance",
                "enriched_from",
                "imported_at",
            ],
        )?;
        let mut results: Vec<(f64, Value)> = Vec::new();
        for (key, data) in &rows {
            let created = data
                .get("created_at")
                .and_then(|c| c.parse::<f64>().ok())
                .unwrap_or(0.0);
            if created < cutoff {
                continue;
            }
            if let Some(l) = licence
                && effective_licence(data, "knowledge") != l
            {
                continue;
            }
            let item_topics: Vec<Value> = data
                .get("topics")
                .and_then(|t| serde_json::from_str(t).ok())
                .unwrap_or_default();
            if let Some(wanted) = topics
                && !wanted
                    .iter()
                    .any(|t| item_topics.iter().any(|i| i.as_str() == Some(t)))
            {
                continue;
            }
            let opt = |n: &str| data.get(n).map_or(Value::Null, |v| v.as_str().into());
            let mut m = Map::new();
            m.insert("key".into(), key.as_str().into());
            for n in [
                "title",
                "content",
                "source_url",
                "feed_name",
                "published_at",
                "created_at",
                "expires_at",
            ] {
                m.insert(n.into(), opt(n));
            }
            m.insert(
                "topics".into(),
                if data.get("topics").is_some_and(|t| !t.is_empty()) {
                    Value::Array(item_topics)
                } else {
                    Value::Null
                },
            );
            m.extend(classification_fields(data, "knowledge", Some(key)));
            results.push((created, compact(m)));
        }
        results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        Ok(Value::Array(
            results.into_iter().take(limit).map(|(_, v)| v).collect(),
        ))
    }
}
