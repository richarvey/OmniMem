# Project Memory Specification

**Key formats**: `mem:project:{project_name}` (the context entry) and `mem:project:{ULID}` (a project-scoped memory)
**Created by**: `set_project_context()`, `compile_project_context(auto_save=True)`, `remember(namespace="project")`

The project namespace holds two different record shapes under the same prefix. It's a known wrinkle rather than a design I'd pick again: `list_projects()` and the settings panel de-duplicate by resolved name, and treat a record as a context entry when it has `goals` or `stack`, and as a plain memory otherwise.

Field formats follow the [storage model](memory-types.md#storage-model).

## 1. Project context entries

One per project, keyed by name: `mem:project:omnimem`. `set_project_context()` creates or replaces it. `update_project_state()` patches `current_state` and `notes` without re-embedding. `compile_project_context(auto_save=True)` regenerates the entry from the project's own memories.

| Field | Format | Always present | Description |
|-------|--------|----------------|-------------|
| `content` | string | yes | A copy of `description`, so anything that reads `content` works. |
| `project_name` | string | yes | The project identifier: 1-200 characters of letters, digits, hyphens, underscores, dots and spaces. |
| `description` | string | yes | What the project does. |
| `stack` | string | yes | Technology stack, free text. |
| `goals` | string | yes | Current objectives. This (or `stack`) is what marks a record as a context entry. |
| `current_state` | string | yes | Where the project is now. |
| `notes` | string | no | Notes for the next session. The compiler fills it with breakthroughs, gotchas and abandoned approaches. |
| `domains` | comma-separated string | no | Work-type domains: the kinds of work inside the project (`python,docker,wcag-accessibility`), normalised through the same rules and aliases as skill domains so the two can't drift apart. At most 20. On start, projects that have never had the field get it seeded from `stack`. An empty string means "considered, nothing derivable", which isn't the same as the field being absent. |
| `state` | lifecycle state | yes | `active`. |
| `surface_score` | float string | yes | `"1.0"`. |
| `created_at` / `updated_at` | unix seconds strings | yes | Standard timestamps. |
| `recall_count` / `last_recalled` | int string / unix seconds string | no | Recall counters. |
| `licence` | `own` | yes | Always `own`: a context entry is written about your own work. |
| `provenance` | provenance class | yes | `asserted` from `set_project_context()`. An auto-saved compiled draft is `concluded` until someone vouches for it, and a recompile keeps whatever the entry already had, so a human's `asserted` survives it. |

The vector embeds `"{description} {goals} {current_state}"`, not `content`. `update_project_state()` doesn't re-embed, so the vector can lag behind `current_state` until the next full save.

`set_project_context()` without a `domains` argument keeps the domains already stored; passing `[]` clears them.

## 2. Project-scoped ULID memories

`remember(namespace="project")` stores an ordinary memory under a ULID key, with the same core fields as an [episodic memory](memory-episodic.md#core-written-by-remember) (`content`, `state`, `surface_score`, `experience_weight`, timestamps, `tags`, `licence`, `provenance`) plus:

| Field | Format | Description |
|-------|--------|-------------|
| `project` | string | The project scope, as on the other namespaces. |
| `project_name` | string | The same value again, so project views (which key on `project_name`) see the memory straight away. |

These default to `own` and `concluded` like any other write-up.

## Calling the tools

```python
# Create or replace a project's context entry.
set_project_context(
    project_name="omnimem",         # required
    description="Self-hosted semantic memory for AI agents",   # required
    stack="Rust, SQLite, ONNX Runtime",                        # required
    goals="Ship 7.0",                                          # required
    current_state="phase 8 done, docs next",                   # required
    notes="Recompile the rust skill after the next round of fixes",  # default None
    domains=["rust", "sqlite"],     # default None keeps existing; [] clears
)

# Patch state and notes without re-embedding.
update_project_state(
    project_name="omnimem",         # required
    current_state="Docs rewritten",  # required
    notes="Installers next",         # default None
)

# Draft (or refresh) the context entry from the project's memories.
compile_project_context(project_name="omnimem", auto_save=False)  # False returns the draft

# The whole context entry, description, goals, current_state and notes included.
get_project_context(project_name="omnimem")

# A project-scoped ULID memory (gets both project and project_name).
remember(content="The settings panel needs a context entry to link a project",
         namespace="project", project="omnimem", tags=["settings-panel"])

# Bulk lifecycle over every memory in the project. confirm=False previews with counts;
# include_context=False leaves the mem:project:{name} entry alone.
delete_project(project_name="omnimem", confirm=True, include_context=False)
deprioritise_project(project_name="omnimem", confirm=True, reason="Parked until autumn",
                     include_context=False)
reinstate_project(project_name="omnimem", confirm=True, include_context=False)

# Every known project, de-duplicated by name; or only those declaring a domain.
list_projects()
list_projects(domain="python")      # aliases resolve: py -> python

# Suggest domains from the stack and the project's recurring tags, with the evidence.
compile_project_domains(project_name="omnimem", auto_save=False)
```

## Work-type domains and cross-project recall

Domains are what turn a pile of separate projects into something you can search by the *kind* of work rather than by where it happened. Declare them on the context entry and `recall(domain_filter="python")` searches every Python project at once:

```python
# Every gotcha from every Python project, not just this one.
recall("pydantic validator ordering", domain_filter="python")

# Both filters intersect: this project, and only if it's a Python project.
recall("pydantic validator ordering", project_filter="omnimem", domain_filter="python")
```

Three things worth knowing:

- **The vocabulary is shared with compiled skills.** A project domain and a skill domain normalise the same way, so the same name reaches `find_skills("python")` and `get_skill()`. Skills hold the lessons that cleared the reinforcement gate; the domain filter reaches the raw memories underneath, including the ones that never made it into a skill.
- **A domain nobody declares doesn't quietly return everything.** If no project declares `rust`, the recall runs unscoped and puts a `domain_filter_notice` first saying so. When a domain *and* a project filter are both given and nothing satisfies both, you get nothing plus the same notice, never a widened search.
- **Domains route, they don't label memories.** A memory isn't stamped with its project's domains. A project can be Python and Docker and CSS at once, and inheriting all three onto every memory would surface a CSS gotcha in a Python search. The domain narrows the candidate *projects*; the vector search still decides what's relevant inside them.

## Behavioural notes

- A recall result for a context entry carries the searchable summary (`content`, `project_name`, `stack`, `domains`, state and counters). `description`, `goals`, `current_state` and `notes` come back from `get_project_context()`.
- Projects that only exist as ULID memories (nobody has called `set_project_context()` yet) have no context entry, so the settings panel can't open a detail page for them until one exists.
- `delete_project()` and the bulk deprioritise and reinstate tools match memories in every namespace on `project` or `project_name`, and leave the context entry alone unless `include_context=True`.
- The RSS `project` label (default `RSS`) doesn't create a pseudo-project: project pages and tools only count `mem:project:*` keys.
