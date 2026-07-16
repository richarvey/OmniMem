# Memory Types Overview

OmniMem stores every memory as a Valkey hash under a namespaced key. There are five namespaces in v6:

| Namespace | Key prefix | Writable via | Spec |
|-----------|-----------|--------------|------|
| Episodic | `mem:episodic:` | `remember()`, `remember_document()` | [memory-episodic.md](memory-episodic.md) |
| Project | `mem:project:` | `set_project_context()`, `remember(namespace="project")` | [memory-project.md](memory-project.md) |
| Knowledge | `mem:knowledge:` | RSS worker, enrichment worker, `remember(namespace="knowledge")` | [memory-knowledge.md](memory-knowledge.md) |
| Preference | `mem:preference:` | enrichment worker, `remember(namespace="preference")` | [memory-preference.md](memory-preference.md) |
| Skill | `mem:skill:` | `compile_skill()` gate only | [memory-skill.md](memory-skill.md) |

The first four accept direct writes through `remember()`. The skill namespace is derived: it is searchable like the others but only writable through the propose-and-accept gate in `compile_skill()`.

## Storage model

Every memory is a single Valkey hash. All field values are strings (Valkey hashes have no other type), so:

- **Timestamps** are stringified unix seconds with fractional precision, e.g. `"1784240112.0184064"`. This applies to `created_at`, `updated_at`, `last_recalled`, `expires_at`, `event_date`, `published_at`, `promoted_at`, `blessed_at`.
- **Numbers** are stringified, e.g. `surface_score` is `"1.0"`, `effort_score` is `"3"`.
- **Lists and objects** are JSON-encoded strings, e.g. `tags` is `'["docker", "arm64"]'`.
- **Booleans** are `"1"`/absent (`blessed`) or `"true"`/absent (`generated`).

The one exception is `vector`: a binary blob of 384 float32 values (1,536 bytes), the sentence-transformers all-MiniLM-L6-v2 embedding of the memory's content. It is written by `store.upsert()` and read only through the binary-safe client (`store.get_vectors_multi()`); the regular text-mode client never touches it.

## Keys

- Episodic, preference, and manually stored knowledge memories use ULID suffixes: `mem:episodic:01KQCNXXCZSX9EPFJ1VCSVXPCV`. ULIDs sort chronologically, which the skill compiler and list views rely on.
- Project context entries use the project name directly: `mem:project:omnimem`.
- RSS articles use a 16-hex-char SHA-256 prefix of the article URL: `mem:knowledge:a1b2c3d4e5f60718`. Digest-mode feeds hash `url + ':' + item_index` so one page can yield several items.
- Compiled skills use `mem:skill:gen:{domain}-{user}`.

`store.upsert()` refuses any key that does not start with a known prefix (`_VALID_KEY_PREFIXES` in `memory/store.py`).

## Common fields

These appear on every namespace unless noted:

| Field | Format | Meaning |
|-------|--------|---------|
| `content` | string, max 50,000 chars | The memory text. For skills this is replaced by `body`; for project context it mirrors `description`. |
| `state` | `active` \| `deprioritised` \| `archived` | Lifecycle state. `deleted` exists as a transition target but deleted keys are removed, never stored. |
| `surface_score` | stringified float | Recall visibility multiplier derived from state: active 1.0, deprioritised `DEPRIORITISED_WEIGHT` (default 0.2), archived 0.0. Enriched facts are written at 0.5 so verbatim sources outrank them. |
| `created_at` | unix seconds string | Set once at write time. |
| `updated_at` | unix seconds string | Bumped on every state transition, retag, experience write, or field update. Backup restore uses it to decide merge wins. |
| `vector` | 1,536-byte float32 blob | Embedding. What gets embedded varies by type (see each spec). |
| `recall_count` | stringified int | Incremented (HINCRBY) each time recall returns the memory. Feeds `/telemetry` and `/metrics`. |
| `last_recalled` | unix seconds string | Set alongside `recall_count`. |

## Lifecycle states

Defined in `memory/lifecycle.py`. Allowed transitions:

- `active` → `deprioritised`, `archived`, `deleted`
- `deprioritised` → `active`, `archived`, `deleted`
- `archived` → `active`, `deleted`
- `deleted` → nothing (the key is hard-deleted)

Deprioritising with a reason writes `deprioritised_reason`; reinstating clears it and resets `surface_score` to 1.0. `reinstate_hints` (JSON array of keywords) flags a deprioritised memory as a reinstate candidate when a recall query matches one of the hints.

## Calling the cross-namespace tools

Writers are shown in each namespace's spec. These work on any memory regardless of type:

```python
# Semantic search across namespaces.
recall(
    query="docker arm64 build failures",  # required
    top_k=5,                        # default; max results after ranking
    namespaces=["episodic", "knowledge"],  # default None — episodic, project, knowledge,
                                    # and preference (skill discovery goes via find_skills)
    project_filter="omnimem",       # default None — no project restriction
    expand_queries=False,           # default follows RECALL_EXPAND_QUERIES env var;
)                                   # True unions alternative phrasings via Claude Haiku

# Replace or adjust tags without re-embedding. tags is mutually exclusive with add/remove.
retag(key="mem:episodic:01KQ...", tags=["docker", "arm64"])   # full replacement; [] clears
retag(key="mem:episodic:01KQ...", add=["ci"], remove=["wip"]) # adjust the existing set

# Lifecycle transitions. Each accepts a key or a natural-language query — a query
# resolves via recall and transitions the confident matches (top 3).
deprioritise(
    key_or_query="mem:episodic:01KQ...",   # required
    reason="Superseded by the v6 approach",  # required
    reinstate_hints=["binfmt", "arm64"],   # default None; keywords that flag it as a
)                                          # reinstate candidate in future queries
archive(
    key_or_query="mem:episodic:01KQ...",   # required
    reason="Historical only",              # default None
)
reinstate(key_or_query="mem:episodic:01KQ...")  # back to active, surface_score 1.0

# Permanent deletion. confirm=False (default) returns a preview of what would go.
forget(key_or_query="mem:episodic:01KQ...", confirm=True)
```

## Search indexes

Each namespace has one HNSW vector index (`idx:episodic`, `idx:project`, `idx:knowledge`, `idx:preference`, `idx:skill`) with cosine distance over the 384-dim vector, plus the tag and numeric fields listed in each spec. Index definitions live in `INDEX_DEFINITIONS` in `memory/store.py`; a startup migration drops and recreates any index whose field count no longer matches.

Two things to keep in mind when adding fields:

1. **`_NAMESPACE_RETURN_FIELDS` is a whitelist.** A field stored in the hash but missing from that tuple is silently absent from search results. This has bitten before (issue #20: `project` missing from the knowledge tuple made project-filtered recall drop every knowledge result).
2. **List/telemetry views use fixed projections.** `get_fields_multi()` only fetches named fields; a new field must be added to each view's projection tuple or it reads as `None` there.

## Validation constraints

- Content: max 50KB per memory.
- Project names: 1-200 chars, alphanumeric plus hyphens, underscores, dots, and spaces.
- Tags: max 20 per memory, each 100 chars or fewer, same charset as project names.
- Skill domains: 1-64 chars of lowercase `[a-z0-9._-]`, normalised to kebab-case, with an alias map (`py` → `python` etc.) in `memory/skills.py`.
