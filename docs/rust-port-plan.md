# OmniMem 7: the Rust port

**Status**: in progress on `v7.0.x`. This is the working plan; update it as phases land.

OmniMem 7 is one Rust binary. It replaces four containers (Valkey with the search module, the Python MCP server, the Python web UI and the Python RSS worker) with a single process that holds its own vector store, serves MCP, runs RSS ingestion on a schedule and serves the web UI, which is where settings live.

The Python tree stays in the repository as the reference implementation until the Rust binary reaches parity, then goes in one commit.

## Decisions

Settled on 2026-09-15:

| Question | Decision | Why |
|---|---|---|
| Storage | **SQLite, vectors searched in memory** | One file, `rusqlite` bundled so there is no system library. At OmniMem's scale (tens of thousands of memories) exact cosine search over a 384-dim matrix is under 10 ms and deterministic, which the v7 clustering rules require. FTS5 comes for free for the hybrid keyword search in `TODO.md`. An HNSW index can be added behind the same trait if a store ever outgrows exact search |
| Moving data | **Import a 6.x backup file** | `omnimem import <dump.json>` reads the existing `dump_to_file` format and re-embeds (backups carry no vectors). About 8 ms per memory on ONNX, so 25,000 memories take roughly 3.5 minutes. No Valkey client in the binary |
| Scope | **Parity port, v7 schema from day one** | Feature-for-feature, in phases. The new store includes `origin_id`, `content_hash`, `epoch` and `classification` from the start, because shaping an empty store correctly costs nothing. `cluster_profile` and the rest of the Mycelium adapter (`docs/v7-change-spec.md`) follow parity |
| Layout | **Cargo workspace beside the Python** | `crates/` at the repo root. The Python suite documents what the Rust tests must prove |

## Architecture

```mermaid
flowchart LR
    subgraph omnimem["omnimem (one process)"]
        mcp["MCP server<br/>streamable HTTP + OAuth"]
        web["Web UI + settings<br/>axum + minijinja + htmx"]
        rss["RSS scheduler<br/>tokio task"]
        enrich["Enrichment queue<br/>tokio task"]
        engine["Memory engine<br/>recall, lifecycle, skills"]
        embed["Embedder<br/>ONNX Runtime"]
        store[("SQLite file<br/>+ in-memory vectors")]
    end
    agent["Claude / MCP clients"] --> mcp
    browser["Browser"] --> web
    mcp --> engine
    web --> engine
    rss --> engine
    enrich --> engine
    engine --> embed
    engine --> store
    rss -. "Anthropic API" .-> haiku["Claude Haiku"]
    enrich -. "Anthropic API" .-> haiku
```

One embedder instance serves every caller. Today each of the three Python services loads its own copy of the model.

### Crates

| Crate | Holds | Replaces |
|---|---|---|
| `omnimem-core` | Namespaces, keys, record types, validation, licence and provenance vocabularies, the v7 content hash | `memory/licence.py`, `provenance.py`, parts of `store.py` and `tools/core.py` validation |
| `omnimem-embed` | ONNX engine, model resolution (local dir, HF cache, download with revision pin) | `memory/onnx_embedding.py`, `embedder.py` |
| `omnimem-store` | SQLite schema and migrations, vector matrix, filtered exact k-NN, backup import and export | Valkey, `memory/store.py`, `migrations.py`, `tools/backup.py` |
| `omnimem-engine` | Recall pipeline, dedup, contradiction, lifecycle, experience, projects and domains, lineage, chunking, temporal, skills compiler, scan and transfer, maintenance, enrichment | the rest of `memory/` |
| `omnimem-llm` | Anthropic client with the summariser's retry classes | `extraction.py`, `query_expansion.py`, `summariser.py`, contradiction tier 2 |
| `omnimem-mcp` | The 48 tools, instructions, telemetry, auth (bearer and OAuth 2.1 server), Host/Origin guard | `server.py`, `tools/`, `oauth/`, `middleware/` |
| `omnimem-rss` | Feed fetch and parse, summary and digest modes, licence gate, scheduler | `rss_worker/` |
| `omnimem-web` | Routes, templates, static assets, sessions, `/metrics`, settings pages | `web_ui/` |
| `omnimem` | The binary: config, startup, subcommands (`serve`, `import`, `export`, `reindex`, `embed`) | `docker-compose.yml` wiring |

## Storage

SQLite in WAL mode, one file (default `./data/omnimem.db`).

- **`memories`**: one row per memory. Typed columns for everything that is filtered, sorted or counted (`key` primary, `namespace`, `state`, `project`, `project_name`, `surface_score`, `created_at`, `updated_at`, `recall_count`, `last_recalled`, `effort_score`, `outcome`, `experience_weight`, `licence`, `provenance`, `feed_name`, `expires_at`, `domain`, `generated`, plus the v7 fields `origin_id`, `content_hash` (indexed), `epoch`, `classification`). Everything else in a `fields` JSON column, so a 6.x field with no column survives an import untouched
- **`vectors`**: `key` → 384 little-endian float32 bytes, the same encoding Valkey stored
- **`memory_tags`**, **`memory_domains`**: join tables, so tag and domain filters are real queries (in 6.x `tags` were JSON strings and unsearchable)
- **`kv`**: `key`, `value`, `expires_at`. Replaces every `meta:*`, `qexp:*` and `topics:suppressed` key, with expiry enforced on read and swept periodically
- **`recall_log`**, **`tool_metrics`**, **`enrich_queue`** (durable, so a crash no longer loses a job), **`oauth_clients`**, **`oauth_codes`**, **`oauth_tokens`**, **`web_sessions`**
- **`memories_fts`**: FTS5 over content, for later hybrid search

Vector search: on start, load every vector into one contiguous matrix per namespace, kept in step with writes. A query computes dot products (vectors are unit length) against rows that pass the filter, then takes the top k. Filters are SQL, so the valkey-search tag-query quirks (`{a|b}` alternation, escaped values, `FT.DROPINDEX` arity) and index drift disappear with Valkey.

Numbers stored as REAL and INTEGER, not Python `str(float)`. Where a number is rendered into output that 6.x tests compare, it is formatted to match.

## Compatibility contract

What a 6.x user must not notice:

1. **MCP tools**: the same 48 names, parameters, defaults and descriptions (docstrings are copied verbatim, agents read them), and results in the same JSON shape, including `_compact()` dropping empty values
2. **Vectors**: identical to the Python engine (`crates/omnimem-embed/tests/fixtures/reference_vectors.json`), so recall thresholds and skill clustering carry over
3. **Recall scoring**: floor, weak band, recency decay, experience weight, temporal boost, reinstate 0.6, fact collapse and ordering exactly as `memory/recall.py`
4. **Skill bodies**: `render_skill_md` byte-identical, including `json.dumps` escaping non-ASCII in the description line, or every existing skill shows a spurious diff after import
5. **Files**: backup JSON, skill bundle zips (format v2, reads v1), `feeds.yml`
6. **Configuration**: every 6.x environment variable honoured with its default, except the Valkey ones, which are dropped. New settings also live in the web UI
7. **Web UI**: the same routes, so bookmarks and links keep working

Deliberately not kept: Valkey, SSE transport (streamable HTTP only), `EMBEDDING_BACKEND=torch`, the per-process suppressed-topics cache (one process now), at-most-once enrichment.

## Phases

Each phase ends with a commit that builds, passes its tests, and is usable for what it covers.

| Phase | Delivers | Done when |
|---|---|---|
| **0. Foundation** | Workspace, `omnimem-core` (keys, content hash), `omnimem-embed` | Rust vectors match `reference_vectors.json` within 1e-5 |
| **1. Store** | Schema and migrations, vector matrix, filtered k-NN, backup import and export | A real 6.x backup imports, and `recall`-shaped queries return the same top 10 as Python on the benchmark corpus |
| **2. Core MCP** | Server on streamable HTTP with bearer auth; remember, remember_document, recall, recall_index, recall_detail, lifecycle, retag, forget, suppressions, dedup, contradiction tier 1, version, health, backup tools | Claude Code connects and a session works end to end |
| **3. Experience, projects, briefing** | Experience tools, project tools and domains, licence and provenance, lineage, briefing, audit, maintenance | The full session-start flow from `claude_config/CLAUDE.md` works |
| **4. Skills** | Compiler, propose-and-accept, find/get/bless, scan, knowledge watch, promotion, transfer bundles | Imported skills recompile with no diff |
| **5. LLM features** | Enrichment queue, fact extraction, query expansion, contradiction tier 2 | Behaviour matches with `ANTHROPIC_API_KEY` set and degrades the same without it |
| **6. RSS** | Feeds, summary and digest modes, licence gate, scheduler, feed influence | A feeds.yml from 6.x ingests the same articles |
| **7. Web UI and settings** | All routes and templates on minijinja, sessions, `/metrics`, settings pages for what is environment-only today | Every page renders against an imported store |
| **8. OAuth and hardening** | OAuth 2.1 server (register, authorize, token with PKCE, refresh rotation with grace window, revoke), Host/Origin guard, fail-closed public bind | claude.ai connects through a reverse proxy |
| **9. Cut-over** | Single-image Docker build, docs, delete the Python tree | 7.0.0 |
| **10. Mycelium** | `cluster_profile`, freshness, by_hash, classification filter, epoch bumps | `docs/v7-change-spec.md` |

## Testing

- The Python suite is the specification. Each Rust module ports the behaviour its Python tests assert, not the fakes
- **Golden files** from the Python implementation for everything with a byte-level contract: vectors, skill bodies, bundle manifests, backup exports, recall orderings on a fixed corpus
- **No fakes for the store.** SQLite runs in memory in tests, so the fake-diverges-from-server failure behind the 6.6.0 index migration bug can't recur
- The real model runs in tests when the Hugging Face cache has it, and is skipped with a message when it doesn't

## Known hard parts

| Area | Problem | Plan |
|---|---|---|
| `dateparser` | No Rust crate parses relative dates in prose ("last Tuesday") the same way | Port the pre-filter regex, handle the phrases the temporal tests cover, golden-test against Python on a phrase corpus, and accept documented differences beyond it |
| OAuth 2.1 server | `rmcp` provides no authorisation server; FastMCP did | Hand-build on axum with the storage semantics in the inventory, including the rotation grace window |
| `render_skill_md` | Must match `json.dumps` defaults and `difflib.unified_diff` output | Implement the json escaping exactly; port unified diff and golden-test it |
| Templates | 35 Jinja2 templates | minijinja is close to Jinja2; port verbatim and fix filters as found |
| Regex lookaround | The sentence chunker uses lookbehind | `fancy-regex`, already a dependency of the tokeniser |
| `feedparser` | Normalises many malformed feeds | `feed-rs`, with the 6.x feed list as a test corpus |
