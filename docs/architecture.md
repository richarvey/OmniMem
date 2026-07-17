# Architecture

Four containers, nothing leaves your machine. Both the MCP server and web UI connect directly to Valkey and share the `mcp_server/memory/` package, so there is one engine and two front doors.

```mermaid
flowchart TB
    agent["AI agent<br/>Claude Code · claude.ai · Cursor · Copilot · ..."]
    browser["Browser"]

    agent -- "MCP · Streamable HTTP / SSE · :8765" --> mcp
    browser -- "HTTP · :8080" --> webui

    subgraph stack["Docker Compose stack"]
        mcp["MCP server<br/>Python · FastMCP<br/><i>remember · recall · briefing<br/>compile_skill · record_experience</i>"]
        webui["Web UI<br/>Starlette · htmx · Jinja2<br/><i>dashboard · search · skills<br/>projects · backups · /metrics</i>"]
        rss["RSS worker<br/>feedparser · APScheduler<br/>Claude Haiku summaries"]
        valkey[("Valkey + valkey-search<br/>HNSW vector indexes<br/><i>idx:episodic · idx:project · idx:knowledge<br/>idx:preference · idx:skill</i>")]

        mcp <--> valkey
        webui <--> valkey
        rss --> valkey
    end
```

| Service | Port | Purpose |
|---------|------|---------|
| `valkey` | 6379 (internal) | Vector DB + search, persisted to a named volume with AOF |
| `mcp_server` | 8765 | MCP transport (Streamable HTTP or SSE) |
| `rss_worker` | — | Background feed ingestion |
| `web_ui` | 8080 | Web dashboard + `/metrics` Prometheus endpoint |

Embeddings are computed locally by sentence-transformers (all-MiniLM-L6-v2, 384 dimensions) — no API calls for storage or recall. The Anthropic API is only used for the optional extras: RSS summaries, fact extraction, query expansion, and Tier 2 contradiction checks.

## The recall pipeline

Every `recall()` runs the same pipeline:

```mermaid
flowchart TB
    q["query"] --> fast["abandoned fast-path<br/>keyword scan, no embedding needed"]
    fast --> embed["embed query"]
    embed --> search["vector search<br/>top 20 candidates per namespace"]
    search --> filter["filter archived + deleted<br/>filter suppressed topics"]
    filter --> score["apply multipliers<br/>surface_score × recency decay × experience_weight"]
    score --> extras["check reinstate eligibility<br/>surface contradiction warnings"]
    extras --> rank["merge namespaces, re-rank"]
    rank --> out["return top_k<br/>log event + bump recall counters"]
```

The final ranking formula:

```
score = similarity x surface_score x recency x experience_weight
```

Four factors decide what comes back. Semantic similarity alone isn't enough — lifecycle state, age, and how hard the lesson was to learn all play a role. [Features in depth](features.md) explains each multiplier.

## Storage model

Every memory is a Valkey hash under a namespaced key (`mem:episodic:{ULID}`, `mem:project:{name}`, `mem:knowledge:{hash}`, `mem:preference:{ULID}`, `mem:skill:gen:{domain}-{user}`). The [memory type specifications](memory-types.md) document every field of every namespace.

Key design decisions:

- **ULIDs** for memory keys — sortable, collision-free
- **Valkey** over Redis — open source fork, with valkey-search providing HNSW vector indexes
- **CPU-only PyTorch** — no GPU dependency, runs on a Raspberry Pi
- **Shared `memory/` package** between the MCP server and web UI — no code duplication
- **Debian-slim Docker base** — PyTorch publishes no musllinux wheels, so Alpine is out
- **Multi-arch images** for amd64 and arm64
