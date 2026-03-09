"""Project context MCP tools: set, get, list, and update project context."""

import json
import logging
import time
from typing import Any

from ..memory.lifecycle import MemoryState

logger = logging.getLogger(__name__)


def _get_deps():
    from ..tools import _store, _embedder
    return _store, _embedder


def set_project_context(
    project_name: str,
    description: str,
    stack: str,
    goals: str,
    current_state: str,
    notes: str | None = None,
) -> dict[str, Any]:
    """Create or update a project context memory. Stores project metadata and embeds a semantic summary for recall.

    Args:
        project_name: Unique project identifier (e.g. 'omnimem', 'my-api').
        description: What the project is and does.
        stack: Technology stack (e.g. 'Python, Valkey, Docker').
        goals: Current goals or objectives.
        current_state: What state the project is in right now.
        notes: Optional freeform notes for the next session.

    Returns:
        Dict with project_name and status.
    """
    store, embedder = _get_deps()

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
    return {"project_name": project_name, "status": "saved"}


def get_project_context(project_name: str) -> dict[str, Any]:
    """Retrieve the full context for a project by name.

    Args:
        project_name: The project name to look up.

    Returns:
        All project fields as a structured dict, or a not_found status with a suggestion.
    """
    store, _ = _get_deps()

    key = f"mem:project:{project_name}"
    data = store.get(key)

    if data is None:
        return {
            "status": "not_found",
            "suggestion": "Use set_project_context to create one",
        }

    return {
        "status": "found",
        "project_name": data.get("project_name", project_name),
        "description": data.get("description", ""),
        "stack": data.get("stack", ""),
        "goals": data.get("goals", ""),
        "current_state": data.get("current_state", ""),
        "notes": data.get("notes"),
        "state": data.get("state", "active"),
        "created_at": data.get("created_at"),
        "updated_at": data.get("updated_at"),
    }


def list_projects() -> dict[str, Any]:
    """List all stored project contexts.

    Returns:
        List of projects with name, description, updated_at, and state.
    """
    store, _ = _get_deps()

    keys = store.scan_prefix("mem:project:")
    projects: list[dict[str, Any]] = []

    for key in keys:
        data = store.get(key)
        if data:
            projects.append({
                "project_name": data.get("project_name", key.split(":")[-1]),
                "description": data.get("description", "")[:100],
                "updated_at": data.get("updated_at"),
                "state": data.get("state", "active"),
            })

    return {"projects": projects, "count": len(projects)}


def update_project_state(
    project_name: str,
    current_state: str,
    notes: str | None = None,
) -> dict[str, Any]:
    """Update only the current state and notes of a project without re-embedding.

    Args:
        project_name: The project name to update.
        current_state: New current state description.
        notes: Optional notes for the next session.

    Returns:
        Dict with project_name and status.
    """
    store, _ = _get_deps()

    key = f"mem:project:{project_name}"
    data = store.get(key)

    if data is None:
        return {
            "status": "not_found",
            "suggestion": "Use set_project_context to create one first",
        }

    now = str(time.time())
    store.set_field(key, "current_state", current_state)
    store.set_field(key, "updated_at", now)
    if notes is not None:
        store.set_field(key, "notes", notes)

    logger.info("Updated project state: %s", project_name)
    return {"project_name": project_name, "status": "updated"}
