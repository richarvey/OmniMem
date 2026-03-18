"""Project context MCP tools: set, get, list, and update project context."""

import json
import logging
import re
import time
from typing import Any

from memory.lifecycle import MemoryState

from . import _compact

logger = logging.getLogger(__name__)

# Allowed characters for project names: alphanumeric, hyphens, underscores, dots
_SAFE_PROJECT_NAME_RE = re.compile(r"^[a-zA-Z0-9_\-. ]+$")


def _validate_project_name(project_name: str) -> None:
    """Validate project name is safe for use in Valkey keys."""
    if not project_name or len(project_name) > 200:
        raise ValueError("Project name must be 1-200 characters")
    if not _SAFE_PROJECT_NAME_RE.match(project_name):
        raise ValueError(
            "Project name contains invalid characters. "
            "Only alphanumeric, hyphens, underscores, dots, and spaces are allowed."
        )


def _get_deps():
    from tools import _store, _embedder
    return _store, _embedder


def set_project_context(
    project_name: str,
    description: str,
    stack: str,
    goals: str,
    current_state: str,
    notes: str | None = None,
) -> dict[str, Any]:
    """Create or update a project's context (description, stack, goals, state).

    Args:
        project_name: Unique project identifier.
        description: What the project does.
        stack: Technology stack.
        goals: Current objectives.
        current_state: Current project state.
        notes: Freeform notes for next session.
    """
    store, embedder = _get_deps()

    _validate_project_name(project_name)

    key = f"mem:project:{project_name}"
    now = str(time.time())

    embed_text = f"{description} {goals} {current_state}"
    vector = embedder.embed(embed_text)

    fields: dict[str, Any] = {
        "content": description,
        "project_name": project_name,
        "description": description,
        "stack": stack,
        "goals": goals,
        "current_state": current_state,
        "state": MemoryState.ACTIVE.value,
        "surface_score": "1.0",
        "created_at": now,
        "updated_at": now,
    }
    if notes:
        fields["notes"] = notes

    store.upsert("project", key, fields, vector)
    logger.info("Saved project context: %s", project_name)
    return {"project_name": project_name}


def get_project_context(project_name: str) -> dict[str, Any]:
    """Retrieve full context for a project by name.

    Args:
        project_name: Project to look up.
    """
    store, _ = _get_deps()

    _validate_project_name(project_name)

    key = f"mem:project:{project_name}"
    data = store.get(key)

    if data is None:
        return {"status": "not_found"}

    return _compact({
        "status": "found",
        "project_name": data.get("project_name", project_name),
        "description": data.get("description", ""),
        "stack": data.get("stack", ""),
        "goals": data.get("goals", ""),
        "current_state": data.get("current_state", ""),
        "notes": data.get("notes"),
        "state": data.get("state", "active"),
        "updated_at": data.get("updated_at"),
    })


def list_projects() -> dict[str, Any]:
    """List all stored project contexts, deduplicated by project name."""
    store, _ = _get_deps()

    keys = store.scan_prefix("mem:project:")

    # Batch fetch all project data in one pipeline round-trip
    all_data = store.get_multi(keys)

    # Group by resolved project name to deduplicate
    project_map: dict[str, dict[str, Any]] = {}
    for key, data in zip(keys, all_data):
        if not data:
            continue
        # Resolve name: prefer project_name, fall back to project field, then key suffix
        name = data.get("project_name") or data.get("project") or key.split(":")[-1]
        is_context = bool(data.get("goals") or data.get("stack"))

        if name not in project_map:
            project_map[name] = {
                "project_name": name,
                "description": "",
                "state": "active",
                "memory_count": 0,
            }

        if is_context:
            project_map[name]["description"] = (data.get("description") or "")[:80]
            project_map[name]["state"] = data.get("state", "active")
        else:
            project_map[name]["memory_count"] += 1

    projects = sorted(project_map.values(), key=lambda p: p["project_name"].lower())
    return {"projects": projects}


def update_project_state(
    project_name: str,
    current_state: str,
    notes: str | None = None,
) -> dict[str, Any]:
    """Update a project's current state and notes without re-embedding.

    Args:
        project_name: Project to update.
        current_state: New state description.
        notes: Notes for next session.
    """
    store, _ = _get_deps()

    _validate_project_name(project_name)

    key = f"mem:project:{project_name}"
    data = store.get(key)

    if data is None:
        return {"status": "not_found"}

    now = str(time.time())
    # Single round-trip instead of 2-3 individual set_field calls
    updates: dict[str, str] = {
        "current_state": current_state,
        "updated_at": now,
    }
    if notes is not None:
        updates["notes"] = notes
    store.set_fields(key, updates)

    logger.info("Updated project state: %s", project_name)
    return {"project_name": project_name}
