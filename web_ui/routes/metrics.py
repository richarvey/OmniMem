"""Prometheus /metrics endpoint — point-in-time gauges read from Valkey.

Caches computed metrics for METRICS_CACHE_TTL seconds (default 60) to avoid
scanning all memories on every Prometheus scrape.
"""

import os
import time

from prometheus_client import CollectorRegistry, Gauge, generate_latest
from starlette.requests import Request
from starlette.responses import Response
from starlette.routing import Route

from .. import deps

METRICS_CACHE_TTL = int(os.getenv("METRICS_CACHE_TTL", "60"))

_NAMESPACE_PREFIXES = {
    "episodic": "mem:episodic:",
    "project": "mem:project:",
    "knowledge": "mem:knowledge:",
}

# Dedicated registry to avoid default process/platform collectors
_registry = CollectorRegistry()

_memories_total = Gauge(
    "omnimem_memories_total",
    "Total active memories by namespace and state",
    ["namespace", "state"],
    registry=_registry,
)

_memories_never_recalled = Gauge(
    "omnimem_memories_never_recalled",
    "Active memories with recall_count=0 by namespace",
    ["namespace"],
    registry=_registry,
)

_recalls_total = Gauge(
    "omnimem_recalls_total",
    "Sum of all recall_count values across all memories",
    registry=_registry,
)

_memories_gone_cold = Gauge(
    "omnimem_memories_gone_cold",
    "Memories recalled before but not in the cold threshold period",
    registry=_registry,
)

_tool_calls_total = Gauge(
    "omnimem_tool_calls_total",
    "Total tool call count by tool name",
    ["tool"],
    registry=_registry,
)

_tool_errors_total = Gauge(
    "omnimem_tool_errors_total",
    "Total tool call errors by tool name",
    ["tool"],
    registry=_registry,
)

# Simple time-based cache
_cache: dict = {"output": None, "computed_at": 0.0}


def _recompute_metrics() -> bytes:
    """Scan Valkey and set all gauge values. Returns serialised Prometheus output."""
    cold_days = int(os.getenv("TELEMETRY_COLD_DAYS", "60"))
    cold_threshold = time.time() - (cold_days * 86400)

    _memories_total._metrics.clear()
    _memories_never_recalled._metrics.clear()
    _tool_calls_total._metrics.clear()
    _tool_errors_total._metrics.clear()

    total_recalls = 0
    gone_cold_count = 0

    for namespace, prefix in _NAMESPACE_PREFIXES.items():
        keys = deps.store.scan_prefix(prefix)
        if not keys:
            continue

        all_data = deps.store.get_fields_multi(
            keys, ("state", "recall_count", "last_recalled")
        )
        state_counts: dict[str, int] = {}
        never_recalled = 0

        for data in all_data:
            if data is None:
                continue

            state = data.get("state", "active")
            state_counts[state] = state_counts.get(state, 0) + 1

            if state in ("archived", "deleted"):
                continue

            recall_count = int(data.get("recall_count") or 0)
            total_recalls += recall_count

            if recall_count == 0:
                never_recalled += 1
            else:
                last_recalled_raw = data.get("last_recalled")
                if last_recalled_raw and float(last_recalled_raw) < cold_threshold:
                    gone_cold_count += 1

        for state, count in state_counts.items():
            _memories_total.labels(namespace=namespace, state=state).set(count)

        _memories_never_recalled.labels(namespace=namespace).set(never_recalled)

    _recalls_total.set(total_recalls)
    _memories_gone_cold.set(gone_cold_count)

    # Tool call metrics
    from .token_overhead import read_tool_metrics
    tool_metrics = read_tool_metrics(deps.store)
    for tool_name, data in tool_metrics.items():
        _tool_calls_total.labels(tool=tool_name).set(data["call_count"])
        _tool_errors_total.labels(tool=tool_name).set(data["error_count"])

    return generate_latest(_registry)


async def metrics_endpoint(request: Request) -> Response:
    """GET /metrics — Prometheus text format (cached)."""
    now = time.time()
    if _cache["output"] is None or (now - _cache["computed_at"]) >= METRICS_CACHE_TTL:
        _cache["output"] = _recompute_metrics()
        _cache["computed_at"] = now

    return Response(
        content=_cache["output"],
        media_type="text/plain; version=0.0.4; charset=utf-8",
    )


routes = [
    Route("/metrics", metrics_endpoint),
]
