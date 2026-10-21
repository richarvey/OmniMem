#!/usr/bin/env python3
"""A very small MCP client, enough to drive OmniMem 6.x and 7.x identically.

Both versions expose the same 48 tools over streamable HTTP at ``/mcp``, so one
client covers both and the benchmark never has to branch on version. Only the
way the server is started differs, and that lives in ``instances.py``.

Deliberately hand-rolled rather than built on an SDK: 6.x speaks the protocol
through FastMCP and 7.x through rmcp, and the point of the benchmark is to
measure those two servers, not whichever client library happens to sit in
front of them. A hundred lines we control is easier to trust than a dependency
that might quietly retry, batch or cache.

Two wire details, both confirmed against running servers rather than docs:

* Responses come back either as plain JSON or as SSE frames, depending on the
  server and the call. ``_parse`` handles both, and skips empty ``data:`` lines
  because rmcp emits them.
* ``initialize`` returns an ``mcp-session-id`` header which must be echoed on
  every later request.
"""

from __future__ import annotations

import json
import time
from dataclasses import dataclass, field
from typing import Any

import httpx

PROTOCOL_VERSION = "2025-06-18"
CLIENT_NAME = "omnimem-bench"
CLIENT_VERSION = "0.1"

_HEADERS = {
    "content-type": "application/json",
    "accept": "application/json, text/event-stream",
}


class McpError(RuntimeError):
    """A JSON-RPC error, a transport failure, or a tool reporting isError."""


@dataclass
class ToolResult:
    """One ``tools/call`` response, with the latency it took to get it."""

    name: str
    text: str
    raw: dict[str, Any] = field(repr=False, default_factory=dict)
    latency_ms: float = 0.0
    is_error: bool = False

    def json(self) -> Any:
        """The text payload parsed as JSON.

        Every OmniMem tool returns a JSON document inside a text block, but a
        tool that failed may return prose, so this raises McpError rather than
        letting a JSONDecodeError escape with no context.
        """
        try:
            return json.loads(self.text)
        except json.JSONDecodeError as exc:
            raise McpError(f"{self.name} did not return JSON: {self.text[:200]!r}") from exc


def _parse(response: httpx.Response) -> dict[str, Any]:
    """Return the JSON-RPC envelope from a plain or SSE-framed response."""
    if "event-stream" not in response.headers.get("content-type", ""):
        return response.json()
    for line in response.text.splitlines():
        if not line.startswith("data:"):
            continue
        payload = line.split(":", 1)[1].strip()
        if not payload:
            continue
        try:
            return json.loads(payload)
        except json.JSONDecodeError:
            continue
    raise McpError(f"no JSON frame in SSE response: {response.text[:200]!r}")


class McpClient:
    """A connected MCP session against one OmniMem server."""

    def __init__(self, base_url: str, token: str | None = None, timeout: float = 120.0) -> None:
        self.base_url = base_url.rstrip("/")
        self.headers = dict(_HEADERS)
        if token:
            self.headers["authorization"] = f"Bearer {token}"
        self._client = httpx.Client(timeout=timeout)
        self._id = 0
        self.server_info: dict[str, Any] = {}

    # -- lifecycle ---------------------------------------------------------

    def connect(self) -> dict[str, Any]:
        """Run the initialize handshake and return the server's own info."""
        envelope = self._post(
            "initialize",
            {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": CLIENT_NAME, "version": CLIENT_VERSION},
            },
            capture_session=True,
        )
        result = envelope.get("result", {})
        self.server_info = result.get("serverInfo", {})
        self._notify("notifications/initialized")
        return self.server_info

    def close(self) -> None:
        self._client.close()

    def __enter__(self) -> McpClient:
        self.connect()
        return self

    def __exit__(self, *_exc: object) -> None:
        self.close()

    # -- calls -------------------------------------------------------------

    def list_tools(self) -> list[dict[str, Any]]:
        return self._post("tools/list", {}).get("result", {}).get("tools", [])

    def call(self, name: str, arguments: dict[str, Any] | None = None) -> ToolResult:
        """Call one tool, timing the round trip.

        The timing is wall clock around the HTTP request, so it includes
        transport. That is deliberate: it is what a real MCP client would
        experience, and it is measured identically for both versions.
        """
        started = time.monotonic()
        envelope = self._post("tools/call", {"name": name, "arguments": arguments or {}})
        latency_ms = (time.monotonic() - started) * 1000
        result = envelope.get("result", {})
        blocks = result.get("content", []) or []
        text = "".join(b.get("text", "") for b in blocks if isinstance(b, dict))
        return ToolResult(
            name=name,
            text=text,
            raw=result,
            latency_ms=latency_ms,
            is_error=bool(result.get("isError")),
        )

    def call_ok(self, name: str, arguments: dict[str, Any] | None = None) -> ToolResult:
        """``call``, but raise if the tool reported an error."""
        result = self.call(name, arguments)
        if result.is_error:
            raise McpError(f"{name} failed: {result.text[:300]}")
        return result

    # -- plumbing ----------------------------------------------------------

    def _post(
        self,
        method: str,
        params: dict[str, Any],
        capture_session: bool = False,
    ) -> dict[str, Any]:
        self._id += 1
        body = {"jsonrpc": "2.0", "id": self._id, "method": method, "params": params}
        response = self._client.post(f"{self.base_url}/mcp", json=body, headers=self.headers)
        if capture_session:
            session = response.headers.get("mcp-session-id")
            if session:
                self.headers["mcp-session-id"] = session
        if response.status_code >= 400:
            raise McpError(f"{method} -> HTTP {response.status_code}: {response.text[:300]}")
        envelope = _parse(response)
        if "error" in envelope:
            raise McpError(f"{method} -> {envelope['error']}")
        return envelope

    def _notify(self, method: str) -> None:
        """Send a notification, which by definition carries no id and no reply."""
        self._client.post(
            f"{self.base_url}/mcp",
            json={"jsonrpc": "2.0", "method": method},
            headers=self.headers,
        )
