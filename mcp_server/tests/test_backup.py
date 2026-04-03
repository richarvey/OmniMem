"""Tests for backup tools: dump_to_file, restore_from_file, list_backups."""

import json
import time
from unittest.mock import patch

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


from tools.backup import (
    _validate_filename,
    dump_to_file,
    list_backups,
    restore_from_file,
)


@pytest.fixture
def backup_dir(tmp_path):
    """Patch _backup_dir to use a temp directory."""
    with patch("tools.backup._backup_dir", return_value=tmp_path):
        yield tmp_path


class TestValidateFilename:
    def test_valid(self):
        assert _validate_filename("backup_2024.json") is None

    def test_empty(self):
        assert _validate_filename("") is not None

    def test_no_json_extension(self):
        assert _validate_filename("backup") is not None

    def test_too_long(self):
        assert _validate_filename("x" * 252 + ".json") is not None

    def test_path_traversal(self):
        assert _validate_filename("../etc/passwd.json") is not None


class TestDumpToFile:
    def test_creates_file(self, fake_store, fake_embedder, backup_dir):
        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "test memory")
        result = dump_to_file("test-backup.json")
        assert result["filename"] == "test-backup.json"
        assert result["total_keys"] >= 1
        assert (backup_dir / "test-backup.json").exists()

    def test_auto_filename(self, fake_store, fake_embedder, backup_dir):
        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "test")
        result = dump_to_file()
        assert result["filename"].startswith("memory_backup_")
        assert result["filename"].endswith(".json")

    def test_namespace_counts(self, fake_store, fake_embedder, backup_dir):
        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "episodic")
        store_memory(
            fake_store, fake_embedder, "mem:knowledge:01A", "knowledge",
            namespace="knowledge",
        )
        result = dump_to_file("test.json")
        # Read the file to check metadata
        with open(backup_dir / "test.json") as f:
            backup = json.load(f)
        assert backup["metadata"]["namespaces"]["episodic"] >= 1
        assert backup["metadata"]["namespaces"]["knowledge"] >= 1

    def test_invalid_filename(self, backup_dir):
        result = dump_to_file("../bad.json")
        assert result["status"] == "error"


class TestRestoreFromFile:
    def _write_backup(self, backup_dir, filename, data):
        backup = {
            "metadata": {"exported_at": "2024-01-01T00:00:00Z", "total_keys": len(data)},
            "data": data,
        }
        filepath = backup_dir / filename
        with open(filepath, "w") as f:
            json.dump(backup, f)
        return filepath

    def test_dry_run(self, backup_dir):
        self._write_backup(backup_dir, "test.json", {
            "mem:episodic:01A": {"content": "test", "updated_at": "1000"},
        })
        result = restore_from_file("test.json", dry_run=True)
        assert result["status"] == "dry_run"
        assert result["total_keys_in_backup"] == 1

    def test_actual_restore(self, fake_store, backup_dir):
        self._write_backup(backup_dir, "test.json", {
            "mem:episodic:01A": {"content": "restored", "updated_at": "9999999999"},
        })
        result = restore_from_file("test.json", dry_run=False)
        assert result["status"] == "restored"
        assert result["restored_keys"] == 1
        # Verify data is in store
        data = fake_store.get("mem:episodic:01A")
        assert data["content"] == "restored"

    def test_file_not_found(self, backup_dir):
        result = restore_from_file("nonexistent.json")
        assert result["status"] == "error"
        assert "not found" in result["message"]

    def test_invalid_json(self, backup_dir):
        filepath = backup_dir / "bad.json"
        filepath.write_text("not valid json")
        result = restore_from_file("bad.json")
        assert result["status"] == "error"
        assert "Invalid JSON" in result["message"]

    def test_invalid_key_prefix(self, backup_dir):
        self._write_backup(backup_dir, "test.json", {
            "bad:prefix:01A": {"content": "bad"},
        })
        result = restore_from_file("test.json", dry_run=False)
        assert result["status"] == "error"
        assert "invalid key prefix" in result["message"]


class TestRestoreReEmbed:
    """Verify that restored memories are immediately recallable (issue #10)."""

    def _write_backup(self, backup_dir, filename, data):
        backup = {
            "metadata": {"exported_at": "2024-01-01T00:00:00Z", "total_keys": len(data)},
            "data": data,
        }
        filepath = backup_dir / filename
        with open(filepath, "w") as f:
            json.dump(backup, f)
        return filepath

    def test_dump_restore_recall_roundtrip(self, fake_store, fake_embedder, pipeline, backup_dir):
        """A memory that is dumped and restored should be immediately recallable."""
        # 1. Store a memory with vector
        store_memory(fake_store, fake_embedder, "mem:episodic:RT01", "valkey search bug fix")

        # 2. Dump — this excludes vectors (as expected)
        result = dump_to_file("roundtrip.json")
        assert result["total_keys"] >= 1

        # 3. Wipe the store to simulate fresh instance
        fake_store._client._data.clear()
        fake_store._client._sets.clear()

        # 4. Verify recall returns nothing
        results = pipeline.recall("valkey search bug fix", top_k=5)
        assert len(results) == 0

        # 5. Restore from backup
        result = restore_from_file("roundtrip.json", dry_run=False)
        assert result["status"] == "restored"
        assert result["restored_keys"] >= 1
        assert result["re_embedded"] >= 1

        # 6. Recall should now find the memory immediately
        results = pipeline.recall("valkey search bug fix", top_k=5)
        assert len(results) >= 1
        assert any("valkey search bug fix" in r.content for r in results)

    def test_restore_re_embeds_only_mem_keys(self, fake_store, fake_embedder, backup_dir):
        """Only mem:* keys should be re-embedded, not topics: or log: keys."""
        self._write_backup(backup_dir, "mixed.json", {
            "mem:episodic:E01": {"content": "test memory", "updated_at": "9999999999"},
            "topics:suppressed": {"_type": "set", "members": ["boring-topic"]},
        })
        result = restore_from_file("mixed.json", dry_run=False)
        assert result["status"] == "restored"
        assert result["re_embedded"] == 1  # Only the mem: key


class TestListBackups:
    def test_empty_directory(self, backup_dir):
        result = list_backups()
        assert result["backups"] == []

    def test_lists_json_files(self, backup_dir):
        (backup_dir / "backup1.json").write_text("{}")
        (backup_dir / "backup2.json").write_text("{}")
        (backup_dir / "readme.txt").write_text("ignore")
        result = list_backups()
        filenames = [b["filename"] for b in result["backups"]]
        assert "backup1.json" in filenames
        assert "backup2.json" in filenames
        assert "readme.txt" not in filenames

    def test_sorted_newest_first(self, backup_dir):
        (backup_dir / "old.json").write_text("{}")
        time.sleep(0.05)
        (backup_dir / "new.json").write_text("{}")
        result = list_backups()
        assert result["backups"][0]["filename"] == "new.json"
