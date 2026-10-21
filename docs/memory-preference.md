# Preference Memory Specification

**Key format**: `mem:preference:{ULID}`
**Created by**: the enrichment worker, `remember(namespace="preference")`

Preferences are rules about how you want to work: "always update the README after a feature", "use British English spelling". The difference from an episodic memory is intent. A preference tells the agent what to do; an episodic memory records what happened. The instructions OmniMem sends every agent when it connects tell it to check preferences first and honour them.

Field formats follow the [storage model](memory-types.md#storage-model).

## Writers

1. **Fact extraction (the usual path)**: with `INGEST_MODE=full` and an `ANTHROPIC_API_KEY`, the enrichment worker classifies each fact it pulls out of a memory, and facts of kind `preference` land here instead of in knowledge. That's how "always run the linter before committing", said in passing, becomes a standing rule.
2. **Direct writes**: `remember(namespace="preference")` stores a preference as written.

## Fields

| Field | Format | Always present | Description |
|-------|--------|----------------|-------------|
| `content` | string, max 50,000 characters | yes | The preference. Embedded as it is. |
| `state` | lifecycle state | yes | `active`. |
| `surface_score` | float string | yes | `"1.0"` for a direct write; **`"0.5"`** for an extracted preference, so the verbatim source outranks what was pulled out of it. |
| `experience_weight` | float string | yes | `"1.0"`. |
| `created_at` / `updated_at` | unix seconds strings | yes | Standard timestamps. |
| `tags` | JSON array of strings | yes | `"[]"` if none. Extracted preferences carry the source write's tags. |
| `project` | string | no | Project scope, when the preference is project-specific. |
| `scope` | `project` \| `global` | no | Extracted preferences only: `project` when the source had a project, `global` otherwise. Direct writes don't set it. |
| `source_doc_id` | string | no | Extracted only: the source's `doc_id`, or its key. |
| `enriched_from` | key string | no | Extracted only: the source memory's key. Recall drops the preference when its source already made the cut. |
| `event_date` | unix seconds string | no | Extracted only: the fact's own date, else the source's `event_date`, else the source's `created_at`. |
| `recall_count` / `last_recalled` | int string / unix seconds string | no | Recall counters. |
| `licence` / `licence_note` | licence class / string | yes / no | `own` for a direct write. An extracted preference inherits its source's. |
| `provenance` | provenance class | yes | `asserted` for a direct write, since a preference is the human saying how they want things. An extracted preference inherits its source's, so one pulled out of an episodic write-up is `concluded` and one the human dictated stays `asserted`. |

Extracted preferences are dedup-checked (similarity 0.92, same project) before they're written, so remembering similar conversations over and over doesn't pile up copies.

## Calling the tools

```python
# Store a preference as written (surface_score 1.0, no scope field).
remember(
    content="Always update the README in the same change as a new feature",
    namespace="preference",
    project="omnimem",              # default None: omit for a global preference
    tags=["workflow"],              # default None
    mode="raw",                     # 'raw' stores it as written; 'full' (the INGEST_MODE
)                                   # default) also queues it for fact extraction
```

The extraction path needs no call at all: with `INGEST_MODE=full`, any `remember()` whose facts classify as preferences puts those facts here, at `surface_score` 0.5 with `scope`, `enriched_from` and `event_date` set.

## Where preferences surface

- `recall()` searches this namespace by default, alongside episodic, project and knowledge.
- The settings panel's Preferences page is a filtered view of the memories list.
- Every compiled skill carries a fixed operating contract telling the agent to check OmniMem preferences first. Preferences steer every session that loads a skill, but they aren't compiled into skill bodies.
