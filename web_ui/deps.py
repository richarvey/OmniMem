"""Shared dependencies for the web UI — same init pattern as mcp_server/server.py."""

import logging

from memory.store import ValkeyStore
from memory.embedder import Embedder
from memory.lifecycle import MemoryLifecycle
from memory.recall import RecallPipeline

logger = logging.getLogger(__name__)

store: ValkeyStore | None = None
embedder: Embedder | None = None
lifecycle: MemoryLifecycle | None = None
pipeline: RecallPipeline | None = None


def init() -> None:
    """Initialise Valkey store, embedder, lifecycle, and recall pipeline."""
    global store, embedder, lifecycle, pipeline

    logger.info("Initialising web UI dependencies...")

    store = ValkeyStore()
    store.connect()

    embedder = Embedder()
    embedder.load()

    lifecycle = MemoryLifecycle(store)
    pipeline = RecallPipeline(store, embedder, lifecycle)

    logger.info("Web UI dependencies initialised")
