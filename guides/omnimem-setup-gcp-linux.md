# Setting Up OmniMem on Google Cloud Platform (Linux)

This guide covers deploying OmniMem on a GCP Compute Engine instance running Linux. The steps are straightforward — GCP-specific parts are clearly marked so the general Linux instructions transfer to any environment.

---

## Prerequisites

- A GCP account with a project and billing enabled
- The `gcloud` CLI installed locally (or use Cloud Shell)
- Basic familiarity with SSH and the terminal
- An Anthropic API key (optional, for AI-powered features)

---

## Step 1 — Create a Compute Engine instance

### Using gcloud CLI

```bash
gcloud compute instances create omnimem \
  --zone=europe-west2-a \
  --machine-type=e2-medium \
  --image-family=ubuntu-2404-lts-amd64 \
  --image-project=ubuntu-os-cloud \
  --boot-disk-size=20GB \
  --boot-disk-type=pd-balanced \
  --tags=omnimem
```

### Choosing a machine type

| Machine type | vCPUs | RAM | Cost (approx.) | Notes |
|-------------|-------|-----|-----------------|-------|
| `e2-small` | 2 | 2 GB | ~$12/month | Minimum — tight on RAM |
| `e2-medium` | 2 | 4 GB | ~$24/month | Comfortable for single user |
| `t2a-standard-1` | 1 | 4 GB | ~$22/month | ARM64 (Tau T2A) — native arm64 images |
| `e2-standard-2` | 2 | 8 GB | ~$48/month | Room for growth |

**ARM64 option:** GCP offers Tau T2A instances (arm64). OmniMem ships native arm64 images. To use one, change `--machine-type=t2a-standard-1` and `--image-family=ubuntu-2404-lts-arm64` in the command above.

### Zone selection

Pick a zone close to where you'll be working to minimise latency. The example uses `europe-west2-a` (London).

### Using the Console

Alternatively, create the instance via the GCP Console:

1. Go to **Compute Engine → VM instances → Create instance**
2. Set the name, region, and machine type
3. Under **Boot disk**, click **Change** → Ubuntu 24.04 LTS, 20 GB balanced persistent disk
4. Click **Create**

---

## Step 2 — Configure the firewall

Create firewall rules to allow access to OmniMem's ports:

```bash
gcloud compute firewall-rules create allow-omnimem \
  --allow tcp:8765,tcp:8080 \
  --source-ranges=YOUR_IP/32 \
  --target-tags=omnimem \
  --description="Allow OmniMem MCP and web UI"
```

Replace `YOUR_IP/32` with your IP address. To find it: `curl ifconfig.me`.

Do **not** use `0.0.0.0/0` as the source range unless you've configured authentication (step 5).

---

## Step 3 — Connect and install Docker

SSH into the instance:

```bash
gcloud compute ssh omnimem --zone=europe-west2-a
```

Install Docker:

```bash
curl -fsSL https://get.docker.com | sh
sudo usermod -aG docker $USER
```

Log out and reconnect:

```bash
exit
gcloud compute ssh omnimem --zone=europe-west2-a
```

Verify:

```bash
docker --version
docker compose version
```

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

Set the essentials:

```bash
VALKEY_PASSWORD=generate-a-strong-password-here
ANTHROPIC_API_KEY=sk-ant-your-key-here    # optional
```

### Security for a remote server

Since the instance is network-accessible, enable authentication:

```bash
MCP_AUTH_TOKEN=generate-a-long-random-token
WEB_UI_AUTH_TOKEN=generate-another-random-token
```

Generate tokens:

```bash
openssl rand -hex 32
```

### Expose to the network

Edit `docker-compose.yml` to change port bindings from localhost:

```yaml
# mcp_server ports:
ports:
  - "0.0.0.0:${MCP_PORT:-8765}:${MCP_PORT:-8765}"

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

First build takes 5–10 minutes. Monitor:

```bash
docker compose logs -f
```

All four services should come up:

- `valkey` — `Ready to accept connections`
- `mcp_server` — listening on port 8765
- `rss_worker` — scheduler started
- `web_ui` — serving on port 8080

---

## Step 7 — Verify

Get your instance's external IP:

```bash
gcloud compute instances describe omnimem \
  --zone=europe-west2-a \
  --format='get(networkInterfaces[0].accessConfigs[0].natIP)'
```

From your local machine:

```bash
curl http://<external-ip>:8080
curl -H "Authorization: Bearer your-mcp-token" http://<external-ip>:8765/health
```

Or open `http://<external-ip>:8080` in your browser.

---

## Step 8 — Connect your AI tool

On the machine where you run Claude Code, add to `~/.claude.json`:

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "sse",
      "url": "http://<external-ip>:8765/sse",
      "headers": {
        "Authorization": "Bearer your-mcp-token"
      }
    }
  }
}
```

Auto-allow tools in `~/.claude/settings.json`:

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
      "url": "http://<external-ip>:8765/mcp",
      "headers": {
        "Authorization": "Bearer your-mcp-token"
      }
    }
  }
}
```

---

## Step 9 — Set up HTTPS with a reverse proxy (recommended)

For production use, terminate TLS in front of OmniMem. Caddy is the simplest option — automatic Let's Encrypt certificates with zero configuration.

### Prerequisites

- A domain pointing to your instance's external IP (e.g. `omnimem.yourdomain.com`)
- Firewall rules allowing ports 80 and 443 (for Let's Encrypt):

```bash
gcloud compute firewall-rules create allow-https \
  --allow tcp:80,tcp:443 \
  --source-ranges=0.0.0.0/0 \
  --target-tags=omnimem
```

### Install and configure Caddy

```bash
sudo apt install -y debian-keyring debian-archive-keyring apt-transport-https
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' | sudo gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' | sudo tee /etc/apt/sources.list.d/caddy-stable.list
sudo apt update
sudo apt install caddy
```

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

```bash
sudo systemctl restart caddy
```

Revert port bindings in `docker-compose.yml` back to `127.0.0.1` — Caddy handles external traffic.

Update your Claude Code config:

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

---

## Step 10 — Configure RSS feeds (optional)

```bash
nano rss_worker/feeds.yml
```

```yaml
feeds:
  - name: "Google Cloud Blog"
    url: "https://cloud.google.com/blog/rss"
    topics: ["gcp", "cloud"]
  - name: "Go Blog"
    url: "https://go.dev/blog/feed.atom"
    topics: ["go", "programming"]
```

---

## Persistence and backups

### Data persistence

Memory data lives in the `valkey_data` Docker volume. It survives container restarts, rebuilds, and instance reboots (as long as the boot disk isn't deleted).

### Automated backups

```bash
crontab -e
```

Add a nightly backup:

```
0 2 * * * cd /home/$USER/omnimem && docker run --rm -v omnimem_valkey_data:/data -v /home/$USER/omnimem/backups:/backup alpine tar czf /backup/valkey-$(date +\%Y\%m\%d).tar.gz -C /data .
```

### Backup to Cloud Storage

```bash
# Install gsutil (part of the Google Cloud SDK, already on GCE instances)
gsutil cp backups/valkey-$(date +%Y%m%d).tar.gz gs://your-backup-bucket/omnimem/
```

Add to your cron job for automated offsite backups.

---

## Using a static external IP

By default, GCE instances get an ephemeral external IP that can change on restart. To keep a stable IP:

```bash
gcloud compute addresses create omnimem-ip --region=europe-west2

gcloud compute instances delete-access-config omnimem \
  --zone=europe-west2-a \
  --access-config-name="External NAT"

gcloud compute instances add-access-config omnimem \
  --zone=europe-west2-a \
  --address=$(gcloud compute addresses describe omnimem-ip --region=europe-west2 --format='get(address)')
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

## Cost management

### Stop when not in use

If you don't need OmniMem running 24/7:

```bash
gcloud compute instances stop omnimem --zone=europe-west2-a
gcloud compute instances start omnimem --zone=europe-west2-a
```

Stopped instances don't incur compute charges (only disk storage). Your data persists.

### Preemptible / Spot instances

For non-critical or development use, create the instance with `--provisioning-model=SPOT` to save up to 60–91%. The trade-off is GCP can reclaim it with 30 seconds notice. Your Valkey data persists on the boot disk, so you just restart.

---

## Troubleshooting

**Cannot SSH** — Check that the firewall allows port 22 from your IP. GCP's default network includes this rule, but custom networks may not.

**Build runs out of memory** — On an `e2-small` (2 GB), add swap:

```bash
sudo fallocate -l 2G /swapfile
sudo chmod 600 /swapfile
sudo mkswap /swapfile
sudo swapon /swapfile
echo '/swapfile none swap sw 0 0' | sudo tee -a /etc/fstab
```

**Valkey crash-looping** — `docker compose logs valkey` — usually means `VALKEY_PASSWORD` isn't set in `.env`.

**External IP changed after reboot** — Use a static IP (see section above) or update your DNS record and Claude Code config.

**Let's Encrypt certificate not issuing** — Make sure ports 80 and 443 are open in the firewall and your DNS A record points to the instance's external IP. Caddy logs are at `journalctl -u caddy`.
