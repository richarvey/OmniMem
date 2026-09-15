# Memory Types Overview

This is the storage model, field by field: what OmniMem actually writes when you (or your agent) remember something, and who writes it. You don't need any of this to use OmniMem. It's here for when you want to know exactly what's sitting in your database, or you're building something on top of it.

There are five namespaces:

| Namespace | Key prefix | Written by | Spec |
|-----------|-----------|------------|------|
| Episodic | `mem:episodic:` | `remember()`, `remember_document()` | [memory-episodic.md](memory-episodic.md) |
| Project | `mem:project:` | `set_project_context()`, `compile_project_context(auto_save=True)`, `remember(namespace="project")` | [memory-project.md](memory-project.md) |
| Knowledge | `mem:knowledge:` | the RSS scheduler, the enrichment worker, `remember(namespace="knowledge")` | [memory-knowledge.md](memory-knowledge.md) |
| Preference | `mem:preference:` | the enrichment worker, `remember(namespace="preference")` | [memory-preference.md](memory-preference.md) |
| Skill | `mem:skill:` | the `compile_skill()` gate only | [memory-skill.md](memory-skill.md) |

The first four accept direct writes through `remember()`. The skill namespace is derived: it's searchable like the others, but only the propose-and-accept gate in `compile_skill()` can write to it.

## Storage model

OmniMem 7 keeps everything in one SQLite file (`data/omnimem.db` for `omnimem serve`, or the desktop app's data folder). A memory is one row in the `memories` table:

| Column | What it holds |
|--------|---------------|
| `key` | The memory's key, e.g. `mem:episodic:01KQCNXXCZSX9EPFJ1VCSVXPCV`. Primary key. |
| `namespace` | `episodic`, `project`, `knowledge`, `preference` or `skill`. |
| `fields` | The record itself: a JSON object of **string** fields. Everything in the tables on these pages lives here. |
| `state`, `project`, `project_name`, `feed_name`, `created_at`, `updated_at`, `content_hash` | Generated columns, computed by SQLite from `fields` on every write and indexed, so filtered search and the list views are real queries. You never write them directly. |

The vector lives beside it in the `vectors` table: 384 little-endian float32 values (1,536 bytes), the all-MiniLM-L6-v2 embedding computed locally by ONNX Runtime. Deleting a memory deletes its vector with it. On start, every vector is loaded into memory and searched exactly (a dot product against every row that passes the filter), so there's no approximate index to drift out of step with the records.

Field values are strings, as they were in 6.x, so a backup round-trips untouched:

- **Timestamps** are unix seconds with fractional precision, written the way Python printed a float: `"1784240112.0184064"`. This covers `created_at`, `updated_at`, `last_recalled`, `expires_at`, `event_date`, `published_at`, `promoted_at`, `blessed_at` and `compiled_at`.
- **Numbers** are strings too: `surface_score` is `"1.0"`, `effort_score` is `"3"`.
- **Lists and objects** are JSON-encoded strings: `tags` is `'["docker", "arm64"]'`.
- **Booleans** are `"1"` or absent (`blessed`), and `"true"` or absent (`generated`).

A field that isn't in these specs (something an old 6.x version wrote, say) survives an import and an export untouched: `fields` holds whatever it's given.

### Everything that isn't a memory

Records that were other Valkey keys in 6.x live in the `kv` table, as a hash, a set or a string with an optional expiry: `meta:*` (maintenance counters, skill proposals, tool metrics, the feed influence mirror, caches), `log:recall:*` (a recall log entry per query, kept for 30 days), `topics:suppressed` (the suppressed topic set) and `qexp:*` (the query expansion cache). An expired row reads as absent. The enrichment queue has its own table (`enrich_queue`), so a queued job survives a crash, and OAuth clients, codes and tokens have theirs.

Backups (`dump_to_file()`, or `omnimem export`) write the 6.x JSON format: every memory plus the `topics:`, `log:recall:` and `meta:` records. Vectors aren't in a backup; `omnimem import <file>` and `restore_from_file()` re-embed on the way in.

## Keys

- Episodic, preference and manually stored knowledge memories get a ULID: `mem:episodic:01KQCNXXCZSX9EPFJ1VCSVXPCV`. ULIDs sort by time, which the skill compiler and the list views rely on.
- Project context entries use the project name: `mem:project:omnimem`.
- RSS articles use the first 16 hex characters of the SHA-256 of the article URL: `mem:knowledge:a1b2c3d4e5f60718`. Digest-mode feeds hash `url:index` instead, so one page can yield several items.
- Compiled skills use `mem:skill:gen:{domain}-{user}`.

The store refuses any key that doesn't start with one of `mem:episodic:`, `mem:project:`, `mem:knowledge:`, `mem:preference:`, `mem:skill:`, `topics:`, `log:recall:`, `meta:`, `qexp:` or `queue:`. Imported keys keep whatever id they arrived with.

## Common fields

These appear on every namespace unless a spec says otherwise:

| Field | Format | Meaning |
|-------|--------|---------|
| `content` | string, max 50,000 characters | The memory text. Skills use `body` instead; project context entries mirror `description` here. |
| `state` | `active` \| `deprioritised` \| `archived` | Lifecycle state. `deleted` is a transition, not a state you'll find stored: the row is removed. |
| `surface_score` | float string | Recall visibility from the state: active 1.0, deprioritised `DEPRIORITISED_WEIGHT` (default 0.2), archived 0.0. Extracted facts are written at 0.5 so their verbatim sources outrank them. |
| `created_at` | unix seconds string | Set once, at write time. |
| `updated_at` | unix seconds string | Bumped by state transitions, retags, experience writes and field updates. A backup restore uses it to decide which copy wins. Reclassifying licence or provenance deliberately doesn't touch it, because the skill compiler reads a change here as "the source changed". |
| `recall_count` | int string | Incremented each time recall returns the memory. Feeds the telemetry page in the [settings panel](settings-panel.md). |
| `last_recalled` | unix seconds string | Set alongside `recall_count`. |
| `licence` | `own` \| `open` \| `restricted` \| `unknown` | Redistribution rights, decided at write time and never used for ranking. Defaults: `own` for episodic, project and preference writes, `unknown` for knowledge. RSS articles get what the feed declares (`unknown` if nothing). Extracted facts inherit their source's. Skills carry none. |
| `licence_note` | string, max 200 characters | Optional detail alongside `licence`: the identifier (`OGL v3.0`, `CC BY 4.0`) or where it was checked. Written as `""` when a reclassification clears it. |
| `provenance` | `retrieved` \| `concluded` \| `asserted` | Who is speaking: an external source, the system's own reasoning or write-up, or the human directly. Defaults: `concluded` for episodic and project writes, `asserted` for preferences, `retrieved` for knowledge. Extracted facts inherit their source's. Reported on recall, never scored on, and independent of `licence`. |

A record written before a field existed (an old import, say) still reports something honest: read paths resolve a missing `licence` or `provenance` the same way the 6.x backfill would have, so an article reads as `unknown` and `retrieved`, an extracted fact as its inherited class, and so on.

### v7 identity fields

The store stamps these itself. They're groundwork for Mycelium (see [the v7 change spec](v7-change-spec.md)), which isn't built yet, so nothing reads them today apart from the hash.

| Field | Format | Meaning |
|-------|--------|---------|
| `content_hash` | `sha256:` plus lowercase hex | Recomputed on every write of `content`: SHA-256 of the content after Unicode NFC, LF line endings, trailing whitespace stripped from each line and blank lines stripped from the ends. Removed if `content` is empty. |
| `origin_id` | ULID string | The node that created the memory: each database gets one on first open and stamps it on every memory created there. |
| `epoch` | int string | `"1"` on creation. Planned to increase on a meaningful edit; nothing bumps it yet. |
| `classification` | JSON string | `{"level":"internal","scopes":[]}` on creation. The disclosure scope planned for Mycelium; nothing filters on it yet. |

## Lifecycle states

Allowed transitions:

- `active` → `deprioritised`, `archived`, `deleted`
- `deprioritised` → `active`, `archived`, `deleted`
- `archived` → `active`, `deleted`
- `deleted` → nothing (the row is gone)

Deprioritising with a reason writes `deprioritised_reason`. Reinstating sets it back to `""` and resets `surface_score` to 1.0. `reinstate_hints` (a JSON array of keywords) marks a deprioritised memory as a reinstate candidate when a recall query mentions one of them. Deprioritising something with an effort score of 4 or 5 still works, but the result warns you that it was hard-won.

## Calling the cross-namespace tools

Each namespace's spec shows its writers. These work on any memory:

```python
# Semantic search across namespaces.
recall(
    query="docker arm64 build failures",  # required
    top_k=5,                        # default MEMORY_RECALL_TOP_K; at most 50
    namespaces=["episodic", "knowledge"],  # default: episodic, project, knowledge and
                                    # preference (skills are found with find_skills)
    project_filter="omnimem",       # default None; a name or a list of names
    domain_filter="python",         # default None; every project declaring the domain
    expand_queries=False,           # default follows RECALL_EXPAND_QUERIES; True adds
)                                   # alternative phrasings from Claude Haiku

# Replace or adjust tags without re-embedding. tags can't be combined with add/remove.
retag(key="mem:episodic:01KQ...", tags=["docker", "arm64"])   # full replacement; [] clears
retag(key="mem:episodic:01KQ...", add=["ci"], remove=["wip"]) # adjust the existing set

# Lifecycle transitions take a key, or a query that resolves to the confident matches.
deprioritise(
    key_or_query="mem:episodic:01KQ...",   # required
    reason="Superseded by the v7 approach",  # required
    reinstate_hints=["binfmt", "arm64"],   # default None
)
archive(key_or_query="mem:episodic:01KQ...", reason="Historical only")  # reason optional
reinstate(key_or_query="mem:episodic:01KQ...")  # back to active, surface_score 1.0

# Permanent deletion. confirm=False (the default) previews what would go.
forget(key_or_query="mem:episodic:01KQ...", confirm=True)

# Reclassify rights or provenance, cascading to chunks and extracted facts.
set_licence(keys=["mem:knowledge:a1b2..."], licence="cc-by-4.0")
set_provenance(keys=["mem:episodic:01KQ..."], provenance="asserted")
```

The [MCP tool reference](mcp-tools.md) has every tool and argument.

## Validation constraints

- Content: at most 50,000 characters per memory.
- Project names: 1-200 characters of letters, digits, hyphens, underscores, dots and spaces.
- Tags: at most 20 per memory, each at most 100 characters.
- Project domains: at most 20 per project.
- Domains (skills and projects): lowercased, whitespace turned into hyphens, then 1-64 characters of `[a-z0-9._-]` starting with a letter or digit. Aliases resolve first: `py` and `python3` → `python`, `js` → `javascript`, `ts` → `typescript`, `golang` → `go`, `rs` → `rust`, `k8s` → `kubernetes`, `postgres` → `postgresql`.
- `licence_note`: at most 200 characters. `set_licence` and `set_provenance` take at most 200 keys per call.
- Namespaces for `remember()`: `episodic`, `project`, `knowledge` or `preference`. `skill` is a search namespace only.
