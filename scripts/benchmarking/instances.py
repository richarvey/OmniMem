#!/usr/bin/env python3
"""Start, reset and stop throwaway OmniMem instances for benchmarking.

The benchmark drives 6.x and 7.x through the same MCP client, so the only real
difference between them is how you get a server running and how you wipe it
between questions. That is all this module is.

Both versions are started on loopback with **no authentication**, which both
of them permit only on loopback and refuse anywhere else. Nothing here is
suitable for a network-reachable deployment, and it is not meant to be.

Ports: the defaults deliberately avoid 8765 and 8080, which a production
OmniMem stack occupies on the maintainer's host. Benchmarking must never point
at a live instance, so every class here creates its own empty store and the
callers verify emptiness before writing (see ``assert_empty``).

Why a full restart to reset, rather than deleting the project: LongMemEval
gives every question its own haystack, and those must not bleed into each
other. A restart onto an empty store is slower but is obviously correct, and
6.x only costs about two seconds to come back including the model load.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path

import httpx

from mcpclient import McpClient, McpError

V7_PORT = 8766
V6_PORT = 8767
VALKEY_PORT = 6399
VALKEY_CONTAINER = "omnimem-bench-valkey"
VALKEY_IMAGE = "valkey/valkey-extension:latest"
VALKEY_PASSWORD = "benchpw"
REPO_ROOT = Path(__file__).resolve().parents[2]


class InstanceError(RuntimeError):
    """The instance would not start, or did not look safe to use."""


@dataclass
class Instance:
    """A running server, described well enough to talk to and to label."""

    version: str
    base_url: str
    server_info: dict
    omnimem_version: str = "unknown"

    def client(self) -> McpClient:
        client = McpClient(self.base_url)
        client.connect()
        return client


def _reported_version(client: McpClient) -> str:
    """The OmniMem version, taken from the ``version`` tool.

    Explicitly not from ``serverInfo``: 6.x reports the FastMCP framework
    version there (4.0.4), not its own (6.7.1). A report labelled from
    serverInfo would credit the wrong software, which matters when the whole
    point of the document is to be checkable.
    """
    try:
        return str(client.call_ok("version").json().get("version", "unknown"))
    except (McpError, ValueError):
        return "unknown"


def assert_empty(client: McpClient) -> int:
    """Refuse to continue unless the store holds nothing.

    This is the guard that stops a misconfigured run writing benchmark noise
    into somebody's real memory. 6.x reports ``records`` as a map keyed by
    namespace and 7.x reports its own shape, so both are summed defensively
    and anything unparseable counts as "not safe".
    """
    payload = client.call_ok("health").json()
    records = payload.get("records", payload.get("memories", {}))
    if isinstance(records, dict):
        total = sum(int(v) for v in records.values() if isinstance(v, (int, float)))
    elif isinstance(records, (int, float)):
        total = int(records)
    else:
        raise InstanceError(f"could not read record count from health: {payload!r}")
    if total != 0:
        raise InstanceError(
            f"refusing to benchmark: store already holds {total} records. "
            "This may be a real instance rather than a throwaway one."
        )
    return total


class V7Instance:
    """OmniMem 7.x: one Rust binary over SQLite, no external services."""

    version = "v7"

    def __init__(self, binary: str | None = None, data_dir: str | None = None, port: int = V7_PORT):
        self.binary = binary or os.environ.get("OMNIMEM_V7_BIN", "")
        if not self.binary or not Path(self.binary).exists():
            raise InstanceError(
                "set OMNIMEM_V7_BIN to a built omnimem binary "
                "(cargo build --release -p omnimem)"
            )
        self.data_dir = Path(data_dir or os.environ.get("OMNIMEM_BENCH_TMP", "/tmp/omnimem-bench-v7"))
        self.port = port
        self.base_url = f"http://127.0.0.1:{port}"
        self._proc: subprocess.Popen | None = None
        self._log = None

    def _env(self) -> dict:
        env = dict(os.environ)
        env.update(
            OMNIMEM_DB=str(self.data_dir / "omnimem.db"),
            MCP_HOST="127.0.0.1",
            MCP_PORT=str(self.port),
            RSS_SCHEDULE_HOURS="0",
            FEEDS_CONFIG_PATH=str(self.data_dir / "no-feeds.yml"),
        )
        # Fact extraction and query expansion call out to an LLM when a key is
        # present. That would make timings and results non-deterministic, so it
        # is removed for the duration of the run.
        env.pop("ANTHROPIC_API_KEY", None)
        return env

    def start(self) -> Instance:
        shutil.rmtree(self.data_dir, ignore_errors=True)
        self.data_dir.mkdir(parents=True, exist_ok=True)
        (self.data_dir / "no-feeds.yml").write_text("feeds: []\n")
        self._log = open(self.data_dir / "server.log", "w")
        self._proc = subprocess.Popen(
            [self.binary, "serve"], env=self._env(), stdout=self._log, stderr=subprocess.STDOUT
        )
        self._wait_ready()
        client = McpClient(self.base_url)
        info = client.connect()
        reported = _reported_version(client)
        client.close()
        return Instance(version=self.version, base_url=self.base_url,
                        server_info=info, omnimem_version=reported)

    def _wait_ready(self, timeout: float = 60.0) -> None:
        """7.x has an unauthenticated /healthz, which is the cheapest probe."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self._proc and self._proc.poll() is not None:
                raise InstanceError(f"v7 exited during startup, rc={self._proc.returncode}")
            try:
                if httpx.get(f"{self.base_url}/healthz", timeout=2).status_code == 200:
                    return
            except httpx.HTTPError:
                pass
            time.sleep(0.25)
        raise InstanceError("v7 never became ready")

    def stop(self) -> None:
        if self._proc:
            self._proc.terminate()
            try:
                self._proc.wait(timeout=15)
            except subprocess.TimeoutExpired:
                self._proc.kill()
            self._proc = None
        if self._log:
            self._log.close()
            self._log = None

    def reset(self) -> Instance:
        """Wipe to an empty store by deleting the database and restarting."""
        self.stop()
        return self.start()


class V6Instance:
    """OmniMem 6.x: a Python server plus Valkey with the search module.

    Plain Valkey or Redis will not do: vector search needs libsearch.so, which
    is why the image is valkey-extension rather than valkey.
    """

    version = "v6"

    def __init__(self, python: str | None = None, port: int = V6_PORT, valkey_port: int = VALKEY_PORT):
        self.python = python or os.environ.get("OMNIMEM_V6_PYTHON", sys.executable)
        self.port = port
        self.valkey_port = valkey_port
        self.base_url = f"http://127.0.0.1:{port}"
        self.server_dir = REPO_ROOT / "mcp_server"
        self._proc: subprocess.Popen | None = None
        self._log = None

    def _env(self) -> dict:
        env = dict(os.environ)
        env.update(
            VALKEY_HOST="127.0.0.1",
            VALKEY_PORT=str(self.valkey_port),
            VALKEY_PASSWORD=VALKEY_PASSWORD,
            MCP_TRANSPORT="http",  # 6.x still defaults to deprecated SSE
            MCP_HOST="127.0.0.1",
            MCP_PORT=str(self.port),
        )
        env.pop("ANTHROPIC_API_KEY", None)
        return env

    def _start_valkey(self) -> None:
        subprocess.run(["docker", "rm", "-f", VALKEY_CONTAINER], capture_output=True)
        started = subprocess.run(
            [
                "docker", "run", "-d", "--name", VALKEY_CONTAINER,
                "-p", f"{self.valkey_port}:6379", VALKEY_IMAGE,
                "valkey-server", "--loadmodule", "/usr/lib/valkey/libsearch.so",
                "--requirepass", VALKEY_PASSWORD,
                "--notify-keyspace-events", "AKE",
            ],
            capture_output=True, text=True,
        )
        if started.returncode != 0:
            raise InstanceError(f"could not start valkey: {started.stderr[:300]}")

    def start(self, fresh_valkey: bool = True) -> Instance:
        if fresh_valkey:
            self._start_valkey()
        log_path = Path(os.environ.get("OMNIMEM_BENCH_TMP", "/tmp")) / "omnimem-bench-v6.log"
        log_path.parent.mkdir(parents=True, exist_ok=True)
        self._log = open(log_path, "w")
        self._proc = subprocess.Popen(
            [self.python, "server.py"], cwd=self.server_dir, env=self._env(),
            stdout=self._log, stderr=subprocess.STDOUT,
        )
        info = self._wait_ready()
        client = McpClient(self.base_url)
        client.connect()
        reported = _reported_version(client)
        client.close()
        return Instance(version=self.version, base_url=self.base_url,
                        server_info=info, omnimem_version=reported)

    def _wait_ready(self, timeout: float = 120.0) -> dict:
        """6.x has no HTTP health route, so readiness is a successful handshake.

        The embedding model loads during startup, so this also waits out the
        cold start rather than letting it land in the first measurement.
        """
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self._proc and self._proc.poll() is not None:
                raise InstanceError(f"v6 exited during startup, rc={self._proc.returncode}")
            try:
                client = McpClient(self.base_url, timeout=10)
                info = client.connect()
                client.close()
                return info
            except (McpError, httpx.HTTPError):
                time.sleep(0.5)
        raise InstanceError("v6 never became ready")

    def stop(self, remove_valkey: bool = True) -> None:
        if self._proc:
            self._proc.terminate()
            try:
                self._proc.wait(timeout=20)
            except subprocess.TimeoutExpired:
                self._proc.kill()
            self._proc = None
        if self._log:
            self._log.close()
            self._log = None
        if remove_valkey:
            subprocess.run(["docker", "rm", "-f", VALKEY_CONTAINER], capture_output=True)

    def reset(self) -> Instance:
        """Wipe to an empty store.

        Valkey is flushed and the server restarted, because 6.x builds its
        search indexes at boot and a flush destroys them.
        """
        self.stop(remove_valkey=False)
        subprocess.run(
            ["docker", "exec", VALKEY_CONTAINER, "valkey-cli", "-a", VALKEY_PASSWORD, "FLUSHALL"],
            capture_output=True,
        )
        return self.start(fresh_valkey=False)


def build(version: str) -> V6Instance | V7Instance:
    """Return an unstarted instance manager for "v6" or "v7"."""
    if version == "v6":
        return V6Instance()
    if version == "v7":
        return V7Instance()
    raise InstanceError(f"unknown version {version!r}, expected v6 or v7")


if __name__ == "__main__":
    # Smoke check: start each version named on the command line, prove the
    # store is empty, round-trip one memory, and tear down again.
    for version in sys.argv[1:] or ["v7"]:
        manager = build(version)
        instance = manager.start()
        try:
            with McpClient(instance.base_url) as client:
                assert_empty(client)
                tools = client.list_tools()
                stored = client.call_ok(
                    "remember",
                    {"content": "Benchmark smoke memory.", "project": "omnimem-bench"},
                )
                found = client.call_ok("recall", {"query": "benchmark smoke", "top_k": 3})
                print(
                    f"{version}: omnimem {instance.omnimem_version} "
                    f"tools={len(tools)} remember={stored.latency_ms:.0f}ms "
                    f"recall={found.latency_ms:.0f}ms"
                )
        finally:
            manager.stop()
