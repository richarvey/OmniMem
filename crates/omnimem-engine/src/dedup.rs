//! Semantic duplicate detection (`memory/dedup.py`).

use std::collections::HashMap;

use omnimem_core::Namespace;
use omnimem_store::SearchFilter;
use serde_json::{Map, Value, json};
use tracing::warn;

use crate::pyfmt::take_chars;
use crate::{Engine, Result};

const MAX_DEDUP_KEYS: usize = 2000;

pub struct DuplicateMatch {
    pub key: String,
    pub content: String,
    pub similarity: f64,
}

fn doc_project(fields: &omnimem_store::Fields) -> Option<&str> {
    fields
        .get("project")
        .filter(|p| !p.is_empty())
        .or_else(|| fields.get("project_name").filter(|p| !p.is_empty()))
        .map(String::as_str)
}

impl Engine {
    /// The first near-identical live memory in the namespace, if any.
    pub fn check_duplicate(
        &self,
        namespace: Namespace,
        vector: &[f32],
        project_filter: Option<&str>,
    ) -> Result<Option<DuplicateMatch>> {
        let hits = self
            .store
            .search(namespace, vector, 5, &SearchFilter::default(), None)?;
        for hit in hits {
            let state = hit
                .fields
                .get("state")
                .map(String::as_str)
                .unwrap_or("active");
            if matches!(state, "archived" | "deleted") {
                continue;
            }
            if let Some(project) = project_filter.filter(|p| !p.is_empty())
                && doc_project(&hit.fields) != Some(project)
            {
                continue;
            }
            let similarity = 1.0 - f64::from(hit.distance);
            if similarity >= self.config.dedup_threshold {
                return Ok(Some(DuplicateMatch {
                    key: hit.key,
                    content: hit.fields.get("content").cloned().unwrap_or_default(),
                    similarity,
                }));
            }
        }
        Ok(None)
    }

    /// Clusters of near-identical memories in a namespace, largest first.
    pub(crate) fn find_all_duplicates(
        &self,
        namespace: Namespace,
        threshold: Option<f64>,
        project_filter: Option<&str>,
    ) -> Result<Vec<Value>> {
        let threshold = threshold.unwrap_or(self.config.dedup_threshold);
        let mut keys = self.store.scan_prefix(&format!("mem:{namespace}:"))?;
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        if keys.len() > MAX_DEDUP_KEYS {
            warn!(
                cap = MAX_DEDUP_KEYS,
                total = keys.len(),
                "dedup scan capped"
            );
            keys.truncate(MAX_DEDUP_KEYS);
        }
        let rows = self.store.get_fields_multi(
            &keys,
            &["content", "state", "project", "project_name", "created_at"],
        )?;
        let entries: Vec<(String, omnimem_store::Fields)> = keys
            .into_iter()
            .zip(rows)
            .filter_map(|(key, row)| row.map(|r| (key, r)))
            .filter(|(_, r)| {
                !matches!(
                    r.get("state").map(String::as_str),
                    Some("archived" | "deleted")
                )
            })
            .filter(|(_, r)| match project_filter.filter(|p| !p.is_empty()) {
                Some(project) => doc_project(r) == Some(project),
                None => true,
            })
            .collect();
        if entries.len() < 2 {
            return Ok(Vec::new());
        }

        let entry_keys: Vec<String> = entries.iter().map(|(k, _)| k.clone()).collect();
        let mut vectors = self.store.get_vectors_multi(&entry_keys);
        let missing: Vec<usize> = (0..vectors.len())
            .filter(|i| vectors[*i].is_none())
            .collect();
        if !missing.is_empty() {
            let texts: Vec<&str> = missing
                .iter()
                .map(|i| {
                    entries[*i]
                        .1
                        .get("content")
                        .map(String::as_str)
                        .unwrap_or("")
                })
                .collect();
            for (i, v) in missing.iter().zip(self.embed_many(&texts)?) {
                vectors[*i] = Some(v);
            }
        }
        let vectors: Vec<Vec<f32>> = vectors.into_iter().map(Option::unwrap_or_default).collect();

        let n = entries.len();
        let mut parent: Vec<usize> = (0..n).collect();
        fn find(parent: &mut [usize], mut x: usize) -> usize {
            while parent[x] != x {
                parent[x] = parent[parent[x]];
                x = parent[x];
            }
            x
        }
        let mut pairs: Vec<(usize, usize, f64)> = Vec::new();
        for i in 0..n {
            for j in (i + 1)..n {
                let sim: f32 = vectors[i].iter().zip(&vectors[j]).map(|(a, b)| a * b).sum();
                let sim = f64::from(sim);
                if sim >= threshold {
                    pairs.push((i, j, sim));
                }
            }
        }
        for &(i, j, _) in &pairs {
            let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
            if ri != rj {
                parent[ri] = rj;
            }
        }
        let mut max_sim: HashMap<usize, f64> = HashMap::new();
        for &(i, _, sim) in &pairs {
            let root = find(&mut parent, i);
            let best = max_sim.entry(root).or_insert(0.0);
            if sim > *best {
                *best = sim;
            }
        }
        let mut order: Vec<usize> = Vec::new();
        let mut members: HashMap<usize, Vec<usize>> = HashMap::new();
        for i in 0..n {
            let root = find(&mut parent, i);
            members
                .entry(root)
                .or_insert_with(|| {
                    order.push(root);
                    Vec::new()
                })
                .push(i);
        }
        let mut clusters: Vec<(usize, Value)> = Vec::new();
        for root in order {
            let indices = &members[&root];
            if indices.len() < 2 {
                continue;
            }
            let memories: Vec<Value> = indices
                .iter()
                .map(|&idx| {
                    let (key, data) = &entries[idx];
                    let mut m = Map::new();
                    m.insert("key".into(), key.as_str().into());
                    m.insert(
                        "content".into(),
                        take_chars(data.get("content").map(String::as_str).unwrap_or(""), 200)
                            .into(),
                    );
                    m.insert(
                        "project".into(),
                        doc_project(data).map_or(Value::Null, Value::from),
                    );
                    m.insert(
                        "state".into(),
                        data.get("state")
                            .map(String::as_str)
                            .unwrap_or("active")
                            .into(),
                    );
                    m.insert(
                        "created_at".into(),
                        data.get("created_at")
                            .map_or(Value::Null, |c| Value::from(c.as_str())),
                    );
                    Value::Object(m)
                })
                .collect();
            let root_now = find(&mut parent, root);
            clusters.push((
                indices.len(),
                json!({
                    "memories": memories,
                    "max_similarity": max_sim.get(&root_now).copied().unwrap_or(0.0),
                }),
            ));
        }
        clusters.sort_by_key(|c| std::cmp::Reverse(c.0));
        Ok(clusters.into_iter().map(|(_, c)| c).collect())
    }
}
