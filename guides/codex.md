# Connecting OmniMem to OpenAI Codex CLI

Codex CLI is OpenAI's open-source coding agent for the terminal. It uses TOML configuration and supports MCP servers via stdio and Streamable HTTP transports.

## Quick setup

Create or edit `~/.codex/config.toml`:

```toml
[mcp_servers.omnimem]
url = "http://localhost:8765/mcp"
```

If you have bearer token auth enabled (`MCP_AUTH_TOKEN` set in your `.env`):

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
- **Legacy SSE**: If you are running an older OmniMem version (pre-streamable-http), you can use the supergateway bridge: `args = ["-y", "supergateway", "--sse", "http://localhost:8765/sse"]` with `command = "npx"`. New installs do not need this.

## Verifying the connection

Start a Codex session and ask:

> Call the OmniMem health tool to check the connection

You should see Valkey connection status, index counts, and embedding model status.

## Notes

- Codex is open source and actively developed -- check the [Codex CLI repository](https://github.com/openai/codex) for the latest MCP configuration options
- If OmniMem is not responding, check that Docker containers are running: `docker compose ps`
