//! The tier-1 contradiction heuristic (`memory/contradiction.py`).

use std::sync::LazyLock;

use regex::{Regex, RegexSet};

use crate::Engine;
use crate::pyfmt::take_chars;

/// The neighbours the heuristic looks at, as in 6.x. `remember` searches
/// this many once and hands the nearest to the duplicate check too.
pub(crate) const CONTRADICTION_NEIGHBOURS: usize = 10;
const _: () = assert!(CONTRADICTION_NEIGHBOURS >= crate::dedup::DUPLICATE_NEIGHBOURS);

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

/// Each side of the pairs as one set, so a text is searched once per side
/// for all seventeen rather than once per pattern. Index `i` of each set is
/// pair `i`.
static NEGATIONS: LazyLock<RegexSet> =
    LazyLock::new(|| RegexSet::new(NEGATION_PATTERNS.iter().map(|(neg, _)| neg)).expect("valid"));
static POSITIVES: LazyLock<RegexSet> =
    LazyLock::new(|| RegexSet::new(NEGATION_PATTERNS.iter().map(|(_, pos)| pos)).expect("valid"));

/// Words that say a statement replaces an earlier one: a change of value
/// rather than a negation. "Lives in Berlin" and "moved to Lisbon" share no
/// negation pair, so without these the keyless heuristic never links a
/// changed fact, and the old one keeps outranking the new.
const STATE_CHANGE_PATTERN: &str = r"(?x)
    \bno\ longer\b | \bnot\ any\ ?more\b | \bany\ ?more\b
  | \bmoved\ (?:from|to|back|away|out|in)\b | \brelocated\b
  | \bswitched\ (?:to|from|over|back)\b | \bchanged\ (?:to|from)\b
  | \bnow\b | \binstead\b | \bused\ to\b | \bstopped\b | \bquit\b
  | \bmisspoke\b | \bwas\ wrong\b | \bcorrection\b
  | \bnot\ true\b | \bthere\ is\ no\b | \bturned\ out\b";

static STATE_CHANGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(STATE_CHANGE_PATTERN).expect("valid"));

/// One of two different statements says it changes or corrects something.
/// Only meaningful between memories already found similar: the caller
/// gates on similarity, as it does for a negation pair.
#[cfg(test)]
pub(crate) fn has_state_change(a: &str, b: &str) -> bool {
    state_change_lower(&a.to_lowercase(), &b.to_lowercase())
}

/// [`has_state_change`] on text already lowercased. Trimming after
/// lowercasing is the same as before: no character lowercases to or from
/// whitespace.
fn state_change_lower(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim(), b.trim());
    a != b && (STATE_CHANGE.is_match(a) || STATE_CHANGE.is_match(b))
}

/// What the keyless heuristic links between two similar memories: a
/// negation pair, or a statement of change. Skills use the narrower
/// [`has_negation_pair`], since a skill saying "now" changes nothing.
/// Each text is lowercased once for both tests; the maintenance pass calls
/// this for every similar pair it finds.
pub(crate) fn signals_conflict(a: &str, b: &str) -> bool {
    let (a, b) = (a.to_lowercase(), b.to_lowercase());
    negation_pair_lower(&a, &b) || state_change_lower(&a, &b)
}

pub(crate) fn has_negation_pair(a: &str, b: &str) -> bool {
    negation_pair_lower(&a.to_lowercase(), &b.to_lowercase())
}

/// Some pair has its negation in one text and its positive in the other.
fn negation_pair_lower(a: &str, b: &str) -> bool {
    let (neg_a, pos_a) = (NEGATIONS.matches(a), POSITIVES.matches(a));
    let (neg_b, pos_b) = (NEGATIONS.matches(b), POSITIVES.matches(b));
    (0..NEGATION_PATTERNS.len())
        .any(|i| (neg_a.matched(i) && pos_b.matched(i)) || (neg_b.matched(i) && pos_a.matched(i)))
}

pub(crate) struct ContradictionMatch {
    pub existing_key: String,
    pub existing_content: String,
    pub similarity: f64,
    /// The new statement says it changes something ("moved to", "now",
    /// "switched to") and the earlier one is still believed: the new one
    /// may supersede it (#35). Only ever a hint; a person decides.
    pub may_supersede: bool,
}

/// Whether `text` says it changes or replaces something.
pub(crate) fn states_a_change(text: &str) -> bool {
    STATE_CHANGE.is_match(&text.trim().to_lowercase())
}

// The store owns the text, so the v5 migration can recognise a heuristic link.
pub(crate) use omnimem_store::HEURISTIC_EXPLANATION;

impl Engine {
    /// The heuristic over neighbours already found, nearest first: the first
    /// [`CONTRADICTION_NEIGHBOURS`] of them, as a search of its own would
    /// return them.
    pub(crate) fn contradiction_among(
        &self,
        hits: &[omnimem_store::SearchHit],
        content: &str,
        project_filter: Option<&str>,
    ) -> Option<ContradictionMatch> {
        for hit in hits.iter().take(CONTRADICTION_NEIGHBOURS) {
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
            if signals_conflict(content, existing) {
                let believed = matches!(
                    omnimem_store::trust_of(&hit.fields),
                    crate::trust::ACCEPTED | crate::trust::CANDIDATE | crate::trust::DISPUTED
                );
                return Some(ContradictionMatch {
                    may_supersede: believed
                        && states_a_change(content)
                        && content.trim().to_lowercase() != existing.trim().to_lowercase(),
                    existing_key: hit.key.clone(),
                    existing_content: take_chars(existing, 200),
                    similarity,
                });
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_of_value_is_a_conflict() {
        assert!(signals_conflict(
            "The user lives in Berlin.",
            "The user moved from Berlin to Lisbon."
        ));
        assert!(signals_conflict(
            "Sam loves coriander.",
            "Sam can't stand coriander any more."
        ));
        assert!(signals_conflict(
            "Sam has a sister who is a doctor.",
            "I misspoke: there is no sister."
        ));
        assert!(signals_conflict(
            "User works at Google.",
            "User now works at Anthropic."
        ));
        assert!(!signals_conflict(
            "User works at Google.",
            "User works at Google."
        ));
        assert!(!signals_conflict(
            "User works at Google.",
            "User likes tea."
        ));
        assert!(
            !has_state_change("knowledge", "snowfall"),
            "word boundaries"
        );
    }

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
