# Running OmniMem with Docker Hub images

Pre-built Docker images are available on Docker Hub. This is the quickest way to get OmniMem running without building from source.

## Images

| Image | Service | Description |
|-------|---------|-------------|
| `richarvey/omnimem-mcp` | MCP server | FastMCP SSE (default) or Streamable HTTP transport on port 8765 |
| `richarvey/omnimem-web` | Web UI | Starlette dashboard on port 8080 |
| `richarvey/omnimem-rss` | RSS worker | Background feed ingestion via Claude Haiku |

All images are built from `python:3.12-slim` with CPU-only PyTorch.

## Quick start

### 1. Create your project directory

```bash
mkdir omnimem && cd omnimem
```

### 2. Create your `.env` file

```bash
cat > .env << 'EOF'
VALKEY_PASSWORD=change-me-to-something-secure
ANTHROPIC_API_KEY=
MCP_PORT=8765
MCP_HOST=0.0.0.0
# MCP_TRANSPORT=http          # uncomment to switch to Streamable HTTP (SSE is default)
WEB_PORT=8080
# MCP_AUTH_TOKEN=
# WEB_UI_AUTH_TOKEN=
EOF
```

Set `VALKEY_PASSWORD` to a secure value. `ANTHROPIC_API_KEY` is optional -- without it, RSS summaries fall back to truncation and contradiction checks use embedding similarity only.

To enable authentication, uncomment and set `MCP_AUTH_TOKEN` and/or `WEB_UI_AUTH_TOKEN`.

### 3. Create your `docker-compose.yml`

```yaml
services:
  valkey:
    image: valkey/valkey-extension:latest
    command: >
      valkey-server
      --loadmodule /usr/lib/valkey/libsearch.so
      --requirepass ${VALKEY_PASSWORD}
    volumes:
      - valkey_data:/data
    environment:
      - VALKEY_PASSWORD=${VALKEY_PASSWORD}
    healthcheck:
      test: ["CMD-SHELL", "valkey-cli -a \"$$VALKEY_PASSWORD\" ping | grep -q PONG"]
      interval: 10s
      timeout: 5s
      retries: 5
    restart: unless-stopped

  mcp_server:
    image: richarvey/omnimem-mcp:latest
    depends_on:
      valkey:
        condition: service_healthy
    ports:
      - "127.0.0.1:${MCP_PORT:-8765}:${MCP_PORT:-8765}"
    volumes:
      - ./backups:/app/backups
    env_file:
      - .env
    environment:
      - MCP_HOST=0.0.0.0
    restart: unless-stopped

  rss_worker:
    image: richarvey/omnimem-rss:latest
    depends_on:
      valkey:
        condition: service_healthy
    volumes:
      - ./feeds.yml:/app/feeds.yml
    env_file:
      - .env
    restart: unless-stopped

  web_ui:
    image: richarvey/omnimem-web:latest
    depends_on:
      valkey:
        condition: service_healthy
    ports:
      - "127.0.0.1:${WEB_PORT:-8080}:8080"
    volumes:
      - ./backups:/app/backups
      - ./feeds.yml:/app/feeds.yml
    env_file:
      - .env
    restart: unless-stopped

volumes:
  valkey_data:
```

### 4. Create a feeds.yml (optional)

```yaml
feeds:
  - url: https://blog.rust-lang.org/feed.xml
    name: Rust Official Blog
    topics: [rust, systems, language]
```

If you do not need RSS ingestion, create an empty file: `touch feeds.yml`

### 5. Start the services

```bash
docker compose up -d
```

Four containers will start:
- **Valkey** with vector search module
- **MCP server** on `http://localhost:8765/sse` (SSE default; set `MCP_TRANSPORT=http` for Streamable HTTP on `/mcp`)
- **Web UI** on `http://localhost:8080`
- **RSS worker** running in the background

### 6. Connect your coding agent

See the [connection guides](../guides/) for your specific tool. For Claude Code, add to `~/.claude.json`:

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "sse",
      "url": "http://localhost:8765/sse"
    }
  }
}
```

> [!WARNING]
> **SSE transport is deprecated.** Switch to Streamable HTTP by setting `MCP_TRANSPORT=http` in your `.env` and using `"type": "http"` with URL `http://localhost:8765/mcp` in your client config. SSE will be removed in a future release.

If you set `MCP_AUTH_TOKEN`, add the header:

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "sse",
      "url": "http://localhost:8765/sse",
      "headers": {
        "Authorization": "Bearer your-token-here"
      }
    }
  }
}
```

## Pinning versions

To pin to a specific release instead of `latest`:

```yaml
mcp_server:
  image: richarvey/omnimem-mcp:v3.8.0
```

This is recommended for production deployments.

## Updating

```bash
docker compose pull
docker compose up -d
```

This pulls the latest images and recreates any containers that have changed. Valkey data persists in the `valkey_data` volume.

## Backups

OmniMem stores all data in Valkey. To back up:

1. Ask your coding agent to call the `dump_to_file()` tool, or
2. Use the web UI at `http://localhost:8080` (Backups page)

Backups are written to the `./backups` directory which is mounted into both the MCP server and web UI containers.

## Troubleshooting

**MCP server not responding**: Check the container is running with `docker compose ps` and view logs with `docker compose logs mcp_server`.

**Web UI shows no data**: The web UI connects directly to Valkey. Check that Valkey is healthy: `docker compose logs valkey`.

**RSS articles not appearing**: Check that `feeds.yml` exists and is valid YAML. View worker logs: `docker compose logs rss_worker`.

**Authentication issues**: If you set `MCP_AUTH_TOKEN`, make sure your coding agent config includes the matching `Authorization: Bearer` header.
