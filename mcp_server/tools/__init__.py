"""Shared tool dependencies — set by server.py at startup."""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from ..memory.embedder import Embedder
    from ..memory.lifecycle import MemoryLifecycle
    from ..memory.recall import RecallPipeline
    from ..memory.store import ValkeyStore

_store: ValkeyStore = None  # type: ignore[assignment]
_embedder: Embedder = None  # type: ignore[assignment]
_lifecycle: MemoryLifecycle = None  # type: ignore[assignment]
_pipeline: RecallPipeline = None  # type: ignore[assignment]
