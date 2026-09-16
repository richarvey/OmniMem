//! Vectors held in memory, one contiguous matrix per namespace, searched
//! exactly.

use std::collections::{HashMap, HashSet};

use omnimem_core::Namespace;
use rusqlite::Connection;
use tracing::warn;

use crate::Result;

#[derive(Default)]
struct Space {
    keys: Vec<String>,
    data: Vec<f32>,
    norms: Vec<f32>,
    rows: HashMap<String, usize>,
}

pub(crate) struct VectorIndex {
    dim: usize,
    spaces: HashMap<Namespace, Space>,
}

impl VectorIndex {
    pub(crate) fn new(dim: usize) -> Self {
        Self {
            dim,
            spaces: HashMap::new(),
        }
    }

    /// Every stored vector, skipping any whose byte length doesn't fit.
    pub(crate) fn load(conn: &Connection, dim: usize) -> Result<Self> {
        let mut index = Self::new(dim);
        let mut stmt = conn.prepare(
            "SELECT v.key, m.namespace, v.data FROM vectors v JOIN memories m ON m.key = v.key ORDER BY v.key",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let key: String = row.get(0)?;
            let namespace: String = row.get(1)?;
            let data: Vec<u8> = row.get(2)?;
            let Ok(namespace) = namespace.parse::<Namespace>() else {
                continue;
            };
            if let Some(vector) = from_bytes(&data, dim) {
                index.insert(namespace, &key, &vector)
            } else {
                warn!(
                    key,
                    bytes = data.len(),
                    "skipping a stored vector of the wrong size"
                );
            }
        }
        Ok(index)
    }

    pub(crate) fn insert(&mut self, namespace: Namespace, key: &str, vector: &[f32]) {
        debug_assert_eq!(vector.len(), self.dim);
        let dim = self.dim;
        let space = self.spaces.entry(namespace).or_default();
        let norm = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        if let Some(&row) = space.rows.get(key) {
            space.data[row * dim..(row + 1) * dim].copy_from_slice(vector);
            space.norms[row] = norm;
        } else {
            space.rows.insert(key.to_owned(), space.keys.len());
            space.keys.push(key.to_owned());
            space.data.extend_from_slice(vector);
            space.norms.push(norm);
        }
    }

    pub(crate) fn remove(&mut self, namespace: Namespace, key: &str) {
        let dim = self.dim;
        let Some(space) = self.spaces.get_mut(&namespace) else {
            return;
        };
        let Some(row) = space.rows.remove(key) else {
            return;
        };
        let last = space.keys.len() - 1;
        if row != last {
            space
                .data
                .copy_within(last * dim..(last + 1) * dim, row * dim);
            space.norms[row] = space.norms[last];
            space.keys.swap(row, last);
            space.rows.insert(space.keys[row].clone(), row);
        }
        space.keys.pop();
        space.norms.pop();
        space.data.truncate(last * dim);
    }

    pub(crate) fn get(&self, namespace: Namespace, key: &str) -> Option<Vec<f32>> {
        let space = self.spaces.get(&namespace)?;
        let row = *space.rows.get(key)?;
        Some(space.data[row * self.dim..(row + 1) * self.dim].to_vec())
    }

    pub(crate) fn len(&self, namespace: Namespace) -> usize {
        self.spaces.get(&namespace).map_or(0, |s| s.keys.len())
    }

    /// The `k` nearest rows by cosine distance (`1 - cosine`, as valkey-search
    /// reported it), ties broken by key so results never depend on insertion
    /// order. `allowed`, when given, restricts the candidates.
    pub(crate) fn search(
        &self,
        namespace: Namespace,
        query: &[f32],
        k: usize,
        allowed: Option<&HashSet<String>>,
    ) -> Vec<(String, f32)> {
        let Some(space) = self.spaces.get(&namespace) else {
            return Vec::new();
        };
        let query_norm = query.iter().map(|x| x * x).sum::<f32>().sqrt();
        let dim = self.dim;
        let mut scored: Vec<(f32, usize)> = space
            .keys
            .iter()
            .enumerate()
            .filter(|(_, key)| allowed.is_none_or(|set| set.contains(key.as_str())))
            .map(|(row, _)| {
                let v = &space.data[row * dim..(row + 1) * dim];
                let dot: f32 = v.iter().zip(query).map(|(a, b)| a * b).sum();
                let denom = space.norms[row] * query_norm;
                let cosine = if denom > 0.0 { dot / denom } else { 0.0 };
                (1.0 - cosine, row)
            })
            .collect();
        let order = |a: &(f32, usize), b: &(f32, usize)| {
            a.0.total_cmp(&b.0)
                .then_with(|| space.keys[a.1].cmp(&space.keys[b.1]))
        };
        if scored.len() > k {
            scored.select_nth_unstable_by(k, order);
            scored.truncate(k);
        }
        scored.sort_by(order);
        scored
            .into_iter()
            .map(|(distance, row)| (space.keys[row].clone(), distance))
            .collect()
    }
}

pub(crate) fn to_bytes(vector: &[f32]) -> Vec<u8> {
    vector.iter().flat_map(|x| x.to_le_bytes()).collect()
}

pub(crate) fn from_bytes(data: &[u8], dim: usize) -> Option<Vec<f32>> {
    (data.len() == dim * 4).then(|| {
        let (chunks, _) = data.as_chunks::<4>();
        chunks.iter().map(|c| f32::from_le_bytes(*c)).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(values: &[f32]) -> Vec<f32> {
        let n = values.iter().map(|x| x * x).sum::<f32>().sqrt();
        values.iter().map(|x| x / n).collect()
    }

    #[test]
    fn nearest_first_with_key_tiebreak() {
        let mut index = VectorIndex::new(2);
        index.insert(Namespace::Episodic, "mem:episodic:b", &unit(&[1.0, 0.0]));
        index.insert(Namespace::Episodic, "mem:episodic:a", &unit(&[1.0, 0.0]));
        index.insert(Namespace::Episodic, "mem:episodic:c", &unit(&[0.0, 1.0]));
        let hits = index.search(Namespace::Episodic, &unit(&[1.0, 0.1]), 3, None);
        let keys: Vec<&str> = hits.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["mem:episodic:a", "mem:episodic:b", "mem:episodic:c"]);
        assert!(hits[0].1 < hits[2].1);
    }

    #[test]
    fn k_limits_and_allowed_filters() {
        let mut index = VectorIndex::new(2);
        for (i, v) in [[1.0, 0.0], [0.9, 0.1], [0.0, 1.0]].iter().enumerate() {
            index.insert(
                Namespace::Knowledge,
                &format!("mem:knowledge:{i}"),
                &unit(v),
            );
        }
        assert_eq!(
            index
                .search(Namespace::Knowledge, &unit(&[1.0, 0.0]), 1, None)
                .len(),
            1
        );
        let allowed: HashSet<String> = ["mem:knowledge:2".to_owned()].into();
        let hits = index.search(Namespace::Knowledge, &unit(&[1.0, 0.0]), 5, Some(&allowed));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, "mem:knowledge:2");
    }

    #[test]
    fn remove_keeps_the_matrix_consistent() {
        let mut index = VectorIndex::new(2);
        index.insert(Namespace::Episodic, "k1", &[1.0, 0.0]);
        index.insert(Namespace::Episodic, "k2", &[0.0, 1.0]);
        index.insert(Namespace::Episodic, "k3", &[0.6, 0.8]);
        index.remove(Namespace::Episodic, "k1");
        assert_eq!(index.len(Namespace::Episodic), 2);
        assert_eq!(
            index.get(Namespace::Episodic, "k3").unwrap(),
            vec![0.6, 0.8]
        );
        assert_eq!(
            index.get(Namespace::Episodic, "k2").unwrap(),
            vec![0.0, 1.0]
        );
        assert!(index.get(Namespace::Episodic, "k1").is_none());
        let hits = index.search(Namespace::Episodic, &[0.0, 1.0], 1, None);
        assert_eq!(hits[0].0, "k2");
    }

    #[test]
    fn replacing_a_vector_updates_in_place() {
        let mut index = VectorIndex::new(2);
        index.insert(Namespace::Episodic, "k", &[1.0, 0.0]);
        index.insert(Namespace::Episodic, "k", &[0.0, 1.0]);
        assert_eq!(index.len(Namespace::Episodic), 1);
        assert_eq!(index.get(Namespace::Episodic, "k").unwrap(), vec![0.0, 1.0]);
    }

    #[test]
    fn bytes_round_trip_and_reject_wrong_sizes() {
        let v = vec![0.25f32, -1.5, 3.0];
        assert_eq!(from_bytes(&to_bytes(&v), 3).unwrap(), v);
        assert!(from_bytes(&to_bytes(&v), 4).is_none());
    }
}
