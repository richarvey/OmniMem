"""Feed influence: tie RSS feeds to skill domains with a weighted score.

A feed can declare, per skill domain, an influence score from 1 (barely
present) to 10 (strongly represented). When a skill for that domain is
recompiled, the latest articles from its influencing feeds are pulled into a
distinct "Feed watch" section — no per-article blessing required. The score
sets how many of the feed's most recent articles appear (influence N ⇒ up to
N articles), so a 10 feed dominates the section and a 1 feed contributes a
single headline. Feeds with no skill association keep today's behaviour:
their articles only ever reach a skill through promote_knowledge().

The source of truth is feeds.yml (a `skills:` mapping per feed). Because the
compiler runs in the MCP server and web UI containers — which don't share the
worker's feeds.yml mount — the config is mirrored into one Valkey hash:

    meta:feed:influence   field = feed name, value = JSON feed entry
                          {"url", "topics", "mode", "project",
                           "skills": {domain: score}}

The web UI mirrors on every feeds.yml write and the RSS worker re-mirrors on
each ingest cycle (rss_worker/ingester.py `_sync_feed_influence` — a
deliberate small copy, the worker image doesn't ship this package), so hand
edits to the file converge too. Reads are lenient: damaged entries are
dropped with a warning, never raised, because a broken feed mapping must not
take skill compilation down.
"""

import json
import logging
from typing import Any

from .skills import resolve_domain, validate_domain

logger = logging.getLogger(__name__)

FEED_INFLUENCE_KEY = "meta:feed:influence"

MIN_INFLUENCE = 1
MAX_INFLUENCE = 10

_MAX_FEED_NAME = 200
_MAX_SKILLS_PER_FEED = 20


def validate_feed_skills(raw: Any) -> dict[str, int]:
    """Strict validation of a feed's skills mapping ({domain: influence}).

    Used where a human supplies the mapping (web UI form, import bundles):
    domains are alias-resolved and validated, scores must be whole numbers
    within 1-10. Raises ValueError with a message fit to show the user.
    """
    if not isinstance(raw, dict):
        raise ValueError("skills must be a mapping of domain to influence score")
    if len(raw) > _MAX_SKILLS_PER_FEED:
        raise ValueError(f"skills: max {_MAX_SKILLS_PER_FEED} domains per feed")
    validated: dict[str, int] = {}
    for domain, score in raw.items():
        canonical, _ = resolve_domain(str(domain))
        validate_domain(canonical)
        try:
            value = int(str(score))
        except (TypeError, ValueError):
            raise ValueError(
                f"Influence for '{canonical}' must be a whole number "
                f"({MIN_INFLUENCE}-{MAX_INFLUENCE}), got {score!r}"
            )
        if not MIN_INFLUENCE <= value <= MAX_INFLUENCE:
            raise ValueError(
                f"Influence for '{canonical}' must be between "
                f"{MIN_INFLUENCE} and {MAX_INFLUENCE}, got {value}"
            )
        if canonical in validated:
            raise ValueError(f"Domain '{canonical}' appears more than once")
        validated[canonical] = value
    return validated


def _parse_skills_lenient(raw: Any) -> dict[str, int]:
    """Read-side skills parsing: drop invalid entries instead of raising."""
    if not isinstance(raw, dict):
        return {}
    skills: dict[str, int] = {}
    for domain, score in raw.items():
        try:
            canonical, _ = resolve_domain(str(domain))
            validate_domain(canonical)
            value = int(str(score))
        except (TypeError, ValueError):
            logger.warning("Dropping invalid feed skill entry %r: %r", domain, score)
            continue
        if not MIN_INFLUENCE <= value <= MAX_INFLUENCE:
            logger.warning("Dropping out-of-range influence %r for %r", score, domain)
            continue
        skills[canonical] = value
    return skills


def normalise_feed_entry(feed: dict[str, Any]) -> dict[str, Any] | None:
    """One feeds.yml entry into the mirrored shape, or None if unusable."""
    if not isinstance(feed, dict):
        return None
    name = str(feed.get("name") or "").strip()
    url = str(feed.get("url") or "").strip()
    if not name or not url or len(name) > _MAX_FEED_NAME:
        return None
    topics = feed.get("topics")
    entry: dict[str, Any] = {
        "url": url,
        "topics": [str(t) for t in topics if t] if isinstance(topics, list) else [],
        "skills": _parse_skills_lenient(feed.get("skills")),
    }
    if feed.get("mode"):
        entry["mode"] = str(feed["mode"])
    if feed.get("project"):
        entry["project"] = str(feed["project"])
    return entry


def sync_feed_influences(client, feeds: list[dict[str, Any]]) -> int:
    """Mirror the feeds.yml list into the meta:feed:influence hash.

    Full replace (delete + rewrite in one pipeline) so removed feeds and
    renames don't leave stale fields behind. Returns the number of feeds
    mirrored. Format must stay in step with rss_worker/ingester.py
    _sync_feed_influence, the worker-side copy of this writer.
    """
    mapping: dict[str, str] = {}
    for feed in feeds or []:
        entry = normalise_feed_entry(feed)
        if entry is None:
            continue
        name = str(feed["name"]).strip()
        mapping[name] = json.dumps(entry, ensure_ascii=False, sort_keys=True)

    pipe = client.pipeline(transaction=False)
    pipe.delete(FEED_INFLUENCE_KEY)
    if mapping:
        pipe.hset(FEED_INFLUENCE_KEY, mapping=mapping)
    pipe.execute()
    logger.info("Mirrored %d feeds into %s", len(mapping), FEED_INFLUENCE_KEY)
    return len(mapping)


def load_feed_influences(client) -> dict[str, dict[str, Any]]:
    """The mirrored feed map: {feed_name: entry}. Lenient — damage is dropped."""
    try:
        raw = client.hgetall(FEED_INFLUENCE_KEY)
    except Exception as exc:
        logger.warning("Could not read %s: %s", FEED_INFLUENCE_KEY, exc)
        return {}
    influences: dict[str, dict[str, Any]] = {}
    for name, value in (raw or {}).items():
        try:
            entry = json.loads(value)
        except (json.JSONDecodeError, TypeError):
            logger.warning("Dropping unreadable feed influence entry %r", name)
            continue
        if not isinstance(entry, dict) or not isinstance(entry.get("url"), str):
            continue
        entry["skills"] = _parse_skills_lenient(entry.get("skills"))
        topics = entry.get("topics")
        entry["topics"] = [str(t) for t in topics if t] if isinstance(topics, list) else []
        influences[str(name)] = entry
    return influences


def feeds_for_domain(
    influences: dict[str, dict[str, Any]], domain: str
) -> list[dict[str, Any]]:
    """Feeds influencing a domain, strongest first (ties by name for
    deterministic compile output)."""
    matched = [
        {"feed_name": name, "influence": entry["skills"][domain], "url": entry.get("url", "")}
        for name, entry in influences.items()
        if domain in entry.get("skills", {})
    ]
    matched.sort(key=lambda f: (-f["influence"], f["feed_name"]))
    return matched
