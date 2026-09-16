//! The tier-1 contradiction heuristic (`memory/contradiction.py`).

use std::sync::LazyLock;

use omnimem_core::Namespace;
use omnimem_store::SearchFilter;
use regex::Regex;

use crate::pyfmt::take_chars;
use crate::{Engine, Result};

const NEGATION_PATTERNS: [(&str, &str); 17] = [
    (r"\bdon't\b", r"\bdo\b"),
    (r"\bdo not\b", r"\bdo\b"),
    (r"\bnever\b", r"\balways\b"),
    (r"\bavoid\b", r"\buse\b"),
    (r"\bdon't use\b", r"\buse\b"),
    (r"\bshouldn't\b", r"\bshould\b"),
    (r"\bshould not\b", r"\bshould\b"),
    (r"\bdisable\b", r"\benable\b"),
    (r"\bremove\b", r"\badd\b"),
    (r"\bwithout\b", r"\bwith\b"),
    (r"\bnot recommended\b", r"\brecommended\b"),
    (r"\bdeprecated\b", r"\brecommended\b"),
    (r"\babandoned\b", r"\badopted\b"),
    (r"\bfailed\b", r"\bsucceeded\b"),
    (r"\bwon't work\b", r"\bworks\b"),
    (r"\bdoes not work\b", r"\bworks\b"),
    (r"\bdoesn't work\b", r"\bworks\b"),
];

static COMPILED: LazyLock<Vec<(Regex, Regex)>> = LazyLock::new(|| {
    NEGATION_PATTERNS
        .iter()
        .map(|(neg, pos)| {
            (
                Regex::new(neg).expect("valid"),
                Regex::new(pos).expect("valid"),
            )
        })
        .collect()
});

pub(crate) fn has_negation_pair(a: &str, b: &str) -> bool {
    let (a, b) = (a.to_lowercase(), b.to_lowercase());
    COMPILED.iter().any(|(neg, pos)| {
        (neg.is_match(&a) && pos.is_match(&b)) || (neg.is_match(&b) && pos.is_match(&a))
    })
}

pub(crate) struct ContradictionMatch {
    pub existing_key: String,
    pub existing_content: String,
    pub similarity: f64,
}

pub(crate) const HEURISTIC_EXPLANATION: &str = "These memories discuss the same topic but contain opposing language (negation patterns detected).";

impl Engine {
    pub(crate) fn check_contradiction_heuristic(
        &self,
        namespace: Namespace,
        vector: &[f32],
        content: &str,
        project_filter: Option<&str>,
    ) -> Result<Option<ContradictionMatch>> {
        let hits = self
            .store
            .search(namespace, vector, 10, &SearchFilter::default(), None)?;
        for hit in hits {
            let state = hit.fields.get("state").map_or("active", String::as_str);
            if matches!(state, "archived" | "deleted") {
                continue;
            }
            if let Some(project) = project_filter.filter(|p| !p.is_empty()) {
                let doc_project = hit
                    .fields
                    .get("project")
                    .filter(|p| !p.is_empty())
                    .or_else(|| hit.fields.get("project_name").filter(|p| !p.is_empty()));
                if doc_project.map(String::as_str) != Some(project) {
                    continue;
                }
            }
            let similarity = 1.0 - f64::from(hit.distance);
            if similarity < self.config.contradiction_threshold {
                continue;
            }
            let existing = hit.fields.get("content").map_or("", String::as_str);
            if has_negation_pair(content, existing) {
                return Ok(Some(ContradictionMatch {
                    existing_key: hit.key,
                    existing_content: take_chars(existing, 200),
                    similarity,
                }));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negation_pairs_either_way() {
        assert!(has_negation_pair("Never use Alpine", "Always use Alpine"));
        assert!(has_negation_pair("use sqlite", "avoid sqlite"));
        assert!(!has_negation_pair("use sqlite", "use sqlite"));
        assert!(
            !has_negation_pair("doing well", "undo it"),
            "word boundaries"
        );
    }
}
