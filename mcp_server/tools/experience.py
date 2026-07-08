"""Experience scoring tools: record_experience, log_abandoned, get_experience, experience_summary, warn_if_abandoned."""

import json
import logging
import time
from typing import Any

from memory.recall import compute_experience_weight

from . import _compact

logger = logging.getLogger(__name__)

VALID_OUTCOMES = {"succeeded", "pivoted", "abandoned"}
VALID_APPROACH_TYPES = {"library", "approach", "tool", "pattern", "service"}


def _get_deps():
    from tools import _store, _lifecycle, _pipeline
    return _store, _lifecycle, _pipeline


def _validate_memory_key(key: str) -> None:
    """Ensure key starts with a valid memory prefix."""
    if not key.startswith("mem:"):
        raise ValueError("Key must start with 'mem:' prefix")


def record_experience(
    key: str,
    effort_score: int,
    outcome: str,
    iterations: int = 1,
    abandoned_approaches: list[dict[str, str]] | None = None,
    breakthrough: str | None = None,
    gotchas: str | None = None,
) -> dict[str, Any]:
    """Record effort, outcome, dead ends, and breakthroughs for a memory. High-effort successes surface more; high-effort failures (>=4, abandoned) auto-suppress the approach names.

    Args:
        key: Memory key to attach experience to.
        effort_score: 1-5 (1=trivial, 3=moderate, 5=battle-hardened).
        outcome: 'succeeded', 'pivoted', or 'abandoned'.
        iterations: Number of attempts.
        abandoned_approaches: List of dicts with 'name', 'type', 'reason'.
        breakthrough: What finally worked.
        gotchas: Caveats to watch for.
    """
    store, lifecycle, pipeline = _get_deps()

    _validate_memory_key(key)

    if not 1 <= effort_score <= 5:
        raise ValueError(f"effort_score must be 1-5, got {effort_score}")

    if outcome not in VALID_OUTCOMES:
        raise ValueError(
            f"outcome must be one of {VALID_OUTCOMES}, got '{outcome}'"
        )

    data = store.get(key)
    if data is None:
        raise ValueError(f"Memory key not found: {key}")

    experience_weight = compute_experience_weight(effort_score, outcome)
    now = str(time.time())

    # Batch all field updates into a single round-trip
    updates: dict[str, str] = {
        "effort_score": str(effort_score),
        "outcome": outcome,
        "iterations": str(iterations),
        "experience_weight": str(experience_weight),
        "updated_at": now,
    }

    if abandoned_approaches:
        existing_raw = data.get("abandoned_approaches", "[]")
        try:
            existing = json.loads(existing_raw)
        except (json.JSONDecodeError, TypeError):
            existing = []
        existing.extend(abandoned_approaches)
        updates["abandoned_approaches"] = json.dumps(existing)

    if breakthrough:
        updates["breakthrough"] = breakthrough

    if gotchas:
        updates["gotchas"] = gotchas

    store.set_fields(key, updates)

    if abandoned_approaches and pipeline is not None:
        pipeline.invalidate_abandoned_cache()

    # Auto-suppress abandoned approach names if high effort and abandoned
    auto_suppressed: list[str] = []
    if effort_score >= 4 and outcome == "abandoned" and abandoned_approaches:
        for approach in abandoned_approaches:
            name = approach.get("name", "")
            if name:
                lifecycle.suppress_topic(name)
                auto_suppressed.append(name)
                logger.info(
                    "Auto-suppressed topic '%s' — abandoned with effort score %d",
                    name, effort_score,
                )

    result: dict[str, Any] = {
        "key": key,
        "effort_score": effort_score,
        "outcome": outcome,
        "experience_weight": experience_weight,
    }

    if auto_suppressed:
        result["auto_suppressed"] = auto_suppressed

    return result


def log_abandoned(
    key: str,
    name: str,
    type: str,
    reason: str,
) -> dict[str, Any]:
    """Append a dead-end approach to a memory's abandoned list.

    Args:
        key: Memory key.
        name: Abandoned approach name.
        type: 'library', 'approach', 'tool', 'pattern', or 'service'.
        reason: Why it was abandoned.
    """
    store, _, pipeline = _get_deps()

    _validate_memory_key(key)

    if type not in VALID_APPROACH_TYPES:
        raise ValueError(
            f"type must be one of {VALID_APPROACH_TYPES}, got '{type}'"
        )

    data = store.get(key)
    if data is None:
        raise ValueError(f"Memory key not found: {key}")

    existing_raw = data.get("abandoned_approaches", "[]")
    try:
        existing = json.loads(existing_raw)
    except (json.JSONDecodeError, TypeError):
        existing = []

    new_entry = {
        "name": name,
        "type": type,
        "reason": reason,
        "attempted_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    }
    existing.append(new_entry)

    # Single round-trip instead of two set_field calls
    store.set_fields(key, {
        "abandoned_approaches": json.dumps(existing),
        "updated_at": str(time.time()),
    })

    if pipeline is not None:
        pipeline.invalidate_abandoned_cache()

    return {
        "key": key,
        "abandoned_count": len(existing),
        "latest_entry": new_entry,
    }


def get_experience(key: str) -> dict[str, Any]:
    """Return experience data for a memory key.

    Args:
        key: Memory key to look up.
    """
    store, _, _ = _get_deps()

    _validate_memory_key(key)

    data = store.get(key)
    if data is None:
        return {"status": "not_found"}

    effort_raw = data.get("effort_score")
    if effort_raw is None:
        return {"status": "no_experience"}

    try:
        effort = int(float(effort_raw))
    except (ValueError, TypeError):
        return {"status": "no_experience"}

    outcome = data.get("outcome", "unknown")
    iterations_raw = data.get("iterations", "1")
    try:
        iterations = int(float(iterations_raw))
    except (ValueError, TypeError):
        iterations = 1

    abandoned_raw = data.get("abandoned_approaches", "[]")
    try:
        abandoned = json.loads(abandoned_raw)
    except (json.JSONDecodeError, TypeError):
        abandoned = []

    return _compact({
        "status": "found",
        "key": key,
        "effort_score": effort,
        "outcome": outcome,
        "iterations": iterations,
        "abandoned_approaches": abandoned,
        "breakthrough": data.get("breakthrough"),
        "gotchas": data.get("gotchas"),
        "experience_weight": data.get("experience_weight", "1.0"),
    })


def experience_summary(project: str | None = None) -> dict[str, Any]:
    """Aggregate experience stats: effort, outcomes, graveyard of abandoned approaches, breakthroughs.

    Args:
        project: Filter to a project.
    """
    store, _, _ = _get_deps()

    keys = store.scan_prefix("mem:episodic:")
    total_effort = 0
    count_with_experience = 0
    outcome_counts: dict[str, int] = {"succeeded": 0, "pivoted": 0, "abandoned": 0}
    effortful: list[dict[str, Any]] = []
    all_abandoned: list[dict[str, Any]] = []
    breakthroughs: list[dict[str, Any]] = []

    # Fetch only the fields this summary needs, in one round-trip — avoids
    # dragging back tags, gotchas, reinstate_hints, contradictions, vectors, etc.
    all_data = store.get_fields_multi(
        keys,
        ("effort_score", "outcome", "content", "abandoned_approaches",
         "breakthrough", "project"),
    )

    for key, data in zip(keys, all_data):
        if data is None:
            continue

        if project:
            mem_project = data.get("project")
            if mem_project != project:
                continue

        effort_raw = data.get("effort_score")
        if effort_raw is None:
            continue

        try:
            effort = int(float(effort_raw))
        except (ValueError, TypeError):
            continue

        outcome = data.get("outcome", "unknown")
        count_with_experience += 1
        total_effort += effort

        if outcome in outcome_counts:
            outcome_counts[outcome] += 1

        content = data.get("content", "")[:80]  # standardised snippet length
        effortful.append({
            "key": key,
            "content": content,
            "effort_score": effort,
            "outcome": outcome,
        })

        # Collect abandoned approaches
        abandoned_raw = data.get("abandoned_approaches", "[]")
        try:
            abandoned = json.loads(abandoned_raw)
        except (json.JSONDecodeError, TypeError):
            abandoned = []

        for approach in abandoned:
            if isinstance(approach, dict):
                all_abandoned.append({
                    "name": approach.get("name", "?"),
                    "type": approach.get("type", "?"),
                    "reason": approach.get("reason", ""),
                    "effort_score": effort,
                })

        # Collect breakthroughs
        breakthrough = data.get("breakthrough")
        if breakthrough and outcome == "succeeded":
            breakthroughs.append({
                "key": key,
                "effort_score": effort,
                "breakthrough": breakthrough,
            })

    # Sort and limit
    effortful.sort(key=lambda x: x["effort_score"], reverse=True)
    breakthroughs.sort(key=lambda x: x["effort_score"], reverse=True)

    # Deduplicate graveyard by name
    seen_names: set[str] = set()
    unique_abandoned: list[dict[str, Any]] = []
    for item in all_abandoned:
        name = item["name"].lower()
        if name not in seen_names:
            seen_names.add(name)
            unique_abandoned.append(item)

    avg_effort = round(total_effort / count_with_experience, 2) if count_with_experience else 0

    return _compact({
        "memories_with_experience": count_with_experience,
        "average_effort_score": avg_effort,
        "outcome_breakdown": outcome_counts,
        "top_5_most_effortful": effortful[:5],
        "graveyard": unique_abandoned,
        "top_3_breakthroughs": breakthroughs[:3],
    })


def warn_if_abandoned(query: str) -> dict[str, Any]:
    """Check if an approach was previously abandoned. Call before suggesting libraries or tools.

    Args:
        query: Library, tool, or approach name to check.
    """
    _, _, pipeline = _get_deps()

    matches = pipeline.warn_if_abandoned(query)

    if not matches:
        return {"status": "clear"}

    return {
        "status": "warning",
        "matches": matches,
    }
