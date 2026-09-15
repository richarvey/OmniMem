# Connecting OmniMem to Claude Code

Claude Code is Anthropic's terminal coding agent, and it's the tool I built OmniMem for in the first place. It speaks Streamable HTTP natively, so there's no bridge and no faff: point it at the URL and you're done.

OmniMem 7 serves MCP at `/mcp` over Streamable HTTP. The old SSE endpoint (`/sse`) has gone, so if you're carrying a 6.x config across, change the URL and the transport.

## Quick setup

The quickest route is the CLI:

```bash
claude mcp add --transport http omnimem http://127.0.0.1:8765/mcp --scope user
```

Running the desktop app? Click the OmniMem icon in your tray or menu bar and pick **Copy MCP URL**, then paste that in instead of typing it.

If you'd rather edit the JSON yourself, add OmniMem to `~/.claude.json`:

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "http",
      "url": "http://127.0.0.1:8765/mcp"
    }
  }
}
```

### With an access token

If OmniMem has an access token set (`MCP_AUTH_TOKEN`, or **Access token** on the desktop app's Configuration page), send it as a header:

```bash
claude mcp add --transport http omnimem http://127.0.0.1:8765/mcp \
  --header "Authorization: Bearer your-token-here" \
  --scope user
```

Or in `~/.claude.json`, using environment variable expansion so the token isn't sitting in a file:

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "http",
      "url": "http://127.0.0.1:8765/mcp",
      "headers": {
        "Authorization": "Bearer ${OMNIMEM_TOKEN}"
      }
    }
  }
}
```

## Global vs project config

| File | Scope | Shared in git? | Use when |
|------|-------|----------------|----------|
| `~/.claude.json` | All projects | No | OmniMem should be available everywhere (recommended) |
| `.claude/` directory | Single project, personal | No | Personal config for one project |
| `.mcp.json` (project root) | Single project, team | Yes | Team shares OmniMem config via version control |

Precedence when the same server name exists at more than one level: local, then project, then user.

Go global. The whole point of OmniMem is that what you learn in one project turns up in the next.

## Auto-allow OmniMem tools

By default Claude Code asks before every OmniMem tool call, which gets old fast. Allow them all in `~/.claude/settings.json`:

```json
{
  "permissions": {
    "allow": [
      "mcp__omnimem__*"
    ]
  }
}
```

That covers all 48 tools (`remember`, `recall`, `briefing` and the rest).

## Auto-loaded instructions

OmniMem sends its usage guide in the MCP `instructions` field when Claude Code connects, so the agent already knows the session workflow, the tagging vocabulary and how to record experience. You don't have to set anything up.

If you want to tweak how Claude uses OmniMem, copy `claude_config/CLAUDE.md` from the repo into your project (or into `~/.claude/CLAUDE.md` for every project) and edit away.

## Verifying the connection

Start Claude Code and run:

```
/mcp
```

You should see `omnimem` listed as connected. Ask Claude to call the `health` tool and you'll get back the record and vector counts per namespace, whether the embedding model is loaded, and the uptime.

## Remote access

If OmniMem runs on another machine, point the URL at it and send the token:

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "http",
      "url": "https://omnimem.yourdomain.com/mcp",
      "headers": {
        "Authorization": "Bearer your-token-here"
      }
    }
  }
}
```

A server listening beyond localhost won't start without an access token or OAuth, so you can't forget. See [remote access](../docs/remote-access.md) for reverse proxies, tunnels and OAuth.

## Tips

- If tool output is being truncated, raise the limit: `MAX_MCP_OUTPUT_TOKENS=50000 claude`
- If the first call times out while the embedding model loads, give it longer: `MCP_TIMEOUT=10000 claude` (10 seconds)
- Reset project MCP approvals with `claude mcp reset-project-choices`
