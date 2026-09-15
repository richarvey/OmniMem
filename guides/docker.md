# Running OmniMem in Docker

OmniMem 7 is one binary, so the Docker setup is one container. No Valkey, no web UI container, no RSS worker, no Compose file with four services and a health check dance. One image, one volume, one port.

The image is `richarvey/omnimem`, built for amd64 and arm64, and it runs `omnimem serve`: the MCP server, the RSS scheduler and the background enrichment worker, all in the one process. It's the headless build, so there's no settings window. You configure it with environment variables.

> [!NOTE]
> Coming with 7.0.0. Until the first release you can build from source (see [Build from source](../docs/quick-start.md#build-from-source)).

## Quick start

```bash
docker run -d --name omnimem \
  --restart unless-stopped \
  -p 127.0.0.1:8765:8765 \
  -v omnimem-data:/data \
  -e OMNIMEM_DB=/data/omnimem.db \
  -e HF_HOME=/data/hf-cache \
  -e MCP_HOST=0.0.0.0 \
  -e MCP_AUTH_TOKEN="$(openssl rand -hex 32)" \
  richarvey/omnimem:7.0.0
```

A few things are going on there:

- **`/data`** holds everything that matters: the SQLite database, `feeds.yml`, and the `backups` folder, which all live beside the database.
- **`HF_HOME=/data/hf-cache`** keeps the embedding model on the volume. The first start downloads it from Hugging Face; after that it's read from the cache and nothing touches the network.
- **`MCP_HOST=0.0.0.0`** is needed inside a container, because the port has to be reachable from outside it. OmniMem refuses to listen beyond localhost without authentication, which is why the token is there. Take it out and the container exits with an error telling you so.
- **`-p 127.0.0.1:8765:8765`** publishes the port on the host's loopback only. Drop the `127.0.0.1:` part if other machines should reach it directly.

Grab the token you generated, you'll need it for your clients:

```bash
docker exec omnimem printenv MCP_AUTH_TOKEN
```

Check it's up:

```bash
curl http://127.0.0.1:8765/healthz
```

That should answer `{"status": "ok"}`, no token needed.

## With Compose

If you'd rather keep it in a file:

```yaml
services:
  omnimem:
    image: richarvey/omnimem:7.0.0
    restart: unless-stopped
    ports:
      - "127.0.0.1:8765:8765"
    volumes:
      - omnimem-data:/data
    env_file:
      - omnimem.env

volumes:
  omnimem-data:
```

And `omnimem.env` beside it:

```bash
OMNIMEM_DB=/data/omnimem.db
HF_HOME=/data/hf-cache
MCP_HOST=0.0.0.0
MCP_AUTH_TOKEN=paste-a-long-random-token-here
# ANTHROPIC_API_KEY=sk-ant-...   # fact extraction, query expansion, contradiction checks, RSS summaries
```

Then `docker compose up -d`. Generate the token with `openssl rand -hex 32`.

Without `ANTHROPIC_API_KEY` everything still works: RSS summaries fall back to truncation and the Claude-powered extras switch off. The [configuration reference](../docs/configuration.md) has every other setting.

## Connecting your agent

Point your client at `http://127.0.0.1:8765/mcp` and send the token as a bearer header. For Claude Code:

```bash
claude mcp add --transport http omnimem http://127.0.0.1:8765/mcp \
  --header "Authorization: Bearer your-token-here" \
  --scope user
```

The [connection guides](README.md) cover the other agents.

## RSS feeds

Put your reading list in `feeds.yml` on the volume (at `/data/feeds.yml`, beside the database). OmniMem checks it when it starts, every `RSS_SCHEDULE_HOURS` (6 by default), and whenever the file changes:

```yaml
feeds:
  - name: "Rust Blog"
    url: "https://blog.rust-lang.org/feed.xml"
    topics: ["rust", "programming"]
```

To edit it from the host, bind mount a folder instead of using a named volume, or copy it in with `docker cp`. See [RSS and knowledge](../docs/rss-knowledge.md) for the full format.

## Behind a reverse proxy, or with OAuth

For claude.ai and other OAuth clients, add the OAuth settings and put a TLS proxy in front:

```bash
OAUTH_ENABLED=true
OAUTH_BASE_URL=https://mcp.yourdomain.com
OAUTH_ADMIN_USER=admin
OAUTH_ADMIN_PASSWORD=a-strong-password-here
```

OmniMem only serves `/mcp`, the OAuth routes and `/healthz`, so the whole thing proxies to port 8765. See [remote access](../docs/remote-access.md) and [claude.ai](claude-ai.md).

## Pinning versions

Pin a release tag rather than `latest`:

```yaml
image: richarvey/omnimem:7.0.0
```

Releases are listed on the [Forgejo releases page](https://code.squarecows.com/ric/omnimem/releases).

## Updating

```bash
docker compose pull
docker compose up -d
```

The database migrates itself on start. It's still worth taking a backup first (below), because I'm careful, not reckless.

## Backups

Everything is in one SQLite file, but don't copy it while OmniMem is writing. Take a proper backup instead:

- ask your agent to call `dump_to_file`, which writes a JSON backup into `/data/backups`, or
- run the CLI inside the container:

```bash
docker exec omnimem omnimem export /data/backups/omnimem-$(date +%Y%m%d).json
```

Restore with the `restore_from_file` tool, or `omnimem import`.

## Coming from the 6.x containers

6.x ran four containers with Valkey underneath. To move across:

1. On 6.x, call `dump_to_file` (or use the web UI's Backups page)
2. Copy the JSON file onto the new volume, for example `docker cp backup.json omnimem:/data/backups/`
3. Import it:

```bash
docker exec omnimem omnimem import /data/backups/backup.json
```

Backups don't carry vectors, so the import re-embeds every memory, at roughly 8 ms each. A few thousand memories take under a minute; 25,000 take about three and a half.

The old `VALKEY_*`, `WEB_*` and `MCP_TRANSPORT` settings are gone, and clients that used `/sse` need to switch to `/mcp`.

## Troubleshooting

**The container exits straight away**: `docker logs omnimem`. The usual cause is `MCP_HOST=0.0.0.0` with no `MCP_AUTH_TOKEN` and no OAuth, which OmniMem refuses on purpose. The other is OAuth switched on with a setting missing; the log names it.

**The first start is slow**: it's downloading the embedding model. Subsequent starts read it from `HF_HOME`.

**Clients get 401**: the token after `Bearer` has to match `MCP_AUTH_TOKEN` exactly.

**Clients get 421 through a proxy**: the Host header isn't trusted. Set `OAUTH_BASE_URL` or `MCP_PUBLIC_URL` to the public address, or add names to `MCP_ALLOWED_HOSTS`.
