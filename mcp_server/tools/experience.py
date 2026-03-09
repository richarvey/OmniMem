"""Experience scoring tools: record_experience, log_abandoned, get_experience, experience_summary, warn_if_abandoned.

Experience scoring captures not just what was solved, but how hard it was, what approaches
were abandoned and why, and what finally cracked it. Hard-won knowledge surfaces more readily;
dead ends warn before they waste time again.
"""

import json
import logging
import time
from typing import Any

from ..memory.recall import compute_experience_weight

logger = logging.getLogger(__name__)

VALID_OUTCOMES = {"succeeded", "pivoted", "abandoned"}
VALID_APPROACH_TYPES = {"library", "approach", "tool", "pattern", "service"}

EFFORT_SCORE_GUIDE = """Effort score guide:
  1 — First attempt succeeded, no meaningful obstacles
  2 — Minor friction: one wrong turn, quick fix
  3 — Moderate effort: multiple iterations, some debugging
  4 — Significant struggle: hours of effort, approach changes required
  5 — Battle-hardened: near-abandonment, fundamental rethink required"""


def _get_deps():
    from ..tools import _store, _lifecycle, _pipeline
    return _store, _lifecycle, _pipeline


def record_experience(
    key: str,
    effort_score: int,
    outcome: str,
    iterations: int = 1,
    abandoned_approaches: list[dict[str, str]] | None = None,
    breakthrough: str | None = None,
    gotchas: str | None = None,
) -> dict[str, Any]:
    """Record the experience of solving (or failing to solve) a problem. Captures effort, outcome, dead ends, and breakthroughs.

    This is how the memory system learns from difficulty. High-effort successes surface more readily.
    High-effort failures auto-suppress the abandoned approach names to prevent wasting time again.

    Effort score guide:
      1 — First attempt succeeded, no meaningful obstacles
      2 — Minor friction: one wrong turn, quick fix
      3 — Moderate effort: multiple iterations, some debugging
      4 — Significant struggle: hours of effort, approach changes required
      5 — Battle-hardened: near-abandonment, fundamental rethink required

    Args:
        key: The memory key to attach experience data to.
        effort_score: 1-5 difficulty rating (see guide above).
        outcome: One of 'succeeded', 'pivoted', or 'abandoned'.
        iterations: Number of attempts/iterations (default 1).
        abandoned_approaches: List of approaches tried and abandoned, each with 'name', 'type' (library/approach/tool/pattern/service), and 'reason'.
        breakthrough: What finally worked — the key insight or solution.
        gotchas: Important caveats or things to watch out for.

    Returns:
        Dict with key, effort_score, outcome, experience_weight, any auto-suppressions, and status.
    """
    store, lifecycle, _ = _get_deps()

    if not 1 <= effort_score <= 5:
        raise ValueError(
            f"effort_score must be 1-5, got {effort_score}. {EFFORT_SCORE_GUIDE}"
        )

    if outcome not in VALID_OUTCOMES:
        raise ValueError(
            f"outcome must be one of {VALID_OUTCOMES}, got '{outcome}'"
        )

    data = store.get(key)
    if data is None:
        raise ValueError(f"Memory key not found: {key}")

    experience_weight = compute_experience_weight(effort_score, outcome)
    now = str(time.time())

    store.set_field(key, "effort_score", str(effort_score))
    store.set_field(key, "outcome", outcome)
    store.set_field(key, "iterations", str(iterations))
    store.set_field(key, "experience_weight", str(experience_weight))
    store.set_field(key, "updated_at", now)

    if abandoned_approaches:
        existing_raw = data.get("abandoned_approaches", "[]")
        try:
            existing = json.loads(existing_raw)
        except (json.JSONDecodeError, TypeError):
            existing = []
        existing.extend(abandoned_approaches)
        store.set_field(key, "abandoned_approaches", json.dumps(existing))

    if breakthrough:
        store.set_field(key, "breakthrough", breakthrough)

    if gotchas:
        store.set_field(key, "gotchas", gotchas)

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
        "status": "recorded",
    }

    if auto_suppressed:
        result["auto_suppressed"] = auto_suppressed
        result["suppression_note"] = (
            f"Auto-suppressed {len(auto_suppressed)} abandoned approach(es) "
            f"with effort score {effort_score}/5: {', '.join(auto_suppressed)}. "
            "These will no longer surface in recall results."
        )

    return result


def log_abandoned(
    key: str,
    name: str,
    type: str,
    reason: str,
) -> dict[str, Any]:
    """Append a single abandoned approach entry to an existing memory. Use this to incrementally record dead ends as they happen during a session.

    Args:
        key: The memory key to append the abandoned approach to.
        name: Name of the abandoned approach (e.g. 'onnxruntime', 'microservices').
        type: Type of approach — one of 'library', 'approach', 'tool', 'pattern', 'service'.
        reason: Why it was abandoned.

    Returns:
        Dict with key, updated abandoned count, and the latest entry.
    """
    store, _, _ = _get_deps()

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

    store.set_field(key, "abandoned_approaches", json.dumps(existing))
    store.set_field(key, "updated_at", str(time.time()))

    return {
        "key": key,
        "abandoned_count": len(existing),
        "latest_entry": new_entry,
    }


def get_experience(key: str) -> dict[str, Any]:
    """Return full experience fields for a memory, including a human-readable summary.

    Effort score guide:
      1 — First attempt succeeded, no meaningful obstacles
      2 — Minor friction: one wrong turn, quick fix
      3 — Moderate effort: multiple iterations, some debugging
      4 — Significant struggle: hours of effort, approach changes required
      5 — Battle-hardened: near-abandonment, fundamental rethink required

    Args:
        key: The memory key to look up experience data for.

    Returns:
        All experience fields with a human-readable summary, or not_found status.
    """
    store, _, _ = _get_deps()

    data = store.get(key)
    if data is None:
        return {"status": "not_found"}

    effort_raw = data.get("effort_score")
    if effort_raw is None:
        return {"status": "not_found", "message": "This memory has no experience data recorded."}

    try:
        effort = int(float(effort_raw))
    except (ValueError, TypeError):
        return {"status": "not_found", "message": "Invalid experience data."}

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

    breakthrough = data.get("breakthrough")
    gotchas = data.get("gotchas")
    experience_weight = data.get("experience_weight", "1.0")

    effort_labels = {
        1: "trivial",
        2: "minor friction",
        3: "moderate effort",
        4: "significant struggle",
        5: "battle-hardened",
    }

    summary_parts = [
        f"This took {iterations} iteration(s), effort score {effort}/5 ({effort_labels.get(effort, 'unknown')}).",
    ]

    if abandoned:
        abandoned_strs = [
            f"{a.get('name', '?')} ({a.get('reason', 'no reason given')})"
            for a in abandoned
        ]
        summary_parts.append(f"Abandoned: {', '.join(abandoned_strs)}.")

    if breakthrough:
        summary_parts.append(f"What worked: {breakthrough}.")

    if gotchas:
        summary_parts.append(f"Gotchas: {gotchas}.")

    return {
        "status": "found",
        "key": key,
        "effort_score": effort,
        "outcome": outcome,
        "iterations": iterations,
        "abandoned_approaches": abandoned,
        "breakthrough": breakthrough,
        "gotchas": gotchas,
        "experience_weight": experience_weight,
        "summary": " ".join(summary_parts),
    }


def experience_summary(project: str | None = None) -> dict[str, Any]:
    """Aggregate experience data across all episodic memories. Shows average effort, outcome breakdown, top struggles, the graveyard of abandoned approaches, and top breakthroughs.

    The graveyard is as operationally valuable as the success list — it prevents repeating painful mistakes.

    Args:
        project: Optional project name to filter by.

    Returns:
        Aggregate stats including outcome breakdown, top effortful memories, graveyard, and top breakthroughs.
    """
    store, _, _ = _get_deps()

    keys = store.scan_prefix("mem:episodic:")
    total_effort = 0
    count_with_experience = 0
    outcome_counts: dict[str, int] = {"succeeded": 0, "pivoted": 0, "abandoned": 0}
    effortful: list[dict[str, Any]] = []
    all_abandoned: list[dict[str, Any]] = []
    breakthroughs: list[dict[str, Any]] = []

    for key in keys:
        data = store.get(key)
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

        content = data.get("content", "")[:80]
        effortful.append({
            "key": key,
            "content": content,
            "effort_score": effort,
            "outcome": outcome,
            "project": data.get("project"),
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
                    "project": data.get("project"),
                })

        # Collect breakthroughs
        breakthrough = data.get("breakthrough")
        if breakthrough and outcome == "succeeded":
            breakthroughs.append({
                "key": key,
                "content": content,
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

    return {
        "memories_with_experience": count_with_experience,
        "average_effort_score": avg_effort,
        "outcome_breakdown": outcome_counts,
        "top_5_most_effortful": effortful[:5],
        "graveyard": unique_abandoned,
        "top_3_breakthroughs": breakthroughs[:3],
    }


def warn_if_abandoned(query: str) -> dict[str, Any]:
    """Check if a library, tool, or approach was previously abandoned. Call this proactively before suggesting solutions.

    If a warning comes back, tell the human before proceeding:
    'We tried [X] before and abandoned it because [reason] — shall we try again or look for alternatives?'

    Args:
        query: The library name, tool, or approach to check against the graveyard.

    Returns:
        List of matches with memory_key, abandoned_name, reason, effort_score, and project.
        Prefixed with WARNING if matches found. Returns 'clear' status if no matches.
    """
    _, _, pipeline = _get_deps()

    matches = pipeline.warn_if_abandoned(query)

    if not matches:
        return {
            "status": "clear",
            "message": "No previously abandoned approaches match this query.",
        }

    return {
        "status": "warning",
        "message": "WARNING: The following approaches were previously tried and abandoned:",
        "matches": matches,
    }
