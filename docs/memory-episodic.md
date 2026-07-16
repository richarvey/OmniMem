# Episodic Memory Specification

**Key format**: `mem:episodic:{ULID}`
**Created by**: `remember()` (default namespace), `remember_document()`
**Index**: `idx:episodic`

Episodic memories are the working record of what happened: decisions, bug fixes, patterns discovered, work done. They are also the substrate for two derived systems — experience scoring (effort, outcomes, the abandoned-approach graveyard) and, in v6, compiled skills, which distil lessons from the episodic pool.

## Fields

### Core (written by `remember()`)

| Field | Format | Required | Description |
|-------|--------|----------|-------------|
| `content` | string, max 50,000 chars | yes | The memory text. Embedded verbatim to produce `vector`. |
| `state` | `active` \| `deprioritised` \| `archived` | yes | Lifecycle state, `active` on creation. |
| `surface_score` | float string | yes | `"1.0"` on creation; follows state thereafter. |
| `experience_weight` | float string | yes | `"1.0"` on creation; recomputed by `record_experience()` (see below). |
| `created_at` | unix seconds string | yes | Write time. |
| `updated_at` | unix seconds string | yes | Bumped on any mutation. |
| `tags` | JSON array of strings | yes | `"[]"` if none given. Max 20 tags, 100 chars each. Lowercased tags double as skill domains for the compiler. |
| `project` | string | no | Project scope. Only present when supplied. |
| `vector` | 384-dim float32 blob | yes | Embedding of `content`. |

### Document chunks (added by `remember_document()`)

Long-form content is split into chunks, each stored as its own episodic memory with three extra fields:

| Field | Format | Description |
|-------|--------|-------------|
| `doc_id` | ULID string | Shared across all chunks of one document, for grouping and cleanup. |
| `chunk_index` | int string | Position of this chunk in the original document, 0-based. |
| `chunk_strategy` | `turn_pairs` \| `sentences` \| `paragraphs` \| `fixed_tokens` | How the document was split. |

### Experience (added by `record_experience()` / `log_abandoned()`)

| Field | Format | Description |
|-------|--------|-------------|
| `effort_score` | int string, 1-5 | 1 = trivial, 3 = moderate, 5 = battle-hardened. |
| `outcome` | `succeeded` \| `pivoted` \| `abandoned` | How the work ended. |
| `iterations` | int string | Number of attempts (default 1). |
| `experience_weight` | float string | Recall multiplier computed from effort and outcome (formula below). |
| `abandoned_approaches` | JSON array | The graveyard. Entries are `{"name", "type", "reason"}`; `log_abandoned()` adds `"attempted_at"` (ISO 8601 UTC). `type` is one of `library`, `approach`, `tool`, `pattern`, `service`. Appended to, never replaced. |
| `breakthrough` | string | What finally worked. Becomes a "Do" rule candidate for skill compilation when the outcome is `succeeded`. |
| `gotchas` | string | Caveats to watch for. Becomes a "Watch out" rule candidate. |

The experience weight formula (`compute_experience_weight` in `memory/recall.py`):

```
base:   succeeded 1.0, pivoted 0.7, abandoned 0.1
effort: 1 → x1.0, 2 → x1.1, 3 → x1.25, 4 → x1.5, 5 → x1.8
weight = base * effort   (capped at 2.0; effort never amplifies abandoned outcomes)
```

Recording an `abandoned` outcome with `effort_score >= 4` auto-suppresses each abandoned approach name as a topic.

### Lifecycle and cross-references

| Field | Format | Description |
|-------|--------|-------------|
| `deprioritised_reason` | string | Why the memory was deprioritised. Cleared (set to `""`) on reinstate. |
| `reinstate_hints` | JSON array of strings | Keywords that mark this deprioritised memory as a reinstate candidate when a recall query matches one. |
| `contradictions` | JSON array | Cross-links written by contradiction detection. Entries are `{"key", "explanation", "detected_at"}`, appended symmetrically to both memories, deduplicated by key. |
| `recall_count` | int string | Incremented per recall hit. |
| `last_recalled` | unix seconds string | Set alongside `recall_count`. |
| `event_date` | unix seconds string | Optional temporal anchor. When present and a recall query mentions a date, the temporal boost (1.0-1.5x) applies. |
| `enriched_from` | key string | Present in the search return whitelist for consistency; extracted facts themselves live in the knowledge and preference namespaces, not here. |

### Skill eligibility (v6, added by `bless()`)

| Field | Format | Description |
|-------|--------|-------------|
| `blessed` | `"1"` | Marks a single strong lesson as skill-eligible, bypassing the reinforcement threshold (default 2 distinct source memories) at the next `compile_skill()`. Only episodic keys can be blessed. |
| `blessed_at` | unix seconds string | When it was blessed. |

Blessing does not write to any skill; the propose-and-accept gate still applies at compile time.

## Calling the tools

Every option shown, with its default where one exists.

```python
# Store a memory. Only content is required.
remember(
    content="Fixed the arm64 build by switching to tonistiigi/binfmt",  # required, max 50KB
    project="omnimem",              # default None — unscoped
    tags=["docker", "arm64"],       # default None → stored as []; max 20, 100 chars each
    namespace="episodic",           # default; also accepts 'project', 'knowledge', 'preference'
    force=False,                    # default; True skips dedup (cosine 0.92), the
                                    # contradiction check, and enrichment (raw bypass write)
    mode="full",                    # default follows INGEST_MODE env var; 'raw' stores verbatim,
)                                   # 'full' also queues fact extraction via Claude

# Index a long document as chunks sharing one doc_id.
remember_document(
    content=long_transcript,        # required, max 50KB
    chunk_strategy="paragraphs",    # default; also 'turn_pairs' (User:/Assistant: transcripts),
                                    # 'sentences', 'fixed_tokens'
    project="omnimem",              # default None
    tags=["meeting"],               # default None; applied to every chunk
    namespace="episodic",           # default; also 'project' or 'knowledge' (not 'preference')
    chunk_size=200,                 # words per chunk, fixed_tokens only (default 200)
    mode="full",                    # as on remember()
)

# Attach effort and outcome to an existing memory.
record_experience(
    key="mem:episodic:01KQ...",     # required
    effort_score=4,                 # required, 1-5 (1=trivial, 5=battle-hardened)
    outcome="succeeded",            # required: 'succeeded', 'pivoted', or 'abandoned'
    iterations=3,                   # default 1
    abandoned_approaches=[          # default None; appended to the graveyard, never replaces
        {"name": "qemu-user-static", "type": "docker-image",
         "reason": "amd64-only, exec format error on arm64"},
    ],                              # type: 'library', 'approach', 'tool', 'pattern', 'service'
    breakthrough="tonistiigi/binfmt registers handlers on arm64 hosts",  # default None
    gotchas="needs --privileged on first run",                           # default None
)

# Append one dead end without re-recording the whole experience. All four required.
log_abandoned(
    key="mem:episodic:01KQ...",
    name="Alpine base image",
    type="approach",                # 'library', 'approach', 'tool', 'pattern', or 'service'
    reason="PyTorch has no musllinux wheels",
)

# Mark a single strong lesson skill-eligible (bypasses the reinforcement gate).
bless(memory_key="mem:episodic:01KQ...")   # episodic keys only
```

`record_experience(outcome="abandoned", effort_score=4)` (or 5) also auto-suppresses each abandoned approach name as a topic.

## Indexed fields

`idx:episodic` indexes: `vector` (HNSW cosine), `project` (tag), `state` (tag), `tags` (tag), `outcome` (tag), `surface_score`, `created_at`, `updated_at`, `effort_score`, `iterations`, `experience_weight`, `recall_count` (numeric).

State and project filters are pushed into FT.SEARCH as tag filters so archived or out-of-project documents don't consume KNN candidate slots. Tag values are interpolated raw after allowlist validation — see the valkey-search gotcha in CLAUDE.md.

## How the skill compiler reads this namespace

`gather_domain_pool()` treats lowercased tags as domains. From each active memory in a domain pool it extracts lessons:

- `breakthrough` + `outcome == succeeded` → **do** lesson
- `gotchas` → **watch** lesson
- each `abandoned_approaches` entry → **dont** lesson (grouped by approach name)
- a blessed memory always contributes: its breakthrough regardless of outcome, or its bare `content` if it carries no structured lesson fields

Do/watch lessons cluster by embedding similarity (`SKILL_CLUSTER_THRESHOLD`, default 0.80); a rule needs `min_reinforcement` distinct source memories (default 2) or a blessing to clear the gate.
