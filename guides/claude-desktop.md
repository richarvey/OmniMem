# Connecting OmniMem to Claude Desktop

Claude Desktop is Anthropic's desktop app for macOS and Windows. It talks to local MCP servers over stdio, so a server on a URL like OmniMem needs a small bridge, `mcp-remote`, to translate between the two. It's one line of config, honestly.

(Not to be confused with the OmniMem desktop app, which is the thing running your memory server. You can happily run both.)

## Prerequisites

- **OmniMem running**: the desktop app, or `omnimem serve` on a server
- **Claude Desktop** installed ([download](https://claude.ai/download))
- **Node.js and npm** installed, for `mcp-remote`

Install `mcp-remote` globally, or let `npx` fetch it on first run:

```bash
npm install -g mcp-remote
```

## Finding your Claude Desktop config file

Claude Desktop keeps its MCP servers in `claude_desktop_config.json`:

| OS | Path |
|----|------|
| macOS | `~/Library/Application Support/Claude/claude_desktop_config.json` |
| Windows | `%APPDATA%\Claude\claude_desktop_config.json` |

On macOS you can open it from a terminal:

```bash
open ~/Library/Application\ Support/Claude/claude_desktop_config.json
```

On Windows, press Win+R and paste:

```
%APPDATA%\Claude\claude_desktop_config.json
```

If the file doesn't exist yet, create it containing `{}` and restart Claude Desktop.

## Setup

**1. Grab the MCP URL.** In the OmniMem desktop app, click the tray or menu bar icon and choose **Copy MCP URL**. On a local install it's `http://127.0.0.1:8765/mcp`.

**2. Add OmniMem to your Claude Desktop config:**

```json
{
  "mcpServers": {
    "omnimem": {
      "command": "npx",
      "args": [
        "mcp-remote",
        "http://127.0.0.1:8765/mcp"
      ]
    }
  }
}
```

**3. Restart Claude Desktop.** OmniMem should show up as a connected server.

### With an access token

If you've set an access token (`MCP_AUTH_TOKEN`, or **Access token** on the OmniMem Configuration page):

```json
{
  "mcpServers": {
    "omnimem": {
      "command": "npx",
      "args": [
        "mcp-remote",
        "http://127.0.0.1:8765/mcp",
        "--header",
        "Authorization: Bearer your-token-here"
      ]
    }
  }
}
```

## Coming from 6.x

6.x served SSE at `/sse` and needed `--allow-http`. Neither applies any more: change the URL to `/mcp` and drop the flag.

## Why does Claude Desktop need mcp-remote?

Claude Desktop launches MCP servers as local processes and talks to them over standard input and output. It doesn't connect to an HTTP endpoint itself. `mcp-remote` sits in the middle: Claude Desktop starts it as a stdio process and it forwards everything to OmniMem over HTTP.

Claude Code doesn't have this problem, it connects to `/mcp` directly.

## Remote access

If OmniMem runs on another machine, point the bridge at that URL:

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

See [remote access](../docs/remote-access.md) for reverse proxies, tunnels and OAuth.

## Verifying the connection

After restarting Claude Desktop, start a new conversation and ask:

> Can you call the OmniMem health tool?

You should get back the record and vector counts per namespace, whether the embedding model is loaded, and how long the server has been up.

## Troubleshooting

- **"Server disconnected" or no tools appearing**: check OmniMem is actually running. The tray icon's status line says so on the desktop app; on a server, `systemctl status omnimem` or `curl http://127.0.0.1:8765/healthz` (which should answer `{"status": "ok"}`).
- **Config says `"type": "http"` and nothing loads**: Claude Desktop's config file doesn't take `"type": "http"`. Use `mcp-remote` as above. That format is for Claude Code.
- **Auth errors**: the token after `Bearer` has to match OmniMem's access token exactly. On the desktop app the token lives in your OS keychain and isn't shown again once saved, so if you've lost it, set a new one on the Configuration page and restart OmniMem.
- **`mcp-remote` not found**: run `npm install -g mcp-remote`, or keep using `npx`, which downloads it on first run.
