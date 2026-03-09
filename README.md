# OmniMem

A self-hosted semantic memory system for Claude Code, exposed as an MCP server. OmniMem gives Claude Code persistent, cross-session, cross-project memory backed by Valkey (Redis fork) with vector search. It also ingests RSS feeds on a schedule to build passive base knowledge that can surface during conversations.

## Overview

OmniMem provides Claude Code with long-term semantic memory across sessions, projects, and machines. Memories have a lifecycle (active, deprioritised, archived, deleted) with surface score weighting, so humans can "soft forget" things without losing them permanently.

Episodic memories carry an **experience score** capturing how hard something was to get right, what approaches were abandoned and why, and what finally cracked it. Hard-won knowledge surfaces more readily; dead ends warn before they waste time again.

Three memory namespaces:
- **Episodic** — decisions, solutions, patterns, debugging outcomes
- **Project** — project-level context: stack, goals, current state
- **Knowledge** — RSS-ingested articles, summarised and embedded

## Quick Start

### Prerequisites
- Docker and Docker Compose

### Setup
```bash
git clone <repo-url> omnimem && cd omnimem
cp .env.example .env
# Edit .env — set VALKEY_PASSWORD and ANTHROPIC_API_KEY
docker compose up -d
```

Verify the server is running by calling the `health` MCP tool.

## Configuration

| Variable | Default | Description |
|---|---|---|
| `VALKEY_HOST` | `valkey` | Valkey hostname |
| `VALKEY_PORT` | `6379` | Valkey port |
| `VALKEY_PASSWORD` | `changeme` | Valkey authentication password |
| `MCP_PORT` | `8765` | Port the MCP server listens on |
| `ANTHROPIC_API_KEY` | — | API key for RSS article summarisation |
| `EMBEDDING_MODEL` | `all-MiniLM-L6-v2` | Sentence-transformers model name |
| `RSS_SCHEDULE_HOURS` | `6` | Hours between RSS ingestion cycles |
| `RSS_MAX_ARTICLES_PER_FEED` | `20` | Max articles to process per feed per cycle |
| `MEMORY_RECALL_TOP_K` | `5` | Default number of recall results |
| `DEPRIORITISED_WEIGHT` | `0.2` | Surface score for deprioritised memories |
| `RECENCY_DECAY_DAYS` | `90` | Days before recency decay begins |
| `BACKUP_DIR` | `/app/backups` | Directory for backup files |

## MCP Tools Reference

### Core Memory

| Tool | Parameters | Description |
|---|---|---|
| `remember` | `content, project?, tags?, namespace?` | Store a new memory |
| `recall` | `query, top_k?, namespaces?, project_filter?` | Semantic search across memories |
| `deprioritise` | `key_or_query, reason, reinstate_hints?` | Reduce visibility without deleting |
| `archive` | `key_or_query, reason?` | Remove from recall results entirely |
| `reinstate` | `key_or_query` | Restore to active state |
| `forget` | `key_or_query, confirm?` | Hard delete (requires confirm=True) |
| `suppress_topic` | `topic, reason?` | Filter a topic from all recalls |
| `unsuppress_topic` | `topic` | Remove topic suppression |
| `list_suppressions` | — | List all suppressed topics |

### Project Context

| Tool | Parameters | Description |
|---|---|---|
| `set_project_context` | `project_name, description, stack, goals, current_state, notes?` | Create/update project context |
| `get_project_context` | `project_name` | Retrieve project context |
| `list_projects` | — | List all stored projects |
| `update_project_state` | `project_name, current_state, notes?` | Update state without re-embedding |

### Experience Scoring

| Tool | Parameters | Description |
|---|---|---|
| `record_experience` | `key, effort_score, outcome, iterations?, abandoned_approaches?, breakthrough?, gotchas?` | Record how hard a problem was |
| `log_abandoned` | `key, name, type, reason` | Incrementally record a dead end |
| `get_experience` | `key` | Get full experience data for a memory |
| `experience_summary` | `project?` | Aggregate view: effort stats, graveyard, breakthroughs |
| `warn_if_abandoned` | `query` | Check if an approach was previously abandoned |

### Audit

| Tool | Parameters | Description |
|---|---|---|
| `memory_audit` | `project?, namespace?, include_archived?` | Summary of all memories by state |
| `why_did_you_mention` | `query` | Explain why a topic was surfaced |
| `explain_memory` | `key` | Full metadata for a single memory |

### Backup

| Tool | Parameters | Description |
|---|---|---|
| `dump_to_file` | `filename?` | Export all data to JSON backup |
| `restore_from_file` | `filename, dry_run?` | Restore from backup (merge, not replace) |
| `list_backups` | — | List available backup files |

### System

| Tool | Parameters | Description |
|---|---|---|
| `health` | — | Server health check: Valkey, indexes, model, uptime |

## RSS Configuration

Edit `rss_worker/feeds.yml` to configure which feeds are ingested:

```yaml
feeds:
  - url: https://blog.rust-lang.org/feed.xml
    name: Rust Official Blog
    topics: [rust, systems, language]
```

Each feed entry supports:
- `url` — RSS/Atom feed URL
- `name` — Human-readable feed name
- `topics` — List of topic tags for categorisation

Articles are fetched, summarised using Claude Haiku, embedded, and stored in the `knowledge` namespace. Duplicate articles (by URL) are skipped automatically.

## Memory Lifecycle

```
                    +-----------------+
                    |     ACTIVE      |  surface_score = 1.0
                    +--------+--------+
                             |
              +--------------+--------------+
              |                             |
    +---------v---------+         +---------v---------+
    |   DEPRIORITISED   |         |     ARCHIVED      |  surface_score = 0.0
    |  surface_score=0.2|         +---------+---------+
    +---------+---------+                   |
              |                             |
              +----------+    +-------------+
                         |    |
                   +-----v----v-----+
                   |    DELETED     |  hard delete
                   +---------------+
```

**Surface scores by state:**

| State | Surface Score | Effect |
|---|---|---|
| Active | 1.0 | Full visibility in recall |
| Deprioritised | 0.2 | Heavily reduced visibility |
| Archived | 0.0 | Hidden from recall entirely |
| Deleted | — | Removed from store |

**Guidance:**
- Use `deprioritise` when something should stop surfacing but might be needed later. Include `reinstate_hints` for conditions that should bring it back.
- Use `archive` for content that's definitely outdated but might have historical value.
- Use `forget` only for content that should be permanently destroyed.

**High-effort lifecycle guard:** When deprioritising a memory with `effort_score >= 4`, the system returns an advisory warning that this represents hard-won knowledge. The transition still proceeds, but alerts you to consider archiving instead.

## Experience Scoring

### Effort Score Guide

| Score | Meaning |
|---|---|
| 1 | First attempt succeeded, no meaningful obstacles |
| 2 | Minor friction: one wrong turn, quick fix |
| 3 | Moderate effort: multiple iterations, some debugging |
| 4 | Significant struggle: hours of effort, approach changes required |
| 5 | Battle-hardened: near-abandonment, fundamental rethink required |

### Outcome Types
- **succeeded** — Problem solved
- **pivoted** — Solved differently than originally planned
- **abandoned** — Gave up on this approach

### The Graveyard

Every abandoned approach is tracked with its name, type (library/approach/tool/pattern/service), and reason. The `experience_summary` tool aggregates these across all memories into "the graveyard" — a consolidated list of all dead ends.

When `effort_score >= 4` and `outcome == "abandoned"`, the abandoned approach names are automatically suppressed from future recall results.

### Experience Weight Formula

```
experience_weight = base_weight * effort_multiplier

base_weight:     succeeded=1.0, pivoted=0.7, abandoned=0.1
effort_multiplier: 1=1.0, 2=1.1, 3=1.25, 4=1.5, 5=1.8
```

Cap: 2.0. Effort does not amplify failures (abandoned always returns 0.1).

### Abandoned Fast-Path

During recall, before any vector search, the system scans for abandoned approaches matching the query. Matches are injected at the top of results with `result_type="abandoned_warning"`, bypassing normal scoring. This ensures dead ends are always surfaced when relevant.

## Backup & Restore

### Creating a Backup
Call `dump_to_file()` — auto-generates a timestamped filename. All memories, suppressions, and recall logs are exported to JSON in the `BACKUP_DIR`.

### Restoring
1. Call `restore_from_file(filename)` with `dry_run=True` (default) to preview
2. Call `restore_from_file(filename, dry_run=False)` to restore

Restoring **merges** with existing data. Existing keys are only overwritten if the backup version is newer (based on `updated_at`).

### Backup Location
Backups are stored in `./backups/` on the host (mounted to `/app/backups` in the container).

## CLAUDE.md Integration

Copy `claude_config/CLAUDE.md` into any project directory to give Claude Code access to OmniMem.

Add the MCP server to your Claude Code config (`~/.claude.json` or project `.mcp.json`):

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

For Claude Desktop, add the same config to `claude_desktop_config.json`.

## Accessing from Multiple Machines

Expose the `MCP_PORT` through your firewall or a reverse proxy. Example Traefik configuration:

```yaml
# Add to docker-compose.yml labels on mcp_server
labels:
  - "traefik.enable=true"
  - "traefik.http.routers.omnimem.rule=Host(`omnimem.yourdomain.com`)"
  - "traefik.http.routers.omnimem.tls.certresolver=letsencrypt"
  - "traefik.http.services.omnimem.loadbalancer.server.port=8765"
```

Then update the MCP config URL to `https://omnimem.yourdomain.com/sse`.

**Security considerations:**
- Set a strong `VALKEY_PASSWORD`
- Use TLS (HTTPS) when exposing over the network
- Consider adding authentication middleware to the reverse proxy
- Do not expose the Valkey port directly to the internet

## Architecture

```
+------------------+     +------------------+     +------------------+
|   Claude Code    |     |   RSS Worker     |     |   Valkey         |
|                  |     |                  |     |   (+ search)     |
|  MCP Client      |     |  APScheduler     |     |                  |
|  (SSE transport) |     |  feedparser      |     |  idx:episodic    |
+--------+---------+     |  summariser      |     |  idx:project     |
         |               +--------+---------+     |  idx:knowledge   |
         | SSE                     |               +--------+---------+
         v                         |                        ^
+--------+---------+               |                        |
|   MCP Server     |               +------------------------+
|                  |                      valkey writes
|  fastmcp         |-------------------------------------->
|  sentence-        |         valkey reads/writes
|  transformers    |
|                  |
|  Tools:          |
|  - Core          |
|  - Project       |
|  - Experience    |
|  - Audit         |
|  - Backup        |
+------------------+

Scoring Pipeline (recall):
  query -> abandoned fast-path (keyword scan)
        -> embed query
        -> vector search (top 20 per namespace)
        -> filter archived/deleted
        -> filter suppressed topics
        -> apply surface_score (lifecycle state)
        -> apply recency_decay (age penalty)
        -> apply experience_weight (effort/outcome)
        -> check reinstate eligibility
        -> merge, rank, return top_k
```
