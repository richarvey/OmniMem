# OmniMem Development Guide

## What is this?

Self-hosted semantic memory MCP server for Claude Code. Provides persistent memory across sessions via three namespaces: episodic (decisions, bugs, patterns), project context (stack, goals, state), and knowledge base (RSS articles auto-summarised by Claude Haiku).

**Version**: 3.10.2
**Stack**: Python 3.12, FastMCP (SSE transport), Valkey + valkey-search (HNSW vectors), sentence-transformers (all-MiniLM-L6-v2, 384-dim), Anthropic API (Claude Haiku for RSS summarisation), Pydantic v2, Docker Compose, APScheduler, feedparser, PyTorch CPU-only

## Project Structure

```
mcp_server/           # MCP server — FastMCP SSE transport
  server.py           # Entry point: init store/embedder/lifecycle/pipeline, register tools
  memory/             # Core engine (shared with web_ui)
    store.py          # ValkeyStore: connection pool, HNSW vector indexes, CRUD
    embedder.py       # Singleton SentenceTransformer (all-MiniLM-L6-v2, 384-dim)
    lifecycle.py      # MemoryState enum, state transitions, topic suppression
    recall.py         # RecallPipeline: abandoned fast-path → vector search → scoring
    dedup.py          # Cosine similarity duplicate detection (threshold 0.92)
    maintenance.py    # Auto-maintenance: dedup + contradiction scan on briefing interval
    contradiction.py  # Tier 1 heuristic + optional Tier 2 Claude Haiku API
  tools/              # 30+ MCP tool implementations
    core.py           # remember, recall, recall_index, recall_detail, deprioritise, archive, forget
    project.py        # set/get/update/compile project_context, list_projects
    experience.py     # record_experience, log_abandoned, warn_if_abandoned
    briefing.py       # Session-start 5-in-1 aggregation
    audit.py          # memory_audit, explain_memory, why_did_you_mention
    backup.py         # dump_to_file, restore_from_file, list_backups
    contradiction.py  # check_contradictions tool
    topics.py         # suppress/unsuppress/list_suppressions
  tests/              # pytest with in-memory fakes (no Docker needed)
    conftest.py       # FakeValkeyClient, FakeEmbedder, FakeStore fixtures

web_ui/               # Starlette + Jinja2 + htmx dashboard
  app.py              # ASGI app setup, route mounting
  deps.py             # Shared init (mirrors server.py pattern)
  routes/             # 16 route modules (memories, search, projects, feeds, telemetry, metrics, etc.)
  templates/          # Jinja2 templates with htmx partials
  static/             # htmx.min.js, style.css

rss_worker/           # Background RSS ingestion
  worker.py           # APScheduler entry + feeds.yml file watcher
  ingester.py         # Fetch → strip HTML → summarise → embed → store
  summariser.py       # Claude Haiku summaries or truncation fallback
  feeds.yml           # Feed definitions (url, name, topics)

claude_config/        # CLAUDE.md template for end-users to copy into their projects
scripts/              # health_check.sh, restore_backup.sh
```

## Running Locally

```bash
cp .env.example .env   # Edit: set VALKEY_PASSWORD, optionally ANTHROPIC_API_KEY
docker compose up -d
```

- MCP server: `http://localhost:8765/mcp`
- Web UI: `http://localhost:8080`
- Valkey: `localhost:6379`

## Running Tests

```bash
cd mcp_server && pytest tests/
```

Tests use in-memory fakes (FakeValkeyClient, FakeEmbedder) — no running Valkey required.

For Docker-based tests: `docker compose -f docker-compose.test.yml up --build`

## Key Architecture Decisions

- **ULIDs** for memory keys (sortable, collision-free)
- **SSE transport** (stateless, simpler than WebSocket for MCP)
- **Valkey** over Redis (open source fork)
- **CPU-only PyTorch** (no GPU dependency)
- **Shared `memory/` package** between MCP server and web UI (no code duplication)
- **Debian-slim Docker base** — Alpine doesn't work (PyTorch has no musllinux wheels)
- **In-memory fakes** for testing (no Docker-in-tests complexity)
- **Auto-maintenance** on briefing interval — dedup + contradiction scan every N `briefing()` calls per project, tracked by `meta:maintenance:{project}` counter in Valkey (configurable via `AUTO_MAINTENANCE_INTERVAL`, default 10, set to 0 to disable)
- **Index migration** on startup — `_migrate_indexes()` compares field count against definitions, drops stale indexes (data-safe) so they get recreated with new fields
- **Per-memory recall counters** — `recall_count` and `last_recalled` updated via pipeline on each recall; `/telemetry` dashboard and `/metrics` Prometheus endpoint expose these

## Validation Constraints

- Project names: alphanumeric, hyphens, underscores, dots, spaces only
- Content: max 50KB per memory
- Tags: max 20 per memory, each ≤100 chars
- Namespaces: `episodic`, `project`, or `knowledge` only
- Key prefixes: `mem:episodic:`, `mem:project:`, `mem:knowledge:`

## Docker Services

| Service | Port | Purpose |
|---------|------|---------|
| `valkey` | 6379 (internal) | Vector DB + search |
| `mcp_server` | 8765 | MCP SSE transport |
| `rss_worker` | — | Background feed ingestion |
| `web_ui` | 8080 | Web dashboard + `/metrics` Prometheus endpoint |

Volumes: `valkey_data` (persistent DB), `./backups` (shared), `./rss_worker/feeds.yml` (shared config)

## Recall Pipeline (how scoring works)

1. Abandoned fast-path: keyword scan on `abandoned_approaches` (no embedding needed)
2. Vector search: embed query, search top 20 per namespace
3. Apply multipliers in order:
   - Surface score (lifecycle state: active 1.0x, deprioritised 0.2x, archived 0.0x)
   - Recency decay (age penalty after `RECENCY_DECAY_DAYS`, default 90)
   - Experience weight (effort × outcome: succeeded 1.0x–1.8x, pivoted 0.7x, abandoned 0.1x)
4. Merge results from all namespaces, re-rank by adjusted_score
5. Log recall event and increment per-memory `recall_count` + `last_recalled` counters

## Gotchas

- **PyTorch is the Alpine blocker** — not sentence-transformers or numpy. PyTorch only publishes manylinux (glibc) wheels. Any project using PyTorch (directly or transitively) cannot use Alpine. The ~2.2GB image size is mostly PyTorch, not the Debian base. Alpine with gcompat shim also fails (pip rejects at download/hash verification stage).
- **inotify doesn't work for Docker bind mounts** — mtime polling (10s interval, configurable via `FEEDS_WATCH_INTERVAL`) is more portable. The RSS worker uses this for feeds.yml change detection.
- **Projects without `set_project_context()`** only exist as ULID memories — the web UI detail view won't work for them until a proper context entry is created. Template conditionally disables links for these.
- **RSS summariser fallback** — Haiku API calls retry up to 2 times with backoff. Fallback truncation is 800 chars (was 300, bumped in v0.2.2).

## Key Breakthroughs (from experience)

- `remember(namespace="project")` creates ULID keys with "project" field but not "project_name" — fixed with startup migration + dedup logic in list functions
- mtime polling over inotify/watchdog for Docker bind mount compatibility; shared feeds.yml via host path mount to both containers
- htmx endpoints must return **partials**, not full page templates — extract into `partials/` and use `{% include %}` in the main template
- Uploading feeds.yml just writes the file and the worker picks up the change automatically via mtime watcher, no inter-process signalling needed
- `table-layout:fixed` with percentage column widths + `white-space:nowrap` on name cell + split date into two spans for responsive tables

## Committing

Commit after each meaningful section of work for easy rollback. The repo is hosted on Codeberg — Forgejo MCP is connected to Codeberg and can be used for PRs and repo operations.

## Web UI Notes

- htmx endpoints must return **partials**, not full page templates
- Footers are full-width (not inside sidebar/container)
- Optional bearer token auth via `WEB_UI_AUTH_TOKEN` env var; `/metrics` and `/static/` are exempt

## Writing Style

- British English spelling (colour, summarised, centre)
- Conversational, humanised tone — no em dashes, no marketing fluff
- Technical but accessible, with concrete numbers where possible
