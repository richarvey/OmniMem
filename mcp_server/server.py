"""OmniMem MCP server entry point."""

import logging
import os
import signal
import sys
import time

from dotenv import load_dotenv
from fastmcp import FastMCP

load_dotenv()

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
)
logger = logging.getLogger("omnimem")

mcp = FastMCP("omnimem")

_start_time = time.time()


def _init() -> None:
    """Initialise shared dependencies: Valkey store, embedder, lifecycle, pipeline."""
    from memory.embedder import Embedder
    from memory.lifecycle import MemoryLifecycle
    from memory.recall import RecallPipeline
    from memory.store import ValkeyStore

    import tools as tools_pkg

    logger.info("Initialising OmniMem...")

    store = ValkeyStore()
    store.connect()

    embedder = Embedder()
    embedder.load()

    lifecycle = MemoryLifecycle(store)
    pipeline = RecallPipeline(store, embedder, lifecycle)

    # Set shared deps for all tool modules
    tools_pkg._store = store
    tools_pkg._embedder = embedder
    tools_pkg._lifecycle = lifecycle
    tools_pkg._pipeline = pipeline

    logger.info("OmniMem initialised successfully")


def _register_tools() -> None:
    """Register all MCP tools from tool modules."""
    from tools.core import (
        remember, recall, deprioritise, archive, reinstate, forget,
        suppress_topic, unsuppress_topic, list_suppressions, find_duplicates,
    )
    from tools.project import (
        set_project_context, get_project_context, list_projects,
        update_project_state,
    )
    from tools.audit import memory_audit, why_did_you_mention, explain_memory
    from tools.experience import (
        record_experience, log_abandoned, get_experience,
        experience_summary, warn_if_abandoned,
    )
    from tools.backup import dump_to_file, restore_from_file, list_backups

    # Core tools
    mcp.tool()(remember)
    mcp.tool()(recall)
    mcp.tool()(deprioritise)
    mcp.tool()(archive)
    mcp.tool()(reinstate)
    mcp.tool()(forget)
    mcp.tool()(suppress_topic)
    mcp.tool()(unsuppress_topic)
    mcp.tool()(list_suppressions)
    mcp.tool()(find_duplicates)

    # Project tools
    mcp.tool()(set_project_context)
    mcp.tool()(get_project_context)
    mcp.tool()(list_projects)
    mcp.tool()(update_project_state)

    # Audit tools
    mcp.tool()(memory_audit)
    mcp.tool()(why_did_you_mention)
    mcp.tool()(explain_memory)

    # Experience tools
    mcp.tool()(record_experience)
    mcp.tool()(log_abandoned)
    mcp.tool()(get_experience)
    mcp.tool()(experience_summary)
    mcp.tool()(warn_if_abandoned)

    # Backup tools
    mcp.tool()(dump_to_file)
    mcp.tool()(restore_from_file)
    mcp.tool()(list_backups)

    logger.info("Registered all MCP tools")


@mcp.tool()
def health() -> dict:
    """Check OmniMem server health. Returns Valkey ping status, index counts, model loaded state, and uptime.

    Returns:
        Dict with valkey_connected, indexes, model_loaded, uptime_seconds.
    """
    import tools as tools_pkg

    result = {
        "valkey_connected": False,
        "indexes": {},
        "model_loaded": False,
        "uptime_seconds": round(time.time() - _start_time, 1),
    }

    try:
        store = tools_pkg._store
        if store and store._client:
            store.client.ping()
            result["valkey_connected"] = True

            for idx_name in ["idx:episodic", "idx:project", "idx:knowledge"]:
                try:
                    info = store.client.ft(idx_name).info()
                    result["indexes"][idx_name] = info.get("num_docs", 0)
                except Exception:
                    result["indexes"][idx_name] = "unavailable"
    except Exception:
        result["valkey_error"] = "connection_failed"

    try:
        embedder = tools_pkg._embedder
        if embedder:
            result["model_loaded"] = embedder.is_loaded
    except Exception:
        pass

    return result


def _handle_shutdown(signum: int, frame) -> None:
    """Graceful shutdown on SIGTERM/SIGINT."""
    logger.info("Received signal %d, shutting down gracefully...", signum)
    sys.exit(0)


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, _handle_shutdown)
    signal.signal(signal.SIGINT, _handle_shutdown)

    _init()
    _register_tools()

    port = int(os.getenv("MCP_PORT", "8765"))
    host = os.getenv("MCP_HOST", "127.0.0.1")
    logger.info("Starting OmniMem MCP server on %s:%d (SSE)", host, port)
    mcp.run(transport="sse", host=host, port=port)
