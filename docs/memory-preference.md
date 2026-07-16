# Preference Memory Specification

**Key format**: `mem:preference:{ULID}`
**Created by**: enrichment worker (`memory/enrichment.py`), `remember(namespace="preference")`
**Index**: `idx:preference`

Preferences are prescriptive rules about how to work: "always update the README after a feature", "use British English spelling". They differ from episodic memories in intent — a preference tells the agent what to do, an episodic memory records what happened. The on-connect MCP instructions tell agents to check preferences first and honour them.

## Writers

1. **Fact extraction (the usual path)**: when `INGEST_MODE=full`, the enrichment worker classifies each extracted fact; facts with `kind == "preference"` are routed here instead of the knowledge namespace. This is how conversational statements like "always run the linter before committing" become standing rules.
2. **Direct writes**: `remember(namespace="preference")` stores a preference verbatim.

## Fields

| Field | Format | Required | Description |
|-------|--------|----------|-------------|
| `content` | string, max 50,000 chars | yes | The preference text. Embedded to produce `vector`. |
| `state` | lifecycle state | yes | `active` on creation. |
| `surface_score` | float string | yes | `"1.0"` for direct writes; **`"0.5"`** for extracted preferences, matching the enrichment convention that verbatim sources outrank their own extractions. |
| `experience_weight` | float string | yes | `"1.0"`. |
| `created_at` / `updated_at` | unix seconds strings | yes | Standard timestamps. |
| `tags` | JSON array of strings | yes | `"[]"` if none. |
| `project` | string | no | Project scope, when the preference is project-specific. |
| `scope` | `project` \| `global` | no | Set by the enrichment worker only: `project` when the source memory carried a project, `global` otherwise. Direct `remember()` writes do not set it. |
| `source_doc_id` | string | no | Enrichment only: the source's `doc_id` or key. |
| `enriched_from` | key string | no | Enrichment only: the source memory's key. Recall suppresses the preference when its source memory already made the result cut. |
| `event_date` | unix seconds string | no | Enrichment only: the fact's own date, else the source's `event_date`, else the source's `created_at`. |
| `recall_count` / `last_recalled` | int string / unix seconds string | no | Standard recall counters. |
| `vector` | 384-dim float32 blob | yes | Embedding of `content`. |

Extracted preferences are dedup-checked (cosine 0.92, project-scoped) before writing, so re-remembering similar conversations does not pile up duplicates.

## Indexed fields

`idx:preference` indexes: `vector` (HNSW cosine), `project` (tag), `scope` (tag), `state` (tag), `tags` (tag), `surface_score`, `created_at`, `updated_at`, `recall_count` (numeric).

## Search return whitelist

`_NAMESPACE_RETURN_FIELDS["preference"]` returns `content`, `project`, `scope`, `state`, `surface_score`, timestamps, `tags`, recall counters, `source_doc_id`, `event_date`, and `enriched_from`.

## Where preferences surface

- `recall()` searches the namespace by default alongside the others.
- The web UI's Preferences page is a filtered `/memories` view over this namespace.
- Compiled skills carry a fixed operating contract instructing agents to check OmniMem preferences first — preferences are policy input to every skill session, but they are not compiled into skill bodies.
