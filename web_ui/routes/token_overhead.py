"""Token overhead estimation: static + dynamic cost of running OmniMem."""

import os
import time

from starlette.requests import Request
from starlette.responses import HTMLResponse
from starlette.routing import Route

from .. import deps

_NAMESPACE_PREFIXES = ["mem:episodic:", "mem:project:", "mem:knowledge:"]

# MCP instructions text size (measured from instructions.py).
# Update if instructions change significantly.
_INSTRUCTIONS_CHARS = int(os.getenv("OMNIMEM_INSTRUCTIONS_CHARS", "5600"))

# Tool schema overhead — 32 tools (31 registered + health).
# Each tool contributes name + description + JSON parameter schema.
_TOOL_COUNT = 32
_TOOL_SCHEMAS_CHARS = int(os.getenv("OMNIMEM_TOOL_SCHEMAS_CHARS", "5835"))

# Deferred tool name overhead (mcp__omnimem__<name> listed in system prompt)
_DEFERRED_NAMES_CHARS = _TOOL_COUNT * 25  # ~25 chars per entry

# Token estimation: ~4 characters per token (conservative for English + JSON)
_CHARS_PER_TOKEN = 4

# Typical per-call token estimates (request + response combined)
_TOKENS_PER_BRIEFING = 1200
_TOKENS_PER_RECALL = 600
_TOKENS_PER_REMEMBER = 250
_TOKENS_PER_WARN = 150
_TOKENS_PER_UPDATE = 200

# Typical session call counts (low / high estimates)
_SESSION_CALLS = {
    "briefing": (1, 1),
    "recall": (3, 5),
    "remember": (2, 3),
    "warn_if_abandoned": (1, 2),
    "update_project_state": (1, 1),
}


def _chars_to_tokens(chars: int) -> int:
    """Convert character count to estimated token count."""
    return chars // _CHARS_PER_TOKEN


def _build_token_data(project_filter: str | None = None) -> dict:
    """Compute token overhead estimates from stored data."""

    # --- Static overhead (always in LLM context per session) ---
    instructions_tokens = _chars_to_tokens(_INSTRUCTIONS_CHARS)
    tool_schemas_tokens = _chars_to_tokens(_TOOL_SCHEMAS_CHARS)
    deferred_names_tokens = _chars_to_tokens(_DEFERRED_NAMES_CHARS)
    static_total_chars = _INSTRUCTIONS_CHARS + _TOOL_SCHEMAS_CHARS + _DEFERRED_NAMES_CHARS
    static_total_tokens = instructions_tokens + tool_schemas_tokens + deferred_names_tokens

    # --- Dynamic data from store ---
    total_memories = 0
    total_recalls = 0
    total_content_chars = 0
    namespace_counts = {"episodic": 0, "project": 0, "knowledge": 0}

    for prefix in _NAMESPACE_PREFIXES:
        namespace = prefix.split(":")[1]
        keys = deps.store.scan_prefix(prefix)
        if not keys:
            continue

        all_data = deps.store.get_multi(keys)

        for data in all_data:
            if data is None:
                continue
            state = data.get("state", "active")
            if state in ("archived", "deleted"):
                continue

            mem_project = data.get("project") or data.get("project_name")
            if project_filter and mem_project != project_filter:
                continue

            total_memories += 1
            namespace_counts[namespace] = namespace_counts.get(namespace, 0) + 1
            total_recalls += int(data.get("recall_count") or 0)
            total_content_chars += len(data.get("content", ""))

    avg_content_chars = total_content_chars // total_memories if total_memories else 0
    avg_content_tokens = _chars_to_tokens(avg_content_chars)

    # --- Dynamic session estimates ---
    dynamic_low = (
        _SESSION_CALLS["briefing"][0] * _TOKENS_PER_BRIEFING
        + _SESSION_CALLS["recall"][0] * _TOKENS_PER_RECALL
        + _SESSION_CALLS["remember"][0] * _TOKENS_PER_REMEMBER
        + _SESSION_CALLS["warn_if_abandoned"][0] * _TOKENS_PER_WARN
        + _SESSION_CALLS["update_project_state"][0] * _TOKENS_PER_UPDATE
    )
    dynamic_high = (
        _SESSION_CALLS["briefing"][1] * _TOKENS_PER_BRIEFING
        + _SESSION_CALLS["recall"][1] * _TOKENS_PER_RECALL
        + _SESSION_CALLS["remember"][1] * _TOKENS_PER_REMEMBER
        + _SESSION_CALLS["warn_if_abandoned"][1] * _TOKENS_PER_WARN
        + _SESSION_CALLS["update_project_state"][1] * _TOKENS_PER_UPDATE
    )

    # --- Per-call breakdown for the table ---
    call_breakdown = [
        {
            "name": "briefing()",
            "calls_low": _SESSION_CALLS["briefing"][0],
            "calls_high": _SESSION_CALLS["briefing"][1],
            "tokens_per_call": _TOKENS_PER_BRIEFING,
            "subtotal_low": _SESSION_CALLS["briefing"][0] * _TOKENS_PER_BRIEFING,
            "subtotal_high": _SESSION_CALLS["briefing"][1] * _TOKENS_PER_BRIEFING,
        },
        {
            "name": "recall()",
            "calls_low": _SESSION_CALLS["recall"][0],
            "calls_high": _SESSION_CALLS["recall"][1],
            "tokens_per_call": _TOKENS_PER_RECALL,
            "subtotal_low": _SESSION_CALLS["recall"][0] * _TOKENS_PER_RECALL,
            "subtotal_high": _SESSION_CALLS["recall"][1] * _TOKENS_PER_RECALL,
        },
        {
            "name": "remember()",
            "calls_low": _SESSION_CALLS["remember"][0],
            "calls_high": _SESSION_CALLS["remember"][1],
            "tokens_per_call": _TOKENS_PER_REMEMBER,
            "subtotal_low": _SESSION_CALLS["remember"][0] * _TOKENS_PER_REMEMBER,
            "subtotal_high": _SESSION_CALLS["remember"][1] * _TOKENS_PER_REMEMBER,
        },
        {
            "name": "warn_if_abandoned()",
            "calls_low": _SESSION_CALLS["warn_if_abandoned"][0],
            "calls_high": _SESSION_CALLS["warn_if_abandoned"][1],
            "tokens_per_call": _TOKENS_PER_WARN,
            "subtotal_low": _SESSION_CALLS["warn_if_abandoned"][0] * _TOKENS_PER_WARN,
            "subtotal_high": _SESSION_CALLS["warn_if_abandoned"][1] * _TOKENS_PER_WARN,
        },
        {
            "name": "update_project_state()",
            "calls_low": _SESSION_CALLS["update_project_state"][0],
            "calls_high": _SESSION_CALLS["update_project_state"][1],
            "tokens_per_call": _TOKENS_PER_UPDATE,
            "subtotal_low": _SESSION_CALLS["update_project_state"][0] * _TOKENS_PER_UPDATE,
            "subtotal_high": _SESSION_CALLS["update_project_state"][1] * _TOKENS_PER_UPDATE,
        },
    ]

    return {
        # Static
        "instructions_chars": _INSTRUCTIONS_CHARS,
        "instructions_tokens": instructions_tokens,
        "tool_count": _TOOL_COUNT,
        "tool_schemas_chars": _TOOL_SCHEMAS_CHARS,
        "tool_schemas_tokens": tool_schemas_tokens,
        "deferred_names_chars": _DEFERRED_NAMES_CHARS,
        "deferred_names_tokens": deferred_names_tokens,
        "static_total_chars": static_total_chars,
        "static_total_tokens": static_total_tokens,
        # Store data
        "total_memories": total_memories,
        "total_recalls": total_recalls,
        "total_content_chars": total_content_chars,
        "total_content_tokens": _chars_to_tokens(total_content_chars),
        "avg_content_chars": avg_content_chars,
        "avg_content_tokens": avg_content_tokens,
        "namespace_counts": namespace_counts,
        # Dynamic session estimates
        "dynamic_low": dynamic_low,
        "dynamic_high": dynamic_high,
        "call_breakdown": call_breakdown,
        # Totals
        "session_total_low": static_total_tokens + dynamic_low,
        "session_total_high": static_total_tokens + dynamic_high,
        # Filter
        "project_filter": project_filter or "",
    }


async def token_overhead_page(request: Request) -> HTMLResponse:
    """GET /token-overhead — token overhead estimation page."""
    project_filter = request.query_params.get("project") or None
    data = _build_token_data(project_filter)

    template = request.app.state.templates.get_template("token_overhead.html")
    content = template.render(
        request=request,
        current_page="token_overhead",
        **data,
    )
    return HTMLResponse(content)


async def token_overhead_refresh(request: Request) -> HTMLResponse:
    """GET /token-overhead/refresh — htmx partial refresh."""
    project_filter = request.query_params.get("project") or None
    data = _build_token_data(project_filter)

    template = request.app.state.templates.get_template("partials/token_overhead_content.html")
    content = template.render(
        request=request,
        **data,
    )
    return HTMLResponse(content)


routes = [
    Route("/token-overhead", token_overhead_page),
    Route("/token-overhead/refresh", token_overhead_refresh),
]
