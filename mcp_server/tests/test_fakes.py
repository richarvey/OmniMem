"""Behavioural tests for the shared in-memory fakes.

The fakes in conftest.py (and the extended variants in test_store.py) are
load-bearing test infrastructure — every suite trusts them to mirror real
Valkey semantics. These tests pin down the branches nothing else exercises:
empty-list pops, scan pattern matching, mixed pipeline dispatch, and the
similarity fallback when a query carries no vector.
"""

import numpy as np

from tests.conftest import (
    FakeSearchDoc,
    FakeSearchIndex,
    FakeValkeyClient,
    FakeValkeyStore,
)
from tests.test_store import ExtClient, ExtPipeline


class TestFakeValkeyClient:
    def test_scan_filters_by_match_pattern(self):
        client = FakeValkeyClient()
        client.hset("mem:episodic:aa", mapping={"content": "one"})
        client.hset("mem:project:bb", mapping={"content": "two"})
        cursor, keys = client.scan(match="mem:episodic:*")
        assert cursor == 0
        assert keys == ["mem:episodic:aa"]

    def test_brpop_empty_list_returns_none(self):
        client = FakeValkeyClient()
        assert client.brpop("queue:empty", timeout=1) is None

    def test_brpop_pops_from_tail(self):
        client = FakeValkeyClient()
        client.lpush("queue:jobs", "b", "a")
        key, value = client.brpop("queue:jobs")
        assert key == "queue:jobs"
        assert value == "b"


class TestFakeSearchDoc:
    def test_unknown_attribute_returns_none(self):
        doc = FakeSearchDoc("mem:episodic:aa", {"content": "hello"})
        assert doc.content == "hello"
        assert doc.nonexistent_field is None


class TestFakeSearchIndex:
    def test_info_and_create_index_are_noops(self):
        index = FakeSearchIndex(FakeValkeyClient(), "idx:memory")
        assert index.info() == {}
        assert index.create_index() is None

    def test_search_without_query_vector_scores_zero(self):
        client = FakeValkeyClient()
        store = FakeValkeyStore()
        store._client = client
        client.hset("mem:episodic:aa", mapping={"content": "hello"})
        index = FakeSearchIndex(client, "idx:memory")
        result = index.search("*", query_params=None)
        docs = {d.id: d for d in result.docs}
        assert docs["mem:episodic:aa"].similarity_score == "0.0"


class TestFakeValkeyStore:
    def test_connect_is_noop(self):
        store = FakeValkeyStore()
        assert store.connect() is None

    def test_set_fields_multi_empty_inputs_return_zero(self):
        store = FakeValkeyStore()
        assert store.set_fields_multi([], {"state": "active"}) == 0
        assert store.set_fields_multi(["mem:episodic:aa"], {}) == 0

    def test_dump_all_includes_sets(self):
        store = FakeValkeyStore()
        store._client.hset("mem:episodic:aa", mapping={"content": "hello"})
        store._client.sadd("meta:some:set", "m1", "m2")
        dump = store.dump_all()
        assert dump["meta:some:set"]["_type"] == "set"
        assert sorted(dump["meta:some:set"]["members"]) == ["m1", "m2"]


class TestExtPipeline:
    def test_mixed_pipeline_dispatches_every_command(self):
        client = ExtClient()
        client.hset("mem:episodic:aa", mapping={"content": "hello", "state": "active"})
        client.set("meta:some:string", "value")
        pipe = ExtPipeline(client)
        # An extended command (type) forces the manual dispatch loop, which
        # must then handle every queued base command too.
        pipe.type("mem:episodic:aa")
        pipe.type("meta:some:string")
        pipe.type("meta:missing")
        pipe.smembers("meta:some:set")
        pipe.sadd("meta:some:set", "m1")
        pipe.hget("mem:episodic:aa", "content")
        pipe.hset("mem:episodic:aa", field="state", value="archived", mapping=None)
        pipe.hkeys("mem:episodic:aa")
        pipe.hmget("mem:episodic:aa", ["content", "state"])
        pipe.delete("mem:episodic:aa")
        results = pipe.execute()
        assert results[0] == "hash"
        assert results[1] == "string"
        assert results[2] == "none"
        assert results[4] == 1  # sadd member count
        assert results[5] == "hello"
        assert results[6] is True
        assert "content" in results[7]
        assert results[8] == ["hello", "archived"]
        assert results[9] == 1
        assert client.hget("mem:episodic:aa", "content") is None

    def test_scan_covers_sets_and_strings(self):
        client = ExtClient()
        client.hset("mem:episodic:aa", mapping={"content": "hello"})
        client.sadd("meta:some:set", "m1")
        client.set("meta:some:string", "value")
        cursor, keys = client.scan(match="meta:*")
        assert cursor == 0
        assert sorted(keys) == ["meta:some:set", "meta:some:string"]


class TestFeedEntry:
    def test_attribute_and_key_access(self):
        from tests.test_rss_worker import FeedEntry

        entry = FeedEntry(title="hello")
        assert entry.title == "hello"
        assert entry.get("title") == "hello"

    def test_missing_attribute_raises(self):
        import pytest
        from tests.test_rss_worker import FeedEntry

        with pytest.raises(AttributeError):
            FeedEntry().missing_field
