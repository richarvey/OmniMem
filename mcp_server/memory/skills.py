"""Skill compiler: distils domain procedure into loadable SKILL.md documents.

The raw memories (experience, graveyard, promoted knowledge) are the source
of truth; a compiled skill is build output, like a binary. Compilation is deterministic — the same
source memories always render the same body (bar the compiled_at stamp) — so
propose-mode diffs show real changes, not rendering noise, and the
accept-to-write gate stays reviewable. No LLM in the loop: rule text is lifted
from the memories themselves (breakthroughs, gotchas, graveyard reasons) with
their keys cited.

A memory error is noise (ranked and diluted by recall); a skill error is
policy (the agent obeys it). Everything here exists to keep bad lessons from
becoming policy silently: reinforcement gating (a pattern across episodes, not
a one-off), the bless override for single strong lessons, and a propose/accept
write path implemented in tools/skills.py.
"""

import difflib
import hashlib
import json
import logging
import os
import re
import time
from dataclasses import dataclass, field
from typing import Any, Iterable

import numpy as np

logger = logging.getLogger(__name__)

# Version of the fixed operating-contract block below. Bump when the contract
# text changes; recompiles then propose the new block like any other diff.
CONTRACT_VERSION = 1

# The operating contract is what makes the flywheel turn, so it travels with
# every skill: work under the skill keeps feeding the experience pool, and a
# richer pool compiles a better skill next time. It is fixed boilerplate,
# identical across all skills — never per-skill compiled content.
OPERATING_CONTRACT = """\
## Operating contract  (fixed, applies to every OmniMem skill)

While working under this skill, keep the data pool alive:

- Check OmniMem preferences first and honour them.
- Read relevant experience before acting (recall / get_experience), and
  warn_if_abandoned before retrying anything that looks like a known dead end.
- Record experience as you go (record_experience): effort, outcome, dead ends
  (log_abandoned into the graveyard), and breakthroughs.
- Remember durable new facts (remember).

This block is fixed boilerplate, identical across all skills, inserted from one
template and versioned by contract_version. It is not per-skill compiled content.
"""

GENERATED_BANNER = """\
> GENERATED. Stored in and served from OmniMem (Valkey), domain tag: {domain}.
> Do not edit by hand. To change the domain guidance, update the underlying
> memories and recompile with omnimem:compile_skill. Hand edits are overwritten.
"""

# Generated skills live in their own gen: namespace so a compiled skill and a
# hand-authored one are different objects that cannot collide by construction.
GENERATED_SKILL_PREFIX = "mem:skill:gen:"
SKILL_KEY_PREFIX = "mem:skill:"

# Domains are normalised to kebab-case before they touch keys or tag filters.
_SAFE_DOMAIN_RE = re.compile(r"^[a-z0-9][a-z0-9._\-]{0,63}$")

# Small alias map for obvious synonyms; the embedding "did you mean" guard in
# suggest_similar_domain() catches near-misses the map doesn't list. Without
# either, `python` vs `py` vs `python3` silently scatters lessons so none
# reaches the reinforcement threshold and no skill ever compiles.
DOMAIN_ALIASES = {
    "py": "python",
    "python3": "python",
    "js": "javascript",
    "ts": "typescript",
    "golang": "go",
    "rs": "rust",
    "k8s": "kubernetes",
    "postgres": "postgresql",
}

_POOL_FIELDS = (
    "content", "state", "project", "tags", "effort_score", "outcome",
    "breakthrough", "gotchas", "abandoned_approaches", "blessed",
    "created_at", "updated_at",
)

_KNOWLEDGE_POOL_FIELDS = (
    "content", "title", "state", "skill_domains", "feed_name",
    "source_url", "created_at", "updated_at", "promoted_at",
)

_MAX_POOL_KEYS = 5000


def skill_user() -> str:
    """Identity segment for generated skill keys (mem:skill:gen:{domain}-{user}).

    Single-node in v6, so this is just a stable label from OMNIMEM_USER
    (default "local") — org scoping and auth are v7 territory.
    """
    raw = os.getenv("OMNIMEM_USER", "local").strip() or "local"
    user = normalise_domain(raw)
    return user if _SAFE_DOMAIN_RE.match(user) else "local"


def normalise_domain(domain: str) -> str:
    """Lowercase, trim, and collapse whitespace to hyphens."""
    return re.sub(r"\s+", "-", domain.strip().lower())


def resolve_domain(domain: str) -> tuple[str, bool]:
    """Normalise and apply the alias map. Returns (canonical, was_aliased)."""
    normalised = normalise_domain(domain)
    canonical = DOMAIN_ALIASES.get(normalised, normalised)
    return canonical, canonical != normalised


def validate_domain(domain: str) -> None:
    """Raise if a normalised domain can't be used in keys and tag filters."""
    if not domain or not _SAFE_DOMAIN_RE.match(domain):
        raise ValueError(
            "Invalid domain. Use 1-64 characters: lowercase letters, digits, "
            "hyphens, underscores, or dots (e.g. 'python', 'technical-blogging')."
        )


def generated_skill_key(domain: str, user: str | None = None) -> str:
    """Canonical Valkey key for a generated skill."""
    return f"{GENERATED_SKILL_PREFIX}{domain}-{user or skill_user()}"


def discovery_text(name: str, description: str, domain: str) -> str:
    """Text embedded for skill discovery (find_skills / briefing suggestions).

    Deliberately metadata-only: the description is the auto-load trigger, so
    it — not the body — is what relevance search runs over.
    """
    return f"{name}. {description} Domain: {domain}."


def _safe_float(raw: Any, default: float = 0.0) -> float:
    try:
        return float(raw)
    except (TypeError, ValueError):
        return default


def _parse_tags(raw: Any) -> list[str]:
    if not raw:
        return []
    try:
        tags = json.loads(raw)
        if isinstance(tags, list):
            return [str(t) for t in tags if t]
    except (json.JSONDecodeError, TypeError):
        if isinstance(raw, str):
            return [t.strip() for t in raw.split(",") if t.strip()]
    return []


def _one_line(text: str, limit: int = 400) -> str:
    """Collapse whitespace so a lesson renders as a single markdown bullet."""
    collapsed = re.sub(r"\s+", " ", text).strip()
    if len(collapsed) > limit:
        collapsed = collapsed[: limit - 1].rstrip() + "…"
    return collapsed


def gather_domain_pool(store, domain: str) -> list[dict[str, Any]]:
    """Active episodic memories tagged with the domain — the compiler's input.

    Domains are tags (settled decision: free-form with a did-you-mean guard),
    so membership is a case-insensitive tag match. Only the raw pool is
    returned; reinforcement gating happens in build_rules().
    """
    return gather_domain_pools(store, [domain])[domain]


def gather_domain_pools(
    store, domains: Iterable[str]
) -> dict[str, list[dict[str, Any]]]:
    """gather_domain_pool for several domains in a single episodic scan.

    The briefing's update-diff detection checks every compiled skill at once;
    one scan for N domains instead of N scans. A memory tagged with more than
    one requested domain appears in each pool.
    """
    pools: dict[str, list[dict[str, Any]]] = {d: [] for d in domains}
    if not pools:
        return pools

    keys = store.scan_prefix("mem:episodic:")
    if not keys:
        return pools
    if len(keys) > _MAX_POOL_KEYS:
        logger.warning(
            "Skill pool scan capped at %d keys (total: %d)",
            _MAX_POOL_KEYS, len(keys),
        )
        keys = keys[:_MAX_POOL_KEYS]

    rows = store.get_fields_multi(keys, _POOL_FIELDS)
    for key, row in zip(keys, rows):
        if row is None:
            continue
        if row.get("state") not in ("active", None):
            continue
        tags = [t.lower() for t in _parse_tags(row.get("tags"))]
        matched = [d for d in pools if d in tags]
        if not matched:
            continue

        abandoned_raw = row.get("abandoned_approaches", "[]")
        try:
            abandoned = json.loads(abandoned_raw)
        except (json.JSONDecodeError, TypeError):
            abandoned = []
        if not isinstance(abandoned, list):
            abandoned = []

        effort = None
        if row.get("effort_score") is not None:
            try:
                effort = int(float(row["effort_score"]))
            except (TypeError, ValueError):
                effort = None

        entry = {
            "key": key,
            "content": row.get("content", ""),
            "project": row.get("project"),
            "tags": tags,
            "effort_score": effort,
            "outcome": row.get("outcome"),
            "breakthrough": row.get("breakthrough"),
            "gotchas": row.get("gotchas"),
            "abandoned": [a for a in abandoned if isinstance(a, dict)],
            "blessed": row.get("blessed") == "1",
            "created_at": _safe_float(row.get("created_at")),
            "updated_at": _safe_float(row.get("updated_at")),
        }
        for d in matched:
            pools[d].append(entry)

    for pool in pools.values():
        pool.sort(key=lambda m: m["key"])  # ULIDs sort chronologically
    return pools


def parse_skill_domains(raw: Any) -> list[str]:
    """Domains a knowledge item has been promoted to (skill_domains field)."""
    return [d.lower() for d in _parse_tags(raw)]


def gather_promoted_knowledge(
    store, domains: Iterable[str]
) -> dict[str, list[dict[str, Any]]]:
    """Active knowledge items promoted to each domain — the compiler's
    reference input.

    Promotion (promote_knowledge with a domain) is the vetting step: unlike
    experience, an article carries no outcome or effort signal, so a human
    marking it skill-eligible is what substitutes for reinforcement. Only
    promoted items ever reach a skill; ordinary knowledge stays lookup-only.
    """
    pools: dict[str, list[dict[str, Any]]] = {d: [] for d in domains}
    if not pools:
        return pools

    keys = store.scan_prefix("mem:knowledge:")
    if not keys:
        return pools
    if len(keys) > _MAX_POOL_KEYS:
        logger.warning(
            "Promoted-knowledge scan capped at %d keys (total: %d)",
            _MAX_POOL_KEYS, len(keys),
        )
        keys = keys[:_MAX_POOL_KEYS]

    rows = store.get_fields_multi(keys, _KNOWLEDGE_POOL_FIELDS)
    for key, row in zip(keys, rows):
        if row is None:
            continue
        if row.get("state") not in ("active", None):
            continue
        promoted_to = parse_skill_domains(row.get("skill_domains"))
        matched = [d for d in pools if d in promoted_to]
        if not matched:
            continue

        entry = {
            "key": key,
            "content": row.get("content", ""),
            "title": row.get("title", ""),
            "feed_name": row.get("feed_name", ""),
            "source_url": row.get("source_url", ""),
            "created_at": _safe_float(row.get("created_at")),
            "updated_at": _safe_float(row.get("updated_at")),
            "promoted_at": _safe_float(row.get("promoted_at")),
        }
        for d in matched:
            pools[d].append(entry)

    for pool in pools.values():
        pool.sort(key=lambda m: m["key"])  # ULIDs sort chronologically
    return pools


def lesson_bearing(mem: dict[str, Any]) -> bool:
    """Would extract_lessons() get anything out of this pool entry?

    Used by update-diff detection to count only memories that can actually
    change a compiled skill.
    """
    if mem.get("blessed"):
        return bool(
            mem.get("breakthrough") or mem.get("gotchas")
            or mem.get("abandoned") or mem.get("content")
        )
    if mem.get("breakthrough") and mem.get("outcome") == "succeeded":
        return True
    return bool(mem.get("gotchas") or mem.get("abandoned"))


@dataclass
class Lesson:
    """One extractable unit of procedure from a single memory."""

    kind: str            # "do" | "watch" | "dont"
    text: str
    source_key: str
    source_updated_at: float
    blessed: bool = False
    project: str | None = None
    name: str | None = None      # dont only: the abandoned approach name
    approach_type: str = ""      # dont only
    reason: str = ""             # dont only


@dataclass
class Rule:
    """A lesson pattern that cleared the promotion gate."""

    kind: str                    # "do" | "watch" | "dont" | "ref"
    text: str
    sources: list[str]           # distinct memory keys, chronological
    reinforcement: int           # distinct source memories backing this rule
    blessed: bool = False
    name: str | None = None      # dont only
    approach_type: str = ""      # dont only
    projects: list[str] = field(default_factory=list)
    url: str = ""                # ref only: the article's source URL

    @property
    def primary_source(self) -> str:
        return self.sources[-1] if self.sources else ""

    def to_dict(self) -> dict[str, Any]:
        d: dict[str, Any] = {
            "kind": self.kind,
            "text": self.text,
            "sources": self.sources,
            "reinforcement": self.reinforcement,
        }
        if self.blessed:
            d["blessed"] = True
        if self.name:
            d["name"] = self.name
        if self.url:
            d["url"] = self.url
        return d


def extract_lessons(
    pool: list[dict[str, Any]], include_graveyard: bool = True
) -> list[Lesson]:
    """Pull lessons out of pool memories.

    Breakthroughs of succeeded work become do-lessons, gotchas become
    watch-lessons, graveyard entries become dont-lessons. A blessed memory
    always contributes: its breakthrough regardless of outcome, or its
    content if it has no structured lesson fields.
    """
    lessons: list[Lesson] = []
    for mem in pool:
        contributed = False
        blessed = mem["blessed"]

        breakthrough = mem.get("breakthrough")
        if breakthrough and (mem.get("outcome") == "succeeded" or blessed):
            lessons.append(Lesson(
                kind="do",
                text=_one_line(breakthrough),
                source_key=mem["key"],
                source_updated_at=mem["updated_at"],
                blessed=blessed,
                project=mem.get("project"),
            ))
            contributed = True

        gotchas = mem.get("gotchas")
        if gotchas:
            lessons.append(Lesson(
                kind="watch",
                text=_one_line(gotchas),
                source_key=mem["key"],
                source_updated_at=mem["updated_at"],
                blessed=blessed,
                project=mem.get("project"),
            ))
            contributed = True

        if include_graveyard:
            for approach in mem["abandoned"]:
                name = approach.get("name")
                if not name:
                    continue
                lessons.append(Lesson(
                    kind="dont",
                    text=_one_line(approach.get("reason", "")),
                    source_key=mem["key"],
                    source_updated_at=mem["updated_at"],
                    blessed=blessed,
                    project=mem.get("project"),
                    name=name,
                    approach_type=approach.get("type", ""),
                    reason=_one_line(approach.get("reason", "")),
                ))
                contributed = True

        if blessed and not contributed and mem.get("content"):
            lessons.append(Lesson(
                kind="do",
                text=_one_line(mem["content"]),
                source_key=mem["key"],
                source_updated_at=mem["updated_at"],
                blessed=True,
                project=mem.get("project"),
            ))

    return lessons


def _cluster_indices(vectors: list[np.ndarray], threshold: float) -> list[list[int]]:
    """Union-find clustering over normalised vectors (same shape as dedup)."""
    n = len(vectors)
    if n == 0:
        return []
    if n == 1:
        return [[0]]

    matrix = np.array(vectors) @ np.array(vectors).T
    parent = list(range(n))

    def find(x: int) -> int:
        while parent[x] != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x

    def union(a: int, b: int) -> None:
        ra, rb = find(a), find(b)
        if ra != rb:
            parent[ra] = rb

    iu, ju = np.triu_indices(n, k=1)
    over = matrix[iu, ju] >= threshold
    for i, j in zip(iu[over].tolist(), ju[over].tolist()):
        union(i, j)

    clusters: dict[int, list[int]] = {}
    for i in range(n):
        clusters.setdefault(find(i), []).append(i)
    # Deterministic order: by smallest member index
    return sorted(clusters.values(), key=lambda idxs: min(idxs))


def _rule_from_cluster(kind: str, members: list[Lesson]) -> Rule:
    """Fold a lesson cluster into one rule.

    The most recently updated source supplies the wording (newest phrasing
    wins); reinforcement counts distinct source memories, so two lessons from
    one memory reinforce nothing.
    """
    representative = max(members, key=lambda l: (l.source_updated_at, l.source_key))
    sources = sorted({l.source_key for l in members})
    projects = sorted({l.project for l in members if l.project})
    return Rule(
        kind=kind,
        text=representative.text,
        sources=sources,
        reinforcement=len(sources),
        blessed=any(l.blessed for l in members),
        name=representative.name,
        approach_type=representative.approach_type,
        projects=projects,
    )


def build_rules(
    embedder,
    lessons: list[Lesson],
    min_reinforcement: int = 2,
) -> tuple[list[Rule], list[Rule]]:
    """Cluster lessons into rules and apply the promotion gate.

    A single episode is a memory; a pattern across episodes earns a skill
    rule. Do/watch lessons cluster by embedding similarity
    (SKILL_CLUSTER_THRESHOLD, default 0.80 — looser than dedup's 0.92 because
    "same lesson, different episode" is phrased differently each time).
    Graveyard lessons group by approach name, which is already the identity
    warn_if_abandoned matches on. Blessing bypasses the threshold for every
    rule the blessed memory backs.

    Returns (eligible, held_back) so callers can report what missed the gate.
    """
    threshold = float(os.getenv("SKILL_CLUSTER_THRESHOLD", "0.80"))

    rules: list[Rule] = []
    for kind in ("do", "watch"):
        kind_lessons = [l for l in lessons if l.kind == kind]
        if not kind_lessons:
            continue
        vectors = embedder.embed_batch([l.text for l in kind_lessons])
        for cluster in _cluster_indices(vectors, threshold):
            rules.append(_rule_from_cluster(kind, [kind_lessons[i] for i in cluster]))

    dont_groups: dict[str, list[Lesson]] = {}
    for lesson in lessons:
        if lesson.kind == "dont" and lesson.name:
            dont_groups.setdefault(lesson.name.lower(), []).append(lesson)
    for _, members in sorted(dont_groups.items()):
        rules.append(_rule_from_cluster("dont", members))

    eligible: list[Rule] = []
    held_back: list[Rule] = []
    for rule in rules:
        if rule.reinforcement >= min_reinforcement or rule.blessed:
            eligible.append(rule)
        else:
            held_back.append(rule)

    kind_order = {"do": 0, "watch": 1, "dont": 2}
    eligible.sort(key=lambda r: (
        kind_order[r.kind], -r.reinforcement, (r.name or r.text).lower(),
    ))
    held_back.sort(key=lambda r: (
        kind_order[r.kind], -r.reinforcement, (r.name or r.text).lower(),
    ))
    return eligible, held_back


def build_reference_rules(promoted: list[dict[str, Any]]) -> list[Rule]:
    """One ref rule per promoted knowledge item, in key (chronological) order.

    No clustering and no reinforcement gate: promotion was the human vetting
    step, so each article stands alone — the same logic that lets bless()
    carry a single strong lesson past the threshold.
    """
    rules: list[Rule] = []
    for item in sorted(promoted, key=lambda m: m["key"]):
        title = (item.get("title") or "").strip()
        content = (item.get("content") or "").strip()
        if title and content and not content.lower().startswith(title.lower()):
            text = _one_line(f"{title}: {content}")
        else:
            text = _one_line(content or title)
        if not text:
            continue
        rules.append(Rule(
            kind="ref",
            text=text,
            sources=[item["key"]],
            reinforcement=1,
            name=title or None,
            url=item.get("source_url", ""),
        ))
    return rules


def draft_description(domain: str, user: str) -> str:
    """Compiler draft of the load trigger, in the structured cue format.

    The description is human-owned: this draft exists so review means
    approving a short cue list instead of writing prose. Once accepted it is
    pinned — recompiles never clobber it.
    """
    return (
        f"How {user} works in {domain}: distilled do/don't procedure compiled "
        f"from experience and graveyard memories. Load when: starting "
        f"{domain} work, reviewing or writing {domain} code or content, or "
        f"beginning a greenfield {domain} project."
    )


def _manifest_annotations(rules: list[Rule]) -> list[tuple[str, str]]:
    """(key, annotation) pairs for the frontmatter manifest, deduplicated."""
    seen: dict[str, str] = {}
    for rule in rules:
        for source in rule.sources:
            if source in seen:
                continue
            if rule.kind == "dont":
                seen[source] = f"graveyard: {rule.name}"
            elif rule.kind == "ref":
                seen[source] = "promoted reference"
            elif rule.blessed and rule.reinforcement < 2:
                seen[source] = "blessed"
            else:
                seen[source] = f"reinforced x{rule.reinforcement}"
    return list(seen.items())


def _rule_bullet(rule: Rule) -> str:
    """Render one rule as a markdown bullet with provenance."""
    if rule.kind == "dont":
        type_part = f" ({rule.approach_type})" if rule.approach_type else ""
        reason = rule.text or "abandoned"
        if not reason.endswith((".", "!", "?", "…")):
            reason += "."
        tried = f" Tried on {', '.join(rule.projects)}, abandoned." if rule.projects else ""
        line = f"- Avoid {rule.name}{type_part}. {reason}{tried}"
    elif rule.kind == "ref":
        text = rule.text
        if not text.endswith((".", "!", "?", "…")):
            text += "."
        line = f"- {text}"
        if rule.url:
            line += f" ({rule.url})"
    else:
        text = rule.text
        if not text.endswith((".", "!", "?", "…")):
            text += "."
        line = f"- {text}"

    if rule.blessed and rule.reinforcement < 2:
        line += " (blessed)"
    elif rule.reinforcement > 1:
        line += f" (reinforced x{rule.reinforcement})"
    return f"{line} [{rule.primary_source}]"


def render_skill_md(
    *,
    domain: str,
    user: str,
    description: str,
    rules: list[Rule],
    compiled_at: float,
    min_reinforcement: int,
    contract_version: int = CONTRACT_VERSION,
) -> str:
    """Render the full SKILL.md body. Deterministic for fixed inputs."""
    name = f"{domain}-{user}"
    compiled_iso = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(compiled_at))

    do_rules = [r for r in rules if r.kind == "do"]
    watch_rules = [r for r in rules if r.kind == "watch"]
    dont_rules = [r for r in rules if r.kind == "dont"]
    ref_rules = [r for r in rules if r.kind == "ref"]
    exp_sources = sorted({s for r in do_rules + watch_rules for s in r.sources})

    lines: list[str] = ["---"]
    lines.append(f"name: {name}")
    # json.dumps gives a valid YAML double-quoted scalar, so colons and quotes
    # in a human-edited description can't corrupt the frontmatter.
    lines.append(f"description: {json.dumps(description)}")
    lines.append("generated: true")
    lines.append("source: omnimem")
    lines.append(f"domain: {domain}")
    lines.append(f"compiled_at: {compiled_iso}")
    lines.append(f"contract_version: {contract_version}")
    manifest = _manifest_annotations(rules)
    if manifest:
        lines.append("source_manifest:")
        for key, annotation in manifest:
            lines.append(f"  - {key}   # {annotation}")
    lines.append("---")
    lines.append("")
    lines.append(GENERATED_BANNER.format(domain=domain).rstrip())
    lines.append("")
    lines.append(OPERATING_CONTRACT.rstrip())
    lines.append("")
    lines.append("## How I work  (compiled, domain-specific)")
    lines.append("")
    lines.append(
        f"Distilled procedure from {len(exp_sources)} experience "
        f"{'memory' if len(exp_sources) == 1 else 'memories'} and "
        f"{len(dont_rules)} graveyard "
        f"{'entry' if len(dont_rules) == 1 else 'entries'} in domain `{domain}`. "
        "Each rule cites its source memory; treat Don't entries as known dead "
        "ends and warn_if_abandoned before retrying one."
    )

    if do_rules:
        lines.append("")
        lines.append("## Do")
        lines.append("")
        lines.extend(_rule_bullet(r) for r in do_rules)

    if watch_rules:
        lines.append("")
        lines.append("## Watch out")
        lines.append("")
        lines.extend(_rule_bullet(r) for r in watch_rules)

    if dont_rules:
        lines.append("")
        lines.append("## Don't (and why)")
        lines.append("")
        lines.extend(_rule_bullet(r) for r in dont_rules)

    if ref_rules:
        lines.append("")
        lines.append("## Reference  (promoted knowledge)")
        lines.append("")
        lines.append(
            "Curated reference material promoted from the knowledge "
            "namespace (promote_knowledge). These are vetted pointers, not "
            "lived experience — check the cited article for the full text "
            "before treating one as procedure."
        )
        lines.append("")
        lines.extend(_rule_bullet(r) for r in ref_rules)

    lines.append("")
    lines.append("## Provenance")
    lines.append("")
    provenance = (
        f"Compiled from {len(exp_sources)} experience "
        f"{'memory' if len(exp_sources) == 1 else 'memories'} "
        f"(min {min_reinforcement} "
        f"{'reinforcement' if min_reinforcement == 1 else 'reinforcements'}) and "
        f"{len(dont_rules)} graveyard "
        f"{'entry' if len(dont_rules) == 1 else 'entries'}. "
    )
    if ref_rules:
        provenance += (
            f"Plus {len(ref_rules)} promoted reference "
            f"{'article' if len(ref_rules) == 1 else 'articles'} from the "
            "knowledge namespace. "
        )
    provenance += (
        "Full source manifest in frontmatter. Operating contract at "
        f"contract_version {contract_version}."
    )
    lines.append(provenance)
    lines.append("")
    return "\n".join(lines)


def strip_volatile(body: str) -> str:
    """Drop the compiled_at stamp so bodies compare on substance."""
    return "\n".join(
        line for line in body.splitlines()
        if not line.startswith("compiled_at: ")
    )


def body_sha(body: str) -> str:
    """Fingerprint a skill body for the propose/write handshake."""
    return hashlib.sha256(body.encode("utf-8")).hexdigest()


def bodies_equivalent(old: str, new: str) -> bool:
    """True when two bodies differ only by the compiled_at stamp."""
    return strip_volatile(old) == strip_volatile(new)


def render_unified_diff(old_body: str, new_body: str, skill_id: str) -> str:
    """Unified diff of stored vs proposed body."""
    diff = difflib.unified_diff(
        old_body.splitlines(keepends=True),
        new_body.splitlines(keepends=True),
        fromfile=f"{skill_id} (stored)",
        tofile=f"{skill_id} (proposed)",
    )
    return "".join(diff)


def _gist(text: str, limit: int = 72) -> str:
    text = text.strip()
    return text if len(text) <= limit else text[: limit - 1].rstrip() + "…"


def summarise_rule_changes(
    old_rules: list[dict[str, Any]],
    new_rules: list[dict[str, Any]],
) -> list[dict[str, Any]]:
    """Risk-classified change list between two rule manifests.

    Prominence scales with risk: a new rule is a low-stakes addition (batch-
    accept eligible); anything that rewrites or removes an existing rule is
    flagged high so it can't slip through a batch accept. Reinforcement-only
    growth (same wording, more sources) is low risk. Untouched rules are not
    reported.

    Matching is by approach name for graveyard rules and by shared source
    memories (falling back to identical text) for do/watch/ref rules — the
    same identities the compiler itself clusters on.
    """
    changes: list[dict[str, Any]] = []

    def _add(change: str, risk: str, rule: dict[str, Any], was: str | None = None) -> None:
        entry = {
            "change": change,
            "risk": risk,
            "rule_kind": rule.get("kind", ""),
            "rule": _gist(
                f"Avoid {rule['name']}" if rule.get("kind") == "dont" and rule.get("name")
                else rule.get("text", "")
            ),
        }
        if was:
            entry["was"] = _gist(was)
        changes.append(entry)

    for kind in ("do", "watch", "dont", "ref"):
        olds = [r for r in old_rules if r.get("kind") == kind]
        news = [r for r in new_rules if r.get("kind") == kind]

        matched_old: set[int] = set()
        for new in news:
            match_idx: int | None = None
            if kind == "dont":
                for i, old in enumerate(olds):
                    if i in matched_old:
                        continue
                    if (old.get("name") or "").lower() == (new.get("name") or "").lower():
                        match_idx = i
                        break
            else:
                new_sources = set(new.get("sources", []))
                best_overlap = 0
                for i, old in enumerate(olds):
                    if i in matched_old:
                        continue
                    overlap = len(new_sources & set(old.get("sources", [])))
                    if overlap > best_overlap:
                        best_overlap = overlap
                        match_idx = i
                if match_idx is None:
                    for i, old in enumerate(olds):
                        if i not in matched_old and old.get("text") == new.get("text"):
                            match_idx = i
                            break

            if match_idx is None:
                _add("added", "low", new)
                continue

            matched_old.add(match_idx)
            old = olds[match_idx]
            if old.get("text") != new.get("text"):
                _add("rewritten", "high", new, was=old.get("text", ""))
            elif set(old.get("sources", [])) != set(new.get("sources", [])):
                _add("reinforced", "low", new)
            # identical rule: nothing to report

        for i, old in enumerate(olds):
            if i not in matched_old:
                _add("removed", "high", old)

    risk_order = {"high": 0, "low": 1}
    changes.sort(key=lambda c: (risk_order[c["risk"]], c["rule_kind"], c["rule"]))
    return changes


def known_domains(store) -> dict[str, int]:
    """Candidate domain vocabulary: episodic tags plus existing skill domains.

    Returns {domain: occurrence_count}, used by the did-you-mean guard and by
    no-candidate responses so the caller can show what actually exists.
    """
    counts: dict[str, int] = {}

    keys = store.scan_prefix("mem:episodic:")
    if keys:
        if len(keys) > _MAX_POOL_KEYS:
            keys = keys[:_MAX_POOL_KEYS]
        rows = store.get_fields_multi(keys, ("tags", "state"))
        for row in rows:
            if row is None or row.get("state") not in ("active", None):
                continue
            for tag in _parse_tags(row.get("tags")):
                tag = tag.lower()
                counts[tag] = counts.get(tag, 0) + 1

    skill_keys = store.scan_prefix(SKILL_KEY_PREFIX)
    if skill_keys:
        for row in store.get_fields_multi(skill_keys, ("domain",)):
            domain = (row or {}).get("domain")
            if domain:
                counts[domain] = counts.get(domain, 0) + 1

    return counts


def suggest_similar_domain(
    embedder, domain: str, candidates: Iterable[str]
) -> tuple[str, float] | None:
    """Did-you-mean guard: closest existing domain to a new one, if any.

    Substring containment (python3 / python) is a strong signal on its own;
    otherwise embedding similarity above SKILL_DOMAIN_SUGGEST_THRESHOLD
    (default 0.60) surfaces synonyms and rephrasings. Exact matches are the
    caller's business, not a suggestion.
    """
    threshold = float(os.getenv("SKILL_DOMAIN_SUGGEST_THRESHOLD", "0.60"))
    pool = sorted({c.lower() for c in candidates if c and c.lower() != domain})
    if not pool:
        return None

    for candidate in pool:
        if domain in candidate or candidate in domain:
            return candidate, 0.9

    # Cap the embedding comparison — tag vocabularies are small, but don't
    # let a pathological store turn a guard into a batch job.
    pool = pool[:200]
    target = embedder.embed(domain)
    vectors = embedder.embed_batch(pool)
    sims = [float(np.dot(target, v)) for v in vectors]
    best = int(np.argmax(sims))
    if sims[best] >= threshold:
        return pool[best], round(sims[best], 4)
    return None
