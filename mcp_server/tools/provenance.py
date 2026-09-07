"""Provenance tool: reclassify where stored memories came from.

Every memory carries a ``provenance`` (see memory/provenance.py). Writes
stamp it at ingest; this tool corrects it afterwards — most often to mark
a memory the human vouches for as ``asserted`` after the backfill defaulted
it to ``concluded``.
"""

import logging
from typing import Any

from memory.lineage import partition_keys
from memory.provenance import classify_provenance, resolve_provenance

from . import _compact

logger = logging.getLogger(__name__)


def _get_deps():
    from tools import _store
    return _store


def set_provenance(
    provenance: str,
    keys: list[str],
) -> dict[str, Any]:
    """Record where one or more memories came from.

    Use it when the human vouches for a memory ('asserted'), when a memory
    turns out to be a copy of an external source ('retrieved'), or when
    something recorded as stated was actually your own inference
    ('concluded'). Facts extracted from a reclassified memory follow it.
    Provenance is reported on recall and never changes ranking.

    Args:
        provenance: 'retrieved' (external source), 'concluded' (the
            system's own reasoning or write-up), or 'asserted' (stated by
            the human).
        keys: Memory keys to reclassify (max 200 per call).
    """
    store = _get_deps()

    try:
        provenance_class = resolve_provenance(provenance)
    except ValueError as exc:
        return {"error": str(exc)}

    valid, skipped, error = partition_keys(keys, "provenance")
    if error:
        return error

    outcome = classify_provenance(store, valid, provenance_class)
    if outcome["classified"]:
        logger.info(
            "Reclassified %d memories as %s (%d extracted facts followed)",
            len(outcome["classified"]), provenance_class, len(outcome["cascaded"]),
        )

    return _compact({
        "provenance": provenance_class,
        "classified": len(outcome["classified"]),
        "keys": outcome["classified"],
        "cascaded_facts": outcome["cascaded"],
        "not_found": outcome["not_found"],
        "skipped": skipped,
    })
