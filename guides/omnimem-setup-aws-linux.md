# Setting Up OmniMem on AWS (Linux)

This guide covers deploying OmniMem on an AWS EC2 instance running Linux. The same steps apply to any Linux server — the AWS-specific parts are clearly marked so you can skip them if you're on bare metal, another cloud, or a VPS.

---

## Prerequisites

- An AWS account with EC2 access
- Basic familiarity with SSH and the terminal
- An Anthropic API key (optional, for AI-powered features)

---

## Step 1 — Launch an EC2 instance

### Instance type

OmniMem's main resource requirement is RAM for the sentence-transformers embedding model. Recommended instances:

| Instance | vCPUs | RAM | Cost (approx.) | Notes |
|----------|-------|-----|-----------------|-------|
| `t3.small` | 2 | 2 GB | ~$15/month | Minimum viable — tight on RAM |
| `t3.medium` | 2 | 4 GB | ~$30/month | Comfortable for single-user |
| `t4g.medium` | 2 | 4 GB | ~$24/month | ARM64 (Graviton) — cheaper and native arm64 images |
| `t4g.large` | 2 | 8 GB | ~$48/month | ARM64, room for growth |

**Graviton (arm64) instances are recommended** — they're cheaper and OmniMem ships native arm64 images, so no emulation overhead.

### AMI

Use **Ubuntu 24.04 LTS** (or 22.04). Select the arm64 AMI if you chose a Graviton instance.

### Storage

The default 8 GB root volume is too small. Set it to **20 GB gp3** minimum.

### Security group

Create or select a security group with:

| Port | Source | Purpose |
|------|--------|---------|
| 22 | Your IP | SSH access |
| 8765 | Your IP / VPN CIDR | MCP server |
| 8080 | Your IP / VPN CIDR | Web dashboard |

Do **not** open 8765 or 8080 to `0.0.0.0/0` unless you've configured authentication (step 4).

### Key pair

Create or select an SSH key pair. Download the `.pem` file.

### Launch

Launch the instance and note the public IP or DNS name.

---

## Step 2 — Connect and install Docker

SSH into your instance:

```bash
ssh -i your-key.pem ubuntu@<instance-ip>
```

Install Docker:

```bash
curl -fsSL https://get.docker.com | sh
sudo usermod -aG docker $USER
```

Log out and back in for the group change to take effect:

```bash
exit
ssh -i your-key.pem ubuntu@<instance-ip>
```

Verify:

```bash
docker --version
docker compose version
```

---

## Step 3 — Clone OmniMem

```bash
git clone https://codeberg.org/ric_harvey/omnimem.git
cd omnimem
```

---

## Step 4 — Configure the environment

```bash
cp .env.example .env
nano .env
```

Set the essentials:

```bash
VALKEY_PASSWORD=generate-a-strong-password-here
ANTHROPIC_API_KEY=sk-ant-your-key-here    # optional
```

### Security configuration for a remote server

Since this instance is accessible over the network, you should enable authentication:

```bash
MCP_AUTH_TOKEN=generate-a-long-random-token
WEB_UI_AUTH_TOKEN=generate-another-random-token
```

Generate random tokens with:

```bash
openssl rand -hex 32
```

### Exposing to the network

Edit `docker-compose.yml` to change the port bindings from localhost to all interfaces:

```yaml
# mcp_server ports:
ports:
  - "0.0.0.0:${MCP_PORT:-8765}:${MCP_PORT:-8765}"

# web_ui ports:
ports:
  - "0.0.0.0:${WEB_PORT:-8080}:8080"
```

This allows connections from outside the instance, controlled by your security group.

---

## Step 5 — Build and start

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

This pulls the images in under a minute rather than building from source. Especially useful on smaller instances where builds can exhaust RAM.

To pin a specific release instead of `latest`, use the version tag (e.g. `richarvey/omnimem-mcp:v5.5.3`). See [releases on Codeberg](https://codeberg.org/ric_harvey/omnimem/releases) for available tags.

### Option B — Build from source

```bash
docker compose up -d
```

First build takes 5–10 minutes depending on instance type. Monitor progress:

```bash
docker compose logs -f
```

Wait for all four services to report healthy:

- `valkey` — `Ready to accept connections`
- `mcp_server` — listening on port 8765
- `rss_worker` — scheduler started
- `web_ui` — serving on port 8080

---

## Step 6 — Verify

From your local machine:

```bash
# Web dashboard
curl http://<instance-ip>:8080

# MCP health check
curl http://<instance-ip>:8765/health

# With auth token:
curl -H "Authorization: Bearer your-mcp-token" http://<instance-ip>:8765/health
```

Or open `http://<instance-ip>:8080` in your browser.

---

## Step 7 — Connect your AI tool

On the machine where you run Claude Code, add to `~/.claude.json`:

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "sse",
      "url": "http://<instance-ip>:8765/sse",
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

### Using Streamable HTTP (recommended)

Set `MCP_TRANSPORT=http` in `.env`, restart, and use:

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "http",
      "url": "http://<instance-ip>:8765/mcp",
      "headers": {
        "Authorization": "Bearer your-mcp-token"
      }
    }
  }
}
```

---

## Step 8 — Set up HTTPS with a reverse proxy (recommended)

Running OmniMem over plain HTTP works on a private network, but for anything exposed to the internet you should terminate TLS. The simplest approach is Caddy, which handles Let's Encrypt certificates automatically.

### Prerequisites

- A domain name pointing to your instance's IP (e.g. `omnimem.yourdomain.com`)
- Port 80 and 443 open in your security group (for Let's Encrypt validation)

### Install Caddy

```bash
sudo apt install -y debian-keyring debian-archive-keyring apt-transport-https
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' | sudo gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' | sudo tee /etc/apt/sources.list.d/caddy-stable.list
sudo apt update
sudo apt install caddy
```

### Configure Caddy

Create `/etc/caddy/Caddyfile`:

```
omnimem.yourdomain.com {
    handle /sse* {
        reverse_proxy localhost:8765
    }
    handle /mcp* {
        reverse_proxy localhost:8765
    }
    handle {
        reverse_proxy localhost:8080
    }
}
```

Restart Caddy:

```bash
sudo systemctl restart caddy
```

Caddy will automatically obtain and renew a Let's Encrypt certificate.

Update your Claude Code config to use the HTTPS URL:

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "sse",
      "url": "https://omnimem.yourdomain.com/sse",
      "headers": {
        "Authorization": "Bearer your-mcp-token"
      }
    }
  }
}
```

If you use the Caddy proxy approach, revert the port bindings in `docker-compose.yml` back to `127.0.0.1` — Caddy handles external connections and forwards locally.

---

## Step 9 — Configure RSS feeds (optional)

```bash
nano rss_worker/feeds.yml
```

```yaml
feeds:
  - name: "AWS Blog"
    url: "https://aws.amazon.com/blogs/aws/feed/"
    topics: ["aws", "cloud"]
  - name: "Rust Blog"
    url: "https://blog.rust-lang.org/feed.xml"
    topics: ["rust", "programming"]
```

---

## Persistence and backups

### Data persistence

All memory data is stored in the `valkey_data` Docker volume, which persists across container restarts and rebuilds.

### Automated backups

Create a cron job to back up the Valkey data:

```bash
crontab -e
```

Add:

```
0 2 * * * cd /home/ubuntu/omnimem && docker run --rm -v omnimem_valkey_data:/data -v /home/ubuntu/omnimem/backups:/backup alpine tar czf /backup/valkey-$(date +\%Y\%m\%d).tar.gz -C /data .
```

This creates a nightly backup at 2 AM.

### Backup to S3

To send backups to S3:

```bash
# Install AWS CLI
sudo apt install awscli

# After the tar backup:
aws s3 cp backups/valkey-$(date +%Y%m%d).tar.gz s3://your-backup-bucket/omnimem/
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

## Troubleshooting

**Instance runs out of memory during build** — If you're on a `t3.small` (2 GB), the build can exhaust RAM. Add swap:

```bash
sudo fallocate -l 2G /swapfile
sudo chmod 600 /swapfile
sudo mkswap /swapfile
sudo swapon /swapfile
echo '/swapfile none swap sw 0 0' | sudo tee -a /etc/fstab
```

**Security group blocking connections** — Double-check that ports 8765 and 8080 are open to your IP in the EC2 security group.

**Valkey container crash-looping** — Check `docker compose logs valkey`. Usually means `VALKEY_PASSWORD` is not set in `.env`.

**High latency from a distant region** — OmniMem's recall involves embedding the query locally on the server. If your EC2 instance is in `us-east-1` and you're working from Europe, consider deploying in a closer region.
