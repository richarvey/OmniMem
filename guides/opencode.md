# Connecting OmniMem to OpenCode

OpenCode is an open source terminal coding agent. It supports remote MCP servers over Streamable HTTP, so it connects straight to OmniMem's `/mcp` endpoint.

## Quick setup

Create or edit `~/.config/opencode/opencode.json` for global access:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "omnimem": {
      "type": "remote",
      "url": "http://127.0.0.1:8765/mcp",
      "oauth": false,
      "timeout": 15000
    }
  }
}
```

OpenCode uses `"mcp"` as the top-level key (not `"mcpServers"`) and `"type": "remote"` for anything on a network.

On the desktop app, **Copy MCP URL** in the tray or menu bar menu gives you the address.

`"oauth": false` stops OpenCode trying to negotiate OAuth when it sees a 401. That's what you want with a plain access token. If you've switched on OmniMem's OAuth for a remote server and want OpenCode to sign in through the login page, leave it out and OpenCode will run the flow itself.

If OmniMem has an access token set (`MCP_AUTH_TOKEN`, or **Access token** on the Configuration page), add it as a header:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "omnimem": {
      "type": "remote",
      "url": "http://127.0.0.1:8765/mcp",
      "headers": {
        "Authorization": "Bearer {env:OMNIMEM_TOKEN}"
      },
      "oauth": false,
      "timeout": 15000
    }
  }
}
```

Set `OMNIMEM_TOKEN` in your shell. OpenCode uses `{env:VAR_NAME}`, single braces and no dollar.

Coming from 6.x, change `/sse` to `/mcp`.

## Config file locations

OpenCode merges config from all of these, later ones winning on conflicts:

| Priority | Location | Use when |
|----------|----------|----------|
| Lowest | `~/.config/opencode/opencode.json` | OmniMem should be available everywhere (recommended) |
| Higher | `opencode.json` in the project root | Only this project needs OmniMem |
| Highest | `OPENCODE_CONFIG_CONTENT` environment variable | Inline JSON for CI or testing |

## Using OmniMem with OpenCode

Once it's configured the tools are available to the model automatically:

- "Call the OmniMem briefing tool for this project"
- "Remember this decision in OmniMem"
- "Check whether we've tried this approach before"

## Verifying the connection

Start an OpenCode session and ask:

> Call the OmniMem health tool to check the connection

You should get back the record and vector counts per namespace, whether the embedding model is loaded, and the uptime.

## Known quirks

- **Connected but no tools**: there's been a bug where OpenCode shows a green "connected" status but registers nothing. Restart OpenCode and check the terminal output.
- **OAuth auto-negotiation**: OpenCode watches for 401 responses and starts OAuth. With a plain access token, keep `"oauth": false`.
- **The 5 second default timeout**: the first call after OmniMem starts can take longer while the embedding model loads. `"timeout": 15000` gives it room.
- **Context cost**: every tool description goes into the model's context on each request, and OmniMem has 48 tools. Set `"enabled": false` to switch OmniMem off for a session if you need the tokens back.

## Notes

- If OmniMem isn't answering, `curl http://127.0.0.1:8765/healthz` should return `{"status": "ok"}`, and the desktop app's tray status line says whether it's running
- OAuth tokens OpenCode obtains are stored in `~/.local/share/opencode/mcp-auth.json`
- Check the [OpenCode documentation](https://opencode.ai/docs/mcp-servers/) for the latest MCP options
