# Setting Up OmniMem on Linux with Tailscale Funnel

This guide covers deploying OmniMem on any Linux server and using Tailscale Funnel to expose the MCP server over HTTPS — no domain name, no port forwarding, no firewall rules. This is the easiest way to connect OmniMem to cloud services like claude.ai that need a publicly reachable HTTPS endpoint with OAuth 2.1 authentication.

---

## Why Tailscale Funnel?

Tailscale Funnel gives you a stable `https://your-machine.tailnet-name.ts.net` URL that routes traffic to a local port. It handles TLS termination automatically and works from behind NAT, on home networks, cloud VMs, or a Raspberry Pi under your desk. No DNS configuration, no Let's Encrypt renewal, no reverse proxy to manage.

OmniMem v5.5.1+ has explicit support for Tailscale Funnel, with fixes for the FastMCP host and origin guards that previously broke tunnel setups.

---

## Prerequisites

- A Linux machine (Ubuntu 22.04/24.04, Debian 12, Fedora, Arch — anything that runs Docker)
- At least 4 GB RAM (2 GB works with swap — see troubleshooting)
- A Tailscale account (free tier is fine) — [tailscale.com](https://tailscale.com)
- An Anthropic API key (optional, for AI-powered features)

---

## Step 1 — Install Docker

```bash
curl -fsSL https://get.docker.com | sh
sudo usermod -aG docker $USER
```

Log out and back in, then verify:

```bash
docker --version
docker compose version
```

---

## Step 2 — Install Tailscale

```bash
curl -fsSL https://tailscale.com/install.sh | sh
```

Start Tailscale and authenticate:

```bash
sudo tailscale up
```

This opens a URL in your terminal — visit it to log in and add the machine to your tailnet.

Verify it's connected:

```bash
tailscale status
```

You should see your machine listed with a `100.x.x.x` IP address and a hostname like `your-machine.tailnet-name.ts.net`.

### Note your Tailscale hostname

```bash
tailscale status --self --json | jq -r '.Self.DNSName' | sed 's/\.$//'
```

This gives you something like `my-server.tail12345.ts.net`. You'll need this for the OAuth configuration.

---

## Step 3 — Enable Tailscale Funnel

Funnel needs to be enabled in your tailnet's ACL policy. Go to the Tailscale admin console at [login.tailscale.com/admin/acls](https://login.tailscale.com/admin/acls) and make sure your ACL includes a `nodeAttrs` entry allowing Funnel:

```json
{
  "nodeAttrs": [
    {
      "target": ["autogroup:member"],
      "attr": ["funnel"]
    }
  ]
}
```

This allows all members of your tailnet to use Funnel. You can restrict it to specific nodes if you prefer.

Then expose port 8765 (the MCP server) via Funnel:

```bash
sudo tailscale funnel 8765
```

Verify Funnel is serving:

```bash
tailscale funnel status
```

You should see output showing that `https://your-machine.tailnet-name.ts.net` is forwarding to `127.0.0.1:8765`.

> **Important:** Tailscale Funnel routes HTTPS traffic on port 443 to your local port. The public URL is `https://your-machine.tailnet-name.ts.net` (no port number). TLS is terminated by Tailscale before the traffic reaches OmniMem.

To make Funnel persist across reboots, use the background mode:

```bash
sudo tailscale funnel --bg 8765
```

### Optional — Also expose the web UI

If you want the web dashboard accessible via Tailscale (without Funnel, so only devices on your tailnet can reach it):

```bash
sudo tailscale serve --bg 8080
```

This makes the web UI available at `https://your-machine.tailnet-name.ts.net:8080` to your tailnet devices only — not to the public internet.

---

## Step 4 — Clone OmniMem

```bash
git clone https://codeberg.org/ric_harvey/omnimem.git
cd omnimem
```

---

## Step 5 — Configure the environment

```bash
cp .env.example .env
nano .env
```

### Core settings

```bash
VALKEY_PASSWORD=generate-a-strong-password-here
ANTHROPIC_API_KEY=sk-ant-your-key-here    # optional
```

Generate a strong Valkey password:

```bash
openssl rand -hex 32
```

### Transport

Set the MCP server to use Streamable HTTP (required for claude.ai):

```bash
MCP_TRANSPORT=http
```

### OAuth 2.1 — required for claude.ai

claude.ai connects to remote MCP servers using OAuth 2.1. OmniMem has a built-in OAuth authorisation server. Enable it:

```bash
OAUTH_ENABLED=true
OAUTH_BASE_URL=https://your-machine.tailnet-name.ts.net
OAUTH_ADMIN_USER=admin
OAUTH_ADMIN_PASSWORD=pick-a-strong-password-here
```

Replace `your-machine.tailnet-name.ts.net` with your actual Tailscale hostname from step 2.

The `OAUTH_BASE_URL` tells OmniMem what its externally-reachable URL is. The server uses this to configure the FastMCP host and origin guards correctly — this is the fix from v5.5.1 that makes tunnels work.

Optional OAuth tuning:

```bash
# OAUTH_REFRESH_MAX_DAYS=30          # How long a refresh token chain lasts before re-login
# OAUTH_REFRESH_GRACE_SECONDS=120    # Grace period for concurrent refresh token rotation
# OAUTH_LOGIN_MAX_ATTEMPTS=10        # Brute-force protection: failed logins per IP
# OAUTH_LOGIN_WINDOW_SECONDS=900     # before the login form is blocked for the window
```

### Bearer token auth for the web UI

Since the MCP endpoint uses OAuth, you only need a bearer token for the web dashboard:

```bash
WEB_UI_AUTH_TOKEN=generate-another-random-token
```

### Port binding

The default `127.0.0.1` binding is correct for Tailscale Funnel — Funnel connects to localhost. No need to change port bindings to `0.0.0.0`.

If you also want direct access to the web UI from your local network (not via Tailscale), change the web_ui binding in `docker-compose.yml`:

```yaml
# web_ui ports:
ports:
  - "0.0.0.0:${WEB_PORT:-8080}:8080"
```

---

## Step 6 — Build and start

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

First build takes 5–15 minutes depending on your machine. Monitor progress:

```bash
docker compose logs -f
```

All four services should come up:

- `valkey` — `Ready to accept connections`
- `mcp_server` — listening on port 8765
- `rss_worker` — scheduler started
- `web_ui` — serving on port 8080

---

## Step 7 — Verify the Funnel endpoint

From any machine (not just your server), test the public HTTPS endpoint:

```bash
curl https://your-machine.tailnet-name.ts.net/health
```

You should get a JSON response with Valkey connection status, index counts, and embedding model info.

Check the web UI locally:

```bash
curl http://localhost:8080
```

Or if you set up Tailscale serve for port 8080, open `https://your-machine.tailnet-name.ts.net:8080` from a device on your tailnet.

---

## Step 8 — Connect claude.ai

This is the main reason for the Tailscale Funnel + OAuth setup. claude.ai can connect to your self-hosted OmniMem as a remote MCP connector.

1. Go to [claude.ai](https://claude.ai)
2. Open **Settings → Connectors** (or the MCP connectors menu)
3. Add a new connector with the URL:

```
https://your-machine.tailnet-name.ts.net/mcp
```

4. claude.ai will redirect you to OmniMem's OAuth login page. Sign in with the `OAUTH_ADMIN_USER` and `OAUTH_ADMIN_PASSWORD` you set in `.env`.

5. Authorise the connection. claude.ai will receive OAuth tokens and can now call OmniMem tools directly from the browser.

The OAuth tokens refresh automatically. The `OAUTH_REFRESH_MAX_DAYS` setting (default 30) controls how long before you need to re-authenticate.

---

## Step 9 — Connect Claude Code (optional)

If you also run Claude Code on your local machine or another device, you can connect directly via Tailscale (using the private tailnet address, not Funnel):

Add to `~/.claude.json`:

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "http",
      "url": "http://100.x.x.x:8765/mcp"
    }
  }
}
```

Replace `100.x.x.x` with your server's Tailscale IP (from `tailscale ip -4`).

Or use the Funnel URL with bearer token auth:

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "http",
      "url": "https://your-machine.tailnet-name.ts.net/mcp",
      "headers": {
        "Authorization": "Bearer your-mcp-token"
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

---

## Step 10 — Configure RSS feeds (optional)

```bash
nano rss_worker/feeds.yml
```

```yaml
feeds:
  - name: "Rust Blog"
    url: "https://blog.rust-lang.org/feed.xml"
    topics: ["rust", "programming"]
  - name: "Tailscale Blog"
    url: "https://tailscale.com/blog/index.xml"
    topics: ["networking", "tailscale"]
```

---

## Persistence and backups

### Data persistence

Memory data lives in the `valkey_data` Docker volume. It survives container restarts, rebuilds, and reboots.

### Automated backups

```bash
crontab -e
```

Add a nightly backup:

```
0 2 * * * cd /home/$USER/omnimem && docker run --rm -v omnimem_valkey_data:/data -v /home/$USER/omnimem/backups:/backup alpine tar czf /backup/valkey-$(date +\%Y\%m\%d).tar.gz -C /data .
```

---

## Updating

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

---

## Tailscale Funnel vs. Tailscale Serve

These are two different features and it's worth understanding the distinction:

**Tailscale Serve** exposes a local port to devices on your tailnet only. Your laptop, phone, and other machines logged into your Tailscale account can reach it. The public internet cannot. Good for the web dashboard.

**Tailscale Funnel** exposes a local port to the entire internet via a `*.ts.net` HTTPS URL. Any device can reach it, including claude.ai's servers. Required for the OAuth flow since claude.ai needs to reach your MCP server from Anthropic's infrastructure.

You can run both:

```bash
sudo tailscale funnel --bg 8765    # MCP server — public (for claude.ai)
sudo tailscale serve --bg 8080     # Web UI — tailnet only
```

---

## Troubleshooting

**Funnel not working — "Funnel not available"** — Make sure Funnel is enabled in your tailnet ACL policy (step 3). You need the `funnel` nodeAttr. Check the Tailscale admin console.

**OAuth login page shows 421 Misdirected Request** — The `OAUTH_BASE_URL` in your `.env` doesn't match the URL you're accessing. Make sure it's set to exactly `https://your-machine.tailnet-name.ts.net` (no trailing slash, no port number). This was fixed in v5.5.1.

**OAuth login page shows 403 Forbidden Origin** — Same cause as above. The server derives allowed origins from `OAUTH_BASE_URL`. If you need additional origins, set `MCP_ALLOWED_ORIGINS` in `.env`.

**claude.ai says "connection failed" after authorising** — Check that the MCP server is running (`docker compose ps`) and that Funnel is active (`tailscale funnel status`). Also verify `MCP_TRANSPORT=http` is set in `.env` — claude.ai requires Streamable HTTP, not SSE.

**Tokens expire and claude.ai disconnects** — The default `OAUTH_REFRESH_MAX_DAYS=30` means you need to re-authenticate monthly. Increase it up to 90 if you want less frequent re-auth. The `OAUTH_REFRESH_GRACE_SECONDS=120` setting prevents logout when multiple connections refresh simultaneously (fixed in v5.5.2).

**Build runs out of memory** — On machines with 2 GB RAM, add swap:

```bash
sudo fallocate -l 2G /swapfile
sudo chmod 600 /swapfile
sudo mkswap /swapfile
sudo swapon /swapfile
echo '/swapfile none swap sw 0 0' | sudo tee -a /etc/fstab
```

Or skip the build entirely by using Docker Hub images (Option A in step 6).

**Valkey container crash-looping** — Check `docker compose logs valkey`. Usually means `VALKEY_PASSWORD` is not set in `.env`.

**Slow first recall** — The embedding model loads lazily on first use. The first `recall()` takes a few seconds as the model loads into memory. Subsequent calls are fast.

**Want to use a custom domain instead of ts.net** — You can point a CNAME at your Tailscale hostname and set `OAUTH_BASE_URL` to your custom domain. Tailscale Funnel handles TLS for `*.ts.net` hostnames automatically; for custom domains you'd need to add Caddy or similar in front.
