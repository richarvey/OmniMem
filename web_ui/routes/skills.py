"""Compiled skills routes: list and detail views (read-only).

Skills are build output — compiled from experience and graveyard memories
through the MCP compile_skill propose-and-accept gate. The web UI deliberately
offers no write path (not even delete): a memory error is noise, a skill error
is policy, so the only way a skill changes is a reviewed proposal. To change
one, update the underlying memories and recompile.
"""

import json
import time

from starlette.requests import Request
from starlette.responses import HTMLResponse
from starlette.routing import Route

from .. import deps

_SKILL_PREFIX = "mem:skill:"
_PROPOSAL_PREFIX = "meta:skill:proposal:"

# Discovery metadata only — the body is fetched by the detail view alone.
_LIST_FIELDS = (
    "name", "description", "domain", "user", "state", "generated",
    "compiled_at", "contract_version", "recall_count", "last_recalled",
    "created_at", "updated_at",
)


def _fmt_ts(raw, fmt: str = "%Y-%m-%d %H:%M") -> str:
    try:
        return time.strftime(fmt, time.localtime(float(raw)))
    except (ValueError, TypeError):
        return "—"


def _parse_json_list(raw) -> list:
    try:
        parsed = json.loads(raw) if raw else []
    except (json.JSONDecodeError, TypeError):
        return []
    return parsed if isinstance(parsed, list) else []


def gather_skills(store) -> dict:
    """Skill catalogue plus pending compile proposals, shaped for the list view."""
    keys = store.scan_prefix(_SKILL_PREFIX)
    rows = store.get_fields_multi(keys, _LIST_FIELDS) if keys else []

    skills = []
    states = {"active": 0, "deprioritised": 0, "archived": 0}
    for key, row in zip(keys, rows):
        row = row or {}
        state = row.get("state", "active")
        if state in states:
            states[state] += 1
        skills.append({
            "key": key,
            "name": row.get("name") or key.rsplit(":", 1)[-1],
            "description": row.get("description", ""),
            "domain": row.get("domain", ""),
            "state": state,
            "generated": row.get("generated") == "true",
            "compiled_at": _fmt_ts(row.get("compiled_at")),
            "recall_count": int(row.get("recall_count") or 0),
        })
    skills.sort(key=lambda s: s["name"].lower())

    # Proposals stashed by compile_skill(mode="propose") and not yet committed.
    # They live under a TTL, so anything visible here is still committable.
    proposals = []
    proposal_keys = store.scan_prefix(_PROPOSAL_PREFIX)
    if proposal_keys:
        proposal_rows = store.get_fields_multi(
            proposal_keys, ("domain", "user", "created_at")
        )
        for pkey, prow in zip(proposal_keys, proposal_rows):
            prow = prow or {}
            proposals.append({
                "key": pkey,
                "domain": prow.get("domain") or pkey.rsplit(":", 1)[-1],
                "created_at": _fmt_ts(prow.get("created_at")),
            })
    proposals.sort(key=lambda p: p["domain"])

    return {
        "skills": skills,
        "states": states,
        "proposals": proposals,
        "total": len(skills),
    }


def gather_skill(store, key: str) -> dict | None:
    """Full skill record for the detail view, or None if it doesn't exist."""
    if not key.startswith(_SKILL_PREFIX):
        return None
    data = store.get(key)
    if data is None:
        return None

    rules = [r for r in _parse_json_list(data.get("rule_manifest")) if isinstance(r, dict)]
    rule_counts = {"do": 0, "watch": 0, "dont": 0}
    for rule in rules:
        kind = rule.get("kind")
        if kind in rule_counts:
            rule_counts[kind] += 1

    return {
        "key": key,
        "name": data.get("name") or key.rsplit(":", 1)[-1],
        "description": data.get("description", ""),
        "domain": data.get("domain", ""),
        "user": data.get("user", ""),
        "state": data.get("state", "active"),
        "generated": data.get("generated") == "true",
        "contract_version": data.get("contract_version", ""),
        "compiled_at": _fmt_ts(data.get("compiled_at"), "%Y-%m-%d %H:%M:%S"),
        "created_at": _fmt_ts(data.get("created_at"), "%Y-%m-%d %H:%M:%S"),
        "updated_at": _fmt_ts(data.get("updated_at"), "%Y-%m-%d %H:%M:%S"),
        "recall_count": int(data.get("recall_count") or 0),
        "last_recalled": (
            _fmt_ts(data.get("last_recalled"), "%Y-%m-%d %H:%M:%S")
            if data.get("last_recalled") else "Never"
        ),
        "body": data.get("body", ""),
        "sources": [s for s in _parse_json_list(data.get("source_manifest")) if isinstance(s, str)],
        "rules": rules,
        "rule_counts": rule_counts,
    }


async def skills_list(request: Request) -> HTMLResponse:
    """GET /skills — compiled skill catalogue with pending proposals."""
    data = gather_skills(deps.store)
    template = request.app.state.templates.get_template("skills/list.html")
    content = template.render(request=request, current_page="skills", **data)
    return HTMLResponse(content)


async def skill_detail(request: Request) -> HTMLResponse:
    """GET /skills/{key} — full skill: metadata, rules, SKILL.md body, provenance."""
    key = request.path_params["key"]
    skill = gather_skill(deps.store, key)

    if skill is None:
        return HTMLResponse('<p class="empty-state">Skill not found.</p>', status_code=404)

    template = request.app.state.templates.get_template("skills/detail.html")
    content = template.render(request=request, skill=skill, current_page="skills")
    return HTMLResponse(content)


routes = [
    Route("/skills", skills_list),
    Route("/skills/{key:path}", skill_detail),
]
