"""Telemetry dashboard: recall counters, most recalled, gone cold, never recalled."""

import os
import time

from starlette.requests import Request
from starlette.responses import HTMLResponse
from starlette.routing import Route

from .. import deps

TELEMETRY_COLD_DAYS = int(os.getenv("TELEMETRY_COLD_DAYS", "60"))
_NAMESPACE_PREFIXES = ["mem:episodic:", "mem:project:", "mem:knowledge:", "mem:preference:"]


def _build_telemetry_data(project_filter: str | None = None) -> dict:
    """Scan all memories and build telemetry stats."""
    now = time.time()
    cold_threshold = now - (TELEMETRY_COLD_DAYS * 86400)

    total_memories = 0
    total_recalls = 0
    unique_recalled = 0
    never_recalled = 0

    most_recalled: list[dict] = []
    gone_cold: list[dict] = []
    never_recalled_list: list[dict] = []

    for prefix in _NAMESPACE_PREFIXES:
        namespace = prefix.split(":")[1]
        keys = deps.store.scan_prefix(prefix)
        if not keys:
            continue

        all_data = deps.store.get_fields_multi(
            keys,
            ("state", "project", "project_name", "recall_count",
             "last_recalled", "content", "created_at"),
        )

        for key, data in zip(keys, all_data):
            if data is None:
                continue

            state = data.get("state", "active")
            if state in ("archived", "deleted"):
                continue

            mem_project = data.get("project") or data.get("project_name")
            if project_filter and mem_project != project_filter:
                continue

            total_memories += 1
            recall_count = int(data.get("recall_count") or 0)
            total_recalls += recall_count
            content = data.get("content", "")
            snippet = content[:80] + ("..." if len(content) > 80 else "")
            last_recalled_raw = data.get("last_recalled")

            entry = {
                "key": key,
                "namespace": namespace,
                "content": snippet,
                "project": mem_project or "",
                "recall_count": recall_count,
                "last_recalled_raw": float(last_recalled_raw) if last_recalled_raw else 0,
                "last_recalled": _fmt_ts(last_recalled_raw) if last_recalled_raw else "Never",
                "created_at_raw": float(data.get("created_at", "0")),
            }

            if recall_count > 0:
                unique_recalled += 1
                most_recalled.append(entry)

                if last_recalled_raw and float(last_recalled_raw) < cold_threshold:
                    gone_cold.append(entry)
            else:
                never_recalled += 1
                never_recalled_list.append(entry)

    most_recalled.sort(key=lambda e: e["recall_count"], reverse=True)
    gone_cold.sort(key=lambda e: e["last_recalled_raw"])
    never_recalled_list.sort(key=lambda e: e["created_at_raw"])

    return {
        "total_memories": total_memories,
        "total_recalls": total_recalls,
        "unique_recalled": unique_recalled,
        "never_recalled": never_recalled,
        "most_recalled": most_recalled[:15],
        "gone_cold": gone_cold[:15],
        "never_recalled_list": never_recalled_list[:20],
        "cold_days": TELEMETRY_COLD_DAYS,
        "project_filter": project_filter or "",
    }


def _fmt_ts(raw) -> str:
    try:
        ts = float(raw)
        return time.strftime("%Y-%m-%d %H:%M", time.localtime(ts))
    except (ValueError, TypeError):
        return "—"


async def telemetry_page(request: Request) -> HTMLResponse:
    """GET /telemetry — telemetry dashboard."""
    project_filter = request.query_params.get("project") or None
    data = _build_telemetry_data(project_filter)

    template = request.app.state.templates.get_template("telemetry.html")
    content = template.render(
        request=request,
        current_page="telemetry",
        **data,
    )
    return HTMLResponse(content)


async def telemetry_refresh(request: Request) -> HTMLResponse:
    """GET /telemetry/refresh — htmx partial refresh."""
    project_filter = request.query_params.get("project") or None
    data = _build_telemetry_data(project_filter)

    template = request.app.state.templates.get_template("partials/telemetry_content.html")
    content = template.render(
        request=request,
        **data,
    )
    return HTMLResponse(content)


routes = [
    Route("/telemetry", telemetry_page),
    Route("/telemetry/refresh", telemetry_refresh),
]
