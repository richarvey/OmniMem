"""Tests for the contradiction detection tool (tools/contradiction.py)."""

import pytest

from tests.conftest import store_memory

import tools as tools_module


@pytest.fixture(autouse=True)
def inject_deps(fake_store, fake_embedder, lifecycle, pipeline):
    tools_module._store = fake_store
    tools_module._embedder = fake_embedder
    tools_module._lifecycle = lifecycle
    tools_module._pipeline = pipeline
    yield
    tools_module._store = None
    tools_module._embedder = None
    tools_module._lifecycle = None
    tools_module._pipeline = None


from tools.contradiction import check_contradictions


class TestCheckContradictions:
    def test_empty_store(self):
        result = check_contradictions()
        assert result["contradictions"] == []

    def test_no_contradictions(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "Use Docker for deployment")
        store_memory(fake_store, fake_embedder, "mem:episodic:01B", "Use Docker Compose for local dev")
        result = check_contradictions()
        assert result["contradictions"] == []

    def test_heuristic_detection(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "Always use Alpine for Docker images")
        store_memory(fake_store, fake_embedder, "mem:episodic:01B", "Never use Alpine for Docker images")
        result = check_contradictions()
        assert len(result["contradictions"]) >= 1
        c = result["contradictions"][0]
        assert c["method"] == "heuristic"
        assert "key_a" in c
        assert "key_b" in c

    def test_scan_mode(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "Enable caching always")
        store_memory(fake_store, fake_embedder, "mem:episodic:01B", "Never enable caching")
        # No query = scan mode
        result = check_contradictions(query=None)
        assert isinstance(result["contradictions"], list)

    def test_project_filter(self, fake_store, fake_embedder):
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01A", "Always use Redis for caching",
            project="projA",
        )
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01B", "Never use Redis for caching",
            project="projA",
        )
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01C", "Always use Memcached",
            project="projB",
        )
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01D", "Never use Memcached",
            project="projB",
        )
        # Identical stored vectors make both pairs deterministic tier-1 hits,
        # so the project filter has something real to exclude.
        client = fake_store._client
        client._data["mem:episodic:01B"]["vector"] = client._data["mem:episodic:01A"]["vector"]
        client._data["mem:episodic:01D"]["vector"] = client._data["mem:episodic:01C"]["vector"]
        result = check_contradictions(project_filter="projA")
        # Should only find contradictions within projA
        assert result["contradictions"]
        for c in result["contradictions"]:
            keys = {c["key_a"], c["key_b"]}
            assert "mem:episodic:01C" not in keys
            assert "mem:episodic:01D" not in keys

    def test_skips_archived(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "Always use Alpine")
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01B", "Never use Alpine",
            state="archived",
        )
        # A second active memory with an identical vector guarantees at least
        # one contradiction is found, proving the loop excludes the archived one.
        store_memory(fake_store, fake_embedder, "mem:episodic:01E", "Never use Alpine")
        client = fake_store._client
        client._data["mem:episodic:01E"]["vector"] = client._data["mem:episodic:01A"]["vector"]
        result = check_contradictions()
        # Archived memory should be excluded from scan
        assert result["contradictions"]
        for c in result["contradictions"]:
            assert "mem:episodic:01B" not in (c["key_a"], c["key_b"])

    def test_links_cross_referenced(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "Always use Alpine for builds")
        store_memory(fake_store, fake_embedder, "mem:episodic:01B", "Never use Alpine for builds")
        client = fake_store._client
        client._data["mem:episodic:01B"]["vector"] = client._data["mem:episodic:01A"]["vector"]
        check_contradictions()
        # Check that contradictions are linked on both memories
        data_a = fake_store.get("mem:episodic:01A")
        assert data_a and data_a.get("contradictions")
        import json
        contras = json.loads(data_a["contradictions"])
        assert any(c.get("key") == "mem:episodic:01B" for c in contras)
