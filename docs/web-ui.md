# Web UI

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
| **Skills** | Browse compiled skills: rules with reinforcement counts and source citations, the full SKILL.md body, load counters, and the source manifest linking back to the memories each skill was compiled from. The New Skill modal compiles a draft for a domain through the same propose-and-accept gate as the MCP `compile_skill` tool — you review the draft in place and nothing is written until you accept it. Skills can be deleted (with confirmation) but never edited: to change one, update the underlying memories and recompile. Export downloads a skill together with its source memories as a checksummed zip bundle; Import validates such a bundle, previews exactly what would be added, and only writes on confirm — strictly additive, so existing skills and memories on the receiving instance are never overwritten |
| **Experience** | Summary dashboard with effort stats, breakthroughs, and a paginated most-effortful table filterable by outcome (succeeded, pivoted, abandoned). The abandoned approach graveyard has its own page |
| **Duplicates** | Scan a namespace for near-identical memory clusters. Archive extras directly |
| **Contradictions** | Side-by-side comparison of contradicting memories with resolve actions |
| **Suppressions** | Add and remove suppressed topics inline |
| **Telemetry** | Recall counters, most recalled, gone cold, never recalled. Filter by project. Includes skill load counts (`get_skill` bumps the same counters) |
| **Token Overhead** | Measured tool call metrics: calls, avg duration, avg tokens, errors per tool, counted since the last reset (a Reset Counters button flushes them from Valkey). Static context cost breakdown |
| **Backups** | Create backups, preview restore contents, and confirm restore |

## Prometheus metrics

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

## Authentication

The web UI asks for a login whenever the OAuth admin credentials (`OAUTH_ADMIN_USER` / `OAUTH_ADMIN_PASSWORD`) are set — one set of credentials for both the MCP OAuth flow and the dashboard. Sessions are opaque tokens in Valkey (`WEB_UI_SESSION_HOURS`, default 7 days) behind an HttpOnly cookie, revoked server-side by the footer's Sign out button, with failed attempts rate limited per IP. Set `WEB_UI_LOGIN_ENABLED=false` to opt out. Bearer token authentication via `WEB_UI_AUTH_TOKEN` works alongside it for scripts. The `/metrics` endpoint is exempt so Prometheus can scrape without credentials. For additional security options (TLS, IP allowlisting, SSO), see [reverse-proxy.md](reverse-proxy.md).
