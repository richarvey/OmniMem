"""Licence tool: classify the redistribution rights of stored memories.

Every memory carries a ``licence`` field (see memory/licence.py). New
writes are stamped at ingest; this tool is for the records that arrived
without a declaration — RSS articles from a feed with no ``licence:`` in
feeds.yml, and everything backfilled as ``unknown`` on upgrade. Recall
points those out; this is how the human's answer gets recorded.
"""

import logging
from typing import Any

from memory.licence import (
    classify_memories,
    licence_fields,
    resolve_licence,
    validate_licence_note,
)
from memory.lineage import partition_keys

from . import _compact

logger = logging.getLogger(__name__)


def _get_deps():
    from tools import _store
    return _store


def set_licence(
    licence: str,
    keys: list[str] | None = None,
    feed_name: str | None = None,
    note: str | None = None,
) -> dict[str, Any]:
    """Record the redistribution rights of one or more memories.

    Give either specific keys, or a feed_name to classify every article
    ingested from that RSS feed at once. Use it when recall reports results
    with an unknown licence and the human can say where the content stands,
    or to correct a wrong classification. Facts extracted from a classified
    memory take the same licence automatically.

    Args:
        licence: 'own' (written here), 'open' (third-party, redistributable —
            OGL, CC BY, public domain), 'restricted' (third-party, not
            redistributable — paywalled, all rights reserved, CC BY-NC/ND),
            or 'unknown'. A recognised identifier such as 'cc-by-4.0',
            'ogl-3.0' or 'all-rights-reserved' is accepted and kept as the
            note.
        keys: Memory keys to classify (max 200 per call).
        feed_name: Classify every knowledge article from this RSS feed
            instead. Only affects articles already stored — set `licence:` on
            the feed itself (feeds.yml or the web UI feed editor) so future
            articles arrive classified.
        note: Optional free-text detail — the specific licence or where it
            was checked (max 200 chars). Overrides the identifier-derived
            note.
    """
    store = _get_deps()

    if bool(keys) == bool(feed_name):
        return {"error": "Give either keys or feed_name, not both and not neither"}

    try:
        licence_class, derived_note = resolve_licence(licence)
        note = validate_licence_note(note) or derived_note
        fields = licence_fields(licence_class, note)
    except ValueError as exc:
        return {"error": str(exc)}
    # Reclassifying must not leave a stale note behind ("CC BY 4.0" on a
    # record now marked restricted) — hashes can't drop a field in a bulk
    # HSET, so an absent note is written as empty, which reads as none.
    fields.setdefault("licence_note", "")

    if feed_name:
        # Articles are never enriched, so there is nothing to cascade to.
        targets = _article_keys_for_feed(store, feed_name)
        if not targets:
            return {"error": f"No knowledge articles found for feed '{feed_name}'"}
        written = store.set_fields_multi(targets, fields)
        logger.info(
            "Classified %d articles from feed %s as %s", written, feed_name, licence_class,
        )
        return _compact({
            "feed_name": feed_name,
            "licence": licence_class,
            "licence_note": fields["licence_note"],
            "classified": written,
            "note": (
                "Existing articles only. Set `licence:` on this feed in feeds.yml "
                "or the web UI feed editor so future articles arrive classified."
            ),
        })

    valid, skipped, error = partition_keys(keys or [], "licence")
    if error:
        return error

    outcome = classify_memories(store, valid, licence_class, note)
    if outcome["classified"]:
        logger.info(
            "Classified %d memories as %s (%d extracted facts followed)",
            len(outcome["classified"]), licence_class, len(outcome["cascaded"]),
        )

    return _compact({
        "licence": licence_class,
        "licence_note": fields["licence_note"],
        "classified": len(outcome["classified"]),
        "keys": outcome["classified"],
        "cascaded_facts": outcome["cascaded"],
        "not_found": outcome["not_found"],
        "skipped": skipped,
    })


def _article_keys_for_feed(store, feed_name: str) -> list[str]:
    """Knowledge keys whose feed_name matches exactly."""
    keys = store.scan_prefix("mem:knowledge:")
    if not keys:
        return []
    rows = store.get_fields_multi(keys, ("feed_name",))
    return [k for k, row in zip(keys, rows) if row and row.get("feed_name") == feed_name]

