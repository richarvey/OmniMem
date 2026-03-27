# Connecting OmniMem to GitHub Copilot

GitHub Copilot in VS Code supports MCP servers natively since VS Code 1.99 (April 2025). It supports SSE and Streamable HTTP transports with custom headers.

> [!WARNING]
> **SSE transport is deprecated.** OmniMem 3.10 defaults to SSE but will switch to Streamable HTTP in a future release. To migrate early, set `MCP_TRANSPORT=http` in your `.env` and use `"type": "http"` with URL `.../mcp` in the config below.

## Requirements

- **VS Code 1.99 or later**
- **Copilot Free, Pro, or Pro+**: MCP works out of the box
- **Copilot Business/Enterprise**: your organisation admin must enable the "MCP servers in Copilot" policy

## Quick setup

Create or edit `.vscode/mcp.json` in your project root:

```json
{
  "servers": {
    "omnimem": {
      "type": "sse",
      "url": "http://localhost:8765/sse"
    }
  }
}
```

Note: Copilot uses `"servers"` as the top-level key, not `"mcpServers"`.

If you have bearer token auth enabled (`MCP_AUTH_TOKEN` set in your `.env`), you can use an input variable so VS Code prompts for the token securely:

```json
{
  "servers": {
    "omnimem": {
      "type": "sse",
      "url": "http://localhost:8765/sse",
      "headers": {
        "Authorization": "Bearer ${input:omnimem_token}"
      }
    }
  },
  "inputs": [
    {
      "type": "promptString",
      "id": "omnimem_token",
      "description": "OmniMem bearer token",
      "password": true
    }
  ]
}
```

VS Code will prompt for the token once and store it securely. You can also reference environment variables directly with `${env:OMNIMEM_TOKEN}`.

## Global vs project config

| Scope | How to access | Use when |
|-------|---------------|----------|
| Workspace | `.vscode/mcp.json` in project root | Share config with your team via version control |
| User (global) | Command palette: `MCP: Open User Configuration` | OmniMem should be available across all projects |

Do not configure the same server in both scopes -- this causes conflicts.

## Agent mode is required

MCP tools are only available in Copilot's **Agent mode**, not in standard Ask/Chat mode. To use OmniMem:

1. Open Copilot Chat (Ctrl/Cmd + Shift + I)
2. Select **Agent** from the mode dropdown
3. OmniMem tools will be available automatically

In Agent mode, Copilot can call OmniMem tools autonomously as part of multi-step tasks.

## Using OmniMem with Copilot

Once connected, you can ask Copilot's agent to use OmniMem tools:

- "Call the OmniMem briefing tool for this project"
- "Remember this architectural decision in OmniMem"
- "Check OmniMem for any prior solutions to this problem"

## Verifying the connection

VS Code shows a trust confirmation prompt before starting a new MCP server for the first time. After approving, you can verify the connection by asking in Agent mode:

> Can you call the OmniMem health tool?

You should see Valkey connection status, index counts, and embedding model status.

## Known quirks

- **128-tool hard cap**: VS Code enforces a limit of 128 tools across all active MCP servers per agent request. OmniMem has 30+ tools, which is fine on its own but be mindful if you have many other MCP servers connected. You can disable individual tools via the "Configure Tools" icon in the chat panel.
- **Agent mode only**: MCP tools do not work in standard Chat/Ask mode. You must select Agent mode.
- **No MCP resources**: Copilot only supports MCP tools, not MCP resources (the read-only data primitive). This does not affect OmniMem since it only exposes tools.
- **Server naming**: Server names in `mcp.json` should use camelCase with no whitespace or special characters.
- **Streamable HTTP**: VS Code also supports `"type": "http"` for Streamable HTTP. OmniMem defaults to SSE in 3.10 but will switch to Streamable HTTP in a future release. Set `MCP_TRANSPORT=http` and use `"type": "http"` with URL `.../mcp` to migrate early.
- **Claude Desktop auto-discovery**: If you also use Claude Desktop, VS Code can discover its MCP servers. Enable with `"chat.mcp.discovery.enabled": true` in VS Code settings.

## Notes

- If OmniMem is not responding, check that Docker containers are running: `docker compose ps`
- OmniMem has 30+ tools which is well under the 128-tool limit, but be mindful when combining with other MCP servers
