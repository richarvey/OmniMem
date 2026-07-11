"""Skill compiler MCP tools: compile_skill, find_skills, get_skill, bless.

Two write paths, two bars: experience and graveyard writes flow freely
(raw, dilutable, noise-tolerant memory), but nothing writes to a skill
silently — compile_skill(mode="propose") surfaces a diff, a human accepts
it, and compile_skill(mode="write") commits exactly that accepted draft.
The gate sits only at compile-to-skill.
"""

import json
import logging
import os
import time
from typing import Any

from memory.skill_compiler import compile_skill_flow
from memory.skills import (
    GENERATED_SKILL_PREFIX,
    SKILL_KEY_PREFIX,
    gather_domain_pools,
    generated_skill_key,
    lesson_bearing,
    normalise_domain,
    resolve_domain,
)

from . import _compact

logger = logging.getLogger(__name__)


def _get_deps():
    from tools import _store, _embedder
    return _store, _embedder


def compile_skill(
    domain: str,
    mode: str = "propose",
    min_reinforcement: int = 2,
    include_graveyard: bool = True,
    export_path: str | None = None,
    description: str | None = None,
) -> dict[str, Any]:
    """Compile domain procedure from experience and graveyard memories into a loadable SKILL.md. 'propose' (default) returns a reviewable diff; 'write' commits only a previously proposed and accepted diff — there is no silent-commit path.

    The compiled skill is build output: derived from raw memories, never
    hand-edited. To change domain guidance, update the underlying memories
    (record_experience, log_abandoned, bless) and recompile.

    Args:
        domain: Free-form domain tag, e.g. 'python', 'rust', 'technical-blogging'.
        mode: 'propose' returns a diff (or full draft if the skill is new) and
            stashes it; 'write' commits the stashed proposal after human review.
        min_reinforcement: Lessons must recur across this many memories to
            become rules (default 2). bless() promotes a single strong lesson
            past the gate.
        include_graveyard: Compile abandoned approaches into Don't rules
            (default True).
        export_path: On write, also mirror the SKILL.md to this relative path
            under SKILL_EXPORT_DIR. Valkey remains the canonical store.
        description: Explicitly set the skill description (the load trigger).
            The description is human-owned: the compiler drafts one at
            creation, and recompiles keep the stored one unless this is passed.
    """
    store, embedder = _get_deps()
    return compile_skill_flow(
        store, embedder, domain,
        mode=mode,
        min_reinforcement=min_reinforcement,
        include_graveyard=include_graveyard,
        export_path=export_path,
        description=description,
    )



def find_skills(query_or_domain: str) -> dict[str, Any]:
    """Discover compiled skills: ranked skill IDs and descriptions for a query or domain. Load the winner intact with get_skill().

    Args:
        query_or_domain: A domain tag ('python') or a free-text description
            of the work at hand.
    """
    store, embedder = _get_deps()

    if not query_or_domain or not query_or_domain.strip():
        raise ValueError("query_or_domain cannot be empty")

    canonical, _ = resolve_domain(query_or_domain)

    all_keys = store.scan_prefix(SKILL_KEY_PREFIX)
    if not all_keys:
        return {
            "skills": [],
            "note": "No skills stored yet. compile_skill(domain=...) creates one.",
        }
    meta_rows = store.get_fields_multi(
        all_keys,
        ("name", "description", "domain", "state", "generated", "compiled_at"),
    )

    def _entry(key: str, row: dict[str, Any], score: float, match: str) -> dict[str, Any]:
        return _compact({
            "skill_id": key,
            "name": row.get("name", ""),
            "description": row.get("description", ""),
            "domain": row.get("domain", ""),
            "generated": row.get("generated") == "true",
            "score": round(score, 4),
            "match": match,
        })

    results: list[dict[str, Any]] = []
    seen: set[str] = set()

    # Exact domain matches lead. Authored before generated within the tie:
    # when both fire on the same work, intent outranks inference.
    domain_hits = [
        (key, row) for key, row in zip(all_keys, meta_rows)
        if row and row.get("state", "active") == "active"
        and row.get("domain") == canonical
    ]
    domain_hits.sort(key=lambda kr: (kr[1].get("generated") == "true", kr[0]))
    for key, row in domain_hits:
        results.append(_entry(key, row, 1.0, "domain"))
        seen.add(key)

    # Semantic pass over discovery metadata for everything else.
    query_vector = embedder.embed(query_or_domain)
    docs = store.search(
        "skill", query_vector, top_k=10, filter_expr="(@state:{active})",
    )
    semantic: list[tuple[float, bool, str, dict[str, Any]]] = []
    for doc in docs:
        key = doc.get("key", "")
        if key in seen or doc.get("state", "active") != "active":
            continue
        similarity = max(0.0, 1.0 - float(doc.get("similarity_score", "1.0")))
        semantic.append((similarity, doc.get("generated") == "true", key, doc))
    semantic.sort(key=lambda t: (-t[0], t[1], t[2]))
    for similarity, _, key, doc in semantic:
        results.append(_entry(key, doc, similarity, "semantic"))

    return _compact({
        "skills": results[:10],
        "note": None if results else (
            "No skill matched. Known skills exist for other domains — "
            "call find_skills with a broader query or compile_skill to create one."
        ),
    })


def get_skill(skill_id: str) -> dict[str, Any]:
    """Load a skill whole: the complete SKILL.md body with frontmatter and structure intact, by ID, name, or domain.

    Args:
        skill_id: Full key ('mem:skill:gen:python-ric'), name
            ('python-ric'), or bare domain ('python').
    """
    store, _ = _get_deps()

    if not skill_id or not skill_id.strip():
        raise ValueError("skill_id cannot be empty")
    skill_id = skill_id.strip()

    candidates: list[str]
    if skill_id.startswith(SKILL_KEY_PREFIX):
        candidates = [skill_id]
    else:
        short = normalise_domain(skill_id)
        candidates = [
            f"{SKILL_KEY_PREFIX}{short}",
            f"{GENERATED_SKILL_PREFIX}{short}",
            generated_skill_key(resolve_domain(short)[0]),
        ]

    for key in candidates:
        data = store.get(key)
        if data is None or not data.get("body"):
            continue

        # Same telemetry counters as memory recall — /telemetry and /metrics
        # then show skill load frequency for free.
        try:
            pipe = store.client.pipeline(transaction=False)
            pipe.hincrby(key, "recall_count", 1)
            pipe.hset(key, "last_recalled", str(time.time()))
            pipe.execute()
        except Exception as exc:
            logger.warning("Failed to bump skill counters for %s: %s", key, exc)

        manifest: list[str] = []
        try:
            manifest = json.loads(data.get("source_manifest", "[]"))
        except (json.JSONDecodeError, TypeError):
            pass

        return _compact({
            "status": "found",
            "skill_id": key,
            "name": data.get("name", ""),
            "description": data.get("description", ""),
            "domain": data.get("domain", ""),
            "generated": data.get("generated") == "true",
            "contract_version": data.get("contract_version"),
            "compiled_at": data.get("compiled_at"),
            "state": data.get("state", "active"),
            "source_manifest": manifest,
            "body": data.get("body", ""),
        })

    available = []
    all_keys = store.scan_prefix(SKILL_KEY_PREFIX)
    if all_keys:
        rows = store.get_fields_multi(all_keys, ("name", "domain"))
        available = [
            {"skill_id": k, "name": (r or {}).get("name", ""),
             "domain": (r or {}).get("domain", "")}
            for k, r in zip(all_keys, rows)
        ][:10]
    return _compact({
        "status": "not_found",
        "tried": candidates,
        "available": available,
    })


def bless(memory_key: str) -> dict[str, Any]:
    """Promote a single strong lesson to skill-eligible now, bypassing the reinforcement threshold at the next compile_skill().

    The human-accept gate at compile time is still the safety net — bless
    only pre-qualifies the lesson, it does not write to any skill.

    Args:
        memory_key: Episodic memory key carrying the lesson
            (breakthrough, gotchas, or graveyard entry).
    """
    store, _ = _get_deps()

    if not memory_key.startswith("mem:episodic:"):
        raise ValueError(
            "bless() takes an episodic memory key (mem:episodic:...) — "
            "skills compile from the episodic experience pool."
        )

    data = store.get(memory_key)
    if data is None:
        return {"status": "not_found", "key": memory_key}

    already = data.get("blessed") == "1"
    if not already:
        store.set_fields(memory_key, {
            "blessed": "1",
            "blessed_at": str(time.time()),
        })
        logger.info("Blessed memory %s as skill-eligible", memory_key)

    tags = []
    try:
        tags = [t.lower() for t in json.loads(data.get("tags", "[]")) if t]
    except (json.JSONDecodeError, TypeError):
        pass

    return _compact({
        "status": "already_blessed" if already else "blessed",
        "key": memory_key,
        "domains": tags,
        "note": "Its lessons now clear the reinforcement gate on the next "
                "compile_skill() for its tagged domains. Compiling still "
                "requires the propose-and-accept flow.",
    })


# ---------------------------------------------------------------------------
# Briefing surfaces (imported by tools.briefing)
# ---------------------------------------------------------------------------


def suggest_skills_for_briefing(
    store, embedder, context_text: str | None, top_k: int = 3
) -> list[dict[str, Any]]:
    """Skill recommendations for the briefing. Suggests, never loads.

    With project context (ongoing project): semantic top-k over discovery
    metadata, shown below the context. Without it (greenfield): every active
    skill, because the skill is the only thing carrying the user's
    conventions there and the agent should pick by description.
    """
    min_similarity = float(os.getenv("SKILL_SUGGEST_MIN_SIMILARITY", "0.30"))

    keys = store.scan_prefix(SKILL_KEY_PREFIX)
    if not keys:
        return []
    rows = store.get_fields_multi(
        keys, ("name", "description", "domain", "state", "generated"),
    )
    active = {
        key: row for key, row in zip(keys, rows)
        if row and row.get("state", "active") == "active"
    }
    if not active:
        return []

    def _suggestion(key: str, row: dict[str, Any], similarity: float | None) -> dict[str, Any]:
        return _compact({
            "skill_id": key,
            "name": row.get("name", ""),
            "description": row.get("description", ""),
            "domain": row.get("domain", ""),
            "similarity": round(similarity, 4) if similarity is not None else None,
            "load_with": f"get_skill('{key}')",
        })

    if not context_text:
        # Greenfield: no context to lead with, so the full catalogue moves to
        # the top and the description (the load trigger) does the choosing.
        entries = sorted(active.items(), key=lambda kv: kv[1].get("name", ""))
        return [_suggestion(k, r, None) for k, r in entries[:10]]

    docs = store.search(
        "skill", embedder.embed(context_text), top_k=max(10, top_k),
        filter_expr="(@state:{active})",
    )
    scored: list[tuple[float, str, dict[str, Any]]] = []
    for doc in docs:
        key = doc.get("key", "")
        row = active.get(key)
        if row is None:
            continue
        similarity = max(0.0, 1.0 - float(doc.get("similarity_score", "1.0")))
        if similarity < min_similarity:
            continue
        scored.append((similarity, key, row))
    scored.sort(key=lambda t: (-t[0], t[1]))
    return [_suggestion(key, row, sim) for sim, key, row in scored[:top_k]]


def pending_skill_updates(store) -> list[dict[str, Any]]:
    """Per-skill gists of source-memory changes since last compile.

    Prominence scales with risk: new lesson-bearing memories are low-risk
    additions (batch-accept eligible); an updated or removed manifest source
    can rewrite or delete an existing rule, so those are flagged high and
    named. The full diff is always one call away via compile_skill(propose).
    """
    skill_keys = [
        k for k in store.scan_prefix(GENERATED_SKILL_PREFIX)
    ]
    if not skill_keys:
        return []

    skill_rows = store.get_fields_multi(
        skill_keys,
        ("name", "domain", "state", "compiled_at", "source_manifest", "rule_manifest"),
    )
    skills = [
        (key, row) for key, row in zip(skill_keys, skill_rows)
        if row and row.get("state", "active") == "active" and row.get("domain")
    ]
    if not skills:
        return []

    # One episodic scan covers new-lesson detection for every skill.
    pools = gather_domain_pools(store, {row["domain"] for _, row in skills})

    updates: list[dict[str, Any]] = []
    for key, row in skills:
        domain = row["domain"]
        compiled_at = float(row.get("compiled_at") or 0.0)

        try:
            manifest = json.loads(row.get("source_manifest", "[]"))
        except (json.JSONDecodeError, TypeError):
            manifest = []
        try:
            rule_manifest = json.loads(row.get("rule_manifest", "[]"))
        except (json.JSONDecodeError, TypeError):
            rule_manifest = []

        def _rules_fed_by(source_key: str) -> list[str]:
            gists = []
            for rule in rule_manifest:
                if source_key in rule.get("sources", []):
                    text = (f"Avoid {rule['name']}"
                            if rule.get("kind") == "dont" and rule.get("name")
                            else rule.get("text", ""))
                    gists.append(text[:60])
            return gists

        changes: list[dict[str, Any]] = []

        if manifest:
            source_rows = store.get_fields_multi(manifest, ("updated_at", "state"))
            for source_key, source in zip(manifest, source_rows):
                if source is None or source.get("state") in ("archived", "deleted"):
                    changes.append(_compact({
                        "change": "source_removed",
                        "risk": "high",
                        "source": source_key,
                        "feeds_rules": _rules_fed_by(source_key),
                        "gist": "source memory gone — its rules may be removed",
                    }))
                elif float(source.get("updated_at") or 0.0) > compiled_at:
                    changes.append(_compact({
                        "change": "source_updated",
                        "risk": "high",
                        "source": source_key,
                        "feeds_rules": _rules_fed_by(source_key),
                        "gist": "source memory updated — its rules may be rewritten",
                    }))

        manifest_set = set(manifest)
        fresh = [
            mem for mem in pools.get(domain, [])
            if mem["key"] not in manifest_set
            and mem["created_at"] > compiled_at
            and lesson_bearing(mem)
        ]
        for mem in fresh[:3]:
            changes.append({
                "change": "new_source",
                "risk": "low",
                "source": mem["key"],
                "gist": mem.get("content", "")[:60],
            })
        if len(fresh) > 3:
            changes.append({
                "change": "new_source",
                "risk": "low",
                "gist": f"+{len(fresh) - 3} more new lesson-bearing memories",
            })

        if changes:
            updates.append({
                "skill_id": key,
                "name": row.get("name", ""),
                "domain": domain,
                "changes": changes,
                "batch_accept_eligible": all(c["risk"] == "low" for c in changes),
                "full_diff": f"compile_skill(domain='{domain}', mode='propose')",
            })

    return updates
