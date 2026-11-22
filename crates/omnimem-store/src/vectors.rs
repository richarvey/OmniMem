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
        debug_assert!(dim > 0, "a vector space needs at least one dimension");
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
        // One buffer for every row, decoded from the blob SQLite holds rather
        // than a copy of it: at 50,000 memories that is 100,000 allocations
        // fewer on every load.
        let mut vector = Vec::with_capacity(dim);
        while let Some(row) = rows.next()? {
            let key: String = row.get(0)?;
            let Ok(namespace) = row
                .get_ref(1)?
                .as_str()
                .map_err(conversion(1))?
                .parse::<Namespace>()
            else {
                continue;
            };
            let data = row.get_ref(2)?.as_blob().map_err(conversion(2))?;
            if decode_into(data, dim, &mut vector) {
                index.insert(namespace, &key, &vector);
            } else {
                warn!(
                    key,
                    bytes = data.len(),
                    "skipping a stored vector of the wrong size"
                );
            }
        }
        // Growing by doubling leaves up to half of each matrix spare; the
        // server keeps it for its lifetime, so give it back.
        for space in index.spaces.values_mut() {
            space.data.shrink_to_fit();
            space.norms.shrink_to_fit();
            space.keys.shrink_to_fit();
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

    /// Vectors across every namespace.
    pub(crate) fn total(&self) -> usize {
        self.spaces.values().map(|s| s.keys.len()).sum()
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
        let dim = self.dim;
        debug_assert_eq!(query.len(), dim);
        let query_norm = query.iter().map(|x| x * x).sum::<f32>().sqrt();
        // One pass over the contiguous matrix. `chunks_exact` hands the loop
        // rows of a length the compiler knows, so the per-element bounds
        // checks go; the multiply-add stays a sequential sum, because the
        // distances are pinned to the last bit and a reassociated sum would
        // move them. The filter runs before the dot product: a set lookup
        // on a short key is far cheaper than the 384 multiply-adds it can
        // skip, and an unselective filter costs about a hash per row.
        let candidates: Vec<usize> = match allowed {
            Some(set) => (0..space.keys.len())
                .filter(|&row| set.contains(space.keys[row].as_str()))
                .collect(),
            None => (0..space.keys.len()).collect(),
        };
        let mut scored: Vec<(f32, usize)> = Vec::with_capacity(candidates.len());
        let mut score = |row: usize, dot: f32| {
            let denom = space.norms[row] * query_norm;
            let cosine = if denom > 0.0 { dot / denom } else { 0.0 };
            scored.push((1.0 - cosine, row));
        };
        // Four rows at a time, each with its own running sum. One sum is a
        // chain of dependent adds, so the CPU waits on each before the next;
        // four independent chains keep it busy, about twice as fast. Every
        // row is still summed in the same order, so the distances are the
        // same to the last bit.
        let row_of = |row: usize| &space.data[row * dim..(row + 1) * dim];
        let (quads, rest) = candidates.as_chunks::<4>();
        for quad in quads {
            let dots = dot4(quad.map(row_of), query);
            for (&row, dot) in quad.iter().zip(dots) {
                score(row, dot);
            }
        }
        for &row in rest {
            score(row, dot(row_of(row), query));
        }
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

/// A column that isn't the type `load` reads it as, as rusqlite reports it.
fn conversion(column: usize) -> impl Fn(rusqlite::types::FromSqlError) -> rusqlite::Error {
    move |e| {
        rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Null, Box::new(e))
    }
}

/// A row's dot product with the query, summed first to last.
fn dot(row: &[f32], query: &[f32]) -> f32 {
    row.iter().zip(query).map(|(a, b)| a * b).sum()
}

/// Four rows' dot products with the query, each summed first to last
/// exactly as [`dot`] sums one.
fn dot4(rows: [&[f32]; 4], query: &[f32]) -> [f32; 4] {
    let [r0, r1, r2, r3] = rows;
    // The same start `Iterator::sum` uses, so an all-zero row comes out with
    // the same sign of zero as `dot` gives it.
    let start = [0.0f32; 0].iter().sum::<f32>();
    let mut acc = [start; 4];
    for ((((a, b), c), d), q) in r0.iter().zip(r1).zip(r2).zip(r3).zip(query) {
        acc[0] += a * q;
        acc[1] += b * q;
        acc[2] += c * q;
        acc[3] += d * q;
    }
    acc
}

/// Decode a stored vector into `out`, if its length fits `dim`.
fn decode_into(data: &[u8], dim: usize, out: &mut Vec<f32>) -> bool {
    if data.len() != dim * 4 {
        return false;
    }
    out.clear();
    let (chunks, _) = data.as_chunks::<4>();
    out.extend(chunks.iter().map(|c| f32::from_le_bytes(*c)));
    true
}

pub(crate) fn to_bytes(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vector.len() * 4);
    for x in vector {
        bytes.extend_from_slice(&x.to_le_bytes());
    }
    bytes
}

#[cfg(test)]
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

    /// The four-row loop must give every row the distance the plain
    /// one-row sum gives it, bit for bit, filtered or not, whatever the
    /// count's remainder by four.
    #[test]
    fn four_row_scoring_matches_the_plain_sum_bit_for_bit() {
        let dim = 384;
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 40) as f32 / (1u64 << 24) as f32 - 0.5
        };
        let mut index = VectorIndex::new(dim);
        let mut plain = Vec::new();
        for i in 0..203 {
            let v: Vec<f32> = (0..dim).map(|_| next()).collect();
            let key = format!("mem:episodic:{i:04}");
            index.insert(Namespace::Episodic, &key, &v);
            plain.push((key, v));
        }
        let query: Vec<f32> = (0..dim).map(|_| next()).collect();
        let query_norm = query.iter().map(|x| x * x).sum::<f32>().sqrt();
        let expected = |keys: &dyn Fn(&str) -> bool| {
            let mut all: Vec<(String, f32)> = plain
                .iter()
                .filter(|(k, _)| keys(k))
                .map(|(k, v)| {
                    let dot: f32 = v.iter().zip(&query).map(|(a, b)| a * b).sum();
                    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                    (k.clone(), 1.0 - dot / (norm * query_norm))
                })
                .collect();
            all.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
            all
        };
        let bits = |hits: Vec<(String, f32)>| -> Vec<(String, u32)> {
            hits.into_iter().map(|(k, d)| (k, d.to_bits())).collect()
        };
        let got = index.search(Namespace::Episodic, &query, 100, None);
        let mut want = expected(&|_| true);
        want.truncate(100);
        assert_eq!(bits(got), bits(want));

        let allowed: HashSet<String> = plain
            .iter()
            .map(|(k, _)| k.clone())
            .filter(|k| k.ends_with(['1', '4', '7']))
            .collect();
        let got = index.search(Namespace::Episodic, &query, 100, Some(&allowed));
        let want = expected(&|k| allowed.contains(k));
        assert_eq!(bits(got), bits(want));
    }

    #[test]
    fn bytes_round_trip_and_reject_wrong_sizes() {
        let v = vec![0.25f32, -1.5, 3.0];
        assert_eq!(from_bytes(&to_bytes(&v), 3).unwrap(), v);
        assert!(from_bytes(&to_bytes(&v), 4).is_none());
    }
}
