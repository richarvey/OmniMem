"""Auto skill scan: proposes skills from cross-project lesson patterns.

Runs opportunistically inside briefing() on a time gate (the MCP server has
no background scheduler), looking for two things:

1. Domains with no compiled skill whose episodic pool already carries rules
   that would clear the reinforcement gate — by default only when a rule
   spans more than one project, since a lesson that recurs across projects
   is the strongest signal it deserves to become policy.
2. Existing skills whose pending changes (as detected by the briefing's
   update-diff surface) are worth turning into a concrete draft.

Everything it produces is a proposal stash — exactly what a human running
compile_skill(mode="propose") would create — so the propose-and-accept gate
is untouched: nothing here ever writes a skill. Noise control is a per-domain
"seen" marker holding the sha of the last auto-proposed body; a draft the
human let expire is not re-proposed until the compiled output actually
changes, so ignoring a proposal is a valid way to decline it.
"""

import logging
import os
import time
from typing import Any, Iterable

from .skill_compiler import compile_skill_flow, proposal_key
from .skills import (
    GENERATED_SKILL_PREFIX,
    body_sha,
    build_rules,
    extract_lessons,
    gather_domain_pools,
    known_domains,
    lesson_bearing,
    skill_user,
    strip_volatile,
    validate_domain,
)

logger = logging.getLogger(__name__)

_LAST_RUN_KEY = "meta:skill_scan:last_run"
_SEEN_PREFIX = "meta:skill_scan:seen:"
_PROPOSAL_PREFIX = "meta:skill:proposal:"

# New-skill discovery runs the full extract/cluster pipeline per candidate,
# so the candidate list is capped (largest pools first) to bound the work.
_MAX_CANDIDATES = 10


def scan_due(store) -> bool:
    """True when the time gate has elapsed. SKILL_SCAN_INTERVAL_HOURS=0 disables."""
    try:
        hours = float(os.getenv("SKILL_SCAN_INTERVAL_HOURS", "24"))
    except ValueError:
        hours = 24.0
    if hours <= 0:
        return False
    raw = store.client.get(_LAST_RUN_KEY)
    if raw:
        try:
            if time.time() - float(raw) < hours * 3600:
                return False
        except (TypeError, ValueError):
            pass
    return True


def _pending_proposal_domains(store) -> set[str]:
    """Domains with a live proposal stash — human or auto, hands off either way."""
    keys = store.scan_prefix(_PROPOSAL_PREFIX)
    if not keys:
        return set()
    rows = store.get_fields_multi(keys, ("domain",))
    return {row["domain"] for row in rows if row and row.get("domain")}


def _existing_skill_domains(store) -> set[str]:
    keys = store.scan_prefix(GENERATED_SKILL_PREFIX)
    if not keys:
        return set()
    rows = store.get_fields_multi(keys, ("domain",))
    return {row["domain"] for row in rows if row and row.get("domain")}


def _propose(
    store, embedder, domain: str, user: str, new_skill: bool,
) -> dict[str, Any] | None:
    """Run the shared propose flow and apply the seen-sha noise gate.

    Returns a briefing entry, or None when the domain didn't produce a
    proposal or produced the same draft the human already ignored.
    """
    result = compile_skill_flow(store, embedder, domain, mode="propose")
    if result.get("status") != "proposal":
        return None

    stash = store.get(proposal_key(domain, user))
    body = (stash or {}).get("body", "")
    sha = body_sha(strip_volatile(body))
    seen_key = f"{_SEEN_PREFIX}{domain}-{user}"
    if store.client.get(seen_key) == sha:
        # Same draft as last time and it was left to expire — declining by
        # ignoring is valid, so withdraw the stash and stay quiet until the
        # compiled output actually changes.
        store.delete(proposal_key(domain, user))
        return None
    store.client.set(seen_key, sha)

    entry: dict[str, Any] = {
        "domain": domain,
        "skill_id": result.get("skill_id", ""),
        "new_skill": new_skill,
        "review": f"compile_skill(domain='{domain}', mode='propose') to see the "
                  f"{'draft' if new_skill else 'diff'}, mode='write' to accept",
    }
    if result.get("rules"):
        entry["rules"] = result["rules"]
    if not new_skill and result.get("changes"):
        entry["changes"] = result["changes"]
    return entry


def run_skill_scan(
    store, embedder, update_domains: Iterable[str] = (),
) -> dict[str, Any]:
    """One scan pass: propose new skills, then drafts for changed skills.

    update_domains: domains of existing skills the caller already knows have
    pending changes (the briefing passes its pending_skill_updates result in,
    so change detection isn't run twice).
    """
    now = time.time()
    # Stamp first so a failing scan waits out the interval instead of
    # retrying on every briefing.
    store.client.set(_LAST_RUN_KEY, str(now))

    max_proposals = max(0, int(os.getenv("SKILL_SCAN_MAX_PROPOSALS", "3")))
    min_pool = max(1, int(os.getenv("SKILL_SCAN_MIN_POOL", "3")))
    cross_project = (
        os.getenv("SKILL_SCAN_CROSS_PROJECT", "true").strip().lower()
        not in ("false", "0", "no")
    )

    user = skill_user()
    existing = _existing_skill_domains(store)
    pending = _pending_proposal_domains(store)
    proposals: list[dict[str, Any]] = []
    candidates_checked = 0

    # Phase 1: domains with no skill yet, largest pools first.
    counts = known_domains(store)
    candidates: list[str] = []
    for domain in sorted(counts, key=lambda d: (-counts[d], d)):
        if domain in existing or domain in pending:
            continue
        try:
            validate_domain(domain)
        except ValueError:
            continue
        candidates.append(domain)
        if len(candidates) >= _MAX_CANDIDATES:
            break

    pools = gather_domain_pools(store, candidates) if candidates else {}
    for domain in candidates:
        if len(proposals) >= max_proposals:
            break
        pool = pools.get(domain, [])
        if sum(1 for m in pool if lesson_bearing(m)) < min_pool:
            continue
        candidates_checked += 1
        eligible, _ = build_rules(embedder, extract_lessons(pool))
        if not eligible:
            continue
        if cross_project and not any(len(r.projects) >= 2 for r in eligible):
            continue
        entry = _propose(store, embedder, domain, user, new_skill=True)
        if entry:
            proposals.append(entry)

    # Phase 2: drafts for existing skills the briefing flagged as changed.
    for domain in update_domains:
        if len(proposals) >= max_proposals:
            break
        if domain in pending or domain not in existing:
            continue
        entry = _propose(store, embedder, domain, user, new_skill=False)
        if entry:
            proposals.append(entry)

    if proposals:
        logger.info(
            "Skill scan proposed %d draft(s): %s",
            len(proposals), ", ".join(p["domain"] for p in proposals),
        )
    return {
        "ran_at": str(now),
        "proposals": proposals,
        "new_skill_candidates_checked": candidates_checked,
    }
