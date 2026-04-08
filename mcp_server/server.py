"""OmniMem MCP server entry point."""

import logging
import os
import signal
import sys
import time

from dotenv import load_dotenv
from fastmcp import FastMCP

from instructions import INSTRUCTIONS

load_dotenv()

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
)
logger = logging.getLogger("omnimem")

# ---------------------------------------------------------------------------
# Authentication setup — bearer token, OAuth 2.1, or both
# ---------------------------------------------------------------------------

_auth_token = os.getenv("MCP_AUTH_TOKEN", "").strip()
_oauth_enabled = os.getenv("OAUTH_ENABLED", "").strip().lower() in ("true", "1", "yes")

_bearer_verifier = None
_oauth_provider = None
_auth = None

# Bearer token auth (unchanged from pre-v4)
if _auth_token:
    from fastmcp.server.auth import AccessToken, TokenVerifier

    class _SharedSecretAuth(TokenVerifier):
        """Validates a shared secret bearer token (not JWT)."""

        def __init__(self, token: str) -> None:
            super().__init__()
            self._token = token

        async def verify_token(self, token: str) -> AccessToken | None:
            if token == self._token:
                return AccessToken(token=token, client_id="omnimem", scopes=[])
            return None

    _bearer_verifier = _SharedSecretAuth(_auth_token)
    logger.info("Bearer token authentication enabled for MCP server")

# OAuth 2.1 auth — optional, enabled via OAUTH_ENABLED + admin credentials
if _oauth_enabled:
    _oauth_user = os.getenv("OAUTH_ADMIN_USER", "").strip()
    _oauth_pass = os.getenv("OAUTH_ADMIN_PASSWORD", "").strip()
    _oauth_base = os.getenv("OAUTH_BASE_URL", "").strip()

    if not _oauth_user or not _oauth_pass:
        logger.error(
            "OAUTH_ENABLED is set but OAUTH_ADMIN_USER and/or "
            "OAUTH_ADMIN_PASSWORD are missing — OAuth disabled"
        )
        _oauth_enabled = False
    elif not _oauth_base:
        logger.error(
            "OAUTH_ENABLED is set but OAUTH_BASE_URL is missing — "
            "set it to the externally-reachable URL (e.g. https://mcp.example.com)"
        )
        _oauth_enabled = False
    else:
        from oauth.provider import OmniMemOAuthProvider, _StoredToken
        from oauth.storage import ValkeyOAuthStorage

        try:
            _oauth_storage = ValkeyOAuthStorage(stored_token_cls=_StoredToken)
        except Exception as exc:
            logger.error(
                "OAuth Valkey storage failed to initialise (%s) — falling back "
                "to in-memory storage. Tokens will not survive restarts.",
                exc,
            )
            _oauth_storage = None

        _oauth_provider = OmniMemOAuthProvider(
            base_url=_oauth_base,
            admin_user=_oauth_user,
            admin_password=_oauth_pass,
            storage=_oauth_storage,
        )
        logger.info(
            "OAuth 2.1 authentication enabled (base URL: %s, storage: %s)",
            _oauth_base,
            "valkey" if _oauth_storage else "in-memory",
        )

# Combine auth sources with MultiAuth when both are active
if _oauth_provider and _bearer_verifier:
    from fastmcp.server.auth import MultiAuth

    _auth = MultiAuth(server=_oauth_provider, verifiers=[_bearer_verifier])
    logger.info("MultiAuth: OAuth 2.1 + bearer token")
elif _oauth_provider:
    _auth = _oauth_provider
elif _bearer_verifier:
    _auth = _bearer_verifier

mcp = FastMCP("omnimem", instructions=INSTRUCTIONS, auth=_auth)

# Register OAuth login routes (must happen after mcp is created)
if _oauth_provider:
    from oauth.routes import register_oauth_routes

    register_oauth_routes(mcp, _oauth_provider)

_start_time = time.time()


def _migrate_project_names(store) -> None:
    """Set project_name from project field on ULID-keyed project memories missing it."""
    keys = store.scan_prefix("mem:project:")
    if not keys:
        return

    all_data = store.get_multi(keys)
    fixed = 0
    for key, data in zip(keys, all_data):
        if not data:
            continue
        # Skip entries that already have project_name set
        if data.get("project_name"):
            continue
        # Use the project field if available
        project = data.get("project")
        if project:
            store.set_field(key, "project_name", project)
            fixed += 1

    if fixed:
        logger.info("Migration: set project_name on %d project memories", fixed)


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

    # One-time migration: set project_name on ULID-keyed project memories
    _migrate_project_names(store)

    logger.info("OmniMem initialised successfully")


def _register_tools() -> None:
    """Register all MCP tools from tool modules."""
    from tools.core import (
        version,
        remember, remember_document, recall, recall_index, recall_detail,
        deprioritise, archive, reinstate, forget,
        suppress_topic, unsuppress_topic, list_suppressions, find_duplicates,
    )
    from tools.project import (
        set_project_context, get_project_context, list_projects,
        update_project_state, compile_project_context,
    )
    from tools.audit import memory_audit, why_did_you_mention, explain_memory, reindex
    from tools.experience import (
        record_experience, log_abandoned, get_experience,
        experience_summary, warn_if_abandoned,
    )
    from tools.backup import dump_to_file, restore_from_file, list_backups
    from tools.contradiction import check_contradictions
    from tools.briefing import briefing
    from tools.knowledge import recent_knowledge, promote_knowledge

    # Core tools
    mcp.tool()(version)
    mcp.tool()(remember)
    mcp.tool()(remember_document)
    mcp.tool()(recall)
    mcp.tool()(recall_index)
    mcp.tool()(recall_detail)
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
    mcp.tool()(compile_project_context)

    # Audit tools
    mcp.tool()(memory_audit)
    mcp.tool()(why_did_you_mention)
    mcp.tool()(explain_memory)
    mcp.tool()(reindex)

    # Experience tools
    mcp.tool()(record_experience)
    mcp.tool()(log_abandoned)
    mcp.tool()(get_experience)
    mcp.tool()(experience_summary)
    mcp.tool()(warn_if_abandoned)

    # Contradiction tools
    mcp.tool()(check_contradictions)

    # Briefing tool
    mcp.tool()(briefing)

    # Knowledge tools
    mcp.tool()(recent_knowledge)
    mcp.tool()(promote_knowledge)

    # Backup tools
    mcp.tool()(dump_to_file)
    mcp.tool()(restore_from_file)
    mcp.tool()(list_backups)

    logger.info("Registered all MCP tools")


@mcp.tool()
def health() -> dict:
    """Server health: Valkey connection, index counts, model status, uptime."""
    import tools as tools_pkg

    result = {
        "valkey_connected": False,
        "indexes": {},
        "records": {},
        "drift": {},
        "model_loaded": False,
        "uptime_seconds": round(time.time() - _start_time, 1),
    }

    try:
        store = tools_pkg._store
        if store and store._client:
            store.client.ping()
            result["valkey_connected"] = True

            for namespace in ("episodic", "project", "knowledge", "preference"):
                idx_name = f"idx:{namespace}"
                num_docs: int | str
                try:
                    info = store.client.ft(idx_name).info()
                    num_docs = int(info.get("num_docs", 0))
                except Exception:
                    num_docs = "unavailable"
                result["indexes"][idx_name] = num_docs

                try:
                    actual = store.count_records(namespace)
                    result["records"][namespace] = actual
                    if isinstance(num_docs, int) and num_docs != actual:
                        result["drift"][namespace] = num_docs - actual
                except Exception:
                    result["records"][namespace] = "unavailable"
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

    from middleware.telemetry import ToolTelemetryMiddleware
    import tools as tools_pkg
    mcp.add_middleware(ToolTelemetryMiddleware(tools_pkg._store))

    port = int(os.getenv("MCP_PORT", "8765"))
    host = os.getenv("MCP_HOST", "127.0.0.1")
    transport = os.getenv("MCP_TRANSPORT", "sse")
    if transport == "sse":
        logger.warning(
            "SSE transport is deprecated and will be removed in a future release. "
            "Set MCP_TRANSPORT=http and update your client config to use "
            "type 'http' with URL http://<host>:<port>/mcp"
        )
    logger.info("Starting OmniMem MCP server on %s:%d (%s)", host, port, transport)
    mcp.run(transport=transport, host=host, port=port)
