# Connecting OmniMem to claude.ai

claude.ai is Anthropic's web chat. It can use remote MCP servers that sign in with OAuth 2.1, which means your self-hosted memory can follow you into the browser. Same memories, same graveyard, no copying things between tools.

The catch is that claude.ai connects from Anthropic's servers, not your laptop. So OmniMem needs a public HTTPS address and OAuth switched on.

## Prerequisites

- **OmniMem 7 running** somewhere reachable from the internet over HTTPS, through a reverse proxy or a tunnel such as Tailscale Funnel
- **OAuth switched on** (below)
- A **claude.ai** plan that supports custom connectors

If you haven't got a public address yet, the [Tailscale Funnel guide](omnimem-setup-linux-tailscale.md) is the easiest way I know to get one.

## Step 1: Switch on OAuth

On a headless install, put this in `/etc/omnimem/omnimem.env` (or pass it as environment variables to the Docker image):

```bash
OAUTH_ENABLED=true
OAUTH_BASE_URL=https://mcp.yourdomain.com
OAUTH_ADMIN_USER=admin
OAUTH_ADMIN_PASSWORD=a-strong-password-here
```

On the desktop app, the same four settings live in the **OAuth** section of the settings window's Configuration page. The password goes into your OS keychain.

| Setting | What it's for |
|----------|-------------|
| `OAUTH_ENABLED` | Turns the OAuth server on |
| `OAUTH_BASE_URL` | The https address clients reach OmniMem at. Its host and origin are trusted automatically, so the Host and Origin checks let your proxy's traffic through |
| `OAUTH_ADMIN_USER` | The username the login page asks for |
| `OAUTH_ADMIN_PASSWORD` | The password the login page asks for. Make it a good one, it guards your entire memory store |

Restart OmniMem to pick it up:

```bash
sudo systemctl restart omnimem
```

If `OAUTH_ENABLED` is on but the base URL, username or password is missing, OmniMem refuses to start and says which one. (6.x used to shrug and run without OAuth, which was a lovely way to lose an evening.)

Check the discovery document answers:

```bash
curl https://mcp.yourdomain.com/.well-known/oauth-authorization-server
```

You should get JSON with `authorization_endpoint`, `token_endpoint` and `registration_endpoint` in it.

## Step 2: Put it behind HTTPS

claude.ai needs TLS. With Caddy it's about as short as config gets:

```
mcp.yourdomain.com {
    reverse_proxy localhost:8765
}
```

Everything OmniMem serves over HTTP is on that one port: `/mcp`, the OAuth routes and `/healthz`. There's no separate web UI to hide any more. See [the reverse proxy docs](../docs/reverse-proxy.md) for Traefik, nginx and friends.

## Step 3: Add OmniMem in claude.ai

1. Open [claude.ai](https://claude.ai) and go to **Settings**
2. Find **Connectors** (the label has moved around over time)
3. Add a custom connector
4. Enter your OmniMem URL, including the path: `https://mcp.yourdomain.com/mcp`
5. Click **Connect**

claude.ai then does the OAuth dance on its own: it discovers the endpoints, registers itself as a client, and opens the OmniMem login page.

## Step 4: Sign in

1. Enter your admin username and password
2. Click **Sign in**
3. You're sent back to claude.ai with the connector live

That's it. OmniMem's tools are now available in your conversations.

## How it works

It's the standard OAuth 2.1 authorisation code flow with PKCE:

```
claude.ai                    OmniMem
   |                            |
   |  GET /.well-known/oauth-   |
   |  authorization-server      |
   |--------------------------->|  Discovery
   |  <metadata>                |
   |<---------------------------|
   |                            |
   |  POST /register            |
   |--------------------------->|  Dynamic client registration
   |  <client_id, secret>       |
   |<---------------------------|
   |                            |
   |  GET /authorize            |
   |  ?code_challenge=...       |
   |--------------------------->|  Redirect to login page
   |  302 -> /oauth/login       |
   |<---------------------------|
   |                            |
   |  [You sign in]             |
   |  POST /oauth/login         |
   |--------------------------->|  Check credentials
   |  302 -> callback?code=...  |
   |<---------------------------|
   |                            |
   |  POST /token               |
   |  code + code_verifier      |
   |--------------------------->|  Token exchange
   |  <access_token, refresh>   |
   |<---------------------------|
   |                            |
   |  POST /mcp                 |
   |  Authorization: Bearer ... |
   |--------------------------->|  MCP requests
   |  <tool results>            |
   |<---------------------------|
```

Access tokens last an hour and claude.ai refreshes them itself. A refresh chain lasts 30 days by default (`OAUTH_REFRESH_MAX_DAYS`, at most 90), then you sign in again.

Refresh tokens rotate every time they're used, but the old one doesn't die the instant it rotates. For a short grace window (`OAUTH_REFRESH_GRACE_SECONDS`, 120 seconds by default) it keeps working and hands back the same new pair. claude.ai holds several connections open and can refresh the same token from more than one at once; without the grace window all but one of those would fail and you'd be signing in every hour or two.

Clients, codes and tokens are kept in OmniMem's database, so restarting OmniMem doesn't sign claude.ai out. Codes and tokens are stored only as SHA-256 hashes, so a copy of the database has nothing in it a client could use.

## OAuth and access tokens together

Switching on OAuth doesn't break your local setup. If `MCP_AUTH_TOKEN` is set too, `/mcp` accepts either:

- **claude.ai** signs in with OAuth
- **Claude Code** and other local clients keep sending the access token as a bearer header

## Security notes

- **Use a strong admin password.** It's the only account, and it opens everything
- **HTTPS isn't optional.** OAuth tokens must never travel over plain HTTP. OmniMem refuses an `OAUTH_BASE_URL` that isn't https, except for localhost
- **The login form is rate limited** per client address: `OAUTH_LOGIN_MAX_ATTEMPTS` failures (10) within `OAUTH_LOGIN_WINDOW_SECONDS` (900) and that address is refused for a while. Behind a proxy every request comes from the proxy's address, so the limit is effectively shared
- **Keep the grace window short.** A leaked old refresh token is only usable for that long. `0` gives you strict single-use rotation, at the cost of claude.ai signing you out now and then
- **`OAUTH_BASE_URL` has to match** the address clients actually use, or the redirects go somewhere odd

## Troubleshooting

| Problem | Fix |
|---------|----------|
| "Invalid or expired session" on the login page | The sign-in took longer than five minutes. Go back to claude.ai and connect again |
| Discovery returns 404 | OAuth isn't on. Check `OAUTH_ENABLED=true` and restart OmniMem |
| OmniMem won't start and says OAuth is misconfigured | Set the missing `OAUTH_BASE_URL`, `OAUTH_ADMIN_USER` or `OAUTH_ADMIN_PASSWORD`. The message names it |
| Every request gets `421 Misdirected Request` | The Host header isn't one OmniMem trusts. Check `OAUTH_BASE_URL` matches your public hostname, or add extra names to `MCP_ALLOWED_HOSTS` |
| The login form gets `403 Forbidden Origin` | The browser's origin isn't trusted. OmniMem accepts its own host whatever the scheme, so this usually means you're serving under a second hostname: add it to `MCP_ALLOWED_ORIGINS` |
| "Connection refused" from claude.ai | Your server isn't reachable from the internet. Check the proxy or tunnel and the firewall |
| "Too many failed attempts" on the login page | The rate limit tripped. Wait out the window, or raise `OAUTH_LOGIN_MAX_ATTEMPTS` |
