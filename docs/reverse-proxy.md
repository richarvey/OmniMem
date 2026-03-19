# Reverse Proxy Authentication

OmniMem has no built-in authentication on any endpoint. For production deployments on public networks, place both the **web UI** (port 8080, includes `/metrics`) and the **MCP server** (port 8765, SSE transport) behind a reverse proxy with basic auth and TLS.

## Traefik

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

> **Note:** If you prefer a single domain, you can use path-based routing instead of separate hosts — e.g. `PathPrefix('/sse')` for the MCP server — but separate subdomains are simpler to reason about.

## Caddy

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

## Keeping it local (no proxy needed)

By default, both services bind to `127.0.0.1` in `docker-compose.yml`, so they are not reachable from the network. If you only access OmniMem from the same machine, no reverse proxy is needed. For remote access without a public endpoint, an SSH tunnel works well:

```bash
ssh -L 8080:127.0.0.1:8080 -L 8765:127.0.0.1:8765 your-server
```
