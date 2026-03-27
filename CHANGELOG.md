# Changelog
All notable changes to omnimem are documented here.
Format: [version] - date - description

## [Unreleased]

## [3.10.0] - 2026-03-27
### Added
- **Streamable HTTP transport support**: MCP server now supports Streamable HTTP via `MCP_TRANSPORT=http` env var, using FastMCP's canonical `"http"` transport string. Endpoint moves from `/sse` to `/mcp` when enabled. Community contribution from [@timstoop](https://codeberg.org/timstoop) ([PR #4](https://codeberg.org/ric_harvey/omnimem/pulls/4))
- **`MCP_TRANSPORT` env var**: Controls which transport the MCP server uses. Accepts `sse` (default) or `http`. Existing deployments continue to work without changes
### Deprecated
- **SSE transport**: SSE remains the default in 3.10 but will be removed in a future release. A deprecation warning is logged on startup when using SSE. All connection guides updated with migration instructions
### Fixed
- **Bandit B310 security finding** (v3.9.5): Validate URL scheme is `http` or `https` before calling `urllib.request.urlopen` in the RSS worker page fetcher. Added `# nosec B310` annotation for the scheme-validated call
- **Version string out of sync** (v3.9.4): `__version__` in `memory/version.py` was stuck on `3.9.3` despite commit messages referencing `3.9.4`
- **Trailing code fence in reverse-proxy docs**: Stray ``` removed from `docs/reverse-proxy.md`
### Changed
- All connection guides (Claude Code, GitHub Copilot, GitLab Duo, Cursor, Kiro, OpenCode, Codex) updated to show SSE as default config with GFM deprecation warnings and Streamable HTTP migration instructions
- Kiro guide type field corrected from `"streamable-http"` to `"http"` for consistency with FastMCP docs
- Docker Hub guide `.env` example shows `MCP_TRANSPORT` as a commented-out option

## [3.9.4] - 2026-03-27
### Changed
- **Proactive memory storage in MCP instructions**: Agents are now explicitly told to store memories proactively without waiting to be asked, with clear namespace guidance (episodic for decisions/work, knowledge for facts/rules, project for scoped context)
- **"Primary persistent memory store" framing**: Header updated across all instruction sources to reinforce OmniMem as the first place to read from and write to

## [3.9.3] - 2026-03-27
### Added
- **Digest mode toggle in web UI**: RSS feed create/edit forms now have a "Digest mode" checkbox that writes `mode: digest` to feeds.yml when enabled. The feeds list page shows a mode column with a badge for digest feeds
- **Tool Priority in MCP instructions**: Agents connecting to OmniMem are now instructed to query `recall()` or `recall_index()` before falling back to web search or training data, making the knowledge base more useful

## [3.9.2] - 2026-03-27
### Fixed
- **RSS worker crash on large digest batches**: `libgomp: Thread creation failed` when digest mode produced many items (e.g. 160+). Added `OMP_NUM_THREADS=1` and `TOKENIZERS_PARALLELISM=false` to rss_worker Docker environment to prevent OpenMP thread explosion
- **All-or-nothing data loss on embedding failure**: Embedding and storing now happens in chunks of 32. If one chunk fails, earlier chunks are already persisted in Valkey instead of losing everything
### Added
- **`RSS_MAX_DIGEST_ENTRIES` env var** (default 2): Limits how many feed entries are processed in digest mode, preventing the worker from churning through an entire backlog of newsletters on first run

## [3.9.1] - 2026-03-27
### Added
- **Digest mode for newsletter-style feeds**: New `mode: digest` option in `feeds.yml` extracts individual items from multi-topic articles (newsletters, roundups, digests) and stores each as a separate knowledge memory with structured who/what/why fields. Single-topic articles produce one item. Opt-in per feed — default remains `mode: summary`
- **Full page content fetching**: When RSS feed content is too short (teasers, paywalls), digest mode automatically fetches the full article page via HTTP and extracts plain text for processing
- `summariser.py`: New `extract_items()` function asks Claude Haiku to return a JSON array of `{title, who, what, why}` objects from the article content
- `ingester.py`: New helpers `_fetch_page_content()`, `_format_item()`, `_get_entry_content()`, and `_process_digest_entry()` for the digest pipeline

### feeds.yml example
```yaml
- url: https://selfh.st/rss/
  name: Self-Host Weekly
  topics: [selfhosted, homelab]
  mode: digest  # extracts individual items from each newsletter issue
```

## [3.9.0] - 2026-03-27
### Fixed
- **RSS worker stores refusal responses as knowledge memories**: When Claude Haiku can't summarise an article (e.g. responds with "I don't have access to external URLs..."), the useless refusal text was stored verbatim in the knowledge base. Now detected and skipped with a warning log instead
### Changed
- `summariser.py`: `summarise()` returns `None` when the model refuses to summarise, detected via `_is_refusal()` helper that checks for common refusal phrases
- `ingester.py`: Articles where `summarise()` returns `None` are counted as skipped, not stored in Valkey

## [3.8.2] - 2026-03-25
### Added
- **Multi-arch Docker images**: All three images (omnimem-mcp, omnimem-web, omnimem-rss) now build for both `linux/amd64` and `linux/arm64` via docker buildx on a self-hosted runner. Docker Hub serves the correct architecture automatically
- **Manual workflow trigger**: Docker build workflow supports `workflow_dispatch` for on-demand builds without creating tags
- **Test coverage to 76%**: 92 new tests across 5 new test files covering project context tools, briefing helpers, audit tools, backup/restore, and contradiction detection tool
### Fixed
- **pip-audit CVE-2026-4539 false positive**: Added `--ignore-vuln` for unpatched low-severity pygments ReDoS (no fix available from upstream)
- **Coverage badge publish failing**: Untracked `coverage-badge.svg` blocked `git checkout badges`. Now removed before branch switch
- **Security scans not triggering on tags**: Added explicit `tags-ignore` to prevent security workflow running on version tag pushes
### Changed
- **Docker workflow migrated from buildah to docker buildx**: Switched from single-arch buildah builds on Codeberg hosted runners to multi-platform docker buildx builds on a self-hosted arm64 runner with QEMU for amd64 cross-compilation

## [3.8.0] - 2026-03-24
### Added
- **Optional bearer token authentication**: Set `MCP_AUTH_TOKEN` to enable bearer token auth on the MCP SSE endpoint (via a custom `TokenVerifier` subclass). Set `WEB_UI_AUTH_TOKEN` to protect the web dashboard with a Starlette middleware that checks `Authorization: Bearer <token>` headers. Both are fully optional — when unset, behaviour is unchanged. The `/metrics` endpoint and `/static/` assets are exempt from web UI auth so Prometheus scraping continues to work without credentials

## [3.7.1] - 2026-03-24
### Fixed
- **Duplicate detection in web UI showing empty clusters**: The duplicates page reported the correct number of clusters but displayed "0 memories, similarity ?" for each one. Root cause was a mismatch between the `find_all_duplicates()` return structure (flat list of dicts) and the Jinja2 template which expected named attributes (`cluster.keys`, `cluster.max_similarity`). Clusters are now returned as `{"memories": [...], "max_similarity": float}` and the template iterates correctly. Max pairwise similarity is now computed and displayed per cluster.

## [3.7.0] - 2026-03-24
### Added
- **Prometheus /metrics endpoint**: Web UI now exposes a `/metrics` endpoint for Prometheus scraping with memory counts, recall statistics, and server uptime

## [3.6.1] - 2026-03-19
### Added
- **Last auto-maintenance timestamp in web UI**: Duplicates and Contradictions pages now show when auto-maintenance last ran, which project it ran for, and how many duplicates were archived or contradictions found

## [3.6.0] - 2026-03-19
### Added
- **Automatic maintenance on briefing interval**: Every N `briefing()` calls per project (default 10, configurable via `AUTO_MAINTENANCE_INTERVAL`), the server automatically scans for duplicate episodic memories and archives the oldest in each cluster, then runs a heuristic contradiction scan on active project memories. Results appear in the briefing response under `auto_maintenance`. Per-project counter tracked in Valkey at `meta:maintenance:{project}`. Set `AUTO_MAINTENANCE_INTERVAL=0` to disable
- New `mcp_server/memory/maintenance.py` module with `run_maintenance()` function
- `meta:` key prefix added to valid key prefixes for maintenance counters and included in backup/restore
### Changed
- **Instructions updated**: Periodic maintenance section in `instructions.py` and `claude_config/CLAUDE.md` now describes automatic maintenance instead of manual `find_duplicates()`/`check_contradictions()` guidance

## [3.5.0] - 2026-03-19
### Added
- **Auto tool setup via MCP instructions**: The server now ships its usage guide (session start, recall workflow, experience recording, tagging vocabulary) as the `instructions` field in the MCP protocol handshake. Agents connecting to OmniMem automatically receive the full usage guide in their system prompt — no need to manually copy `claude_config/CLAUDE.md` into project directories
- New `mcp_server/instructions.py` module contains the embedded instructions constant
### Changed
- **FastMCP minimum version**: Bumped from `>=0.9.0` to `>=2.13.0` (instructions support was added in v2.12.4)

## [3.4.1] - 2026-03-19
### Changed
- **CLAUDE.md**: Added `compile_project_context` instructions to Session Start (auto-generate context from existing memories) and Session End (refresh context after significant work)
- **README**: Added `compile_project_context` to the Project context tools table

## [3.4.0] - 2026-03-19
### Added
- **compile_project_context tool**: New MCP tool that gathers all episodic memories, tags, experience data, breakthroughs, gotchas, and abandoned approaches for a project and returns them as a structured draft context. Supports `auto_save=True` to automatically create or update the project context record from the compiled data

## [3.3.6] - 2026-03-19
### Added
- **Download feeds.yml**: Download icon button on the RSS Feeds page saves the current `feeds.yml` configuration file to the browser
- **Upload feeds.yml**: Upload icon button accepts `.yml`/`.yaml` files only, validates YAML structure (must contain a top-level `feeds` list), replaces `feeds.yml` on disk, and triggers automatic RSS worker reload via mtime change
### Changed
- **Sidebar "Knowledge" section**: RSS Feeds moved into its own labelled sidebar section with dividers, separating knowledge ingestion tools from memory maintenance

## [3.3.5] - 2026-03-19
### Added
- **Upload backups**: New Upload button on the backups page opens a file picker filtered to `.json` files, validates the content is valid JSON, sanitises the filename, and saves to the backup directory. The list refreshes automatically after upload
### Fixed
- **Project name badge overlap**: Added right padding to the name cell on the projects table so the memory count badge no longer overlaps the description column

## [3.3.4] - 2026-03-18
### Added
- **Delete projects from web UI**: Delete button on both the projects list and detail pages with confirmation prompt. Deletes the project context record but preserves memories tagged with the project
- **Download backups**: New download button on the backups page serves the JSON backup file directly to the browser, with path traversal protection
### Changed
- **Backup action buttons redesigned**: Preview button replaced with magnifying glass icon, download uses arrow-down icon, all three action buttons (preview, download, delete) are uniform icon buttons aligned at the same height
- **README**: Added Claude Code permission tip for auto-allowing OmniMem MCP tools via `mcp__omnimem__*` wildcard

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
