# Connecting OmniMem to Open Design

[Open Design](https://opendesign.ai) is a desktop app that can use remote MCP servers over OAuth 2.1. Connect it to OmniMem and its agent shares the same memories, project context and graveyard as the rest of your tools.

Unlike claude.ai, Open Design is a **native desktop client**, so it signs in as a *public* OAuth client. It uses PKCE with no client secret, registers a loopback redirect (`http://127.0.0.1:<port>/api/mcp/oauth/callback`) on a random port, and registers itself afresh every time it connects. OmniMem 7 handles all of that out of the box: public clients get no secret and prove themselves with PKCE alone.

## Prerequisites

- **OmniMem 7** reachable over HTTPS from wherever Open Design runs, through a reverse proxy or a tunnel
- **OAuth switched on** (below)
- **Open Design** installed

## Step 1: Switch on OAuth

On a headless install, put this in `/etc/omnimem/omnimem.env` (or pass it to the Docker image as environment variables):

```bash
OAUTH_ENABLED=true
OAUTH_BASE_URL=https://mcp.yourdomain.com
OAUTH_ADMIN_USER=admin
OAUTH_ADMIN_PASSWORD=a-strong-password-here
```

On the desktop app these live in the **OAuth** section of the Configuration page, with the password kept in your OS keychain.

| Setting | What it's for |
|----------|-------------|
| `OAUTH_BASE_URL` | The https address Open Design connects to. OmniMem trusts this host and origin automatically |
| `OAUTH_ADMIN_USER` | The username the login page asks for |
| `OAUTH_ADMIN_PASSWORD` | The password the login page asks for. Make it strong, it guards your whole memory store |

Restart OmniMem (`sudo systemctl restart omnimem` on a server) and check discovery answers:

```bash
curl https://mcp.yourdomain.com/.well-known/oauth-authorization-server
```

You should get JSON with `authorization_endpoint`, `token_endpoint` and `registration_endpoint`. A `421` means the hostname isn't trusted, see [Troubleshooting](#troubleshooting).

## Step 2: Put it behind HTTPS

A minimal Caddy config does it:

```
mcp.yourdomain.com {
    reverse_proxy localhost:8765
}
```

A Tailscale Funnel works too. See [the reverse proxy docs](../docs/reverse-proxy.md) for more.

Most proxies and tunnels terminate TLS and forward to OmniMem over plain http. That's fine: OmniMem accepts a browser origin that matches its own host whatever the scheme, so the login form isn't refused. If you serve under an extra hostname, add it with `MCP_ALLOWED_HOSTS` and `MCP_ALLOWED_ORIGINS`.

## Step 3: Add OmniMem in Open Design

1. Open **Open Design** and go to its **Settings** (the MCP section's label depends on your version)
2. Add a **remote MCP server** or **custom connector**
3. Enter your OmniMem URL, including the `/mcp` path:

   ```
   https://mcp.yourdomain.com/mcp
   ```
4. Connect

Open Design takes it from there: it discovers the endpoints, registers as a public client, opens the OmniMem login page in your browser, and finishes the PKCE token exchange.

## Step 4: Sign in

1. Enter your admin username and password
2. Click **Sign in**
3. Your browser is sent back to Open Design's local callback and the connection completes

The OmniMem tools are now available to the Open Design agent.

## How it works

The OAuth 2.1 authorisation code flow with PKCE, as a public client with **no client secret**:

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
   |--------------------------->|  Registration (public client, no secret)
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
   |  [You sign in]             |
   |  POST /oauth/login         |
   |--------------------------->|  Check credentials
   |  302 -> 127.0.0.1:PORT?    |
   |         code=...           |
   |<---------------------------|
   |                            |
   |  POST /token               |
   |  code + code_verifier      |
   |  (no client_secret)        |
   |--------------------------->|  Token exchange, PKCE checked
   |  <access_token, refresh>   |
   |<---------------------------|
   |                            |
   |  POST /mcp                 |
   |  Authorization: Bearer ... |
   |--------------------------->|  MCP requests
   |  <tool results>            |
   |<---------------------------|
```

Because it's a public client, OmniMem neither issues nor asks for a `client_secret`. The `code_verifier` is what proves the token request came from whoever started the flow. Access tokens last an hour and refresh automatically; a refresh chain lasts 30 days by default (`OAUTH_REFRESH_MAX_DAYS`), with the same rotation grace window claude.ai relies on.

## OAuth and access tokens together

If you also set `MCP_AUTH_TOKEN`, local clients like Claude Code keep using the access token while Open Design uses OAuth. `/mcp` accepts either.

## Troubleshooting

| Problem | Fix |
|---------|----------|
| Every request gets `421 Misdirected Request` | Your public hostname isn't trusted. Check `OAUTH_BASE_URL` matches it, or add extra names to `MCP_ALLOWED_HOSTS` |
| `403 Forbidden Origin` when submitting the login form | You're serving under an origin OmniMem doesn't know about. Add it to `MCP_ALLOWED_ORIGINS` |
| Discovery returns 404 | OAuth isn't on. Set `OAUTH_ENABLED=true` and restart OmniMem |
| OmniMem refuses to start after switching on OAuth | The base URL, username or password is missing, or the base URL isn't https. The error says which |
| The browser can't reach `127.0.0.1:<port>` after signing in | Open Design's local callback listener wasn't there. Make sure the app is still running and nothing blocks loopback, then connect again |
| "Invalid or expired session" on the login page | The sign-in took more than five minutes. Reconnect from Open Design |
| "Too many failed attempts" on the login page | The login rate limit tripped (`OAUTH_LOGIN_MAX_ATTEMPTS` within `OAUTH_LOGIN_WINDOW_SECONDS`). Wait out the window |
