"""Enrichment queue status tool."""

import logging
from typing import Any

from memory.enrichment import QUEUE_KEY

logger = logging.getLogger(__name__)


def _get_store():
    from tools import _store
    return _store


def queue_status() -> dict[str, Any]:
    """Check the enrichment queue. Returns the number of pending jobs waiting for background fact extraction.

    Use this to know when enrichment has finished after a batch ingest —
    poll until pending reaches 0 before running recall/scoring.
    """
    store = _get_store()
    pending = 0
    try:
        pending = store.client.llen(QUEUE_KEY)
    except Exception as exc:
        logger.warning("Failed to read enrichment queue length: %s", exc)
        return {"pending": -1, "error": str(exc)}

    return {"pending": pending}
