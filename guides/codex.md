# Connecting OmniMem to OpenAI Codex CLI

Codex CLI is OpenAI's open-source coding agent for the terminal. It uses TOML configuration and supports MCP servers via stdio and Streamable HTTP transports.

## Important: SSE compatibility

OmniMem currently serves a **legacy SSE endpoint** (`/sse`). Codex's remote transport support targets **Streamable HTTP**, which is a different protocol. Pointing Codex directly at `http://localhost:8765/sse` will not work reliably.

There are two options: use the **supergateway bridge** (works now) or wait for OmniMem to add a Streamable HTTP endpoint (future).

## Option 1: supergateway bridge (recommended)

[supergateway](https://github.com/supercorp-ai/supergateway) translates between SSE and stdio, which Codex supports natively. Codex launches and manages the bridge process automatically.

Create or edit `~/.codex/config.toml`:

```toml
[mcp_servers.omnimem]
command = "npx"
args = ["-y", "supergateway", "--sse", "http://localhost:8765/sse"]
```

If you have bearer token auth enabled (`MCP_AUTH_TOKEN` set in your `.env`):

```toml
[mcp_servers.omnimem]
command = "npx"
args = ["-y", "supergateway", "--sse", "http://localhost:8765/sse", "--header", "Authorization: Bearer your-token"]
```

You will need Node.js and npx installed for this to work.

## Option 2: direct Streamable HTTP (future)

When OmniMem adds a Streamable HTTP endpoint (e.g. `/mcp`), you will be able to connect directly:

```toml
[mcp_servers.omnimem]
url = "http://localhost:8765/mcp"
bearer_token_env_var = "OMNIMEM_TOKEN"
```

Set the token in your shell environment: `export OMNIMEM_TOKEN=your-token-here`

Note: `bearer_token_env_var` takes the **name** of the env var, not the token itself.

## Config file locations

| File | Scope | Use when |
|------|-------|----------|
| `~/.codex/config.toml` | All projects | OmniMem should be available everywhere (recommended) |
| `.codex/config.toml` (project root) | Single project | Only this project needs OmniMem (requires project trust) |

## Known quirks

- **TOML section name**: The section must be `[mcp_servers]` with an underscore. Using `[mcp-servers]` or `[mcpservers]` causes Codex to silently ignore the entire block.
- **Silent config failures**: TOML syntax errors are not reported -- Codex simply ignores the MCP configuration.
- **Streamable HTTP is not legacy SSE**: These are different protocols. `url =` in Codex config targets Streamable HTTP, not SSE. Do not point it at `/sse`.

## Verifying the connection

Start a Codex session and ask:

> Call the OmniMem health tool to check the connection

You should see Valkey connection status, index counts, and embedding model status.

## Notes

- Codex is open source and actively developed -- check the [Codex CLI repository](https://github.com/openai/codex) for the latest MCP configuration options
- If OmniMem is not responding, check that Docker containers are running: `docker compose ps`
