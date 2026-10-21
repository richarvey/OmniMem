# Knowledge Memory Specification

**Key formats**: `mem:knowledge:{16 hex characters of the URL hash}` (RSS articles) and `mem:knowledge:{ULID}` (extracted facts and manual writes)
**Created by**: the RSS scheduler, the enrichment worker, `remember(namespace="knowledge")`

Knowledge is reference material: RSS articles summarised when they're ingested, discrete facts pulled out of your conversation memories, and anything you store here directly. Three writers, three slightly different shapes, all on the [common fields](memory-types.md#common-fields).

## 1. RSS articles

Written by the RSS scheduler that runs inside `omnimem serve` and the desktop app (or by hand with `omnimem rss`). The key is the first 16 hex characters of `sha256(article_url)`, so the same URL is never ingested twice. Digest-mode feeds hash `url:index`, so one page can yield several items.

| Field | Format | Always present | Description |
|-------|--------|----------------|-------------|
| `content` | string | yes | The Claude Haiku summary, or an 800-character truncation when there's no API key or the summary fails. A digest item is `# title` plus Who/What/Why lines. This is what gets embedded. |
| `title` | string | yes | Article title. |
| `source_url` | string | yes | The article URL. |
| `feed_name` | string | yes | Which feed it came from. Having a `feed_name` is what makes a record an RSS article (the Articles and Learned Knowledge split in the settings panel, and expiry). |
| `project` | string | yes | Project label: `RSS` unless the feed sets `project:` in `feeds.yml`. A label that isn't a valid project name falls back to `RSS`. |
| `published_at` | unix seconds string | yes | From the feed entry; `""` when the feed gives no date. |
| `topics` | JSON array of strings | yes | The feed's configured topics. |
| `state` | lifecycle state | yes | `active`. |
| `surface_score` | float string | yes | `"1.0"`. |
| `experience_weight` | float string | yes | `"1.0"`. |
| `created_at` / `updated_at` | unix seconds strings | yes | Ingest time. |
| `expires_at` | unix seconds string | yes | Ingest time plus `MAX_KNOWLEDGE_AGE_DAYS` (default 30 days). Auto-maintenance archives active articles past it. Only records with both `feed_name` and a non-empty `expires_at` are ever expired, and promotion clears it. |
| `licence` | licence class | yes | What the feed declares with `licence:` in `feeds.yml`, resolved through the alias table (`ogl-3.0` → `open`); `unknown` when it declares nothing. With `RSS_REQUIRE_LICENCE=true`, an undeclared feed is skipped instead. |
| `licence_note` | string | no | The canonical identifier when the feed declared one (`OGL v3.0`), or the feed's own `licence_note:`. |
| `provenance` | `retrieved` | yes | Always `retrieved`. |

## 2. Extracted facts (enrichment)

With `INGEST_MODE=full` and an `ANTHROPIC_API_KEY`, a `remember()` stores the memory as written and queues it. The enrichment worker (a background thread in the same process) asks Claude for discrete facts and writes each one as its own knowledge memory under a ULID key. Facts that classify as preferences go to the [preference namespace](memory-preference.md) instead.

| Field | Format | Description |
|-------|--------|-------------|
| `content` | string | The extracted fact. |
| `state` | lifecycle state | `active`. |
| `surface_score` | float string | **`"0.5"`**: deliberately half, so the verbatim source outranks its own facts. |
| `experience_weight` | float string | `"1.0"`. |
| `created_at` / `updated_at` | unix seconds strings | Extraction time. |
| `tags` | JSON array | The tags from the original write. |
| `source_doc_id` | string | The source's `doc_id` if it was a document chunk, otherwise the source key. |
| `enriched_from` | key string | The source memory's key. Recall drops a fact when its source already made the cut. |
| `project` | string | The source's project, when it had one. |
| `event_date` | unix seconds string | The fact's own date if Claude found one, else the source's `event_date`, else the source's `created_at`, so date-shaped queries still find it. |
| `licence` / `licence_note` | licence class / string | Inherited from the source: a fact has exactly its source's rights. The live source record wins over what the queued job recorded; a source with no licence gives `own`. |
| `provenance` | provenance class | Inherited from the source: extraction is restating, not reasoning. A source with none gives `concluded`. |

Every fact is dedup-checked (similarity 0.92, same project) against its target namespace before it's written. The queue lives in the database, so a job queued before a crash or a restart still runs.

## 3. Manual writes

`remember(namespace="knowledge")` stores the standard core fields (`content`, `state`, `surface_score`, `experience_weight`, timestamps, `tags`, optional `project`, `licence`, `provenance`) under a ULID key. No `feed_name` and no `expires_at`, so manual knowledge never expires. Knowledge writes are never queued for enrichment (facts extracting facts would go round in circles).

`licence` defaults to `unknown` here, because knowledge is where third-party material usually ends up, so pass `licence=` when you know where it came from. `provenance` defaults to `retrieved`.

## Promotion fields (added by `promote_knowledge()`)

Promotion marks an article as worth keeping and, with a domain, feeds it to the skill compiler. These fields can appear on any knowledge record:

| Field | Format | Description |
|-------|--------|-------------|
| `expires_at` | `""` | Cleared by any promotion, so the article survives maintenance. |
| `skill_domains` | JSON array of strings | Domains it's promoted to, sorted. The next `compile_skill()` for one of them renders it in the skill's Reference section. Demoting removes the domain. |
| `promoted_at` | unix seconds string | When it was first promoted to a domain. The briefing uses it to spot skills that need a recompile. |
| `skill_rules` | JSON array | Optional rules pulled out of the article at promotion time, each `{"kind": "do"|"watch"|"dont"|"note", "text": "..."}`. At most 20 per article, 400 characters each. Each renders as its own Reference bullet instead of a single summary line. Extraction happens at promotion, under review, never at compile time, so compiling stays deterministic. Promote again with `rules=[]` to go back to the summary. |

A promoted article can't be archived first: promoting an archived item is refused. Promotion counts as vetting, the same reasoning as `bless()`, so promoted references skip the reinforcement gate but never count towards it, and they render in their own Reference section, never in Do or Don't. Reading something isn't the same as living through it.

## Calling the tools

RSS articles and extracted facts arrive on their own. What you can call is manual writes and promotion:

```python
# Store knowledge directly. Never expires, never enriched.
remember(
    content="SQLite WAL mode lets readers carry on while a write is in progress",
    namespace="knowledge",
    project="omnimem",              # default None
    tags=["sqlite"],                # default None
    licence="open",                 # default 'unknown' in this namespace
)

# Keep an article forever (clears expires_at). Only key is required.
promote_knowledge(key="mem:knowledge:a1b2c3d4e5f60718")

# Also make it skill-eligible: the next compile_skill("python") renders it
# as a summary rule in the Reference section.
promote_knowledge(
    key="mem:knowledge:a1b2c3d4e5f60718",
    domain="python",                # default None: promotion without skill eligibility
    demote=False,                   # default; True removes the domain again
)

# Articles with discrete guidance: pull the rules out at promotion, under review.
promote_knowledge(
    key="mem:knowledge:a1b2c3d4e5f60718",
    domain="python",
    rules=[                         # kind: 'do', 'dont', 'watch' or 'note'
        {"kind": "dont", "text": "Never mutate a list while iterating over it"},
        {"kind": "do", "text": "Prefer pathlib over os.path for new code"},
    ],
)

# The latest articles, for a quick look at what's arrived.
recent_knowledge()
```

Feeds can also influence a skill directly without promoting each article: a feed's `skills:` scores in `feeds.yml` pull its latest articles into the skill's Feed watch section. See [RSS and knowledge](rss-knowledge.md).
