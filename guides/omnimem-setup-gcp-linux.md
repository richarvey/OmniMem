# Setting Up OmniMem on Google Cloud Platform (Linux)

This guide puts OmniMem on a Compute Engine VM. The GCP-specific parts are clearly marked, and the Linux parts work anywhere.

OmniMem 7 is one binary with a SQLite file, so a small VM does the job. No Docker, no Valkey, no container sizing.

---

## What you need

- A GCP account with a project and billing switched on
- The `gcloud` CLI installed locally, or Cloud Shell
- To be comfortable with SSH and a terminal
- An Anthropic API key if you want the Claude-powered extras (optional)

---

## Step 1: Create a VM

### With gcloud

```bash
gcloud compute instances create omnimem \
  --zone=europe-west2-a \
  --machine-type=e2-small \
  --image-family=debian-12 \
  --image-project=debian-cloud \
  --boot-disk-size=10GB \
  --boot-disk-type=pd-balanced \
  --tags=omnimem
```

### Picking a machine type

OmniMem needs a few hundred MB of RAM, mostly for the embedding model:

| Machine type | vCPUs | RAM | Notes |
|-------------|-------|-----|-------|
| `e2-small` | 2 | 2 GB | Comfortable for one person |
| `e2-medium` | 2 | 4 GB | Room to run other things alongside |
| `t2a-standard-1` | 1 | 4 GB | ARM64 (Tau T2A), native arm64 build |

For arm64, use `--machine-type=t2a-standard-1` and `--image-family=debian-12-arm64`.

Pick a zone near where you work. The example uses `europe-west2-a`, London, because that's where I am.

### Or in the Console

1. **Compute Engine → VM instances → Create instance**
2. Set the name, region and machine type
3. **Boot disk → Change**: Debian 12 (or Ubuntu 24.04), 10 GB balanced persistent disk
4. Add the network tag `omnimem`
5. **Create**

---

## Step 2: Firewall

You'll put OmniMem behind Caddy for HTTPS (step 6), so open 80 and 443 and keep 8765 closed:

```bash
gcloud compute firewall-rules create allow-omnimem-https \
  --allow tcp:80,tcp:443 \
  --source-ranges=0.0.0.0/0 \
  --target-tags=omnimem \
  --description="HTTPS for OmniMem through Caddy"
```

If you'd rather reach OmniMem directly from a VPN instead, allow `tcp:8765` from your VPN range only. Never from `0.0.0.0/0`.

---

## Step 3: Install OmniMem

> [!NOTE]
> Coming with 7.0.0. Until the first release you can build from source (see [Build from source](../docs/quick-start.md#build-from-source)).

SSH in:

```bash
gcloud compute ssh omnimem --zone=europe-west2-a
```

Download the `.deb` for your architecture from the [releases page](https://code.squarecows.com/ric/omnimem/releases) and install it:

```bash
sudo apt install ./omnimem_<version>_amd64.deb
```

(On a T2A VM it's `omnimem_<version>_arm64.deb`.)

You now have:

- a systemd service, `omnimem.service`, running `omnimem serve` as the `omnimem` system user
- configuration in `/etc/omnimem/omnimem.env`
- the database, `feeds.yml`, backups and the model cache in `/var/lib/omnimem`

---

## Step 4: Configure it

```bash
sudo nano /etc/omnimem/omnimem.env
```

Generate an access token and add it, plus your Anthropic key if you have one:

```bash
openssl rand -hex 32
```

```bash
MCP_AUTH_TOKEN=paste-the-token-here
ANTHROPIC_API_KEY=sk-ant-your-key-here    # optional
```

Leave `MCP_HOST` at `127.0.0.1`. Caddy reaches OmniMem over localhost, so it never has to listen anywhere else. If you do set `0.0.0.0` for the VPN route, OmniMem won't start without the token or OAuth.

Everything else is in the [configuration reference](../docs/configuration.md).

---

## Step 5: Start it

```bash
sudo systemctl enable --now omnimem
journalctl -u omnimem -f
```

The first start downloads the embedding model. Once the log says the MCP server is listening on `/mcp`:

```bash
curl http://127.0.0.1:8765/healthz
```

should answer `{"status": "ok"}`.

---

## Step 6: HTTPS with Caddy

You'll need a domain pointing at the VM's external IP (see [static IP](#using-a-static-external-ip) below, or your DNS will point at the wrong thing after a restart).

```bash
sudo apt install -y debian-keyring debian-archive-keyring apt-transport-https curl
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' | sudo gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' | sudo tee /etc/apt/sources.list.d/caddy-stable.list
sudo apt update
sudo apt install caddy
```

`/etc/caddy/Caddyfile`:

```
omnimem.yourdomain.com {
    reverse_proxy localhost:8765
}
```

One port, one line. `/mcp`, the OAuth routes and `/healthz` all come from OmniMem on 8765.

Tell OmniMem its public address in `/etc/omnimem/omnimem.env`, so it trusts the hostname:

```bash
MCP_PUBLIC_URL=https://omnimem.yourdomain.com
```

```bash
sudo systemctl restart omnimem caddy
```

### Want claude.ai too?

Add OAuth to the environment file and restart:

```bash
OAUTH_ENABLED=true
OAUTH_BASE_URL=https://omnimem.yourdomain.com
OAUTH_ADMIN_USER=admin
OAUTH_ADMIN_PASSWORD=a-strong-password-here
```

Then follow [the claude.ai guide](claude-ai.md). Token-based clients keep working.

---

## Step 7: Connect your agent

On the machine where you run Claude Code:

```bash
claude mcp add --transport http omnimem https://omnimem.yourdomain.com/mcp \
  --header "Authorization: Bearer your-token-here" \
  --scope user
```

And in `~/.claude/settings.json`, so you're not asked every call:

```json
{
  "permissions": {
    "allow": [
      "mcp__omnimem__*"
    ]
  }
}
```

The [connection guides](README.md) cover the other agents.

---

## Step 8: RSS feeds (optional)

```bash
sudo -u omnimem nano /var/lib/omnimem/feeds.yml
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

OmniMem notices the change by itself. See [RSS and knowledge](../docs/rss-knowledge.md).

---

## Backups

The database is one SQLite file in `/var/lib/omnimem`. Don't copy it live, take a backup.

### Nightly backups

```bash
sudo crontab -u omnimem -e
```

```
0 2 * * * /usr/bin/omnimem --db /var/lib/omnimem/omnimem.db export /var/lib/omnimem/backups/omnimem-$(date +\%Y\%m\%d).json
```

### Off to Cloud Storage

```bash
gcloud storage cp /var/lib/omnimem/backups/omnimem-$(date +%Y%m%d).json gs://your-backup-bucket/omnimem/
```

Give the VM's service account write access to the bucket and add the copy to the cron job.

---

## Using a static external IP

VMs get an ephemeral external IP by default, and it can change when the VM restarts. Reserve one:

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

```bash
sudo apt install ./omnimem_<new-version>_amd64.deb
sudo systemctl restart omnimem
```

The database migrates itself on start. Back up first anyway.

---

## Saving money

### Stop it when you're not using it

```bash
gcloud compute instances stop omnimem --zone=europe-west2-a
gcloud compute instances start omnimem --zone=europe-west2-a
```

A stopped VM only costs its disk. Your memories are on that disk, so nothing's lost.

### Spot VMs

For personal use, `--provisioning-model=SPOT` saves a lot, with the catch that GCP can take the VM back at short notice. OmniMem writes every change to disk as it goes, so a reclaimed VM just needs starting again.

---

## Docker instead

If you'd rather run containers, the `richarvey/omnimem` image works on any VM. The firewall and Caddy steps still apply. See [Running OmniMem in Docker](docker.md).

---

## Coming from 6.x

1. On the 6.x server, call `dump_to_file` and copy the JSON file across
2. Import it:

```bash
sudo -u omnimem omnimem --db /var/lib/omnimem/omnimem.db import backup.json
sudo systemctl restart omnimem
```

The import re-embeds every memory at roughly 8 ms each. Then switch your clients from `/sse` to `/mcp` and delete the old firewall rule for port 8080.

---

## Troubleshooting

**Can't SSH in**: the default network allows port 22, but custom networks might not.

**The service won't start**: `journalctl -u omnimem -e`. A non-local `MCP_HOST` without a token or OAuth is refused on purpose, and a half-configured OAuth setup names the missing setting.

**421 Misdirected Request through Caddy**: set `MCP_PUBLIC_URL` (or `OAUTH_BASE_URL`) to the address clients use, and restart OmniMem.

**External IP changed after a restart**: reserve a static IP (above) and update DNS.

**Let's Encrypt won't issue a certificate**: ports 80 and 443 have to be open and your DNS A record has to point at the VM. `journalctl -u caddy` explains.
