# Setting Up OmniMem on AWS (Linux)

This guide puts OmniMem on an EC2 instance. Nearly all of it applies to any Linux server, so if you're on bare metal, another cloud or a VPS, skip the AWS-specific bits and carry on.

OmniMem 7 is one binary and one SQLite file, which makes this a lot less work than it used to be. No Docker, no Valkey, no working out how much RAM four containers want.

---

## What you need

- An AWS account with EC2 access
- To be comfortable with SSH and a terminal
- An Anthropic API key if you want the Claude-powered extras (optional)

---

## Step 1: Launch an EC2 instance

### Instance type

OmniMem's appetite is mostly the embedding model, a few hundred MB of RAM. A small instance is plenty for one person:

| Instance | vCPUs | RAM | Notes |
|----------|-------|-----|-------|
| `t4g.small` | 2 | 2 GB | ARM64 (Graviton). Comfortable for one person, and what I'd pick |
| `t4g.medium` | 2 | 4 GB | ARM64, room to run other things alongside |
| `t3.small` | 2 | 2 GB | x86_64, if you need Intel |

**Graviton is the one to go for.** It's cheaper, and OmniMem ships a native arm64 build.

### AMI

**Ubuntu 24.04 LTS** or **Debian 12**. Pick the arm64 AMI for Graviton. (Amazon Linux works too, with the `.rpm`.)

### Storage

The default 8 GB root volume is enough. OmniMem's binary, model and database fit in well under a gigabyte; make it 16 GB gp3 if you want headroom for backups.

### Security group

| Port | Source | Purpose |
|------|--------|---------|
| 22 | Your IP | SSH |
| 443 | `0.0.0.0/0` | HTTPS through Caddy (step 6) |
| 80 | `0.0.0.0/0` | Let's Encrypt certificate checks (step 6) |

Keep port 8765 closed to the world. OmniMem will listen on localhost and Caddy will forward to it. If you'd rather skip TLS and reach it only from a VPN, open 8765 to your VPN range instead, and never to `0.0.0.0/0`.

### Key pair and launch

Create or pick an SSH key pair, launch, and note the public IP or DNS name.

---

## Step 2: Install OmniMem

> [!NOTE]
> Coming with 7.0.0. Until the first release you can build from source (see [Build from source](../docs/quick-start.md#build-from-source)).

SSH in:

```bash
ssh -i your-key.pem ubuntu@<instance-ip>
```

Download the package for your architecture from the [releases page](https://code.squarecows.com/ric/omnimem/releases) and install it. On Graviton with Ubuntu or Debian:

```bash
sudo apt install ./omnimem_<version>_arm64.deb
```

On Amazon Linux use the `.rpm` (`omnimem-<version>.aarch64.rpm`, or `x86_64` on Intel):

```bash
sudo dnf install ./omnimem-<version>.aarch64.rpm
```

You now have:

- a systemd service, `omnimem.service`, running `omnimem serve` as the `omnimem` system user
- configuration in `/etc/omnimem/omnimem.env`
- the database, `feeds.yml`, backups and the model cache in `/var/lib/omnimem`

---

## Step 3: Configure it

```bash
sudo nano /etc/omnimem/omnimem.env
```

Generate an access token and add it:

```bash
openssl rand -hex 32
```

```bash
MCP_AUTH_TOKEN=paste-the-token-here
ANTHROPIC_API_KEY=sk-ant-your-key-here    # optional
```

Leave `MCP_HOST` at its default, `127.0.0.1`. Caddy is going to talk to OmniMem over localhost, so it never needs to listen anywhere else.

If you do change `MCP_HOST` to `0.0.0.0` (the VPN route), OmniMem won't start without the token or OAuth. Good.

The [configuration reference](../docs/configuration.md) has everything else.

---

## Step 4: Start it

```bash
sudo systemctl enable --now omnimem
journalctl -u omnimem -f
```

The first start downloads the embedding model. When the log says the MCP server is listening on `/mcp`, you're in business. Check it:

```bash
curl http://127.0.0.1:8765/healthz
```

That should answer `{"status": "ok"}`.

---

## Step 5: HTTPS with Caddy

Plain HTTP is fine inside a private network, but for anything on the internet you want TLS. Caddy sorts out Let's Encrypt certificates on its own, which is why I reach for it every time.

You'll need a domain pointing at the instance, for example `omnimem.yourdomain.com`, and ports 80 and 443 open (step 1).

### Install Caddy

```bash
sudo apt install -y debian-keyring debian-archive-keyring apt-transport-https
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' | sudo gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' | sudo tee /etc/apt/sources.list.d/caddy-stable.list
sudo apt update
sudo apt install caddy
```

### Configure Caddy

`/etc/caddy/Caddyfile`:

```
omnimem.yourdomain.com {
    reverse_proxy localhost:8765
}
```

That's the whole thing. OmniMem serves `/mcp`, the OAuth routes and `/healthz` on the one port, and there's no web UI to route separately any more.

Tell OmniMem its public address, so it trusts that hostname, in `/etc/omnimem/omnimem.env`:

```bash
MCP_PUBLIC_URL=https://omnimem.yourdomain.com
```

Then restart both:

```bash
sudo systemctl restart omnimem caddy
```

### Want claude.ai too?

claude.ai connects with OAuth rather than a token. Add these to the environment file and restart OmniMem:

```bash
OAUTH_ENABLED=true
OAUTH_BASE_URL=https://omnimem.yourdomain.com
OAUTH_ADMIN_USER=admin
OAUTH_ADMIN_PASSWORD=a-strong-password-here
```

Then follow [the claude.ai guide](claude-ai.md). Your token-based clients keep working alongside it.

---

## Step 6: Connect your agent

On the machine where you run Claude Code:

```bash
claude mcp add --transport http omnimem https://omnimem.yourdomain.com/mcp \
  --header "Authorization: Bearer your-token-here" \
  --scope user
```

Allow the OmniMem tools without a prompt each time, in `~/.claude/settings.json`:

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

## Step 7: RSS feeds (optional)

```bash
sudo -u omnimem nano /var/lib/omnimem/feeds.yml
```

```yaml
feeds:
  - name: "AWS News Blog"
    url: "https://aws.amazon.com/blogs/aws/feed/"
    topics: ["aws", "cloud"]
  - name: "Rust Blog"
    url: "https://blog.rust-lang.org/feed.xml"
    topics: ["rust", "programming"]
```

OmniMem picks up the change on its own. See [RSS and knowledge](../docs/rss-knowledge.md) for the rest of the format.

---

## Backups

Everything lives in `/var/lib/omnimem`, and the database is one SQLite file. Don't copy it while OmniMem is running; take a proper backup instead.

### Nightly backups

```bash
sudo crontab -u omnimem -e
```

```
0 2 * * * /usr/bin/omnimem --db /var/lib/omnimem/omnimem.db export /var/lib/omnimem/backups/omnimem-$(date +\%Y\%m\%d).json
```

### Off to S3

A backup on the same disk isn't much of a backup. Ship them off the instance:

```bash
aws s3 cp /var/lib/omnimem/backups/omnimem-$(date +%Y%m%d).json s3://your-backup-bucket/omnimem/
```

Give the instance an IAM role with write access to that bucket rather than putting keys on the box, and add the copy to the cron job.

---

## Updating

Download the new package and install it over the old one:

```bash
sudo apt install ./omnimem_<new-version>_arm64.deb
sudo systemctl restart omnimem
```

The database migrates itself on start. Take a backup first anyway.

---

## Docker instead

If your instances run everything in containers, use the `richarvey/omnimem` image. The security group and Caddy parts of this guide still apply. See [Running OmniMem in Docker](docker.md).

---

## Coming from 6.x

1. On the 6.x server, call `dump_to_file` and copy the JSON file across
2. Import it on the new instance:

```bash
sudo -u omnimem omnimem --db /var/lib/omnimem/omnimem.db import backup.json
sudo systemctl restart omnimem
```

The import re-embeds every memory at roughly 8 ms each. Then change your clients from `/sse` to `/mcp`, and remove the old port 8080 rule from the security group.

---

## Troubleshooting

**The service won't start**: `journalctl -u omnimem -e`. If `MCP_HOST` is `0.0.0.0` without a token or OAuth, OmniMem refuses on purpose and says so. If OAuth is on with a setting missing, the log names it.

**421 Misdirected Request through Caddy**: OmniMem doesn't trust the hostname. Set `MCP_PUBLIC_URL` (or `OAUTH_BASE_URL`) to the address clients use, and restart.

**Clients get 401**: the token after `Bearer` has to match `MCP_AUTH_TOKEN` exactly.

**Caddy can't get a certificate**: ports 80 and 443 must be open and the DNS record must point at the instance. `journalctl -u caddy` says what went wrong.

**High latency from far away**: recall embeds your query on the server, so a server in `us-east-1` feels slower from Europe. Launch it in a region near you.
