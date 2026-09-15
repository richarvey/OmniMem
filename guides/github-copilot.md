# Connecting OmniMem to GitHub Copilot

GitHub Copilot in VS Code has supported MCP servers natively since VS Code 1.99. It speaks Streamable HTTP with custom headers, which is all OmniMem needs.

## Requirements

- **VS Code 1.99 or later**
- **Copilot Free, Pro or Pro+**: MCP works out of the box
- **Copilot Business or Enterprise**: your organisation admin has to enable the "MCP servers in Copilot" policy

## Quick setup

Create or edit `.vscode/mcp.json` in your project root:

```json
{
  "servers": {
    "omnimem": {
      "type": "http",
      "url": "http://127.0.0.1:8765/mcp"
    }
  }
}
```

Copilot uses `"servers"` as the top-level key, not `"mcpServers"`. Easy to miss.

If you're running the desktop app, **Copy MCP URL** in the tray or menu bar menu gives you the address.

If OmniMem has an access token set (`MCP_AUTH_TOKEN`, or **Access token** on the Configuration page), use an input variable so VS Code prompts for it and stores it securely:

```json
{
  "servers": {
    "omnimem": {
      "type": "http",
      "url": "http://127.0.0.1:8765/mcp",
      "headers": {
        "Authorization": "Bearer ${input:omnimem_token}"
      }
    }
  },
  "inputs": [
    {
      "type": "promptString",
      "id": "omnimem_token",
      "description": "OmniMem access token",
      "password": true
    }
  ]
}
```

VS Code asks once and remembers. You can also reference an environment variable directly with `${env:OMNIMEM_TOKEN}`.

Coming from 6.x, change `"type": "sse"` to `"type": "http"` and `/sse` to `/mcp`.

## Global vs project config

| Scope | How to get there | Use when |
|-------|---------------|----------|
| Workspace | `.vscode/mcp.json` in the project root | Share the config with your team through version control |
| User (global) | Command palette: `MCP: Open User Configuration` | OmniMem should be available in every project |

Don't configure the same server in both, it causes conflicts.

## Agent mode is required

MCP tools are only available in Copilot's **Agent mode**, not Ask or Chat. To use OmniMem:

1. Open Copilot Chat (Ctrl/Cmd + Shift + I)
2. Pick **Agent** from the mode dropdown
3. The OmniMem tools are there

In Agent mode Copilot can call OmniMem on its own as part of a multi-step task.

## Using OmniMem with Copilot

- "Call the OmniMem briefing tool for this project"
- "Remember this architectural decision in OmniMem"
- "Check OmniMem for anything we've already solved here"

## Verifying the connection

VS Code asks you to trust a new MCP server the first time it starts it. Approve it, then ask in Agent mode:

> Can you call the OmniMem health tool?

You should get back the record and vector counts per namespace, whether the embedding model is loaded, and the uptime.

## Known quirks

- **The 128-tool cap**: VS Code allows 128 tools across all active MCP servers per request. OmniMem's 48 fit comfortably, but it adds up if you've got a lot of servers. Use the "Configure Tools" icon in the chat panel to switch individual tools off.
- **Agent mode only**: MCP tools don't work in Chat or Ask mode.
- **No MCP resources**: Copilot only supports tools. That doesn't matter here, OmniMem only exposes tools.
- **Server naming**: keep server names in `mcp.json` free of whitespace and special characters.
- **Claude Desktop discovery**: VS Code can pick up MCP servers from Claude Desktop. Turn it on with `"chat.mcp.discovery.enabled": true` in VS Code settings.

## Notes

- If OmniMem isn't answering, `curl http://127.0.0.1:8765/healthz` should return `{"status": "ok"}`, and the desktop app's tray status line says whether it's running
- For a server on another machine, see [remote access](../docs/remote-access.md)
