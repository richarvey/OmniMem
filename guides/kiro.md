# Connecting OmniMem to Kiro

Kiro is AWS's AI IDE, built on VS Code, and it plans before it codes. It supports Streamable HTTP MCP servers, and its spec-driven workflow pairs rather nicely with OmniMem's project context.

## Quick setup

Create or edit `~/.kiro/settings/mcp.json` for global access:

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "http",
      "url": "http://127.0.0.1:8765/mcp"
    }
  }
}
```

On the desktop app, **Copy MCP URL** in the tray or menu bar menu gives you the address.

If OmniMem has an access token set (`MCP_AUTH_TOKEN`, or **Access token** on the Configuration page), add it as a header:

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "http",
      "url": "http://127.0.0.1:8765/mcp",
      "headers": {
        "Authorization": "Bearer ${OMNIMEM_TOKEN}"
      }
    }
  }
}
```

Set `OMNIMEM_TOKEN` in your shell environment (`.zshrc`, `.bashrc`) so Kiro can expand it. The IDE uses `${VAR_NAME}` syntax.

Coming from 6.x, change `"type": "sse"` to `"type": "http"` and `/sse` to `/mcp`.

## Global vs project config

| File | Scope | Use when |
|------|-------|----------|
| `~/.kiro/settings/mcp.json` | All projects | OmniMem should be available everywhere (recommended) |
| `.kiro/settings/mcp.json` (project root) | Single project | Only this project needs OmniMem |

## Kiro's MCP panel

You can also add the server through Kiro's UI:

1. Open the **MCP Servers** panel from the sidebar or command palette
2. Click **Add Server**
3. Choose **HTTP** as the transport
4. Enter the URL: `http://127.0.0.1:8765/mcp`
5. Add the authorisation header if you've set a token

## Using OmniMem with Kiro

Once connected, the tools are available to Kiro's agent:

- "Call the OmniMem briefing tool for this project"
- "Remember this architectural decision in OmniMem"
- "Check whether we've tried this approach before"

The agent can load project state at the start of each session and store the decisions as your specs evolve, so the reasoning behind a design doesn't evaporate when you close the window.

## Verifying the connection

In Kiro's chat panel, ask:

> Can you call the OmniMem health tool?

You should get back the record and vector counts per namespace, whether the embedding model is loaded, and the uptime.

## Known quirks

- **Silent failures**: if OmniMem can't be reached, Kiro can fail to load every MCP server without saying anything. If your tools suddenly disappear, check OmniMem is running first.
- **CLI vs IDE variable syntax**: the IDE uses `${VAR_NAME}` but the Kiro CLI expects `${env:VAR_NAME}`, so one `mcp.json` can't serve both without editing.
- **OAuth redirects**: Kiro hasn't been able to complete localhost OAuth redirects in the versions I've tried. Stick with an access token in `headers`, which works fine. OAuth is for clients like claude.ai that connect from elsewhere.

## Notes

- If OmniMem isn't answering, `curl http://127.0.0.1:8765/healthz` should return `{"status": "ok"}`, and the desktop app's tray status line says whether it's running
- Check the [Kiro documentation](https://kiro.dev/docs/mcp/configuration/) for the latest MCP options
