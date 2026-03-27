# Connecting OmniMem to Claude Code

Claude Code is Anthropic's CLI-based coding agent. It supports MCP servers natively via SSE and Streamable HTTP transports.

> [!WARNING]
> **SSE transport is deprecated.** OmniMem 3.10 defaults to SSE but will switch to Streamable HTTP in a future release. To migrate early, set `MCP_TRANSPORT=http` in your `.env` and use the Streamable HTTP config shown below.

## Quick setup

Add OmniMem to your global config at `~/.claude.json`:

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

If you have bearer token auth enabled (`MCP_AUTH_TOKEN` set in your `.env`), add the token as a header:

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

You can also use environment variable expansion to avoid hardcoding the token:

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "sse",
      "url": "http://localhost:8765/sse",
      "headers": {
        "Authorization": "Bearer ${OMNIMEM_TOKEN}"
      }
    }
  }
}
```

### Streamable HTTP (recommended migration)

Set `MCP_TRANSPORT=http` in your `.env`, then use this config instead:

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "http",
      "url": "http://localhost:8765/mcp"
    }
  }
}
```

### Adding via CLI

Instead of editing JSON manually, you can use the `claude mcp add` command:

```bash
# SSE (current default)
claude mcp add --transport sse omnimem http://localhost:8765/sse --scope user

# Streamable HTTP (after setting MCP_TRANSPORT=http)
claude mcp add --transport http omnimem http://localhost:8765/mcp --scope user

# With auth header
claude mcp add --transport sse omnimem http://localhost:8765/sse \
  --header "Authorization: Bearer your-token-here" \
  --scope user
```

## Global vs project config

| File | Scope | Shared in git? | Use when |
|------|-------|----------------|----------|
| `~/.claude.json` | All projects | No | OmniMem should be available everywhere (recommended) |
| `.claude/` directory | Single project, personal | No | Personal config for one project |
| `.mcp.json` (project root) | Single project, team | Yes | Team shares OmniMem config via version control |

Precedence when the same server name exists at multiple levels: local > project > user.

For most users, the global config is the right choice since OmniMem is designed to share memory across projects.

## Auto-allow OmniMem tools

By default, Claude Code will ask for permission each time it calls an OmniMem tool. To auto-allow all OmniMem tools, add a wildcard rule to `~/.claude/settings.json`:

```json
{
  "permissions": {
    "allow": [
      "mcp__omnimem__*"
    ]
  }
}
```

This allows all 30+ OmniMem tools (`remember`, `recall`, `briefing`, etc.) to run without prompts.

## Auto-loaded instructions

OmniMem ships its full usage guide via the MCP protocol's `instructions` field. Claude Code loads this automatically on connection, so your agent will know how to use OmniMem's session workflow, tagging vocabulary, and experience recording without any manual setup.

If you want to customise the instructions, copy `claude_config/CLAUDE.md` from the OmniMem repo into your project and edit as needed.

## Verifying the connection

Start Claude Code and run:

```
/mcp
```

You should see `omnimem` listed as a connected server. You can also ask Claude to call the `health()` tool to check the connection to Valkey and the embedding model.

## Remote access

If OmniMem runs on a different machine, update the URL to point to your server:

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "sse",
      "url": "https://omnimem.yourdomain.com/sse",
      "headers": {
        "Authorization": "Bearer your-token-here"
      }
    }
  }
}
```

See `docs/reverse-proxy.md` for Traefik and Caddy configuration examples.

## Tips

- If MCP tool output is being truncated, increase the limit: `MAX_MCP_OUTPUT_TOKENS=50000 claude`
- Set a custom timeout if OmniMem is slow to respond: `MCP_TIMEOUT=10000 claude` (10 seconds)
- Reset project MCP approvals with: `claude mcp reset-project-choices`
