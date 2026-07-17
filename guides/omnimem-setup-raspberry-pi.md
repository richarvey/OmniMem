# Setting Up OmniMem on a Raspberry Pi

This guide walks you through installing OmniMem on a Raspberry Pi. OmniMem ships multi-arch Docker images (amd64 and arm64), so it runs natively on any Pi with a 64-bit OS — no emulation, no cross-compilation.

---

## Prerequisites

- **Raspberry Pi 4 (4 GB+) or Raspberry Pi 5** — a Pi 3 will work but expect slower embedding generation on first boot
- **Raspberry Pi OS (64-bit)** — Bookworm or later; the 32-bit OS will not work because the sentence-transformers model requires arm64
- **At least 16 GB SD card** (32 GB+ recommended) — the embedding model, container images, and Valkey data need room to breathe
- **Docker and Docker Compose** installed
- **Git** installed

> **Heads up on memory:** The sentence-transformers model (`all-MiniLM-L6-v2`) loads into RAM on first start. On a 4 GB Pi, this works fine but leaves limited headroom for other services. If you're running other containers on the same Pi, consider the 8 GB model.

---

## Step 1 — Install Docker

If you don't already have Docker installed:

```bash
curl -fsSL https://get.docker.com | sh
sudo usermod -aG docker $USER
```

Log out and back in (or run `newgrp docker`) so your user picks up the `docker` group.

Verify it's working:

```bash
docker --version
docker compose version
```

You need Docker Compose V2 (the `docker compose` plugin, not the old `docker-compose` binary). The install script above includes it.

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

Open `.env` in your editor and set at minimum:

```bash
VALKEY_PASSWORD=pick-a-strong-password-here
```

If you want AI-powered RSS summaries and richer fact extraction, also set:

```bash
ANTHROPIC_API_KEY=sk-ant-your-key-here
```

If you leave `ANTHROPIC_API_KEY` unset or blank, OmniMem still works — RSS summaries fall back to simple truncation and the ingest mode falls back to `raw` (verbatim storage).

### Optional but recommended for Pi

The Pi's limited RAM means you may want to constrain parallelism. The defaults are already conservative, but if you're on a 4 GB Pi you can add:

```bash
VALKEY_MAX_CONNECTIONS=10
```

### Binding to your local network

By default, the MCP server and web UI only listen on `127.0.0.1` (localhost). If you want to access OmniMem from other machines on your network (e.g. your laptop pointing at the Pi), you need to update the port mappings.

Edit `docker-compose.yml` and change the `ports` entries from:

```yaml
ports:
  - "127.0.0.1:${MCP_PORT:-8765}:${MCP_PORT:-8765}"
```

to:

```yaml
ports:
  - "0.0.0.0:${MCP_PORT:-8765}:${MCP_PORT:-8765}"
```

Do the same for the `web_ui` service:

```yaml
ports:
  - "0.0.0.0:${WEB_PORT:-8080}:8080"
```

**Important:** if you expose OmniMem beyond localhost, set `MCP_AUTH_TOKEN` and `WEB_UI_AUTH_TOKEN` in your `.env` to protect both endpoints with bearer token authentication.

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

The first build takes a while on a Pi — expect 10–20 minutes as it downloads base images, installs Python dependencies, and downloads the embedding model. Subsequent starts are fast because everything is cached.

Watch the logs to make sure all four containers come up healthy:

```bash
docker compose logs -f
```

You're looking for:

- `valkey` — `Ready to accept connections`
- `mcp_server` — listening on the configured port
- `rss_worker` — scheduler started
- `web_ui` — serving on port 8080

Press `Ctrl+C` to stop following logs (the containers keep running).

---

## Step 5 — Verify it's running

Check the web dashboard:

```bash
curl http://localhost:8080
```

Or open `http://<your-pi-ip>:8080` in a browser on another machine (if you changed the bind address in step 3).

Check the MCP server health:

```bash
curl http://localhost:8765/health
```

---

## Step 6 — Connect your AI tool

Add OmniMem to your Claude Code config (`~/.claude.json` on the machine where you run Claude Code):

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "sse",
      "url": "http://<your-pi-ip>:8765/sse"
    }
  }
}
```

If you set `MCP_AUTH_TOKEN` in your `.env`, add the token:

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "sse",
      "url": "http://<your-pi-ip>:8765/sse",
      "headers": {
        "Authorization": "Bearer your-token-here"
      }
    }
  }
}
```

To skip permission prompts for OmniMem tools, add to `~/.claude/settings.json`:

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

If you prefer the newer Streamable HTTP transport instead of SSE, set `MCP_TRANSPORT=http` in your `.env`, restart the containers, and use this config:

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "http",
      "url": "http://<your-pi-ip>:8765/mcp"
    }
  }
}
```

---

## Step 7 — Configure RSS feeds (optional)

Edit `rss_worker/feeds.yml` to add your feeds:

```yaml
feeds:
  - name: "Rust Blog"
    url: "https://blog.rust-lang.org/feed.xml"
    topics: ["rust", "programming"]
  - name: "Hacker News Best"
    url: "https://hnrss.org/best"
    topics: ["tech", "startups"]
```

The RSS worker picks up changes automatically (it polls the file every 10 seconds by default).

---

## Keeping it running

The `docker-compose.yml` sets `restart: unless-stopped` on all containers, so OmniMem survives reboots as long as the Docker daemon starts at boot (it does by default after the install script).

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

Your memory data lives in the `valkey_data` Docker volume. You can also use OmniMem's built-in backup:

```bash
# From any connected Claude Code session:
# Call dump_to_file() — exports everything to a timestamped JSON in ./backups/
```

Or back up the Docker volume directly:

```bash
docker run --rm -v omnimem_valkey_data:/data -v $(pwd)/backups:/backup \
  alpine tar czf /backup/valkey-backup-$(date +%Y%m%d).tar.gz -C /data .
```

---

## Troubleshooting

**Build fails with out-of-memory errors** — The Pi 4 (4 GB) can run tight during the Docker build, especially when pip installs torch/sentence-transformers. Add a swap file:

```bash
sudo fallocate -l 2G /swapfile
sudo chmod 600 /swapfile
sudo mkswap /swapfile
sudo swapon /swapfile
# Make permanent:
echo '/swapfile none swap sw 0 0' | sudo tee -a /etc/fstab
```

Then retry `docker compose build`.

**Containers restart in a loop** — Check `docker compose logs valkey` first. The most common cause is `VALKEY_PASSWORD` not being set in `.env`.

**Slow first recall** — The embedding model loads lazily on first use. The first `recall()` or `remember()` call takes 10–30 seconds on a Pi as the model loads into RAM. Subsequent calls are fast.

**Cannot connect from another machine** — Make sure you changed the port bindings from `127.0.0.1` to `0.0.0.0` in `docker-compose.yml` and set auth tokens.
