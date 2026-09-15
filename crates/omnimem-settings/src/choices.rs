//! The licence and provenance vocabularies as the forms show them
//! (`LICENCE_CHOICES` and `PROVENANCE_CHOICES` in 6.x).

/// (value, label) pairs in vocabulary order, for form selects.
pub(crate) const LICENCE_CHOICES: [[&str; 2]; 4] = [
    ["own", "Own work"],
    ["open", "Open (redistributable)"],
    ["restricted", "Restricted (not redistributable)"],
    ["unknown", "Unknown (needs classifying)"],
];

pub(crate) const PROVENANCE_CHOICES: [[&str; 2]; 3] = [
    ["retrieved", "Retrieved (external source)"],
    ["concluded", "Concluded (system reasoning)"],
    ["asserted", "Asserted (stated by the human)"],
];

fn label(choices: &[[&'static str; 2]], value: &str) -> &'static str {
    choices
        .iter()
        .find(|[v, _]| *v == value)
        .map_or("Not recorded", |[_, label]| label)
}

pub(crate) fn licence_label(value: &str) -> &'static str {
    label(&LICENCE_CHOICES, value)
}

pub(crate) fn provenance_label(value: &str) -> &'static str {
    label(&PROVENANCE_CHOICES, value)
}

pub(crate) fn is_licence_class(value: &str) -> bool {
    LICENCE_CHOICES.iter().any(|[v, _]| *v == value)
}

pub(crate) fn is_provenance_class(value: &str) -> bool {
    PROVENANCE_CHOICES.iter().any(|[v, _]| *v == value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use omnimem_core::classification::{LICENCE_CLASSES, PROVENANCE_CLASSES};

    #[test]
    fn the_choices_follow_the_vocabularies() {
        let licences: Vec<&str> = LICENCE_CHOICES.iter().map(|[v, _]| *v).collect();
        let provenances: Vec<&str> = PROVENANCE_CHOICES.iter().map(|[v, _]| *v).collect();
        assert_eq!(licences, LICENCE_CLASSES);
        assert_eq!(provenances, PROVENANCE_CLASSES);
        assert_eq!(licence_label("open"), "Open (redistributable)");
        assert_eq!(provenance_label(""), "Not recorded");
    }
}
