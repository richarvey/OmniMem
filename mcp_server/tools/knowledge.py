"""Knowledge tools: query and manage knowledge namespace items."""

import json
import time
from typing import Any

from . import _compact


def _get_deps():
    from tools import _store
    return _store


def recent_knowledge(
    days: int = 7,
    feed_name: str | None = None,
    topics: list[str] | None = None,
    limit: int = 20,
) -> list[dict[str, Any]]:
    """Recent knowledge articles ingested by the RSS worker.

    Returns knowledge items created within the given lookback window,
    sorted newest first. Optionally filter by feed name or topics.

    Args:
        days: Lookback window in days (default 7, max 365).
        feed_name: Filter to a specific RSS feed name.
        topics: Filter to items tagged with at least one of these topics.
        limit: Maximum results to return (default 20, max 50).
    """
    store = _get_deps()
    now = time.time()
    days = max(1, min(days, 365))
    limit = max(1, min(limit, 50))
    cutoff = now - (days * 86400)

    keys = store.scan_prefix("mem:knowledge:")
    if not keys:
        return []

    all_data = store.get_fields_multi(
        keys,
        ("state", "created_at", "feed_name", "topics", "title", "content",
         "source_url", "published_at", "expires_at"),
    )
    results = []
    for key, data in zip(keys, all_data):
        if data is None:
            continue
        if data.get("state") != "active":
            continue
        if float(data.get("created_at", "0")) < cutoff:
            continue
        if feed_name and data.get("feed_name") != feed_name:
            continue
        if topics:
            try:
                item_topics = json.loads(data.get("topics", "[]"))
            except (json.JSONDecodeError, TypeError):
                item_topics = []
            if not any(t in item_topics for t in topics):
                continue
        results.append(_compact({
            "key": key,
            "title": data.get("title"),
            "content": data.get("content"),
            "source_url": data.get("source_url"),
            "feed_name": data.get("feed_name"),
            "published_at": data.get("published_at"),
            "created_at": data.get("created_at"),
            "expires_at": data.get("expires_at"),
            "topics": json.loads(data.get("topics", "[]")) if data.get("topics") else None,
        }))

    results.sort(key=lambda x: float(x.get("created_at") or "0"), reverse=True)
    return results[:limit]


def promote_knowledge(
    key: str,
    domain: str | None = None,
    demote: bool = False,
    rules: list[dict[str, str]] | None = None,
) -> dict[str, Any]:
    """Mark a knowledge item as permanently useful, and optionally skill-eligible for a domain.

    Without a domain: clears the expires_at field so the item is never
    auto-archived by maintenance. Use this when an RSS-ingested article turns
    out to be genuinely valuable.

    With a domain: additionally marks the article skill-eligible — the next
    compile_skill() for that domain compiles it into the skill's Reference
    section, citing the article. Promotion is the vetting step (an article
    carries no experience signal, so a human marking it eligible substitutes
    for reinforcement); the compile itself still runs the propose-and-accept
    gate. Expiry is cleared too — an article feeding a skill must not
    auto-archive underneath it.

    When the article contains discrete guidance (a "5 things to avoid" list,
    a best-practice post), read it first and pass the items as rules — each
    becomes its own stance-prefixed bullet in the Reference section instead
    of one summary line. Extraction happens here, under human review, never
    at compile time, so compilation stays deterministic. Re-promote with an
    edited list to revise; rules=[] reverts to the single summary rule.

    Args:
        key: The memory key (e.g. mem:knowledge:01ABC...).
        domain: Skill domain to make this article eligible for (e.g.
            'python'). Aliases resolve the same way as compile_skill.
        demote: With domain, remove that domain from the article's
            skill-eligibility instead of adding it.
        rules: With domain, extracted rules from the article, each
            {"kind": "do"|"watch"|"dont"|"note", "text": "..."} (max 20,
            400 chars each). Review them with the human before promoting.
    """
    from memory.skills import (
        parse_skill_domains,
        resolve_domain,
        validate_domain,
        validate_reference_rules,
    )

    store = _get_deps()

    if not key.startswith("mem:knowledge:"):
        return {"error": f"Key must be in the knowledge namespace: {key}"}
    if demote and not domain:
        return {"error": "demote requires a domain to remove"}
    if rules is not None and (not domain or demote):
        return {"error": "rules only apply when promoting to a domain"}

    data = store.get(key)
    if data is None:
        return {"error": f"Key not found: {key}"}
    if data.get("state") == "archived":
        return {"error": f"Cannot promote archived item: {key}"}

    if domain is None:
        store.set_field(key, "expires_at", "")
        return {"key": key, "promoted": True}

    canonical, _ = resolve_domain(domain)
    try:
        validate_domain(canonical)
    except ValueError as exc:
        return {"error": str(exc)}

    now = str(time.time())
    domains = parse_skill_domains(data.get("skill_domains"))
    if demote:
        if canonical not in domains:
            return {"error": f"{key} is not promoted to domain '{canonical}'"}
        domains.remove(canonical)
        store.set_fields(key, {
            "skill_domains": json.dumps(domains),
            "updated_at": now,
        })
        return _compact({
            "key": key,
            "demoted_from": canonical,
            "skill_domains": domains,
            "note": f"Recompile with compile_skill(domain='{canonical}') to "
                    "drop its Reference rule from the skill.",
        })

    validated_rules: list[dict[str, str]] | None = None
    if rules is not None:
        try:
            validated_rules = validate_reference_rules(rules)
        except ValueError as exc:
            return {"error": str(exc)}

    already = canonical in domains
    fields: dict[str, Any] = {}
    if not already:
        domains.append(canonical)
        domains.sort()
        fields.update({
            "skill_domains": json.dumps(domains),
            "promoted_at": now,
            "expires_at": "",
        })
    if validated_rules is not None:
        fields["skill_rules"] = json.dumps(validated_rules)
    if fields:
        fields["updated_at"] = now
        store.set_fields(key, fields)

    result: dict[str, Any] = {
        "key": key,
        "promoted": True,
        "skill_domains": domains,
        "note": ("Already promoted to this domain." if already and not fields else
                 f"Compiles into the '{canonical}' skill's Reference section "
                 f"at the next compile_skill(domain='{canonical}') — the "
                 "propose-and-accept gate still applies."),
    }
    if validated_rules is not None:
        result["reference_rules"] = validated_rules
        if not validated_rules:
            result["note"] = ("Extracted rules cleared — the article reverts "
                              "to a single summary Reference rule at the next "
                              "compile.")
    return _compact(result)
