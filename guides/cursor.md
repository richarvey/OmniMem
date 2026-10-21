# Connecting OmniMem to Cursor

Cursor is the AI code editor built on VS Code. It supports Streamable HTTP MCP servers, so it connects straight to OmniMem's `/mcp` endpoint.

## Quick setup

Create or edit `~/.cursor/mcp.json` for global access:

```json
{
  "mcpServers": {
    "omnimem": {
      "url": "http://127.0.0.1:8765/mcp"
    }
  }
}
```

Using the desktop app? **Copy MCP URL** in the tray or menu bar menu gives you the address.

If OmniMem has an access token set (`MCP_AUTH_TOKEN`, or **Access token** on the Configuration page), send it as a header:

```json
{
  "mcpServers": {
    "omnimem": {
      "url": "http://127.0.0.1:8765/mcp",
      "headers": {
        "Authorization": "Bearer your-token-here"
      }
    }
  }
}
```

Or keep the token out of the file with environment variable interpolation:

```json
{
  "mcpServers": {
    "omnimem": {
      "url": "http://127.0.0.1:8765/mcp",
      "headers": {
        "Authorization": "Bearer ${env:OMNIMEM_TOKEN}"
      }
    }
  }
}
```

Cursor uses `${env:VAR_NAME}`, not `${VAR_NAME}`.

Coming from 6.x, just change `/sse` to `/mcp`.

## Global vs project config

| File | Scope | Use when |
|------|-------|----------|
| `~/.cursor/mcp.json` | All projects | OmniMem should be available everywhere (recommended) |
| `.cursor/mcp.json` (project root) | Single project | Only this project needs OmniMem |

Project config wins when the same server name is in both. Some Cursor versions have had a bug where the global file is silently ignored, so if the tools don't appear, try the project file.

## Enabling MCP tools

MCP tools only show up in Cursor's **Agent mode**, not plain chat. After adding the config:

1. Restart Cursor or reload the window
2. Open **Settings** (Cmd/Ctrl + ,) and search for **MCP**
3. Check `omnimem` is listed and enabled
4. Switch the chat panel to Agent mode

Cursor asks before each MCP tool call by default. There's a separate setting to auto-run MCP tools if the prompts drive you round the bend.

## Using OmniMem in Cursor

Once it's connected, just ask:

- "Call the OmniMem briefing tool for this project"
- "Remember that we decided to use SQLite for the store"
- "Check whether we've tried this approach before"

## Known quirks

- **The tool limit**: Cursor has capped how many MCP tools it sends to the model (40 across all servers in the versions I've used). OmniMem has 48, so some won't reach the model even on its own. Switch off the ones you don't use in Cursor's MCP settings and keep `briefing`, `recall`, `remember`, `record_experience`, `log_abandoned` and `warn_if_abandoned`.
- **Agent mode only**: no MCP tools in standard chat.
- **"No tools available"** while the server shows as connected: restart Cursor.
- **Remote-SSH**: MCP doesn't behave well over Remote-SSH, because the server config lives on one side and your files on the other.
- **CLI mode**: `cursor-agent` has had MCP transport bugs. The GUI agent is more reliable.

## Verifying the connection

In Agent chat, ask:

> Can you call the OmniMem health tool?

You should get back the record and vector counts per namespace, whether the embedding model is loaded, and the uptime.

## Notes

- If OmniMem isn't answering, `curl http://127.0.0.1:8765/healthz` should return `{"status": "ok"}`. The desktop app's tray status line tells you the same thing
- For a server on another machine, see [remote access](../docs/remote-access.md)
