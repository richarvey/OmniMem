# Setting Up OmniMem on Linux with a Cloudflare Tunnel

This guide puts OmniMem on a permanent address on your own domain, say `https://mcp.example.com`, through a named Cloudflare Tunnel. It isn't a quick tunnel: those hand out a new `trycloudflare.com` name every time `cloudflared` restarts, so claude.ai loses the connector and you're back to editing settings. A named tunnel keeps one DNS record pointing at it for good.

Like Tailscale Funnel, it needs no open ports, no port forwarding and no certificates to renew. `cloudflared` dials out to Cloudflare, Cloudflare terminates TLS, and requests reach OmniMem over plain http on localhost. It works the same from a cloud VM, a home server behind NAT or a Raspberry Pi.

The plan: install the OmniMem `.deb`, create a tunnel, give it a hostname, switch on OAuth.

---

## Why a Cloudflare Tunnel?

- **Your own domain.** Funnel only does `ts.net` names; here you choose the name.
- **Mistral's Le Chat can reach it.** In testing, Le Chat never got a single request through to a Funnel address, while a Cloudflare address worked first time. claude.ai is happy with either.
- **Nothing listens on the internet.** OmniMem stays on `127.0.0.1`, and the only way in is the tunnel.

The cost is that your domain has to use Cloudflare's nameservers, and Cloudflare sees the traffic in the clear, since it terminates TLS. If that second point bothers you, the [Tailscale guide](omnimem-setup-linux-tailscale.md) or Caddy on a cloud machine ([AWS](omnimem-setup-aws-linux.md), [Google Cloud](omnimem-setup-gcp-linux.md)) keep TLS on your own box.

---

## What you need

- A Linux machine running Debian, Ubuntu, Raspberry Pi OS (64-bit), Fedora or similar
- A couple of GB of RAM. OmniMem uses a few hundred MB, mostly the embedding model
- A Cloudflare account, the free plan is fine, with a domain added to it (its nameservers moved to Cloudflare)
- An Anthropic API key if you want the Claude-powered extras (optional)

Throughout, `mcp.example.com` stands for the hostname you'll use. Swap in your own.

---

## Step 1: Install OmniMem

Download the package for your machine from the [releases page](https://code.squarecows.com/ric/omnimem/releases). On Debian, Ubuntu or Raspberry Pi OS:

```bash
sudo apt install ./omnimem_<version>_amd64.deb     # or _arm64.deb
```

On Fedora and friends:

```bash
sudo dnf install ./omnimem-<version>.x86_64.rpm    # or .aarch64.rpm
```

You get a systemd service (`omnimem.service`, running `omnimem serve` as the `omnimem` user), configuration in `/etc/omnimem/omnimem.env`, and data in `/var/lib/omnimem`.

Don't start it yet, it needs to know its address first.

---

## Step 2: Install cloudflared

On Debian, Ubuntu and Raspberry Pi OS, from Cloudflare's own repository:

```bash
sudo mkdir -p --mode=0755 /usr/share/keyrings
curl -fsSL https://pkg.cloudflare.com/cloudflare-main.gpg | sudo tee /usr/share/keyrings/cloudflare-main.gpg >/dev/null
echo "deb [signed-by=/usr/share/keyrings/cloudflare-main.gpg] https://pkg.cloudflare.com/cloudflared any main" | sudo tee /etc/apt/sources.list.d/cloudflared.list
sudo apt-get update && sudo apt-get install cloudflared
```

For Fedora and others, Cloudflare's [downloads page](https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/) has an `.rpm` and plain binaries.

```bash
cloudflared --version
```

---

## Step 3: Create the tunnel

There are two ways to manage a named tunnel: from the Cloudflare dashboard, or from a config file on the machine. Both give you a permanent hostname. I use the config file here because the whole setup then lives in two files you can read and back up. The dashboard way is at the end of this step if you'd rather click.

### Log in

```bash
cloudflared tunnel login
```

It prints a URL. Open it, sign in to Cloudflare and pick the domain you want to use. That writes an account certificate to `~/.cloudflared/cert.pem`, which lets `cloudflared` create tunnels and DNS records in that zone.

### Create it

```bash
cloudflared tunnel create omnimem
```

This prints the tunnel's UUID and writes its credentials to `~/.cloudflared/<UUID>.json`. That file is what lets this machine run this tunnel, so treat it like a password. Check it's there:

```bash
cloudflared tunnel list
```

### Give it a hostname

```bash
cloudflared tunnel route dns omnimem mcp.example.com
```

That creates a proxied CNAME, `mcp.example.com` pointing at `<UUID>.cfargotunnel.com`, in your zone. This is the persistent bit: the record stays put however often the tunnel or the machine restarts.

### Write the config

The service will run as root, so put everything under `/etc/cloudflared` rather than in your home folder:

```bash
sudo mkdir -p /etc/cloudflared
sudo cp ~/.cloudflared/<UUID>.json /etc/cloudflared/
sudo chmod 600 /etc/cloudflared/<UUID>.json
sudo nano /etc/cloudflared/config.yml
```

```yaml
tunnel: <UUID>
credentials-file: /etc/cloudflared/<UUID>.json

ingress:
  - hostname: mcp.example.com
    service: http://127.0.0.1:8765
  - service: http_status:404
```

The last rule is the catch-all `cloudflared` insists on: anything that isn't for your hostname gets a 404. Pass the whole path through, as here. OmniMem needs `/mcp`, `/healthz`, `/.well-known/...`, `/authorize`, `/token` and the rest to arrive, so don't add a `path:` that only matches `/mcp`.

Check the file before going further:

```bash
sudo cloudflared tunnel --config /etc/cloudflared/config.yml ingress validate
sudo cloudflared tunnel --config /etc/cloudflared/config.yml ingress rule https://mcp.example.com/mcp
```

The second should name the `http://127.0.0.1:8765` rule.

### Run it as a service

```bash
sudo cloudflared --config /etc/cloudflared/config.yml service install
sudo systemctl enable --now cloudflared
systemctl status cloudflared
```

From now on the tunnel comes up at boot. You can delete `~/.cloudflared/cert.pem` once you're happy, it's only needed to create tunnels and DNS records, not to run them.

### Or: from the dashboard

In the Cloudflare dashboard, go to **Zero Trust → Networks → Tunnels**, create a tunnel of type **Cloudflared**, and name it `omnimem`. It shows an install command with a token in it:

```bash
sudo cloudflared service install <token>
```

Run that on the machine. Then, on the tunnel's **Public Hostname** tab, add `mcp.example.com` with the service type **HTTP** and URL `127.0.0.1:8765`. Cloudflare creates the DNS record for you. The token is the tunnel's credentials, so keep it out of screenshots and chat logs.

---

## Step 4: Configure OmniMem

```bash
sudo nano /etc/omnimem/omnimem.env
```

### OAuth, for claude.ai

```bash
OAUTH_ENABLED=true
OAUTH_BASE_URL=https://mcp.example.com
OAUTH_ADMIN_USER=admin
OAUTH_ADMIN_PASSWORD=pick-a-strong-password-here
```

`https://`, your hostname, no trailing slash, no port. OmniMem checks every request's `Host` against it, and `cloudflared` passes the original `mcp.example.com` through, so the two match.

### An access token, for everything else

Claude Code, Cursor and the other local agents are simpler with a token:

```bash
MCP_AUTH_TOKEN=paste-a-long-random-token-here
```

Generate one with `openssl rand -hex 32`. `/mcp` takes the token or an OAuth sign-in, whichever the client sends.

### Mistral's Le Chat (optional)

Le Chat calls `/mcp` from its own web page, so it needs its origin allowed:

```bash
MCP_ALLOWED_ORIGINS=https://chat.mistral.ai
```

### The rest

```bash
ANTHROPIC_API_KEY=sk-ant-your-key-here    # optional
```

Leave `MCP_HOST` at `127.0.0.1`. `cloudflared` connects over localhost, so OmniMem never needs to listen anywhere else.

Optional OAuth tuning, with the defaults:

```bash
# OAUTH_REFRESH_MAX_DAYS=30          # how long before you sign in again (at most 90)
# OAUTH_REFRESH_GRACE_SECONDS=120    # how long a replaced refresh token keeps working
# OAUTH_LOGIN_MAX_ATTEMPTS=10        # failed logins from one address before it's refused
# OAUTH_LOGIN_WINDOW_SECONDS=900     # for this long
```

About that login limit: through the tunnel every request reaches OmniMem from `127.0.0.1`, so the limit counts everyone together rather than per visitor. Ten wrong passwords inside 15 minutes, from anyone, shuts the login page for a while. Fine for a personal server, and it's also what stops someone guessing your password from the internet.

---

## Step 5: Start it

```bash
sudo systemctl enable --now omnimem
journalctl -u omnimem -f
```

The first start downloads the embedding model, about 90 MB. If OAuth is on but a setting is missing, OmniMem refuses to start and the log says which, which beats finding out later from a vague claude.ai error.

---

## Step 6: Check it end to end

From any machine, not just the server:

```bash
curl https://mcp.example.com/healthz
curl https://mcp.example.com/.well-known/oauth-authorization-server
curl -i -X POST https://mcp.example.com/mcp
```

The first should answer `{"status": "ok"}`, the second a JSON document listing the OAuth endpoints on `https://mcp.example.com`, and the third `401` with a `WWW-Authenticate` header naming your hostname. An empty `401` body is correct: it's asking for a sign-in.

If the first one fails, `cloudflared` can't reach OmniMem or isn't running (`systemctl status cloudflared`). A `502` from Cloudflare usually means OmniMem itself isn't up yet.

---

## Step 7: Connect claude.ai

1. Go to [claude.ai](https://claude.ai)
2. Open **Settings → Connectors**
3. Add a custom connector with the URL:

```
https://mcp.example.com/mcp
```

4. claude.ai sends you to OmniMem's login page. Sign in with the admin username and password from step 4
5. You land back in claude.ai with OmniMem connected

Tokens refresh on their own, and because the address never changes, the connector keeps working across reboots and tunnel restarts. After 30 days (`OAUTH_REFRESH_MAX_DAYS`) you sign in again. More in [the claude.ai guide](claude-ai.md).

---

## Step 8: Connect Claude Code (optional)

The same address works from anywhere:

```bash
claude mcp add --transport http omnimem https://mcp.example.com/mcp \
  --header "Authorization: Bearer your-token-here" \
  --scope user
```

and allow the tools in `~/.claude/settings.json`:

```json
{
  "permissions": {
    "allow": [
      "mcp__omnimem__*"
    ]
  }
}
```

On the server itself you can skip Cloudflare and use `http://127.0.0.1:8765/mcp` with the same token.

---

## Cloudflare settings that get in the way

Most of Cloudflare's defaults are fine. A few of its security features aren't, because they answer some requests with a challenge page, and an MCP client or OAuth server can't solve a challenge. It just sees HTML where it expected JSON and gives up.

- **Bot Fight Mode** and **Block AI bots** (under **Security → Bots**) can catch claude.ai's servers, which is exactly who needs to reach `/mcp` and `/token`. If claude.ai connects and then fails, try with these off for the zone, or add a WAF custom rule that skips them for `mcp.example.com`.
- **Managed challenges and "I'm Under Attack" mode** do the same. Keep them off this hostname.
- **Cloudflare Access** in front of the hostname will stop claude.ai cold: it can't sign in to Access. It's the same reason basic auth at a proxy doesn't work. OmniMem's own OAuth is the gate here.
- **Caching**: leave it alone. Cloudflare doesn't cache JSON by default. OmniMem's OAuth metadata says it can be cached for an hour, varying by `Host`, which is harmless. Don't add a "cache everything" rule for this hostname.

Cloudflare doesn't need any special setting for MCP's streamed responses; `cloudflared` passes them through as they arrive.

---

## RSS feeds (optional)

```bash
sudo -u omnimem nano /var/lib/omnimem/feeds.yml
```

```yaml
feeds:
  - name: "Rust Blog"
    url: "https://blog.rust-lang.org/feed.xml"
    topics: ["rust", "programming"]
  - name: "The Cloudflare Blog"
    url: "https://blog.cloudflare.com/rss/"
    topics: ["networking", "cloudflare"]
```

OmniMem notices the change. See [RSS and knowledge](../docs/rss-knowledge.md).

---

## Backups and updates

Back up nightly with the CLI:

```bash
sudo crontab -u omnimem -e
```

```
0 2 * * * /usr/bin/omnimem --db /var/lib/omnimem/omnimem.db export /var/lib/omnimem/backups/omnimem-$(date +\%Y\%m\%d).json
```

Copy them somewhere off the machine, and keep a copy of `/etc/cloudflared` too: with the credentials file you can bring the same tunnel and hostname up on a new machine without touching DNS.

To update OmniMem, install the new package over the old one and restart; the database migrates itself:

```bash
sudo apt install ./omnimem_<new-version>_amd64.deb
sudo systemctl restart omnimem
```

`cloudflared` updates through apt with everything else.

---

## Running it in Docker instead

The same idea works with the [Docker image](docker.md) and Cloudflare's `cloudflared` image side by side. This uses a tunnel made in the dashboard (step 3's second way), with its public hostname pointing at `http://omnimem:8765`:

```yaml
services:
  omnimem:
    image: richarvey/omnimem:<version>
    environment:
      MCP_HOST: 0.0.0.0
      OAUTH_ENABLED: "true"
      OAUTH_BASE_URL: https://mcp.example.com
      OAUTH_ADMIN_USER: admin
      OAUTH_ADMIN_PASSWORD: ${OAUTH_ADMIN_PASSWORD}
      MCP_AUTH_TOKEN: ${MCP_AUTH_TOKEN}
    volumes:
      - omnimem-data:/data
    restart: unless-stopped

  cloudflared:
    image: cloudflare/cloudflared:latest
    command: tunnel --no-autoupdate run --token ${TUNNEL_TOKEN}
    depends_on:
      - omnimem
    restart: unless-stopped

volumes:
  omnimem-data:
```

Put `OAUTH_ADMIN_PASSWORD`, `MCP_AUTH_TOKEN` and `TUNNEL_TOKEN` in a `.env` file beside it. There's no `ports:` on either service: nothing is published on the host, and the tunnel is the only way in. `MCP_HOST: 0.0.0.0` is needed so the `cloudflared` container can reach OmniMem across the Compose network, and OAuth is what allows it to start that way.

---

## Troubleshooting

**`https://mcp.example.com` says the host can't be found**: the DNS record isn't there. `cloudflared tunnel route dns omnimem mcp.example.com` again, or check **DNS → Records** for a proxied CNAME to `<UUID>.cfargotunnel.com`. If `tunnel login` picked a different domain, the record went into that zone.

**Cloudflare error 1033**: Cloudflare has the hostname but no `cloudflared` connected for that tunnel. `systemctl status cloudflared` and `journalctl -u cloudflared -e`.

**Cloudflare 502 Bad Gateway**: the tunnel is up but OmniMem isn't answering on `127.0.0.1:8765`. `systemctl status omnimem`, and check the `service:` line in `config.yml`.

**OmniMem won't start after switching on OAuth**: `journalctl -u omnimem -e`. A missing `OAUTH_BASE_URL`, username or password is named in the log, as is a base URL that isn't https.

**421 Misdirected Request**: OmniMem doesn't recognise the host it was reached on. `OAUTH_BASE_URL` must be exactly `https://mcp.example.com`, no trailing slash, no port. If you set `httpHostHeader` in the ingress rule, remove it: OmniMem needs the real hostname.

**403 Forbidden Origin**: a browser-based client is calling from an origin OmniMem doesn't know. Add it to `MCP_ALLOWED_ORIGINS`, as for Le Chat in step 4.

**claude.ai fails after you sign in, or never shows the login page**: first check the end-to-end tests in step 6 from outside. If they pass, look for blocked requests under **Security → Events** in the Cloudflare dashboard; a bot or WAF rule challenging claude.ai's servers shows up there. See "Cloudflare settings that get in the way" above.

**Signed out after a month**: that's `OAUTH_REFRESH_MAX_DAYS` doing its job. Raise it, up to 90, if you'd rather sign in less often.

**Moving from a quick tunnel**: if you had `MCP_ALLOWED_HOSTS=*.trycloudflare.com` and `OAUTH_BASE_URL=...,*` for a `trycloudflare.com` address, drop them. With a fixed hostname, `OAUTH_BASE_URL=https://mcp.example.com` is all OmniMem needs, and remove and re-add the claude.ai connector on the new address.
