# Connecting OmniMem to OpenCode

OpenCode is an open-source, terminal-based AI coding agent. It supports MCP servers natively via SSE and Streamable HTTP transports.

## Quick setup

Create or edit `~/.config/opencode/opencode.json` for global access:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "omnimem": {
      "type": "remote",
      "url": "http://localhost:8765/mcp",
      "oauth": false,
      "timeout": 15000
    }
  }
}
```

Note: OpenCode uses `"mcp"` as the top-level key (not `"mcpServers"`) and `"type": "remote"` for network servers.

Setting `"oauth": false` is recommended to prevent OpenCode's auto OAuth negotiation from interfering with OmniMem's simple bearer token auth.

If you have bearer token auth enabled (`MCP_AUTH_TOKEN` set in your `.env`), add the token as a header:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "omnimem": {
      "type": "remote",
      "url": "http://localhost:8765/mcp",
      "headers": {
        "Authorization": "Bearer {env:OMNIMEM_TOKEN}"
      },
      "oauth": false,
      "timeout": 15000
    }
  }
}
```

Set `OMNIMEM_TOKEN` in your shell environment. Note: OpenCode uses `{env:VAR_NAME}` syntax (single braces, not `${}`).

## Config file locations

OpenCode merges configs from all locations, with later sources overriding conflicting keys:

| Priority | Location | Use when |
|----------|----------|----------|
| Lowest | `~/.config/opencode/opencode.json` | OmniMem should be available everywhere (recommended) |
| Higher | `opencode.json` in project root | Only this project needs OmniMem |
| Highest | `OPENCODE_CONFIG_CONTENT` env var | Inline JSON for CI/testing |

## Using OmniMem with OpenCode

Once configured, OmniMem's tools are available to the LLM automatically. You can prompt OpenCode to use them:

- "Call the OmniMem briefing tool for this project"
- "Remember this decision in OmniMem"
- "Check if we've tried this approach before"

## Verifying the connection

Start an OpenCode session and ask:

> Call the OmniMem health tool to check the connection

You should see Valkey connection status, index counts, and embedding model status.

## Known quirks

- **Silent tool registration failure**: There is an open bug where OpenCode shows a green "connected" status but silently fails to register any tools. If OmniMem tools do not appear, restart OpenCode and check terminal output for errors.
- **OAuth auto-negotiation**: OpenCode auto-detects OAuth on remote connections by watching for 401 responses. For OmniMem's simple bearer token auth, set `"oauth": false` to prevent unexpected auth flows.
- **Default timeout is 5 seconds**: OmniMem's embedding model may need longer on first call. Set `"timeout": 15000` (15 seconds) to avoid premature timeouts.
- **Context window consumption**: Every MCP tool description is injected into the LLM context on each request. OmniMem's 30+ tools will consume a non-trivial number of tokens. Use `"enabled": false` to temporarily disable OmniMem for a session if needed.
- **Transport detection order**: OpenCode tries SSE first, then falls back to Streamable HTTP. Point it directly at `/mcp` to use Streamable HTTP without the detection step.

## Notes

- If OmniMem is not responding, check that Docker containers are running: `docker compose ps`
- OAuth tokens (if auto-negotiation triggers accidentally) are stored at `~/.local/share/opencode/mcp-auth.json`
- Check the [OpenCode documentation](https://opencode.ai/docs/mcp-servers/) for the latest MCP configuration options
