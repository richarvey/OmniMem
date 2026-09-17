# Episodic Memory Specification

**Key format**: `mem:episodic:{ULID}`
**Created by**: `remember()` (the default namespace), `remember_document()`

Episodic memories are the working record of what happened: decisions, bug fixes, patterns you found, work done. They're also what two other systems feed on. Experience scoring (effort, outcomes and the graveyard of abandoned approaches) lives on these records, and compiled skills are distilled from them.

Field formats follow the [storage model](memory-types.md#storage-model): every value is a string, lists are JSON.

## Fields

### Core (written by `remember()`)

| Field | Format | Always present | Description |
|-------|--------|----------------|-------------|
| `content` | string, max 50,000 characters | yes | The memory text. Embedded as it is. |
| `state` | lifecycle state | yes | `active` on creation. |
| `surface_score` | float string | yes | `"1.0"` on creation; follows the state after that. |
| `experience_weight` | float string | yes | `"1.0"` on creation; recomputed by `record_experience()`. |
| `created_at` / `updated_at` | unix seconds strings | yes | Write time; `updated_at` moves on every change. |
| `tags` | JSON array of strings | yes | `"[]"` when none are given. At most 20, 100 characters each. Lowercased tags double as skill domains for the compiler. |
| `project` | string | no | Project scope, only when one is given. |
| `licence` / `licence_note` | licence class / string | yes / no | `own` by default, since an episodic memory is a write-up of work done here. Pass `licence=` when it's someone else's content (a pasted document, a vendor page). |
| `provenance` | provenance class | yes | `concluded` by default: it's the agent's write-up. Pass `provenance="asserted"` when the human dictated it, or `"retrieved"` for a pasted external document. |

The store adds `content_hash`, `origin_id`, `epoch` and `classification` to every new memory; see [v7 identity fields](memory-types.md#v7-identity-fields). The vector (the embedding of `content`) is stored in the `vectors` table, not in the fields.

### Document chunks (added by `remember_document()`)

Long content is split into chunks, and each chunk is stored as its own episodic memory with three extra fields:

| Field | Format | Description |
|-------|--------|-------------|
| `doc_id` | ULID string | Shared by every chunk of one document. |
| `chunk_index` | int string | Position in the original document, from 0. |
| `chunk_strategy` | `turn_pairs` \| `sentences` \| `paragraphs` \| `fixed_tokens` | How it was split. |

Each chunk is dedup-checked on its own, and a chunk that's already stored is skipped.

### Experience (added by `record_experience()` and `log_abandoned()`)

| Field | Format | Description |
|-------|--------|-------------|
| `effort_score` | int string, 1-5 | 1 is trivial, 3 moderate, 5 battle-hardened. |
| `outcome` | `succeeded` \| `pivoted` \| `abandoned` | How the work ended. |
| `iterations` | int string | Number of attempts (default 1). |
| `experience_weight` | float string | The recall multiplier from effort and outcome (below). |
| `abandoned_approaches` | JSON array | The graveyard. Entries are `{"name", "type", "reason"}`, and `log_abandoned()` adds `"attempted_at"` (ISO 8601 UTC, e.g. `2026-09-15T20:32:56Z`). `type` is one of `library`, `approach`, `tool`, `pattern`, `service`. Always appended to, never replaced. |
| `breakthrough` | string | What finally worked this time. |
| `lesson` | string | The general rule the work taught, one that holds beyond this incident. Preferred over `breakthrough` when the skill compiler picks a "Do" rule. |
| `gotchas` | string | Caveats to watch for. |

The experience weight:

```
base:   succeeded 1.0, pivoted 0.7, abandoned 1.0
effort: 1 → x1.0, 2 → x1.1, 3 → x1.25, 4 → x1.5, 5 → x1.8
weight = base * effort, capped at 2.0
an abandoned outcome is always 1.0: effort never amplifies it
```

Recording an `abandoned` outcome with an effort score of 4 or 5 lists the approach names under `auto_suppressed` in the result. They are not actually suppressed: nothing is added to `topics:suppressed`. Suppressing them hid the memory that explained the failure, and store-wide, so a dead end on one project silenced unrelated memories elsewhere. `suppress_topic` remains the only thing that writes that list.

### Lifecycle and cross-references

| Field | Format | Description |
|-------|--------|-------------|
| `deprioritised_reason` | string | Why it was deprioritised. Set to `""` on reinstate. |
| `reinstate_hints` | JSON array of strings | Keywords that make this deprioritised memory a reinstate candidate when a recall query mentions one. |
| `contradictions` | JSON array | Cross-links from contradiction detection: `{"key", "explanation", "detected_at"}`, written to both memories and de-duplicated by key. |
| `recall_count` / `last_recalled` | int string / unix seconds string | Recall counters. |
| `event_date` | unix seconds string | Optional date the memory is about. When a recall query mentions a date near it, the temporal boost (1.0-1.5x) applies. `remember()` doesn't set it; it arrives with imported 6.x records, and extracted facts carry one (see [knowledge](memory-knowledge.md)). |

### Skill eligibility (added by `bless()`)

| Field | Format | Description |
|-------|--------|-------------|
| `blessed` | `"1"` | Makes one strong lesson skill-eligible without waiting for it to recur (the reinforcement gate, default 2 distinct source memories). Only episodic keys can be blessed. |
| `blessed_at` | unix seconds string | When it was blessed. |

Blessing doesn't write a skill. The propose-and-accept gate still applies when you compile.

## Calling the tools

```python
# Store a memory. Only content is required.
remember(
    content="Fixed the arm64 build by switching to tonistiigi/binfmt",  # max 50,000 chars
    project="omnimem",              # default None: unscoped
    tags=["docker", "arm64"],       # default None, stored as []
    namespace="episodic",           # default; also 'project', 'knowledge', 'preference'
    force=False,                    # default; True skips the duplicate check (similarity
                                    # 0.92), the contradiction check and enrichment
    mode="full",                    # default INGEST_MODE; 'raw' stores it as written,
                                    # 'full' also queues fact extraction with Claude
    licence="own",                  # default depends on the namespace
    provenance="concluded",         # default depends on the namespace
)

# Index a long document as chunks sharing one doc_id.
remember_document(
    content=long_transcript,        # required, max 50,000 chars
    chunk_strategy="paragraphs",    # default; also 'turn_pairs', 'sentences', 'fixed_tokens'
    project="omnimem",              # default None
    tags=["meeting"],               # default None; applied to every chunk
    namespace="episodic",           # default; also 'project' or 'knowledge'
    chunk_size=200,                 # words per chunk, fixed_tokens only
    mode="full",                    # as on remember()
)

# Attach effort and outcome to an existing memory.
record_experience(
    key="mem:episodic:01KQ...",     # required
    effort_score=4,                 # required, 1-5
    outcome="succeeded",            # required: 'succeeded', 'pivoted' or 'abandoned'
    iterations=3,                   # default 1
    abandoned_approaches=[          # default None; appended, never replaces
        {"name": "qemu-user-static", "type": "tool",
         "reason": "amd64-only, exec format error on arm64"},
    ],
    breakthrough="tonistiigi/binfmt registers handlers on arm64 hosts",  # default None
    gotchas="needs --privileged on first run",                           # default None
    lesson="check an image's architectures before building on it",       # default None
)

# Add one dead end without re-recording the whole experience. All four are required.
log_abandoned(
    key="mem:episodic:01KQ...",
    name="Alpine base image",
    type="approach",                # 'library', 'approach', 'tool', 'pattern' or 'service'
    reason="no musl build of the runtime",
)

# Make one strong lesson skill-eligible.
bless(memory_key="mem:episodic:01KQ...")   # episodic keys only
```

With `mode="full"` (and no `force`), the result says `"enrichment": "queued"` and the enrichment worker picks it up in the background. That needs `ANTHROPIC_API_KEY`; without it nothing is extracted and the memory stays as it was written.

## How the skill compiler reads this namespace

The compiler treats lowercased tags as domains. From each active memory in a domain it takes:

- `lesson` (or `breakthrough` when there's no lesson) on a `succeeded` outcome → a **do** lesson
- `gotchas` → a **watch** lesson
- each `abandoned_approaches` entry → a **don't** lesson, grouped by approach name
- a blessed memory always contributes: its lesson or breakthrough whatever the outcome, or its bare `content` if it has no structured lesson fields

Do and watch lessons cluster by embedding similarity (`SKILL_CLUSTER_THRESHOLD`, default 0.80). A rule needs `min_reinforcement` distinct source memories (default 2) or a blessing to make it into the skill. The whole story is in [the skill compiler](skill-compiler.md).
