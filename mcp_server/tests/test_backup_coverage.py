"""Coverage tests for tools/backup.py — error handling paths, validation, edge cases."""

import json
import os
from pathlib import Path
from unittest.mock import patch

import pytest

import tools as tools_module
from tests.conftest import store_memory


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
    _safe_filepath,
    _validate_filename,
    dump_to_file,
    list_backups,
    restore_from_file,
)


class TestValidateFilename:
    def test_empty_filename(self):
        assert _validate_filename("") == "Filename cannot be empty"

    def test_invalid_characters(self):
        err = _validate_filename("../evil.json")
        assert err is not None
        assert "Invalid filename" in err

    def test_no_json_extension(self):
        err = _validate_filename("backup.txt")
        assert err is not None

    def test_too_long(self):
        err = _validate_filename("a" * 252 + ".json")
        assert err is not None
        assert "too long" in err

    def test_valid_filename(self):
        assert _validate_filename("backup_2024.json") is None


class TestSafeFilepath:
    def test_valid_path(self, tmp_path):
        with patch("tools.backup._backup_dir", return_value=tmp_path):
            path, err = _safe_filepath("test.json")
            assert err is None
            assert path is not None

    def test_invalid_filename_returns_error(self):
        path, err = _safe_filepath("")
        assert path is None
        assert err is not None


class TestDumpToFile:
    def test_auto_generates_filename(self, tmp_path):
        with patch("tools.backup._backup_dir", return_value=tmp_path):
            result = dump_to_file()
            assert "filename" in result
            assert result["filename"].startswith("memory_backup_")

    def test_counts_namespaces(self, tmp_path, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:ns01", "Episodic content")
        store_memory(fake_store, fake_embedder, "mem:project:ns02", "Project content",
                     namespace="project")
        store_memory(fake_store, fake_embedder, "mem:knowledge:ns03", "Knowledge content",
                     namespace="knowledge")
        with patch("tools.backup._backup_dir", return_value=tmp_path):
            result = dump_to_file("test_ns.json")
            assert result["total_keys"] >= 3

    def test_invalid_filename_returns_error(self, tmp_path):
        with patch("tools.backup._backup_dir", return_value=tmp_path):
            result = dump_to_file("../evil.json")
            assert result["status"] == "error"

    def test_dump_all_exception(self, tmp_path, fake_store):
        with patch("tools.backup._backup_dir", return_value=tmp_path):
            with patch.object(fake_store, "dump_all", side_effect=RuntimeError("DB error")):
                result = dump_to_file("test.json")
                assert result["status"] == "error"
                assert "export" in result["message"]

    def test_file_write_error(self, tmp_path):
        # Point to a directory that doesn't allow writing
        readonly_dir = tmp_path / "readonly"
        readonly_dir.mkdir()
        bad_file = readonly_dir / "subdir" / "test.json"
        with patch("tools.backup._backup_dir", return_value=tmp_path):
            with patch("builtins.open", side_effect=OSError("Permission denied")):
                result = dump_to_file("test.json")
                assert result["status"] == "error"
                assert "write" in result["message"]


class TestRestoreFromFile:
    def test_file_not_found(self, tmp_path):
        with patch("tools.backup._backup_dir", return_value=tmp_path):
            result = restore_from_file("nonexistent.json")
            assert result["status"] == "error"
            assert "not found" in result["message"]

    def test_invalid_filename(self):
        result = restore_from_file("../evil.json")
        assert result["status"] == "error"

    def test_file_too_large(self, tmp_path):
        large_file = tmp_path / "large.json"
        large_file.write_text("{}")
        with patch("tools.backup._backup_dir", return_value=tmp_path):
            with patch("pathlib.Path.stat") as mock_stat:
                mock_stat.return_value.st_size = 200 * 1024 * 1024  # 200MB
                result = restore_from_file("large.json")
                assert result["status"] == "error"
                assert "too large" in result["message"]

    def test_invalid_json(self, tmp_path):
        bad_file = tmp_path / "bad.json"
        bad_file.write_text("not json at all{{{")
        with patch("tools.backup._backup_dir", return_value=tmp_path):
            result = restore_from_file("bad.json")
            assert result["status"] == "error"
            assert "Invalid JSON" in result["message"]

    def test_invalid_backup_format_not_dict(self, tmp_path):
        bad_file = tmp_path / "notdict.json"
        bad_file.write_text('"just a string"')
        with patch("tools.backup._backup_dir", return_value=tmp_path):
            result = restore_from_file("notdict.json")
            assert result["status"] == "error"
            assert "Invalid backup format" in result["message"]

    def test_invalid_data_format(self, tmp_path):
        bad_file = tmp_path / "baddata.json"
        bad_file.write_text(json.dumps({"data": "not a dict", "metadata": {}}))
        with patch("tools.backup._backup_dir", return_value=tmp_path):
            result = restore_from_file("baddata.json")
            assert result["status"] == "error"
            assert "data format" in result["message"]

    def test_invalid_key_prefix(self, tmp_path):
        bad_file = tmp_path / "badkeys.json"
        bad_file.write_text(json.dumps({
            "data": {"evil:key": {"content": "bad"}},
            "metadata": {},
        }))
        with patch("tools.backup._backup_dir", return_value=tmp_path):
            result = restore_from_file("badkeys.json")
            assert result["status"] == "error"
            assert "invalid key prefix" in result["message"]

    def test_dry_run(self, tmp_path):
        backup_file = tmp_path / "dryrun.json"
        backup_file.write_text(json.dumps({
            "data": {"mem:episodic:test01": {"content": "test"}},
            "metadata": {"version": "1.0"},
        }))
        with patch("tools.backup._backup_dir", return_value=tmp_path):
            result = restore_from_file("dryrun.json", dry_run=True)
            assert result["status"] == "dry_run"
            assert result["total_keys_in_backup"] == 1

    def test_actual_restore(self, tmp_path, fake_store):
        backup_file = tmp_path / "restore.json"
        backup_file.write_text(json.dumps({
            "data": {"mem:episodic:restored01": {"content": "restored", "updated_at": "999999"}},
            "metadata": {},
        }))
        with patch("tools.backup._backup_dir", return_value=tmp_path):
            result = restore_from_file("restore.json", dry_run=False)
            assert result["status"] == "restored"
            assert result["restored_keys"] >= 1

    def test_restore_exception(self, tmp_path, fake_store):
        backup_file = tmp_path / "fail.json"
        backup_file.write_text(json.dumps({
            "data": {"mem:episodic:fail01": {"content": "test"}},
            "metadata": {},
        }))
        with patch("tools.backup._backup_dir", return_value=tmp_path):
            with patch.object(fake_store, "restore_all", side_effect=RuntimeError("Restore error")):
                result = restore_from_file("fail.json", dry_run=False)
                assert result["status"] == "error"
                assert "failed" in result["message"].lower()

    def test_os_error_on_read(self, tmp_path):
        bad_file = tmp_path / "oserr.json"
        bad_file.write_text("{}")
        with patch("tools.backup._backup_dir", return_value=tmp_path):
            with patch("builtins.open", side_effect=OSError("Disk error")):
                result = restore_from_file("oserr.json")
                assert result["status"] == "error"


class TestListBackups:
    def test_empty_dir(self, tmp_path):
        with patch("tools.backup._backup_dir", return_value=tmp_path):
            result = list_backups()
            assert result["backups"] == []

    def test_nonexistent_dir(self, tmp_path):
        with patch("tools.backup._backup_dir", return_value=tmp_path / "nonexistent"):
            result = list_backups()
            assert result["backups"] == []

    def test_lists_json_files(self, tmp_path):
        (tmp_path / "backup1.json").write_text("{}")
        (tmp_path / "backup2.json").write_text("{}")
        (tmp_path / "readme.txt").write_text("not a backup")
        with patch("tools.backup._backup_dir", return_value=tmp_path):
            result = list_backups()
            assert len(result["backups"]) == 2
            assert all(b["filename"].endswith(".json") for b in result["backups"])
