"""Backup management routes: list, create, preview restore, restore."""

import json
import logging
import os
import time
from pathlib import Path

from starlette.requests import Request
from starlette.responses import FileResponse, HTMLResponse, RedirectResponse
from starlette.routing import Route

from .. import deps

logger = logging.getLogger(__name__)


def _backup_dir() -> Path:
    return Path(os.getenv("BACKUP_DIR", "/app/backups"))


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
    safe_name = "".join(c for c in upload.filename if c.isalnum() or c in "._-")
    if not safe_name.endswith(".json"):
        safe_name += ".json"

    backup_path = _backup_dir()
    backup_path.mkdir(parents=True, exist_ok=True)
    filepath = (backup_path / safe_name).resolve()

    # Path traversal protection
    if not str(filepath).startswith(str(backup_path.resolve())):
        return RedirectResponse(url="/backups?error=Invalid+filename", status_code=303)

    try:
        contents = await upload.read()
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
    filepath = _backup_dir() / filename

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
    filepath = (_backup_dir() / filename).resolve()

    # Path traversal protection
    if not str(filepath).startswith(str(_backup_dir().resolve())):
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
    filepath = (_backup_dir() / filename).resolve()

    # Path traversal protection
    if not str(filepath).startswith(str(_backup_dir().resolve())):
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
    filepath = _backup_dir() / filename

    if not filepath.exists():
        return RedirectResponse(url="/backups?error=Backup+file+not+found", status_code=303)

    try:
        with open(filepath, "r", encoding="utf-8") as f:
            backup = json.load(f)

        data = backup.get("data", {})
        restored, skipped = deps.store.restore_all(data)
        logger.info("Restored from %s: %d keys restored, %d skipped", filename, restored, skipped)
        return RedirectResponse(
            url=f"/backups?message=Restored+{restored}+keys+from+{filename}+(skipped+{skipped})",
            status_code=303,
        )
    except Exception as exc:
        logger.error("Restore failed: %s", exc)
        return RedirectResponse(url=f"/backups?error=Restore+failed:+{exc}", status_code=303)


routes = [
    Route("/backups", backups_page),
    Route("/backups/create", create_backup, methods=["POST"]),
    Route("/backups/upload", upload_backup, methods=["POST"]),
    Route("/backups/{filename}/preview", preview_restore),
    Route("/backups/{filename}/download", download_backup),
    Route("/backups/{filename}/restore", restore_backup, methods=["POST"]),
    Route("/backups/{filename}/delete", delete_backup, methods=["POST"]),
]
