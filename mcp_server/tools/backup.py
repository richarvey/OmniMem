"""Backup and restore MCP tools: dump_to_file, restore_from_file, list_backups."""

import json
import logging
import os
import re
import time
from pathlib import Path
from typing import Any

logger = logging.getLogger(__name__)

# Maximum backup file size to load (100 MB)
_MAX_BACKUP_FILE_SIZE = 100 * 1024 * 1024

# Allowed filename pattern: alphanumeric, underscores, hyphens, dots, ending in .json
_SAFE_FILENAME_RE = re.compile(r"^[a-zA-Z0-9_\-]+\.json$")


def _get_deps():
    from ..tools import _store
    return _store


def _backup_dir() -> Path:
    return Path(os.getenv("BACKUP_DIR", "/app/backups"))


def _validate_filename(filename: str) -> str | None:
    """Validate a backup filename is safe. Returns an error message or None if valid."""
    if not filename:
        return "Filename cannot be empty"
    if not _SAFE_FILENAME_RE.match(filename):
        return (
            "Invalid filename. Only alphanumeric characters, underscores, hyphens, "
            "and dots are allowed, and the file must end in .json"
        )
    if len(filename) > 255:
        return "Filename too long (max 255 characters)"
    return None


def _safe_filepath(filename: str) -> tuple[Path | None, str | None]:
    """Return a validated filepath within BACKUP_DIR, or an error message."""
    err = _validate_filename(filename)
    if err:
        return None, err

    backup_path = _backup_dir()
    filepath = (backup_path / filename).resolve()

    # Ensure resolved path is still within backup directory
    try:
        filepath.relative_to(backup_path.resolve())
    except ValueError:
        return None, "Filename must not escape the backup directory"

    return filepath, None


def dump_to_file(filename: str | None = None) -> dict[str, Any]:
    """Export all memories, suppression lists, and recall logs to a JSON backup file. One tool call backs up everything.

    Args:
        filename: Optional filename. Auto-generates one like 'memory_backup_20250309_143200.json' if not provided.

    Returns:
        Dict with filename, path, total_keys, and status.
    """
    store = _get_deps()

    if filename is None:
        ts = time.strftime("%Y%m%d_%H%M%S")
        filename = f"memory_backup_{ts}.json"

    filepath, err = _safe_filepath(filename)
    if err:
        return {"status": "error", "message": err}

    backup_path = _backup_dir()
    backup_path.mkdir(parents=True, exist_ok=True)

    try:
        all_data = store.dump_all()
    except Exception as exc:
        logger.error("Failed to dump data: %s", exc)
        return {"status": "error", "message": "Failed to export data from store"}

    # Count by namespace
    ns_counts: dict[str, int] = {"episodic": 0, "project": 0, "knowledge": 0}
    for key in all_data:
        if key.startswith("mem:episodic:"):
            ns_counts["episodic"] += 1
        elif key.startswith("mem:project:"):
            ns_counts["project"] += 1
        elif key.startswith("mem:knowledge:"):
            ns_counts["knowledge"] += 1

    backup = {
        "metadata": {
            "exported_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "total_keys": len(all_data),
            "namespaces": ns_counts,
            "version": "1.0",
        },
        "data": all_data,
    }

    try:
        with open(filepath, "w", encoding="utf-8") as f:
            json.dump(backup, f, indent=2, ensure_ascii=False)
    except OSError as exc:
        logger.error("Failed to write backup file: %s", exc)
        return {"status": "error", "message": "Failed to write backup file"}

    logger.info("Backup written to %s (%d keys)", filepath, len(all_data))
    return {
        "filename": filename,
        "path": str(filepath),
        "total_keys": len(all_data),
        "status": "success",
    }


def restore_from_file(
    filename: str,
    dry_run: bool = True,
) -> dict[str, Any]:
    """Restore memories from a backup file. Default is dry_run mode which previews without writing.

    Restoring MERGES with existing data — existing keys are only overwritten if the backup version is newer.

    Args:
        filename: The backup filename to restore from (must be in BACKUP_DIR).
        dry_run: If True (default), previews what would be restored without writing. Set to False to actually restore.

    Returns:
        Dict with restored_keys count, skipped_keys count, and status.
    """
    store = _get_deps()

    filepath, err = _safe_filepath(filename)
    if err:
        return {"status": "error", "message": err}

    if not filepath.exists():
        return {"status": "error", "message": "Backup file not found"}

    # Check file size before loading
    file_size = filepath.stat().st_size
    if file_size > _MAX_BACKUP_FILE_SIZE:
        return {
            "status": "error",
            "message": f"Backup file too large ({file_size // 1024 // 1024} MB). "
                       f"Maximum is {_MAX_BACKUP_FILE_SIZE // 1024 // 1024} MB.",
        }

    try:
        with open(filepath, "r", encoding="utf-8") as f:
            backup = json.load(f)
    except json.JSONDecodeError:
        return {"status": "error", "message": "Backup file contains invalid JSON"}
    except OSError:
        return {"status": "error", "message": "Failed to read backup file"}

    if not isinstance(backup, dict):
        return {"status": "error", "message": "Invalid backup format"}

    data = backup.get("data", {})
    metadata = backup.get("metadata", {})

    if not isinstance(data, dict):
        return {"status": "error", "message": "Invalid backup data format"}

    # Validate that all keys in the backup have expected prefixes
    allowed_prefixes = ("mem:", "topics:", "log:recall:")
    for key in data:
        if not isinstance(key, str) or not key.startswith(allowed_prefixes):
            return {
                "status": "error",
                "message": f"Backup contains invalid key prefix: {key[:50]}",
            }

    if dry_run:
        return {
            "status": "dry_run",
            "message": "Preview — no data was written. Call with dry_run=False to restore.",
            "total_keys_in_backup": len(data),
            "metadata": metadata,
        }

    try:
        restored, skipped = store.restore_all(data)
    except Exception as exc:
        logger.error("Restore failed: %s", exc)
        return {"status": "error", "message": "Restore operation failed"}

    logger.info("Restored %d keys, skipped %d from %s", restored, skipped, filename)
    return {
        "status": "restored",
        "restored_keys": restored,
        "skipped_keys": skipped,
        "filename": filename,
    }


def list_backups() -> dict[str, Any]:
    """List all available backup files in the backup directory, sorted newest first.

    Returns:
        List of backups with filename, size_kb, and created_at.
    """
    backup_path = _backup_dir()
    if not backup_path.exists():
        return {"backups": [], "count": 0}

    backups: list[dict[str, Any]] = []
    for f in backup_path.glob("*.json"):
        stat = f.stat()
        backups.append({
            "filename": f.name,
            "size_kb": round(stat.st_size / 1024, 2),
            "created_at": time.strftime(
                "%Y-%m-%dT%H:%M:%SZ", time.gmtime(stat.st_mtime)
            ),
        })

    backups.sort(key=lambda x: x["created_at"], reverse=True)
    return {"backups": backups, "count": len(backups)}
