# Claude Code Build Prompt: OmniMem

> Hand this file to Claude Code and run it from an empty project directory.
> Claude Code will build the entire project iteratively, committing to git after each feature.

---

## Project Overview

Build a self-hosted semantic memory system for Claude Code, exposed as an MCP server. The system gives Claude Code persistent, cross-session, cross-project memory backed by Valkey (Redis fork) with vector search. It also ingests RSS feeds on a schedule to build passive base knowledge that can surface during conversations.

Memory is not binary — it has a lifecycle (active → deprioritised → archived → deleted) with surface score weighting so humans can "soft forget" things without losing them permanently. Episodic memories also carry an **experience score** — capturing how hard something was to get right, what approaches were abandoned and why, and what finally cracked it. Hard-won knowledge surfaces more readily; dead ends warn before they waste time again.

### Key Goals

- Claude Code can remember decisions, solutions, patterns, and project context across sessions and machines
- RSS articles are ingested on a schedule, summarised, embedded, and stored as base knowledge
- Recall is semantic (vector similarity) but modulated by memory state, topic suppression, recency decay, and experience weight
- Humans can deprioritise memories without hard-deleting them; retired memories can earn their way back via reinstate hints
- Abandoned approaches are tracked and surfaced as warnings when similar paths are suggested again
- Everything runs in Docker, is fully self-hosted, and can be backed up with a single MCP tool call

---

## Technology Stack

| Component | Choice | Reason |
|---|---|---|
| Memory store | Valkey + valkey-search | Open source Redis fork with native vector index support |
| MCP server | Python + `fastmcp` | Clean, minimal MCP server library |
| Embeddings | `sentence-transformers` (all-MiniLM-L6-v2) | Local, no API cost, fast, Pi-compatible |
| RSS worker | Python + `feedparser` + `APScheduler` | Lightweight scheduled ingestion |
| Containerisation | Docker + Docker Compose | Alpine-based MCP image for small footprint |
| Backup | MCP tool → JSON dump | Single tool call exports everything |

---

## Project Structure to Create

```
omnimem/
├── docker-compose.yml
├── .env.example
├── README.md
├── CHANGELOG.md
├── mcp_server/
│   ├── Dockerfile              # Alpine-based
│   ├── requirements.txt
│   ├── server.py               # Main MCP server entry point
│   ├── memory/
│   │   ├── __init__.py
│   │   ├── store.py            # Valkey connection + vector ops
│   │   ├── lifecycle.py        # State machine: active/deprioritised/archived/deleted
│   │   ├── recall.py           # Recall pipeline with scoring
│   │   └── embedder.py         # sentence-transformers wrapper
│   └── tools/
│       ├── __init__.py
│       ├── core.py             # remember, recall, forget, deprioritise, archive, reinstate
│       ├── project.py          # get_project_context, set_project_context
│       ├── topics.py           # suppress_topic, unsuppress_topic, list_suppressions
│       ├── audit.py            # memory_audit, why_did_you_mention, explain_memory
│       ├── experience.py       # record_experience, log_abandoned, warn_if_abandoned, experience_summary
│       └── backup.py           # dump_to_file, restore_from_file
├── rss_worker/
│   ├── Dockerfile              # Alpine-based
│   ├── requirements.txt
│   ├── worker.py               # Scheduler entry point
│   ├── feeds.yml               # Configurable feed list
│   ├── ingester.py             # Fetch → summarise → embed → store
│   └── summariser.py           # Claude API summarisation
├── claude_config/
│   ├── CLAUDE.md               # Drop into any project to wire up memory
│   └── mcp_config.json         # MCP server config snippet for claude_desktop_config
└── scripts/
    ├── health_check.sh
    └── restore_backup.sh
```

---

## Implementation Stages

Complete each stage fully before moving to the next. **After each stage, run `git add -A && git commit -m "<message>"` before proceeding.**

---

### Stage 1 — Project Scaffold and Git Init

**Tasks:**

1. Initialise git repository: `git init`
2. Create the full directory structure above (empty files with placeholder comments where needed)
3. Create `.gitignore` — exclude: `.env`, `__pycache__`, `*.pyc`, `backups/`, `data/`, `.DS_Store`, `*.egg-info`, `dist/`, `venv/`, `.venv/`
4. Create `.env.example`:
   ```
   VALKEY_HOST=valkey
   VALKEY_PORT=6379
   VALKEY_PASSWORD=changeme
   MCP_PORT=8765
   ANTHROPIC_API_KEY=your_key_here
   EMBEDDING_MODEL=all-MiniLM-L6-v2
   RSS_SCHEDULE_HOURS=6
   RSS_MAX_ARTICLES_PER_FEED=20
   MEMORY_RECALL_TOP_K=5
   DEPRIORITISED_WEIGHT=0.2
   RECENCY_DECAY_DAYS=90
   BACKUP_DIR=/app/backups
   ```
5. Create `CHANGELOG.md` with initial structure:
   ```markdown
   # Changelog
   All notable changes to omnimem are documented here.
   Format: [version] - date - description

   ## [Unreleased]

   ## [0.1.0] - YYYY-MM-DD
   ### Added
   - Initial project scaffold
   ```
6. Create `README.md` skeleton with: project name, one-paragraph description, and section headers for: Overview, Quick Start, Configuration, MCP Tools Reference, RSS Configuration, Memory Lifecycle, Experience Scoring, Backup & Restore, CLAUDE.md Integration

**Commit message:** `feat: initial project scaffold`

---

### Stage 2 — Docker Compose and Container Definitions

**Tasks:**

1. Create `docker-compose.yml` with three services:

   **valkey:**
   - Image: `valkey/valkey:8-alpine`
   - Load the valkey-search module: `--loadmodule /usr/lib/valkey/valkey-search.so`
   - Persist data with a named volume: `valkey_data:/data`
   - Set password via command: `--requirepass ${VALKEY_PASSWORD}`
   - Expose port 6379 (internal only, do not publish to host by default)
   - Add a healthcheck using `valkey-cli ping`

   **mcp_server:**
   - Build from `./mcp_server`
   - Depends on valkey (with condition: service_healthy)
   - Expose `MCP_PORT` to host
   - Mount a `./backups` volume to `/app/backups`
   - Pass all env vars from `.env`
   - Restart policy: `unless-stopped`

   **rss_worker:**
   - Build from `./rss_worker`
   - Depends on valkey (with condition: service_healthy)
   - Mount `./rss_worker/feeds.yml` into container
   - Pass all env vars from `.env`
   - Restart policy: `unless-stopped`

2. Create `mcp_server/Dockerfile`:
   - Base: `python:3.12-alpine`
   - Install build dependencies for sentence-transformers: `gcc musl-dev g++ openblas-dev`
   - Copy and install `requirements.txt`
   - Copy application code
   - Create `/app/backups` directory
   - Non-root user: `adduser -D mcpuser` and `USER mcpuser`
   - Entrypoint: `python server.py`

3. Create `rss_worker/Dockerfile`:
   - Base: `python:3.12-alpine`
   - Minimal — only needs `feedparser`, `anthropic`, `apscheduler`, `valkey`, `sentence-transformers`
   - Non-root user: `adduser -D rssuser`
   - Entrypoint: `python worker.py`

4. Create `mcp_server/requirements.txt`:
   ```
   fastmcp>=0.9.0
   valkey[hiredis]>=6.0.0
   sentence-transformers>=2.7.0
   numpy>=1.26.0
   anthropic>=0.25.0
   python-dotenv>=1.0.0
   pydantic>=2.0.0
   ```

5. Create `rss_worker/requirements.txt`:
   ```
   feedparser>=6.0.0
   anthropic>=0.25.0
   apscheduler>=3.10.0
   valkey[hiredis]>=6.0.0
   sentence-transformers>=2.7.0
   numpy>=1.26.0
   python-dotenv>=1.0.0
   pyyaml>=6.0.0
   ```

6. Update `README.md` Quick Start section with `docker compose up -d` instructions

**Commit message:** `feat: docker compose and container definitions`

---

### Stage 3 — Valkey Connection and Vector Store

**File:** `mcp_server/memory/store.py`

**Tasks:**

1. Implement `ValkeyStore` class:
   - Connect to Valkey using environment variables, with retry on startup
   - On first connect, create three vector indexes if they don't exist:
     - `idx:episodic` on key prefix `mem:episodic:` — fields: `vector` (HNSW, FLOAT32, 384 dims, cosine), `content`, `project`, `state`, `surface_score`, `created_at`, `updated_at`, `tags`, `deprioritised_reason`, `reinstate_hints`, `effort_score`, `outcome`, `iterations`, `abandoned_approaches`, `breakthrough`, `gotchas`, `experience_weight`
     - `idx:project` on key prefix `mem:project:` — same vector field, plus `project_name`, `stack`, `state`
     - `idx:knowledge` on key prefix `mem:knowledge:` — same vector field, plus `source_url`, `feed_name`, `published_at`, `topics`
   - All indexes use `HNSW` algorithm (faster search) not `FLAT` for production use
   - Implement `upsert(namespace, key, fields: dict, vector: np.ndarray)` — stores hash + vector
   - Implement `search(namespace, vector, top_k, filter_expr)` — returns list of `{key, score, fields}` dicts
   - Implement `get(key)` → dict
   - Implement `set_field(key, field, value)` — update a single field without re-embedding
   - Implement `delete(key)` — hard delete
   - Implement `scan_prefix(prefix)` → list of all keys matching prefix
   - Implement `dump_all()` → dict of all `mem:*` keys with all fields (for backup). Use `HSCAN`, not `KEYS *`
   - Implement `restore_all(data: dict)` → bulk restore from backup dict

2. Implement `mcp_server/memory/embedder.py`:
   - Singleton `Embedder` class — loads model once on startup
   - `embed(text: str) -> np.ndarray` — returns normalised float32 vector
   - `embed_batch(texts: list[str]) -> list[np.ndarray]`
   - Model name from env var `EMBEDDING_MODEL`

**Commit message:** `feat: valkey vector store and embedder`

---

### Stage 4 — Memory Lifecycle State Machine

**File:** `mcp_server/memory/lifecycle.py`

Implement a `MemoryLifecycle` class that wraps `ValkeyStore` and enforces state transitions.

**States:**
```python
class MemoryState(str, Enum):
    ACTIVE = "active"
    DEPRIORITISED = "deprioritised"
    ARCHIVED = "archived"
    DELETED = "deleted"  # logical marker before hard delete
```

**Surface scores by state** (read from env with these defaults):
- `active` → `1.0`
- `deprioritised` → `0.2` (env: `DEPRIORITISED_WEIGHT`)
- `archived` → `0.0`
- `deleted` → `0.0`

**Methods to implement:**

- `transition(key, new_state, reason=None)` — validates allowed transitions, updates `state`, `surface_score`, `updated_at`, and stores `reason` if deprioritising. When transitioning to `DEPRIORITISED`, check the memory's `effort_score` field: if it is >= 4, return a warning alongside the successful transition:
  ```
  "warning": "This memory has an effort score of {N}/5. It represents hard-won knowledge.
              Deprioritised as requested, but consider archiving rather than suppressing it entirely."
  ```
  The transition still happens — this is advisory, not a block.
- `suppress_topic(topic: str)` — stores topic in `topics:suppressed:` hash set
- `unsuppress_topic(topic: str)` — removes from suppressed set
- `get_suppressed_topics()` → list of suppressed topic strings
- `is_topic_suppressed(text: str)` → bool — checks if any suppressed topic appears in text (case-insensitive substring match)
- `add_reinstate_hints(key, hints: list[str])` — appends hints to the `reinstate_hints` JSON array field
- `check_reinstate_eligibility(key, query: str)` → bool — returns True if query matches any reinstate hint and memory is deprioritised

**Allowed transitions:**
```
active         → deprioritised, archived, deleted
deprioritised  → active, archived, deleted
archived       → active, deleted
deleted        → (none — hard deletes via store.delete())
```

**Commit message:** `feat: memory lifecycle state machine`

---

### Stage 5 — Recall Pipeline

**File:** `mcp_server/memory/recall.py`

Implement `RecallPipeline` class:

**`RecallResult` dataclass fields:**
```python
key: str
namespace: str
content: str
score: float
adjusted_score: float
state: str
project: str | None
source_url: str | None
published_at: str | None
reinstate_candidate: bool
tags: list[str]
deprioritised_reason: str | None
effort_score: int | None
outcome: str | None
experience_weight: float
result_type: str          # "memory" | "abandoned_warning" | "knowledge"
breakthrough: str | None
```

**`recall(query: str, namespaces: list, top_k: int) -> list[RecallResult]`**

Pipeline steps in order:

1. **Abandoned fast-path** — before vector search, call `warn_if_abandoned(query)`. If matches are found, inject them at the top of results with `result_type="abandoned_warning"`. These bypass normal scoring and always surface when relevant.
2. Embed the query
3. Search each requested namespace (default: all three) — fetch top 20 candidates per namespace
4. Filter out any result whose `state` is `archived` or `deleted`
5. Filter out results where `is_topic_suppressed(content)` returns True
6. Apply surface score: multiply raw similarity score by the memory's `surface_score` field
7. Apply recency decay: memories older than `RECENCY_DECAY_DAYS` (env, default 90) have their score reduced by `0.05` per 30 days beyond threshold (floor at `0.3` multiplier)
8. Apply experience weight: multiply by `experience_weight` field (default `1.0` if not set). Memories without experience data are not penalised.
9. Check reinstate eligibility: for `deprioritised` memories that match a reinstate hint, flag with `reinstate_candidate: True` and restore score to `0.6`
10. Merge and re-rank all results across namespaces by `adjusted_score`
11. Return top `top_k` results

**Scoring formula:**
```
adjusted_score = raw_similarity
              × surface_score        (lifecycle state weight)
              × recency_multiplier   (age decay)
              × experience_weight    (effort/outcome weight, default 1.0)
```

Recency decay is applied after surface score. Experience weight is applied last.

**Experience weight formula** — implement as `compute_experience_weight(effort_score, outcome) -> float`:
```python
def compute_experience_weight(effort_score: int, outcome: str) -> float:
    base = {"succeeded": 1.0, "pivoted": 0.7, "abandoned": 0.1}.get(outcome, 1.0)
    effort_multiplier = {1: 1.0, 2: 1.1, 3: 1.25, 4: 1.5, 5: 1.8}.get(effort_score, 1.0)
    if outcome == "abandoned":
        return base  # effort does not amplify failures
    return min(base * effort_multiplier, 2.0)  # cap at 2.0
```

**`warn_if_abandoned(query: str) -> list[dict]`**
- Keyword scan of all `abandoned_approaches[].name` values across all episodic memories against the query string (case-insensitive)
- Also do a semantic search for the query against stored `abandoned_approaches` reasons
- Return list of `{memory_key, abandoned_name, reason, effort_score, project}` for each match
- Used both by the recall fast-path and as a standalone MCP tool

**`log_recall_event(query, results)`** — store a lightweight recall log entry in Valkey at `log:recall:<timestamp>` with: query, top result keys, scores, timestamp. TTL: 30 days.

**Commit message:** `feat: recall pipeline with scoring, experience weight, and abandoned fast-path`

---

### Stage 6 — Core MCP Tools

**File:** `mcp_server/tools/core.py`

Implement the following MCP tools using `fastmcp`. Each tool must have a clear docstring — this becomes the tool description Claude sees.

---

**`remember(content, project=None, tags=None, namespace="episodic")`**
- Generate a unique key: `mem:{namespace}:{ulid}`
- Embed content
- Store with state=active, surface_score=1.0, experience_weight=1.0, created_at=now, project if provided, tags as JSON array
- Return: `{key, status: "stored", namespace}`

---

**`recall(query, top_k=5, namespaces=None, project_filter=None)`**
- Run recall pipeline
- Optionally filter to a specific project
- For `abandoned_warning` results, lead with: `"⚠️ WARNING: The following approaches were previously tried and abandoned:"`
- For `reinstate_candidate` results, prepend: `"[This memory was deprioritised but may be relevant again: {deprioritised_reason}]"`
- Return: list of results with content, source, score, state, namespace, result_type, and any warning/reinstate note

---

**`deprioritise(key_or_query, reason, reinstate_hints=None)`**
- If given a key directly, deprioritise it
- If given a natural language query, recall top 3 matches and deprioritise all that score > 0.85
- Store the reason and optional reinstate hints
- Return: list of affected keys, their previous state, and any high-effort warnings from the lifecycle guard

---

**`archive(key_or_query, reason=None)`**
- Same key-or-query pattern as deprioritise
- Transition to archived
- Return: affected keys

---

**`reinstate(key_or_query)`**
- Transition back to active, clear deprioritised_reason, surface_score → 1.0
- Return: affected keys

---

**`forget(key_or_query, confirm=False)`**
- Hard delete — requires `confirm=True` to execute
- If `confirm=False`, return a list of what would be deleted and ask for confirmation
- Return: deleted keys or preview list

---

**`suppress_topic(topic, reason=None)`**
- Add topic to suppression list
- Return: `{topic, status: "suppressed"}`

---

**`unsuppress_topic(topic)`**
- Remove from suppression list
- Return: `{topic, status: "active"}`

---

**`list_suppressions()`**
- Return current suppressed topic list

**Commit message:** `feat: core MCP tools (remember, recall, forget, deprioritise, archive, reinstate, topic suppression)`

---

### Stage 7 — Project Context Tools

**File:** `mcp_server/tools/project.py`

---

**`set_project_context(project_name, description, stack, goals, current_state, notes=None)`**
- Upsert a project context memory at key `mem:project:{project_name}`
- Embed `description + goals + current_state` concatenated
- Return: `{project_name, status: "saved"}`

---

**`get_project_context(project_name)`**
- Retrieve the project memory by exact key
- Return all fields as a structured dict
- If not found, return `{status: "not_found", suggestion: "Use set_project_context to create one"}`

---

**`list_projects()`**
- Scan `mem:project:*` keys
- Return list of `{project_name, description, updated_at, state}` for each

---

**`update_project_state(project_name, current_state, notes=None)`**
- Update only the `current_state`, `notes`, `updated_at` fields without re-embedding
- Return: `{project_name, status: "updated"}`

**Commit message:** `feat: project context MCP tools`

---

### Stage 8 — Audit Tools

**File:** `mcp_server/tools/audit.py`

---

**`memory_audit(project=None, namespace=None, include_archived=False)`**
- Return a summary of all memories, grouped by state
- Each entry: key, content (first 100 chars), state, surface_score, effort_score, outcome, created_at, updated_at, project
- If `project` filter provided, only return memories tagged to that project
- Return count by state as a summary header

---

**`why_did_you_mention(query)`**
- Search recent recall logs (last 50) for any log entry whose query is semantically similar to the given query
- Return the most recent matching log: query text, timestamp, top results that were returned and their scores at the time
- Purpose: lets humans understand why Claude surfaced something

---

**`explain_memory(key)`**
- Return full metadata for a single memory key: all fields, state history note, when created, last recalled (from logs), deprioritised reason if any, reinstate hints if any, full experience data if present
- Return `{status: "not_found"}` if key doesn't exist

**Commit message:** `feat: audit MCP tools (memory_audit, why_did_you_mention, explain_memory)`

---

### Stage 9 — Experience Scoring Tools

**File:** `mcp_server/tools/experience.py`

Experience scoring captures not just *what* was solved, but *how hard* it was, what approaches were abandoned and why, and what finally cracked it. This lets future sessions avoid dead ends and surface hard-won knowledge more readily.

---

**Effort score reference** (include in all relevant docstrings):
```
1 — First attempt succeeded, no meaningful obstacles
2 — Minor friction: one wrong turn, quick fix
3 — Moderate effort: multiple iterations, some debugging
4 — Significant struggle: hours of effort, approach changes required
5 — Battle-hardened: near-abandonment, fundamental rethink required
```

**`abandoned_approaches` item structure** (JSON array stored on the memory):
```json
{
  "name": "onnxruntime",
  "type": "library",
  "reason": "SIGILL crash on Alpine musl libc — incompatible with non-glibc",
  "attempted_at": "2025-03-09T14:32:00Z"
}
```
`type` values: `library`, `approach`, `tool`, `pattern`, `service`

---

**`record_experience(key, effort_score, outcome, iterations=1, abandoned_approaches=None, breakthrough=None, gotchas=None)`**
- Validates `effort_score` is 1–5; raises a clear error otherwise
- Validates `outcome` is one of `succeeded`, `pivoted`, `abandoned`
- Updates the memory at `key` with all provided experience fields
- Computes and stores `experience_weight` using `compute_experience_weight(effort_score, outcome)` from the recall module
- If `effort_score >= 4` and `outcome == "abandoned"`, automatically call `suppress_topic()` for each `name` in `abandoned_approaches` — dead ends with high effort invested should not keep resurfacing. Log this suppression clearly in the return value.
- Returns: `{key, effort_score, outcome, experience_weight, auto_suppressed: [...], status: "recorded"}`

---

**`log_abandoned(key, name, type, reason)`**
- Append a single abandoned approach entry to an existing memory's `abandoned_approaches` JSON array without modifying any other fields
- Useful for incrementally recording dead ends during a session as they happen
- Returns: `{key, abandoned_count, latest_entry}`

---

**`get_experience(key)`**
- Return full experience fields for a memory: `effort_score`, `outcome`, `iterations`, `abandoned_approaches`, `breakthrough`, `gotchas`, `experience_weight`
- Include a human-readable summary:
  ```
  "This took 3 iterations, effort score 4/5 (significant struggle).
   Abandoned: onnxruntime (SIGILL on Alpine), openai embeddings (API cost/latency).
   What worked: sentence-transformers with --prefer-binary on Alpine pip install.
   Gotchas: must install openblas-dev and g++ before pip install."
  ```
- Returns `{status: "not_found"}` if key doesn't exist or has no experience data

---

**`experience_summary(project=None)`**
- Aggregate view across all episodic memories, optionally filtered by project
- Return:
  - Average effort score across all memories with experience data
  - Outcome breakdown: `{succeeded: N, pivoted: N, abandoned: N}`
  - Top 5 most effortful memories (`effort_score` descending) with content preview and outcome
  - **The graveyard** — consolidated list of all unique abandoned approaches across all memories: `{name, type, reason, effort_score, project}`
  - Top 3 breakthroughs — highest `effort_score` where `outcome == "succeeded"`, with their `breakthrough` text
- The graveyard is as operationally valuable as the success list — it prevents repeating painful mistakes

---

**`warn_if_abandoned(query)`**
- Re-export the `warn_if_abandoned` function from the recall module as a standalone MCP tool
- Claude Code should call this proactively before suggesting libraries or architectural approaches
- Returns: list of matches with `{memory_key, abandoned_name, reason, effort_score, project}`, prefixed with `"⚠️ WARNING:"` if any matches found
- Returns `{status: "clear", message: "No previously abandoned approaches match this query"}` if no matches

**Commit message:** `feat: experience scoring tools — effort, outcomes, graveyard, abandoned warnings`

---

### Stage 10 — Backup and Restore Tools

**File:** `mcp_server/tools/backup.py`

---

**`dump_to_file(filename=None)`**
- If no filename, auto-generate: `memory_backup_{YYYYMMDD_HHMMSS}.json`
- Call `store.dump_all()` to get all `mem:*` and `topics:suppressed:*` keys
- Also export all recall logs: `log:recall:*`
- Write to `BACKUP_DIR` (env var, default `/app/backups`) as formatted JSON
- Include a metadata header:
  ```json
  {
    "exported_at": "...",
    "total_keys": 0,
    "namespaces": {"episodic": 0, "project": 0, "knowledge": 0},
    "version": "1.0"
  }
  ```
- Return: `{filename, path, total_keys, status: "success"}`

---

**`restore_from_file(filename, dry_run=True)`**
- Read backup JSON from `BACKUP_DIR/{filename}`
- If `dry_run=True` (default), return what would be restored without writing anything
- If `dry_run=False`, call `store.restore_all()` — this MERGES, not replaces. Existing keys are not overwritten unless their `updated_at` in the backup is newer.
- Return: `{restored_keys, skipped_keys, status}`

---

**`list_backups()`**
- List all `.json` files in `BACKUP_DIR`
- Return: list of `{filename, size_kb, created_at}` sorted newest first

**Commit message:** `feat: backup and restore MCP tools`

---

### Stage 11 — MCP Server Entry Point

**File:** `mcp_server/server.py`

**Tasks:**

1. Initialise `fastmcp` app with name `"omnimem"`
2. On startup: connect to Valkey, load embedder, create indexes if missing, log readiness
3. Register all tools from: `tools/core.py`, `tools/project.py`, `tools/audit.py`, `tools/experience.py`, `tools/backup.py`
4. Add a `health()` tool — returns Valkey ping status, index counts, model loaded bool, uptime
5. Expose server on `0.0.0.0:MCP_PORT` (env, default 8765) using SSE transport
6. Implement graceful shutdown — flush any pending operations on SIGTERM

**Commit message:** `feat: MCP server entry point with all tools registered`

---

### Stage 12 — RSS Ingestion Worker

**File:** `rss_worker/feeds.yml`

Create a starter feed configuration:

```yaml
feeds:
  # Rust & systems programming
  - url: https://blog.rust-lang.org/feed.xml
    name: Rust Official Blog
    topics: [rust, systems, language]

  - url: https://this-week-in-rust.org/rss.xml
    name: This Week in Rust
    topics: [rust, community, crates]

  # Self-hosting & open source
  - url: https://selfhosted.show/rss
    name: Self-Hosted Show
    topics: [selfhosted, homelab, docker]

  # n8n / automation
  - url: https://blog.n8n.io/rss/
    name: n8n Blog
    topics: [automation, workflow, n8n]

  # AI / LLM
  - url: https://www.anthropic.com/rss.xml
    name: Anthropic Blog
    topics: [ai, llm, claude]

  # DevOps / infra
  - url: https://www.docker.com/blog/feed/
    name: Docker Blog
    topics: [docker, containers, devops]
```

**File:** `rss_worker/summariser.py`

- `summarise(title, url, content_text) -> str`
- Call Anthropic API with prompt: *"Summarise the following article in 2-3 sentences, focusing on what a developer might find actionable or useful. Include the main technology or concept. Be concise."*
- Model: `claude-haiku-4-5` (fast and cheap for this use)
- Return the summary string
- On API error, fall back to truncating the first 300 chars of content_text

**File:** `rss_worker/ingester.py`

- `ingest_feed(feed_config: dict)` — fetch feed, iterate entries, for each:
  1. Check if key `mem:knowledge:{url_hash}` already exists — skip if so (dedup)
  2. Strip HTML from content/summary, truncate to 2000 chars
  3. Call `summariser.summarise()`
  4. Embed the summary
  5. Store in Valkey at `mem:knowledge:{url_hash}` with fields: `content` (summary), `source_url`, `feed_name`, `title`, `published_at`, `topics` (JSON), `state=active`, `surface_score=1.0`, `experience_weight=1.0`
- `ingest_all_feeds(feeds_config_path)` — load YAML, call `ingest_feed` for each, return stats dict

**File:** `rss_worker/worker.py`

- Load feeds YAML from mounted path
- Run `ingest_all_feeds()` immediately on startup
- Schedule repeat runs every `RSS_SCHEDULE_HOURS` hours (env, default 6)
- Log ingestion stats: feeds processed, articles added, articles skipped (dedup), errors
- On Valkey connection failure at startup, retry with exponential backoff (max 5 attempts)

**Commit message:** `feat: RSS ingestion worker with scheduling and dedup`

---

### Stage 13 — Claude Integration Files

**File:** `claude_config/CLAUDE.md`

Write a ready-to-use CLAUDE.md snippet that users drop into any project:

````markdown
## OmniMem — Persistent Semantic Memory

You have access to a persistent memory system via the `omnimem` MCP server.

### Session Start
At the beginning of every session:
1. Call `get_project_context("<project_name>")` where project_name matches this project
2. Call `experience_summary(project="<project_name>")` — review the graveyard of abandoned approaches alongside what worked
3. Call `recall("<brief description of what we're working on>", project_filter="<project_name>")`
4. Briefly summarise what you found: current state, recent decisions, any abandoned approaches to avoid, relevant knowledge articles

### During a Session
- Before attempting to solve a non-trivial problem, call `recall("<problem description>")` — you may find a prior solution or a relevant article
- Before suggesting a library or architectural approach, call `warn_if_abandoned("<library or approach name>")`. If a warning comes back, tell the human before proceeding: *"We tried [X] before and abandoned it because [reason] — shall we try again or look for alternatives?"*
- When you and the human reach a decision, solve a tricky bug, discover a pattern, or agree on an approach: call `remember("<what was decided/discovered>", project="<project_name>", tags=["<relevant_tag>"])`
- If the human says something like "forget about X" or "stop bringing up Y" or "I don't do that anymore": call `deprioritise()` with a clear reason. Do NOT hard delete unless they say "permanently delete" or "wipe"
- If a recalled memory seems to keep coming back when it shouldn't, call `suppress_topic("<topic>")` and let the human know

### Recording Experience
After solving any non-trivial problem (or giving up on one), record the experience:

```
record_experience(
  key="mem:episodic:...",
  effort_score=3,           # 1=trivial, 5=battle-hardened
  outcome="succeeded",      # succeeded | pivoted | abandoned
  iterations=2,
  abandoned_approaches=[
    {"name": "onnxruntime", "type": "library", "reason": "SIGILL on Alpine musl libc"}
  ],
  breakthrough="sentence-transformers with --prefer-binary pip flag",
  gotchas=["Needs openblas-dev and g++ installed in Alpine first"]
)
```

For dead ends discovered mid-session, use `log_abandoned(key, name, type, reason)` to record them incrementally as they happen.

If `effort_score >= 4` and `outcome == "abandoned"`, the system will automatically suppress the abandoned approach names — dead ends don't need to keep resurfacing.

**Effort score guide:**
- **1** — Worked first time, no issues
- **2** — Minor friction, quick fix
- **3** — Multiple iterations, some debugging
- **4** — Significant effort, approach changes required
- **5** — Near-abandonment, fundamental rethink

### Session End
When wrapping up:
1. Call `update_project_state("<project_name>", current_state="<current state>", notes="<anything important for next session>")`
2. If significant work was done, call `remember()` for any key outcomes not already stored
3. Ensure `record_experience()` has been called for any non-trivial work this session

### Key Principles
- Prefer `deprioritise` over `forget` — humans usually mean "stop surfacing this" not "destroy this"
- Always include a `reason` when deprioritising — it helps future sessions understand the context
- Include `reinstate_hints` when deprioritising if the memory might become relevant again
- When a recalled knowledge article seems relevant, mention it as a starting point: *"I found an article from [source] about X — shall I use that as a research base?"*
- The graveyard of abandoned approaches is as valuable as the list of successes — consult it before suggesting solutions
- Never store secrets, credentials, or personally sensitive data in memory
````

**File:** `claude_config/mcp_config.json`

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "sse",
      "url": "http://localhost:8765/sse",
      "description": "Persistent semantic memory with lifecycle management and experience scoring"
    }
  }
}
```

Include instructions in the README for adding this to `claude_desktop_config.json` and to Claude Code's MCP config.

**Commit message:** `feat: claude integration config files and CLAUDE.md`

---

### Stage 14 — README and CHANGELOG Completion

Complete the `README.md` with all sections fully written:

1. **Overview** — what this is, why it exists, the memory lifecycle model in plain English, the experience scoring rationale
2. **Quick Start** — prerequisites (Docker, Docker Compose), clone, copy `.env.example` to `.env`, set password and API key, `docker compose up -d`, verify with `health` tool
3. **Configuration** — table of all env vars, defaults, and descriptions
4. **MCP Tools Reference** — every tool, its parameters, return shape, and a one-line example. Group by category: Core Memory, Project Context, Experience Scoring, Audit, Backup
5. **RSS Configuration** — how to edit `feeds.yml`, what fields are supported, how ingestion works
6. **Memory Lifecycle** — ASCII diagram of state transitions, surface score table, deprioritise vs archive vs forget guidance, high-effort lifecycle guard explanation
7. **Experience Scoring** — effort score guide, outcome types, the graveyard concept, `experience_weight` formula, how abandoned fast-path works in recall
8. **Backup & Restore** — how to call `dump_to_file`, where backups are stored, `restore_from_file dry_run` workflow
9. **CLAUDE.md Integration** — copy the file, where to put it, what it does
10. **Accessing from Multiple Machines** — expose `MCP_PORT` through firewall or reverse proxy (Traefik example snippet), security considerations
11. **Architecture** — ASCII diagram of the three containers and data flow including scoring pipeline

Update `CHANGELOG.md`:

```markdown
## [0.1.0] - YYYY-MM-DD
### Added
- Valkey + valkey-search vector store with HNSW indexing
- Three memory namespaces: episodic, project, knowledge
- Memory lifecycle state machine: active → deprioritised → archived → deleted
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
- Alpine-based Docker images for minimal footprint
- CLAUDE.md integration file for drop-in project wiring
```

**Commit message:** `docs: complete README and CHANGELOG for v0.1.0`

---

### Stage 15 — Final Polish and Tag

**Tasks:**

1. Run `docker compose build` — fix any dependency or build issues found
2. Create `scripts/health_check.sh`:
   ```bash
   #!/bin/sh
   # Quick health check — call the health MCP tool and print status
   curl -s http://localhost:${MCP_PORT:-8765}/health || echo "MCP server not responding"
   ```
3. Create `scripts/restore_backup.sh`:
   ```bash
   #!/bin/sh
   # Usage: ./scripts/restore_backup.sh <backup_filename>
   # Triggers restore_from_file via the MCP tool with dry_run=False
   echo "Restoring from backup: $1"
   echo "Run restore_from_file tool in Claude with filename='$1' and dry_run=False"
   ```
4. Verify `.gitignore` covers `backups/`, `data/`, `.env`
5. Tag the release: `git tag -a v0.1.0 -m "Initial release: OmniMem — semantic memory with experience scoring"`

**Commit message:** `chore: health check scripts and v0.1.0 release tag`

---

## Quality Requirements Throughout

These apply to every stage — Claude Code should not consider a stage done unless these are met:

- **No secrets in code** — all config via env vars, never hardcoded
- **Meaningful error handling** — every Valkey operation and API call wrapped in try/except with informative error returns, not bare exceptions
- **Type hints** — all Python functions have type annotations
- **Docstrings on MCP tools** — these become the tool descriptions Claude sees, so write them clearly and accurately. Include the effort score guide in experience tool docstrings.
- **Idempotent setup** — index creation and schema initialisation must not fail if run twice
- **Consistent logging** — use Python `logging` module, not bare `print()` statements
- **Git commits are atomic** — each commit should represent a working state, not a partial one

---

## Notes for Claude Code

- If a dependency version conflict arises with `sentence-transformers` on Alpine, use `--no-cache-dir --prefer-binary` flags on pip installs, and ensure `openblas-dev`, `lapack-dev`, and `g++` are in the apk layer
- The `valkey-search` module path in `valkey/valkey:8-alpine` is `/usr/lib/valkey/valkey-search.so` — verify this when the container first starts and adjust if needed
- `fastmcp` SSE transport is the preferred mode for Claude Code MCP integration; confirm the port is reachable from the host before marking Stage 11 complete
- In the recall pipeline, apply multipliers in this exact order: surface score → recency decay → experience weight. Order affects the final score.
- The `dump_all()` backup method must use Valkey `HSCAN` for large datasets, not `KEYS *` — safer for production
- The abandoned fast-path in recall runs **before** embedding the query — it's a cheap keyword check. Don't reverse the order.
- `experience_weight` defaults to `1.0` on all memories that have no experience data — no penalty, no bonus. Never set it to `0` unless outcome is `abandoned`.
- When `record_experience` auto-suppresses abandoned approach names, log each suppression clearly in the tool's return value so Claude can inform the human what was suppressed and why
