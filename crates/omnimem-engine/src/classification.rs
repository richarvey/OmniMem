//! Licence and provenance: resolving what a write declares, and what a stored
//! record reads as. Ported from `memory/licence.py`, `memory/provenance.py`
//! and `memory/classification.py`.

use omnimem_core::classification::{
    LICENCE_CLASSES, LICENCE_OPEN, LICENCE_OWN, LICENCE_RESTRICTED, LICENCE_UNKNOWN,
    PROVENANCE_ASSERTED, PROVENANCE_CLASSES, PROVENANCE_CONCLUDED, PROVENANCE_RETRIEVED,
};
use omnimem_store::Fields;
use serde_json::{Map, Value};

use crate::Result;
use crate::error::invalid;

const MAX_LICENCE_NOTE: usize = 200;

const CLASSIFIABLE_PREFIXES: [&str; 4] = [
    "mem:episodic:",
    "mem:project:",
    "mem:knowledge:",
    "mem:preference:",
];

/// `'CC BY 4.0'` → `cc-by-4.0`, `'Looked_Up'` → `looked-up`.
pub fn normalise_alias_key(raw: &str) -> String {
    raw.trim()
        .to_lowercase()
        .replace(['_', '-'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
}

/// Recognised identifiers → (class, canonical note). Verbatim from 6.6.1.
const LICENCE_ALIASES: &[(&str, &str, Option<&str>)] = &[
    ("own", LICENCE_OWN, None),
    ("open", LICENCE_OPEN, None),
    ("restricted", LICENCE_RESTRICTED, None),
    ("unknown", LICENCE_UNKNOWN, None),
    ("self", LICENCE_OWN, None),
    ("internal", LICENCE_OWN, None),
    ("in-house", LICENCE_OWN, None),
    ("original", LICENCE_OWN, None),
    ("", LICENCE_UNKNOWN, None),
    ("undetermined", LICENCE_UNKNOWN, None),
    ("unclassified", LICENCE_UNKNOWN, None),
    ("tbd", LICENCE_UNKNOWN, None),
    ("ogl", LICENCE_OPEN, Some("OGL v3.0")),
    ("ogl-3", LICENCE_OPEN, Some("OGL v3.0")),
    ("ogl-3.0", LICENCE_OPEN, Some("OGL v3.0")),
    ("ogl-uk-3.0", LICENCE_OPEN, Some("OGL v3.0")),
    ("open-government-licence", LICENCE_OPEN, Some("OGL v3.0")),
    ("public-domain", LICENCE_OPEN, Some("Public domain")),
    ("pd", LICENCE_OPEN, Some("Public domain")),
    ("cc0", LICENCE_OPEN, Some("CC0 1.0")),
    ("cc0-1.0", LICENCE_OPEN, Some("CC0 1.0")),
    ("cc-by", LICENCE_OPEN, Some("CC BY 4.0")),
    ("cc-by-4.0", LICENCE_OPEN, Some("CC BY 4.0")),
    ("cc-by-3.0", LICENCE_OPEN, Some("CC BY 3.0")),
    ("cc-by-sa", LICENCE_OPEN, Some("CC BY-SA 4.0")),
    ("cc-by-sa-4.0", LICENCE_OPEN, Some("CC BY-SA 4.0")),
    ("cc-by-sa-3.0", LICENCE_OPEN, Some("CC BY-SA 3.0")),
    ("mit", LICENCE_OPEN, Some("MIT")),
    ("apache-2.0", LICENCE_OPEN, Some("Apache 2.0")),
    ("apache", LICENCE_OPEN, Some("Apache 2.0")),
    ("bsd", LICENCE_OPEN, Some("BSD")),
    ("bsd-2-clause", LICENCE_OPEN, Some("BSD 2-Clause")),
    ("bsd-3-clause", LICENCE_OPEN, Some("BSD 3-Clause")),
    ("gfdl", LICENCE_OPEN, Some("GFDL")),
    ("cc-by-nc", LICENCE_RESTRICTED, Some("CC BY-NC 4.0")),
    ("cc-by-nc-4.0", LICENCE_RESTRICTED, Some("CC BY-NC 4.0")),
    ("cc-by-nd", LICENCE_RESTRICTED, Some("CC BY-ND 4.0")),
    ("cc-by-nd-4.0", LICENCE_RESTRICTED, Some("CC BY-ND 4.0")),
    ("cc-by-nc-sa", LICENCE_RESTRICTED, Some("CC BY-NC-SA 4.0")),
    (
        "cc-by-nc-sa-4.0",
        LICENCE_RESTRICTED,
        Some("CC BY-NC-SA 4.0"),
    ),
    ("cc-by-nc-nd", LICENCE_RESTRICTED, Some("CC BY-NC-ND 4.0")),
    (
        "cc-by-nc-nd-4.0",
        LICENCE_RESTRICTED,
        Some("CC BY-NC-ND 4.0"),
    ),
    (
        "all-rights-reserved",
        LICENCE_RESTRICTED,
        Some("All rights reserved"),
    ),
    ("arr", LICENCE_RESTRICTED, Some("All rights reserved")),
    ("copyright", LICENCE_RESTRICTED, Some("All rights reserved")),
    ("proprietary", LICENCE_RESTRICTED, Some("Proprietary")),
    ("commercial", LICENCE_RESTRICTED, Some("Commercial")),
    ("paywalled", LICENCE_RESTRICTED, Some("Paywalled")),
    (
        "crown-copyright",
        LICENCE_RESTRICTED,
        Some("Crown copyright (not under OGL)"),
    ),
];

const PROVENANCE_ALIASES: &[(&str, &str)] = &[
    ("retrieved", PROVENANCE_RETRIEVED),
    ("external", PROVENANCE_RETRIEVED),
    ("source", PROVENANCE_RETRIEVED),
    ("looked-up", PROVENANCE_RETRIEVED),
    ("concluded", PROVENANCE_CONCLUDED),
    ("inferred", PROVENANCE_CONCLUDED),
    ("derived", PROVENANCE_CONCLUDED),
    ("reasoned", PROVENANCE_CONCLUDED),
    ("system", PROVENANCE_CONCLUDED),
    ("asserted", PROVENANCE_ASSERTED),
    ("stated", PROVENANCE_ASSERTED),
    ("user", PROVENANCE_ASSERTED),
    ("human", PROVENANCE_ASSERTED),
    ("dictated", PROVENANCE_ASSERTED),
];

pub fn resolve_licence(raw: &str) -> Result<(&'static str, Option<&'static str>)> {
    let key = normalise_alias_key(raw);
    LICENCE_ALIASES
        .iter()
        .find(|(alias, _, _)| *alias == key)
        .map(|(_, class, note)| (*class, *note))
        .ok_or_else(|| {
            invalid(format!(
                "Unrecognised licence '{raw}'. Use one of {}, or a known identifier such as \
                 'ogl-3.0', 'cc-by-4.0' or 'all-rights-reserved'.",
                LICENCE_CLASSES.join(", ")
            ))
        })
}

pub fn default_licence(namespace: &str) -> &'static str {
    if matches!(namespace, "episodic" | "project" | "preference") {
        LICENCE_OWN
    } else {
        LICENCE_UNKNOWN
    }
}

fn validate_licence_note(note: Option<&str>) -> Result<Option<String>> {
    let Some(note) = note else { return Ok(None) };
    let note = note.split_whitespace().collect::<Vec<_>>().join(" ");
    if note.is_empty() {
        return Ok(None);
    }
    if note.chars().count() > MAX_LICENCE_NOTE {
        return Err(invalid(format!(
            "licence note must be {MAX_LICENCE_NOTE} characters or fewer"
        )));
    }
    Ok(Some(note))
}

/// Licence fields for a new write: the declared licence, or the namespace
/// default. An empty string means "not given".
pub fn licence_for_write(raw: Option<&str>, namespace: &str) -> Result<Fields> {
    let (class, note) = match raw.filter(|r| !r.is_empty()) {
        None => (default_licence(namespace), None),
        Some(raw) => resolve_licence(raw)?,
    };
    let mut fields = Fields::from([("licence".to_owned(), class.to_owned())]);
    if let Some(note) = validate_licence_note(note)? {
        fields.insert("licence_note".to_owned(), note);
    }
    Ok(fields)
}

fn get<'a>(doc: &'a Fields, name: &str) -> Option<&'a str> {
    doc.get(name).map(String::as_str).filter(|v| !v.is_empty())
}

/// The licence a stored record has, or would have had after the backfill.
pub fn effective_licence(doc: &Fields, namespace: &str) -> &'static str {
    if let Some(stored) = doc.get("licence")
        && let Some(class) = LICENCE_CLASSES.iter().find(|c| *c == stored)
    {
        return class;
    }
    if get(doc, "feed_name").is_some() || get(doc, "imported_at").is_some() {
        return LICENCE_UNKNOWN;
    }
    if get(doc, "enriched_from").is_some() {
        return LICENCE_OWN;
    }
    if namespace == "knowledge" {
        return LICENCE_UNKNOWN;
    }
    LICENCE_OWN
}

pub fn resolve_provenance(raw: &str) -> Result<&'static str> {
    let key = normalise_alias_key(raw);
    PROVENANCE_ALIASES
        .iter()
        .find(|(alias, _)| *alias == key)
        .map(|(_, class)| *class)
        .ok_or_else(|| {
            invalid(format!(
                "Unrecognised provenance '{raw}'. Use one of {}.",
                PROVENANCE_CLASSES.join(", ")
            ))
        })
}

pub fn default_provenance(namespace: &str) -> &'static str {
    match namespace {
        "preference" => PROVENANCE_ASSERTED,
        "knowledge" => PROVENANCE_RETRIEVED,
        _ => PROVENANCE_CONCLUDED,
    }
}

pub fn provenance_for_write(raw: Option<&str>, namespace: &str) -> Result<&'static str> {
    match raw.filter(|r| !r.is_empty()) {
        None => Ok(default_provenance(namespace)),
        Some(raw) => resolve_provenance(raw),
    }
}

/// The provenance a stored record has, or would have had after the backfill.
pub fn effective_provenance(doc: &Fields, namespace: &str, key: Option<&str>) -> &'static str {
    if let Some(stored) = doc.get("provenance")
        && let Some(class) = PROVENANCE_CLASSES.iter().find(|c| *c == stored)
    {
        return class;
    }
    if get(doc, "feed_name").is_some() {
        return PROVENANCE_RETRIEVED;
    }
    if get(doc, "enriched_from").is_some() {
        return PROVENANCE_CONCLUDED;
    }
    if namespace == "preference" {
        return PROVENANCE_ASSERTED;
    }
    if namespace == "project" {
        let context = get(doc, "project_name")
            .is_some_and(|name| key == Some(format!("mem:project:{name}").as_str()));
        if context || get(doc, "stack").is_some() || get(doc, "goals").is_some() {
            return PROVENANCE_ASSERTED;
        }
    }
    PROVENANCE_CONCLUDED
}

pub fn is_classifiable_key(key: &str) -> bool {
    CLASSIFIABLE_PREFIXES.iter().any(|p| key.starts_with(p))
}

/// `licence`, `licence_note` (when set) and `provenance`, in that order.
pub fn classification_fields(
    doc: &Fields,
    namespace: &str,
    key: Option<&str>,
) -> Map<String, Value> {
    let mut out = Map::new();
    if key.is_some_and(|k| !is_classifiable_key(k)) {
        return out;
    }
    out.insert("licence".into(), effective_licence(doc, namespace).into());
    if let Some(note) = get(doc, "licence_note") {
        out.insert("licence_note".into(), note.into());
    }
    out.insert(
        "provenance".into(),
        effective_provenance(doc, namespace, key).into(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(pairs: &[(&str, &str)]) -> Fields {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn identifiers_resolve_case_and_separator_insensitively() {
        assert_eq!(
            resolve_licence("CC BY 4.0").unwrap(),
            ("open", Some("CC BY 4.0"))
        );
        assert_eq!(
            resolve_licence("all_rights_reserved").unwrap(),
            ("restricted", Some("All rights reserved"))
        );
        assert_eq!(resolve_licence("own").unwrap(), ("own", None));
        assert!(resolve_licence("gpl-banana").is_err());
        assert_eq!(resolve_provenance("Looked_Up").unwrap(), "retrieved");
        assert!(resolve_provenance("gossip").is_err());
    }

    #[test]
    fn write_defaults_follow_the_namespace() {
        assert_eq!(
            licence_for_write(None, "episodic").unwrap()["licence"],
            "own"
        );
        assert_eq!(
            licence_for_write(Some(""), "knowledge").unwrap()["licence"],
            "unknown"
        );
        let ogl = licence_for_write(Some("ogl-3.0"), "knowledge").unwrap();
        assert_eq!(ogl["licence"], "open");
        assert_eq!(ogl["licence_note"], "OGL v3.0");
        assert_eq!(
            provenance_for_write(None, "preference").unwrap(),
            "asserted"
        );
        assert_eq!(
            provenance_for_write(None, "knowledge").unwrap(),
            "retrieved"
        );
        assert_eq!(provenance_for_write(None, "project").unwrap(), "concluded");
    }

    #[test]
    fn read_time_fallbacks_match_the_backfill() {
        assert_eq!(
            effective_licence(&doc(&[("feed_name", "F")]), "knowledge"),
            "unknown"
        );
        assert_eq!(
            effective_licence(&doc(&[("enriched_from", "x")]), "knowledge"),
            "own"
        );
        assert_eq!(
            effective_licence(&doc(&[("licence", "bogus")]), "episodic"),
            "own"
        );
        assert_eq!(
            effective_provenance(
                &doc(&[("project_name", "omnimem")]),
                "project",
                Some("mem:project:omnimem")
            ),
            "asserted"
        );
        assert_eq!(
            effective_provenance(
                &doc(&[("project_name", "omnimem")]),
                "project",
                Some("mem:project:01ULID")
            ),
            "concluded"
        );
    }

    #[test]
    fn skills_carry_no_classification() {
        assert!(
            classification_fields(&doc(&[]), "skill", Some("mem:skill:gen:x-local")).is_empty()
        );
        let fields = classification_fields(
            &doc(&[("licence_note", "MIT")]),
            "knowledge",
            Some("mem:knowledge:a"),
        );
        assert_eq!(
            fields.keys().collect::<Vec<_>>(),
            ["licence", "licence_note", "provenance"]
        );
    }
}
