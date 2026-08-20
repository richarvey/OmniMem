"""Tests driving the real ValkeyStore over an extended in-memory fake client.

The rest of the suite substitutes FakeValkeyStore for the whole store class,
which leaves ValkeyStore itself — pipeline batching, index migration, the
search fallback ladder, dump/restore — untested. Here the real class runs
against a client-level fake extended (locally, so conftest stays lean) with
the commands only ValkeyStore uses: TYPE, SMEMBERS-in-pipeline, HGET,
FT.DROPINDEX, and configurable FT.INFO/FT.SEARCH failures.
"""

import numpy as np
import pytest
import valkey

from tests.conftest import FakePipeline, FakeSearchIndex, FakeValkeyClient

from memory.store import INDEX_DEFINITIONS, VECTOR_DIM, ValkeyStore


class ExtPipeline(FakePipeline):
    """FakePipeline plus the commands only the real store issues."""

    def type(self, key):
        self._commands.append(("type", key))
        return self

    def smembers(self, key):
        self._commands.append(("smembers", key))
        return self

    def hget(self, key, field):
        self._commands.append(("hget", key, field))
        return self

    def sadd(self, key, *members):
        self._commands.append(("sadd", key, members))
        return self

    def execute(self):
        extended = [c for c in self._commands if c[0] in
                    ("type", "smembers", "hget", "sadd")]
        if not extended:
            return super().execute()
        results = []
        for cmd in self._commands:
            if cmd[0] == "type":
                results.append(self._client.type(cmd[1]))
            elif cmd[0] == "smembers":
                results.append(self._client.smembers(cmd[1]))
            elif cmd[0] == "hget":
                results.append(self._client.hget(cmd[1], cmd[2]))
            elif cmd[0] == "sadd":
                self._client.sadd(cmd[1], *cmd[2])
                results.append(len(cmd[2]))
            elif cmd[0] == "hset":
                _, key, field, value, mapping = cmd
                self._client.hset(key, field=field, value=value, mapping=mapping)
                results.append(True)
            elif cmd[0] == "hkeys":
                results.append(self._client.hkeys(cmd[1]))
            elif cmd[0] == "hmget":
                results.append(self._client.hmget(cmd[1], cmd[2]))
            elif cmd[0] == "delete":
                self._client.delete(cmd[1])
                results.append(1)
        self._commands = []
        return results


class ExtSearchIndex(FakeSearchIndex):
    """FakeSearchIndex with dropindex and configurable info/search failures."""

    def __init__(self, client, index_name):
        super().__init__(client, index_name)
        cfg = client._index_config.setdefault(index_name, {})
        self._cfg = cfg

    def info(self):
        if self._cfg.get("missing"):
            raise valkey.ResponseError("Unknown index name")
        return self._cfg.get("info", {"attributes": [], "num_docs": 0})

    def dropindex(self, delete_documents: bool = False):
        # valkey-py passes its delete-documents flag positionally even when
        # False, so the wire command is `FT.DROPINDEX <index> ""` — three
        # arguments, which valkey-search rejects. The fake used to accept this
        # happily, which is exactly why _migrate_indexes could be silently
        # broken against a real server while every test passed. Reproduce the
        # real failure so nothing reaches for this helper again.
        raise valkey.ResponseError(
            "wrong number of arguments for FT.DROPINDEX command"
        )

    def create_index(self, *args, **kwargs):
        self._client.created.append(self._index_name)
        self._cfg.pop("missing", None)

    def search(self, query, query_params=None):
        raise_on = self._cfg.get("raise_on")
        if raise_on is not None:
            qs = query.query_string() if hasattr(query, "query_string") else str(query)
            if raise_on in qs:
                raise valkey.ResponseError("Syntax error in query")
        return super().search(query, query_params=query_params)


class ExtClient(FakeValkeyClient):
    def __init__(self):
        super().__init__()
        self._index_config: dict[str, dict] = {}
        self.dropped: list[str] = []
        self.created: list[str] = []
        self.raw_commands: list[tuple] = []
        self.dropindex_error: str | None = None

    def pipeline(self, transaction=False):
        return ExtPipeline(self)

    def ft(self, index_name):
        return ExtSearchIndex(self, index_name)

    def hget(self, key, field):
        return self._data.get(key, {}).get(field)

    def hmget(self, key, fields):
        # Real valkey accepts a bare string as a single field name; the base
        # fake would iterate its characters.
        if isinstance(fields, str):
            fields = [fields]
        return super().hmget(key, fields)

    def type(self, key):
        if key in self._data:
            return "hash"
        if key in self._sets:
            return "set"
        if key in self._strings:
            return "string"
        return "none"

    def scan(self, cursor=0, match="*", count=500):
        import fnmatch
        pool = list(self._data) + list(self._sets) + list(self._strings)
        return (0, [k for k in pool if fnmatch.fnmatch(k, match)])

    def execute_command(self, *args):
        self.raw_commands.append(args)
        if args[0] == "FT.DROPINDEX":
            if self.dropindex_error:
                raise valkey.ResponseError(self.dropindex_error)
            # A dropped index is then missing, so the caller's next info()
            # fails and the create path runs — as it does on a real server.
            self.dropped.append(args[1])
            self._index_config.setdefault(args[1], {})["missing"] = True


def _vec(seed: float = 1.0) -> np.ndarray:
    v = np.full(VECTOR_DIM, seed, dtype=np.float32)
    return v / np.linalg.norm(v)


@pytest.fixture
def store():
    s = ValkeyStore()
    client = ExtClient()
    s._client = client
    s._raw_client = client
    return s


def _seed(store, key="mem:episodic:01A", content="hello", **fields):
    store.upsert(
        key.split(":")[1], key,
        {"content": content, "state": "active", "updated_at": "100", **fields},
        _vec(),
    )
    return key


class TestConnectionLifecycle:
    def test_client_property_requires_connect(self):
        with pytest.raises(RuntimeError, match="not connected"):
            ValkeyStore().client

    def test_connect_success_creates_missing_indexes(self, monkeypatch):
        client = ExtClient()
        for idx in INDEX_DEFINITIONS:
            client._index_config[idx] = {"missing": True}
        monkeypatch.setattr(valkey, "ConnectionPool", lambda **kw: object())
        monkeypatch.setattr(valkey, "Valkey", lambda connection_pool: client)

        s = ValkeyStore()
        s.connect(max_retries=1, retry_delay=0)
        assert s.client is client
        assert sorted(client.created) == sorted(INDEX_DEFINITIONS)

    def test_connect_retries_then_succeeds(self, monkeypatch):
        client = ExtClient()
        attempts = []

        def flaky(connection_pool):
            attempts.append(1)
            if len(attempts) == 1:
                raise valkey.ConnectionError("refused")
            return client

        monkeypatch.setattr(valkey, "ConnectionPool", lambda **kw: object())
        monkeypatch.setattr(valkey, "Valkey", flaky)

        s = ValkeyStore()
        s.connect(max_retries=3, retry_delay=0)
        assert len(attempts) == 2

    def test_connect_exhausts_retries(self, monkeypatch):
        def always_down(connection_pool):
            raise valkey.ConnectionError("refused")

        monkeypatch.setattr(valkey, "ConnectionPool", lambda **kw: object())
        monkeypatch.setattr(valkey, "Valkey", always_down)

        with pytest.raises(valkey.ConnectionError):
            ValkeyStore().connect(max_retries=2, retry_delay=0)

    def test_raw_client_lazily_created(self, monkeypatch):
        client = ExtClient()
        monkeypatch.setattr(valkey, "ConnectionPool", lambda **kw: object())
        monkeypatch.setattr(valkey, "Valkey", lambda connection_pool: client)
        s = ValkeyStore()
        assert s.raw_client is client
        assert s.raw_client is client  # cached


class TestIndexMigration:
    def test_stale_index_dropped_and_recreated(self, store):
        # One index short a field, one up to date, one missing entirely.
        stale = "idx:episodic"
        fresh = "idx:project"
        missing = "idx:knowledge"
        expected = len(INDEX_DEFINITIONS[stale]["fields"])
        store.client._index_config[stale] = {
            "info": {"attributes": list(range(expected - 1))},
        }
        store.client._index_config[fresh] = {
            "info": {"attributes": list(range(len(INDEX_DEFINITIONS[fresh]["fields"])))},
        }
        store.client._index_config[missing] = {"missing": True}

        store._ensure_indexes()
        # Two arguments exactly. Passing valkey-py's empty delete-documents
        # flag as a third makes valkey-search reject the command outright.
        assert ("FT.DROPINDEX", stale) in store.client.raw_commands
        assert stale in store.client.dropped
        assert fresh not in store.client.dropped
        # The stale index is recreated after the drop, not just removed —
        # that recreation is what puts the new fields into the index.
        assert stale in store.client.created
        assert missing in store.client.created
        assert fresh not in store.client.created

    def test_drop_failure_is_logged_not_swallowed(self, store, caplog):
        # The original bug: the drop's ResponseError was caught by the same
        # `except` that handles a missing index, so a failed migration looked
        # identical to a healthy startup.
        stale = "idx:episodic"
        store.client._index_config[stale] = {
            "info": {"attributes": list(range(
                len(INDEX_DEFINITIONS[stale]["fields"]) - 1
            ))},
        }
        store.client.dropindex_error = "wrong number of arguments"

        with caplog.at_level("ERROR"):
            store._ensure_indexes()

        assert "Could not drop index idx:episodic" in caplog.text
        assert stale not in store.client.created

    def test_missing_index_is_not_an_error(self, store, caplog):
        store.client._index_config["idx:episodic"] = {"missing": True}
        with caplog.at_level("ERROR"):
            store._ensure_indexes()
        assert "Could not drop index" not in caplog.text
        assert "idx:episodic" in store.client.created

    def test_extra_fields_on_an_index_are_left_alone(self, store):
        # Only a shortfall triggers a drop; a longer index (an older build
        # that had a field since removed) is not worth destroying.
        idx = "idx:episodic"
        store.client._index_config[idx] = {
            "info": {"attributes": list(range(
                len(INDEX_DEFINITIONS[idx]["fields"]) + 2
            ))},
        }
        store._ensure_indexes()
        assert idx not in store.client.dropped


class TestCrud:
    def test_upsert_and_get_roundtrip(self, store):
        _seed(store, content="the content")
        data = store.get("mem:episodic:01A")
        assert data["content"] == "the content"
        assert "vector" not in data

    def test_upsert_drops_none_fields(self, store):
        store.upsert("episodic", "mem:episodic:01A",
                     {"content": "x", "outcome": None}, _vec())
        assert "outcome" not in store.get("mem:episodic:01A")

    def test_upsert_rejects_bad_prefix(self, store):
        with pytest.raises(ValueError, match="Invalid key prefix"):
            store.upsert("episodic", "wat:123", {"content": "x"}, _vec())

    def test_get_missing_returns_none(self, store):
        assert store.get("mem:episodic:nope") is None

    def test_get_vector_only_hash_returns_none(self, store):
        store.client.hset("mem:episodic:01V", mapping={"vector": _vec().tobytes()})
        assert store.get("mem:episodic:01V") is None

    def test_set_field_and_set_fields(self, store):
        _seed(store)
        store.set_field("mem:episodic:01A", "state", "archived")
        store.set_fields("mem:episodic:01A", {"outcome": "succeeded"})
        store.set_fields("mem:episodic:01A", {})  # no-op branch
        data = store.get("mem:episodic:01A")
        assert data["state"] == "archived"
        assert data["outcome"] == "succeeded"
        with pytest.raises(ValueError):
            store.set_field("bad:key", "state", "active")
        with pytest.raises(ValueError):
            store.set_fields("bad:key", {"state": "active"})

    def test_delete_and_validation(self, store):
        _seed(store)
        store.delete("mem:episodic:01A")
        assert store.get("mem:episodic:01A") is None
        with pytest.raises(ValueError):
            store.delete("bad:key")


class TestBatchOps:
    def test_set_fields_multi_batches(self, store):
        keys = [_seed(store, f"mem:episodic:01{i}") for i in range(5)]
        written = store.set_fields_multi(keys, {"state": "archived"}, batch_size=2)
        assert written == 5
        assert all(store.get(k)["state"] == "archived" for k in keys)

    def test_set_fields_multi_empty_inputs(self, store):
        assert store.set_fields_multi([], {"a": "b"}) == 0
        assert store.set_fields_multi(["mem:episodic:01A"], {}) == 0

    def test_set_fields_multi_validates_keys(self, store):
        with pytest.raises(ValueError):
            store.set_fields_multi(["bad:key"], {"state": "active"})

    def test_get_multi_alignment_with_missing(self, store):
        _seed(store, "mem:episodic:01A", content="a")
        _seed(store, "mem:episodic:01C", content="c")
        rows = store.get_multi([
            "mem:episodic:01A", "mem:episodic:01B", "mem:episodic:01C",
        ])
        assert rows[0]["content"] == "a"
        assert rows[1] is None
        assert rows[2]["content"] == "c"
        assert store.get_multi([]) == []

    def test_get_fields_multi_projection(self, store):
        _seed(store, "mem:episodic:01A", content="a", outcome="succeeded")
        rows = store.get_fields_multi(
            ["mem:episodic:01A", "mem:episodic:01B"], ("content", "outcome"),
        )
        assert rows[0] == {"content": "a", "outcome": "succeeded"}
        assert rows[1] is None
        assert store.get_fields_multi([], ("content",)) == []

    def test_delete_many_batches_and_validates(self, store):
        keys = [_seed(store, f"mem:episodic:01{i}") for i in range(5)]
        assert store.delete_many(keys, batch_size=2) == 5
        assert all(store.get(k) is None for k in keys)
        with pytest.raises(ValueError):
            store.delete_many(["bad:key"])


class TestCounts:
    def test_count_records_and_all(self, store):
        _seed(store, "mem:episodic:01A")
        _seed(store, "mem:episodic:01B")
        _seed(store, "mem:knowledge:01C")
        assert store.count_records("episodic") == 2
        counts = store.count_all_records()
        assert counts["episodic"] == 2
        assert counts["knowledge"] == 1
        assert counts["project"] == 0

    def test_count_records_invalid_namespace(self, store):
        with pytest.raises(ValueError, match="Invalid namespace"):
            store.count_records("wat")


class TestVectors:
    def test_get_vectors_multi(self, store):
        _seed(store, "mem:episodic:01A")
        store.client.hset("mem:episodic:01B", mapping={"vector": b"short"})
        vectors = store.get_vectors_multi([
            "mem:episodic:01A", "mem:episodic:01B", "mem:episodic:01C",
        ])
        assert vectors[0].shape == (VECTOR_DIM,)
        assert vectors[1] is None  # malformed length
        assert vectors[2] is None  # missing key
        assert store.get_vectors_multi([]) == []


class TestSearch:
    def test_basic_search_returns_docs(self, store):
        _seed(store, "mem:episodic:01A", content="alpha")
        results = store.search("episodic", _vec())
        assert results[0]["key"] == "mem:episodic:01A"
        assert "similarity_score" in results[0]

    def test_invalid_namespace_raises(self, store):
        with pytest.raises(ValueError, match="Invalid namespace"):
            store.search("wat", _vec())

    def test_suspicious_filter_is_dropped(self, store):
        _seed(store, "mem:episodic:01A")
        results = store.search("episodic", _vec(), filter_expr="@state:{a};\n")
        assert len(results) == 1  # ran unfiltered instead of erroring

    def test_bad_filter_degrades_to_unfiltered(self, store):
        _seed(store, "mem:episodic:01A")
        store.client._index_config["idx:episodic"] = {"raise_on": "@state:{bad}"}
        results = store.search(
            "episodic", _vec(), filter_expr="@state:{bad}",
        )
        assert len(results) == 1  # retried without the filter

    def test_total_search_failure_returns_empty(self, store):
        _seed(store, "mem:episodic:01A")
        store.client._index_config["idx:episodic"] = {"raise_on": "KNN"}
        assert store.search("episodic", _vec()) == []
        assert store.search("episodic", _vec(), filter_expr="@state:{active}") == []


class TestReindex:
    def test_reindex_recreates_and_reports(self, store):
        _seed(store, "mem:episodic:01A")
        store.client._index_config["idx:episodic"] = {
            "info": {"num_docs": 5, "attributes": []},
        }
        result = store.reindex_namespace("episodic")
        assert ("FT.DROPINDEX", "idx:episodic") in store.client.raw_commands
        assert "idx:episodic" in store.client.created
        assert result["before_num_docs"] == 5
        assert result["actual_records"] == 1
        assert result["removed_phantoms"] == 4

    def test_reindex_tolerates_already_missing_index(self, store):
        store.client.dropindex_error = "Unknown index name"
        result = store.reindex_namespace("episodic")
        assert result["actual_records"] == 0

    def test_reindex_raises_on_real_drop_failure(self, store):
        store.client.dropindex_error = "connection lost"
        with pytest.raises(valkey.ResponseError):
            store.reindex_namespace("episodic")

    def test_reindex_invalid_namespace(self, store):
        with pytest.raises(ValueError):
            store.reindex_namespace("wat")


class TestDumpRestore:
    def test_dump_all_hashes_and_sets(self, store):
        _seed(store, "mem:episodic:01A", content="a")
        store.client.sadd("topics:suppressed", "kubernetes", "systemd")
        dump = store.dump_all()
        assert dump["mem:episodic:01A"]["content"] == "a"
        assert "vector" not in dump["mem:episodic:01A"]
        assert dump["topics:suppressed"]["_type"] == "set"
        assert sorted(dump["topics:suppressed"]["members"]) == [
            "kubernetes", "systemd",
        ]

    def test_restore_all_merge_semantics(self, store):
        _seed(store, "mem:episodic:01A", content="local newer")
        store.set_field("mem:episodic:01A", "updated_at", "200")

        restored, skipped, keys = store.restore_all({
            # Older than local — must be skipped.
            "mem:episodic:01A": {"content": "backup older", "updated_at": "100"},
            # New key — written.
            "mem:episodic:01B": {"content": "backup new", "updated_at": "100"},
            # Invalid prefix — skipped with a warning.
            "evil:key": {"content": "nope"},
            # A set — merged.
            "topics:suppressed": {"_type": "set", "members": ["docker"]},
        })
        assert restored == 2
        assert skipped == 2
        assert "mem:episodic:01B" in keys
        assert "topics:suppressed" in keys
        assert store.get("mem:episodic:01A")["content"] == "local newer"
        assert store.get("mem:episodic:01B")["content"] == "backup new"
        assert "docker" in store.client.smembers("topics:suppressed")

    def test_restore_all_newer_backup_wins(self, store):
        _seed(store, "mem:episodic:01A", content="local old")
        restored, skipped, keys = store.restore_all({
            "mem:episodic:01A": {"content": "backup newer", "updated_at": "999"},
        })
        assert restored == 1 and skipped == 0
        assert store.get("mem:episodic:01A")["content"] == "backup newer"

    def test_scan_prefix(self, store):
        _seed(store, "mem:episodic:01A")
        _seed(store, "mem:knowledge:01B")
        assert store.scan_prefix("mem:episodic:") == ["mem:episodic:01A"]
