# Connecting OmniMem to Cursor

Cursor is an AI-powered code editor built on VS Code. It supports MCP servers via SSE and Streamable HTTP transports.

> [!WARNING]
> **SSE transport is deprecated.** OmniMem 3.10 defaults to SSE but will switch to Streamable HTTP in a future release. To migrate early, set `MCP_TRANSPORT=http` in your `.env` and use URL `.../mcp` in the config below.

## Quick setup

Create or edit `~/.cursor/mcp.json` for global access:

```json
{
  "mcpServers": {
    "omnimem": {
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
      "url": "http://localhost:8765/sse",
      "headers": {
        "Authorization": "Bearer your-token-here"
      }
    }
  }
}
```

You can use environment variable interpolation to avoid hardcoding the token:

```json
{
  "mcpServers": {
    "omnimem": {
      "url": "http://localhost:8765/sse",
      "headers": {
        "Authorization": "Bearer ${env:OMNIMEM_TOKEN}"
      }
    }
  }
}
```

Note: Cursor uses `${env:VAR_NAME}` syntax (not `${VAR_NAME}`).

## Global vs project config

| File | Scope | Use when |
|------|-------|----------|
| `~/.cursor/mcp.json` | All projects | OmniMem should be available everywhere (recommended) |
| `.cursor/mcp.json` (project root) | Single project | Only this project needs OmniMem |

Project config takes precedence over global when the same server name exists in both. There is a known bug in some Cursor versions where the global config is silently ignored -- if tools do not appear, try the project-level config instead.

## Enabling MCP tools

MCP tools are only available in Cursor's **Agent mode** (not standard chat). After adding the config:

1. Restart Cursor or reload the window
2. Open **Settings** (Cmd/Ctrl + ,) and search for **MCP**
3. Verify `omnimem` appears and is enabled
4. Switch to Agent mode in the chat panel

By default, Cursor asks for approval before each MCP tool call. There is a separate "auto-run MCP tools" toggle in settings if you want to skip approval prompts.

## Using OmniMem in Cursor

Once connected, you can ask Cursor's agent to use OmniMem tools directly:

- "Call the OmniMem briefing tool for this project"
- "Remember that we decided to use Valkey over Redis"
- "Check if we've tried this approach before"

## Known quirks

- **40-tool hard limit**: Cursor sends a maximum of 40 MCP tools to the LLM across all connected servers. OmniMem has 30+ tools, so if you have other MCP servers connected, some tools may be silently inaccessible.
- **Agent mode only**: MCP tools do not appear in standard chat mode -- you must use Agent mode.
- **Connection issues**: If you see "no tools available" despite the server showing as connected, try restarting Cursor. If problems persist, a reverse proxy on a standard port (80/443) may help.
- **SSH remote development**: MCP does not work reliably over Remote-SSH. The MCP server runs locally but Cursor edits files remotely, creating a disconnect.
- **CLI mode**: Cursor's CLI/headless mode (`cursor-agent`) has a known bug with some MCP transports. The GUI agent is more reliable.

## Verifying the connection

In Cursor's Agent chat, ask:

> Can you call the OmniMem health tool?

If the server is connected, you will see Valkey connection status, index counts, and model status.

## Notes

- If OmniMem is not responding, check that Docker containers are running: `docker compose ps`
- OmniMem has 30+ tools which is under the 40-tool limit, but be mindful if adding other MCP servers
