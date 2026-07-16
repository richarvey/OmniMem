# \<OmniMem\>
Development happens on Codeberg (https://codeberg.org/ric_harvey/omnimem) — issues and PRs there please.
---



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
- **Skill suggestions** — compiled skills relevant to the current work, as a recommendation rather than an auto-load. On an ongoing project they sit below project context; on a greenfield project with no context yet they move to the top, because there the skill is the only thing carrying your conventions
- **Skill updates** — one-line gists where a skill's source memories changed since it was last compiled, with prominence scaled to risk

One tool call, one response, full context.

### Automatic maintenance

Memory systems accumulate duplicates and contradictions over time. OmniMem handles this automatically.

Every N `briefing()` calls per project (default 10, configurable via `AUTO_MAINTENANCE_INTERVAL`), the server runs a maintenance pass:

1. **Dedup scan** — finds clusters of near-identical episodic memories and archives the oldest in each cluster, keeping the newest
2. **Contradiction scan** — checks semantically similar active project memories for negation pattern mismatches (requires cosine similarity >= 0.5 before checking, capped at 10 results)
3. **Knowledge expiry** — archives RSS-ingested knowledge articles that have passed their `expires_at` timestamp (default 30 days after ingestion, configurable via `MAX_KNOWLEDGE_AGE_DAYS`). Manually stored knowledge items are never affected

The results appear in the briefing response under `auto_maintenance` so you know what was cleaned up. Set `AUTO_MAINTENANCE_INTERVAL=0` to disable. Manual `find_duplicates()` and `check_contradictions()` calls still work as before.

### The skill compiler

Memories tell an agent what happened. A skill tells it how you work.

A skill in OmniMem is not a set list of instructions, and it is never finished. Skills are built the way you actually build a skill: by doing the work, failing at some of it, succeeding at the rest, and noticing which lessons keep coming back. OmniMem compiles them from exactly that record, the work you did and the outcomes it produced. And they keep developing. As new experience and new dead ends land in memory, the skill evolves over time with your input and approval, so the agent gets faster and more precise at helping you reach your outcomes and goals.

`compile_skill("python")` distils your accumulated experience in a domain — reinforced breakthroughs, recurring gotchas, the graveyard of dead ends — into a loadable `SKILL.md`: do this, watch out for that, never try X again because it cost you an afternoon on that other project. Each rule cites the memories it came from. Load it at the start of Python work (or Rust work, or blog writing) and the agent works your way from the first prompt, which matters most on greenfield projects where no project context exists yet.

The design premise: a memory error is noise, ranked and diluted by recall, but a skill error is policy — the agent obeys it. So bad lessons cannot become policy silently:

- **A pattern earns a rule, an episode doesn't.** Lessons must recur across `min_reinforcement` memories (default 2) before they compile. One strong lesson can jump the queue if you `bless()` it.
- **Nothing writes silently.** `compile_skill` proposes a diff with a risk-classified change summary; you review it, then `mode="write"` commits exactly what you accepted. Recompiles that rewrite or remove an existing rule are flagged loudly; simple additions stay cheap.
- **Derived-only, never hand-edited.** The raw memories are the source of truth and the skill is build output. To change the guidance, update the memories and recompile.
- **Suggested, never auto-loaded.** The briefing recommends relevant skills; you and the agent decide.
- **Reference material is promoted, never absorbed.** Knowledge articles only reach a skill through `promote_knowledge(key, domain=...)` — a deliberate act that substitutes for the reinforcement an article can't earn. Promoted articles compile into a distinct Reference section, each citing its source. An article with discrete guidance (a "5 things to avoid" list) can be promoted with `rules=[{kind, text}, ...]` — the agent reads it, drafts the items, you approve them, and each becomes its own Avoid/Do/Watch bullet rather than one summary line. Extraction happens at promotion under review, never at compile, so compilation stays deterministic. Volatile facts (version numbers, latest releases) should stay in the knowledge namespace and be looked up with `recall()` instead.

Every skill carries a fixed operating contract that instructs the agent to keep recording experience and dead ends while working under it. That closes the flywheel: the data pool compiles into a skill, the skill keeps feeding the pool, and a richer pool compiles a better skill next time.

The RSS feed acts as an early-warning system for the skills you've compiled: the briefing's knowledge watch compares recent articles against each skill and surfaces the ones that look relevant, flagging a possible contradiction when an article's language opposes one of the skill's rules. Nothing changes automatically — you review, promote the article into the skill if it belongs there, or ignore it and let it age out of the watch window.

Skills live whole in Valkey (`mem:skill:gen:{domain}-{user}`) — discovery metadata is embedded and searchable, the body is retrieved intact, and `export_path` can mirror a copy to disk. Domains are free-form tags with a "did you mean" guard, so `py` resolves to `python` instead of silently scattering your lessons across tags that never reach the threshold.

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

The fastest route is the installer, which uses the pre-built Docker Hub images. It checks Docker is installed, generates secure passwords, asks whether the MCP server should be reachable from your network, writes a sensible `.env`, and starts everything:

```bash
curl -fsSL https://codeberg.org/ric_harvey/omnimem/raw/branch/main/install.sh | bash
```

Works on macOS and Linux. See [guides/docker-hub.md](guides/docker-hub.md) for the manual version of the same setup.

Or build from source instead:

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
| Open Design | [guides/open-design.md](guides/open-design.md) | Streamable HTTP + OAuth 2.1 (public/PKCE) |
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
| `retag(key, tags?, add?, remove?)` | Replace or adjust a memory's tags without re-embedding. Pass `tags` for a full replacement (`[]` clears), or `add`/`remove` to tweak the existing set |
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
| `delete_project(name, confirm?, include_context?)` | Bulk delete every memory belonging to a project by direct key scan (no semantic search, so nothing gets missed). Preview by default; `confirm=True` deletes in pipelined batches; `include_context=True` also removes the project context entry |

### Experience scoring

| Tool | What it does |
|---|---|
| `record_experience(key, effort_score, outcome, abandoned_approaches?, breakthrough?, gotchas?)` | Log how hard it was and what failed |
| `log_abandoned(key, name, type, reason)` | Add dead ends incrementally mid-session |
| `warn_if_abandoned(query)` | Check the graveyard before proceeding |
| `experience_summary(project?)` | Graveyard, breakthroughs, and effort stats |
| `get_experience(key)` | Full experience data for one memory |

### Skill compiler

| Tool | What it does |
|---|---|
| `compile_skill(domain, mode?, min_reinforcement?, include_graveyard?, export_path?, description?)` | Compile a domain's experience and graveyard into a `SKILL.md`. `propose` (default) returns a reviewable diff and change summary; `write` commits only a previously proposed and accepted draft — no silent writes. `export_path` mirrors the file under `SKILL_EXPORT_DIR` |
| `find_skills(query_or_domain)` | Ranked skill discovery over indexed metadata. Exact domain matches lead; a hand-authored skill outranks a generated one on the same domain |
| `get_skill(skill_id)` | Load the whole skill body intact, by key (`mem:skill:gen:python-ric`), name (`python-ric`), or bare domain (`python`) |
| `bless(memory_key)` | Promote one strong lesson to skill-eligible now, bypassing the reinforcement threshold at the next compile |

### Knowledge

| Tool | What it does |
|---|---|
| `recent_knowledge(days?, feed_name?, topics?, limit?)` | Query recent RSS articles with optional filters, sorted newest first |
| `promote_knowledge(key, domain?, demote?, rules?)` | Mark an article as permanently useful by clearing its expiry. With `domain`, also mark it skill-eligible: the next `compile_skill()` for that domain renders it in the skill's Reference section. Pass `rules=[{kind, text}, ...]` to extract an article's discrete guidance into individual stance-prefixed Reference rules (reviewed at promotion, so compiles stay deterministic). `demote=True` removes a domain again |

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
| `VALKEY_HOST` | `valkey` | Hostname of the Valkey server (the Compose service name; only change it if you point at an external Valkey) |
| `VALKEY_PORT` | `6379` | Port of the Valkey server |
| `ANTHROPIC_API_KEY` | required | For RSS summarisation via Claude Haiku |
| `MCP_AUTH_TOKEN` | *(unset)* | Set to enable bearer token auth on the MCP endpoint (constant-time compared). When unset, no auth is required — but the server refuses to start unauthenticated on a non-loopback `MCP_HOST` |
| `WEB_UI_AUTH_TOKEN` | *(unset)* | Set to enable bearer token auth on the web dashboard (constant-time compared). `/metrics` and static assets are exempt |
| `WEB_UI_LOGIN_ENABLED` | *(auto)* | The web dashboard shows a login page whenever `OAUTH_ADMIN_USER` and `OAUTH_ADMIN_PASSWORD` are set — the same credentials as the OAuth flow. Set to `false` to opt out |
| `WEB_UI_SESSION_HOURS` | `168` | Dashboard session lifetime. Sessions are opaque tokens stored in Valkey, revoked server-side on sign out |
| `OAUTH_ENABLED` | *(unset)* | Set to `true` to enable OAuth 2.1 authorisation server for claude.ai and other OAuth MCP clients |
| `OAUTH_BASE_URL` | *(unset)* | Externally-reachable URL of your OmniMem instance (e.g. `https://mcp.example.com`). Required when OAuth is enabled |
| `OAUTH_ADMIN_USER` | *(unset)* | Username for the OAuth admin account. Required when OAuth is enabled |
| `OAUTH_ADMIN_PASSWORD` | *(unset)* | Password for the OAuth admin account. Required when OAuth is enabled |
| `OAUTH_REFRESH_MAX_DAYS` | `30` | Absolute lifetime of an OAuth refresh-token chain. Each rotation silently re-issues tokens without re-prompting the user, until this cap is reached. Hard-capped at `90` |
| `OAUTH_REFRESH_GRACE_SECONDS` | `120` | Grace window after a refresh token rotates. The old token keeps working for this long and replays return the same new pair, so claude.ai isn't logged out when several connections refresh at once. Capped at `3600`; set `0` for strict single-use rotation |
| `MCP_ALLOWED_HOSTS` | *(unset)* | Comma-separated extra hostnames allowed in the `Host` header, on top of localhost and the `OAUTH_BASE_URL` host. Needed when serving behind a reverse proxy or tunnel under a hostname not already covered by `OAUTH_BASE_URL`, otherwise FastMCP 3.x returns `421 Misdirected Request`. See [Troubleshooting: 421 / 403 behind a proxy](#getting-421-misdirected-request-or-403-forbidden-origin-behind-a-proxy) |
| `MCP_ALLOWED_ORIGINS` | *(unset)* | Comma-separated extra browser origins (full `scheme://host`) trusted for the login page, on top of the `OAUTH_BASE_URL` origin. Needed when the proxy terminates TLS and forwards over http, otherwise the browser login POST gets `403 Forbidden Origin`. See [Troubleshooting](#getting-421-misdirected-request-or-403-forbidden-origin-behind-a-proxy) |
| `OAUTH_LOGIN_MAX_ATTEMPTS` | `10` | Failed admin logins per client IP before the login form is temporarily blocked. Set `0` to disable |
| `OAUTH_LOGIN_WINDOW_SECONDS` | `900` | Sliding window for the failed-login limit |
| `RSS_MAX_PAGE_BYTES` | `10485760` | Max bytes the RSS worker reads when fetching a full article page (10 MB), guarding against hostile or endless responses |
| `ABANDONED_CACHE_TTL_SECONDS` | `60` | How long the recall pipeline caches the parsed abandoned-approach list between episodic rescans. Invalidated automatically on experience writes in the same process; set `0` to rescan on every recall |
| `DASHBOARD_STATS_TTL` | `60` | How long the web UI caches dashboard stats (namespace counts + recent list) in Valkey, so the page doesn't rescan the keyspace on every load. The page shows when stats were computed and offers a refresh link; set `0` to recompute on every load |
| `MCP_PORT` | `8765` | Port the MCP server listens on |
| `MCP_HOST` | `127.0.0.1` | Bind address for the MCP server (set to `0.0.0.0` inside Docker) |
| `MCP_TRANSPORT` | `sse` | MCP transport: `http` (Streamable HTTP, recommended) or `sse` (deprecated default, will be removed in a future release) |
| `VALKEY_MAX_CONNECTIONS` | `20` | Valkey connection pool size for the MCP server and web UI (the RSS worker defaults to `50` when unset) |
| `VALKEY_RAW_MAX_CONNECTIONS` | `4` | Pool size for the second binary-safe Valkey client that reads stored vectors (dedup, maintenance, contradiction checks) |
| `OAUTH_VALKEY_MAX_CONNECTIONS` | `5` | Pool size for the OAuth token store's dedicated Valkey client |
| `EMBEDDING_MODEL` | `all-MiniLM-L6-v2` | Local sentence-transformers model |
| `RSS_SCHEDULE_HOURS` | `6` | How often feeds are ingested |
| `RSS_MAX_ARTICLES_PER_FEED` | `20` | Articles per feed per cycle |
| `RSS_MAX_DIGEST_ENTRIES` | `2` | Entries ingested per cycle for feeds set to `mode: digest` in feeds.yml |
| `FEEDS_CONFIG_PATH` | `/app/feeds.yml` | Path to feeds.yml inside the RSS worker and web UI containers |
| `FEEDS_WATCH_INTERVAL` | `10` | Seconds between mtime polls of feeds.yml for change detection (inotify doesn't work on Docker bind mounts) |
| `MEMORY_RECALL_TOP_K` | `5` | Default number of recall results |
| `DEPRIORITISED_WEIGHT` | `0.2` | Surface score for deprioritised memories |
| `RECENCY_DECAY_DAYS` | `90` | Days before the age penalty kicks in |
| `INGEST_MODE` | `full` | `full` stores content verbatim then extracts atomic facts via Claude Haiku in the background — facts land in the knowledge namespace (preferences in the preference namespace) as supplements to the verbatim original, inheriting its timestamp so temporal recall works; `raw` stores verbatim only. Falls back to raw automatically when no API key is set |
| `FACT_EXTRACTION_MODEL` | `claude-haiku-4-5-20251001` | Claude model used for background fact extraction in `full` ingest mode |
| `RECALL_EXPAND_QUERIES` | `false` | Globally enable query expansion on `recall()`. Generates alternative phrasings via Claude Haiku and unions the results |
| `RECALL_EXPAND_COUNT` | `3` | Number of variant queries to generate when expansion is enabled |
| `QUERY_EXPANSION_MODEL` | `claude-haiku-4-5-20251001` | Claude model used to generate query expansion variants |
| `ENRICHMENT_BATCH_MODE` | `false` | When `true`, `remember_document()` sends all chunks as a single enrichment job so the background worker makes one Haiku API call instead of N. Faster for large documents and benchmark runs |
| `DEDUP_SIMILARITY_THRESHOLD` | `0.92` | Cosine similarity threshold for duplicate detection on `remember()` |
| `CONTRADICTION_SIMILARITY_THRESHOLD` | `0.7` | Similarity threshold for contradiction candidate search |
| `STALE_MEMORY_DAYS` | `30` | Days without update before a memory is flagged as stale in `briefing()` |
| `AUTO_MAINTENANCE_INTERVAL` | `10` | Number of `briefing()` calls per project before auto-maintenance runs (0 to disable) |
| `MAX_KNOWLEDGE_AGE_DAYS` | `30` | Days before RSS-ingested knowledge articles expire and are auto-archived during maintenance |
| `METRICS_CACHE_TTL` | `60` | Seconds to cache `/metrics` endpoint results between Prometheus scrapes |
| `TELEMETRY_COLD_DAYS` | `60` | Days without recall before a memory is flagged as "gone cold" on the telemetry dashboard |
| `OMNIMEM_INSTRUCTIONS_CHARS` | `12512` | Calibration for the token-overhead dashboard page: character count of the MCP instructions text |
| `OMNIMEM_TOOL_SCHEMAS_CHARS` | `7620` | Calibration for the token-overhead dashboard page: total character count of the tool schemas |
| `WEB_PORT` | `8080` | Port the web UI listens on |
| `BACKUP_DIR` | `/app/backups` | Where backup files are written (shared between MCP server and web UI) |
| `OMNIMEM_USER` | `local` | Identity segment in generated skill keys (`mem:skill:gen:{domain}-{user}`) and the "How {user} works in..." description draft. Single-node label only — auth and org scoping are v7 |
| `SKILL_CLUSTER_THRESHOLD` | `0.80` | Cosine similarity above which two lessons count as the same lesson for reinforcement. Looser than dedup's 0.92 because the same lesson is phrased differently across episodes |
| `SKILL_DOMAIN_SUGGEST_THRESHOLD` | `0.60` | Similarity floor for the domain "did you mean" guard when a compile finds no candidates |
| `SKILL_PROPOSAL_TTL_SECONDS` | `86400` | How long a proposed skill diff stays committable via `compile_skill(mode='write')` before it expires and must be re-proposed |
| `SKILL_SUGGEST_MIN_SIMILARITY` | `0.30` | Similarity floor for skill suggestions in `briefing()` on projects that already have context |
| `SKILL_EXPORT_DIR` | `/app/backups/skills` | Root directory for optional `export_path` mirrors of compiled skills. Valkey stays the canonical store |
| `SKILL_KNOWLEDGE_WATCH_DAYS` | `14` | Lookback window for the briefing's knowledge watch — how long a recent article can keep flagging itself as relevant to a compiled skill. Set to 0 to disable |
| `SKILL_KNOWLEDGE_WATCH_THRESHOLD` | `0.35` | Similarity floor between an article and a skill's discovery embedding before the knowledge watch surfaces it |

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
    project: automation-research   # optional, defaults to "RSS"
```

Each article gets fetched, stripped of HTML, summarised to a couple of sentences by Claude Haiku, embedded, and stored in the `knowledge` namespace with an `expires_at` timestamp (default 30 days, configurable via `MAX_KNOWLEDGE_AGE_DAYS`). Articles are labelled with the project `RSS` (or the feed's own `project:` label if you set one) so ingested content stays separable from knowledge captured in conversation — filter by project in the web UI, or pass `project="RSS"` to `recall()` to search only articles. Expired articles are auto-archived during maintenance. If an article turns out to be genuinely useful, call `promote_knowledge(key)` to clear its expiry and keep it permanently — or `promote_knowledge(key, domain="python")` to also feed it into that domain's compiled skill as reference material. Duplicates are skipped by URL. The worker runs once on startup and then on whatever schedule you set in `RSS_SCHEDULE_HOURS`.

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

You can expose the web UI the same way — add a route for `WEB_PORT`. With `OAUTH_ADMIN_USER` and `OAUTH_ADMIN_PASSWORD` set, the dashboard's own login page guards it with the same credentials as the OAuth flow; `docs/reverse-proxy.md` has Traefik and Caddy examples if you'd rather (or additionally) gate it at the proxy.

Security checklist: strong `VALKEY_PASSWORD` (Compose now refuses to start if it's empty), set `MCP_AUTH_TOKEN` in your `.env` (with OAuth credentials configured the web dashboard requires a login automatically; add `WEB_UI_AUTH_TOKEN` if scripts need bearer access), TLS on the proxy if exposing publicly, and keep the Valkey port off the public internet. Bearer tokens are compared in constant time, the MCP server won't start unauthenticated on a non-loopback address, backup filenames are validated against path traversal, and uploaded/restored backups and fetched RSS pages are size-capped.

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

#### Getting `421 Misdirected Request` or `403 Forbidden Origin` behind a proxy

If connecting through a reverse proxy or tunnel suddenly stops working, this is FastMCP 3.x's Host/Origin guard, not an OAuth problem — it just looks like one. There are two symptoms:

- **`421 Misdirected Request`** on everything (e.g. `curl https://your-host/mcp` returns `421` where it used to return `401`). FastMCP rejects any `Host` header outside its allowlist, which defaults to just localhost plus the bind address. Your public hostname isn't on it, so every request 421s, including the `/.well-known/*` discovery endpoints claude.ai probes first. A local `curl` to `localhost` still works, which makes it easy to misread as an OAuth fault.
- **`403 Forbidden Origin`** when you submit the login form. Most proxies terminate TLS and forward over plain http, so the server sees the request as `http://` while your browser sends `Origin: https://your-host`. FastMCP treats that scheme mismatch as an untrusted origin and blocks the login POST.

The server allows the `OAUTH_BASE_URL` (and `MCP_PUBLIC_URL`) hostname **and** its https origin automatically, so with OAuth configured correctly both usually just work. If you serve under an additional hostname or origin, add them:

```bash
MCP_ALLOWED_HOSTS=mcp.example.com,alt.example.com
MCP_ALLOWED_ORIGINS=https://mcp.example.com
```

You can also set FastMCP's raw knobs directly, but they're JSON arrays — a bare string won't parse:

```bash
FASTMCP_HTTP_ALLOWED_HOSTS=["mcp.example.com"]
FASTMCP_HTTP_ALLOWED_ORIGINS=["https://mcp.example.com"]
```

Quick check: `curl -s -o /dev/null -w '%{http_code}\n' https://your-host/.well-known/oauth-authorization-server` should return `200`. A `421` means the hostname isn't allowlisted; a `403` on login means the origin isn't.

---

## Web UI

OmniMem includes a browser-based management interface at `http://localhost:8080`. It connects directly to Valkey and does not depend on the MCP server running.

The UI wears the SquareCows palette in two themes: dark (brand navy with orange accents, the default) and a warm paper light theme, switched from a button in the footer. Your first visit follows the browser's colour scheme preference; after that the choice sticks in localStorage. Both themes hold WCAG 2.2 AA contrast throughout, and everything — including the self-hosted Ubuntu fonts — works fully offline.

The sidebar is organised into groups: **Memory** (memories, projects, preferences, experience, graveyard), **Skills** (compiled skills), **Management** (duplicates, contradictions, suppressions), **Knowledge Management** (articles, learned knowledge, RSS feeds), and **System Management** (telemetry, backups).

| Page | What it does |
|---|---|
| **Dashboard** | A composition bar showing each namespace's share of the corpus, namespace cards with state breakdowns, health indicators, recent activity. The projects card counts distinct projects by state (active, deprioritised, archived) and the skills card shows compiled skills plus any pending compile proposals |
| **Memories** | Browse all memories with namespace, state, and project filters, plus inline deprioritise/delete actions on each row. A recall-heat rule on each row fades with time since the memory last surfaced in recall. Paginated, htmx-powered. The Preferences, Articles, and Learned Knowledge sidebar entries are filtered views of this page |
| **Search** | Semantic search using the full recall pipeline. Abandoned warnings highlighted |
| **Detail** | Full memory content, metadata, tags, experience data, contradictions. Lifecycle action buttons, plus inline tag editing (comma-separated, same validation as the `retag` tool) |
| **Create** | Store a new memory with duplicate detection shown inline |
| **Projects** | List, view, edit, and create project contexts |
| **Skills** | Browse compiled skills: rules with reinforcement counts and source citations, the full SKILL.md body, load counters, and the source manifest linking back to the memories each skill was compiled from. The New Skill modal compiles a draft for a domain through the same propose-and-accept gate as the MCP `compile_skill` tool — you review the draft in place and nothing is written until you accept it. Skills can be deleted (with confirmation) but never edited: to change one, update the underlying memories and recompile |
| **Experience** | Summary dashboard with effort stats, breakthroughs, and a paginated most-effortful table filterable by outcome (succeeded, pivoted, abandoned). The abandoned approach graveyard has its own page |
| **Duplicates** | Scan a namespace for near-identical memory clusters. Archive extras directly |
| **Contradictions** | Side-by-side comparison of contradicting memories with resolve actions |
| **Suppressions** | Add and remove suppressed topics inline |
| **Telemetry** | Recall counters, most recalled, gone cold, never recalled. Filter by project. Includes skill load counts (`get_skill` bumps the same counters) |
| **Token Overhead** | Measured tool call metrics: calls, avg duration, avg tokens, errors per tool, counted since the last reset (a Reset Counters button flushes them from Valkey). Static context cost breakdown |
| **Backups** | Create backups, preview restore contents, and confirm restore |

### Prometheus metrics

The web UI exposes a `/metrics` endpoint in Prometheus text format. Point your Grafana or Prometheus scraper at `http://localhost:8080/metrics` with a 15-60 second scrape interval.

Available gauges:

| Metric | Labels | Description |
|---|---|---|
| `omnimem_memories_total` | `namespace`, `state` | Total records by namespace (episodic, project, knowledge, preference, skill) and lifecycle state |
| `omnimem_memories_never_recalled` | `namespace` | Active records with zero recalls |
| `omnimem_recalls_total` | — | Sum of all recall counts across all memories |
| `omnimem_memories_gone_cold` | — | Memories recalled before but not within the cold threshold |
| `omnimem_tool_calls_total` | `tool` | Total MCP tool call count by tool name |
| `omnimem_tool_errors_total` | `tool` | Total MCP tool call errors by tool name |

Metrics are cached for 60 seconds (configurable via `METRICS_CACHE_TTL`) to avoid scanning all memories on every scrape.

The web UI asks for a login whenever the OAuth admin credentials (`OAUTH_ADMIN_USER` / `OAUTH_ADMIN_PASSWORD`) are set — one set of credentials for both the MCP OAuth flow and the dashboard. Sessions are opaque tokens in Valkey (`WEB_UI_SESSION_HOURS`, default 7 days) behind an HttpOnly cookie, revoked server-side by the footer's Sign out button, with failed attempts rate limited per IP. Set `WEB_UI_LOGIN_ENABLED=false` to opt out. Bearer token authentication via `WEB_UI_AUTH_TOKEN` works alongside it for scripts. The `/metrics` endpoint is exempt so Prometheus can scrape without credentials. For additional security options (TLS, IP allowlisting, SSO), see `docs/reverse-proxy.md`.

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
