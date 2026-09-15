# Configuration

OmniMem runs happily with no configuration at all: it listens on `127.0.0.1:8765`, keeps everything local, and turns the Claude Haiku features off until you give it an API key. This page covers everything you can change.

## Where settings live

**The desktop app** has a **Configuration** page in its settings window (**Settings… → Configuration**). Ordinary settings are saved to `omnimem.env` in the [data folder](quick-start.md#install-the-desktop-app). Secrets go to your operating system's keychain (Keychain on macOS, Credential Manager on Windows, the Secret Service on Linux) and are never shown again once saved. Those are:

- `MCP_AUTH_TOKEN`
- `OAUTH_ADMIN_PASSWORD`
- `ANTHROPIC_API_KEY`
- `HF_TOKEN`

Changes apply the next time OmniMem starts, and the page tells you so. Anything the page doesn't list can still go in `omnimem.env` by hand; the page keeps lines it doesn't recognise.

**Headless installs** (`omnimem serve`, the packages, the Docker image) read environment variables only. The packages load them from `/etc/omnimem/omnimem.env`, and [`.env.example`](../.env.example) in the repo is a commented starting point.

**Either way, a real environment variable wins.** If `MCP_PORT` is set in the environment, the desktop app uses it over whatever the Configuration page saved, and the page points that out.

Flags accept `true`, `1` or `yes` (any case); anything else, or leaving it unset, means off. A number that doesn't parse is logged and the default is used.

## The database

| Variable | Default | Description |
|---|---|---|
| `OMNIMEM_DB` | Desktop app: `omnimem.db` in the data folder. Headless: `data/omnimem.db` under the working directory | The SQLite database. `feeds.yml` and `backups/` default to the folder it's in. Also available as `--db` on every command |
| `OMNIMEM_LOG` | `info` for `serve` and the desktop app, `warn` for the other commands | Log level or filter, such as `debug` or `omnimem_mcp=debug,info` |

## MCP server

| Variable | Default | Description |
|---|---|---|
| `MCP_HOST` | `127.0.0.1` | Address to listen on. Anything other than loopback needs `MCP_AUTH_TOKEN` or OAuth, or OmniMem refuses to start. Use `0.0.0.0` inside Docker |
| `MCP_PORT` | `8765` | Port. Clients connect to `http://address:port/mcp` |
| `MCP_AUTH_TOKEN` | *(unset)* | A shared bearer token clients send as `Authorization: Bearer <token>`. Compared in constant time. Works alongside OAuth |
| `MCP_PUBLIC_URL` | *(unset)* | Where clients reach OmniMem through a proxy or tunnel. Its host and origin are trusted automatically |
| `MCP_ALLOWED_HOSTS` | *(unset)* | Extra `Host` headers to accept, comma separated, on top of localhost, the bind address and the hosts of `MCP_PUBLIC_URL` and `OAUTH_BASE_URL`. A host that isn't allowed gets `421 Misdirected Request` |
| `MCP_ALLOWED_ORIGINS` | *(unset)* | Extra browser origins (`scheme://host[:port]`) to accept, comma separated |

See [remote access](remote-access.md) for putting OmniMem behind a reverse proxy, and what 421 and 403 errors mean.

## OAuth

For claude.ai and other clients that sign in rather than send a token. The login page is the only HTML OmniMem serves over the network.

| Variable | Default | Description |
|---|---|---|
| `OAUTH_ENABLED` | off | Turn on the OAuth 2.1 authorisation server. If it's on but the three settings below aren't all set, OmniMem refuses to start and says which is missing |
| `OAUTH_BASE_URL` | *(unset)* | The address clients reach OmniMem at, such as `https://mcp.example.com`. Must be https, except for localhost. Its host and origin are trusted |
| `OAUTH_ADMIN_USER` | *(unset)* | The username the login page asks for |
| `OAUTH_ADMIN_PASSWORD` | *(unset)* | The password it asks for |
| `OAUTH_REFRESH_MAX_DAYS` | `30` | How long a client stays signed in, however often it refreshes. 1 to 90 |
| `OAUTH_REFRESH_GRACE_SECONDS` | `120` | How long a replaced refresh token keeps working, returning the same new tokens, so clients refreshing at once (claude.ai does) aren't signed out. Up to 3600; `0` makes refresh tokens strictly single use |
| `OAUTH_LOGIN_MAX_ATTEMPTS` | `10` | Failed logins from one address before the login page refuses it for a while. `0` turns the limit off |
| `OAUTH_LOGIN_WINDOW_SECONDS` | `900` | How long a failed login counts towards that limit |

Clients, codes and tokens are kept in the database (tokens only as hashes), so a restart signs nobody out.

## Claude

Everything here is optional. Without `ANTHROPIC_API_KEY`, OmniMem stores memories as written, recalls with the original query, confirms nothing as a contradiction, and truncates RSS articles instead of summarising them.

| Variable | Default | Description |
|---|---|---|
| `ANTHROPIC_API_KEY` | *(unset)* | Turns on fact extraction, query expansion, contradiction checks and RSS summaries |
| `ANTHROPIC_BASE_URL` | `https://api.anthropic.com` | A different Messages API endpoint, such as a proxy |
| `INGEST_MODE` | `full` | `full` stores a memory as written, then extracts atomic facts in the background (facts go to the knowledge namespace, preferences to the preference namespace). `raw` just stores it. Falls back to raw with no key |
| `ENRICHMENT_BATCH_MODE` | off | Send a whole `remember_document()` to one extraction call instead of one per chunk. Faster for big documents, with a bigger prompt |
| `FACT_EXTRACTION_MODEL` | `claude-haiku-4-5-20251001` | Model for fact extraction |
| `RECALL_EXPAND_QUERIES` | off | Rephrase every recall query with Claude and merge the results, unless a call says otherwise |
| `RECALL_EXPAND_COUNT` | `3` | Variants per expanded query, 1 to 10 |
| `QUERY_EXPANSION_MODEL` | `claude-haiku-4-5-20251001` | Model for query expansion |

## Recall and maintenance

| Variable | Default | Description |
|---|---|---|
| `MEMORY_RECALL_TOP_K` | `5` | Default number of recall results. A ceiling, not a target |
| `RECALL_MIN_SCORE` | `0.15` | Relevance floor on raw similarity. Anything below it is left out, so a short or empty result is a real answer. Abandoned-approach warnings and reinstate candidates are exempt. `0` keeps everything |
| `RECALL_WEAK_SCORE` | `0.35` | Results above the floor but below this come back marked `weak_match`: read them, don't build on them. `0` turns the marker off |
| `RECENCY_DECAY_DAYS` | `90` | Age after which the recency penalty starts |
| `DEPRIORITISED_WEIGHT` | `0.2` | How much a deprioritised memory still counts |
| `DEDUP_SIMILARITY_THRESHOLD` | `0.92` | Similarity at which a new memory is flagged as a duplicate |
| `CONTRADICTION_SIMILARITY_THRESHOLD` | `0.7` | Similarity at which two memories are checked for contradicting each other |
| `STALE_MEMORY_DAYS` | `30` | The briefing lists active memories untouched this long |
| `AUTO_MAINTENANCE_INTERVAL` | `10` | Briefings per project between maintenance runs (dedup, contradiction scan, article expiry). `0` turns it off |
| `ABANDONED_CACHE_TTL_SECONDS` | `60` | How long recall caches the list of abandoned approaches. Experience writes clear it straight away. `0` rescans every time |
| `PROJECT_DOMAIN_CACHE_TTL_SECONDS` | `60` | How long `recall(domain_filter=...)` caches which projects hold which domains. Project writes clear it. `0` rescans every time |
| `BACKUP_DIR` | `backups/` beside the database | Where `dump_to_file()` and the Backups page write, and where restores are read from |

## RSS

| Variable | Default | Description |
|---|---|---|
| `FEEDS_CONFIG_PATH` | `feeds.yml` beside the database | The reading list. The settings panel's Feeds page edits the same file |
| `RSS_SCHEDULE_HOURS` | `6` | How often feeds are checked. Feeds are also checked at start and whenever `feeds.yml` changes. `0` means only those two |
| `FEEDS_WATCH_INTERVAL` | `10` | Seconds between checks for a changed `feeds.yml` |
| `RSS_MAX_ARTICLES_PER_FEED` | `20` | Articles per feed per check |
| `RSS_MAX_DIGEST_ENTRIES` | `2` | Entries per check for feeds set to `mode: digest` |
| `RSS_MAX_PAGE_BYTES` | `10485760` | Most bytes read when fetching a full article page (10 MB) |
| `RSS_REQUIRE_LICENCE` | off | Skip feeds whose `feeds.yml` entry doesn't declare a licence, before fetching anything, instead of ingesting their articles as `unknown` |
| `MAX_KNOWLEDGE_AGE_DAYS` | `30` | Days before an ingested article expires and is archived |

## Skills

| Variable | Default | Description |
|---|---|---|
| `OMNIMEM_USER` | `local` | The user part of generated skill names (`{domain}-{user}`) |
| `SKILL_MIN_SCORE` | `0.25` | Relevance floor for `find_skills()`. Exact domain matches are never held back |
| `SKILL_CLUSTER_THRESHOLD` | `0.80` | Similarity at which two lessons count as the same lesson for reinforcement |
| `SKILL_DOMAIN_SUGGEST_THRESHOLD` | `0.60` | Floor for the "did you mean" domain suggestion when a compile finds nothing |
| `SKILL_PROPOSAL_TTL_SECONDS` | `86400` | How long a proposed skill can still be written with `compile_skill(mode='write')` |
| `SKILL_EXPORT_DIR` | `skills/` inside the backup folder | Where `compile_skill(export_path=...)` writes a copy |
| `SKILL_FEED_MAX_ARTICLES` | `25` | Cap on a skill's Feed watch section from influencing feeds. `0` leaves it out |
| `SKILL_KNOWLEDGE_WATCH_DAYS` | `14` | How far back the briefing's knowledge watch looks for articles relevant to a skill. `0` turns it off |
| `SKILL_KNOWLEDGE_WATCH_THRESHOLD` | `0.35` | Similarity an article needs to a skill before the watch mentions it |
| `SKILL_SUGGEST_MIN_SIMILARITY` | `0.30` | Floor for skill suggestions in the briefing |
| `SKILL_SCAN_INTERVAL_HOURS` | `24` | How often a briefing may run the automatic skill scan. `0` turns it off |
| `SKILL_SCAN_MIN_POOL` | `3` | Lesson-bearing memories a domain needs before the scan looks at it |
| `SKILL_SCAN_CROSS_PROJECT` | on | Only propose a new skill when a rule spans two or more projects. Set `false`, `0` or `no` to allow single-project patterns |
| `SKILL_SCAN_MAX_PROPOSALS` | `3` | Most proposals one scan may create |

## Embeddings

The default model is all-MiniLM-L6-v2 on ONNX Runtime, pinned to the same revision 6.7 used, so the vectors match a 6.x store exactly.

| Variable | Default | Description |
|---|---|---|
| `EMBEDDING_MODEL` | `all-MiniLM-L6-v2` | A bare name is looked up under `sentence-transformers/`; give `owner/name` for anything else, or a path to a folder holding `onnx/model.onnx` and `tokenizer.json` to load with no network at all. The model must produce 384-dimensional vectors |
| `EMBEDDING_MODEL_REVISION` | pinned for the default model, `main` otherwise | Which commit of the model repo to fetch |
| `EMBEDDING_ONNX_FILE` | `onnx/model.onnx` | Which graph in the repo to run. Quantised graphs are faster but don't produce the same vectors, so re-embed (export and import) before switching |
| `EMBEDDING_MAX_SEQ_LENGTH` | from the model (256 for MiniLM) | Token cap per text. Clamped to what the model supports |
| `EMBEDDING_THREADS` | ONNX Runtime's choice | Threads per embedding call |
| `HF_HOME`, `HF_HUB_CACHE` | `~/.cache/huggingface` | Where downloaded models are cached, as the Hugging Face tools use it. `XDG_CACHE_HOME` is honoured too |
| `HF_ENDPOINT` | `https://huggingface.co` | A mirror to download from |
| `HF_HUB_OFFLINE` | off | Never contact Hugging Face. The model must already be in the cache (or `EMBEDDING_MODEL` must be a folder) |
| `HF_TOKEN` | *(unset)* | Only needed for a private or gated model |

## Settings panel

Desktop app only.

| Variable | Default | Description |
|---|---|---|
| `DASHBOARD_STATS_TTL` | `60` | Seconds the dashboard caches its counts. `0` recounts on every visit |
| `TELEMETRY_COLD_DAYS` | `60` | Days without a recall before the Telemetry page calls a memory gone cold |

## Gone since 6.x

These did something in 6.x and are ignored now, mostly because the thing they configured no longer exists:

| Settings | Why |
|---|---|
| `VALKEY_HOST`, `VALKEY_PORT`, `VALKEY_PASSWORD`, `VALKEY_MAX_CONNECTIONS`, `VALKEY_RAW_MAX_CONNECTIONS`, `OAUTH_VALKEY_MAX_CONNECTIONS` | Valkey is gone; everything is in the SQLite file |
| `WEB_PORT`, `WEB_UI_AUTH_TOKEN`, `WEB_UI_LOGIN_ENABLED`, `WEB_UI_SESSION_HOURS` | There's no web UI over HTTP; its pages are in the desktop app's settings panel |
| `METRICS_CACHE_TTL` | `/metrics` is gone with the web UI; the Telemetry page shows the same numbers |
| `MCP_TRANSPORT`, `FASTMCP_HTTP_ALLOWED_HOSTS`, `FASTMCP_HTTP_ALLOWED_ORIGINS` | Streamable HTTP is the only transport, and FastMCP isn't involved. Use `MCP_ALLOWED_HOSTS` and `MCP_ALLOWED_ORIGINS` |
| `EMBEDDING_BACKEND` | ONNX Runtime is the only backend; the PyTorch rollback is gone |
| `INDEX_DRIFT_CHECK` | There's no separate search index to drift from the data |
| `OMNIMEM_INSTRUCTIONS_CHARS`, `OMNIMEM_TOOL_SCHEMAS_CHARS` | The Token overhead page measures what the server actually sends |
