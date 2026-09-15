# Authentication and Reverse Proxies

OmniMem serves one thing worth putting behind a proxy: the MCP endpoint on port 8765 (plus the OAuth routes when OAuth is on). There's no web dashboard to protect any more; that moved into the desktop app's [settings panel](settings-panel.md), which is never served over HTTP.

This page covers the bearer token and some proxy configs. [Using it from multiple machines](remote-access.md) covers OAuth for claude.ai and the 421 and 403 errors.

## The bearer token

Set `MCP_AUTH_TOKEN` and every request to `/mcp` has to carry `Authorization: Bearer <token>`. Leave it unset and `/mcp` is open, which is fine on `127.0.0.1` and refused anywhere else (unless OAuth is on).

Where to set it:

- **Desktop app:** the Configuration page, which keeps it in the OS keychain
- **Linux packages:** `/etc/omnimem/omnimem.env`
- **Docker:** `-e MCP_AUTH_TOKEN=...` or your Compose file's environment

Make a decent one:

```bash
openssl rand -base64 32
```

Then give it to your client. Claude Code, for example:

```bash
claude mcp add --transport http omnimem https://mcp.example.com/mcp \
  --header "Authorization: Bearer your-secret-token"
```

or in `~/.claude.json`:

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "http",
      "url": "https://mcp.example.com/mcp",
      "headers": {
        "Authorization": "Bearer your-secret-token"
      }
    }
  }
}
```

Without a valid token `/mcp` answers `401` with a `WWW-Authenticate` header. With OAuth on, that header also names the protected resource document, which is how claude.ai finds the login.

## Putting a proxy in front

Whichever proxy you use, three things matter:

1. **Forward to port 8765** and pass the whole path through. `/mcp`, `/.well-known/...`, `/authorize`, `/token` and the rest all need to arrive.
2. **Don't buffer.** Streamable HTTP answers can arrive as a stream of events, so turn response buffering off.
3. **Tell OmniMem its public address** with `OAUTH_BASE_URL` (OAuth) or `MCP_PUBLIC_URL` (token only), so the Host and Origin guard accepts it.

Run OmniMem with `MCP_HOST=127.0.0.1` when the proxy is on the same machine, so the only way in is through the proxy.

### Caddy

```
mcp.example.com {
    reverse_proxy 127.0.0.1:8765 {
        flush_interval -1
    }
}
```

Caddy sorts out the certificate for you.

### nginx

```nginx
server {
    listen 443 ssl;
    server_name mcp.example.com;

    ssl_certificate     /etc/letsencrypt/live/mcp.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/mcp.example.com/privkey.pem;

    location / {
        proxy_pass http://127.0.0.1:8765;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_buffering off;
        proxy_read_timeout 1h;
    }
}
```

### Traefik

With OmniMem in a container next to Traefik:

```yaml
services:
  omnimem:
    image: richarvey/omnimem
    environment:
      MCP_HOST: 0.0.0.0
      OAUTH_ENABLED: "true"
      OAUTH_BASE_URL: https://mcp.example.com
      OAUTH_ADMIN_USER: admin
      OAUTH_ADMIN_PASSWORD: ${OAUTH_ADMIN_PASSWORD}
    volumes:
      - omnimem-data:/data
    labels:
      - "traefik.enable=true"
      - "traefik.http.routers.omnimem.rule=Host(`mcp.example.com`)"
      - "traefik.http.routers.omnimem.entrypoints=websecure"
      - "traefik.http.routers.omnimem.tls.certresolver=letsencrypt"
      - "traefik.http.services.omnimem.loadbalancer.server.port=8765"

volumes:
  omnimem-data:
```

`MCP_HOST: 0.0.0.0` is needed inside a container so Traefik can reach it, and OAuth (or a token) is what lets that start.

> [!NOTE]
> Coming with 7.0.0. Until the first release you can build from source (see [Build from source](quick-start.md#build-from-source)).

### Basic auth at the proxy

You can still put basic auth in front, but only if every client can send it, and claude.ai can't. For a token-only setup it adds little over `MCP_AUTH_TOKEN` itself. If you use OAuth, leave the OAuth routes and `/mcp` without proxy auth or the sign-in can't work.

## No proxy at all

If you only use OmniMem from the machine it runs on, you need none of this. For the odd remote session, an SSH tunnel is hard to beat:

```bash
ssh -L 8765:127.0.0.1:8765 your-server
```

then connect to `http://127.0.0.1:8765/mcp` as if it were local. A private network like Tailscale works the same way without the tunnel.
