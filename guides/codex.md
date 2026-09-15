# Connecting OmniMem to OpenAI Codex CLI

Codex CLI is OpenAI's open source coding agent for the terminal. It's configured in TOML and speaks Streamable HTTP natively, which is exactly what OmniMem 7 serves, so it's a two-line job.

## Quick setup

Create or edit `~/.codex/config.toml`:

```toml
[mcp_servers.omnimem]
url = "http://127.0.0.1:8765/mcp"
```

On the desktop app, **Copy MCP URL** in the tray or menu bar menu puts that address on your clipboard.

If OmniMem has an access token set (`MCP_AUTH_TOKEN`, or **Access token** on the Configuration page):

```toml
[mcp_servers.omnimem]
url = "http://127.0.0.1:8765/mcp"
bearer_token_env_var = "OMNIMEM_TOKEN"
```

Then set the token in your shell: `export OMNIMEM_TOKEN=your-token-here`

Note that `bearer_token_env_var` takes the **name** of the variable, not the token itself. I've tripped over that one.

## Config file locations

| File | Scope | Use when |
|------|-------|----------|
| `~/.codex/config.toml` | All projects | OmniMem should be available everywhere (recommended) |
| `.codex/config.toml` (project root) | Single project | Only this project needs OmniMem (requires project trust) |

## Known quirks

- **The section name matters**: it must be `[mcp_servers]` with an underscore. `[mcp-servers]` or `[mcpservers]` and Codex quietly ignores the whole block.
- **Silent config failures**: a TOML syntax error isn't reported, Codex just carries on without the server. If the tools vanish, check your TOML first.
- **Coming from 6.x**: the old supergateway bridge to `/sse` isn't needed any more. Point Codex straight at `/mcp`.

## Verifying the connection

Start a Codex session and ask:

> Call the OmniMem health tool to check the connection

You should get back the record and vector counts per namespace, whether the embedding model is loaded, and the uptime.

## Notes

- Codex moves quickly, so check the [Codex CLI repository](https://github.com/openai/codex) for the latest MCP options
- If OmniMem isn't answering, `curl http://127.0.0.1:8765/healthz` should come back with `{"status": "ok"}`. On the desktop app the tray icon's status line tells you too
