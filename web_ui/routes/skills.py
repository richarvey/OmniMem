"""Compiled skills routes: list, detail, gated create, delete, and transfer.

Skills are build output — compiled from experience and graveyard memories
through the compile_skill propose-and-accept gate. The web UI offers no edit
path: a memory error is noise, a skill error is policy, so the only way a
skill's content changes is a reviewed proposal. Creation here runs the exact
same shared flow (memory/skill_compiler.py) — compile a draft, a human reviews
it in the modal, accept commits it. Deletion is allowed with confirmation;
to change a skill, update the underlying memories and recompile.

Transfer moves a skill between instances: export bundles the skill and its
source memories into a checksummed zip, import validates the bundle, previews
what would change, and only writes on an explicit confirm — strictly additive,
existing keys are never overwritten (memory/skill_transfer.py).
"""

import json
import re
import secrets
import time
from urllib.parse import quote

from starlette.requests import Request
from starlette.responses import HTMLResponse, RedirectResponse, Response
from starlette.routing import Route

from memory.skill_compiler import compile_skill_flow
from memory.skill_transfer import (
    apply_skill_import,
    build_skill_export,
    plan_skill_import,
    validate_skill_import,
)
from memory.skills import generated_skill_key, resolve_domain, validate_domain

from .. import deps

_SKILL_PREFIX = "mem:skill:"
_PROPOSAL_PREFIX = "meta:skill:proposal:"

# Validated-but-unconfirmed import bundles, keyed by a one-shot token so the
# confirm step commits exactly what was previewed.
_IMPORT_STASH_PREFIX = "meta:skill:import:"
_IMPORT_STASH_TTL = 1800
_IMPORT_TOKEN_RE = re.compile(r"^[A-Za-z0-9_\-]{8,64}$")

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
    rule_counts = {"do": 0, "watch": 0, "dont": 0, "ref": 0}
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
    content = template.render(
        request=request, current_page="skills",
        message=request.query_params.get("message"),
        error=request.query_params.get("error"),
        **data,
    )
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


def _render_compile_result(request: Request, result: dict) -> HTMLResponse:
    template = request.app.state.templates.get_template("skills/_compile_result.html")
    return HTMLResponse(template.render(request=request, result=result))


async def skill_compile(request: Request) -> HTMLResponse:
    """POST /skills/compile — propose a draft skill for a domain (htmx partial).

    Creation only: an existing skill for the domain is refused here so the
    modal can't silently become a recompile UI — recompiles carry a diff
    review and stay on the MCP flow.
    """
    form = await request.form()
    domain = (form.get("domain") or "").strip()
    if not domain:
        return _render_compile_result(request, {
            "status": "error", "reason": "Enter a domain, e.g. python.",
        })

    canonical, _ = resolve_domain(domain)
    try:
        validate_domain(canonical)
    except ValueError as exc:
        return _render_compile_result(request, {"status": "error", "reason": str(exc)})

    skill_id = generated_skill_key(canonical)
    existing = deps.store.get(skill_id)
    if existing is not None:
        return _render_compile_result(request, {
            "status": "exists",
            "skill_id": skill_id,
            "name": existing.get("name") or skill_id.rsplit(":", 1)[-1],
            "domain": canonical,
        })

    result = compile_skill_flow(deps.store, deps.embedder, canonical, mode="propose")
    return _render_compile_result(request, result)


async def skill_commit(request: Request) -> HTMLResponse:
    """POST /skills/commit — commit the reviewed proposal (htmx partial)."""
    form = await request.form()
    domain = (form.get("domain") or "").strip()
    if not domain:
        return _render_compile_result(request, {
            "status": "error", "reason": "Missing domain.",
        })

    canonical, _ = resolve_domain(domain)
    try:
        validate_domain(canonical)
    except ValueError as exc:
        return _render_compile_result(request, {"status": "error", "reason": str(exc)})

    result = compile_skill_flow(deps.store, deps.embedder, canonical, mode="write")
    if result.get("status") == "written":
        # htmx follows HX-Redirect to the freshly written skill.
        return HTMLResponse("", headers={"HX-Redirect": f"/skills/{result['skill_id']}"})
    return _render_compile_result(request, result)


async def skill_delete(request: Request):
    """POST /skills/delete — delete a compiled skill (confirmed client-side)."""
    form = await request.form()
    key = (form.get("key") or "").strip()
    if not key.startswith(_SKILL_PREFIX) or deps.store.get(key) is None:
        return HTMLResponse('<p class="empty-state">Skill not found.</p>', status_code=404)
    deps.store.delete(key)
    return RedirectResponse("/skills", status_code=303)


async def skill_export(request: Request):
    """GET /skills/export/{key} — download the skill + source memories as a zip."""
    key = request.path_params["key"]
    bundle, err = build_skill_export(deps.store, key)
    if err:
        return HTMLResponse(f'<p class="empty-state">{err}.</p>', status_code=404)
    return Response(
        bundle["data"],
        media_type="application/zip",
        headers={
            "Content-Disposition": f'attachment; filename="{bundle["filename"]}"',
        },
    )


def _render_import_result(request: Request, result: dict) -> HTMLResponse:
    template = request.app.state.templates.get_template("skills/_import_result.html")
    return HTMLResponse(template.render(request=request, result=result))


async def skill_import(request: Request) -> HTMLResponse:
    """POST /skills/import — validate an uploaded bundle and preview the plan.

    Nothing is written here: the validated bundle is stashed under a one-shot
    token and the preview partial shows exactly what confirm would add.
    """
    form = await request.form()
    upload = form.get("file")
    if upload is None or isinstance(upload, str) or not upload.filename:
        return _render_import_result(request, {
            "status": "error", "reason": "Choose a .zip bundle to upload.",
        })
    if not upload.filename.lower().endswith(".zip"):
        return _render_import_result(request, {
            "status": "error", "reason": "Only .zip bundles exported from the "
                                         "skills page are accepted.",
        })

    raw = await upload.read()
    result = validate_skill_import(raw)
    if not result["ok"]:
        return _render_import_result(request, {
            "status": "error", "reason": result["error"],
        })

    plan = plan_skill_import(deps.store, result)
    token = secrets.token_urlsafe(24)
    deps.store.client.set(
        f"{_IMPORT_STASH_PREFIX}{token}",
        json.dumps({
            "skill_key": result["skill_key"],
            "skill_fields": result["skill_fields"],
            "memories": result["memories"],
        }),
        ex=_IMPORT_STASH_TTL,
    )

    return _render_import_result(request, {
        "status": "preview",
        "token": token,
        "manifest": result["manifest"],
        "warnings": result["warnings"],
        "plan": plan,
        "nothing_to_do": (
            plan["skill_exists"] and not plan["new_memories"]
        ),
    })


async def skill_import_confirm(request: Request) -> HTMLResponse:
    """POST /skills/import/confirm — commit the previewed bundle."""
    form = await request.form()
    token = (form.get("token") or "").strip()
    if not _IMPORT_TOKEN_RE.match(token):
        return _render_import_result(request, {
            "status": "error", "reason": "Invalid import token.",
        })

    stash_key = f"{_IMPORT_STASH_PREFIX}{token}"
    raw = deps.store.client.get(stash_key)
    if raw is None:
        return _render_import_result(request, {
            "status": "error",
            "reason": "This import preview has expired — upload the bundle again.",
        })
    deps.store.client.delete(stash_key)

    try:
        bundle = json.loads(raw)
    except json.JSONDecodeError:
        return _render_import_result(request, {
            "status": "error", "reason": "Stored import bundle is unreadable — "
                                         "upload the bundle again.",
        })

    summary = apply_skill_import(deps.store, deps.embedder, bundle)
    # Imported episodic memories may carry abandoned-approach entries.
    if deps.pipeline is not None:
        deps.pipeline.invalidate_abandoned_cache()

    written = len(summary["memories_written"])
    skipped = len(summary["memories_skipped"])
    parts = []
    if summary["skill_written"]:
        parts.append("Skill imported")
    else:
        parts.append("Skill already existed (left untouched)")
    parts.append(f"{written} memor{'y' if written == 1 else 'ies'} added")
    if skipped:
        parts.append(f"{skipped} already present (skipped)")
    message = quote("; ".join(parts) + ".")
    # htmx follows HX-Redirect back to the catalogue with the outcome flash.
    return HTMLResponse("", headers={"HX-Redirect": f"/skills?message={message}"})


routes = [
    Route("/skills", skills_list),
    Route("/skills/compile", skill_compile, methods=["POST"]),
    Route("/skills/commit", skill_commit, methods=["POST"]),
    Route("/skills/delete", skill_delete, methods=["POST"]),
    Route("/skills/import", skill_import, methods=["POST"]),
    Route("/skills/import/confirm", skill_import_confirm, methods=["POST"]),
    Route("/skills/export/{key:path}", skill_export),
    Route("/skills/{key:path}", skill_detail),
]
