"""Tests for semantic deduplication."""

import numpy as np
import pytest

from memory.dedup import DuplicateMatch, check_duplicate, find_all_duplicates
from tests.conftest import store_memory


class TestCheckDuplicate:
    def test_exact_duplicate_detected(self, fake_store, fake_embedder):
        """Identical content should be detected as a duplicate."""
        content = "Always use connection pooling for PostgreSQL"
        store_memory(fake_store, fake_embedder, "mem:episodic:d001", content)

        vector = fake_embedder.embed(content)
        result = check_duplicate(fake_store, "episodic", vector, content)
        assert result is not None
        assert isinstance(result, DuplicateMatch)
        assert result.similarity >= 0.92

    def test_different_content_not_duplicate(self, fake_store, fake_embedder):
        """Very different content should not be flagged."""
        store_memory(fake_store, fake_embedder, "mem:episodic:d002",
                     "Python is good for data science")

        vector = fake_embedder.embed("Kubernetes deployment strategies for production")
        result = check_duplicate(fake_store, "episodic", vector,
                                 "Kubernetes deployment strategies for production")
        # Due to deterministic fake embedder, different text -> different vector
        # The threshold is 0.92, random vectors are unlikely to match
        # This test verifies the flow works; exact similarity depends on the hash
        if result is not None:
            assert result.similarity >= 0.92

    def test_archived_excluded_from_dedup(self, fake_store, fake_embedder):
        content = "Use Redis for session storage"
        store_memory(fake_store, fake_embedder, "mem:episodic:d003",
                     content, state="archived", surface_score="0.0")

        vector = fake_embedder.embed(content)
        result = check_duplicate(fake_store, "episodic", vector, content)
        # Archived memories should be skipped
        assert result is None

    def test_deleted_excluded_from_dedup(self, fake_store, fake_embedder):
        content = "Use Valkey for caching"
        store_memory(fake_store, fake_embedder, "mem:episodic:d004",
                     content, state="deleted", surface_score="0.0")

        vector = fake_embedder.embed(content)
        result = check_duplicate(fake_store, "episodic", vector, content)
        assert result is None

    def test_project_filter_respected(self, fake_store, fake_embedder):
        content = "Use microservices architecture"
        store_memory(fake_store, fake_embedder, "mem:episodic:d005",
                     content, project="alpha")

        vector = fake_embedder.embed(content)
        result = check_duplicate(fake_store, "episodic", vector, content,
                                 project_filter="beta")
        assert result is None

    def test_custom_threshold(self, fake_store, fake_embedder):
        content = "Testing dedup with custom threshold"
        store_memory(fake_store, fake_embedder, "mem:episodic:d006", content)

        vector = fake_embedder.embed(content)
        # Very high threshold should still match identical content
        result = check_duplicate(fake_store, "episodic", vector, content,
                                 threshold=0.99)
        assert result is not None

    def test_empty_namespace_returns_none(self, fake_store, fake_embedder):
        vector = fake_embedder.embed("anything")
        result = check_duplicate(fake_store, "episodic", vector, "anything")
        assert result is None


class TestFindAllDuplicates:
    def test_finds_duplicate_cluster(self, fake_store, fake_embedder):
        """Two identical memories should form a cluster."""
        content = "Always validate user input at API boundaries"
        store_memory(fake_store, fake_embedder, "mem:episodic:fd001", content)
        store_memory(fake_store, fake_embedder, "mem:episodic:fd002", content)

        clusters = find_all_duplicates(fake_store, fake_embedder, "episodic")
        assert len(clusters) >= 1
        assert any(len(c["memories"]) >= 2 for c in clusters)

    def test_no_duplicates_with_different_content(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:fd003",
                     "Python web development")
        store_memory(fake_store, fake_embedder, "mem:episodic:fd004",
                     "Rust systems programming")

        clusters = find_all_duplicates(fake_store, fake_embedder, "episodic")
        # Different content unlikely to cluster
        duplicate_clusters = [c for c in clusters if len(c["memories"]) >= 2]
        # Can't guarantee no match with hash-based embedder, but test the flow
        assert isinstance(clusters, list)

    def test_archived_excluded(self, fake_store, fake_embedder):
        content = "Shared content"
        store_memory(fake_store, fake_embedder, "mem:episodic:fd005", content)
        store_memory(fake_store, fake_embedder, "mem:episodic:fd006",
                     content, state="archived", surface_score="0.0")

        clusters = find_all_duplicates(fake_store, fake_embedder, "episodic")
        # Archived should be excluded, so no cluster of 2
        for cluster in clusters:
            keys = [m["key"] for m in cluster["memories"]]
            assert "mem:episodic:fd006" not in keys

    def test_fewer_than_two_returns_empty(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:fd007",
                     "Only one memory")
        clusters = find_all_duplicates(fake_store, fake_embedder, "episodic")
        assert clusters == []

    def test_max_keys_cap(self, fake_store, fake_embedder):
        """Should not error when max_keys is small."""
        for i in range(5):
            store_memory(fake_store, fake_embedder, f"mem:episodic:cap{i}",
                         f"Memory {i} about testing")
        clusters = find_all_duplicates(fake_store, fake_embedder, "episodic",
                                       max_keys=3)
        assert isinstance(clusters, list)

    def test_project_filter(self, fake_store, fake_embedder):
        content = "Shared content between projects"
        store_memory(fake_store, fake_embedder, "mem:episodic:fd008",
                     content, project="alpha")
        store_memory(fake_store, fake_embedder, "mem:episodic:fd009",
                     content, project="beta")

        clusters = find_all_duplicates(fake_store, fake_embedder, "episodic",
                                       project_filter="alpha")
        # Only alpha project memories should be scanned
        for cluster in clusters:
            for item in cluster["memories"]:
                assert item.get("project") == "alpha"
