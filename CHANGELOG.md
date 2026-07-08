# Changelog
All notable changes to omnimem are documented here.
Format: [version] - date - description

## [Unreleased]

## [5.3.1] - 2026-07-08
### Fixed
- **Fact extraction now supplements verbatim content instead of competing with it** ([#20](https://codeberg.org/ric_harvey/omnimem/issues/20)): extracted facts were stored in the same namespace as their source chunks, so compact 1-2 sentence facts crowded the richer verbatim content out of the per-namespace KNN candidate budget — v5-full mode scored 33.6% on LongMemEval against v5-raw's 65.6%, with temporal reasoning collapsing to 7.5%. Four changes land together:
  - **Facts route to the `knowledge` namespace** (preferences still go to `preference`). Verbatim chunks keep their own episodic candidate budget; both are searchable at recall time
  - **`event_date` fallback chain**: an extracted fact carries its own date if Claude found one, else it inherits the source memory's `event_date`, else the source's ingest time. `enqueue()`/`enqueue_batch()` now pass `created_at` through the queue payload so batch mode gets timestamps too. Without this, extraction stripped the temporal anchor and date-shaped queries couldn't find the fact
  - **Verbatim-first ranking**: facts are written with `surface_score` 0.5 so the original wording outranks its own derived fact on direct recall, and when a fact and its `enriched_from` source both match, the merge keeps only the source — promoted to the fact's score if the fact ranked higher, so the verbatim stands in at the fact's rank instead of a near-duplicate burning a result slot
  - **Knowledge results carry `project`, `event_date`, `tags` and `enriched_from`**: `_NAMESPACE_RETURN_FIELDS` for knowledge was missing `project` entirely, so a project-filtered recall silently dropped every knowledge result. The knowledge index also gains an indexed `project` tag (picked up by the startup index migration) so the project filter is pushed into the vector search
### Changed
- **Recall dedupe is always on** (was only under query expansion), keyed by (key, result type) so an abandoned-approach warning never collapses into the memory that carries it
- No data migration needed: existing enriched facts stay where they are and age out naturally; new facts take the new path

## [5.3.0] - 2026-07-08
### Fixed
- **claude.ai re-authenticating too often**: OAuth refresh-token rotation was strict single-use — the old token was deleted the instant it rotated. claude.ai keeps several connections open and can refresh the same token concurrently (or retry a request), so every refresh after the first failed with `invalid_grant`, which claude.ai surfaces as a dropped connection and a fresh sign-in prompt. `exchange_refresh_token()` now retires the old token with a short grace window instead: during the window it stays loadable and replays return the *same* successor pair, so concurrent/retried refreshes all succeed. Configurable via `OAUTH_REFRESH_GRACE_SECONDS` (default 120s, capped 3600, `0` restores strict single-use). The `rotated_to` marker is persisted in the Valkey storage backend so it survives restarts
- **OAuth tokens could be lost on restart**: Valkey now runs with AOF persistence (`--appendonly yes --appendfsync everysec`) so tokens issued shortly before a `docker compose restart` are durable, rather than relying on periodic RDB snapshots
- **Web UI restore was broken**: `restore_backup` unpacked `restore_all()` as a 2-tuple when it returns three values (crashing every restore), and didn't re-embed restored memories so they stayed invisible to search. Both fixed — it now mirrors the MCP `restore_from_file` tool
### Security
- **Constant-time token comparison**: MCP and web UI bearer tokens are compared with `hmac.compare_digest` instead of `==`, closing a timing side-channel
- **Fail closed on exposure**: the MCP server refuses to start unauthenticated when `MCP_HOST` is a non-loopback address; Docker Compose refuses to start if `VALKEY_PASSWORD` is empty
- **Path-traversal hardening**: web UI backup preview/restore/download/delete now validate the filename against a strict pattern and confirm the resolved path stays inside the backup directory (previously only download/delete did)
- **Denial-of-service caps**: uploaded and restored backups are limited to 100 MB, and the RSS worker caps page fetches at 10 MB (`RSS_MAX_PAGE_BYTES`) so a hostile or endless response can't exhaust memory
- **Login brute-force protection**: failed OAuth admin logins are rate-limited per client IP (`OAUTH_LOGIN_MAX_ATTEMPTS` / `OAUTH_LOGIN_WINDOW_SECONDS`)
### Changed
- **Projected batch fetches**: new `ValkeyStore.get_fields_multi(keys, fields)` does a single-round-trip HMGET of only the fields a view needs (half the round trips of `get_multi`, no large-field or vector payload). Applied to the scan-all hot paths — the recall abandoned-approach fast path (ran a full-field episodic scan on *every* recall), `briefing()` session-start scans, `experience_summary`, `memory_audit`, and the web UI dashboard, memories, telemetry, token-overhead and metrics pages
- **memory_audit is paginated**: the per-memory `entries` list now takes `limit` (default 100, max 500) and `offset`; the state-count summary still covers the whole store. Stops a large store returning thousands of rows in one MCP response
- **Web UI gzip**: responses are gzip-compressed, and the contradictions page batches its "other memory" lookups into one fetch instead of one-per-pair (N+1)

### Recall quality (full MCP server function audit)
- **Project and state filters are pushed into the vector search**: recall previously fetched a fixed 20 KNN candidates per namespace and filtered by project/state in Python afterwards, so a busy store could fill all 20 slots with other projects' memories and return nothing for the one you asked about. `recall(project_filter=…)` now builds an FT.SEARCH tag filter (`@project:{name}` plus `(@state:{active} | @state:{deprioritised})`) so candidates are in-scope from the start. Syntax was validated against live valkey-search, which behaves differently from the RediSearch docs: raw tag values match (including spaces and dots) while backslash-escaped or quoted values match nothing, and in-brace `{a|b}` alternation is broken — clause-level OR works. Values are interpolated raw only when they match the project-name allowlist; a failed filtered search retries unfiltered so recall degrades instead of going dark
- **Candidate over-fetch**: the per-namespace KNN candidate count now scales with the request (`max(20, top_k)`, min 50 under a project filter) — previously `recall(top_k=50)` in one namespace could never return more than 20 results
- **Query-expansion variants score consistently**: variant searches now apply the temporal boost and carry `event_date`, matching the main query loop (date-anchored memories no longer rank lower when found via a variant)
- **Abandoned-approach cache**: the per-recall episodic scan behind the abandoned-approach fast path is cached for `ABANDONED_CACHE_TTL_SECONDS` (default 60, 0 disables) and invalidated on `record_experience` / `log_abandoned` / `forget` / restore
- **Startup migration** backfills `state=active` on memories that pre-date the state field so the query-side state filter can't hide legacy data

### Fixed (function audit)
- **`restore_from_file` rejected every backup made by `dump_to_file`**: dumps include `meta:*` keys (tool metrics exist as soon as telemetry records one call) but the restore validator didn't allow the `meta:` prefix, so round-tripping your own backup failed with "invalid key prefix". `meta:` is now allowed
- **Async enrichment stored duplicate facts**: the v5.2.0 queue migration dropped the `check_duplicate` guard the synchronous extraction path had, so re-remembering similar content piled up duplicate fact memories. The worker dedups each extracted fact again; the dead synchronous helper was removed
- **`check_contradictions` mass false positives**: the tool never received the 3.12.1 similarity-gating fix that auto-maintenance got, so its no-query path flagged *and cross-linked* unrelated memories sharing words like "use" or "works". It now gates pairs on ≥0.5 cosine similarity (using stored vectors), applies the same comparison/result caps, and its query path no longer links contradictions against archived memories
- **Backup filename validation**: dotted names like `backup.v2.json` were rejected while the error message claimed dots were allowed; the dump metadata also omitted the `preference` namespace count
- **`remember(namespace="project")`** now writes `project_name` at ingest so new project memories are correct immediately instead of after the next restart's migration
- **Duplicate clusters could under-report `max_similarity`** when a later union re-rooted the cluster; the max is now computed against final roots

### Performance (function audit)
- **Dedup and maintenance reuse stored embeddings**: `find_all_duplicates` re-embedded every memory in the namespace on each scan (tens of seconds at the 2,000-key cap — inside `briefing()` whenever auto-maintenance fired). Both it and the maintenance contradiction scan now read the vectors already stored in Valkey via a new binary-safe client (`ValkeyStore.get_vectors_multi`), falling back to the model only for missing vectors. Pairwise comparison is vectorised in numpy instead of a 2M-iteration Python loop
- **`recall_detail` batches** its key lookups (was one round trip per key); `health()` counts all namespaces in one keyspace SCAN instead of four; `list_projects`, `recent_knowledge` and knowledge expiry use projected fetches; deleting a memory no longer writes its fields first

## [5.2.1] - 2026-04-09
### Added
- **`queue_status()` MCP tool** ([#19](https://codeberg.org/ric_harvey/omnimem/issues/19)): returns the number of pending enrichment jobs in the Valkey queue. Benchmark runners can poll this between ingest and scoring phases to know when background fact extraction has completed
- **Dashboard enrichment indicator**: the web UI dashboard now shows enrichment queue status as a health indicator — green when the queue is empty, amber with a count when jobs are pending

## [5.2.0] - 2026-04-09
### Added
- **Async enrichment queue** ([#19](https://codeberg.org/ric_harvey/omnimem/issues/19)): `remember()` and `remember_document()` in full mode now store content raw (embed + write, ~0.25s) and return immediately. A background `EnrichmentWorker` daemon thread pops keys from a Valkey-backed queue (`queue:enrich`), runs fact extraction via Claude Haiku, and writes extracted facts/preferences as new linked memories. The queue persists across mcp_server restarts. The return payload includes `enrichment: "queued"` so callers know enrichment is in progress
- **Batch extraction mode**: when `ENRICHMENT_BATCH_MODE=true`, `remember_document()` sends all chunks as a single enrichment job so the background worker makes one Haiku API call instead of N. Returns `enrichment: "batch_queued"`. Off by default; enable for benchmark runs or large document ingestion where API call reduction matters
- **New env var**: `ENRICHMENT_BATCH_MODE` (default `false`)
### Changed
- **Full-mode ingest is now non-blocking**: the synchronous fact extraction loop that previously ran inline during `remember()` and `remember_document()` has been replaced by the async queue. Callers no longer wait for Haiku API calls to complete before getting a response

## [5.1.2] - 2026-04-09
### Fixed
- **`remember(force=True)` 5x slower than necessary** ([#18](https://codeberg.org/ric_harvey/omnimem/issues/18)): the Tier 1 contradiction heuristic (a vector search + negation scan) was running unconditionally on every `remember()` call, adding ~1.3s per write even when `force=True` should be a raw bypass. Contradiction check and full-mode extracted-fact dedup are now skipped when `force=True`. Expected improvement: ~1.7s → ~0.4s per call, reducing bulk ingestion time from ~59h to ~3.5h for a 500-item LongMemEval run

## [5.1.1] - 2026-04-09
### Fixed
- **`reindex` tool failing with "Index already exists"**: the valkey-py client wrapper's `.dropindex()` method was returning a response the client misinterpreted as an error, so the try/except in `reindex_namespace()` silently swallowed it and the index was never actually dropped. The subsequent `create_index()` then failed because the old index was still in place. Switched to `execute_command("FT.DROPINDEX", idx_name)` directly, which works reliably, and stopped swallowing unexpected errors so any future failures surface to the caller instead of being hidden
- **`health()` missing the preference namespace**: the hardcoded namespace list in the `/health` MCP tool only iterated over episodic/project/knowledge, so the new `idx:preference` index counts and drift were invisible. Now reports all four namespaces

## [5.1.0] - 2026-04-08
### Changed
- **OAuth refresh tokens now extend session lifetime via rotation**: each `/token` refresh issues a new access+refresh pair without re-prompting the user. Refresh tokens carry an `absolute_expires_at` cap that is preserved across rotations, so an active claude.ai connector silently refreshes forever within the configured window and only re-auths once the absolute cap is reached
- **New `OAUTH_REFRESH_MAX_DAYS` env var** controls the absolute lifetime of a refresh-token chain. Default `30`, hard-capped at `90`. Invalid or out-of-range values fall back to the default with a warning
- **Storage backend** persists `absolute_expires_at` so the cap survives `mcp_server` restarts. Pre-upgrade tokens without the field get a fresh cap on first rotation rather than being invalidated

## [5.0.0] - 2026-04-08
### Added
- **`remember_document()` MCP tool** ([#15](https://codeberg.org/ric_harvey/omnimem/issues/15)): a new tool for indexing long-form content. Splits input via one of four chunking strategies (`turn_pairs` for User:/Assistant: transcripts, `sentences`, `paragraphs`, `fixed_tokens`) and stores each chunk as an individual memory linked by a shared `doc_id`, with `chunk_index` and `chunk_strategy` metadata. Driven by the LongMemEval benchmark finding that whole-session storage produced 45% zero-recall on 500-2000 word inputs while turn-pair chunking dropped that to 0%
- **Built-in query expansion for `recall()`** ([#16](https://codeberg.org/ric_harvey/omnimem/issues/16)): new `expand_queries` flag on `recall()` and `recall_index()`. When enabled, generates alternative phrasings of the query via Claude Haiku and unions the results, deduplicating by key and keeping the best `adjusted_score` per memory. Variants are cached in Valkey under `qexp:<sha1>` with a 24h TTL so repeated queries don't re-bill the API. Configurable via `RECALL_EXPAND_QUERIES` (default `false`) and `RECALL_EXPAND_COUNT` (default `3`) env vars
- **Fact extraction at ingest** ([#17](https://codeberg.org/ric_harvey/omnimem/issues/17)): new `mode` parameter on `remember()` and `remember_document()`. In `full` mode (the new default), Claude Haiku extracts atomic declarative facts from raw input before storing each as its own memory. Preference-shaped facts ("I prefer X", "always do Y") are routed automatically to a new `preference` namespace. Each extracted fact carries a `source_doc_id` linking back to the original input. Configurable via `INGEST_MODE` env var (`full` default, `raw` to opt out). Falls back to raw automatically when no API key is set or extraction errors out
- **Preference namespace**: a new fourth memory namespace alongside episodic, project, and knowledge. Preferences are prescriptive ("always update README and CHANGELOG after a feature lands") rather than descriptive. Has its own Valkey search index, key prefix (`mem:preference:`), and pink badge in the web UI. Surfaced on the dashboard, memories list, create form, backups export, telemetry, and duplicates scan
- **Temporal-aware retrieval**: new scoring multiplier in the recall pipeline. When the query contains date language ("yesterday", "last Tuesday", "in March"), `dateparser` extracts the referenced date and any matching memory's `event_date` is used to compute a 1.0–1.5x score boost on a linear falloff (full boost within ±7 days, decaying to 1.0 at ±60 days). A cheap regex pre-filter avoids paying the dateparser cost on every recall — only queries that look temporal trigger the parse. `event_date` is now stored on the episodic and preference indexes and surfaced via `RecallResult` and the `recall()` tool output
- **New env vars**: `INGEST_MODE`, `RECALL_EXPAND_QUERIES`, `RECALL_EXPAND_COUNT`
- **New dependency**: `dateparser>=1.2.0` for temporal query parsing
- **47 new tests** covering chunking strategies, query expansion (cache + variant union + dedupe), preference namespace plumbing, temporal scoring, fact extraction parsing, and ingest mode routing
### Changed
- **Default ingest behaviour**: `remember()` now extracts atomic facts before storing by default. Set `INGEST_MODE=raw` to restore the previous verbatim behaviour. The fallback path is automatic — deployments without an Anthropic key keep working unchanged
- **Recall pipeline searches four namespaces** instead of three (episodic, project, knowledge, preference)
- **Index migration on startup** automatically picks up the new `idx:preference` index — no manual reindex needed on existing deployments

## [4.0.1] - 2026-04-08
### Added
- **Brand icon for the OAuth connector** ([#12](https://codeberg.org/ric_harvey/omnimem/issues/12)): an embedded SVG mark is now served from `/icon.svg`, `/favicon.svg`, `/favicon.ico`, and `/oauth/icon.svg` so claude.ai (and other OAuth MCP clients) can display a recognisable logo for the OmniMem connector regardless of which discovery convention they use
### Fixed
- **OAuth tokens lost on `mcp_server` restart** ([#13](https://codeberg.org/ric_harvey/omnimem/issues/13)): claude.ai sessions used to die with `401 invalid_token` after every restart because all OAuth state lived in plain dicts. State is now persisted in Valkey via a new pluggable storage backend (`oauth/storage.py`) with native TTLs matching `expires_in`. Registered clients survive indefinitely; auth codes, access tokens, and refresh tokens expire automatically. In-memory backend retained as a default for tests and as a fallback if Valkey is unreachable at startup
- **New restart-replay test** verifying tokens issued by one provider instance remain valid via a fresh provider instance sharing the same storage

## [4.0.0] - 2026-04-05
### Added
- **Optional OAuth 2.1 authorisation server**: OmniMem can now act as a full OAuth 2.1 authorisation server for claude.ai and other MCP clients that use OAuth. Enabled via `OAUTH_ENABLED=true` with a single admin user (`OAUTH_ADMIN_USER` / `OAUTH_ADMIN_PASSWORD`). Implements dynamic client registration (RFC 7591), authorisation code flow with PKCE (S256), token refresh with rotation, and token revocation (RFC 7009). Discovery via `/.well-known/oauth-authorization-server` is handled automatically by FastMCP
- **Browser-based login page**: `/oauth/login` serves a styled login form during the OAuth authorisation flow. Users authenticate with admin credentials and are redirected back to the client with an authorisation code
- **MultiAuth support**: When both `OAUTH_ENABLED` and `MCP_AUTH_TOKEN` are set, both authentication methods work simultaneously via FastMCP's `MultiAuth`. Local Claude Code instances can use bearer tokens while claude.ai uses OAuth
- **New env vars**: `OAUTH_ENABLED`, `OAUTH_BASE_URL`, `OAUTH_ADMIN_USER`, `OAUTH_ADMIN_PASSWORD`
- **18 new tests** for OAuth provider covering client registration, credential verification, authorisation flow, token exchange, refresh with rotation, expiry, and revocation
### Changed
- **FastMCP 3.x features used**: OAuth implementation uses `OAuthProvider`, `MultiAuth`, and `custom_route` from FastMCP 3.x. The minimum version constraint (`>=2.13.0`) still applies but OAuth features require 3.x

## [3.13.1] - 2026-04-03
### Fixed
- **Restored memories not searchable until manual intervention** ([#10](https://codeberg.org/ric_harvey/omnimem/issues/10)): `restore_from_file` now automatically re-embeds all restored `mem:*` keys after writing them back to Valkey. Previously, backups excluded binary vector data (required by `decode_responses=True`), so restored memories had no embeddings and were invisible to `recall` and `recall_index`. The restore response now includes a `re_embedded` count
- **`restore_all` return type extended**: Now returns `(restored_count, skipped_count, restored_keys)` so callers know exactly which keys were written
- **2 new tests** for the dump-restore-recall round-trip and selective re-embedding (462 total)
### Removed
- **CVE-2026-4539 `--ignore-vuln` workaround**: Upstream pygments has patched the ReDoS vulnerability in AdlLexer. Removed the `--ignore-vuln CVE-2026-4539` flag from the pip-audit CI step

## [3.13.0] - 2026-03-31
### Added
- **RSS knowledge expiry**: Articles ingested by the RSS worker now get an `expires_at` timestamp set to `created_at + MAX_KNOWLEDGE_AGE_DAYS` (default 30, configurable via env var). Expired items are auto-archived during maintenance Phase 3. Manually stored knowledge items are never affected
- **`recent_knowledge()` tool**: Query recent knowledge articles with filters for `days` (lookback window, default 7), `feed_name`, `topics`, and `limit` (default 20, max 50). Returns articles sorted newest first
- **`promote_knowledge()` tool**: Mark an RSS-ingested knowledge article as permanently useful by clearing its `expires_at` field, preventing auto-archival
- **`NumericField("expires_at")`** added to `idx:knowledge` index — triggers automatic index migration on startup
- **22 new tests** for knowledge expiry, promotion, and the recent_knowledge tool (458 total)

Community contribution from [@timstoop](https://codeberg.org/timstoop) ([PR #9](https://codeberg.org/ric_harvey/omnimem/pulls/9), resolves [#6](https://codeberg.org/ric_harvey/omnimem/issues/6))

## [3.12.1] - 2026-03-30
### Fixed
- **Auto-maintenance contradiction scanner producing mass false positives**: The heuristic negation check (`_has_negation_pair`) was running on all pairwise combinations without checking semantic similarity first. Memories containing common words like "use", "with", or "works" matched against almost everything. Now embeds all content and requires cosine similarity >= 0.5 before checking negation patterns, matching the approach used by the Tier 1 check in `remember()`. Also capped results at 10 contradictions per maintenance run (was unbounded)

## [3.12.0] - 2026-03-30
### Changed
- **Briefing: 3 episodic scans consolidated into 1** — stale memories, contradiction warnings, and reinstate candidates now collected in a single `_scan_episodic_once()` pass instead of three separate `scan_prefix` + `get_multi` calls. Reduces session-start latency by ~200-400ms per 1,000 memories
- **Dashboard: single-pass scan** — state counts and recent memories now collected in one loop per namespace instead of two separate passes
- **Memories page: projects extracted during same scan** — removed the separate `_get_projects()` re-scan of all namespaces; project names now collected alongside memory filtering
- **Metrics endpoint: 60-second cache** — `/metrics` no longer re-scans all memories on every Prometheus scrape. Cache TTL configurable via `METRICS_CACHE_TTL` env var (default 60s)
- **Docker: multi-stage builds** — all three Dockerfiles (mcp_server, web_ui, rss_worker) now use a builder stage with gcc/g++ and a slim runtime stage with only libgomp1/libopenblas0. Removes ~300-500MB of build tools from final images
- **Maintenance: pairwise comparison cap** — heuristic contradiction scan now capped at 2,000 comparisons (was unbounded O(n^2) on up to 200 entries = 19,900 comparisons)
- **Dedup: max_similarity tracked during union-find** — eliminates redundant pairwise re-scan when building cluster results
- **RSS worker: configurable connection pool** — `VALKEY_MAX_CONNECTIONS` env var (default 50, was hardcoded 10)

## [3.11.1] - 2026-03-30
### Changed
- **Token overhead page simplified**: Replaced confusing estimated session ranges with actual measured tool usage since uptime. Single "Tool Usage Since Uptime" table shows real call counts, avg duration, avg/total tokens, errors, and last called per tool
- **Stat cards updated**: Now show total tool calls and total tool tokens (measured) instead of estimated low–high ranges
### Fixed
- **Token Overhead link position** ([#8](https://codeberg.org/ric_harvey/omnimem/issues/8)): Moved navigation links after the Refresh button on both Telemetry and Token Overhead pages so they no longer split the filter/refresh controls

## [3.11.0] - 2026-03-30
### Added
- **Real tool call telemetry**: FastMCP middleware instruments every MCP tool call, recording call count, duration (ms), response size (chars), and errors to Valkey (`meta:tool_metrics:{tool_name}` hashes). Metrics accumulate via atomic `hincrby` counters and never block or break tool calls
- **Measured averages on token overhead page**: The dynamic overhead table now shows a "Measured Tokens/Call" column alongside estimates when telemetry data exists. A new "All Tool Call Metrics" section displays per-tool stats: calls, avg duration, avg response size, avg tokens, errors, and last called timestamp
- **Prometheus tool call metrics**: New `omnimem_tool_calls_total` and `omnimem_tool_errors_total` gauges on `/metrics`, labelled by tool name
- **17 new tests** for telemetry middleware and metrics reader (434 total, 91% coverage)
### Changed
- **Token Overhead moved under Telemetry**: Removed standalone sidebar link; now accessed via a button on the Telemetry page header, with a back link on the Token Overhead page

## [3.10.4] - 2026-03-30
### Fixed
- **Version update tooltip clipped by sidebar** ([#5](https://codeberg.org/ric_harvey/omnimem/issues/5)): The hover tooltip showing "vX.Y.Z is available" was partially hidden because the sidebar's `overflow-y: auto` clips absolute-positioned children. Changed tooltip alignment from centred (`left: 50%; transform: translateX(-50%)`) to right-anchored (`right: 0`) so it extends leftward within the sidebar bounds

## [3.10.3] - 2026-03-30
### Added
- **Token overhead estimation page**: New `/token-overhead` endpoint in the web UI showing estimated token cost of running OmniMem per Claude Code session. Breaks down static overhead (MCP instructions, tool schemas, deferred tool names) and dynamic per-session overhead (briefing, recall, remember, warn_if_abandoned, update_project_state calls). Includes memory store metrics (namespace counts, total content size, average content per memory). Supports project filtering and htmx refresh
- **Sidebar link**: "Token Overhead" added to the web UI sidebar under Telemetry
- **Configurable overhead constants**: `OMNIMEM_INSTRUCTIONS_CHARS` and `OMNIMEM_TOOL_SCHEMAS_CHARS` env vars allow tuning static overhead estimates if instructions or tool count change

## [3.10.1] - 2026-03-27
### Added
- **Update available indicator in web UI**: A warning triangle appears next to the version badge in the sidebar when a newer release is published on Codeberg. Hovering shows the available version number. Checks the Codeberg releases API with a 1-hour cache and 5-second timeout, fails silently if unreachable
- **Claude Desktop connection guide**: New guide at `guides/claude-desktop.md` covering Streamable HTTP and legacy SSE setup via `mcp-remote`, config file locations for macOS and Windows, auth, migration steps, and troubleshooting
- **`MCP_TRANSPORT` in `.env.example`**: Documented as a commented-out option

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
