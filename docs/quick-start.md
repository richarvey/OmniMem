# Quick Start

The fastest route is the installer, which uses the pre-built Docker Hub images. It checks Docker is installed, generates secure passwords, asks whether the MCP server should be reachable from your network, writes a sensible `.env`, and starts everything:

```bash
curl -fsSL https://codeberg.org/ric_harvey/omnimem/raw/branch/main/install.sh | bash
```

Works on macOS and Linux. See [../guides/docker-hub.md](../guides/docker-hub.md) for the manual version of the same setup.

Or build from source instead:

```bash
git clone https://codeberg.org/ric_harvey/omnimem.git && cd omnimem
cp .env.example .env
# Set VALKEY_PASSWORD and ANTHROPIC_API_KEY in .env
docker compose up -d
```

Edit the `.env` file to set at least `VALKEY_PASSWORD` to a secure value. You can also set `ANTHROPIC_API_KEY` if you want AI-powered RSS article summaries and richer contradiction detection. If you leave `ANTHROPIC_API_KEY` unset (or blank), OmniMem still works — the RSS worker will fall back to simple truncation for summaries, and contradiction checks will use embedding similarity only.

Four containers start: Valkey with vector search, the OmniMem MCP server, the RSS worker, and the web UI. The MCP server listens on port `8765` by default and the web UI on port `8080`.

Open `http://localhost:8080` in a browser to access the management dashboard — browse memories, run semantic searches, manage projects, track experience, and handle backups without needing to use MCP tool calls. See [web-ui.md](web-ui.md) for a tour.

## Connect your coding agent

The example below is for Claude Code — see the full guides for other tools:

| Agent | Guide | Transport |
|-------|-------|-----------|
| claude.ai | [../guides/claude-ai.md](../guides/claude-ai.md) | Streamable HTTP + OAuth 2.1 |
| Open Design | [../guides/open-design.md](../guides/open-design.md) | Streamable HTTP + OAuth 2.1 (public/PKCE) |
| Claude Code | [../guides/claude-code.md](../guides/claude-code.md) | SSE (default) / Streamable HTTP |
| Claude Desktop | [../guides/claude-desktop.md](../guides/claude-desktop.md) | SSE / Streamable HTTP (via mcp-remote) |
| GitHub Copilot | [../guides/github-copilot.md](../guides/github-copilot.md) | SSE (default) / Streamable HTTP |
| GitLab Duo | [../guides/gitlab-duo.md](../guides/gitlab-duo.md) | SSE (default) / Streamable HTTP |
| Cursor | [../guides/cursor.md](../guides/cursor.md) | SSE (default) / Streamable HTTP |
| AWS Kiro | [../guides/kiro.md](../guides/kiro.md) | SSE (default) / Streamable HTTP |
| OpenCode | [../guides/opencode.md](../guides/opencode.md) | SSE (default) / Streamable HTTP |
| OpenAI Codex CLI | [../guides/codex.md](../guides/codex.md) | SSE (default) / Streamable HTTP |

**Claude Code** (`~/.claude.json`):

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "sse",
      "url": "http://localhost:8765/sse"
    }
  }
}
```

If you set `MCP_AUTH_TOKEN` in your `.env`, add the token to the config:

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "sse",
      "url": "http://localhost:8765/sse",
      "headers": {
        "Authorization": "Bearer your-token-here"
      }
    }
  }
}
```

To stop Claude Code asking for permission every time it calls an OmniMem tool, add a wildcard allow rule to your global settings (`~/.claude/settings.json`):

```json
{
  "permissions": {
    "allow": [
      "mcp__omnimem__*"
    ]
  }
}
```

This allows all OmniMem MCP tools (`remember`, `recall`, `briefing`, etc.) to run without prompts across every project. If you already have other entries in the `allow` array, just add `"mcp__omnimem__*"` to it.

That is it. The server automatically delivers its usage guide to any connecting agent via the MCP protocol's `instructions` field. Claude Code will load project context at session start, check the graveyard before suggesting approaches, and store what it learns as you go — no manual configuration file needed.

If you want to customise the instructions or use OmniMem with a setup that does not support MCP instructions, a copy of the guide lives at `claude_config/CLAUDE.md` for manual use.

## Next steps

- [Configuration reference](configuration.md) — every environment variable
- [RSS feeds and the knowledge base](rss-knowledge.md) — set up passive knowledge ingestion
- [Using it from multiple machines](remote-access.md) — reverse proxy, OAuth 2.1 for claude.ai, security checklist
- [Features in depth](features.md) — lifecycle, graveyard, experience scoring, and the rest
