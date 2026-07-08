# Connecting OmniMem to claude.ai

claude.ai is Anthropic's web-based chat interface. It supports remote MCP servers via OAuth 2.1, meaning you can connect OmniMem directly from your browser without any local setup beyond a running OmniMem instance.

> [!NOTE]
> This requires OmniMem 4.0+ with OAuth 2.1 enabled and Streamable HTTP transport (`MCP_TRANSPORT=http`). Your OmniMem instance must be reachable from the public internet over HTTPS.

## Prerequisites

- **OmniMem running** via Docker Compose with public HTTPS access (via reverse proxy)
- **`MCP_TRANSPORT=http`** set in your `.env` (Streamable HTTP is required for claude.ai)
- **OAuth 2.1 enabled** in your `.env` (see below)
- A **claude.ai** account (Pro, Team, or Enterprise plan with MCP support)

## Step 1: Configure OAuth in OmniMem

Add the following to your `.env` file:

```bash
MCP_TRANSPORT=http

OAUTH_ENABLED=true
OAUTH_BASE_URL=https://mcp.yourdomain.com
OAUTH_ADMIN_USER=admin
OAUTH_ADMIN_PASSWORD=a-strong-password-here
```

| Variable | Description |
|----------|-------------|
| `OAUTH_BASE_URL` | The externally-reachable URL of your OmniMem instance. Must be HTTPS. This is what claude.ai will connect to |
| `OAUTH_ADMIN_USER` | Username for the single admin account |
| `OAUTH_ADMIN_PASSWORD` | Password for the admin account. Use something strong — this protects access to your entire memory store |

Restart the MCP server to pick up the changes:

```bash
docker compose restart mcp_server
```

Verify it's working by checking the OAuth discovery endpoint:

```bash
curl https://mcp.yourdomain.com/.well-known/oauth-authorization-server
```

You should see a JSON response with `authorization_endpoint`, `token_endpoint`, and `registration_endpoint` fields.

## Step 2: Expose OmniMem over HTTPS

claude.ai connects to your server from the public internet, so OmniMem needs to be reachable over HTTPS. If you haven't already set up a reverse proxy, here's a minimal Traefik example:

```yaml
labels:
  - "traefik.enable=true"
  - "traefik.http.routers.omnimem.rule=Host(`mcp.yourdomain.com`)"
  - "traefik.http.routers.omnimem.tls.certresolver=letsencrypt"
  - "traefik.http.services.omnimem.loadbalancer.server.port=8765"
```

Or with Caddy:

```
mcp.yourdomain.com {
    reverse_proxy localhost:8765
}
```

See `docs/reverse-proxy.md` for full examples including authentication middleware for the web UI.

## Step 3: Add OmniMem in claude.ai

1. Go to [claude.ai](https://claude.ai) and open **Settings**
2. Navigate to the **Integrations** or **MCP Servers** section
3. Click **Add Integration** (or **Add MCP Server**)
4. Enter your OmniMem URL: `https://mcp.yourdomain.com/mcp`
5. Click **Connect**

claude.ai will automatically:
- Discover the OAuth endpoints via `/.well-known/oauth-authorization-server`
- Register itself as an OAuth client
- Open a browser window for you to sign in
- Exchange the authorisation code for access tokens

## Step 4: Sign in

When claude.ai redirects you to the OmniMem login page:

1. Enter your `OAUTH_ADMIN_USER` and `OAUTH_ADMIN_PASSWORD`
2. Click **Sign in**
3. You'll be redirected back to claude.ai with an active session

That's it. OmniMem tools are now available in your claude.ai conversations.

## How it works

The connection uses the standard OAuth 2.1 authorisation code flow with PKCE:

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
   |  [User signs in]           |
   |  POST /oauth/login         |
   |--------------------------->|  Verify credentials
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

Access tokens expire after 1 hour. claude.ai automatically refreshes them using the refresh token (valid for 30 days by default, `OAUTH_REFRESH_MAX_DAYS`). Refresh tokens rotate on every use — each refresh issues a new pair — but the old token isn't invalidated the instant it rotates. It stays valid for a short grace window (`OAUTH_REFRESH_GRACE_SECONDS`, default 120s) during which replays return the same new pair. claude.ai holds several connections open and can refresh the same token from more than one at once; the grace window lets those concurrent refreshes all succeed instead of all-but-one failing and forcing you to sign in again.

## Using both OAuth and bearer tokens

OAuth does not interfere with bearer token auth. If you also have `MCP_AUTH_TOKEN` set in your `.env`, both methods work simultaneously:

- **claude.ai** authenticates via OAuth
- **Claude Code** (and other local clients) authenticates via bearer token in the `Authorization` header
- Both are verified by OmniMem's `MultiAuth` layer, which tries the OAuth provider first, then falls back to the bearer token verifier

This means your local development setup continues working exactly as before.

## Security considerations

- **Use a strong password** for `OAUTH_ADMIN_PASSWORD`. This is the only account that can authorise access to your entire memory store
- **HTTPS is mandatory**. OAuth tokens must never travel over plain HTTP. The MCP spec requires TLS for all authorisation endpoints
- **Tokens are persisted in Valkey** (with AOF enabled) with TTLs matching their lifetimes. Restarting the MCP server no longer clears active sessions, so a `docker compose restart` won't log claude.ai out. If Valkey is unreachable at startup the provider falls back to in-memory storage, which *is* cleared on restart
- **Token rotation** happens on every refresh, with a short reuse grace window (`OAUTH_REFRESH_GRACE_SECONDS`, default 120s, set `0` for strict single-use). Keeping the window small limits how long a leaked old token remains usable while still allowing claude.ai's concurrent refreshes to succeed
- **The login form is rate-limited** per client IP (`OAUTH_LOGIN_MAX_ATTEMPTS` failures per `OAUTH_LOGIN_WINDOW_SECONDS`) to slow brute-force attempts on the admin password
- **`OAUTH_BASE_URL` must match** the URL clients use to reach your server. If it does not match, redirects will fail

## Troubleshooting

| Problem | Solution |
|---------|----------|
| "Invalid or expired session" on login page | The authorisation session timed out (5 minutes). Go back to claude.ai and reconnect |
| Discovery endpoint returns 404 | Ensure `OAUTH_ENABLED=true` is set and the MCP server has been restarted. Check logs: `docker compose logs mcp_server` |
| "OAUTH_BASE_URL is missing" in logs | Set `OAUTH_BASE_URL` to your externally-reachable HTTPS URL in `.env` |
| Login works but tools don't appear | Check that `MCP_TRANSPORT=http` is set. claude.ai requires Streamable HTTP, not SSE |
| "Connection refused" from claude.ai | Your server must be reachable from the public internet. Check your reverse proxy and firewall rules |
| Tools stop working after a server restart | Tokens persist in Valkey (with AOF), so a restart should not log you out. If it does, Valkey likely fell back to in-memory storage — check `docker compose logs mcp_server` for "falling back to in-memory storage" and confirm Valkey is healthy |
| Prompted to re-authenticate every hour or two | You're on a version before the refresh-token grace window, or `OAUTH_REFRESH_GRACE_SECONDS=0`. Concurrent refreshes from claude.ai were racing to `invalid_grant`. Upgrade to 5.3.0+ and leave the grace window at its default |
| "Too many failed attempts" on the login page | The per-IP login rate limit tripped (`OAUTH_LOGIN_MAX_ATTEMPTS` in `OAUTH_LOGIN_WINDOW_SECONDS`). Wait for the window to pass, or raise the limit |
