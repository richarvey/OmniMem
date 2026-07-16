# Project Memory Specification

**Key formats**: `mem:project:{project_name}` (context entry) and `mem:project:{ULID}` (project-scoped memory)
**Created by**: `set_project_context()`, `compile_project_context(auto_save=True)`, `remember(namespace="project")`
**Index**: `idx:project`

The project namespace holds two distinct record shapes under the same prefix. This is a known wrinkle: `list_projects()` and the web UI deduplicate by resolved name, treating a record as a context entry when it has `goals` or `stack` and as a plain memory otherwise.

## 1. Project context entries

One per project, keyed by name: `mem:project:omnimem`. Created and fully replaced by `set_project_context()`; `update_project_state()` patches `current_state` and `notes` without re-embedding; `compile_project_context(auto_save=True)` regenerates the whole entry from the project's episodic memories.

| Field | Format | Required | Description |
|-------|--------|----------|-------------|
| `content` | string | yes | Mirror of `description`, so generic content readers work. |
| `project_name` | string | yes | The project identifier. 1-200 chars: alphanumeric, hyphens, underscores, dots, spaces. |
| `description` | string | yes | What the project does. |
| `stack` | string | yes | Technology stack (free text; the compiler drafts it from top tags). |
| `goals` | string | yes | Current objectives. Presence of this (or `stack`) is what marks the record as a context entry. |
| `current_state` | string | yes | Where the project is now. Updated by `update_project_state()` without touching the vector. |
| `notes` | string | no | Freeform notes for the next session. The compiler fills this with breakthroughs, gotchas, and abandoned approaches. |
| `state` | lifecycle state | yes | `active` on creation. |
| `surface_score` | float string | yes | `"1.0"` on creation. |
| `created_at` / `updated_at` | unix seconds strings | yes | Standard timestamps. |
| `recall_count` / `last_recalled` | int string / unix seconds string | no | Standard recall counters. |
| `vector` | 384-dim float32 blob | yes | Embeds `"{description} {goals} {current_state}"`, not `content`. `update_project_state()` deliberately skips re-embedding, so the vector can lag the stored `current_state` until the next full save. |

## 2. Project-scoped ULID memories

`remember(namespace="project")` stores an ordinary memory under a ULID key with the standard episodic-style core fields (`content`, `state`, `surface_score`, `experience_weight`, `created_at`, `updated_at`, `tags`, `vector`) plus:

| Field | Format | Description |
|-------|--------|-------------|
| `project` | string | The project scope, as on other namespaces. |
| `project_name` | string | Set to the same value at write time so the project index and UI (which key on `project_name`) see the memory immediately. A startup migration backfills older records that only carried `project`. |

## Calling the tools

```python
# Create or fully replace a project's context entry. Everything except notes is required.
set_project_context(
    project_name="omnimem",         # 1-200 chars: alphanumeric, hyphens, underscores, dots, spaces
    description="Self-hosted semantic memory MCP server for Claude Code",
    stack="Python 3.12, FastMCP, Valkey, sentence-transformers",
    goals="Ship v6.3.1; keep coverage above 90%",
    current_state="v6 branch, skills compiler and web UI login shipped",
    notes="Recompile the python skill after the next round of fixes",  # default None
)

# Patch state and notes without re-embedding (the vector lags until the next full save).
update_project_state(
    project_name="omnimem",         # required
    current_state="Changelog updated for 6.3.1",  # required
    notes="Docs examples still to review",        # default None
)

# Draft (or refresh) the context entry from the project's episodic memories.
compile_project_context(
    project_name="omnimem",         # required
    auto_save=False,                # default — returns the draft for review;
)                                   # True saves it straight to mem:project:{name}

# Fetch the full context entry (description, goals, current_state, notes included —
# vector search only returns the whitelisted subset).
get_project_context(project_name="omnimem")

# Store a project-scoped ULID memory (gets both project and project_name fields).
remember(
    content="The web UI detail view needs a context entry to enable links",
    namespace="project",
    project="omnimem",
    tags=["web-ui"],
)

# Bulk lifecycle over every memory matching the project. All three share the same shape:
# confirm=False (default) returns a preview with counts; include_context=False (default)
# leaves the mem:project:{name} context entry untouched.
delete_project(project_name="omnimem", confirm=True, include_context=False)
deprioritise_project(
    project_name="omnimem",
    confirm=True,
    reason="Parked until autumn",   # default None; delete_project has no reason arg
    include_context=False,
)
reinstate_project(project_name="omnimem", confirm=True, include_context=False)

# List every known project (context entries and ULID-only projects, deduplicated by name).
list_projects()
```

## Indexed fields

`idx:project` indexes: `vector` (HNSW cosine), `project_name` (tag), `stack` (tag), `state` (tag), `surface_score`, `created_at`, `updated_at`, `recall_count` (numeric).

## Behavioural notes

- **Search returns** (`_NAMESPACE_RETURN_FIELDS["project"]`) include only `content`, `project_name`, `stack`, `state`, `surface_score`, timestamps, and recall counters. `description`, `goals`, `current_state`, and `notes` come back via `get_project_context()`, not vector search.
- Projects that only exist as ULID memories (no `set_project_context()` call yet) have no context entry; the web UI detail view disables links for them until one is created.
- `delete_project()` and the bulk deprioritise/reinstate tools match memories across all namespaces on `project` or `project_name`, and leave the context entry alone unless `include_context=True`.
- The RSS `project` label (default `RSS`) does not create a pseudo-project: project pages and tools only count `mem:project:*` keys.
