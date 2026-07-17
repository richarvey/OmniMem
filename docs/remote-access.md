# Using It from Multiple Machines

Expose `MCP_PORT` through your reverse proxy. Traefik example:

```yaml
labels:
  - "traefik.enable=true"
  - "traefik.http.routers.omnimem.rule=Host(`omnimem.yourdomain.com`)"
  - "traefik.http.routers.omnimem.tls.certresolver=letsencrypt"
  - "traefik.http.services.omnimem.loadbalancer.server.port=8765"
```

Update the MCP config URL to `https://omnimem.yourdomain.com/sse` (or `.../mcp` if using Streamable HTTP) and every machine you work from shares the same memory, the same graveyard, and the same project context. See the [connection guides](../guides/) for how to configure each coding agent.

You can expose the web UI the same way — add a route for `WEB_PORT`. With `OAUTH_ADMIN_USER` and `OAUTH_ADMIN_PASSWORD` set, the dashboard's own login page guards it with the same credentials as the OAuth flow; [reverse-proxy.md](reverse-proxy.md) has Traefik and Caddy examples if you'd rather (or additionally) gate it at the proxy.

## Security checklist

Strong `VALKEY_PASSWORD` (Compose now refuses to start if it's empty), set `MCP_AUTH_TOKEN` in your `.env` (with OAuth credentials configured the web dashboard requires a login automatically; add `WEB_UI_AUTH_TOKEN` if scripts need bearer access), TLS on the proxy if exposing publicly, and keep the Valkey port off the public internet. Bearer tokens are compared in constant time, the MCP server won't start unauthenticated on a non-loopback address, backup filenames are validated against path traversal, and uploaded/restored backups and fetched RSS pages are size-capped.

## OAuth 2.1 for claude.ai

If you want to connect from **claude.ai** (or any MCP client that uses OAuth), OmniMem has optional built-in OAuth 2.1 support. Enable it in your `.env`:

```bash
OAUTH_ENABLED=true
OAUTH_BASE_URL=https://mcp.yourdomain.com   # externally-reachable URL
OAUTH_ADMIN_USER=admin
OAUTH_ADMIN_PASSWORD=a-strong-password-here
```

When enabled, OmniMem acts as a full OAuth 2.1 authorisation server with:

- **Discovery** (`/.well-known/oauth-authorization-server`) — auto-discovered by clients
- **Dynamic client registration** (`/register`) — clients register automatically (RFC 7591)
- **Authorisation code flow with PKCE** (`/authorize` + `/oauth/login`) — browser-based login
- **Token exchange and refresh** (`/token`) — 1-hour access tokens, 30-day refresh tokens with rotation

Point claude.ai at your OmniMem URL and it handles the rest — discovery, registration, browser login, and token management all happen automatically.

**Staying signed in.** Refresh tokens rotate on every use, but the old token isn't thrown away the instant it rotates — it stays valid for a short grace window (`OAUTH_REFRESH_GRACE_SECONDS`, default 120s) and replays during that window return the same new pair. claude.ai keeps several connections open and can refresh the same token from more than one at once; without the grace window, all but the first refresh would fail with `invalid_grant` and claude.ai would drop the connection and prompt you to sign in again. Token state is also persisted to Valkey with AOF enabled, so a `docker compose restart` doesn't lose sessions. If you still get logged out sooner than `OAUTH_REFRESH_MAX_DAYS`, that's the place to look. The login form is rate-limited per IP (`OAUTH_LOGIN_MAX_ATTEMPTS` / `OAUTH_LOGIN_WINDOW_SECONDS`) to blunt brute-force attempts on the admin password.

OAuth works alongside bearer token auth. If you have both `OAUTH_ENABLED` and `MCP_AUTH_TOKEN` set, both authentication methods are accepted via `MultiAuth`. Local Claude Code instances can continue using bearer tokens while claude.ai uses OAuth.

## Getting `421 Misdirected Request` or `403 Forbidden Origin` behind a proxy

If connecting through a reverse proxy or tunnel suddenly stops working, this is FastMCP 3.x's Host/Origin guard, not an OAuth problem — it just looks like one. There are two symptoms:

- **`421 Misdirected Request`** on everything (e.g. `curl https://your-host/mcp` returns `421` where it used to return `401`). FastMCP rejects any `Host` header outside its allowlist, which defaults to just localhost plus the bind address. Your public hostname isn't on it, so every request 421s, including the `/.well-known/*` discovery endpoints claude.ai probes first. A local `curl` to `localhost` still works, which makes it easy to misread as an OAuth fault.
- **`403 Forbidden Origin`** when you submit the login form. Most proxies terminate TLS and forward over plain http, so the server sees the request as `http://` while your browser sends `Origin: https://your-host`. FastMCP treats that scheme mismatch as an untrusted origin and blocks the login POST.

The server allows the `OAUTH_BASE_URL` (and `MCP_PUBLIC_URL`) hostname **and** its https origin automatically, so with OAuth configured correctly both usually just work. If you serve under an additional hostname or origin, add them:

```bash
MCP_ALLOWED_HOSTS=mcp.example.com,alt.example.com
MCP_ALLOWED_ORIGINS=https://mcp.example.com
```

You can also set FastMCP's raw knobs directly, but they're JSON arrays — a bare string won't parse:

```bash
FASTMCP_HTTP_ALLOWED_HOSTS=["mcp.example.com"]
FASTMCP_HTTP_ALLOWED_ORIGINS=["https://mcp.example.com"]
```

Quick check: `curl -s -o /dev/null -w '%{http_code}\n' https://your-host/.well-known/oauth-authorization-server` should return `200`. A `421` means the hostname isn't allowlisted; a `403` on login means the origin isn't.
