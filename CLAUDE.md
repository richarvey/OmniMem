# OmniMem Development Guide

## What is this?

Self-hosted semantic memory MCP server for Claude Code. Provides persistent memory across sessions via four namespaces: episodic (decisions, bugs, patterns), project context (stack, goals, state), knowledge base (RSS articles auto-summarised by Claude Haiku), and preferences (prescriptive rules extracted from conversation, e.g. "always update README after a feature"). v6 adds a fifth, derived namespace: compiled skills (`mem:skill:`) — SKILL.md documents distilled from experience and graveyard memories per domain, gated behind a propose-and-accept write path.

v6.6 adds work-type domains on projects: a project declares what kinds of work it contains (`python`, `docker`, `wcag-accessibility`) using the same vocabulary compiled skills use, so `recall(domain_filter="python")` searches every Python project at once.

**Version**: 6.6.0
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
    skills.py         # v6 skill compiler engine: domain pools, lesson clustering, SKILL.md rendering, diffs
    skill_compiler.py # Shared propose-and-accept compile flow (used by MCP compile_skill AND the web UI)
    skill_transfer.py # v6.4 skill export/import: checksummed zip bundles (skill + source memories + influencing feeds), strictly additive import
    feed_influence.py # v6.5 feed→skill influence: meta:feed:influence mirror of feeds.yml, validation, per-domain feed lookup
    project_domains.py # v6.6 project work-type domains: vocabulary shared with skills, domain→project resolution (TTL cached), stack/tag-derived suggestions
    skill_scan.py     # v6.4.1 auto skill scan: time-gated in briefing, proposes drafts (stash only) for cross-project patterns + changed skills
  tools/              # 30+ MCP tool implementations
    core.py           # remember, recall, recall_index, recall_detail, deprioritise, archive, forget
    project.py        # set/get/update/compile project_context, compile_project_domains, list_projects, delete_project
    experience.py     # record_experience, log_abandoned, warn_if_abandoned
    briefing.py       # Session-start 5-in-1 aggregation
    audit.py          # memory_audit, explain_memory, why_did_you_mention
    backup.py         # dump_to_file, restore_from_file, list_backups
    contradiction.py  # check_contradictions tool
    topics.py         # suppress/unsuppress/list_suppressions
    skills.py         # compile_skill (propose/write gate), find_skills, get_skill, bless + briefing surfaces
  tests/              # pytest with in-memory fakes (no Docker needed)
    conftest.py       # FakeValkeyClient, FakeEmbedder, FakeStore fixtures + web_client
                      # (TestClient over the real web_ui app; neutralises load_dotenv so
                      # a production .env further up the tree can't leak into tests)

web_ui/               # Starlette + Jinja2 + htmx dashboard
  app.py              # ASGI app setup, route mounting
  deps.py             # Shared init (mirrors server.py pattern)
  routes/             # 17 route modules (memories, search, projects, skills, feeds, telemetry, metrics, etc.)
  templates/          # Jinja2 templates with htmx partials
  static/             # htmx.min.js, style.css

rss_worker/           # Background RSS ingestion
  worker.py           # APScheduler entry + feeds.yml file watcher
  ingester.py         # Fetch → strip HTML → summarise → embed → store
  summariser.py       # Claude Haiku summaries or truncation fallback
  feeds.yml           # Feed definitions (url, name, topics, optional project label)

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
- **Auto skill scan** (v6.4.1) — time-gated in `briefing()` (`meta:skill_scan:last_run`, `SKILL_SCAN_INTERVAL_HOURS` default 24, 0 disables): proposes new skills for domains whose lessons clear the reinforcement gate (cross-project by default, `SKILL_SCAN_CROSS_PROJECT`) and drafts for changed skills (fed the `pending_skill_updates` domains so change detection runs once). Stash-only — the propose-and-accept gate is untouched. Noise gate: `meta:skill_scan:seen:{domain}-{user}` holds the last auto-proposed body sha (volatile-stripped), so an ignored/expired draft is never re-proposed until the compiled output changes; the scan withdraws its own stash when the sha matches. Domains with any live proposal stash (human or auto) are skipped entirely
- **Project work-type domains** (v6.6) — a project context carries `domains` (`python,docker`), normalised through the *same* `resolve_domain`/`validate_domain` as compiled skills so the two can never become parallel taxonomies. `recall(domain_filter=...)`/`recall_index` resolve domains to a project set and filter on all of them; `project_filter` and `domain_filter` intersect. Two rules the design turns on: (1) **domains route, they never label memories** — a memory doesn't inherit its project's domains, because OmniMem is Python *and* CSS at once and inheritance would surface a CSS gotcha in a Python search; the domain narrows candidate projects and the vector search still decides relevance; (2) **an unmatched domain is always reported** — it degrades to an unscoped search with a leading `domain_filter_notice`, and an empty domain∩project intersection returns nothing rather than falling through to a global search (the pipeline reads an empty project list as "search everything", so the tool layer short-circuits). `migrate_project_domains` seeds from `stack` so an upgrade isn't empty on day one; the field being absent differs from it being an empty string ("considered, nothing derivable"). The skill scan boosts, but never introduces, candidates by project-declared domains
- **`domains` is stored comma-separated, not JSON** — a TAG field tokenises on commas, so the JSON-array form used by `tags`/`topics` indexes as garbage tokens (`["python`, `"docker"]`) and `@tags:{python}` matches nothing. Nothing filters on those fields, which is why it has never bitten; `domains` is meant to be filtered, so it uses the format the index actually reads. Validation guarantees a domain can't contain a comma
- **Never call valkey-py's `client.ft(name).dropindex()`** — use `execute_command("FT.DROPINDEX", name)`. `dropindex()` passes its delete-documents flag positionally even when False, so the wire command is `FT.DROPINDEX <index> ""` (three args). RediSearch tolerates the trailing empty string, valkey-search rejects it with "wrong number of arguments". This silently broke `_migrate_indexes()` from its introduction until 6.6.0: the error was caught by the same `except valkey.ResponseError` that handles a missing index, so every upgraded instance kept a stale index and the logs said nothing. `reindex_namespace()` always used the raw command and was unaffected. The test fake now *raises* on `dropindex()` so nothing can reach for it again
- **Index migration** on startup — `_migrate_indexes()` compares field count against definitions, drops stale indexes (data-safe) so they get recreated with new fields
- **Per-memory recall counters** — `recall_count` and `last_recalled` updated via pipeline on each recall; `/telemetry` dashboard and `/metrics` Prometheus endpoint expose these
- **RSS articles carry a project label** (v6.1.1) — default `RSS`, per-feed override via `project:` in feeds.yml, backfilled by a startup migration (`memory/migrations.py`), so ingested articles stay separable from conversation-sourced knowledge. Articles are identified by `feed_name`; the label must satisfy the project-name charset or the ingester falls back to `RSS`. It does not create a pseudo-project: projects pages/tools only count `mem:project:*` keys
- **Skills are whole document objects, never chunked** (v6) — canonical body lives in the `body` hash field at `mem:skill:gen:{domain}-{user}`; `idx:skill` embeds discovery metadata only (name + description + domain) and `body` is deliberately absent from `_NAMESPACE_RETURN_FIELDS["skill"]`. `get_skill` returns it intact by ID
- **Skill writes are gated** (v6) — `compile_skill(mode="propose")` stashes the rendered draft in `meta:skill:proposal:{domain}-{user}` (TTL `SKILL_PROPOSAL_TTL_SECONDS`); `mode="write"` commits that stashed body verbatim (no recompile at write time), refuses if the stored skill's sha changed since the proposal, and refuses anything not flagged `generated: true`. Experience/graveyard writes stay ungated — that asymmetry is the design
- **Promoted knowledge feeds skills, ordinary knowledge doesn't** (v6.2) — `promote_knowledge(key, domain=...)` sets `skill_domains` + `promoted_at` on the article (and clears expiry); `gather_promoted_knowledge()` pools those into `compile_skill`, rendered as `ref` rules in a separate Reference section — one summary rule per article, or one stance-prefixed rule per item when the article was promoted with extracted `rules=[{kind, text}, ...]` (stored in `skill_rules` on the article; extraction happens at promotion under human review, never at compile, so rendering stays deterministic). Promotion substitutes for reinforcement (same logic as `bless`), so refs bypass the gate but never count toward it. The briefing's `knowledge_watch()` (tools/skills.py) is the awareness layer: recent unpromoted articles vs skill discovery vectors (both read via `get_vectors_multi`, no re-embedding), with the tier-1 negation heuristic upgrading matches to `possible_contradiction`. Tunables: `SKILL_KNOWLEDGE_WATCH_DAYS` (14, 0 disables), `SKILL_KNOWLEDGE_WATCH_THRESHOLD` (0.35)
- **Skill compilation is deterministic** — same source memories render a byte-identical body except the `compiled_at` frontmatter line (`bodies_equivalent()` strips exactly that line). Don't introduce randomness, dict-order dependence, or extra timestamps into `render_skill_md()` or every recompile will propose noise diffs
- **RSS feeds can influence skills** (v6.5) — a feed's `skills: {domain: score}` mapping (1-10, web UI feed editor or feeds.yml) makes recompiles of that skill pull the feed's latest articles into a Feed watch section: score = number of most-recent articles contributed, total capped by `SKILL_FEED_MAX_ARTICLES` (25, 0 disables). Config is mirrored into the `meta:feed:influence` Valkey hash by the web UI on every feed save AND by the worker each ingest cycle (`rss_worker/ingester.py _sync_feed_influence` is a deliberate copy of `memory/feed_influence.py` — the worker image doesn't ship the memory package; keep the formats in step). Feed rules are kind `feed`, bypass the reinforcement gate like refs, never bootstrap a skill (no pool + no refs is still `no_candidates`), and their churn is always low-risk in `summarise_rule_changes`. Skill bundles are format v2: `feeds.json` carries influencing feeds; import merges them additively by URL (existing feeds at most gain a missing influence entry)

## Validation Constraints

- Project names: alphanumeric, hyphens, underscores, dots, spaces only
- Content: max 50KB per memory
- Tags: max 20 per memory, each ≤100 chars
- Project domains: max 20 per project, same charset as skill domains, aliases resolve via `DOMAIN_ALIASES`
- Namespaces: `episodic`, `project`, `knowledge`, or `preference` for `remember()`; `skill` exists as a search namespace but is only writable through the `compile_skill` gate
- Key prefixes: `mem:episodic:`, `mem:project:`, `mem:knowledge:`, `mem:preference:`, `mem:skill:`
- Skill domains: normalised to lowercase kebab-case, 1-64 chars of `[a-z0-9._-]`; aliases (`py`→`python` etc) resolve in `memory/skills.py DOMAIN_ALIASES`

## Docker Services

| Service | Port | Purpose |
|---------|------|---------|
| `valkey` | 6379 (internal) | Vector DB + search |
| `mcp_server` | 8765 | MCP SSE transport |
| `rss_worker` | — | Background feed ingestion |
| `web_ui` | 8080 | Web dashboard + `/metrics` Prometheus endpoint |

Volumes: `valkey_data` (persistent DB), `./backups` (shared), `./rss_worker/feeds.yml` (shared config)

## Recall Pipeline (how scoring works)

1. Abandoned fast-path: keyword scan on `abandoned_approaches` (no embedding needed; parsed entries cached for `ABANDONED_CACHE_TTL_SECONDS`, default 60, invalidated on experience/forget/restore writes)
2. Vector search: embed query, search `max(20, top_k)` candidates per namespace (min 50 under a project filter, widening by 10 per extra project up to 100). State and project filters are pushed into FT.SEARCH as tag filters (episodic, preference, knowledge) so archived/out-of-project docs don't consume candidate slots; Python-side filters remain as the safety net. `project_filter` takes a list since v6.6 and composes with clause-level OR — never in-brace alternation — and one unsafe value drops push-down for the whole clause rather than filtering on part of the set
3. Apply multipliers in order:
   - Surface score (lifecycle state: active 1.0x, deprioritised 0.2x, archived 0.0x)
   - Recency decay (age penalty after `RECENCY_DECAY_DAYS`, default 90)
   - Experience weight (effort × outcome: succeeded 1.0x–1.8x, pivoted 0.7x, abandoned 0.1x)
   - Temporal boost (1.0–1.5x when the query mentions a date and the memory has a close `event_date`; applied in both the main loop and query-expansion variants)
4. Merge results from all namespaces, dedupe by (key, result type), re-rank by adjusted_score
5. Select top_k, suppressing an extracted fact when its `enriched_from` source memory already made the cut (verbatim carries more context — issue #20)
6. Log recall event and increment per-memory `recall_count` + `last_recalled` counters

**Enriched facts** (issue #20): background extraction routes facts to the `knowledge` namespace (preferences to `preference`) with `surface_score` 0.5 so verbatim chunks outrank their own facts, `enriched_from` linking back to the source, and an `event_date` fallback chain (fact's own date → source `event_date` → source `created_at`) so temporal queries can find them.

## Gotchas

- **valkey-search FT.SEARCH tag filters diverge from the RediSearch docs** (verified live): raw tag values match — including spaces, dots and hyphens (`@project:{omni mem}` works as-is) — while backslash-escaped or quoted values match NOTHING. In-brace alternation `{a|b}` is also broken; use clause-level OR: `(@state:{a} | @state:{b})`. Interpolate values raw and only after allowlist validation (`_TAG_VALUE_SAFE_RE` in recall.py). `store.search()` retries unfiltered when a filtered query errors, so a bad filter degrades rather than returning [].
- **Stored vectors are readable via `store.get_vectors_multi(keys)`** — a second binary-safe client (decode_responses=False) reads the `vector` field the main client can't. Dedup/maintenance/check_contradictions reuse stored embeddings instead of re-embedding namespaces; fall back to `embed_batch` only for entries whose vector is missing.
- **Batch reads use `store.get_fields_multi(keys, fields)`** for list/scan/aggregate views — one pipelined HMGET per key, only the named fields, no vector payload. `get_multi` (two round trips, all text fields) is for when you genuinely need the whole record. When adding a field to a list/telemetry/audit view, remember to add it to that view's projection tuple or it will silently read as `None`.
- **The object returned by `load_refresh_token` must expose `expires_at`** (absolute unix seconds), not just `expires_in`. The MCP SDK bundled with FastMCP 3.x reads `refresh_token.expires_at` in its token handler; without it every refresh raises `AttributeError` → 500 → the client (desktop app, claude.ai) can't get a token and every subsequent `/mcp` request 401s. `_StoredToken` stores `created_at` + `expires_in` and exposes `expires_at` as a computed property (`created_at + expires_in`). Storage already drops expired refresh tokens in `load_refresh`, so any token reaching the handler is live. Fixed in v5.5.2.
- **OAuth refresh uses a rotation grace window**, not strict single-use. `exchange_refresh_token` retires the old token by re-saving it with a `rotated_to` marker and a short TTL (`OAUTH_REFRESH_GRACE_SECONDS`); replays inside the window return the same successor pair. This is what stops claude.ai's concurrent refreshes from racing to `invalid_grant`. Any change to token storage must round-trip `rotated_to` (see `_serialise_stored_token`).
- **Valkey runs with AOF** (`--appendonly yes`) so OAuth tokens survive restarts; Compose refuses to start with an empty `VALKEY_PASSWORD`.
- **FastMCP 3.x guards the Host *and* Origin headers** (`HostOriginGuardMiddleware`, added in the 3.x upgrade). Two distinct failures behind a reverse proxy / tunnel (Traefik, Caddy, Tailscale funnel), both easy to misread as OAuth breakage:
  - **`421 Misdirected Request`** — the `Host` isn't in the allowlist, which defaults to just localhost (`127.0.0.1`, `localhost`, `::1`) plus the bind host. The public hostname isn't on it, so **every** request 421s, including the `/.well-known/*` OAuth discovery endpoints. Local `curl` keeps working because `localhost` is allowed, so it's easy to misdiagnose. Symptom: `curl https://host/mcp` returns `421` where it used to return `401`.
  - **`403 Forbidden Origin`** — hits the browser login POST to `/oauth/login` after Host is fixed. The proxy usually terminates TLS and forwards over http, so the ASGI scope scheme is `http` and FastMCP's derived origin is `http://host`, while the browser sends `Origin: https://host`. Scheme mismatch → 403. Fixing Host alone is not enough; the https origin must be trusted too.

  `server.py` fixes both automatically: it derives the hostname and the full `scheme://host[:port]` origin from `OAUTH_BASE_URL` / `MCP_PUBLIC_URL` (plus optional comma-separated `MCP_ALLOWED_HOSTS` / `MCP_ALLOWED_ORIGINS`) and writes `fastmcp.settings.http_allowed_hosts` and `http_allowed_origins` before `mcp.run()`. The underlying FastMCP knobs are `FASTMCP_HTTP_ALLOWED_HOSTS` / `FASTMCP_HTTP_ALLOWED_ORIGINS` — both pydantic `list[str]`, so values must be **JSON arrays** (`["mcp.example.com"]`, `["https://mcp.example.com"]`); a bare string fails to parse.
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

Commit after each meaningful section of work for easy rollback. The repo is hosted on a self-hosted Forgejo instance at `code.squarecows.com` (owner `ric`) — push over git (SSH on port 222). The connected Forgejo MCP is pointed at `code.squarecows.com`, so its repo/action tools (PRs, releases, `list_action_runners_jobs`, etc.) operate directly on this repo — use them for PRs and repo operations.

**Branch policy**: version lines (v6, v7, ...) are developed AND released from their own branch — cut tags and releases on the version branch. Never merge a version branch into main unless explicitly asked; main only moves when Ric says so.

## Web UI Notes

- htmx endpoints must return **partials**, not full page templates
- **Every data table uses `table-layout: fixed` with percentage widths summing to exactly 100%** — the only method that cannot overflow the viewport. Long content is handled inside the cell (ellipsis via `content-cell`, or `overflow-wrap`). Never switch a table to `table-layout: auto` in media queries, never put `display:flex` on a `<td>` (it detaches the cell from table layout so its column width stops applying), and when adding a column, rebalance the percentages — the old ones still sum to 100% and the new column gets no room. `.content` keeps `min-width: 0` so long unbroken strings can't widen the page
- **Fonts are self-hosted**: Ubuntu + Ubuntu Mono woff2 files live in `web_ui/static/fonts/` with `@font-face` rules at the top of `style.css` — never link out to Google Fonts, the UI must work offline. Monospace elements use `var(--font-mono)`
- **Accent colour is split for WCAG AA**: the palette is brand orange (`--accent-strong` #ff8e01 for solid fills, always paired with `--on-accent` navy text). `--accent` is for borders and boundaries, `--accent-text` for accent-coloured text (#ff8e01 on dark at 8.35:1, #9a5a00 on paper at 5.1:1), and `--accent-soft` is the translucent wash — use the token rather than hardcoding an rgba, or a theme change leaves your fill behind
- **The projects table is six columns** since v6.6 added Domains — Name, Domains, Description, Current State, Updated, Actions, widths summing to exactly 100%. Domain pills wrap inside their cell and kebab-case names break at hyphens
- **Dashboard stat cards are `<a class="stat-card">`** — whole-card links, styled via `a.stat-card` so the plain `div.stat-card` on telemetry/experience/token pages is unaffected
- Footers are full-width (not inside sidebar/container)
- Auth is a single `AuthMiddleware` (web_ui/auth.py): session login turns on automatically when `OAUTH_ADMIN_USER` + `OAUTH_ADMIN_PASSWORD` are set (`WEB_UI_LOGIN_ENABLED=false` opts out), reusing the OAuth credentials with the same constant-time compare and per-IP rate limit knobs. Sessions are opaque tokens in Valkey (`meta:webui:session:{token}`, TTL `WEB_UI_SESSION_HOURS`, default 168) behind an HttpOnly `omnimem_session` cookie; `/logout` deletes the token server-side. `WEB_UI_AUTH_TOKEN` bearer auth is accepted alongside for scripts. `/metrics`, `/static/` and `/login` are exempt; unauthenticated htmx requests get 401 + `HX-Redirect` so partials never swap in the login page
- Sidebar is grouped: Memory (memories, projects, preferences, experience, graveyard), Skills, Management (duplicates, contradictions, suppressions), Knowledge Management (articles, learned knowledge, RSS feeds), System Management (telemetry, backups). Preferences, Articles, and Learned Knowledge are filtered `/memories` views — `memories_list` maps namespace + `source` (`rss` = has `feed_name`, `learned` = doesn't) to `current_page` so the right sidebar entry highlights
- Memory list rows carry Deprioritise/Delete buttons posting to `/lifecycle/*` with a `next` field for the return redirect — same-site paths only (see `_redirect_target`)
- `/skills` pages allow **create, delete, export, and import — never edit**. Export (`/skills/export/{key}`) bundles the skill plus its source memories into a checksummed zip (`memory/skill_transfer.py`); import validates the upload, stashes it under a one-shot token (`meta:skill:import:{token}`, TTL 30 min), previews the plan in the modal, and only writes on confirm. Import is strictly additive: existing keys are never overwritten, vectors are re-embedded locally, and `recall_count`/`last_recalled` never travel. The New Skill modal runs the same propose-and-accept gate as MCP (`memory/skill_compiler.py compile_skill_flow` — shared so the two paths can't drift): compile a draft, review it in the modal, accept commits it. It refuses domains whose skill already exists (recompiles stay on the MCP flow, which carries the diff review). Delete asks for confirmation; the source memories survive, so recompiling the domain can recreate the skill. The UI links each rule and the source manifest back to `/memory/{key}` because the raw memories are what you change
- The dashboard stats cache payload must carry the `skills` key and recent entries must carry `updated_date` — `_load_cached_stats` treats older shapes as stale and recomputes. If you add fields the dashboard template requires, extend that shape check or upgrades will KeyError until the TTL expires
- Skills count into telemetry/metrics via the shared `recall_count`/`last_recalled` counters (`get_skill` bumps them); telemetry substitutes name + description for their missing `content` field and links them to `/skills/...`

## Writing Style

- British English spelling (colour, summarised, centre)
- Conversational, humanised tone — no em dashes, no marketing fluff
- Technical but accessible, with concrete numbers where possible
