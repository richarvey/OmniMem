# Using It from Multiple Machines

One OmniMem, every machine you work from. Same memories, same graveyard, same project context, whether you're in Claude Code on your laptop or in claude.ai on your phone.

Out of the box OmniMem only listens on `127.0.0.1:8765`, so nothing else can reach it. Opening it up takes three things: a listen address other clients can reach, authentication, and usually a reverse proxy with TLS in front. [Authentication and reverse proxies](reverse-proxy.md) has the proxy configs.

## Listening beyond this machine

Set `MCP_HOST` to the address to listen on (`0.0.0.0` for everything). OmniMem fails closed: a non-loopback address with no `MCP_AUTH_TOKEN` and no OAuth refuses to start, with a message saying so. I'd rather it didn't start than quietly hand your memory to the local network.

Then point your clients at the public URL, which always ends in `/mcp`:

```
https://mcp.yourdomain.com/mcp
```

The [connection guides](../guides/) cover each agent.

## Two ways to authenticate

**A bearer token** (`MCP_AUTH_TOKEN`) is the simple one. Every client sends `Authorization: Bearer <token>`, compared in constant time. Claude Code, Cursor and most coding agents are happy with that.

**OAuth 2.1** is what claude.ai and other OAuth clients need: they discover the server, register themselves, send you to a login page and manage their own tokens.

You can have both at once. Local agents keep using the token while claude.ai signs in with OAuth.

## OAuth 2.1 for claude.ai

Turn it on with four settings (in the environment, `/etc/omnimem/omnimem.env`, or the desktop app's Configuration page, where the password goes to the keychain):

```bash
OAUTH_ENABLED=true
OAUTH_BASE_URL=https://mcp.yourdomain.com   # where clients reach OmniMem
OAUTH_ADMIN_USER=admin
OAUTH_ADMIN_PASSWORD=a-strong-password-here
```

`OAUTH_BASE_URL` has to be https (plain http is only accepted for localhost) with no query string or fragment. If OAuth is switched on and any of those four are missing or wrong, OmniMem refuses to start rather than running without OAuth. 6.x logged a warning and carried on, and you found out when claude.ai couldn't connect.

With it on, OmniMem is its own authorisation server:

| Route | What it's for |
|---|---|
| `/.well-known/oauth-authorization-server` | Server metadata (RFC 8414) |
| `/.well-known/oauth-protected-resource/mcp` | Where `/mcp` says to get a token (RFC 9728) |
| `/register` | Dynamic client registration (RFC 7591). Public clients get no secret and use PKCE alone |
| `/authorize` | The authorisation code flow, PKCE with S256 only |
| `/oauth/login` | The login page |
| `/token` | Code exchange and refresh |
| `/revoke` | Token revocation (RFC 7009) |
| `/icon.svg`, `/favicon.svg`, `/favicon.ico`, `/oauth/icon.svg` | The icon claude.ai shows for the connector |

Add the connector in claude.ai with your `https://mcp.yourdomain.com/mcp` URL and it does the rest: discovery, registration, the browser login and the tokens.

### Staying signed in

Access tokens last an hour. Refresh tokens rotate every time they're used, and a chain of them lasts `OAUTH_REFRESH_MAX_DAYS` (30 by default, 90 at most) before you have to sign in again.

A rotated refresh token isn't thrown away straight off. For `OAUTH_REFRESH_GRACE_SECONDS` (120) it still works, and using it returns the same new pair. claude.ai keeps several connections open and can refresh the same token from more than one at once; without that window every refresh but the first would fail and it would sign you out.

Clients and tokens live in the database, so restarting OmniMem signs nobody out. Tokens are stored as SHA-256 hashes, never the tokens themselves.

The login page allows `OAUTH_LOGIN_MAX_ATTEMPTS` (10) failed logins per address in `OAUTH_LOGIN_WINDOW_SECONDS` (900) before it starts refusing that address. Behind a reverse proxy every login arrives from the proxy's address, so the limit applies to everyone at once.

## `421 Misdirected Request` or `403 Forbidden Origin`

These look like OAuth failures. They aren't. They're the Host and Origin guard, which stops a hostile web page from talking to your server through someone's browser (DNS rebinding and friends).

- **`421 Misdirected Request`** means the `Host` header isn't on the allowed list. The list is localhost, `127.0.0.1`, `::1`, the listen address, the hosts of `OAUTH_BASE_URL` and `MCP_PUBLIC_URL`, and anything in `MCP_ALLOWED_HOSTS`. A `curl` to localhost still works, which is what makes this one confusing.
- **`403 Forbidden Origin`** means a browser sent an `Origin` that isn't allowed. An origin is allowed when it's on the list (the origins of `OAUTH_BASE_URL` and `MCP_PUBLIC_URL`, `http://localhost:<port>`, `http://127.0.0.1:<port>`, and `MCP_ALLOWED_ORIGINS`), when it's loopback talking to loopback, or when it's the server's own host.

That last rule ignores the scheme on purpose. A proxy that terminates TLS forwards the request over plain http while your browser says `Origin: https://...`, and in 6.x that mismatch got the login form a 403. It doesn't any more.

Most setups need nothing extra: set `OAUTH_BASE_URL` (or `MCP_PUBLIC_URL` if you're only using a bearer token) to the address clients use. For additional hostnames:

```bash
MCP_ALLOWED_HOSTS=mcp.example.com,alt.example.com
MCP_ALLOWED_ORIGINS=https://alt.example.com
```

FastMCP's `FASTMCP_HTTP_ALLOWED_HOSTS` and `FASTMCP_HTTP_ALLOWED_ORIGINS` JSON-array knobs are gone with FastMCP.

A quick check from outside:

```bash
curl -s -o /dev/null -w '%{http_code}\n' https://mcp.yourdomain.com/.well-known/oauth-authorization-server
```

`200` is good. `421` means the hostname isn't allowed, and a `404` means OAuth isn't switched on.

## Security checklist

- Keep `MCP_HOST` on `127.0.0.1` unless something else really needs to reach it
- Use TLS on the proxy for anything beyond your own network
- Use a long random `MCP_AUTH_TOKEN` (`openssl rand -base64 32` does the job) and a strong OAuth password
- `/healthz` is the only route that needs no authentication, and it says nothing but `{"status": "ok"}`

What OmniMem does for you: constant-time token and password checks, the fail-closed start, the Host and Origin guard, backup filenames checked against path traversal, and size caps on uploaded backups, skill bundles and fetched RSS pages.
