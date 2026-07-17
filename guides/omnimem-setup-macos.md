# Setting Up OmniMem on macOS

This guide covers installing OmniMem on macOS using Docker Desktop. OmniMem ships multi-arch images that run natively on both Apple Silicon (M1/M2/M3/M4) and Intel Macs — no Rosetta needed.

---

## Prerequisites

- **macOS 13 (Ventura) or later** — older versions work but Docker Desktop support is best on recent releases
- **Docker Desktop for Mac** — download from [docker.com](https://www.docker.com/products/docker-desktop/) or install via Homebrew
- **Git** — pre-installed on macOS or available via `xcode-select --install`

---

## Step 1 — Install Docker Desktop

If you don't have Docker Desktop:

**Option A — Homebrew (recommended):**

```bash
brew install --cask docker
```

**Option B — Direct download:**

Download from [docker.com/products/docker-desktop](https://www.docker.com/products/docker-desktop/) and drag to Applications.

Launch Docker Desktop from Applications and wait for the whale icon in the menu bar to show "Docker Desktop is running".

Verify:

```bash
docker --version
docker compose version
```

### Resource allocation

Docker Desktop defaults are fine for OmniMem. If you've reduced Docker's memory allocation below 4 GB, bump it back up — the embedding model needs room. Check Docker Desktop → Settings → Resources.

---

## Step 2 — Clone OmniMem

```bash
git clone https://codeberg.org/ric_harvey/omnimem.git
cd omnimem
```

---

## Step 3 — Configure the environment

```bash
cp .env.example .env
```

Open `.env` and set:

```bash
VALKEY_PASSWORD=pick-a-strong-password-here
```

For AI-powered RSS summaries and fact extraction:

```bash
ANTHROPIC_API_KEY=sk-ant-your-key-here
```

OmniMem works without the API key — RSS summaries fall back to truncation and ingest mode falls back to raw.

### Defaults that work on macOS

The default `.env` binds the MCP server and web UI to `127.0.0.1`, which is what you want for local development. If you're only connecting from the same Mac (Claude Code running locally), no changes to `docker-compose.yml` are needed.

---

## Step 4 — Build and start

### Option A — Use pre-built images from Docker Hub (recommended)

Pre-built multi-arch images (amd64 + arm64) are published to Docker Hub, so you can skip the build entirely. Edit `docker-compose.yml` and replace the `build:` directives with `image:` for the three application services:

```yaml
mcp_server:
  image: richarvey/omnimem-mcp:latest
  # build: ./mcp_server        ← comment out or remove

rss_worker:
  image: richarvey/omnimem-rss:latest
  # build: ./rss_worker

web_ui:
  image: richarvey/omnimem-web:latest
  # build:                      ← comment out or remove
  #   context: .
  #   dockerfile: web_ui/Dockerfile
```

The `valkey` service already uses an upstream image, so it needs no change.

Then start:

```bash
docker compose up -d
```

This pulls the images in under a minute rather than building from source.

To pin a specific release instead of `latest`, use the version tag (e.g. `richarvey/omnimem-mcp:v5.5.3`). See [releases on Codeberg](https://codeberg.org/ric_harvey/omnimem/releases) for available tags.

### Option B — Build from source

```bash
docker compose up -d
```

First build takes 3–5 minutes on Apple Silicon, a bit longer on Intel. Docker pulls base images, installs Python dependencies, and downloads the embedding model.

Watch the startup:

```bash
docker compose logs -f
```

All four services should report healthy:

- `valkey` — `Ready to accept connections`
- `mcp_server` — listening on port 8765
- `rss_worker` — scheduler started
- `web_ui` — serving on port 8080

Press `Ctrl+C` to stop tailing (containers keep running).

---

## Step 5 — Verify

Open the web dashboard in your browser:

```
http://localhost:8080
```

You should see the OmniMem management interface — browse memories, run searches, manage projects.

Check the MCP server:

```bash
curl http://localhost:8765/health
```

---

## Step 6 — Connect Claude Code

Since OmniMem is running locally, the connection config is straightforward.

Add to `~/.claude.json`:

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

If you set `MCP_AUTH_TOKEN`:

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

Auto-allow OmniMem tools in `~/.claude/settings.json`:

```json
{
  "permissions": {
    "allow": [
      "mcp__omnimem__*"
    ]
  }
}
```

### Using Streamable HTTP (recommended)

Set `MCP_TRANSPORT=http` in `.env`, restart, and use:

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "http",
      "url": "http://localhost:8765/mcp"
    }
  }
}
```

---

## Step 7 — Configure RSS feeds (optional)

Edit `rss_worker/feeds.yml`:

```yaml
feeds:
  - name: "Rust Blog"
    url: "https://blog.rust-lang.org/feed.xml"
    topics: ["rust", "programming"]
  - name: "Go Blog"
    url: "https://go.dev/blog/feed.atom"
    topics: ["go", "programming"]
```

Changes are picked up automatically.

---

## Running OmniMem as a background service

Docker Desktop starts automatically on login by default, and the `restart: unless-stopped` policy means OmniMem comes back up with it. If you've disabled Docker Desktop auto-start, OmniMem won't start until you open Docker Desktop.

To check status anytime:

```bash
docker compose ps
```

### Updating

If you're using Docker Hub images:

```bash
cd omnimem
docker compose pull
docker compose up -d
```

If you built from source:

```bash
cd omnimem
git pull
docker compose build
docker compose up -d
```

### Backups

```bash
# Via MCP: call dump_to_file() from a Claude Code session
# Via Docker volume:
docker run --rm -v omnimem_valkey_data:/data -v $(pwd)/backups:/backup \
  alpine tar czf /backup/valkey-backup-$(date +%Y%m%d).tar.gz -C /data .
```

---

## Accessing from other machines on your network

If you want to use OmniMem from a different computer (e.g. a work laptop connecting to your home Mac), edit `docker-compose.yml` to change the port bindings from `127.0.0.1` to `0.0.0.0`:

```yaml
ports:
  - "0.0.0.0:${MCP_PORT:-8765}:${MCP_PORT:-8765}"
```

Do this for both `mcp_server` and `web_ui`. Then set `MCP_AUTH_TOKEN` and `WEB_UI_AUTH_TOKEN` in `.env` to secure the endpoints.

Find your Mac's IP:

```bash
ipconfig getifaddr en0
```

Point your remote Claude Code config at `http://<mac-ip>:8765/sse`.

---

## Troubleshooting

**Docker Desktop not starting** — Make sure virtualisation is enabled. On Intel Macs this is in BIOS; on Apple Silicon it's always available. Try quitting and relaunching Docker Desktop.

**Port conflicts** — If port 8765 or 8080 is already in use, change `MCP_PORT` or `WEB_PORT` in `.env`.

**Slow first recall** — The embedding model loads lazily. First `recall()` takes a few seconds as the model loads into memory. Subsequent calls are fast.

**Build fails on Intel Mac** — Older Intel Macs with limited RAM may struggle during the build. Increase Docker Desktop's memory allocation to at least 4 GB in Settings → Resources.
