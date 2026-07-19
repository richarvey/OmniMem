"""Coverage for the three entry-point modules no other test imports.

* ``mcp_server/server.py`` — module-level auth wiring is exercised by
  re-importing the module under different env combinations, and the
  ``if __name__ == "__main__"`` block (init, fail-closed check, Host/Origin
  allowlist derivation) is driven for real via ``runpy`` with every heavy
  dependency (Valkey store, embedder, migrations, enrichment worker,
  ``mcp.run``) monkeypatched out.
* ``mcp_server/instructions.py`` — constant usage guide; asserted for shape
  and for mentioning the tools it teaches.
* ``rss_worker/worker.py`` — scheduler entry point; individual functions are
  tested with fakes, never a real scheduler loop or a real sleep.

No network, no model loading, no lingering env mutation (monkeypatch only).
"""

import asyncio
import importlib
import runpy
import signal
import sys
import time
import types
from pathlib import Path
from unittest.mock import MagicMock

import pytest

_REPO_ROOT = Path(__file__).resolve().parent.parent.parent

# The worker modules import each other as top-level modules (they run with
# /app as the working directory in Docker), so rss_worker goes on sys.path.
sys.path.insert(0, str(_REPO_ROOT / "rss_worker"))

# apscheduler is a runtime dependency of the RSS worker container, not of the
# test venv. The worker only needs the BlockingScheduler name at import time,
# so stub the module hierarchy when the real package is absent.
try:  # pragma: no cover - depends on the environment
    import apscheduler.schedulers.blocking  # noqa: F401
except ImportError:
    _aps = types.ModuleType("apscheduler")
    _aps_schedulers = types.ModuleType("apscheduler.schedulers")
    _aps_blocking = types.ModuleType("apscheduler.schedulers.blocking")

    class _StubBlockingScheduler:
        """Placeholder so ``worker`` can import; tests always replace it."""

    _aps_blocking.BlockingScheduler = _StubBlockingScheduler
    _aps_schedulers.blocking = _aps_blocking
    _aps.schedulers = _aps_schedulers
    sys.modules.setdefault("apscheduler", _aps)
    sys.modules.setdefault("apscheduler.schedulers", _aps_schedulers)
    sys.modules.setdefault("apscheduler.schedulers.blocking", _aps_blocking)


# ---------------------------------------------------------------------------
# instructions.py
# ---------------------------------------------------------------------------


class TestInstructions:
    def test_instructions_is_substantial_text(self):
        import instructions

        assert isinstance(instructions.INSTRUCTIONS, str)
        assert len(instructions.INSTRUCTIONS) > 2000
        assert instructions.INSTRUCTIONS.startswith("## OmniMem")

    def test_instructions_mention_the_core_tools(self):
        from instructions import INSTRUCTIONS

        for tool in (
            "recall",
            "remember",
            "briefing",
            "warn_if_abandoned",
            "record_experience",
            "log_abandoned",
            "compile_skill",
            "find_skills",
            "get_skill",
            "bless",
            "promote_knowledge",
            "set_project_context",
            "update_project_state",
            "compile_project_context",
            "deprioritise",
            "suppress_topic",
            "retag",
            "health",
        ):
            assert f"{tool}(" in INSTRUCTIONS or f"`{tool}`" in INSTRUCTIONS, tool

    def test_instructions_use_british_spelling(self):
        from instructions import INSTRUCTIONS

        assert "summarise" in INSTRUCTIONS
        assert "deprioritise" in INSTRUCTIONS
        assert "prioritized" not in INSTRUCTIONS


# ---------------------------------------------------------------------------
# server.py helpers
# ---------------------------------------------------------------------------

_SERVER_ENV_VARS = (
    "MCP_AUTH_TOKEN",
    "OAUTH_ENABLED",
    "OAUTH_ADMIN_USER",
    "OAUTH_ADMIN_PASSWORD",
    "OAUTH_BASE_URL",
    "MCP_PUBLIC_URL",
    "MCP_ALLOWED_HOSTS",
    "MCP_ALLOWED_ORIGINS",
    "MCP_HOST",
    "MCP_PORT",
    "MCP_TRANSPORT",
)


def _set_server_env(monkeypatch, env):
    """Clear every env var server.py reads, then set only the given ones."""
    for var in _SERVER_ENV_VARS:
        monkeypatch.delenv(var, raising=False)
    for key, value in env.items():
        monkeypatch.setenv(key, value)


@pytest.fixture
def import_server(monkeypatch):
    """Import server.py fresh under a controlled environment.

    The module runs auth wiring at import time, so each scenario needs a
    clean re-import; the fixture drops the cached module afterwards so no
    other test can accidentally pick up a scenario-specific instance.
    """

    def _import(**env):
        _set_server_env(monkeypatch, env)
        sys.modules.pop("server", None)
        return importlib.import_module("server")

    yield _import
    sys.modules.pop("server", None)


@pytest.fixture
def server_runtime(monkeypatch):
    """Patch everything the __main__ block touches so runpy can drive it.

    Covers: no Valkey connection, no model load, no migrations, no enrichment
    thread, no real signal handlers, no real ``mcp.run``. The tools package
    globals and fastmcp allowlist settings are registered with monkeypatch
    first so the mutations the block performs are rolled back afterwards.
    """
    import fastmcp
    import memory.embedder
    import memory.enrichment
    import memory.lifecycle
    import memory.migrations
    import memory.recall
    import memory.store
    import tools as tools_pkg

    # Register current values so runpy's mutations are undone on teardown.
    for attr in ("_store", "_embedder", "_lifecycle", "_pipeline", "_enrichment_worker"):
        monkeypatch.setattr(tools_pkg, attr, getattr(tools_pkg, attr, None), raising=False)
    monkeypatch.setattr(fastmcp.settings, "http_allowed_hosts", None)
    monkeypatch.setattr(fastmcp.settings, "http_allowed_origins", None)

    store = MagicMock(name="store")
    embedder = MagicMock(name="embedder")
    enrichment = MagicMock(name="enrichment_worker")
    monkeypatch.setattr(memory.store, "ValkeyStore", MagicMock(return_value=store))
    monkeypatch.setattr(memory.embedder, "Embedder", MagicMock(return_value=embedder))
    monkeypatch.setattr(memory.lifecycle, "MemoryLifecycle", MagicMock(name="MemoryLifecycle"))
    monkeypatch.setattr(memory.recall, "RecallPipeline", MagicMock(name="RecallPipeline"))
    migrations = MagicMock(name="migrations")
    monkeypatch.setattr(memory.migrations, "migrate_project_names", migrations.project_names)
    monkeypatch.setattr(memory.migrations, "migrate_missing_state", migrations.missing_state)
    monkeypatch.setattr(memory.migrations, "migrate_rss_article_projects", migrations.rss_projects)
    monkeypatch.setattr(memory.enrichment, "EnrichmentWorker", MagicMock(return_value=enrichment))

    signal_calls = []
    monkeypatch.setattr(signal, "signal", lambda signum, handler: signal_calls.append(signum))

    run_calls = []

    def _fake_run(self, transport=None, host=None, port=None, **kwargs):
        run_calls.append({"transport": transport, "host": host, "port": port})

    monkeypatch.setattr(fastmcp.FastMCP, "run", _fake_run)

    yield {
        "store": store,
        "embedder": embedder,
        "enrichment": enrichment,
        "migrations": migrations,
        "signal_calls": signal_calls,
        "run_calls": run_calls,
        "settings": fastmcp.settings,
    }


def _run_server_as_main():
    """Execute server.py with __name__ == "__main__" via runpy."""
    sys.modules.pop("server", None)
    try:
        runpy.run_module("server", run_name="__main__")
    finally:
        sys.modules.pop("server", None)


# ---------------------------------------------------------------------------
# server.py — module-level auth wiring
# ---------------------------------------------------------------------------


class TestServerAuthWiring:
    def test_no_auth_configured(self, import_server):
        mod = import_server()
        assert mod._auth is None
        assert mod._bearer_verifier is None
        assert mod._oauth_provider is None

    def test_bearer_token_verifier(self, import_server):
        mod = import_server(MCP_AUTH_TOKEN="sekrit-token")
        assert mod._auth is mod._bearer_verifier

        token = asyncio.run(mod._bearer_verifier.verify_token("sekrit-token"))
        assert token is not None
        assert token.client_id == "omnimem"

        assert asyncio.run(mod._bearer_verifier.verify_token("wrong")) is None
        assert asyncio.run(mod._bearer_verifier.verify_token("")) is None

    def test_oauth_missing_credentials_disables_oauth(self, import_server):
        mod = import_server(OAUTH_ENABLED="true")
        assert mod._oauth_provider is None
        assert mod._auth is None

    def test_oauth_missing_base_url_disables_oauth(self, import_server):
        mod = import_server(
            OAUTH_ENABLED="true",
            OAUTH_ADMIN_USER="ric",
            OAUTH_ADMIN_PASSWORD="hunter2",
        )
        assert mod._oauth_provider is None
        assert mod._auth is None

    def test_oauth_enabled_with_valkey_storage(self, monkeypatch, import_server):
        import oauth.routes
        import oauth.storage
        from oauth.provider import _StoredToken

        created = []

        class _FakeStorage:
            def __init__(self, stored_token_cls):
                self.stored_token_cls = stored_token_cls
                created.append(self)

        route_calls = []
        monkeypatch.setattr(oauth.storage, "ValkeyOAuthStorage", _FakeStorage)
        monkeypatch.setattr(
            oauth.routes,
            "register_oauth_routes",
            lambda mcp, provider: route_calls.append((mcp, provider)),
        )

        mod = import_server(
            OAUTH_ENABLED="true",
            OAUTH_ADMIN_USER="ric",
            OAUTH_ADMIN_PASSWORD="hunter2",
            OAUTH_BASE_URL="https://mcp.example.com",
        )

        assert mod._oauth_provider is not None
        assert mod._auth is mod._oauth_provider
        assert created and created[0].stored_token_cls is _StoredToken
        assert mod._oauth_provider._storage is created[0]
        assert route_calls == [(mod.mcp, mod._oauth_provider)]

    def test_oauth_storage_failure_falls_back_to_memory(self, monkeypatch, import_server):
        import oauth.routes
        import oauth.storage
        from oauth.storage import InMemoryOAuthStorage

        class _BrokenStorage:
            def __init__(self, stored_token_cls):
                raise RuntimeError("valkey unreachable")

        monkeypatch.setattr(oauth.storage, "ValkeyOAuthStorage", _BrokenStorage)
        monkeypatch.setattr(oauth.routes, "register_oauth_routes", lambda mcp, provider: None)

        mod = import_server(
            OAUTH_ENABLED="true",
            OAUTH_ADMIN_USER="ric",
            OAUTH_ADMIN_PASSWORD="hunter2",
            OAUTH_BASE_URL="https://mcp.example.com",
        )

        assert mod._oauth_provider is not None
        assert isinstance(mod._oauth_provider._storage, InMemoryOAuthStorage)

    def test_bearer_plus_oauth_uses_multiauth(self, monkeypatch, import_server):
        import oauth.routes
        import oauth.storage
        from fastmcp.server.auth import MultiAuth

        class _FakeStorage:
            def __init__(self, stored_token_cls):
                self.stored_token_cls = stored_token_cls

        monkeypatch.setattr(oauth.storage, "ValkeyOAuthStorage", _FakeStorage)
        monkeypatch.setattr(oauth.routes, "register_oauth_routes", lambda mcp, provider: None)

        mod = import_server(
            MCP_AUTH_TOKEN="sekrit-token",
            OAUTH_ENABLED="true",
            OAUTH_ADMIN_USER="ric",
            OAUTH_ADMIN_PASSWORD="hunter2",
            OAUTH_BASE_URL="https://mcp.example.com",
        )

        assert isinstance(mod._auth, MultiAuth)


# ---------------------------------------------------------------------------
# server.py — health tool and shutdown handler
# ---------------------------------------------------------------------------


class _FakeFT:
    """FT.INFO handle: returns a dict or raises the given exception."""

    def __init__(self, info_result):
        self._info = info_result

    def info(self):
        if isinstance(self._info, Exception):
            raise self._info
        return self._info


class _FakeHealthClient:
    def __init__(self, ft_map, ping_exc=None):
        self._ft_map = ft_map
        self._ping_exc = ping_exc

    def ping(self):
        if self._ping_exc is not None:
            raise self._ping_exc
        return True

    def ft(self, idx_name):
        return _FakeFT(self._ft_map.get(idx_name, RuntimeError("no such index")))


class _FakeHealthStore:
    def __init__(self, client, counts):
        self._client = client
        self.client = client
        self._counts = counts

    def count_all_records(self):
        if isinstance(self._counts, Exception):
            raise self._counts
        return self._counts


class _RaisingEmbedder:
    @property
    def is_loaded(self):
        raise RuntimeError("model exploded")


class TestServerHealth:
    @pytest.fixture
    def server_mod(self, import_server):
        return import_server()

    def _wire(self, monkeypatch, store, embedder):
        import tools as tools_pkg

        monkeypatch.setattr(tools_pkg, "_store", store, raising=False)
        monkeypatch.setattr(tools_pkg, "_embedder", embedder, raising=False)

    def test_health_reports_indexes_records_and_drift(self, monkeypatch, server_mod):
        client = _FakeHealthClient(
            {
                "idx:episodic": {"num_docs": 7},
                "idx:project": {"num_docs": 2},
                # knowledge/preference/skill indexes raise -> "unavailable"
            }
        )
        counts = {"episodic": 5, "project": 2, "knowledge": 1, "preference": 1}
        store = _FakeHealthStore(client, counts)
        embedder = types.SimpleNamespace(is_loaded=True)
        self._wire(monkeypatch, store, embedder)

        result = server_mod.health()

        assert result["valkey_connected"] is True
        assert result["model_loaded"] is True
        assert result["uptime_seconds"] >= 0
        assert result["indexes"]["idx:episodic"] == 7
        assert result["indexes"]["idx:knowledge"] == "unavailable"
        assert result["records"]["episodic"] == 5
        assert result["records"]["skill"] == "unavailable"  # not in counts
        assert result["drift"] == {"episodic": 2}  # 7 indexed vs 5 actual
        assert "valkey_error" not in result

    def test_health_survives_count_failure(self, monkeypatch, server_mod):
        client = _FakeHealthClient({"idx:episodic": {"num_docs": 3}})
        store = _FakeHealthStore(client, RuntimeError("scan failed"))
        self._wire(monkeypatch, store, None)

        result = server_mod.health()

        assert result["valkey_connected"] is True
        assert result["indexes"]["idx:episodic"] == 3
        assert all(v == "unavailable" for v in result["records"].values())
        assert result["drift"] == {}
        assert result["model_loaded"] is False

    def test_health_with_no_store(self, monkeypatch, server_mod):
        self._wire(monkeypatch, None, None)

        result = server_mod.health()

        assert result["valkey_connected"] is False
        assert result["indexes"] == {}
        assert "valkey_error" not in result

    def test_health_reports_connection_failure(self, monkeypatch, server_mod):
        client = _FakeHealthClient({}, ping_exc=ConnectionError("gone"))
        store = _FakeHealthStore(client, {})
        self._wire(monkeypatch, store, None)

        result = server_mod.health()

        assert result["valkey_connected"] is False
        assert result["valkey_error"] == "connection_failed"

    def test_health_swallows_embedder_errors(self, monkeypatch, server_mod):
        self._wire(monkeypatch, None, _RaisingEmbedder())

        result = server_mod.health()

        assert result["model_loaded"] is False

    def test_handle_shutdown_exits_cleanly(self, server_mod):
        with pytest.raises(SystemExit) as excinfo:
            server_mod._handle_shutdown(signal.SIGTERM, None)
        assert excinfo.value.code == 0


# ---------------------------------------------------------------------------
# server.py — __main__ block via runpy
# ---------------------------------------------------------------------------


class TestServerMain:
    def test_default_startup_runs_sse_on_loopback(self, monkeypatch, server_runtime):
        _set_server_env(monkeypatch, {})

        _run_server_as_main()

        assert server_runtime["run_calls"] == [
            {"transport": "sse", "host": "127.0.0.1", "port": 8765}
        ]
        assert signal.SIGTERM in server_runtime["signal_calls"]
        assert signal.SIGINT in server_runtime["signal_calls"]
        server_runtime["store"].connect.assert_called_once_with()
        server_runtime["embedder"].load.assert_called_once_with()
        server_runtime["enrichment"].start.assert_called_once_with()
        server_runtime["migrations"].project_names.assert_called_once()
        server_runtime["migrations"].missing_state.assert_called_once()
        server_runtime["migrations"].rss_projects.assert_called_once()
        # No public URLs configured, so the allowlists stay untouched.
        assert server_runtime["settings"].http_allowed_hosts is None
        assert server_runtime["settings"].http_allowed_origins is None

    def test_refuses_unauthenticated_non_loopback_bind(self, monkeypatch, server_runtime):
        _set_server_env(monkeypatch, {"MCP_HOST": "0.0.0.0"})

        with pytest.raises(SystemExit) as excinfo:
            _run_server_as_main()

        assert excinfo.value.code == 1
        assert server_runtime["run_calls"] == []

    def test_allowlists_derived_from_public_urls(self, monkeypatch, server_runtime):
        _set_server_env(
            monkeypatch,
            {
                "MCP_AUTH_TOKEN": "sekrit",  # auth present, non-loopback bind allowed
                "MCP_HOST": "0.0.0.0",
                "MCP_PORT": "9000",
                "MCP_TRANSPORT": "http",
                "OAUTH_BASE_URL": "https://mcp.example.com",
                "MCP_PUBLIC_URL": "https://proxy.example.com:8443",
                "MCP_ALLOWED_HOSTS": " alt.example.com, mcp.example.com ,",
                "MCP_ALLOWED_ORIGINS": " https://alt.example.com ,",
            },
        )
        # Pre-existing values (e.g. from FASTMCP_HTTP_ALLOWED_HOSTS) must merge.
        monkeypatch.setattr(
            server_runtime["settings"], "http_allowed_hosts", ["preset.example.com"]
        )

        _run_server_as_main()

        assert server_runtime["run_calls"] == [
            {"transport": "http", "host": "0.0.0.0", "port": 9000}
        ]
        # Order preserved, duplicates collapsed, preset kept first.
        assert server_runtime["settings"].http_allowed_hosts == [
            "preset.example.com",
            "mcp.example.com",
            "proxy.example.com",
            "alt.example.com",
        ]
        # netloc keeps the explicit port so the origin matches the browser's.
        assert server_runtime["settings"].http_allowed_origins == [
            "https://mcp.example.com",
            "https://proxy.example.com:8443",
            "https://alt.example.com",
        ]

    def test_schemeless_public_url_contributes_nothing(self, monkeypatch, server_runtime):
        # urlsplit sees no scheme/netloc, so neither a host nor an origin is derived.
        _set_server_env(monkeypatch, {"MCP_PUBLIC_URL": "bare.example.com"})

        _run_server_as_main()

        assert server_runtime["settings"].http_allowed_hosts is None
        assert server_runtime["settings"].http_allowed_origins is None
        assert len(server_runtime["run_calls"]) == 1


# ---------------------------------------------------------------------------
# rss_worker/worker.py
# ---------------------------------------------------------------------------


@pytest.fixture(scope="module")
def worker():
    # The Docker test image builds from mcp_server/ only — no rss_worker there.
    return pytest.importorskip("worker", reason="rss_worker not present in this test image")


class _StopLoop(Exception):
    """Raised by the fake sleep to break out of the watcher's while True."""


class TestWorkerValkeyWait:
    def test_wait_succeeds_first_try(self, monkeypatch, worker):
        client = MagicMock()
        factory = MagicMock(return_value=client)
        monkeypatch.setattr(worker.valkey, "Valkey", factory)
        monkeypatch.setenv("VALKEY_HOST", "test-valkey")
        monkeypatch.setenv("VALKEY_PORT", "6380")
        monkeypatch.setenv("VALKEY_PASSWORD", "pw")

        worker._wait_for_valkey()

        factory.assert_called_once_with(host="test-valkey", port=6380, password="pw")
        client.ping.assert_called_once_with()
        client.close.assert_called_once_with()

    def test_wait_retries_then_succeeds(self, monkeypatch, worker):
        client = MagicMock()
        client.ping.side_effect = [worker.valkey.ConnectionError("not yet"), True]
        monkeypatch.setattr(worker.valkey, "Valkey", MagicMock(return_value=client))
        sleeps = []
        monkeypatch.setattr(time, "sleep", sleeps.append)

        worker._wait_for_valkey(max_retries=3)

        assert sleeps == [2]  # exponential backoff: 2 ** 1
        assert client.ping.call_count == 2

    def test_wait_exits_after_exhausting_retries(self, monkeypatch, worker):
        client = MagicMock()
        client.ping.side_effect = worker.valkey.TimeoutError("never")
        monkeypatch.setattr(worker.valkey, "Valkey", MagicMock(return_value=client))
        sleeps = []
        monkeypatch.setattr(time, "sleep", sleeps.append)

        with pytest.raises(SystemExit) as excinfo:
            worker._wait_for_valkey(max_retries=2)

        assert excinfo.value.code == 1
        assert sleeps == [2]  # no sleep after the final failed attempt


class TestWorkerIngestionAndWatcher:
    def test_run_ingestion_uses_configured_feeds_path(self, monkeypatch, worker):
        calls = []
        monkeypatch.setattr(worker, "ingest_all_feeds", lambda path: calls.append(path) or {"ok": 1})

        worker.run_ingestion()

        assert calls == [worker.FEEDS_PATH]

    def test_get_mtime_existing_and_missing(self, tmp_path, worker):
        feeds = tmp_path / "feeds.yml"
        feeds.write_text("feeds: []\n")

        assert worker._get_mtime(str(feeds)) > 0
        assert worker._get_mtime(str(tmp_path / "missing.yml")) == 0.0

    def test_watch_feeds_file_triggers_on_mtime_change(self, monkeypatch, worker):
        # initial read, then one change, a second change, then no change
        mtimes = iter([1.0, 2.0, 3.0, 3.0])
        monkeypatch.setattr(worker, "_get_mtime", lambda path: next(mtimes))

        remaining_sleeps = [None, None, None]

        def fake_sleep(seconds):
            if not remaining_sleeps:
                raise _StopLoop()
            remaining_sleeps.pop()

        monkeypatch.setattr(time, "sleep", fake_sleep)

        scheduler = MagicMock()
        # First change schedules fine; the second exercises the except branch.
        scheduler.add_job.side_effect = [None, RuntimeError("scheduler shut down")]

        with pytest.raises(_StopLoop):
            worker._watch_feeds_file(scheduler)

        assert scheduler.add_job.call_count == 2
        scheduler.add_job.assert_called_with(
            worker.run_ingestion, id="file_change_trigger", replace_existing=True
        )


class TestWorkerMain:
    def test_main_wires_scheduler_and_watcher(self, monkeypatch, worker):
        calls = []
        monkeypatch.setattr(worker, "_wait_for_valkey", lambda: calls.append("wait"))
        monkeypatch.setattr(worker, "run_ingestion", lambda: calls.append("ingest"))
        monkeypatch.setenv("RSS_SCHEDULE_HOURS", "2")

        scheduler = MagicMock()
        scheduler.start.side_effect = KeyboardInterrupt  # covers the shutdown path
        monkeypatch.setattr(worker, "BlockingScheduler", MagicMock(return_value=scheduler))

        threads = []

        class _FakeThread:
            def __init__(self, target=None, args=(), daemon=False):
                threads.append((target, args, daemon))

            def start(self):
                pass

        monkeypatch.setattr(worker.threading, "Thread", _FakeThread)

        worker.main()

        assert calls == ["wait", "ingest"]
        scheduler.add_job.assert_called_once_with(
            worker.run_ingestion,
            "interval",
            hours=2,
            misfire_grace_time=7200,
            coalesce=True,
        )
        assert threads == [(worker._watch_feeds_file, (scheduler,), True)]
        scheduler.start.assert_called_once_with()

    def test_worker_module_as_main(self, monkeypatch, worker):
        """Drive the __main__ guard via runpy with globals patched at source."""
        import apscheduler.schedulers.blocking as blocking_mod
        import ingester as ingester_mod
        import threading

        import valkey as valkey_mod

        monkeypatch.setattr(valkey_mod, "Valkey", MagicMock(return_value=MagicMock()))
        monkeypatch.setattr(
            ingester_mod, "ingest_all_feeds", MagicMock(return_value={"feeds": 0})
        )
        scheduler = MagicMock()
        scheduler.start.side_effect = KeyboardInterrupt
        monkeypatch.setattr(
            blocking_mod, "BlockingScheduler", MagicMock(return_value=scheduler)
        )

        class _FakeThread:
            def __init__(self, target=None, args=(), daemon=False):
                pass

            def start(self):
                pass

        monkeypatch.setattr(threading, "Thread", _FakeThread)

        saved = sys.modules.pop("worker", None)
        try:
            runpy.run_module("worker", run_name="__main__")
        finally:
            if saved is not None:
                sys.modules["worker"] = saved

        scheduler.start.assert_called_once_with()
