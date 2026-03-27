# Authentication

## Built-in bearer token auth (v3.8.0+)

OmniMem supports optional bearer token authentication on both the MCP server and web UI. Set the relevant environment variables in your `.env` file to enable:

```bash
MCP_AUTH_TOKEN=your-secret-token      # Protects the MCP endpoint (port 8765)
WEB_UI_AUTH_TOKEN=your-secret-token   # Protects the web dashboard (port 8080)
```

When set, all requests must include an `Authorization: Bearer <token>` header. The `/metrics` endpoint and static assets are exempt from web UI auth so Prometheus can scrape without credentials.

When unset or blank, authentication is disabled and behaviour is unchanged from previous versions.

You can use the same token for both or different tokens. To generate a secure token:

```bash
python3 -c "import secrets; print(secrets.token_urlsafe(32))"
```

**Claude Code configuration**: Add the token to your MCP server config in `~/.claude.json`:

```json
{
  "mcpServers": {
    "omnimem": {
      "type": "http",
      "url": "http://localhost:8765/mcp",
      "headers": {
        "Authorization": "Bearer your-secret-token"
      }
    }
  }
}
```

## Reverse proxy (alternative)

For production deployments that need TLS termination, IP allowlisting, or more advanced auth (OAuth, SSO), place both the **web UI** (port 8080, includes `/metrics`) and the **MCP server** (port 8765, Streamable HTTP transport) behind a reverse proxy. This can be used instead of or in addition to built-in bearer token auth.

### Traefik

Add labels to both the `web_ui` and `mcp_server` services in `docker-compose.yml`. They share a single basic auth middleware definition.

```yaml
web_ui:
  labels:
    - "traefik.enable=true"
    - "traefik.http.routers.omnimem-ui.rule=Host(`omnimem.example.com`)"
    - "traefik.http.routers.omnimem-ui.entrypoints=websecure"
    - "traefik.http.routers.omnimem-ui.tls.certresolver=letsencrypt"
    - "traefik.http.routers.omnimem-ui.middlewares=omnimem-auth"
    - "traefik.http.middlewares.omnimem-auth.basicauth.users=admin:$$2y$$05$$..."
    - "traefik.http.services.omnimem-ui.loadbalancer.server.port=8080"

mcp_server:
  labels:
    - "traefik.enable=true"
    - "traefik.http.routers.omnimem-mcp.rule=Host(`mcp.example.com`)"
    - "traefik.http.routers.omnimem-mcp.entrypoints=websecure"
    - "traefik.http.routers.omnimem-mcp.tls.certresolver=letsencrypt"
    - "traefik.http.routers.omnimem-mcp.middlewares=omnimem-auth"
    - "traefik.http.services.omnimem-mcp.loadbalancer.server.port=8765"
```

Generate the password hash:

```bash
htpasswd -nB admin
```

Escape `$` signs as `$$` in the compose file. Both routers reference the same `omnimem-auth` middleware, so you only define the credentials once.

> **Note:** If you prefer a single domain, you can use path-based routing instead of separate hosts — e.g. `PathPrefix('/mcp')` for the MCP server — but separate subdomains are simpler to reason about.

### Caddy

```
omnimem.example.com {
    basicauth {
        admin $2a$14$...
    }
    reverse_proxy web_ui:8080
}

mcp.example.com {
    basicauth {
        admin $2a$14$...
    }
    reverse_proxy mcp_server:8765
}
```

Generate the password hash:

```bash
caddy hash-password
```

### Keeping it local (no proxy needed)

By default, both services bind to `127.0.0.1` in `docker-compose.yml`, so they are not reachable from the network. If you only access OmniMem from the same machine, no reverse proxy is needed. For remote access without a public endpoint, an SSH tunnel works well:

```bash
ssh -L 8080:127.0.0.1:8080 -L 8765:127.0.0.1:8765 your-server
```

Then connect to `http://localhost:8765/mcp` from your local machine.
```
