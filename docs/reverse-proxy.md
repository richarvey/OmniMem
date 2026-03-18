# Reverse Proxy Authentication

OmniMem's web UI has no built-in authentication. For production deployments on public networks, place it behind a reverse proxy with basic auth.

## Traefik

Add labels to the `web_ui` service in `docker-compose.yml`:

```yaml
web_ui:
  labels:
    - "traefik.enable=true"
    - "traefik.http.routers.omnimem.rule=Host(`omnimem.example.com`)"
    - "traefik.http.routers.omnimem.entrypoints=websecure"
    - "traefik.http.routers.omnimem.tls.certresolver=letsencrypt"
    - "traefik.http.routers.omnimem.middlewares=omnimem-auth"
    - "traefik.http.middlewares.omnimem-auth.basicauth.users=admin:$$2y$$05$$..."
    - "traefik.http.services.omnimem.loadbalancer.server.port=8080"
```

Generate the password hash:

```bash
htpasswd -nB admin
```

Escape `$` signs as `$$` in the compose file.

## Caddy

```
omnimem.example.com {
    basicauth {
        admin $2a$14$...
    }
    reverse_proxy web_ui:8080
}
```

Generate the password hash:

```bash
caddy hash-password
```
