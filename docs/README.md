# OmniMem Documentation

Everything that used to live in one very long README, in sensible pieces. Start with the [project README](../README.md) for the overview.

These pages describe OmniMem 7, the single Rust binary. It isn't released yet, so the install steps are for the packages that arrive with 7.0.0 (each one says so). 6.x's docs live on the `v6.7.x` branch.

## Getting started

- [Quick start](quick-start.md): installing, building from source, moving from 6.x, connecting your agent
- [Configuration](configuration.md): every setting, where the desktop app and headless installs keep them, and their defaults
- [Using it from multiple machines](remote-access.md): reverse proxies, the security checklist, OAuth 2.1 for claude.ai, and fixing 421 and 403 errors
- [Reverse proxy examples](reverse-proxy.md): Traefik and Caddy configs

## Using OmniMem

- [Features in depth](features.md): the memory lifecycle, the graveyard, experience scoring, deduplication, contradiction detection, the session briefing, auto-maintenance
- [The skill compiler](skill-compiler.md): compiling experience into SKILL.md documents, the propose-and-accept gate, promoted reference material
- [RSS feeds and the knowledge base](rss-knowledge.md): passive knowledge ingestion and promotion
- [MCP tool reference](mcp-tools.md): all 48 tools with their parameters
- [The settings panel](settings-panel.md): the desktop app's window, page by page

## Internals

- [Architecture](architecture.md): the one process, the recall pipeline and the design decisions behind it
- [Memory types overview](memory-types.md): the storage model all five namespaces share
  - [Episodic](memory-episodic.md) · [Project](memory-project.md) · [Knowledge](memory-knowledge.md) · [Preference](memory-preference.md) · [Skill](memory-skill.md)
- [The Rust port plan](rust-port-plan.md): how 7.0 was built, phase by phase, and what it promises to keep from 6.x
- [The v7 change spec](v7-change-spec.md): the Mycelium adapter that follows the port

## Connection and deployment guides

Per-agent setup lives in [../guides/](../guides/): claude.ai, Claude Code, Claude Desktop, Cursor, GitHub Copilot, GitLab Duo, AWS Kiro, OpenCode, OpenAI Codex CLI and Open Design.

Deployment guides live there too: [Docker](../guides/docker.md), [macOS](../guides/omnimem-setup-macos.md), [Raspberry Pi](../guides/omnimem-setup-raspberry-pi.md), [AWS (Linux)](../guides/omnimem-setup-aws-linux.md), [GCP (Linux)](../guides/omnimem-setup-gcp-linux.md) and [Linux with Tailscale Funnel](../guides/omnimem-setup-linux-tailscale.md).
