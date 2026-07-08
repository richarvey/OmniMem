# \<OmniMem\>

[![Security Scans](https://codeberg.org/ric_harvey/omnimem/badges/workflows/security.yml/badge.svg)](https://codeberg.org/ric_harvey/omnimem/actions)
[![Docker Build](https://codeberg.org/ric_harvey/omnimem/badges/workflows/docker.yml/badge.svg)](https://codeberg.org/ric_harvey/omnimem/actions)
[![Coverage](https://codeberg.org/ric_harvey/omnimem/raw/branch/badges/coverage-badge.svg)](https://codeberg.org/ric_harvey/omnimem/actions)

> [!WARNING]
> **SSE transport is deprecated.** OmniMem 3.10 defaults to SSE but now supports Streamable HTTP via `MCP_TRANSPORT=http`. SSE will be removed in a future release. To migrate: set `MCP_TRANSPORT=http` in your `.env` and update your client config to use `"type": "http"` with URL `.../mcp` instead of `.../sse`. See the [connection guides](guides/) for updated examples.
>
> Streamable HTTP support is a community contribution from [@timstoop](https://codeberg.org/timstoop) — thanks!

**Stop living the same session twice.**

Every Claude Code session starts from zero. No memory of your project. No memory of what failed last week. No memory that you spent three hours last Tuesday discovering why `onnxruntime` explodes on Alpine before finding something that actually works.

So you explain the project again. Claude suggests the same broken library again. Same alarm. Same song. You are Bill Murray and Claude is Punxsutawney.

OmniMem fixes that. It is a self-hosted MCP server that gives Claude Code persistent memory across sessions, projects, and machines. It runs on your own hardware and it is free forever.

---

## What it does

OmniMem gives Claude Code four things it currently lacks.

**Episodic memory** is the decisions you made, the bugs you fixed, the patterns you discovered. The things that took real effort to learn and should not have to be re-learned every morning.

**Project context** is your stack, goals, and current state. Claude arrives at every session already briefed rather than starting cold.

**Passive knowledge** comes from RSS feeds you configure. They get fetched on a schedule, summarised by Claude Haiku, embedded, and stored. When you are working on a Rust problem and a relevant article was ingested last week, it surfaces as a starting point worth reading.

**Preferences** are prescriptive rules about how you want to work — "always update the README and CHANGELOG after a feature lands", "prefer terse responses with no trailing summary". These are extracted from your conversations automatically (via the new fact-extraction ingest mode) and surfaced whenever they apply.

All four namespaces are searched together at recall time. The top result might be a decision from six months ago on a different project, a solution from yesterday, or an article that landed in your knowledge base on Tuesday night. It does not matter where it came from as long as it is useful.

---

## The bits no other memory system has

### Memory is not binary

Most systems remember or forget. OmniMem has a lifecycle:

```
ACTIVE  ->  DEPRIORITISED  ->  ARCHIVED  ->  DELETED
 1.0x         0.2x             0.0x        gone
```

When you say "forget about X" you do not usually mean destroy it. You mean stop surfacing it. OmniMem deprioritises rather than deletes, applying a surface score multiplier at recall time. If something becomes relevant again later it can earn its way back.

You can also suppress entire topics. Calling `suppress_topic("pisource.org")` means nothing touching that topic surfaces in any recall, across any session, until you lift it.

### The Graveyard

OmniMem tracks not just what worked but what did not and why.

Every abandoned approach gets logged with its name, type, and reason for failure. Before Claude suggests a library or architectural pattern the graveyard is checked first. If you tried something before and gave up on it, that warning surfaces at the top of results before anything else does.

```
WARNING: previously abandoned approaches match this query

  onnxruntime       library     SIGILL crash on Alpine musl libc       effort: 4/5
  FLAT index        approach    too slow above 10k vectors              effort: 3/5
  openai embeddings service     API cost and latency were prohibitive   effort: 2/5
```

Dead ends do not get a second chance to waste your afternoon.

### Experience scoring

Not all successful memories are equal. Something that worked first time is useful. Something that took four attempts, two abandoned libraries, and a weird Alpine-specific workaround to crack is gold, and it should surface more readily.

OmniMem assigns an experience weight to every memory based on effort and outcome:

| Effort | Meaning | Recall weight |
|---|---|---|
| 1 | Worked first time | 1.0x |
| 2 | Minor friction | 1.1x |
| 3 | Multiple iterations | 1.25x |
| 4 | Significant struggle | 1.5x |
| 5 | Battle-hardened | 1.8x |

The recall score formula:

```
score = similarity x surface_score x recency x experience_weight
```

A score-5 success is worth nearly twice as much in recall ranking as something trivial. Knowledge earns its rank.

### Semantic deduplication

Over time memory systems accumulate near-identical entries. OmniMem catches this at two points.

At write time, `remember()` embeds the new content and checks for existing memories above a cosine similarity threshold (default 0.92, configurable via `DEDUP_SIMILARITY_THRESHOLD`). If a near-identical memory already exists it returns the duplicate instead of storing a redundant copy. Pass `force=True` when you genuinely want both versions.

For bulk cleanup, `find_duplicates()` scans an entire namespace, batch-embeds everything, computes pairwise similarity, and returns clusters of duplicates grouped by union-find. Point it at your episodic namespace once a month and archive the extras.

### Contradiction detection

The graveyard warns you about things that failed. Contradiction detection warns you about things that disagree with each other.

When `remember()` stores a new memory it runs a fast heuristic check — finding semantically similar memories and scanning for negation pattern mismatches (e.g. one says "use X" while the other says "avoid X"). If a potential contradiction is detected it stores the memory but returns a warning so you can investigate.

For deeper analysis, `check_contradictions()` can optionally call Claude Haiku (Tier 2) to evaluate candidate pairs. Confirmed contradictions are cross-linked on both memories and flagged whenever either one surfaces in a `recall()`.

```
contradiction_warning:
  existing_key: mem:episodic:01ARZ3NDEK...
  existing_content: "Always use connection pooling for Valkey..."
  explanation: "These memories discuss the same topic but contain opposing language"
```

### Session briefing

Instead of making three separate calls at session start, a single `briefing(project="myproject")` returns everything Claude needs to get up to speed:

- **Project context** — current state, stack, last update
- **Experience summary** — effort stats, graveyard, breakthroughs
- **Stale memories** — active memories not updated in 30+ days (configurable via `STALE_MEMORY_DAYS`)
- **New knowledge** — RSS articles ingested in the last 7 days
- **Contradiction warnings** — memories with unresolved contradictions
- **Reinstate candidates** — deprioritised memories whose reinstate hints match current work
- **Suppressed topics** — what is currently filtered out

One tool call, one response, full context.

### Automatic maintenance

Memory systems accumulate duplicates and contradictions over time. OmniMem handles this automatically.

Every N `briefing()` calls per project (default 10, configurable via `AUTO_MAINTENANCE_INTERVAL`), the server runs a maintenance pass:

1. **Dedup scan** — finds clusters of near-identical episodic memories and archives the oldest in each cluster, keeping the newest
2. **Contradiction scan** — checks semantically similar active project memories for negation pattern mismatches (requires cosine similarity >= 0.5 before checking, capped at 10 results)
3. **Knowledge expiry** — archives RSS-ingested knowledge articles that have passed their `expires_at` timestamp (default 30 days after ingestion, configurable via `MAX_KNOWLEDGE_AGE_DAYS`). Manually stored knowledge items are never affected

The results appear in the briefing response under `auto_maintenance` so you know what was cleaned up. Set `AUTO_MAINTENANCE_INTERVAL=0` to disable. Manual `find_duplicates()` and `check_contradictions()` calls still work as before.

---

## Self-hosted, open source, yours

No SaaS. No vendor lock-in. No context shipped to someone else's servers.

- **Valkey** is an open source Redis fork. All your data stays in a named Docker volume on your own machine.
- **Multi-arch Docker images** for amd64 and arm64. It runs on a Raspberry Pi, AWS Graviton, or Apple Silicon just as well as x86.
- **sentence-transformers** runs embeddings locally with no API calls.
- **MIT licensed** means fork it, extend it, run it wherever you want.
- **One backup command** calls `dump_to_file()` and exports everything to a JSON file you own.

Expose the MCP port through Traefik and every machine you work from shares the same memory. One deployment, everywhere.

---

## Quick start

```bash
git clone https://codeberg.org/ric_harvey/omnimem.git && cd omnimem
cp .env.example .env
# Set VALKEY_PASSWORD and ANTHROPIC_API_KEY in .env
docker compose up -d
```

Edit the `.env` file to set at least `VALKEY_PASSWORD` to a secure value. You can also set `ANTHROPIC_API_KEY` if you want AI-powered RSS article summaries and richer contradiction detection. If you leave `ANTHROPIC_API_KEY` unset (or blank), OmniMem still works — the RSS worker will fall back to simple truncation for summaries, and contradiction checks will use embedding similarity only.

Four containers start: Valkey with vector search, the OmniMem MCP server, the RSS worker, and the web UI. The MCP server listens on port `8765` by default and the web UI on port `8080`.

Open `http://localhost:8080` in a browser to access the management dashboard — browse memories, run semantic searches, manage projects, track experience, and handle backups without needing to use MCP tool calls.

Connect your coding agent to OmniMem. The example below is for Claude Code — see the full guides for other tools:

| Agent | Guide | Transport |
|-------|-------|-----------|
| claude.ai | [guides/claude-ai.md](guides/claude-ai.md) | Streamable HTTP + OAuth 2.1 |
| Claude Code | [guides/claude-code.md](guides/claude-code.md) | SSE (default) / Streamable HTTP |
| Claude Desktop | [guides/claude-desktop.md](guides/claude-desktop.md) | SSE / Streamable HTTP (via mcp-remote) |
| GitHub Copilot | [guides/github-copilot.md](guides/github-copilot.md) | SSE (default) / Streamable HTTP |
| GitLab Duo | [guides/gitlab-duo.md](guides/gitlab-duo.md) | SSE (default) / Streamable HTTP |
| Cursor | [guides/cursor.md](guides/cursor.md) | SSE (default) / Streamable HTTP |
| AWS Kiro | [guides/kiro.md](guides/kiro.md) | SSE (default) / Streamable HTTP |
| OpenCode | [guides/opencode.md](guides/opencode.md) | SSE (default) / Streamable HTTP |
| OpenAI Codex CLI | [guides/codex.md](guides/codex.md) | SSE (default) / Streamable HTTP |

**Claude Code** (`~/.claude.json`):

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "sse",
      "url": "http://localhost:8765/sse"
    }
  }
}
```

If you set `MCP_AUTH_TOKEN` in your `.env`, add the token to the config:

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "sse",
      "url": "http://localhost:8765/sse",
      "headers": {
        "Authorization": "Bearer your-token-here"
      }
    }
  }
}
```

To stop Claude Code asking for permission every time it calls an OmniMem tool, add a wildcard allow rule to your global settings (`~/.claude/settings.json`):

```json
{
  "permissions": {
    "allow": [
      "mcp__omnimem__*"
    ]
  }
}
```

This allows all OmniMem MCP tools (`remember`, `recall`, `briefing`, etc.) to run without prompts across every project. If you already have other entries in the `allow` array, just add `"mcp__omnimem__*"` to it.

That is it. The server automatically delivers its usage guide to any connecting agent via the MCP protocol's `instructions` field. Claude Code will load project context at session start, check the graveyard before suggesting approaches, and store what it learns as you go — no manual configuration file needed.

If you want to customise the instructions or use OmniMem with a setup that does not support MCP instructions, a copy of the guide lives at `claude_config/CLAUDE.md` for manual use.

---

## MCP tools

### Core memory

| Tool | What it does |
|---|---|
| `remember(content, project?, tags?, force?, mode?)` | Store a memory. In `full` mode (default) extracts atomic facts via Claude Haiku and routes preferences to the preference namespace; `raw` stores verbatim. Auto-checks for duplicates and contradictions |
| `remember_document(content, chunk_strategy, project?, tags?, namespace?, chunk_size?, mode?)` | Index a long-form document by splitting it into chunks (`turn_pairs`, `sentences`, `paragraphs`, or `fixed_tokens`) and storing each as a memory linked by a shared `doc_id` |
| `recall(query, top_k?, project_filter?, expand_queries?)` | Semantic search across all namespaces. With `expand_queries=true`, generates alternative phrasings via Claude Haiku and unions the results to improve recall coverage when query vocabulary doesn't match stored content |
| `deprioritise(key_or_query, reason, reinstate_hints?)` | Soft-suppress without deleting |
| `archive(key_or_query)` | Remove from recall but keep for history |
| `reinstate(key_or_query)` | Bring a deprioritised memory back |
| `forget(key_or_query, confirm=True)` | Hard delete, requires explicit confirmation |
| `suppress_topic(topic)` | Filter a topic from all future recalls |
| `unsuppress_topic(topic)` | Remove a topic from the suppression list |
| `list_suppressions()` | Show all currently suppressed topics |
| `find_duplicates(namespace?, threshold?, project_filter?)` | Scan for clusters of near-identical memories |
| `check_contradictions(query?, namespace?, use_api?)` | Detect memories that contradict each other |
| `briefing(project?, include_knowledge?)` | Single-call session start with full context |

### Project context

| Tool | What it does |
|---|---|
| `set_project_context(name, description, stack, goals, current_state)` | Create or update project memory |
| `get_project_context(name)` | Retrieve it, called at every session start |
| `update_project_state(name, current_state, notes?)` | Update state without re-embedding |
| `compile_project_context(name, auto_save?)` | Auto-produce or refresh a project context from its episodic memories, tags, experience data, and abandoned approaches |
| `list_projects()` | See all stored projects |

### Experience scoring

| Tool | What it does |
|---|---|
| `record_experience(key, effort_score, outcome, abandoned_approaches?, breakthrough?, gotchas?)` | Log how hard it was and what failed |
| `log_abandoned(key, name, type, reason)` | Add dead ends incrementally mid-session |
| `warn_if_abandoned(query)` | Check the graveyard before proceeding |
| `experience_summary(project?)` | Graveyard, breakthroughs, and effort stats |
| `get_experience(key)` | Full experience data for one memory |

### Knowledge

| Tool | What it does |
|---|---|
| `recent_knowledge(days?, feed_name?, topics?, limit?)` | Query recent RSS articles with optional filters, sorted newest first |
| `promote_knowledge(key)` | Mark an article as permanently useful by clearing its expiry |

### Audit and backup

| Tool | What it does |
|---|---|
| `memory_audit(project?, namespace?, limit?, offset?)` | All memories by state; full state-count summary plus a paginated `entries` list (default 100, max 500) |
| `explain_memory(key)` | Full history for a single memory |
| `why_did_you_mention(query)` | Debug why something surfaced |
| `dump_to_file(filename?)` | Export everything to a timestamped JSON file |
| `restore_from_file(filename, dry_run?)` | Restore from backup, merges rather than overwrites, re-embeds for immediate recall |
| `list_backups()` | See available backup files |
| `health()` | Server, Valkey, index, and model status |
| `queue_status()` | Enrichment queue depth — poll until `pending` reaches 0 after batch ingest before running recall/scoring |
| `reindex(namespace?)` | Drop and recreate Valkey search indexes to clear orphaned vector entries. Data-safe |
| `version()` | Return the current OmniMem version |

---

## Configuration

| Variable | Default | Description |
|---|---|---|
| `VALKEY_PASSWORD` | `changeme` | Please change this |
| `ANTHROPIC_API_KEY` | required | For RSS summarisation via Claude Haiku |
| `MCP_AUTH_TOKEN` | *(unset)* | Set to enable bearer token auth on the MCP endpoint (constant-time compared). When unset, no auth is required — but the server refuses to start unauthenticated on a non-loopback `MCP_HOST` |
| `WEB_UI_AUTH_TOKEN` | *(unset)* | Set to enable bearer token auth on the web dashboard (constant-time compared). `/metrics` and static assets are exempt |
| `OAUTH_ENABLED` | *(unset)* | Set to `true` to enable OAuth 2.1 authorisation server for claude.ai and other OAuth MCP clients |
| `OAUTH_BASE_URL` | *(unset)* | Externally-reachable URL of your OmniMem instance (e.g. `https://mcp.example.com`). Required when OAuth is enabled |
| `OAUTH_ADMIN_USER` | *(unset)* | Username for the OAuth admin account. Required when OAuth is enabled |
| `OAUTH_ADMIN_PASSWORD` | *(unset)* | Password for the OAuth admin account. Required when OAuth is enabled |
| `OAUTH_REFRESH_MAX_DAYS` | `30` | Absolute lifetime of an OAuth refresh-token chain. Each rotation silently re-issues tokens without re-prompting the user, until this cap is reached. Hard-capped at `90` |
| `OAUTH_REFRESH_GRACE_SECONDS` | `120` | Grace window after a refresh token rotates. The old token keeps working for this long and replays return the same new pair, so claude.ai isn't logged out when several connections refresh at once. Capped at `3600`; set `0` for strict single-use rotation |
| `OAUTH_LOGIN_MAX_ATTEMPTS` | `10` | Failed admin logins per client IP before the login form is temporarily blocked. Set `0` to disable |
| `OAUTH_LOGIN_WINDOW_SECONDS` | `900` | Sliding window for the failed-login limit |
| `RSS_MAX_PAGE_BYTES` | `10485760` | Max bytes the RSS worker reads when fetching a full article page (10 MB), guarding against hostile or endless responses |
| `ABANDONED_CACHE_TTL_SECONDS` | `60` | How long the recall pipeline caches the parsed abandoned-approach list between episodic rescans. Invalidated automatically on experience writes in the same process; set `0` to rescan on every recall |
| `MCP_PORT` | `8765` | Port the MCP server listens on |
| `MCP_HOST` | `127.0.0.1` | Bind address for the MCP server (set to `0.0.0.0` inside Docker) |
| `VALKEY_MAX_CONNECTIONS` | `20` | Valkey connection pool size |
| `EMBEDDING_MODEL` | `all-MiniLM-L6-v2` | Local sentence-transformers model |
| `RSS_SCHEDULE_HOURS` | `6` | How often feeds are ingested |
| `RSS_MAX_ARTICLES_PER_FEED` | `20` | Articles per feed per cycle |
| `MEMORY_RECALL_TOP_K` | `5` | Default number of recall results |
| `DEPRIORITISED_WEIGHT` | `0.2` | Surface score for deprioritised memories |
| `RECENCY_DECAY_DAYS` | `90` | Days before the age penalty kicks in |
| `INGEST_MODE` | `full` | `full` extracts atomic facts via Claude Haiku before storing and routes preferences to the preference namespace; `raw` stores verbatim. Falls back to raw automatically when no API key is set |
| `RECALL_EXPAND_QUERIES` | `false` | Globally enable query expansion on `recall()`. Generates alternative phrasings via Claude Haiku and unions the results |
| `RECALL_EXPAND_COUNT` | `3` | Number of variant queries to generate when expansion is enabled |
| `ENRICHMENT_BATCH_MODE` | `false` | When `true`, `remember_document()` sends all chunks as a single enrichment job so the background worker makes one Haiku API call instead of N. Faster for large documents and benchmark runs |
| `DEDUP_SIMILARITY_THRESHOLD` | `0.92` | Cosine similarity threshold for duplicate detection on `remember()` |
| `CONTRADICTION_SIMILARITY_THRESHOLD` | `0.7` | Similarity threshold for contradiction candidate search |
| `STALE_MEMORY_DAYS` | `30` | Days without update before a memory is flagged as stale in `briefing()` |
| `AUTO_MAINTENANCE_INTERVAL` | `10` | Number of `briefing()` calls per project before auto-maintenance runs (0 to disable) |
| `MAX_KNOWLEDGE_AGE_DAYS` | `30` | Days before RSS-ingested knowledge articles expire and are auto-archived during maintenance |
| `METRICS_CACHE_TTL` | `60` | Seconds to cache `/metrics` endpoint results between Prometheus scrapes |
| `TELEMETRY_COLD_DAYS` | `60` | Days without recall before a memory is flagged as "gone cold" on the telemetry dashboard |
| `WEB_PORT` | `8080` | Port the web UI listens on |
| `BACKUP_DIR` | `/app/backups` | Where backup files are written (shared between MCP server and web UI) |

---

## RSS configuration

Edit `rss_worker/feeds.yml` to choose which feeds get ingested:

```yaml
feeds:
  - url: https://blog.rust-lang.org/feed.xml
    name: Rust Official Blog
    topics: [rust, systems, language]

  - url: https://this-week-in-rust.org/rss.xml
    name: This Week in Rust
    topics: [rust, community, crates]

  - url: https://blog.n8n.io/rss/
    name: n8n Blog
    topics: [automation, workflow, n8n]
```

Each article gets fetched, stripped of HTML, summarised to a couple of sentences by Claude Haiku, embedded, and stored in the `knowledge` namespace with an `expires_at` timestamp (default 30 days, configurable via `MAX_KNOWLEDGE_AGE_DAYS`). Expired articles are auto-archived during maintenance. If an article turns out to be genuinely useful, call `promote_knowledge(key)` to clear its expiry and keep it permanently. Duplicates are skipped by URL. The worker runs once on startup and then on whatever schedule you set in `RSS_SCHEDULE_HOURS`.

---

## Memory lifecycle

```
ACTIVE (1.0x)  ->  DEPRIORITISED (0.2x)  ->  ARCHIVED (0.0x)
     |                    |                        |
     +--------------------+------------------------+
                          |
                       DELETED
```

Use `deprioritise` when something should stop surfacing but might be needed again someday. Add `reinstate_hints` to describe what should bring it back. If a future query strongly matches a hint, the memory resurfaces with a note explaining why it was deprioritised in the first place.

Use `archive` for content that is definitely outdated but has historical value worth keeping.

Use `forget` only when you want something permanently gone. It requires `confirm=True` so nothing disappears by accident.

One thing worth knowing: if you deprioritise a memory with `effort_score >= 4` the system will flag it before letting you proceed. It is not blocking you, just making sure you meant to soft-suppress something that was genuinely hard to figure out.

---

## Using it from multiple machines

Expose `MCP_PORT` through your reverse proxy. Traefik example:

```yaml
labels:
  - "traefik.enable=true"
  - "traefik.http.routers.omnimem.rule=Host(`omnimem.yourdomain.com`)"
  - "traefik.http.routers.omnimem.tls.certresolver=letsencrypt"
  - "traefik.http.services.omnimem.loadbalancer.server.port=8765"
```

Update the MCP config URL to `https://omnimem.yourdomain.com/sse` (or `.../mcp` if using Streamable HTTP) and every machine you work from shares the same memory, the same graveyard, and the same project context. See the [connection guides](guides/) for how to configure each coding agent.

You can expose the web UI the same way — add a route for `WEB_PORT` with basic auth middleware. See `docs/reverse-proxy.md` for Traefik and Caddy examples.

Security checklist: strong `VALKEY_PASSWORD` (Compose now refuses to start if it's empty), set `MCP_AUTH_TOKEN` and `WEB_UI_AUTH_TOKEN` in your `.env`, TLS on the proxy if exposing publicly, and keep the Valkey port off the public internet. Bearer tokens are compared in constant time, the MCP server won't start unauthenticated on a non-loopback address, backup filenames are validated against path traversal, and uploaded/restored backups and fetched RSS pages are size-capped.

### OAuth 2.1 for claude.ai

If you want to connect from **claude.ai** (or any MCP client that uses OAuth), OmniMem has optional built-in OAuth 2.1 support. Enable it in your `.env`:

```bash
OAUTH_ENABLED=true
OAUTH_BASE_URL=https://mcp.yourdomain.com   # externally-reachable URL
OAUTH_ADMIN_USER=admin
OAUTH_ADMIN_PASSWORD=a-strong-password-here
```

When enabled, OmniMem acts as a full OAuth 2.1 authorisation server with:

- **Discovery** (`/.well-known/oauth-authorization-server`) — auto-discovered by clients
- **Dynamic client registration** (`/register`) — clients register automatically (RFC 7591)
- **Authorisation code flow with PKCE** (`/authorize` + `/oauth/login`) — browser-based login
- **Token exchange and refresh** (`/token`) — 1-hour access tokens, 30-day refresh tokens with rotation

Point claude.ai at your OmniMem URL and it handles the rest — discovery, registration, browser login, and token management all happen automatically.

**Staying signed in.** Refresh tokens rotate on every use, but the old token isn't thrown away the instant it rotates — it stays valid for a short grace window (`OAUTH_REFRESH_GRACE_SECONDS`, default 120s) and replays during that window return the same new pair. claude.ai keeps several connections open and can refresh the same token from more than one at once; without the grace window, all but the first refresh would fail with `invalid_grant` and claude.ai would drop the connection and prompt you to sign in again. Token state is also persisted to Valkey with AOF enabled, so a `docker compose restart` doesn't lose sessions. If you still get logged out sooner than `OAUTH_REFRESH_MAX_DAYS`, that's the place to look. The login form is rate-limited per IP (`OAUTH_LOGIN_MAX_ATTEMPTS` / `OAUTH_LOGIN_WINDOW_SECONDS`) to blunt brute-force attempts on the admin password.

OAuth works alongside bearer token auth. If you have both `OAUTH_ENABLED` and `MCP_AUTH_TOKEN` set, both authentication methods are accepted via `MultiAuth`. Local Claude Code instances can continue using bearer tokens while claude.ai uses OAuth.

---

## Web UI

OmniMem includes a browser-based management interface at `http://localhost:8080`. It connects directly to Valkey and does not depend on the MCP server running.

| Page | What it does |
|---|---|
| **Dashboard** | Namespace counts, state breakdowns, health indicators, recent activity |
| **Memories** | Browse all memories with namespace, state, and project filters. Paginated, htmx-powered |
| **Search** | Semantic search using the full recall pipeline. Abandoned warnings highlighted |
| **Detail** | Full memory content, metadata, tags, experience data, contradictions. Lifecycle action buttons |
| **Create** | Store a new memory with duplicate detection shown inline |
| **Projects** | List, view, edit, and create project contexts |
| **Experience** | Summary dashboard with effort stats, breakthroughs, and the abandoned approach graveyard |
| **Duplicates** | Scan a namespace for near-identical memory clusters. Archive extras directly |
| **Contradictions** | Side-by-side comparison of contradicting memories with resolve actions |
| **Suppressions** | Add and remove suppressed topics inline |
| **Telemetry** | Recall counters, most recalled, gone cold, never recalled. Filter by project |
| **Token Overhead** | Measured tool call metrics since uptime: calls, avg duration, avg tokens, errors per tool. Static context cost breakdown |
| **Backups** | Create backups, preview restore contents, and confirm restore |

### Prometheus metrics

The web UI exposes a `/metrics` endpoint in Prometheus text format. Point your Grafana or Prometheus scraper at `http://localhost:8080/metrics` with a 15-60 second scrape interval.

Available gauges:

| Metric | Labels | Description |
|---|---|---|
| `omnimem_memories_total` | `namespace`, `state` | Total memories by namespace and lifecycle state |
| `omnimem_memories_never_recalled` | `namespace` | Active memories with zero recalls |
| `omnimem_recalls_total` | — | Sum of all recall counts across all memories |
| `omnimem_memories_gone_cold` | — | Memories recalled before but not within the cold threshold |
| `omnimem_tool_calls_total` | `tool` | Total MCP tool call count by tool name |
| `omnimem_tool_errors_total` | `tool` | Total MCP tool call errors by tool name |

Metrics are cached for 60 seconds (configurable via `METRICS_CACHE_TTL`) to avoid scanning all memories on every scrape.

The web UI supports optional bearer token authentication via the `WEB_UI_AUTH_TOKEN` environment variable. The `/metrics` endpoint is exempt so Prometheus can scrape without credentials. For additional security options (TLS, IP allowlisting, SSO), see `docs/reverse-proxy.md`.

---

## Architecture

```
  Claude Code (any machine)           Browser
         |                               |
         |  SSE / MCP                    |  HTTP :8080
         v                               v
  +-------------------------+   +-------------------------+
  |   OmniMem MCP Server    |   |    OmniMem Web UI       |
  |   Python  fastmcp       |   |    Starlette  htmx      |
  |                         |   |    Jinja2 templates      |
  |  remember  recall       |   |                         |
  |  deprioritise  archive  |   |  Dashboard  Search      |
  |  record_experience      |   |  Browse  Create         |
  |  warn_if_abandoned      |   |  Projects  Experience   |
  |  briefing  health       |   |  Duplicates  Backups    |
  +-----------+-------------+   +-----------+-------------+
              |                             |
              +-------------+---------------+
                            |
              +-------------+-------------+
              |                           |
              v                           v
      +---------------+         +------------------+
      |    Valkey     |         |   RSS Worker     |
      |  + search     |  <---   |                  |
      |               |         |  feedparser      |
      | idx:episodic  |         |  APScheduler     |
      | idx:project   |         |  Claude Haiku    |
      | idx:knowledge |         +------------------+
      +---------------+

  Both the MCP server and web UI connect directly to Valkey
  and share the mcp_server/memory/ package.

  Recall pipeline:
    query
      -> abandoned fast-path (keyword scan, no embedding needed)
      -> embed query
      -> vector search, top 20 candidates per namespace
      -> filter archived and deleted
      -> filter suppressed topics
      -> apply surface_score (lifecycle state multiplier)
      -> apply recency decay (age penalty after 90 days)
      -> apply experience_weight (effort x outcome multiplier)
      -> check reinstate eligibility
      -> surface contradiction warnings
      -> merge, re-rank, return top_k
      -> log recall event + increment per-memory recall counters
```

---

## Contributing

Issues and PRs are welcome. OmniMem is designed to be extended and the scoring pipeline is structured so new multipliers can be added without touching the core. New MCP tools, additional namespace types, and alternative embedding backends are all reasonable directions.

---

## Licence

MIT. Free to use, fork, and modify. No enterprise tier, no hosted version, no strings.

---

*Built by Ric Harvey @ [SquareCows Ltd](https://squarecows.com), an AI and automation consultancy for people who would rather own their tools.*
