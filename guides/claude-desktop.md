# Connecting OmniMem to Claude Desktop

Claude Desktop is Anthropic's desktop app for macOS and Windows. It connects to MCP servers via stdio, so remote servers (SSE or Streamable HTTP) need the `mcp-remote` bridge to translate between the two.

> [!NOTE]
> **OmniMem 3.10 introduces Streamable HTTP transport**, which replaces the older SSE transport. Both work with Claude Desktop via `mcp-remote`, but Streamable HTTP is the recommended setup. SSE is deprecated and will be removed in a future release.

## Prerequisites

- **OmniMem running** via Docker Compose (`docker compose up -d`)
- **Claude Desktop** installed ([download](https://claude.ai/download))
- **Node.js and npm** installed (needed for `mcp-remote`)

Install `mcp-remote` globally:

```bash
npm install -g mcp-remote
```

## Finding your Claude Desktop config file

Claude Desktop stores its MCP server configuration in `claude_desktop_config.json`. The location depends on your operating system:

| OS | Path |
|----|------|
| macOS | `~/Library/Application Support/Claude/claude_desktop_config.json` |
| Windows | `%APPDATA%\Claude\claude_desktop_config.json` |

On macOS you can open the file directly from a terminal:

```bash
open ~/Library/Application\ Support/Claude/claude_desktop_config.json
```

On Windows, press Win+R and paste:

```
%APPDATA%\Claude\claude_desktop_config.json
```

If the file does not exist yet, create it with an empty JSON object `{}` and restart Claude Desktop.

## Streamable HTTP setup (recommended)

**1. Set the transport in your `.env`:**

```bash
MCP_TRANSPORT=http
```

Then restart the MCP server: `docker compose restart mcp_server`

**2. Add OmniMem to your Claude Desktop config:**

```json
{
  "mcpServers": {
    "omnimem": {
      "command": "npx",
      "args": [
        "mcp-remote",
        "http://localhost:8765/mcp"
      ]
    }
  }
}
```

**3. Restart Claude Desktop.** OmniMem should appear as a connected MCP server.

### With bearer token auth

If you have `MCP_AUTH_TOKEN` set in your `.env`:

```json
{
  "mcpServers": {
    "omnimem": {
      "command": "npx",
      "args": [
        "mcp-remote",
        "http://localhost:8765/mcp",
        "--header",
        "Authorization: Bearer your-token-here"
      ]
    }
  }
}
```

## SSE setup (legacy)

If you are running OmniMem 3.9 or earlier (or have not set `MCP_TRANSPORT=http`), the server uses SSE transport.

```json
{
  "mcpServers": {
    "omnimem": {
      "command": "npx",
      "args": [
        "mcp-remote",
        "http://localhost:8765/sse",
        "--allow-http"
      ]
    }
  }
}
```

The `--allow-http` flag is needed for SSE connections over plain HTTP. Without it, `mcp-remote` will reject the connection.

### SSE with bearer token auth

```json
{
  "mcpServers": {
    "omnimem": {
      "command": "npx",
      "args": [
        "mcp-remote",
        "http://localhost:8765/sse",
        "--allow-http",
        "--header",
        "Authorization: Bearer your-token-here"
      ]
    }
  }
}
```

## Migrating from SSE to Streamable HTTP

If you are upgrading from the SSE setup:

1. Set `MCP_TRANSPORT=http` in your `.env`
2. Restart the MCP server: `docker compose restart mcp_server`
3. Update your Claude Desktop config — change the URL from `/sse` to `/mcp` and remove the `--allow-http` flag (see Streamable HTTP setup above)
4. Restart Claude Desktop

## Why does Claude Desktop need mcp-remote?

Claude Desktop communicates with MCP servers over stdio (standard input/output). It does not connect to HTTP endpoints directly. The `mcp-remote` package acts as a bridge: Claude Desktop launches it as a stdio process, and `mcp-remote` forwards requests to your HTTP server.

This is different from Claude Code, which connects to SSE and Streamable HTTP endpoints natively without any bridge.

## Remote access

If OmniMem runs on a different machine, update the URL to point to your server:

```json
{
  "mcpServers": {
    "omnimem": {
      "command": "npx",
      "args": [
        "mcp-remote",
        "https://omnimem.yourdomain.com/mcp",
        "--header",
        "Authorization: Bearer your-token-here"
      ]
    }
  }
}
```

See `docs/reverse-proxy.md` for Traefik and Caddy configuration examples.

## Verifying the connection

After restarting Claude Desktop, start a new conversation and ask:

> Can you call the OmniMem health tool?

You should see Valkey connection status, index counts, and embedding model status returned by the server.

## Troubleshooting

- **"Server disconnected" or no tools appearing**: Check that Docker containers are running with `docker compose ps` and that the MCP server logs show no errors: `docker compose logs mcp_server`
- **Config shows `"type": "http"` — tools not loading**: Claude Desktop does not support `"type": "http"` directly. You must use `mcp-remote` as shown above. The `"type": "http"` format only works in Claude Code.
- **"Connection refused" on localhost**: Make sure `MCP_HOST` in your `.env` is set to `0.0.0.0` (not `127.0.0.1`) if the MCP server runs inside Docker but Claude Desktop runs on the host.
- **Auth errors**: Verify that the token in your `mcp-remote` args matches `MCP_AUTH_TOKEN` in your `.env` exactly.
- **`mcp-remote` not found**: Run `npm install -g mcp-remote` to install it, or use `npx` which will download it on first run.
