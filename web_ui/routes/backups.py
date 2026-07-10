"""Backup management routes: list, create, preview restore, restore."""

import json
import logging
import os
import re
import time
from pathlib import Path

from starlette.requests import Request
from starlette.responses import FileResponse, HTMLResponse, RedirectResponse
from starlette.routing import Route

from .. import deps

logger = logging.getLogger(__name__)

# Reject anything but a plain <name>.json filename (no slashes, no traversal).
_SAFE_FILENAME_RE = re.compile(r"^[a-zA-Z0-9_\-.]+\.json$")

# Cap uploaded/restored backup files at 100 MB (matches the MCP backup tool).
_MAX_BACKUP_FILE_SIZE = 100 * 1024 * 1024


def _backup_dir() -> Path:
    return Path(os.getenv("BACKUP_DIR", "/app/backups"))


def _safe_filepath(filename: str) -> Path | None:
    """Resolve a request-supplied filename inside BACKUP_DIR, or None if unsafe.

    Guards against path traversal ('../…') and absolute paths by validating the
    filename shape and confirming the resolved path stays within the backup dir.
    """
    if not filename or not _SAFE_FILENAME_RE.match(filename):
        return None
    backup_path = _backup_dir()
    filepath = (backup_path / filename).resolve()
    try:
        filepath.relative_to(backup_path.resolve())
    except ValueError:
        return None
    return filepath


async def backups_page(request: Request) -> HTMLResponse:
    """GET /backups — list backup files with actions."""
    backup_path = _backup_dir()
    backups = []

    if backup_path.exists():
        for f in backup_path.glob("*.json"):
            stat = f.stat()
            backups.append({
                "filename": f.name,
                "size_kb": round(stat.st_size / 1024, 2),
                "modified": time.strftime("%Y-%m-%d %H:%M", time.localtime(stat.st_mtime)),
            })
        backups.sort(key=lambda x: x["modified"], reverse=True)

    template = request.app.state.templates.get_template("backups.html")
    content = template.render(
        request=request,
        current_page="backups",
        backups=backups,
        message=request.query_params.get("message", ""),
        error=request.query_params.get("error", ""),
    )
    return HTMLResponse(content)


async def create_backup(request: Request) -> RedirectResponse:
    """POST /backups/create — create a new backup."""
    ts = time.strftime("%Y%m%d_%H%M%S")
    filename = f"memory_backup_{ts}.json"

    backup_path = _backup_dir()
    backup_path.mkdir(parents=True, exist_ok=True)

    try:
        all_data = deps.store.dump_all()

        ns_counts = {"episodic": 0, "project": 0, "knowledge": 0, "preference": 0}
        for key in all_data:
            if key.startswith("mem:episodic:"):
                ns_counts["episodic"] += 1
            elif key.startswith("mem:project:"):
                ns_counts["project"] += 1
            elif key.startswith("mem:knowledge:"):
                ns_counts["knowledge"] += 1
            elif key.startswith("mem:preference:"):
                ns_counts["preference"] += 1

        from memory.version import __version__
        backup = {
            "metadata": {
                "exported_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                "total_keys": len(all_data),
                "namespaces": ns_counts,
                "version": __version__,
            },
            "data": all_data,
        }

        filepath = backup_path / filename
        with open(filepath, "w", encoding="utf-8") as f:
            json.dump(backup, f, indent=2, ensure_ascii=False)

        logger.info("Backup created via web UI: %s (%d keys)", filename, len(all_data))
        return RedirectResponse(url=f"/backups?message=Backup+created:+{filename}+({len(all_data)}+keys)", status_code=303)
    except Exception as exc:
        logger.error("Backup creation failed: %s", exc)
        return RedirectResponse(url=f"/backups?error=Backup+failed:+{exc}", status_code=303)


async def upload_backup(request: Request) -> RedirectResponse:
    """POST /backups/upload — upload a JSON backup file."""
    form = await request.form()
    upload = form.get("file")

    if upload is None or not upload.filename:
        return RedirectResponse(url="/backups?error=No+file+selected", status_code=303)

    if not upload.filename.endswith(".json"):
        return RedirectResponse(url="/backups?error=Only+.json+files+are+allowed", status_code=303)

    # Sanitise filename: keep only safe characters
    safe_name = "".join(c for c in os.path.basename(upload.filename) if c.isalnum() or c in "._-")
    if not safe_name.endswith(".json"):
        safe_name += ".json"

    filepath = _safe_filepath(safe_name)
    if filepath is None:
        return RedirectResponse(url="/backups?error=Invalid+filename", status_code=303)

    backup_path = _backup_dir()
    backup_path.mkdir(parents=True, exist_ok=True)

    try:
        # Bound the read so a giant upload can't exhaust memory.
        contents = await upload.read(_MAX_BACKUP_FILE_SIZE + 1)
        if len(contents) > _MAX_BACKUP_FILE_SIZE:
            return RedirectResponse(
                url=f"/backups?error=File+too+large+(max+{_MAX_BACKUP_FILE_SIZE // 1024 // 1024}+MB)",
                status_code=303,
            )
        # Validate it's actually JSON
        json.loads(contents)

        with open(filepath, "wb") as f:
            f.write(contents)

        logger.info("Backup uploaded via web UI: %s (%d bytes)", safe_name, len(contents))
        return RedirectResponse(
            url=f"/backups?message=Uploaded+{safe_name}",
            status_code=303,
        )
    except json.JSONDecodeError:
        return RedirectResponse(url="/backups?error=File+is+not+valid+JSON", status_code=303)
    except Exception as exc:
        logger.error("Backup upload failed: %s", exc)
        return RedirectResponse(url=f"/backups?error=Upload+failed:+{exc}", status_code=303)


async def preview_restore(request: Request) -> HTMLResponse:
    """GET /backups/{filename}/preview — preview a restore operation."""
    filename = request.path_params["filename"]
    filepath = _safe_filepath(filename)
    if filepath is None:
        return HTMLResponse('<p class="empty-state">Invalid filename.</p>', status_code=400)

    if not filepath.exists():
        return HTMLResponse('<p class="empty-state">Backup file not found.</p>', status_code=404)

    try:
        with open(filepath, "r", encoding="utf-8") as f:
            backup = json.load(f)
    except (json.JSONDecodeError, OSError):
        return HTMLResponse('<p class="empty-state">Invalid backup file.</p>', status_code=400)

    metadata = backup.get("metadata", {})
    data = backup.get("data", {})

    template = request.app.state.templates.get_template("backups.html")
    content = template.render(
        request=request,
        current_page="backups",
        backups=[],
        message="",
        error="",
        preview={
            "filename": filename,
            "total_keys": len(data),
            "metadata": metadata,
        },
    )
    return HTMLResponse(content)


async def download_backup(request: Request):
    """GET /backups/{filename}/download — download a backup file."""
    filename = request.path_params["filename"]
    filepath = _safe_filepath(filename)
    if filepath is None:
        return RedirectResponse(url="/backups?error=Invalid+filename", status_code=303)

    if not filepath.exists():
        return RedirectResponse(url="/backups?error=Backup+file+not+found", status_code=303)

    return FileResponse(
        filepath,
        media_type="application/json",
        filename=filename,
    )


async def delete_backup(request: Request) -> RedirectResponse:
    """POST /backups/{filename}/delete — permanently delete a backup file."""
    filename = request.path_params["filename"]
    filepath = _safe_filepath(filename)
    if filepath is None:
        return RedirectResponse(url="/backups?error=Invalid+filename", status_code=303)

    if not filepath.exists():
        return RedirectResponse(url="/backups?error=Backup+file+not+found", status_code=303)

    try:
        filepath.unlink()
        logger.info("Backup deleted via web UI: %s", filename)
        return RedirectResponse(url=f"/backups?message=Deleted+{filename}", status_code=303)
    except Exception as exc:
        logger.error("Backup deletion failed: %s", exc)
        return RedirectResponse(url=f"/backups?error=Delete+failed:+{exc}", status_code=303)


async def restore_backup(request: Request) -> RedirectResponse:
    """POST /backups/{filename}/restore — execute restore from backup."""
    filename = request.path_params["filename"]
    filepath = _safe_filepath(filename)
    if filepath is None:
        return RedirectResponse(url="/backups?error=Invalid+filename", status_code=303)

    if not filepath.exists():
        return RedirectResponse(url="/backups?error=Backup+file+not+found", status_code=303)

    if filepath.stat().st_size > _MAX_BACKUP_FILE_SIZE:
        return RedirectResponse(
            url=f"/backups?error=Backup+too+large+(max+{_MAX_BACKUP_FILE_SIZE // 1024 // 1024}+MB)",
            status_code=303,
        )

    try:
        with open(filepath, "r", encoding="utf-8") as f:
            backup = json.load(f)

        data = backup.get("data", {})
        # restore_all returns (restored, skipped, restored_keys).
        restored, skipped, restored_keys = deps.store.restore_all(data)

        # Re-embed restored memories so they are searchable again — backups
        # exclude the binary vector field, so restored hashes have no embedding
        # until we regenerate it from content (mirrors the MCP restore tool).
        # Skills embed their discovery metadata instead — they carry no
        # content field, and relevance search runs over name/description/domain.
        from memory.skills import discovery_text

        re_embedded = 0
        if deps.embedder:
            for key in restored_keys:
                if not key.startswith("mem:"):
                    continue
                fields = data.get(key, {})
                if key.startswith("mem:skill:"):
                    content = discovery_text(
                        fields.get("name", ""), fields.get("description", ""),
                        fields.get("domain", ""),
                    ) if fields.get("name") else ""
                else:
                    content = fields.get("content", "")
                if content:
                    vector = deps.embedder.embed(content)
                    namespace = key.split(":")[1]
                    deps.store.upsert(namespace, key, {}, vector)
                    re_embedded += 1

        logger.info(
            "Restored from %s: %d keys restored, %d skipped, %d re-embedded",
            filename, restored, skipped, re_embedded,
        )
        return RedirectResponse(
            url=f"/backups?message=Restored+{restored}+keys+from+{filename}+(skipped+{skipped})",
            status_code=303,
        )
    except Exception as exc:
        logger.error("Restore failed: %s", exc)
        return RedirectResponse(url="/backups?error=Restore+failed", status_code=303)


routes = [
    Route("/backups", backups_page),
    Route("/backups/create", create_backup, methods=["POST"]),
    Route("/backups/upload", upload_backup, methods=["POST"]),
    Route("/backups/{filename}/preview", preview_restore),
    Route("/backups/{filename}/download", download_backup),
    Route("/backups/{filename}/restore", restore_backup, methods=["POST"]),
    Route("/backups/{filename}/delete", delete_backup, methods=["POST"]),
]
