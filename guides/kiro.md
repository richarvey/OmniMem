# Connecting OmniMem to Kiro

Kiro is AWS's AI-powered IDE built on VS Code. It supports MCP servers via SSE transport.

## Quick setup

Create or edit `~/.kiro/settings/mcp.json` for global access:

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
        "Authorization": "Bearer ${OMNIMEM_TOKEN}"
      }
    }
  }
}
```

Set `OMNIMEM_TOKEN` in your shell environment (e.g. `.zshrc` or `.bashrc`) so Kiro can expand it. Environment variables use `${VAR_NAME}` syntax in the IDE config.

## Global vs project config

| File | Scope | Use when |
|------|-------|----------|
| `~/.kiro/settings/mcp.json` | All projects | OmniMem should be available everywhere (recommended) |
| `.kiro/settings/mcp.json` (project root) | Single project | Only this project needs OmniMem |

## MCP settings panel

You can also configure MCP servers through Kiro's UI:

1. Open the **MCP Servers** panel from the sidebar or command palette
2. Click **Add Server**
3. Select **SSE** as the transport type
4. Enter the URL: `http://localhost:8765/sse`
5. Add the authorisation header if auth is enabled

## Using OmniMem with Kiro

Once connected, OmniMem's tools are available to Kiro's AI agent. You can use them through the chat interface:

- "Call the OmniMem briefing tool for this project"
- "Remember this architectural decision in OmniMem"
- "Check if we've tried this approach before"

Kiro's spec-driven development workflow pairs well with OmniMem's project context -- the agent can load project state at the start of each session and store decisions as specs evolve.

## Verifying the connection

In Kiro's chat panel, ask:

> Can you call the OmniMem health tool?

You should see Valkey connection status, index counts, and embedding model status.

## Known quirks

- **Silent failures**: If OmniMem is unreachable, Kiro can fail to load all MCP servers without any visible error. If tools suddenly disappear, check that OmniMem's Docker containers are running.
- **CLI vs IDE env var syntax**: The IDE uses `${VAR_NAME}` but the Kiro CLI expects `${env:VAR_NAME}`. You cannot share a single `mcp.json` between both without editing it.
- **OAuth redirects don't work**: If you ever add OAuth-based auth to OmniMem via a reverse proxy, Kiro cannot complete localhost OAuth redirects. Plain bearer tokens via `headers` work fine.
- **Streamable HTTP**: Kiro also supports `"type": "streamable-http"` for the newer MCP transport, but OmniMem currently uses SSE. Stick with `"type": "sse"`.

## Notes

- If OmniMem is not responding, check that Docker containers are running: `docker compose ps`
- Check the [Kiro documentation](https://kiro.dev/docs/mcp/configuration/) for the latest MCP configuration options
