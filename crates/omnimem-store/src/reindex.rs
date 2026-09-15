//! Rebuilding the in-memory vector matrix from the database.
//!
//! There is no separate search index to drift from the records, but the
//! matrix is still a copy. `reload_vectors` rebuilds it from the `vectors`
//! table, which is what 6.x's `reindex` tool maps onto.

use std::collections::BTreeMap;

use omnimem_core::Namespace;

use crate::vectors::VectorIndex;
use crate::{Result, Store};

impl Store {
    /// Rebuild every namespace's matrix. Returns (vectors before, vectors
    /// after) per namespace.
    pub fn reload_vectors(&self) -> Result<BTreeMap<Namespace, (usize, usize)>> {
        let before: BTreeMap<Namespace, usize> = Namespace::ALL
            .iter()
            .map(|ns| (*ns, self.vector_count(*ns)))
            .collect();
        let index = {
            let conn = self.conn();
            VectorIndex::load(&conn, self.dimension())?
        };
        *self.vectors_write() = index;
        Ok(Namespace::ALL
            .iter()
            .map(|ns| (*ns, (before[ns], self.vector_count(*ns))))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Fields;

    #[test]
    fn reload_matches_the_table() {
        let store = Store::open_in_memory_with_dim(2).unwrap();
        let fields = Fields::from([("content".to_owned(), "x".to_owned())]);
        store
            .upsert("mem:episodic:a", &fields, Some(&[1.0, 0.0]))
            .unwrap();
        let report = store.reload_vectors().unwrap();
        assert_eq!(report[&Namespace::Episodic], (1, 1));
        assert_eq!(report[&Namespace::Skill], (0, 0));
    }
}
