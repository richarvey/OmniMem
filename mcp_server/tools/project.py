"""Project context MCP tools: set, get, list, update, and compile project context."""

import json
import logging
import re
import time
from collections import Counter
from typing import Any

from memory.lifecycle import MemoryState
from memory.project_domains import (
    invalidate_domain_cache,
    known_project_domains,
    normalise_domains,
    read_project_domains,
    serialise_domains,
    suggest_domains_for_project,
)

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
    domains: list[str] | str | None = None,
) -> dict[str, Any]:
    """Create or update a project's context (description, stack, goals, state).

    Args:
        project_name: Unique project identifier.
        description: What the project does.
        stack: Technology stack.
        goals: Current objectives.
        current_state: Current project state.
        notes: Freeform notes for next session.
        domains: Kinds of work in this project ('python', 'docker', 'design'),
            sharing the compiled-skill vocabulary so recall(domain_filter=...)
            and find_skills() speak the same names. Omit to leave any existing
            domains untouched; pass [] to clear them. Use
            compile_project_domains() to have them suggested from the stack
            and the project's own memories.
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

    # None means "don't touch"; an empty list means "clear". Distinguishing
    # them matters because this tool is also the update path — a caller
    # refreshing goals shouldn't silently wipe the project's domains.
    resolved: list[str] | None = None
    aliased: dict[str, str] = {}
    rejected: list[str] = []
    if domains is None:
        existing = store.get(key)
        if existing is not None:
            resolved = read_project_domains(existing)
    else:
        resolved, aliased, rejected = normalise_domains(domains)
    if resolved is not None:
        fields["domains"] = serialise_domains(resolved)

    store.upsert("project", key, fields, vector)
    invalidate_domain_cache()
    logger.info("Saved project context: %s", project_name)

    return _compact({
        "project_name": project_name,
        "domains": resolved or None,
        "resolved_aliases": aliased or None,
        "rejected_domains": rejected or None,
    })


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
        "domains": read_project_domains(data) or None,
        "goals": data.get("goals", ""),
        "current_state": data.get("current_state", ""),
        "notes": data.get("notes"),
        "state": data.get("state", "active"),
        "updated_at": data.get("updated_at"),
    })


def list_projects(domain: str | None = None) -> dict[str, Any]:
    """List all stored project contexts, deduplicated by project name.

    Args:
        domain: Only list projects declaring this work-type domain
            (e.g. 'python'). Aliases resolve the same way skill domains do.
    """
    store, _ = _get_deps()

    wanted: str | None = None
    if domain:
        resolved, _, rejected = normalise_domains(domain)
        if rejected or not resolved:
            raise ValueError(
                f"Invalid domain filter: {domain!r}. Use 1-64 characters of "
                "lowercase letters, digits, hyphens, underscores or dots."
            )
        wanted = resolved[0]

    keys = store.scan_prefix("mem:project:")

    # One round-trip, only the fields the listing shows
    all_data = store.get_fields_multi(
        keys,
        ("project_name", "project", "goals", "stack", "description", "state",
         "domains"),
    )

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
                "domains": [],
            }

        if is_context:
            project_map[name]["description"] = (data.get("description") or "")[:80]
            project_map[name]["state"] = data.get("state", "active")
            project_map[name]["domains"] = read_project_domains(data)
        else:
            project_map[name]["memory_count"] += 1

    projects = sorted(project_map.values(), key=lambda p: p["project_name"].lower())
    if wanted is not None:
        projects = [p for p in projects if wanted in p["domains"]]

    result: dict[str, Any] = {
        "projects": [
            {k: v for k, v in p.items() if k != "domains" or v} for p in projects
        ]
    }
    if wanted is not None:
        result["domain"] = wanted
        if not projects:
            result["note"] = (
                f"No project declares the domain '{wanted}'. "
                "Run compile_project_domains(project_name) to suggest domains "
                "for a project from its stack and memories."
            )
    return result


def compile_project_domains(
    project_name: str,
    auto_save: bool = False,
) -> dict[str, Any]:
    """Suggest work-type domains for a project from its stack and its own memories, with the evidence behind each one. Returns a draft by default; pass auto_save=True to store it.

    Domains are what makes cross-project recall work: once projects declare
    them, recall(domain_filter='python') searches every Python project at
    once. They share the compiled-skill vocabulary, so the same names reach
    find_skills() and get_skill().

    Suggestions are merged with any domains the project already declares —
    this never removes one.

    Args:
        project_name: Project to suggest domains for.
        auto_save: If True, write the merged domain list to the project context.
    """
    store, _ = _get_deps()

    _validate_project_name(project_name)

    key = f"mem:project:{project_name}"
    if store.get(key) is None:
        return {
            "status": "not_found",
            "project_name": project_name,
            "note": (
                "No project context stored. Create one with "
                "set_project_context() or compile_project_context() first."
            ),
        }

    suggestion = suggest_domains_for_project(store, project_name)

    saved = False
    if auto_save and suggestion["merged_domains"] != suggestion["existing_domains"]:
        store.set_fields(key, {
            "domains": serialise_domains(suggestion["merged_domains"]),
            "updated_at": str(time.time()),
        })
        invalidate_domain_cache()
        saved = True
        logger.info(
            "compile_project_domains('%s'): saved %d domains",
            project_name, len(suggestion["merged_domains"]),
        )

    result: dict[str, Any] = {
        "status": "compiled",
        "project_name": project_name,
        "existing_domains": suggestion["existing_domains"],
        "suggested_domains": suggestion["suggested_domains"],
        "merged_domains": suggestion["merged_domains"],
    }
    if suggestion["evidence"]:
        result["evidence"] = suggestion["evidence"]
    if saved:
        result["auto_saved"] = True
    elif not suggestion["suggested_domains"]:
        result["note"] = (
            "Nothing new to suggest. Domains are read from the project's "
            "stack field and from tags that recur across its memories — set "
            "them by hand with set_project_context(domains=[...]) if neither "
            "carries the signal."
        )
    else:
        result["note"] = "Call again with auto_save=True to store these."

    known = known_project_domains(store)
    if known:
        result["domains_in_use"] = dict(
            sorted(known.items(), key=lambda kv: (-kv[1], kv[0]))
        )
    return result


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


def delete_project(
    project_name: str,
    confirm: bool = False,
    include_context: bool = False,
) -> dict[str, Any]:
    """Bulk delete every memory belonging to a project. Requires confirm=True; returns a preview otherwise.

    Finds memories by scanning keys directly (no semantic search), so it
    catches everything — including memories that recall can't surface.
    Deletes in pipelined batches rather than one call per key.

    Args:
        project_name: Project whose memories should be deleted.
        confirm: Must be True to delete. False returns a preview with counts.
        include_context: Also delete the project's context entry
            (mem:project:<name>). Default False keeps it.
    """
    store, _ = _get_deps()

    _validate_project_name(project_name)

    # Direct key scan across all namespaces — a project's memories carry
    # either `project` (episodic/knowledge/preference and ULID project
    # memories) or `project_name` (project context entries).
    to_delete: dict[str, list[str]] = {}
    for ns in ("episodic", "project", "knowledge", "preference"):
        keys = store.scan_prefix(f"mem:{ns}:")
        if not keys:
            continue
        rows = store.get_fields_multi(keys, ("project", "project_name"))
        matched = []
        for key, row in zip(keys, rows):
            if row is None:
                continue
            doc_project = row.get("project") or row.get("project_name")
            if doc_project != project_name:
                continue
            # The context entry is only deleted when explicitly requested.
            if key == f"mem:project:{project_name}" and not include_context:
                continue
            matched.append(key)
        if matched:
            to_delete[ns] = matched

    total = sum(len(v) for v in to_delete.values())
    counts = {ns: len(v) for ns, v in to_delete.items()}

    if total == 0:
        return {"status": "not_found", "project_name": project_name}

    if not confirm:
        return {
            "status": "preview",
            "project_name": project_name,
            "would_delete": counts,
            "total": total,
            "note": "Call again with confirm=True to delete.",
        }

    deleted = 0
    for keys in to_delete.values():
        deleted += store.delete_many(keys)

    # The project context may have been one of them — its domains no longer
    # route anything.
    invalidate_domain_cache()

    # Deleted memories may have carried abandoned-approach entries.
    from tools import _pipeline
    if _pipeline is not None:
        _pipeline.invalidate_abandoned_cache()

    logger.info(
        "delete_project('%s'): deleted %d memories (%s)",
        project_name, deleted,
        ", ".join(f"{ns}={n}" for ns, n in counts.items()),
    )
    return {
        "status": "deleted",
        "project_name": project_name,
        "deleted": counts,
        "total": deleted,
    }


def deprioritise_project(
    project_name: str,
    confirm: bool = False,
    reason: str | None = None,
    include_context: bool = False,
) -> dict[str, Any]:
    """Bulk deprioritise every active memory in a project (0.2x recall visibility, reversible). Requires confirm=True; returns a preview otherwise.

    Like delete_project but non-destructive: memories stay stored and searchable,
    just heavily down-weighted in recall. Undo the whole project with
    reinstate_project(). Only memories currently in the active state are changed;
    already-deprioritised or archived ones are reported under `already_inactive`.

    Args:
        project_name: Project whose memories should be deprioritised.
        confirm: Must be True to apply. False returns a preview with counts.
        reason: Optional note stored on each memory explaining why.
        include_context: Also deprioritise the project's context entry
            (mem:project:<name>). Default False keeps it active.
    """
    store, _ = _get_deps()
    _validate_project_name(project_name)

    from memory.lifecycle import MemoryState, bulk_transition_project

    result = bulk_transition_project(
        store, project_name, MemoryState.DEPRIORITISED,
        apply=confirm, reason=reason, include_context=include_context,
    )

    if result["total"] == 0:
        status = "nothing_to_change" if result["skipped"] else "not_found"
        return _compact({
            "status": status,
            "project_name": project_name,
            "already_inactive": result["skipped"] or None,
        })

    if not confirm:
        return _compact({
            "status": "preview",
            "project_name": project_name,
            "would_deprioritise": result["counts"],
            "total": result["total"],
            "already_inactive": result["skipped"] or None,
            "note": "Call again with confirm=True to deprioritise.",
        })

    logger.info(
        "deprioritise_project('%s'): deprioritised %d memories (%s)",
        project_name, result["changed"],
        ", ".join(f"{ns}={n}" for ns, n in result["counts"].items()),
    )
    return _compact({
        "status": "deprioritised",
        "project_name": project_name,
        "deprioritised": result["counts"],
        "total": result["changed"],
        "already_inactive": result["skipped"] or None,
    })


def reinstate_project(
    project_name: str,
    confirm: bool = False,
    include_context: bool = False,
) -> dict[str, Any]:
    """Bulk reinstate every deprioritised or archived memory in a project back to active. Requires confirm=True; returns a preview otherwise. The inverse of deprioritise_project().

    Args:
        project_name: Project whose memories should be reactivated.
        confirm: Must be True to apply. False returns a preview with counts.
        include_context: Also reinstate the project's context entry
            (mem:project:<name>). Default False leaves it as-is.
    """
    store, _ = _get_deps()
    _validate_project_name(project_name)

    from memory.lifecycle import MemoryState, bulk_transition_project

    result = bulk_transition_project(
        store, project_name, MemoryState.ACTIVE,
        apply=confirm, include_context=include_context,
    )

    if result["total"] == 0:
        status = "nothing_to_change" if result["skipped"] else "not_found"
        return _compact({
            "status": status,
            "project_name": project_name,
            "already_active": result["skipped"] or None,
        })

    if not confirm:
        return _compact({
            "status": "preview",
            "project_name": project_name,
            "would_reinstate": result["counts"],
            "total": result["total"],
            "already_active": result["skipped"] or None,
            "note": "Call again with confirm=True to reinstate.",
        })

    logger.info(
        "reinstate_project('%s'): reinstated %d memories (%s)",
        project_name, result["changed"],
        ", ".join(f"{ns}={n}" for ns, n in result["counts"].items()),
    )
    return _compact({
        "status": "reinstated",
        "project_name": project_name,
        "reinstated": result["counts"],
        "total": result["changed"],
        "already_active": result["skipped"] or None,
    })


def compile_project_context(
    project_name: str,
    auto_save: bool = False,
) -> dict[str, Any]:
    """Gather all stored memories for a project and compile them into a structured context draft. Use this before set_project_context() to auto-produce or refresh a project's context from its episodic memories, experience data, and abandoned approaches.

    Args:
        project_name: Project to compile context for.
        auto_save: If True, automatically save the compiled context (creates or updates).
    """
    store, embedder = _get_deps()

    _validate_project_name(project_name)

    # 1. Fetch existing project context (if any)
    existing_key = f"mem:project:{project_name}"
    existing_data = store.get(existing_key)
    existing_context: dict[str, Any] | None = None
    if existing_data and (existing_data.get("goals") or existing_data.get("stack")):
        existing_context = _compact({
            "description": existing_data.get("description", ""),
            "stack": existing_data.get("stack", ""),
            "domains": read_project_domains(existing_data) or None,
            "goals": existing_data.get("goals", ""),
            "current_state": existing_data.get("current_state", ""),
            "notes": existing_data.get("notes"),
            "updated_at": existing_data.get("updated_at"),
        })

    # 2. Scan all episodic memories for this project
    keys = store.scan_prefix("mem:episodic:")
    all_data = store.get_multi(keys) if keys else []

    memories: list[dict[str, Any]] = []
    tag_counter: Counter[str] = Counter()
    breakthroughs: list[str] = []
    gotchas: list[str] = []
    abandoned: list[dict[str, str]] = []

    for key, data in zip(keys, all_data):
        if data is None:
            continue
        if data.get("state") not in ("active", None):
            continue
        doc_project = data.get("project") or data.get("project_name")
        if doc_project != project_name:
            continue

        content = data.get("content", "")
        updated_at = data.get("updated_at", "0")

        # Collect tags
        tags_raw = data.get("tags", "[]")
        try:
            tags = json.loads(tags_raw)
            if isinstance(tags, list):
                for t in tags:
                    if isinstance(t, str) and t:
                        tag_counter[t] += 1
        except (json.JSONDecodeError, TypeError):
            tags = []

        # Collect experience data
        effort_raw = data.get("effort_score")
        effort = None
        if effort_raw is not None:
            try:
                effort = int(float(effort_raw))
            except (ValueError, TypeError):
                pass

        outcome = data.get("outcome")

        entry: dict[str, Any] = {
            "key": key,
            "content": content,
            "updated_at": updated_at,
        }
        if tags:
            entry["tags"] = tags
        if effort is not None:
            entry["effort_score"] = effort
        if outcome:
            entry["outcome"] = outcome
        memories.append(entry)

        # Collect breakthroughs and gotchas
        bt = data.get("breakthrough")
        if bt:
            breakthroughs.append(bt)
        gc = data.get("gotchas")
        if gc:
            gotchas.append(gc)

        # Collect abandoned approaches
        abandoned_raw = data.get("abandoned_approaches", "[]")
        try:
            approaches = json.loads(abandoned_raw)
            if isinstance(approaches, list):
                for a in approaches:
                    if isinstance(a, dict) and a.get("name"):
                        abandoned.append({
                            "name": a["name"],
                            "type": a.get("type", ""),
                            "reason": a.get("reason", ""),
                        })
        except (json.JSONDecodeError, TypeError):
            pass

    # Sort memories by updated_at descending (most recent first)
    memories.sort(key=lambda m: float(m.get("updated_at", "0")), reverse=True)

    # Deduplicate abandoned approaches by name
    seen_names: set[str] = set()
    unique_abandoned: list[dict[str, str]] = []
    for a in abandoned:
        name_lower = a["name"].lower()
        if name_lower not in seen_names:
            seen_names.add(name_lower)
            unique_abandoned.append(a)

    # 3. Build the draft context
    top_tags = [tag for tag, _ in tag_counter.most_common(20)]

    # Work-type domains, derived from the same two sources the standalone
    # compile_project_domains() reads. Existing domains are never dropped.
    domain_suggestion = suggest_domains_for_project(store, project_name)
    draft_domains = domain_suggestion["merged_domains"]

    # Compile notes from breakthroughs, gotchas, and abandoned approaches
    notes_parts: list[str] = []
    if breakthroughs:
        notes_parts.append("Breakthroughs: " + "; ".join(breakthroughs))
    if gotchas:
        notes_parts.append("Gotchas: " + "; ".join(gotchas))
    if unique_abandoned:
        dead_ends = "; ".join(
            f"{a['name']} ({a['reason']})" for a in unique_abandoned
        )
        notes_parts.append("Abandoned approaches: " + dead_ends)

    compiled_notes = "\n".join(notes_parts) if notes_parts else ""

    # Use most recent memories as current_state summary
    recent_snippets = [
        m["content"][:200] for m in memories[:5]
    ]
    compiled_state = "\n---\n".join(recent_snippets) if recent_snippets else ""

    draft: dict[str, Any] = _compact({
        "description": (
            existing_context["description"]
            if existing_context and existing_context.get("description")
            else ""
        ),
        "stack": (
            existing_context["stack"]
            if existing_context and existing_context.get("stack")
            else ", ".join(top_tags)
        ),
        "domains": draft_domains or None,
        "goals": (
            existing_context.get("goals", "")
            if existing_context
            else ""
        ),
        "current_state": compiled_state,
        "notes": compiled_notes,
    })

    # 4. Optionally auto-save
    saved = False
    if auto_save and (memories or existing_context):
        now = str(time.time())
        description = draft.get("description", "")
        stack = draft.get("stack", "")
        goals = draft.get("goals", "")
        current_state = draft.get("current_state", "")
        notes = draft.get("notes", "")

        embed_text = f"{description} {goals} {current_state}"
        vector = embedder.embed(embed_text)

        fields: dict[str, Any] = {
            "content": description,
            "project_name": project_name,
            "description": description,
            "stack": stack,
            "goals": goals,
            "current_state": current_state,
            "notes": notes,
            "domains": serialise_domains(draft_domains),
            "state": MemoryState.ACTIVE.value,
            "surface_score": "1.0",
            "updated_at": now,
        }
        if existing_data is None:
            fields["created_at"] = now

        store.upsert("project", existing_key, fields, vector)
        invalidate_domain_cache()
        saved = True
        logger.info("Auto-saved compiled project context: %s", project_name)

    # 5. Return structured result
    result: dict[str, Any] = {
        "project_name": project_name,
        "memory_count": len(memories),
        "draft": draft,
    }
    if existing_context:
        result["existing_context"] = existing_context
    if top_tags:
        result["top_tags"] = top_tags
    if domain_suggestion["evidence"]:
        result["domain_evidence"] = domain_suggestion["evidence"]
    if unique_abandoned:
        result["abandoned_approaches"] = unique_abandoned
    if breakthroughs:
        result["breakthroughs"] = breakthroughs
    if gotchas:
        result["gotchas"] = gotchas
    if memories:
        result["memories"] = memories
    if saved:
        result["auto_saved"] = True

    return result
