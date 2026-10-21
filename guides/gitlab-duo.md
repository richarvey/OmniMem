# Connecting OmniMem to GitLab Duo

GitLab Duo has supported MCP servers since GitLab 18.1 (as an experiment) and generally since 18.8. It works in VS Code and JetBrains IDEs through the GitLab Workflow extension.

## Requirements

- **GitLab 18.8 or later** for the generally available version
- **Premium or Ultimate** with the Duo Core add-on
- **GitLab Workflow VS Code extension v6.28.2+** (v6.35.6+ for workspace-scoped config)
- **"Allow external MCP tools"** switched on in your GitLab Duo admin settings at `/settings/gitlab_duo/configuration`
- It does **not** work with GitLab Duo self-hosted models

## Quick setup

Create or edit `~/.gitlab/duo/mcp.json` for global access:

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "http",
      "url": "http://127.0.0.1:8765/mcp",
      "approvedTools": true
    }
  }
}
```

The desktop app's **Copy MCP URL** menu item gives you that address.

`"approvedTools": true` pre-approves every OmniMem tool so you aren't asked each session. It's your own server, so trusting it is reasonable. If you'd rather be choosy:

```json
"approvedTools": ["briefing", "recall", "remember", "health"]
```

Coming from 6.x, change `"type": "sse"` to `"type": "http"` and `/sse` to `/mcp`.

## Global vs workspace config

| File | Scope | Extension version | Use when |
|------|-------|-------------------|----------|
| `~/.gitlab/duo/mcp.json` | All projects | v6.28.2+ | OmniMem should be available everywhere (recommended) |
| `.gitlab/duo/mcp.json` (project root) | Single project | v6.35.6+ | Only this project needs OmniMem |

Workspace config wins over user config.

## The authentication catch

GitLab Duo's `mcp.json` doesn't support a `headers` field for HTTP servers, so there's nowhere to put `Authorization: Bearer`. Your options:

1. **Run without a token on localhost.** OmniMem listens on `127.0.0.1` by default and happily runs without auth there. Anything listening beyond localhost refuses to start without a token or OAuth, so you can't accidentally leave it open.
2. **Put a local reverse proxy in front** that adds the header for you. See [the reverse proxy docs](../docs/reverse-proxy.md).
3. **Use an SSH tunnel** to a remote OmniMem, so it looks local to Duo.

## Using OmniMem with GitLab Duo

Once it's configured, the tools are available in Duo's agentic chat:

- "Call the OmniMem briefing tool for this project"
- "Remember this architectural decision in OmniMem"
- "Check whether we've tried this approach before"

## Verifying the connection

In the GitLab Duo chat panel, ask:

> Can you call the OmniMem health tool?

You should get back the record and vector counts per namespace, whether the embedding model is loaded, and the uptime.

## Known quirks

- **No custom headers**: as above, bearer tokens can't go in the config file.
- **VS Code and JetBrains only**: the GitLab Web IDE doesn't support MCP servers.
- **Tool approval sticks**: `approvedTools: true` survives restarts. Without it you're asked once per session, not once per call.
- **AI Catalog MCP servers** (18.10+, experimental) are a separate, admin-managed feature.

## Notes

- If OmniMem isn't answering, `curl http://127.0.0.1:8765/healthz` should return `{"status": "ok"}`, and the desktop app's tray status line says whether it's running
- Stale auth state can pile up in `~/.mcp-auth/`; delete `~/.mcp-auth/mcp-remote*` to reset it
- Check the [GitLab MCP clients documentation](https://docs.gitlab.com/user/gitlab_duo/model_context_protocol/mcp_clients/) for the latest options
