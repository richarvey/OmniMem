# Knowledge Memory Specification

**Key formats**: `mem:knowledge:{16-hex URL hash}` (RSS articles) and `mem:knowledge:{ULID}` (extracted facts, manual writes)
**Created by**: RSS worker (`rss_worker/ingester.py`), enrichment worker (`memory/enrichment.py`), `remember(namespace="knowledge")`
**Index**: `idx:knowledge`

The knowledge namespace is reference material: RSS articles summarised at ingest, discrete facts extracted from conversation memories, and anything stored there directly. Three writers, three field shapes; all share the common core.

## 1. RSS articles

Written by the RSS worker. Keyed by `sha256(article_url)[:16]` for URL-level dedup; digest-mode feeds hash `url + ':' + item_index` so one page can yield several items.

| Field | Format | Required | Description |
|-------|--------|----------|-------------|
| `content` | string | yes | The Claude Haiku summary (or the 800-char truncation fallback). Digest items render as `# title` plus Who/What/Why lines. This is what gets embedded. |
| `title` | string | yes | Article title. **Not** in the search return whitelist, so it is absent from recall results; list views and `recent_knowledge()` fetch it explicitly. |
| `source_url` | string | yes | The article URL. |
| `feed_name` | string | yes | Which feed it came from. Presence of `feed_name` is what identifies a record as an RSS article (the web UI's Articles vs Learned Knowledge split, and expiry). |
| `project` | string | yes | Project label, default `RSS`, overridable per feed via `project:` in feeds.yml. Must satisfy the project-name charset or the ingester falls back to `RSS`. Backfilled onto pre-v6.1.1 articles by a startup migration. |
| `published_at` | unix seconds string | yes | From the feed entry; empty string when the feed gives no date. |
| `topics` | JSON array of strings | yes | The feed's configured topics. |
| `state` | lifecycle state | yes | `active` on creation. |
| `surface_score` | float string | yes | `"1.0"`. |
| `experience_weight` | float string | yes | `"1.0"`. |
| `created_at` / `updated_at` | unix seconds strings | yes | Ingest time. |
| `expires_at` | unix seconds string | yes | `created_at + MAX_KNOWLEDGE_AGE_DAYS` (default 30 days). Auto-maintenance archives articles past this. Only records with **both** `feed_name` and `expires_at` are ever auto-archived; promotion clears it. |
| `vector` | 384-dim float32 blob | yes | Embedding of the summary. |

## 2. Extracted facts (enrichment)

When `INGEST_MODE=full`, `remember()` stores the raw memory and queues it; the enrichment worker extracts discrete facts via Claude and writes each as its own knowledge memory under a ULID key (preference-kind facts go to the preference namespace instead).

| Field | Format | Description |
|-------|--------|-------------|
| `content` | string | The extracted fact text. |
| `state` | lifecycle state | `active`. |
| `surface_score` | float string | **`"0.5"`** — deliberately half, so verbatim source chunks outrank their own facts on direct recall (issue #20). |
| `experience_weight` | float string | `"1.0"`. |
| `created_at` / `updated_at` | unix seconds strings | Extraction time. |
| `tags` | JSON array | Inherited from the source memory's write call. |
| `source_doc_id` | string | The source's `doc_id` if it was a document chunk, else the source key. |
| `enriched_from` | key string | The source memory's key. Recall suppresses a fact when its source memory already made the result cut. |
| `project` | string | Inherited from the source. |
| `event_date` | unix seconds string | Fallback chain: the fact's own extracted date → the source's `event_date` → the source's `created_at`. Keeps temporal queries able to find extracted facts. |
| `vector` | 384-dim float32 blob | Embedding of the fact text. |

Facts are dedup-checked (cosine 0.92) against the target namespace before writing.

## 3. Manual writes

`remember(namespace="knowledge")` stores the standard core fields (`content`, `state`, `surface_score`, `experience_weight`, `created_at`, `updated_at`, `tags`, optional `project`, `vector`) under a ULID key. No `feed_name` and no `expires_at`, so manual knowledge never auto-expires. Knowledge writes are never queued for enrichment (facts extracting facts would recurse).

## Promotion fields (v6.2, added by `promote_knowledge()`)

Promotion marks an article permanently useful and, with a domain, skill-eligible. These fields can appear on any knowledge record:

| Field | Format | Description |
|-------|--------|-------------|
| `expires_at` | set to `""` | Cleared on any promotion, so the article survives maintenance. |
| `skill_domains` | JSON array of strings | Domains this article is promoted to. The next `compile_skill()` for one of them renders the article into the skill's Reference section. Demoting removes the domain from the list. |
| `promoted_at` | unix seconds string | When first promoted to a domain. Used by the briefing's pending-update detection. |
| `skill_rules` | JSON array | Optional rules extracted from the article at promotion time, each `{"kind": "do"\|"watch"\|"dont"\|"note", "text": "..."}`. Max 20 per article, 400 chars each. Rendered one stance-prefixed Reference bullet per rule instead of a single summary line. Extraction happens at promotion under human review, never at compile, so compilation stays deterministic. Re-promote with `rules=[]` to revert to the summary form. |

Promotion substitutes for reinforcement (the same reasoning as `bless()`): promoted references bypass the skill compiler's reinforcement gate but never count toward it, and they render in a separate Reference section, never in Do/Don't. Read is not lived experience.

## Calling the tools

RSS articles and extracted facts are written by their workers, not by tool calls. The callable surface is manual writes and promotion:

```python
# Store knowledge directly. Never expires, never queued for enrichment.
remember(
    content="valkey-search tag filters need raw values, not escaped ones",
    namespace="knowledge",          # routes the write here instead of episodic
    project="omnimem",              # default None
    tags=["valkey"],                # default None
    force=False,                    # default; True skips the dedup check
    mode="raw",                     # knowledge writes are never enriched, so 'raw' and
)                                   # 'full' behave the same here

# Keep an article forever (clears expires_at). Only key is required.
promote_knowledge(key="mem:knowledge:a1b2c3d4e5f60718")

# Also make it skill-eligible: the next compile_skill("python") renders it
# as one summary rule in the Reference section.
promote_knowledge(
    key="mem:knowledge:a1b2c3d4e5f60718",
    domain="python",                # default None — promotion without skill eligibility
    demote=False,                   # default; True removes the domain again (next recompile
)                                   # flags the dropped rule as a high-risk removal)

# Articles with discrete guidance: extract the items at promotion, under review.
# Each becomes its own stance-prefixed Reference bullet. Max 20 rules, 400 chars each.
promote_knowledge(
    key="mem:knowledge:a1b2c3d4e5f60718",
    domain="python",
    rules=[                         # default None; kind: 'do', 'dont', 'watch', or 'note'
        {"kind": "dont", "text": "Never mutate a list while iterating it"},
        {"kind": "do", "text": "Prefer pathlib over os.path for new code"},
    ],
)

# Re-promote with rules=[] to drop the extracted rules and revert to the summary form.
promote_knowledge(key="mem:knowledge:a1b2c3d4e5f60718", domain="python", rules=[])
```

## Indexed fields

`idx:knowledge` indexes: `vector` (HNSW cosine), `feed_name` (tag), `topics` (tag), `state` (tag), `project` (tag), `published_at`, `surface_score`, `created_at`, `updated_at`, `recall_count`, `expires_at` (numeric).

## Search return whitelist

`_NAMESPACE_RETURN_FIELDS["knowledge"]` returns `content`, `source_url`, `feed_name`, `published_at`, `topics`, `state`, `surface_score`, timestamps, recall counters, `expires_at`, `project`, `event_date`, `tags`, and `enriched_from`. Notably absent: `title`, `skill_domains`, `promoted_at`, `skill_rules` — those are fetched by key where needed.
