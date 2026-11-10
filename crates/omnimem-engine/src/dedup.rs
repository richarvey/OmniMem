//! Semantic duplicate detection (`memory/dedup.py`).

use std::collections::HashMap;

use omnimem_core::Namespace;
use omnimem_store::SearchFilter;
use serde_json::{Map, Value, json};
use tracing::warn;

use crate::pyfmt::take_chars;
use crate::{Engine, Result};

const MAX_DEDUP_KEYS: usize = 2000;
/// The neighbours a duplicate check looks at, as in 6.x.
pub(crate) const DUPLICATE_NEIGHBOURS: usize = 5;

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
        let hits = self.store.search(
            namespace,
            vector,
            DUPLICATE_NEIGHBOURS,
            &SearchFilter::default(),
            None,
        )?;
        Ok(self.duplicate_among(&hits, project_filter))
    }

    /// [`Engine::check_duplicate`] over neighbours already found, nearest
    /// first. Only the first [`DUPLICATE_NEIGHBOURS`] count, so a caller that
    /// searched for more (`remember`, for the contradiction check too) gets
    /// the answer a search of its own would have given: ties break by key, so
    /// the nearest five of ten are the nearest five.
    pub(crate) fn duplicate_among(
        &self,
        hits: &[omnimem_store::SearchHit],
        project_filter: Option<&str>,
    ) -> Option<DuplicateMatch> {
        for hit in hits.iter().take(DUPLICATE_NEIGHBOURS) {
            let state = hit.fields.get("state").map_or("active", String::as_str);
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
                return Some(DuplicateMatch {
                    key: hit.key.clone(),
                    content: hit.fields.get("content").cloned().unwrap_or_default(),
                    similarity,
                });
            }
        }
        None
    }

    /// Clusters of near-identical memories in a namespace, largest first.
    pub(crate) fn find_all_duplicates(
        &self,
        namespace: Namespace,
        threshold: Option<f64>,
        project_filter: Option<&str>,
    ) -> Result<Vec<Value>> {
        let threshold = threshold.unwrap_or(self.config.dedup_threshold);
        let project_filter = project_filter.filter(|p| !p.is_empty());
        // The project is cut in SQL on its indexes (read as `doc_project`
        // reads it), and the cap applies to what is in scope, keeping the
        // newest: capping the namespace's oldest keys first, as this did,
        // meant a store past the cap never compared anything newer, and a
        // project's memories could be capped away by other projects'.
        let filter = omnimem_store::MemoryFilter {
            project: project_filter.map(str::to_owned),
            ..omnimem_store::MemoryFilter::default()
        };
        let mut entries: Vec<(String, omnimem_store::Fields)> = self
            .store
            .list_memories(
                namespace,
                &filter,
                &["content", "state", "project", "project_name", "created_at"],
            )?
            .into_iter()
            .filter(|(_, r)| {
                !matches!(
                    r.get("state").map(String::as_str),
                    Some("archived" | "deleted")
                )
            })
            .filter(|(_, r)| match project_filter {
                Some(project) => doc_project(r) == Some(project),
                None => true,
            })
            .collect();
        if entries.len() > MAX_DEDUP_KEYS {
            warn!(
                cap = MAX_DEDUP_KEYS,
                total = entries.len(),
                "dedup scan capped to the newest memories"
            );
            entries.drain(..entries.len() - MAX_DEDUP_KEYS);
        }
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
                .map(|i| entries[*i].1.get("content").map_or("", String::as_str))
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
                        take_chars(
                            data.get("content").map_or("", String::as_str),
                            crate::CLUSTER_PREVIEW_CHARS,
                        )
                        .into(),
                    );
                    m.insert(
                        "project".into(),
                        doc_project(data).map_or(Value::Null, Value::from),
                    );
                    m.insert(
                        "state".into(),
                        data.get("state").map_or("active", String::as_str).into(),
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
