# OmniMem's server image: the Linux server .deb, installed with apt on Debian
# 13 exactly as on a server, running `omnimem serve` in the foreground.
#
# scripts/docker-image.sh builds it, one architecture per runner, from a
# context holding this file and the .deb as omnimem.deb. Everything that
# matters lives in the /data volume (see guides/docker.md).
FROM debian:13-slim

# The 6.x images ran as uid 1000 too, so a bind-mounted folder that worked
# with them still does. Made before the package, whose postinst then finds
# the user already there.
RUN groupadd --system --gid 1000 omnimem \
    && useradd --system --uid 1000 --gid omnimem --home-dir /var/lib/omnimem \
        --no-create-home --shell /usr/sbin/nologin --comment "OmniMem" omnimem

COPY omnimem.deb /tmp/omnimem.deb
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends /tmp/omnimem.deb \
    && rm -rf /var/lib/apt/lists/* /tmp/omnimem.deb \
    && install -d -o omnimem -g omnimem -m 0750 /data /data/hf-cache

# The database, feeds.yml and backups sit together in /data; the embedding
# model downloads into it once. 0.0.0.0 so the published port reaches the
# server, which is why it won't start without MCP_AUTH_TOKEN or OAuth.
ENV OMNIMEM_DB=/data/omnimem.db \
    HF_HOME=/data/hf-cache \
    MCP_HOST=0.0.0.0 \
    MCP_PORT=8765

USER omnimem
WORKDIR /data
VOLUME /data
EXPOSE 8765

# No curl in the image: bash asks /healthz itself. The first start downloads
# the model (about 90 MB), hence the start period.
HEALTHCHECK --interval=30s --timeout=5s --start-period=180s --retries=3 \
    CMD ["bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/${MCP_PORT:-8765} && printf 'GET /healthz HTTP/1.0\\r\\nHost: localhost\\r\\n\\r\\n' >&3 && head -n 1 <&3 | grep -q ' 200 '"]

ENTRYPOINT ["omnimem"]
CMD ["serve"]
