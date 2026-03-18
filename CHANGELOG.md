# Changelog
All notable changes to omnimem are documented here.
Format: [version] - date - description

## [Unreleased]

## [3.3.3] - 2026-03-18
### Changed
- **Responsive memories table**: Fixed-width table layout with proportional columns (10/44/12/16/18%) and ellipsis truncation on content and project columns, matching the projects page pattern
- **Split date display**: Updated column shows human-readable date ("18 Mar 2026") with time underneath in smaller mono font, replacing the single-line ISO-style timestamp

## [3.3.2] - 2026-03-18
### Fixed
- **Duplicate detection page rendered twice on scan**: The htmx scan endpoint returned the full page template (header, nav, form, and results) instead of just the results partial, causing a page-within-a-page effect. Extracted results into `partials/dup_results.html` and the scan endpoint now returns only the partial

## [3.3.1] - 2026-03-18
### Added
- **Delete backups from web UI**: Trash bin icon button in the Actions column of the backups table permanently deletes the backup file from disk with a confirmation prompt before deletion
- Path traversal protection on the delete endpoint

## [3.3.0] - 2026-03-18
### Added
- **RSS Feeds management in web UI**: New `/feeds` page to view, add, edit, and delete RSS feed subscriptions directly from the browser — changes are written to `feeds.yml` in real time
- **feeds.yml file watcher in RSS worker**: Background thread polls `feeds.yml` for changes (default every 10s, configurable via `FEEDS_WATCH_INTERVAL`) and triggers re-ingestion when the file is modified by the web UI or externally
- **Sidebar nav link**: "RSS Feeds" added to the navigation between Suppressions and Backups
### Changed
- **docker-compose**: `feeds.yml` is now mounted read-write in both `rss_worker` and `web_ui` containers (was read-only in rss_worker, unmounted in web_ui)
- **web_ui requirements**: Added `pyyaml` dependency for YAML read/write

## [3.2.13] - 2026-03-18
### Changed
- **Projects table responsive layout**: Fixed-width table with proportional columns (20/32/32/16%) so Description and Current State shrink gracefully with ellipsis truncation
- **Name + badge inline**: Project name and memory count badge now always stay on one line, even for long project names
- **Clearer date display**: Updated column shows human-readable date ("18 Mar 2026") with time underneath in smaller mono font, replacing the raw ISO-style timestamp
- **Moved inline styles to CSS classes**: Project name, memory count badge, and date cells use proper CSS classes instead of inline styles

## [3.2.12] - 2026-03-18
### Fixed
- **UID-only project names in web UI**: Projects created via `remember(namespace="project")` appeared as raw ULIDs (e.g. `01KKHC8WYX7R1SQQT5DGA7619S`) instead of their actual project name. Root cause: `list_projects()` fell back to the key suffix when `project_name` field was missing
- **Duplicate project entries**: Multiple memories for the same project each appeared as separate rows in the projects list
### Added
- **Startup migration**: Automatically sets `project_name` from `project` field on ULID-keyed project memories missing it
- **Project deduplication**: `list_projects()` (MCP and web UI) now groups entries by resolved project name, showing one row per project with a memory count badge
- **Memory count badges**: Projects list template shows how many project-namespace memories exist per project

## [3.2.11] - 2026-03-18
### Fixed
- **Footer confined to sidebar**: Moved footer links (Codeberg, omnimem.org, Mastodon) out of the sidebar nav into a full-width fixed bar at the bottom of the page with centered layout and dot separators

## [3.2.10] - 2026-03-18
### Fixed
- **Web UI backup creation failed with "No module named 'tools'"**: The `create_backup` route imported `__version__` from the `tools` package which is not available in the web UI container. Changed to import from `memory.version` (the shared single source of truth added in v3.2.9)

## [3.2.9] - 2026-03-18
### Added
- **Sidebar footer**: Links to Codeberg repo, omnimem.org, and Mastodon with `rel="me"` verification
### Changed
- **Dynamic version badge**: Sidebar now shows full version number (e.g. v3.2.9) instead of hardcoded "v3"
- **Version single source of truth**: Moved `__version__` to `memory/version.py`, imported by both MCP server and web UI

## [3.2.8] - 2026-03-18
### Improved
- **CLAUDE.md experience recording**: Bug fixes must now always be recorded with structured symptom/cause/fix descriptions, improving recall of prior fixes when similar issues appear in future sessions

## [3.2.7] - 2026-03-18
### Fixed
- **Web UI form parsing broken**: Deprioritise (and other lifecycle POST actions) returned 500 because `python-multipart` was not listed in `web_ui/requirements.txt`. Starlette requires it for `request.form()` parsing

## [3.2.6] - 2026-03-18
### Fixed
- **Web UI missing `ulid-py` dependency**: The create memory route imports `ulid` but the package was not listed in `web_ui/requirements.txt`, causing an `ImportError` on startup

## [3.2.5] - 2026-03-18
### Added
- **Backup management**: `/backups` with list, create, preview restore, and confirm restore
- Shared backups volume between MCP server and web UI

## [3.2.4] - 2026-03-18
### Added
- **Topic suppression management**: `/suppressions` with inline add/remove via htmx

## [3.2.3] - 2026-03-18
### Added
- **Contradiction viewer**: `/contradictions` with side-by-side comparison and archive actions

## [3.2.2] - 2026-03-18
### Added
- **Duplicate detection**: `/duplicates` with scan trigger, cluster display, and archive actions

## [3.2.1] - 2026-03-18
### Added
- **Experience tracking dashboard**: `/experience` with summary stats, breakthroughs, most effortful memories
- **Abandoned approach graveyard**: `/experience/graveyard` listing all dead-end approaches

## [3.2.0] - 2026-03-18
### Added
- **Project management**: `/projects` with list, detail, edit, and create views
- Full project context editing with re-embedding on save

## [3.1.5] - 2026-03-18
### Added
- **Create memory form**: `/create` with content, namespace, project, tags, and force-save option
- Inline duplicate detection shows near-match with similarity score before saving

## [3.1.4] - 2026-03-18
### Added
- **Lifecycle management**: POST endpoints for deprioritise, archive, reinstate, and delete
- Confirmation modal for delete actions
- Flash/redirect flow after lifecycle transitions

## [3.1.3] - 2026-03-18
### Added
- **Memory detail view**: `/memory/{key}` shows full content, metadata, tags, experience data, contradictions, abandoned approaches
- Lifecycle action buttons (deprioritise, archive, reinstate, delete with confirmation modal)

## [3.1.2] - 2026-03-18
### Added
- **Semantic search page**: `/search` with full recall pipeline integration
- Search form with namespace, project filter, and top_k controls
- Results show scores, abandoned warnings highlighted, reinstate candidates flagged

## [3.1.1] - 2026-03-18
### Added
- **Browse memories**: `/memories` page with namespace, state, and project filters
- htmx-powered filter dropdowns update results without full page reload
- Paginated results (25 per page) with sort by newest/oldest

## [3.1.0] - 2026-03-18
### Added
- **Web UI service**: Browser-based management dashboard for OmniMem (Starlette + htmx + Jinja2)
- Dashboard page with namespace stats, state counts, health indicators, and recent activity
- Docker Compose `web_ui` service with shared `mcp_server/memory/` package
- Vendored htmx 2.0.4 (no CDN dependency)
- Reverse proxy authentication docs (Traefik + Caddy)

## [3.0.1] - 2026-03-18
### Added
- `version()` MCP tool returns the current OmniMem version string

## [3.0.0] - 2026-03-18
### Added
- Start of v3.x branch: browser-based management UI for OmniMem

## [2.2.0] - 2026-03-18
### Changed
- **Removed redundant `count` fields**: 15+ responses that returned both an array and a count of that array now return only the array. Affects deprioritise, archive, reinstate, forget, list_suppressions, recall_index, find_duplicates, list_projects, list_backups, check_contradictions, and all briefing subsections.
- **Removed constant `status` fields**: Dropped `"status": "stored"`, `"status": "recorded"`, `"status": "saved"`, `"status": "updated"`, `"status": "suppressed"`, `"status": "active"`, `"status": "success"`, and `"status": "complete"` from responses where the value never varies. Error and branch statuses (`duplicate_found`, `not_found`, `preview`, `deleted`) remain.
- **Removed `stack` from briefing response**: The full tech stack string was included in every session-start briefing. It rarely changes and is available via `get_project_context` when needed.
- **Standardised content truncation to 80 chars**: All content previews (briefing, audit, forget preview, contradictions, project list, experience summary) now truncate consistently at 80 characters instead of the previous mix of 100, 150, and 200.

## [2.1.1] - 2026-03-17
### Fixed
- **Negative similarity scores broke ranking**: Raw cosine similarity scores were not clamped, so distances > 1.0 produced negative scores. Multiplying a negative score by surface_score (0.2) moved deprioritised memories *closer* to zero (ranking them higher, not lower). Fixed by clamping raw scores to `[0, 1]`.

## [2.1.0] - 2026-03-17
### Added
- **Progressive disclosure for recall**: New `recall_index` tool returns compact summaries (key, snippet, score, estimated token count) instead of full content. Defaults to 10 results. New `recall_detail` tool fetches full content for selected keys only. Together they let the agent decide which memories are worth the context budget before committing tokens to full content.
- Token estimates in `recall_index` response: `token_estimate.index` vs `token_estimate.full` shows tokens saved by using the two-step flow.

## [2.0.0] - 2026-03-17
### Changed
- **Tool descriptions cut by 53%**: All docstrings rewritten for conciseness. These ship with every API call.
- **Response payloads compacted**: New `_compact()` helper strips None/empty values. Recall results return only populated fields. Redundant narrative messages removed.
- **217 lines removed** across the tool layer (17% code reduction).

## [0.2.2] - 2026-03-17
### Fixed
- **RSS summariser retry on transient errors**: Haiku API calls now retry up to 2 times on timeouts, rate limits, server errors, and connection errors with backoff, instead of immediately falling back to truncation.
- **Better fallback summaries**: Fallback truncation limit increased from 300 to 800 characters so articles that miss summarisation are still useful.
- **Summariser error logging**: Log the full error message on API failures, not just the exception type.

## [0.2.1] - 2026-03-17
### Fixed
- **RSS scheduler misses on sleeping machines**: APScheduler's default `misfire_grace_time` of 1 second meant every scheduled ingestion was silently skipped when the host machine was asleep at fire time. Set `misfire_grace_time` to the full interval window and `coalesce=True` so missed jobs execute on wake (once, not per missed cycle).

## [0.2.0] - 2026-03-10
### Added
- **Semantic deduplication**: `remember()` now checks for near-identical memories before storing (threshold configurable via `DEDUP_SIMILARITY_THRESHOLD`, default 0.92). Use `force=True` to override. New `find_duplicates` tool scans a namespace and returns clusters of duplicate memories using pairwise cosine similarity with union-find clustering.
- **Contradiction detection**: Two-tier system — Tier 1 (fast keyword heuristic with negation pattern matching) runs automatically on every `remember()` call and warns about potential contradictions. Tier 2 (Claude API analysis) available on-demand via `check_contradictions` tool. Contradictions are cross-linked between memories and surfaced in `recall()` results.
- **Briefing tool**: Single-call `briefing(project="...")` replaces the previous 3-step session start. Aggregates project context, experience summary, stale memories (configurable via `STALE_MEMORY_DAYS`), new knowledge articles, contradiction warnings, reinstate candidates, and suppressed topics.
- New env vars: `DEDUP_SIMILARITY_THRESHOLD`, `CONTRADICTION_SIMILARITY_THRESHOLD`, `STALE_MEMORY_DAYS`
- `contradictions` field added to episodic memory return fields, RecallResult, and explain_memory output
- Updated CLAUDE.md to use briefing tool for session start workflow

## [0.1.0] - 2026-03-09
### Added
- Valkey + valkey-search vector store with HNSW indexing
- Three memory namespaces: episodic, project, knowledge
- Memory lifecycle state machine: active -> deprioritised -> archived -> deleted
- Surface score weighting with topic suppression
- Reinstate hints for graceful memory retirement
- Recall pipeline: semantic search + surface score + recency decay + experience weight
- Experience scoring: effort_score (1-5), outcome, iterations, abandoned_approaches, breakthrough, gotchas
- Abandoned approach fast-path — warnings bypass normal scoring
- High-effort lifecycle guard — warns before deprioritising battle-hardened memories
- Core MCP tools: remember, recall, deprioritise, archive, reinstate, forget, suppress_topic
- Project context tools: set_project_context, get_project_context, list_projects, update_project_state
- Experience tools: record_experience, log_abandoned, get_experience, experience_summary, warn_if_abandoned
- Audit tools: memory_audit, why_did_you_mention, explain_memory
- Backup tools: dump_to_file, restore_from_file, list_backups
- RSS ingestion worker with dedup, scheduling, and Claude-powered summarisation
- Slim Debian-based Docker images (torch CPU-only for embedding support)
- CLAUDE.md integration file for drop-in project wiring
