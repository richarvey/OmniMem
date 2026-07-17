# OmniMem Documentation

Everything that used to live in one very long README, now in sensible pieces. Start with the [project README](../README.md) for the overview.

## Getting started

- [Quick start](quick-start.md) — installer, building from source, connecting your coding agent
- [Configuration reference](configuration.md) — every environment variable and its default
- [Using it from multiple machines](remote-access.md) — reverse proxy, security checklist, OAuth 2.1 for claude.ai, and the 421/403 troubleshooting guide
- [Reverse proxy examples](reverse-proxy.md) — Traefik and Caddy configs for the web UI

## Using OmniMem

- [Features in depth](features.md) — memory lifecycle, the graveyard, experience scoring, deduplication, contradiction detection, session briefing, auto-maintenance
- [The skill compiler](skill-compiler.md) — compiling experience into loadable SKILL.md documents, the propose-and-accept gate, promoted reference material
- [RSS feeds and the knowledge base](rss-knowledge.md) — passive knowledge ingestion and promotion
- [MCP tool reference](mcp-tools.md) — all 30+ tools with parameters
- [Web UI](web-ui.md) — the management dashboard, page by page, plus Prometheus metrics

## Internals

- [Architecture](architecture.md) — the four containers, the recall pipeline, and key design decisions
- [Memory types overview](memory-types.md) — the storage model shared by all five namespaces
  - [Episodic](memory-episodic.md) · [Project](memory-project.md) · [Knowledge](memory-knowledge.md) · [Preference](memory-preference.md) · [Skill](memory-skill.md)

## Connection guides

Per-agent setup lives in [../guides/](../guides/): claude.ai, Claude Code, Claude Desktop, Cursor, GitHub Copilot, GitLab Duo, AWS Kiro, OpenCode, OpenAI Codex CLI, Open Design, and the Docker Hub images.
