"""Stamp classification fields on memories and their derived facts.

A fact extracted from a memory is a restatement of it: it carries exactly
its source's redistribution rights and its source's provenance. So when a
human classifies a memory, the facts linked to it must follow, or recall
keeps raising the facts as unclassified after the source was answered.

Facts name their source two ways: ``enriched_from`` holds a source key, and
``source_doc_id`` holds either that key or, for batch enrichment of a
chunked document, the document's ``doc_id`` (no single chunk's key). Both
are followed here. This module is the one write path for the set_licence
and set_provenance tools and the web UI's detail forms, so none of them can
drift on these rules.
"""

from __future__ import annotations

from typing import Any

# Only these namespaces carry classification fields. Skills are derived
# from their sources; everything else under mem: (and every meta:/topics:/
# log: key) is not a memory at all.
CLASSIFIABLE_PREFIXES = tuple(
    f"mem:{ns}:" for ns in ("episodic", "project", "knowledge", "preference")
)


def is_classifiable_key(key: str) -> bool:
    """True for a key in a namespace that carries classification fields."""
    return key.startswith(CLASSIFIABLE_PREFIXES)


# Keys a classification tool will take in one call.
MAX_KEYS_PER_CALL = 200


def normalise_alias_key(raw: Any) -> str:
    """Case- and separator-insensitive alias lookup key: 'CC BY 4.0' →
    'cc-by-4.0', 'Looked_Up' → 'looked-up'. Shared by the licence and
    provenance resolvers so the two can't drift on matching rules."""
    text = raw if isinstance(raw, str) else ("" if raw is None else str(raw))
    return "-".join(text.strip().lower().replace("_", " ").replace("-", " ").split())


def partition_keys(keys: list[Any], field: str) -> tuple[list[str], list[Any], dict[str, Any] | None]:
    """Split tool input into (classifiable, skipped, error).

    Shared by set_licence and set_provenance so the cap, the skill refusal
    and the error wording live once. ``error`` is a ready-to-return dict
    when the call cannot proceed at all.
    """
    if not keys:
        return [], [], {"error": "keys is required"}
    if len(keys) > MAX_KEYS_PER_CALL:
        return [], [], {
            "error": f"Too many keys ({len(keys)}); max {MAX_KEYS_PER_CALL} per call",
        }
    skills = [k for k in keys if isinstance(k, str) and k.startswith("mem:skill:")]
    if skills:
        # A compiled skill is derived from its sources; it carries no
        # classification of its own. Classify the memories it cites instead.
        return [], [], {
            "error": f"Compiled skills carry no {field} — classify the source "
                     "memories in the skill's manifest instead",
            "skill_keys": skills,
        }
    valid = [k for k in keys if isinstance(k, str) and is_classifiable_key(k)]
    skipped = [k for k in keys if not (isinstance(k, str) and is_classifiable_key(k))]
    if not valid:
        return [], skipped, {
            "error": "No valid memory keys given (mem:episodic:, mem:project:, "
                     "mem:knowledge: or mem:preference:)",
        }
    return valid, skipped, None


def stamp_lineage(
    store, keys: list[str], fields: dict[str, Any],
) -> dict[str, list[str]]:
    """Write ``fields`` onto ``keys`` and onto every fact derived from them.

    Returns ``{"classified": [...], "cascaded": [...], "not_found": [...]}``.
    ``updated_at`` is deliberately not touched: classification is metadata,
    not a content edit, and the skill compiler reads a bumped ``updated_at``
    as "source changed".
    """
    rows = store.get_fields_multi(keys, ("created_at", "state", "doc_id"))
    found = [k for k, row in zip(keys, rows) if row]
    missing = [k for k, row in zip(keys, rows) if not row]
    if not found:
        return {"classified": [], "cascaded": [], "not_found": missing}

    source_ids = set(found)
    for row in rows:
        if row and row.get("doc_id"):
            source_ids.add(row["doc_id"])
    cascaded: list[str] = []
    for ns in ("knowledge", "preference"):
        fact_keys = store.scan_prefix(f"mem:{ns}:")
        if not fact_keys:
            continue
        fact_rows = store.get_fields_multi(fact_keys, ("enriched_from", "source_doc_id"))
        cascaded.extend(
            k for k, row in zip(fact_keys, fact_rows)
            if row and k not in source_ids and (
                row.get("enriched_from") in source_ids
                or row.get("source_doc_id") in source_ids
            )
        )

    store.set_fields_multi(found + cascaded, fields)
    return {"classified": found, "cascaded": cascaded, "not_found": missing}
