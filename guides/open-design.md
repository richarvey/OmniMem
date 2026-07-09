# Connecting OmniMem to Open Design

[Open Design](https://opendesign.ai) is a desktop application that can talk to remote MCP servers over OAuth 2.1. You can connect it to OmniMem so its agent shares the same persistent memory, project context, and graveyard as your other tools.

Unlike claude.ai, Open Design is a **native/desktop client**, so it authorises as a *public* OAuth client: it uses PKCE with no client secret, and it registers a loopback redirect URI (`http://127.0.0.1:<port>/api/mcp/oauth/callback`) on an ephemeral port. It also runs a fresh dynamic client registration each time it connects.

> [!IMPORTANT]
> This needs **OmniMem 5.5.1 or newer**. Earlier versions stamped a `client_secret` onto every registered client, including public ones, which made the token exchange fail with `invalid_client` / "Client secret is required". See [Troubleshooting](#troubleshooting) if you hit that.

> [!NOTE]
> Requires OAuth 2.1 enabled and Streamable HTTP transport (`MCP_TRANSPORT=http`). Your OmniMem instance must be reachable over HTTPS from wherever Open Design runs.

## Prerequisites

- **OmniMem 5.5.1+** running via Docker Compose with public HTTPS access (via reverse proxy or tunnel)
- **`MCP_TRANSPORT=http`** set in your `.env` (Streamable HTTP is required)
- **OAuth 2.1 enabled** in your `.env` (see below)
- **Open Design** desktop app installed

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
| `OAUTH_BASE_URL` | The externally-reachable HTTPS URL of your OmniMem instance. This is what Open Design connects to, and OmniMem allows this hostname and origin through its Host/Origin guard automatically |
| `OAUTH_ADMIN_USER` | Username for the single admin account |
| `OAUTH_ADMIN_PASSWORD` | Password for the admin account. Use something strong, this protects your entire memory store |

Restart the MCP server to pick up the changes:

```bash
docker compose restart mcp_server
```

Verify the discovery endpoint responds:

```bash
curl https://mcp.yourdomain.com/.well-known/oauth-authorization-server
```

You should get JSON with `authorization_endpoint`, `token_endpoint`, and `registration_endpoint` fields. A `421` here means the hostname isn't allowlisted, see [Troubleshooting](#troubleshooting).

## Step 2: Expose OmniMem over HTTPS

Open Design connects over HTTPS, so OmniMem needs a public TLS endpoint. A minimal Caddy config:

```
mcp.yourdomain.com {
    reverse_proxy localhost:8765
}
```

Or Traefik labels:

```yaml
labels:
  - "traefik.enable=true"
  - "traefik.http.routers.omnimem.rule=Host(`mcp.yourdomain.com`)"
  - "traefik.http.routers.omnimem.tls.certresolver=letsencrypt"
  - "traefik.http.services.omnimem.loadbalancer.server.port=8765"
```

A Tailscale funnel works too. See `docs/reverse-proxy.md` for full examples.

> [!NOTE]
> Most reverse proxies and tunnels terminate TLS and forward to OmniMem over plain http. FastMCP 3.x guards the browser `Origin` header, and OmniMem trusts the `OAUTH_BASE_URL` origin automatically so this is handled for you. If you serve under an extra hostname, add it via `MCP_ALLOWED_HOSTS` / `MCP_ALLOWED_ORIGINS`.

## Step 3: Add OmniMem in Open Design

1. Open **Open Design** and go to its **Settings** (or **Integrations** / **MCP Servers** section, the exact label depends on your version)
2. Choose to add a **remote MCP server** / **custom connector**
3. Enter your OmniMem MCP URL, including the `/mcp` path:

   ```
   https://mcp.yourdomain.com/mcp
   ```
4. Confirm/connect

Open Design then handles the OAuth flow itself:
- Discovers the endpoints via `/.well-known/oauth-authorization-server`
- Registers itself as a public OAuth client (dynamic client registration, no secret)
- Opens your browser to the OmniMem login page
- Completes the PKCE token exchange and stores the tokens locally

## Step 4: Sign in

When Open Design opens the OmniMem login page in your browser:

1. Enter your `OAUTH_ADMIN_USER` and `OAUTH_ADMIN_PASSWORD`
2. Click **Sign in**
3. Your browser redirects back to Open Design's local callback (`http://127.0.0.1:<port>/...`) and the connection completes

OmniMem tools are now available to the Open Design agent.

## How it works

Open Design uses the OAuth 2.1 authorisation code flow with PKCE, as a public client with **no client secret**:

```
Open Design                  OmniMem
   |                            |
   |  GET /.well-known/oauth-   |
   |  authorization-server      |
   |--------------------------->|  Discovery
   |  <metadata>                |
   |<---------------------------|
   |                            |
   |  POST /register            |
   |  token_endpoint_auth_      |
   |  method: "none"            |
   |--------------------------->|  Dynamic registration (public client, no secret)
   |  <client_id>               |
   |<---------------------------|
   |                            |
   |  GET /authorize            |
   |  ?code_challenge=...       |
   |  &redirect_uri=            |
   |   http://127.0.0.1:PORT/.. |
   |--------------------------->|  Redirect to login page
   |  302 -> /oauth/login       |
   |<---------------------------|
   |                            |
   |  [User signs in]           |
   |  POST /oauth/login         |
   |--------------------------->|  Verify credentials
   |  302 -> 127.0.0.1:PORT?    |
   |         code=...           |
   |<---------------------------|
   |                            |
   |  POST /token               |
   |  code + code_verifier      |
   |  (NO client_secret)        |
   |--------------------------->|  Token exchange, PKCE verified
   |  <access_token, refresh>   |
   |<---------------------------|
   |                            |
   |  POST /mcp                 |
   |  Authorization: Bearer ... |
   |--------------------------->|  MCP requests
   |  <tool results>            |
   |<---------------------------|
```

Because it is a public client, OmniMem does not issue or require a `client_secret`. The `code_verifier` (PKCE) is what proves the token request came from the same client that started the flow. Access tokens last 1 hour and refresh automatically using the refresh token (30 days by default, `OAUTH_REFRESH_MAX_DAYS`), with the same rotation grace window used for claude.ai.

## Using both OAuth and bearer tokens

OAuth runs alongside bearer token auth. If you also set `MCP_AUTH_TOKEN`, local clients (Claude Code, etc.) can keep using bearer tokens while Open Design uses OAuth. OmniMem's `MultiAuth` layer tries OAuth first, then the bearer verifier.

## Troubleshooting

| Problem | Solution |
|---------|----------|
| `invalid_client` / "Client secret is required" at the token step (after signing in) | You're on OmniMem older than 5.5.1, which wrongly assigned a secret to public clients. Upgrade to 5.5.1+. If a broken registration is cached, it clears itself on the next connect since Open Design re-registers each time |
| Discovery / everything returns `421 Misdirected Request` | FastMCP 3.x's Host guard doesn't allow your public hostname. OmniMem allows the `OAUTH_BASE_URL` host automatically, so confirm it's set correctly. For extra hostnames use `MCP_ALLOWED_HOSTS` |
| `403 Forbidden Origin` when submitting the login form | The proxy terminates TLS and forwards over http, so the origin scheme mismatches. OmniMem trusts the `OAUTH_BASE_URL` origin automatically; if you serve under another origin add it via `MCP_ALLOWED_ORIGINS` |
| Discovery endpoint returns 404 | Ensure `OAUTH_ENABLED=true` is set and the MCP server was restarted. Check `docker compose logs mcp_server` |
| Login works but tools don't appear | Confirm `MCP_TRANSPORT=http`. Streamable HTTP is required, not SSE |
| Browser redirect fails / "connection refused" at `127.0.0.1:<port>` | Open Design's local callback listener wasn't reachable. Make sure the app is still running and no firewall is blocking loopback, then retry the connection |
| "Invalid or expired session" on the login page | The authorisation session timed out (5 minutes). Reconnect from Open Design to start a fresh flow |
| "Too many failed attempts" on the login page | The per-IP login rate limit tripped (`OAUTH_LOGIN_MAX_ATTEMPTS` in `OAUTH_LOGIN_WINDOW_SECONDS`). Wait for the window to pass |
