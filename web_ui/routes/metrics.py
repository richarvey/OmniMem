"""Prometheus /metrics endpoint — point-in-time gauges read from Valkey."""

from prometheus_client import CollectorRegistry, Gauge, generate_latest
from starlette.requests import Request
from starlette.responses import Response
from starlette.routing import Route

from .. import deps

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


async def metrics_endpoint(request: Request) -> Response:
    """GET /metrics — Prometheus text format."""
    import os
    import time

    cold_days = int(os.getenv("TELEMETRY_COLD_DAYS", "60"))
    cold_threshold = time.time() - (cold_days * 86400)

    # Reset all gauges before scanning
    _memories_total._metrics.clear()
    _memories_never_recalled._metrics.clear()

    total_recalls = 0
    gone_cold_count = 0

    for namespace, prefix in _NAMESPACE_PREFIXES.items():
        keys = deps.store.scan_prefix(prefix)
        if not keys:
            continue

        all_data = deps.store.get_multi(keys)
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

    output = generate_latest(_registry)
    return Response(
        content=output,
        media_type="text/plain; version=0.0.4; charset=utf-8",
    )


routes = [
    Route("/metrics", metrics_endpoint),
]
