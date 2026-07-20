# Configuration Reference

All configuration is via environment variables, usually set in your `.env` file. The installer writes sensible defaults for you; this page covers everything you can tune.

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
| `MCP_ALLOWED_HOSTS` | *(unset)* | Comma-separated extra hostnames allowed in the `Host` header, on top of localhost and the `OAUTH_BASE_URL` host. Needed when serving behind a reverse proxy or tunnel under a hostname not already covered by `OAUTH_BASE_URL`, otherwise FastMCP 3.x returns `421 Misdirected Request`. See [Troubleshooting: 421 / 403 behind a proxy](remote-access.md#getting-421-misdirected-request-or-403-forbidden-origin-behind-a-proxy) |
| `MCP_ALLOWED_ORIGINS` | *(unset)* | Comma-separated extra browser origins (full `scheme://host`) trusted for the login page, on top of the `OAUTH_BASE_URL` origin. Needed when the proxy terminates TLS and forwards over http, otherwise the browser login POST gets `403 Forbidden Origin`. See [Troubleshooting](remote-access.md#getting-421-misdirected-request-or-403-forbidden-origin-behind-a-proxy) |
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
| `OMNIMEM_INSTRUCTIONS_CHARS` | `14162` | Calibration for the token-overhead dashboard page: character count of the MCP instructions text |
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
| `SKILL_SCAN_INTERVAL_HOURS` | `24` | How often a `briefing()` may run the auto skill scan that proposes new skills from cross-project lesson patterns and drafts for changed skills. Set to 0 to disable |
| `SKILL_SCAN_MIN_POOL` | `3` | Minimum lesson-bearing memories a domain needs before the scan even checks it for a new skill |
| `SKILL_SCAN_CROSS_PROJECT` | `true` | Require at least one qualifying rule to span two or more projects before auto-proposing a new skill. Set to `false` to propose from single-project patterns too |
| `SKILL_FEED_MAX_ARTICLES` | `25` | Overall cap on the Feed watch section a compiled skill can carry from influencing RSS feeds, trimmed weakest-influence-first. Set to 0 to disable feed influence entirely |
| `SKILL_SCAN_MAX_PROPOSALS` | `3` | Cap on proposals a single scan run may create, so one briefing never floods the review queue |
