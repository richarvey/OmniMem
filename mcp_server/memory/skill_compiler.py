"""The gated compile-to-skill flow, shared by the MCP server and the web UI.

Two write paths, two bars: experience and graveyard writes flow freely
(raw, dilutable, noise-tolerant memory), but nothing writes to a skill
silently — a propose surfaces a diff, a human accepts it, and a write
commits exactly that accepted draft. The gate sits only at compile-to-skill.

This lives in the shared memory package so both entry points (the
compile_skill MCP tool and the web UI's create-skill flow) run the exact
same propose-and-accept logic instead of drifting copies.
"""

import json
import logging
import os
import re
import time
from pathlib import Path
from typing import Any

from .skills import (
    CONTRACT_VERSION,
    Rule,
    body_sha,
    bodies_equivalent,
    build_reference_rules,
    build_rules,
    discovery_text,
    draft_description,
    extract_lessons,
    gather_domain_pool,
    gather_promoted_knowledge,
    generated_skill_key,
    known_domains,
    normalise_domain,
    render_skill_md,
    render_unified_diff,
    resolve_domain,
    skill_user,
    suggest_similar_domain,
    summarise_rule_changes,
    validate_domain,
)

logger = logging.getLogger(__name__)

# Segments of an export path: no separators, no leading dots, so a mirrored
# SKILL.md can't escape the export directory. Mirrors backup.py's policy.
_SAFE_EXPORT_SEGMENT_RE = re.compile(r"^[a-zA-Z0-9_][a-zA-Z0-9_.\-]*$")

_MAX_SKILL_BODY = 100_000


def _compact(d: dict[str, Any]) -> dict[str, Any]:
    """Strip None and empty values from response dict to reduce token usage."""
    out: dict[str, Any] = {}
    for k, v in d.items():
        if v is None:
            continue
        if isinstance(v, str) and not v:
            continue
        if isinstance(v, list) and not v:
            continue
        if isinstance(v, dict) and not v:
            continue
        out[k] = v
    return out


def proposal_key(domain: str, user: str) -> str:
    return f"meta:skill:proposal:{domain}-{user}"


def proposal_ttl() -> int:
    return int(os.getenv("SKILL_PROPOSAL_TTL_SECONDS", "86400"))


def _export_dir() -> Path:
    return Path(os.getenv("SKILL_EXPORT_DIR", "/app/backups/skills"))


def safe_export_path(export_path: str) -> tuple[Path | None, str | None]:
    """Resolve export_path inside SKILL_EXPORT_DIR, or return an error."""
    if not export_path or not export_path.strip():
        return None, "export_path cannot be empty"
    export_path = export_path.strip()
    if os.path.isabs(export_path) or export_path.startswith("~"):
        return None, (
            "export_path must be relative — it is written inside "
            f"SKILL_EXPORT_DIR ({_export_dir()})"
        )
    segments = [s for s in export_path.replace("\\", "/").split("/") if s]
    if not segments:
        return None, "export_path cannot be empty"
    for segment in segments:
        if not _SAFE_EXPORT_SEGMENT_RE.match(segment):
            return None, (
                "Invalid export_path segment. Use alphanumerics, underscores, "
                "hyphens, and dots (no leading dot); e.g. 'python-ric/SKILL.md'"
            )
    if not segments[-1].endswith(".md"):
        return None, "export_path must end in .md"

    root = _export_dir().resolve()
    filepath = (root / Path(*segments)).resolve()
    try:
        filepath.relative_to(root)
    except ValueError:
        return None, "export_path must not escape the export directory"
    return filepath, None


def _rule_counts(rules: list[Rule]) -> dict[str, int]:
    counts: dict[str, int] = {}
    for rule in rules:
        counts[rule.kind] = counts.get(rule.kind, 0) + 1
    return counts


def _held_back_preview(held_back: list[Rule], limit: int = 10) -> list[dict[str, Any]]:
    preview = []
    for rule in held_back[:limit]:
        preview.append(_compact({
            "kind": rule.kind,
            "rule": (f"Avoid {rule.name}" if rule.kind == "dont" and rule.name
                     else rule.text[:80]),
            "reinforcement": rule.reinforcement,
            "sources": rule.sources,
        }))
    return preview


def compile_skill_flow(
    store,
    embedder,
    domain: str,
    mode: str = "propose",
    min_reinforcement: int = 2,
    include_graveyard: bool = True,
    export_path: str | None = None,
    description: str | None = None,
) -> dict[str, Any]:
    """Run the compile_skill propose/write flow against the given store.

    See tools.skills.compile_skill for the human-facing contract; this is
    the same flow with explicit dependencies.
    """
    if mode not in ("propose", "write"):
        raise ValueError(f"mode must be 'propose' or 'write', got '{mode}'")
    min_reinforcement = max(1, min(int(min_reinforcement), 10))

    canonical, aliased = resolve_domain(domain)
    validate_domain(canonical)
    user = skill_user()
    skill_id = generated_skill_key(canonical, user)

    existing = store.get(skill_id)
    if existing is not None and existing.get("generated") != "true":
        # Namespacing should make this impossible; refuse anyway. Intent
        # outranks inference — the compiler never writes over authored work.
        return {
            "status": "refused",
            "skill_id": skill_id,
            "reason": "Existing object is not flagged generated:true — "
                      "the compiler only overwrites its own output.",
        }

    if mode == "write":
        return _commit_proposal(
            store, embedder, canonical, user, skill_id, existing, export_path,
        )

    return _propose(
        store, embedder, canonical, user, skill_id, existing,
        min_reinforcement=min_reinforcement,
        include_graveyard=include_graveyard,
        description_override=description,
        aliased_from=normalise_domain(domain) if aliased else None,
    )


def _propose(
    store,
    embedder,
    domain: str,
    user: str,
    skill_id: str,
    existing: dict[str, Any] | None,
    *,
    min_reinforcement: int,
    include_graveyard: bool,
    description_override: str | None,
    aliased_from: str | None,
) -> dict[str, Any]:
    pool = gather_domain_pool(store, domain)
    promoted = gather_promoted_knowledge(store, [domain])[domain]
    ref_rules = build_reference_rules(promoted)

    if not pool and not ref_rules:
        domains = known_domains(store)
        suggestion = suggest_similar_domain(embedder, domain, domains.keys())
        top_domains = sorted(domains.items(), key=lambda kv: -kv[1])[:10]
        return _compact({
            "status": "no_candidates",
            "domain": domain,
            "did_you_mean": (
                {"domain": suggestion[0], "similarity": suggestion[1]}
                if suggestion else None
            ),
            "known_domains": [
                {"domain": d, "memories": n} for d, n in top_domains
            ],
            "note": "No active episodic memories are tagged with this domain "
                    "and no knowledge is promoted to it. Domains are tags — "
                    "tag memories at remember() time, or promote_knowledge("
                    "key, domain=...) to feed reference material in.",
        })

    lessons = extract_lessons(pool, include_graveyard=include_graveyard)
    if not lessons and not ref_rules:
        return {
            "status": "no_lessons",
            "domain": domain,
            "pool_size": len(pool),
            "note": "Memories exist for this domain but none carry lessons yet. "
                    "record_experience() breakthroughs/gotchas, log_abandoned() "
                    "dead ends, or bless() a memory to make it skill-eligible.",
        }

    eligible, held_back = build_rules(embedder, lessons, min_reinforcement)
    if not eligible and not ref_rules:
        return _compact({
            "status": "insufficient_reinforcement",
            "domain": domain,
            "pool_size": len(pool),
            "min_reinforcement": min_reinforcement,
            "held_back": _held_back_preview(held_back),
            "note": "No lesson recurs across enough memories to earn a rule. "
                    "A single episode is a memory; a pattern earns a skill "
                    "rule. Lower min_reinforcement or bless() a strong lesson.",
        })
    # Promoted references join after the gate: promotion is the vetting, so
    # they neither need reinforcement nor consume it.
    eligible = eligible + ref_rules

    # Description: human-owned and pinned. An explicit override is the
    # strongest ownership signal; otherwise the stored one survives
    # recompiles, and the compiler only drafts for a brand-new skill.
    existing_description = (existing or {}).get("description", "")
    if description_override is not None and description_override.strip():
        skill_description = description_override.strip()
    elif existing_description:
        skill_description = existing_description
    else:
        skill_description = draft_description(domain, user)
    description_pinned = bool(existing_description) and description_override is None

    now = time.time()
    body = render_skill_md(
        domain=domain,
        user=user,
        description=skill_description,
        rules=eligible,
        compiled_at=now,
        min_reinforcement=min_reinforcement,
    )
    if len(body) > _MAX_SKILL_BODY:
        return {
            "status": "error",
            "reason": f"Compiled body too large ({len(body)} chars, "
                      f"max {_MAX_SKILL_BODY}).",
        }

    existing_body = (existing or {}).get("body", "")
    if existing_body and bodies_equivalent(existing_body, body):
        return {
            "status": "unchanged",
            "skill_id": skill_id,
            "domain": domain,
            "note": "Compiled output matches the stored skill — nothing to propose.",
        }

    new_manifest = [r.to_dict() for r in eligible]
    old_manifest: list[dict[str, Any]] = []
    if existing:
        try:
            old_manifest = json.loads(existing.get("rule_manifest", "[]"))
        except (json.JSONDecodeError, TypeError):
            old_manifest = []
    changes = summarise_rule_changes(old_manifest, new_manifest)

    # Stash the proposal so write commits exactly what was reviewed, even if
    # source memories move underneath in the meantime.
    proposal = {
        "body": body,
        "description": skill_description,
        "domain": domain,
        "user": user,
        "based_on": body_sha(existing_body) if existing_body else "",
        "created_at": str(now),
        "min_reinforcement": str(min_reinforcement),
        "rule_manifest": json.dumps(new_manifest),
        "source_manifest": json.dumps(
            sorted({s for r in eligible for s in r.sources})
        ),
    }
    ttl = proposal_ttl()
    pipe = store.client.pipeline(transaction=False)
    pipe.hset(proposal_key(domain, user), mapping=proposal)
    pipe.expire(proposal_key(domain, user), ttl)
    pipe.execute()

    is_new = existing is None
    result: dict[str, Any] = {
        "status": "proposal",
        "skill_id": skill_id,
        "domain": domain,
        "new_skill": is_new,
        "description": skill_description,
        "description_pinned": description_pinned,
        "rules": _rule_counts(eligible),
        "changes": changes,
        "note": "Review, then commit with "
                f"compile_skill(domain='{domain}', mode='write'). "
                f"Proposal expires in {ttl}s.",
    }
    if aliased_from:
        result["domain_resolved_from"] = aliased_from
    if is_new:
        result["draft"] = body
    else:
        result["diff"] = render_unified_diff(existing_body, body, skill_id)
    if held_back:
        result["held_back"] = _held_back_preview(held_back)
    return _compact(result)


def _commit_proposal(
    store,
    embedder,
    domain: str,
    user: str,
    skill_id: str,
    existing: dict[str, Any] | None,
    export_path: str | None,
) -> dict[str, Any]:
    stash = store.get(proposal_key(domain, user))
    if stash is None or not stash.get("body"):
        return {
            "status": "no_proposal",
            "domain": domain,
            "note": "Nothing proposed for this domain (or the proposal "
                    "expired). Run compile_skill(mode='propose') and review "
                    "the diff first — write only commits an accepted proposal.",
        }

    existing_body = (existing or {}).get("body", "")
    current_sha = body_sha(existing_body) if existing_body else ""
    if stash.get("based_on", "") != current_sha:
        return {
            "status": "stale_proposal",
            "skill_id": skill_id,
            "note": "The stored skill changed after this diff was proposed. "
                    "Re-run compile_skill(mode='propose') and review again.",
        }

    now = str(time.time())
    body = stash["body"]
    description = stash.get("description", "")
    name = f"{domain}-{user}"

    fields: dict[str, Any] = {
        "name": name,
        "description": description,
        "domain": domain,
        "user": user,
        "body": body,
        "generated": "true",
        "state": "active",
        "surface_score": "1.0",
        "contract_version": str(CONTRACT_VERSION),
        "compiled_at": stash.get("created_at", now),
        "created_at": (existing or {}).get("created_at") or now,
        "updated_at": now,
        "tags": json.dumps([domain]),
        "source_manifest": stash.get("source_manifest", "[]"),
        "rule_manifest": stash.get("rule_manifest", "[]"),
    }

    vector = embedder.embed(discovery_text(name, description, domain))
    store.upsert("skill", skill_id, fields, vector)
    store.delete(proposal_key(domain, user))
    logger.info("Committed skill %s (%d chars)", skill_id, len(body))

    result: dict[str, Any] = {
        "status": "written",
        "skill_id": skill_id,
        "domain": domain,
        "description": description,
        "new_skill": existing is None,
    }

    if export_path:
        filepath, err = safe_export_path(export_path)
        if err:
            result["export_error"] = err
        else:
            try:
                filepath.parent.mkdir(parents=True, exist_ok=True)
                filepath.write_text(body, encoding="utf-8")
                result["exported_to"] = str(filepath)
            except OSError as exc:
                logger.error("Skill export failed: %s", exc)
                result["export_error"] = "Failed to write export file"

    return _compact(result)
