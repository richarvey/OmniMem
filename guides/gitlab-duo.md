# Connecting OmniMem to GitLab Duo

GitLab Duo supports MCP servers natively since GitLab 18.1 (experiment), with general availability in GitLab 18.8. It works in VS Code and JetBrains IDEs via the GitLab Workflow extension.

## Requirements

- **GitLab 18.8 or later** (for GA support)
- **Premium or Ultimate tier** with Duo Core add-on
- **GitLab Workflow VS Code extension v6.28.2+** (v6.35.6+ for workspace-scoped config)
- **"Allow external MCP tools"** must be enabled in your GitLab Duo admin settings at `/settings/gitlab_duo/configuration`
- Does **not** work with GitLab Duo self-hosted models

## Quick setup

Create or edit `~/.gitlab/duo/mcp.json` for global access:

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "sse",
      "url": "http://localhost:8765/sse",
      "approvedTools": true
    }
  }
}
```

Setting `"approvedTools": true` pre-approves all OmniMem tools so you are not prompted each session. Since this is your own self-hosted server, trusting all tools is reasonable. You can also approve specific tools only:

```json
"approvedTools": ["briefing", "recall", "remember", "health"]
```

## Global vs workspace config

| File | Scope | Extension version | Use when |
|------|-------|-------------------|----------|
| `~/.gitlab/duo/mcp.json` | All projects | v6.28.2+ | OmniMem should be available everywhere (recommended) |
| `.gitlab/duo/mcp.json` (project root) | Single project | v6.35.6+ | Only this project needs OmniMem |

Workspace config takes precedence over user-level config.

## Authentication caveat

GitLab Duo's `mcp.json` does **not** currently support a `headers` field for SSE or HTTP server types. This means you cannot pass `Authorization: Bearer` tokens directly in the config.

If you need authentication, your options are:

1. **Run without auth** (default) -- OmniMem binds to `127.0.0.1` by default, so it is only accessible locally
2. **Use a reverse proxy** that injects the auth header -- see `docs/reverse-proxy.md`
3. **Use SSH tunnel** for remote access without exposing the port publicly

## Using OmniMem with GitLab Duo

Once configured, OmniMem's tools are available in GitLab Duo's agentic chat. You can prompt it to use them:

- "Call the OmniMem briefing tool for this project"
- "Remember this architectural decision in OmniMem"
- "Check if we've tried this approach before"

## Verifying the connection

In GitLab Duo's chat panel, ask:

> Can you call the OmniMem health tool?

You should see Valkey connection status, index counts, and embedding model status.

## Known quirks

- **No custom headers**: The `headers` field is not supported for `sse` or `http` server types. Bearer token auth cannot be passed via the config file.
- **VS Code and JetBrains only**: The GitLab Web IDE does not support MCP servers.
- **Tool approval persists**: `approvedTools: true` survives IDE restarts. Without it, you are prompted once per session (not per call).
- **Relative command paths**: For `stdio` servers, absolute paths are required if the command is not in `PATH`. Not relevant for OmniMem's SSE transport.
- **AI Catalog MCP servers** (18.10+, experimental) are a separate admin-managed feature that only supports HTTP transport, not SSE.

## Notes

- If OmniMem is not responding, check that Docker containers are running: `docker compose ps`
- Stale auth tokens may accumulate in `~/.mcp-auth/` -- delete `~/.mcp-auth/mcp-remote*` to reset
- Check the [GitLab MCP clients documentation](https://docs.gitlab.com/user/gitlab_duo/model_context_protocol/mcp_clients/) for the latest configuration options
