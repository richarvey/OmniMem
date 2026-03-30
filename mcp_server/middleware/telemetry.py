"""FastMCP middleware that records per-tool call metrics to Valkey."""

from __future__ import annotations

import logging
import time
from typing import TYPE_CHECKING

from fastmcp.server.middleware import CallNext, Middleware, MiddlewareContext

if TYPE_CHECKING:
    from fastmcp.tools.tool import ToolResult

logger = logging.getLogger("omnimem.middleware.telemetry")


class ToolTelemetryMiddleware(Middleware):
    """Records call count, duration, and response size for every tool call."""

    def __init__(self, store) -> None:
        super().__init__()
        self._store = store

    async def on_call_tool(
        self,
        context: MiddlewareContext[CallToolRequestParams],
        call_next: CallNext[CallToolRequestParams, ToolResult],
    ) -> ToolResult:
        tool_name = context.message.name
        start = time.perf_counter()

        try:
            result = await call_next(context)
        except Exception:
            self._record_error(tool_name)
            raise

        duration_ms = int((time.perf_counter() - start) * 1000)
        response_chars = _measure_response(result)
        self._record_metrics(tool_name, duration_ms, response_chars)
        return result

    def _record_metrics(
        self, tool_name: str, duration_ms: int, response_chars: int
    ) -> None:
        """Write call metrics to Valkey. Fails silently."""
        try:
            key = f"meta:tool_metrics:{tool_name}"
            pipe = self._store.client.pipeline(transaction=False)
            pipe.hincrby(key, "call_count", 1)
            pipe.hincrby(key, "total_duration_ms", duration_ms)
            pipe.hincrby(key, "total_response_chars", response_chars)
            pipe.hset(key, "last_called_at", str(time.time()))
            pipe.execute()
        except Exception:
            logger.debug("Failed to record tool metrics for %s", tool_name, exc_info=True)

    def _record_error(self, tool_name: str) -> None:
        """Increment error counter. Fails silently."""
        try:
            key = f"meta:tool_metrics:{tool_name}"
            pipe = self._store.client.pipeline(transaction=False)
            pipe.hincrby(key, "call_count", 1)
            pipe.hincrby(key, "error_count", 1)
            pipe.hset(key, "last_called_at", str(time.time()))
            pipe.execute()
        except Exception:
            logger.debug("Failed to record error metric for %s", tool_name, exc_info=True)


def _measure_response(result: ToolResult) -> int:
    """Sum character length of all text content blocks in a ToolResult."""
    total = 0
    for block in result.content:
        text = getattr(block, "text", None)
        if isinstance(text, str):
            total += len(text)
    return total
