"""Shared tool dependencies — set by server.py at startup."""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from memory.embedder import Embedder
    from memory.lifecycle import MemoryLifecycle
    from memory.recall import RecallPipeline
    from memory.store import ValkeyStore

__version__ = "3.1.3"

_store: ValkeyStore = None  # type: ignore[assignment]
_embedder: Embedder = None  # type: ignore[assignment]
_lifecycle: MemoryLifecycle = None  # type: ignore[assignment]
_pipeline: RecallPipeline = None  # type: ignore[assignment]


def _compact(d: dict[str, Any]) -> dict[str, Any]:
    """Strip None and empty values from response dict to reduce token usage."""
    out: dict[str, Any] = {}
    for k, v in d.items():
        if v is None:
            continue
        if isinstance(v, str) and not v:
            continue
        if isinstance(v, list) and not v:
            continue
        if isinstance(v, dict) and not v:
            continue
        out[k] = v
    return out
