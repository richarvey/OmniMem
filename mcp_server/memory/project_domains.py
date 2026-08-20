"""Project work-type domains: the routing layer between projects and skills.

A project declares which *kinds* of work live inside it — python, docker,
css, technical-writing — so that "I'm coding Python, what have I learned the
hard way?" can be answered across every project instead of one at a time.

The vocabulary is deliberately the SAME one compiled skills use. Domains here
go through skills.resolve_domain / validate_domain, so `py` on a project and
`py` on a skill both land on `python` and the two systems can never drift into
parallel taxonomies. That shared vocabulary is the whole point of the feature:
project domains route you to a candidate set of projects, and the skill
namespace holds the distilled lessons for the same names.

Two deliberate choices worth knowing about:

* **Storage is comma-separated, not JSON.** Every other list-ish field in this
  codebase (`tags`, `topics`, `skill_domains`) is stored as a JSON array, and
  as a consequence `@tags:{python}` matches nothing — a TAG field splits its
  value on commas, so `["python","docker"]` indexes as the tokens `["python"`
  and `"docker"]`. Nothing in the codebase filters on those fields, which is
  why it has never bitten. `domains` is meant to be filtered, so it is stored
  in the format the TAG index actually reads. Validation guarantees a domain
  can never itself contain a comma.
* **Resolution scans, it doesn't search.** Domain to project-name resolution
  reads the `mem:project:` keys directly rather than pushing `@domains:{...}`
  into FT.SEARCH. The project namespace holds one key per project plus a
  handful of stray ULID memories, so the scan is cheap, and it keeps
  correctness off valkey-search tag semantics — which this project's graveyard
  shows diverge from the documented ones in more than one direction.
"""

from __future__ import annotations

import json
import logging
import os
import re
import time
from typing import Any, Iterable, Sequence

from .skills import DOMAIN_ALIASES, resolve_domain, validate_domain

logger = logging.getLogger(__name__)

PROJECT_KEY_PREFIX = "mem:project:"

# A project carries a handful of domains, not a folksonomy. The cap is here so
# a bad import or a runaway compile can't turn one project into a filter that
# matches everything.
MAX_PROJECT_DOMAINS = 20

# Cheap split for free-text sources (the `stack` string, mostly): commas,
# slashes, pipes, plus signs, ampersands and newlines all mean "next item", and
# so does a spaced-out "and" — "Python and Docker" is how people actually write
# a stack. Requiring whitespace on both sides keeps a domain like `and-then`
# intact, and a stored domain can never contain a space anyway (validation
# limits them to [a-z0-9._-]), so this can only ever fire on free text.
_SOURCE_SPLIT_RE = re.compile(r"[,/|+&\n;]+|\s+and\s+")

# Words that survive splitting a stack string but say nothing about the kind
# of work in the project.
_STACK_STOPWORDS = frozenset({
    "and", "or", "the", "a", "an", "with", "using", "etc", "etc.", "plus",
    "via", "for", "on", "in", "of", "some", "various", "misc", "other",
    "others", "custom", "cli", "app", "apps", "stack", "based",
})


class DomainResolution:
    """Outcome of turning a set of domains into a set of project names.

    `matched` maps each domain that names at least one project to those
    project names; `unmatched` lists the domains no project declares. The
    caller needs both: filtering on the matched set alone would silently
    present a global search as a domain-scoped one.
    """

    __slots__ = ("requested", "matched", "unmatched", "aliased")

    def __init__(
        self,
        requested: list[str],
        matched: dict[str, list[str]],
        unmatched: list[str],
        aliased: dict[str, str],
    ) -> None:
        self.requested = requested
        self.matched = matched
        self.unmatched = unmatched
        self.aliased = aliased

    @property
    def projects(self) -> list[str]:
        """Every project named by any matched domain, deduplicated and sorted."""
        seen: set[str] = set()
        for names in self.matched.values():
            seen.update(names)
        return sorted(seen)

    @property
    def fully_unmatched(self) -> bool:
        """True when no requested domain names any project at all."""
        return bool(self.requested) and not self.matched

    def as_dict(self) -> dict[str, Any]:
        out: dict[str, Any] = {"domains": self.requested}
        if self.matched:
            out["projects"] = self.projects
        if self.unmatched:
            out["unmatched_domains"] = self.unmatched
        if self.aliased:
            out["resolved_aliases"] = self.aliased
        return out


def parse_domains(raw: Any) -> list[str]:
    """Read a stored or submitted domain list in any of the shapes it arrives in.

    Canonical storage is comma-separated, but a JSON array is accepted so a
    hand-edited record, a restored backup written by an older version, or an
    imported bundle still loads.
    """
    if raw is None:
        return []
    if isinstance(raw, (list, tuple, set)):
        values: Iterable[Any] = raw
    elif isinstance(raw, str):
        text = raw.strip()
        if not text:
            return []
        if text.startswith("["):
            try:
                decoded = json.loads(text)
                values = decoded if isinstance(decoded, list) else [text]
            except (json.JSONDecodeError, TypeError):
                values = _SOURCE_SPLIT_RE.split(text)
        else:
            values = _SOURCE_SPLIT_RE.split(text)
    else:
        return []

    out: list[str] = []
    seen: set[str] = set()
    for value in values:
        if not isinstance(value, str):
            continue
        item = value.strip()
        if not item or item in seen:
            continue
        seen.add(item)
        out.append(item)
    return out


def normalise_domains(raw: Any) -> tuple[list[str], dict[str, str], list[str]]:
    """Canonicalise a domain list against the shared skill vocabulary.

    Returns (domains, aliased, rejected): the canonical list in first-seen
    order, a map of original to canonical for anything the alias table
    rewrote, and the inputs that could not be a domain at all.
    """
    domains: list[str] = []
    aliased: dict[str, str] = {}
    rejected: list[str] = []
    seen: set[str] = set()

    for item in parse_domains(raw):
        canonical, was_aliased = resolve_domain(item)
        try:
            validate_domain(canonical)
        except ValueError:
            rejected.append(item)
            continue
        if was_aliased:
            aliased[item] = canonical
        if canonical in seen:
            continue
        seen.add(canonical)
        domains.append(canonical)
        if len(domains) >= MAX_PROJECT_DOMAINS:
            break

    return domains, aliased, rejected


def serialise_domains(domains: Sequence[str]) -> str:
    """Storage form for the `domains` hash field and its TAG index."""
    return ",".join(domains)


def read_project_domains(row: dict[str, Any] | None) -> list[str]:
    """Domains declared on one already-fetched project record."""
    if not row:
        return []
    domains, _, _ = normalise_domains(row.get("domains"))
    return domains


def project_name_from(key: str, row: dict[str, Any] | None) -> str:
    """Resolve a project record's name the way every other view does.

    Prefer `project_name`, fall back to `project` (ULID-keyed memories written
    before the startup migration backfills), then the key suffix.
    """
    row = row or {}
    return (
        row.get("project_name")
        or row.get("project")
        or key.split(":")[-1]
    )


# --- domain -> project resolution -------------------------------------------

# Recall consults this on every domain-filtered call, and the underlying scan
# touches every project key. Projects change rarely, so a short TTL cache
# mirrors the abandoned-approach cache in recall.py: writers in this process
# invalidate explicitly, other processes are covered by the TTL.
_cache: dict[str, list[str]] | None = None
_cache_at: float = 0.0


def invalidate_domain_cache() -> None:
    """Drop the cached domain map after a project write."""
    global _cache
    _cache = None


def domain_map(store, use_cache: bool = True) -> dict[str, list[str]]:
    """{domain: [project names]} across every stored project context."""
    global _cache, _cache_at

    try:
        ttl = float(os.getenv("PROJECT_DOMAIN_CACHE_TTL_SECONDS", "60"))
    except ValueError:
        ttl = 60.0

    now = time.time()
    if use_cache and ttl > 0 and _cache is not None and now - _cache_at < ttl:
        return _cache

    mapping: dict[str, list[str]] = {}
    keys = store.scan_prefix(PROJECT_KEY_PREFIX)
    if keys:
        rows = store.get_fields_multi(
            keys, ("project_name", "project", "domains", "state")
        )
        for key, row in zip(keys, rows):
            if not row:
                continue
            # An archived project shouldn't route a search to itself.
            if row.get("state") in ("archived", "deleted"):
                continue
            domains = read_project_domains(row)
            if not domains:
                continue
            name = project_name_from(key, row)
            for domain in domains:
                bucket = mapping.setdefault(domain, [])
                if name not in bucket:
                    bucket.append(name)

    for bucket in mapping.values():
        bucket.sort(key=str.lower)

    _cache = mapping
    _cache_at = now
    return mapping


def resolve_projects_for_domains(store, domains: Any) -> DomainResolution:
    """Turn requested domains into the projects that declare them."""
    requested, aliased, rejected = normalise_domains(domains)
    if rejected:
        logger.debug("Ignoring unusable domain filter values: %s", rejected)

    mapping = domain_map(store)
    matched: dict[str, list[str]] = {}
    unmatched: list[str] = []
    for domain in requested:
        names = mapping.get(domain)
        if names:
            matched[domain] = list(names)
        else:
            unmatched.append(domain)

    return DomainResolution(requested, matched, unmatched, aliased)


def known_project_domains(store) -> dict[str, int]:
    """{domain: project count} — the vocabulary already in use on projects."""
    return {d: len(names) for d, names in domain_map(store).items()}


# --- suggestion --------------------------------------------------------------

_SUGGEST_FIELDS = ("project", "project_name", "tags", "state")

# Only suggest a domain from an episodic tag once it has actually recurred;
# a single memory tagged `bugfix` is not evidence the project is about it.
_MIN_TAG_OCCURRENCES = 2

_MAX_SUGGEST_SCAN_KEYS = 5000


def suggest_domains_for_project(
    store, project_name: str, limit: int = 10
) -> dict[str, Any]:
    """Propose domains for a project from its stack string and episodic tags.

    Two evidence sources, both already written by hand at some point, so this
    is a re-reading of existing signal rather than an inference:

    * the project's `stack` field, split on the usual separators
    * tags on the project's active episodic memories, once a tag has recurred

    Returns the suggestion plus the evidence behind each one, so the caller
    can show its working before anything is written. Nothing is stored here.
    """
    key = f"{PROJECT_KEY_PREFIX}{project_name}"
    row = store.get(key)

    existing = read_project_domains(row)
    stack_raw = (row or {}).get("stack") or ""

    evidence: dict[str, list[str]] = {}
    order: list[str] = []

    def _add(domain: str, why: str) -> None:
        if domain not in evidence:
            evidence[domain] = []
            order.append(domain)
        if why not in evidence[domain]:
            evidence[domain].append(why)

    # Source 1: the stack string.
    for item in parse_domains(stack_raw):
        if item.strip().lower() in _STACK_STOPWORDS:
            continue
        canonical, _ = resolve_domain(item)
        try:
            validate_domain(canonical)
        except ValueError:
            continue
        _add(canonical, "stack")

    # Source 2: recurring tags on this project's active episodic memories.
    tag_counts: dict[str, int] = {}
    keys = store.scan_prefix("mem:episodic:")
    if keys:
        if len(keys) > _MAX_SUGGEST_SCAN_KEYS:
            logger.warning(
                "Domain suggestion scan capped at %d keys (total: %d)",
                _MAX_SUGGEST_SCAN_KEYS, len(keys),
            )
            keys = keys[:_MAX_SUGGEST_SCAN_KEYS]
        rows = store.get_fields_multi(keys, _SUGGEST_FIELDS)
        for episodic_row in rows:
            if not episodic_row:
                continue
            if episodic_row.get("state") not in ("active", None):
                continue
            doc_project = (
                episodic_row.get("project") or episodic_row.get("project_name")
            )
            if doc_project != project_name:
                continue
            for tag in _parse_tag_field(episodic_row.get("tags")):
                canonical, _ = resolve_domain(tag)
                try:
                    validate_domain(canonical)
                except ValueError:
                    continue
                tag_counts[canonical] = tag_counts.get(canonical, 0) + 1

    for domain, count in sorted(
        tag_counts.items(), key=lambda kv: (-kv[1], kv[0])
    ):
        if count >= _MIN_TAG_OCCURRENCES:
            _add(domain, f"tagged on {count} memories")

    suggested = [d for d in order if d not in existing][:limit]
    merged, _, _ = normalise_domains(list(existing) + suggested)

    return {
        "project_name": project_name,
        "existing_domains": existing,
        "suggested_domains": suggested,
        "merged_domains": merged,
        "evidence": {d: evidence[d] for d in suggested},
    }


def _parse_tag_field(raw: Any) -> list[str]:
    """Tags as stored on episodic memories: a JSON array, sometimes a string."""
    if not raw:
        return []
    if isinstance(raw, list):
        return [str(t) for t in raw if t]
    try:
        decoded = json.loads(raw)
    except (json.JSONDecodeError, TypeError):
        pass
    else:
        # Decoding succeeded, so this is structured data. A list is a tag
        # list; anything else (an object, a number) is not, and splitting its
        # source text on commas would only manufacture nonsense tags.
        return [str(t) for t in decoded if t] if isinstance(decoded, list) else []
    if isinstance(raw, str):
        return [t.strip() for t in raw.split(",") if t.strip()]
    return []


def alias_hint(domain: str) -> str | None:
    """The canonical form an alias resolves to, for UI hints. None if not an alias."""
    return DOMAIN_ALIASES.get(domain.strip().lower())
