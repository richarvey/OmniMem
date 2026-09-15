# Setting Up OmniMem on Linux with Tailscale Funnel

This is the easy route to connecting OmniMem to claude.ai and other OAuth clients that need a public HTTPS address. Tailscale Funnel gives you one with no domain, no port forwarding, no firewall rules and no certificates to renew. It works from a cloud VM, a home server behind NAT, or a Raspberry Pi under your desk.

The plan: install the OmniMem `.deb`, switch on OAuth, and point a Funnel at it.

---

## Why Tailscale Funnel?

Funnel gives you a stable `https://your-machine.tailnet-name.ts.net` address that routes to a local port. Tailscale terminates TLS for you, so traffic arrives at OmniMem over plain http on localhost.

OmniMem is happy with that. It trusts the host and origin of `OAUTH_BASE_URL`, and it accepts a browser origin matching its own host whatever the scheme, so the login page isn't refused just because TLS stopped at Tailscale.

---

## What you need

- A Linux machine running Debian, Ubuntu, Raspberry Pi OS (64-bit), Fedora or similar
- A couple of GB of RAM. OmniMem uses a few hundred MB, mostly the embedding model
- A Tailscale account; the free tier is fine ([tailscale.com](https://tailscale.com))
- An Anthropic API key if you want the Claude-powered extras (optional)

---

## Step 1: Install OmniMem

> [!NOTE]
> Coming with 7.0.0. Until the first release you can build from source (see [Build from source](../docs/quick-start.md#build-from-source)).

Download the package for your machine from the [releases page](https://code.squarecows.com/ric/omnimem/releases). On Debian, Ubuntu or Raspberry Pi OS:

```bash
sudo apt install ./omnimem_<version>_amd64.deb     # or _arm64.deb
```

On Fedora and friends:

```bash
sudo dnf install ./omnimem-<version>.x86_64.rpm    # or .aarch64.rpm
```

You get a systemd service (`omnimem.service`, running `omnimem serve` as the `omnimem` user), configuration in `/etc/omnimem/omnimem.env`, and data in `/var/lib/omnimem`.

Don't start it yet, it needs the Tailscale address first.

---

## Step 2: Install Tailscale

```bash
curl -fsSL https://tailscale.com/install.sh | sh
sudo tailscale up
```

`tailscale up` prints a URL; open it to log in and add the machine to your tailnet. Check it worked:

```bash
tailscale status
```

### Note your Tailscale hostname

```bash
tailscale status --self --json | jq -r '.Self.DNSName' | sed 's/\.$//'
```

Something like `my-server.tail12345.ts.net`. You'll need it in step 4.

---

## Step 3: Enable Funnel

Funnel has to be allowed in your tailnet policy. In the admin console at [login.tailscale.com/admin/acls](https://login.tailscale.com/admin/acls), make sure there's a `nodeAttrs` entry like this:

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

That lets every member of your tailnet use Funnel. Narrow the target if you prefer.

Then point Funnel at OmniMem's port, in the background so it survives reboots:

```bash
sudo tailscale funnel --bg 8765
tailscale funnel status
```

You should see `https://your-machine.tailnet-name.ts.net` forwarding to `127.0.0.1:8765`. The public address has no port number; Funnel serves it on 443.

---

## Step 4: Configure OmniMem

```bash
sudo nano /etc/omnimem/omnimem.env
```

### OAuth, for claude.ai

```bash
OAUTH_ENABLED=true
OAUTH_BASE_URL=https://your-machine.tailnet-name.ts.net
OAUTH_ADMIN_USER=admin
OAUTH_ADMIN_PASSWORD=pick-a-strong-password-here
```

Use your real Tailscale hostname from step 2, with no trailing slash and no port.

### An access token, for everything else

Claude Code, Cursor and the other local agents are simpler with a token:

```bash
MCP_AUTH_TOKEN=paste-a-long-random-token-here
```

Generate one with `openssl rand -hex 32`. `/mcp` accepts the token or an OAuth sign-in, whichever the client sends.

### The rest

```bash
ANTHROPIC_API_KEY=sk-ant-your-key-here    # optional
```

Leave `MCP_HOST` at `127.0.0.1`. Funnel connects over localhost, so OmniMem never needs to listen anywhere else.

Optional OAuth tuning, with the defaults:

```bash
# OAUTH_REFRESH_MAX_DAYS=30          # how long before you sign in again (at most 90)
# OAUTH_REFRESH_GRACE_SECONDS=120    # how long a replaced refresh token keeps working
# OAUTH_LOGIN_MAX_ATTEMPTS=10        # failed logins from one address before it's refused
# OAUTH_LOGIN_WINDOW_SECONDS=900     # for this long
```

One thing about that login limit: through Funnel every request reaches OmniMem from localhost, so the limit applies to everyone at once rather than per visitor. Fine for a personal server; just don't fat-finger your password ten times in a row.

---

## Step 5: Start it

```bash
sudo systemctl enable --now omnimem
journalctl -u omnimem -f
```

The first start downloads the embedding model. If OAuth is on but a setting is missing, OmniMem refuses to start and the log names the culprit, which beats finding out later from a vague claude.ai error.

---

## Step 6: Check the Funnel end to end

From any machine, not just the server:

```bash
curl https://your-machine.tailnet-name.ts.net/healthz
curl https://your-machine.tailnet-name.ts.net/.well-known/oauth-authorization-server
```

The first should answer `{"status": "ok"}`, the second a JSON document listing the OAuth endpoints on your `ts.net` address.

---

## Step 7: Connect claude.ai

The whole reason we're here.

1. Go to [claude.ai](https://claude.ai)
2. Open **Settings → Connectors**
3. Add a custom connector with the URL:

```
https://your-machine.tailnet-name.ts.net/mcp
```

4. claude.ai sends you to OmniMem's login page. Sign in with the admin username and password from step 4
5. You land back in claude.ai with OmniMem connected

Tokens refresh on their own; after 30 days (`OAUTH_REFRESH_MAX_DAYS`) you sign in again. The full story is in [the claude.ai guide](claude-ai.md).

---

## Step 8: Connect Claude Code (optional)

OmniMem only listens on localhost in this setup, so the server's `100.x.x.x` tailnet address won't reach it directly. You've got two options:

- **Use the Funnel address**, which works from anywhere:

  ```bash
  claude mcp add --transport http omnimem https://your-machine.tailnet-name.ts.net/mcp \
    --header "Authorization: Bearer your-token-here" \
    --scope user
  ```

- **Or keep it tailnet-only** with `tailscale serve` on a second port, which only your own devices can reach. Funnel and Serve can't share the same port, so pick another:

  ```bash
  sudo tailscale serve --bg --https=8443 http://127.0.0.1:8765
  ```

  and connect to `https://your-machine.tailnet-name.ts.net:8443/mcp`.

Either way, allow the tools in `~/.claude/settings.json`:

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

## Step 9: RSS feeds (optional)

```bash
sudo -u omnimem nano /var/lib/omnimem/feeds.yml
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

Copy them somewhere off the machine. To update, install the new package over the old one and restart; the database migrates itself:

```bash
sudo apt install ./omnimem_<new-version>_amd64.deb
sudo systemctl restart omnimem
```

---

## Funnel vs Serve

They're easy to mix up:

- **Tailscale Serve** exposes a local port to devices on your tailnet only. Your laptop and phone can reach it; the internet can't.
- **Tailscale Funnel** exposes a local port to the whole internet on a `ts.net` HTTPS address. claude.ai's servers need this, because they aren't on your tailnet.

With OmniMem 7 there's only one port to think about. The settings panel lives in the desktop app, not on a port, so there's no dashboard to hide behind Serve any more.

---

## Coming from 6.x

1. On 6.x, call `dump_to_file` and copy the JSON file across
2. Import it:

```bash
sudo -u omnimem omnimem --db /var/lib/omnimem/omnimem.db import backup.json
sudo systemctl restart omnimem
```

The import re-embeds every memory at roughly 8 ms each. OAuth clients registered with 6.x aren't carried across, so remove the claude.ai connector and add it again. Drop `MCP_TRANSPORT` and `WEB_UI_AUTH_TOKEN` from your old settings; neither exists any more.

---

## Troubleshooting

**"Funnel not available"**: Funnel isn't allowed in your tailnet policy. Add the `funnel` nodeAttr (step 3).

**OmniMem won't start after switching on OAuth**: `journalctl -u omnimem -e`. A missing `OAUTH_BASE_URL`, username or password is named in the log, as is a base URL that isn't https.

**The login page shows 421 Misdirected Request**: `OAUTH_BASE_URL` doesn't match the address you're using. It must be exactly `https://your-machine.tailnet-name.ts.net`, no trailing slash, no port.

**The login page shows 403 Forbidden Origin**: you're reaching OmniMem under a hostname it doesn't know, a custom domain for instance. Add it to `MCP_ALLOWED_HOSTS` and `MCP_ALLOWED_ORIGINS`.

**claude.ai says the connection failed after signing in**: check OmniMem is running (`systemctl status omnimem`) and the Funnel is up (`tailscale funnel status`), and that the connector URL ends in `/mcp`.

**Signed out after a month**: that's `OAUTH_REFRESH_MAX_DAYS` doing its job. Raise it, up to 90, if you'd rather sign in less often.

**Want a custom domain instead of ts.net?** Funnel only does TLS for `ts.net` names, so you'd need your own proxy, Caddy for instance, in front. At that point the [AWS guide](omnimem-setup-aws-linux.md)'s Caddy section is the better fit.
