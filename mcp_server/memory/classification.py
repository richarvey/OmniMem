"""The classification fields every read surface reports, from one place.

recall, recall_detail, recent_knowledge, the briefing's new-knowledge list,
explain_memory and the web UI all merge this in, so the shape (and the
read-time fallback rules behind it) cannot drift between them.
"""

from __future__ import annotations

from typing import Any

from .licence import effective_licence
from .lineage import is_classifiable_key
from .provenance import effective_provenance


def classification_fields(doc: dict, namespace: str, key: str | None = None) -> dict[str, Any]:
    """``licence``, ``licence_note`` (when set) and ``provenance`` for a
    stored record — empty for a key that carries none (skills)."""
    if key is not None and not is_classifiable_key(key):
        return {}
    fields: dict[str, Any] = {"licence": effective_licence(doc, namespace)}
    if doc.get("licence_note"):
        fields["licence_note"] = doc["licence_note"]
    fields["provenance"] = effective_provenance(doc, namespace, key)
    return fields
