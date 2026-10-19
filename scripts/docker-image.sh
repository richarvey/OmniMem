#!/usr/bin/env bash
# OmniMem's server image, for this machine's architecture, from the server
# .deb that scripts/linux-package.sh left in $OMNIMEM_DIST (default
# dist/linux), using packaging/docker/Dockerfile:
#
#   scripts/docker-image.sh                    build it and test it
#   scripts/docker-image.sh --push REPO        then push REPO:<version>-<arch>
#   scripts/docker-image.sh --manifest REPO    join REPO:<version>-amd64 and
#                                              -arm64 into REPO:<version>, and
#                                              REPO:latest for a release
#
# Each architecture is built natively on its own runner, so nothing runs under
# emulation; --manifest then publishes the pair under one tag. The test runs
# the image the way guides/docker.md does: it refuses to start without a
# token, and with one it answers /healthz and an MCP initialize, goes healthy,
# and runs as uid 1000. The embedding model it downloads is kept in the
# omnimem-image-test-hf volume between runs.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
dist="${OMNIMEM_DIST:-$repo/dist/linux}"
version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$repo/Cargo.toml" | head -n 1)"
pkgver="$(printf %s "$version" | tr - \~)"
case "$(uname -m)" in
    x86_64 | amd64) arch=amd64 ;;
    aarch64 | arm64) arch=arm64 ;;
    *)
        echo "no image for $(uname -m)" >&2
        exit 1
        ;;
esac

if [ "${1:-}" = --manifest ]; then
    target="${2:?usage: docker-image.sh --manifest REPO}"
    tags=("$version")
    # A dev or release-candidate version never moves latest.
    case "$version" in *-*) ;; *) tags+=(latest) ;; esac
    for tag in "${tags[@]}"; do
        docker manifest create --amend "$target:$tag" \
            "$target:$version-amd64" "$target:$version-arm64" >/dev/null
        docker manifest push --purge "$target:$tag" >/dev/null
        echo "Pushed $target:$tag (amd64 and arm64)"
    done
    exit 0
fi

deb="$dist/omnimem_${pkgver}_${arch}.deb"
[ -f "$deb" ] || {
    echo "no $deb: run scripts/linux-package.sh first" >&2
    exit 1
}
image="omnimem-server:$version-$arch"
name="omnimem-image-test-$$"
context="$(mktemp -d)"
cleanup() {
    docker rm -f "$name" >/dev/null 2>&1 || true
    rm -rf "$context"
}
trap cleanup EXIT

cp "$repo/packaging/docker/Dockerfile" "$context/Dockerfile"
cp "$deb" "$context/omnimem.deb"
# --pull: a published image starts from Debian's latest security updates.
docker build --quiet --pull --tag "$image" "$context" >/dev/null

fail() {
    echo "image test failed: $*" >&2
    docker logs "$name" 2>&1 | tail -n 30 >&2 || true
    exit 1
}

[ "$(docker run --rm "$image" --version)" = "omnimem $version" ] || fail "wrong version"
[ "$(docker run --rm --entrypoint id "$image" -u)" = 1000 ] || fail "not running as uid 1000"

# Fail closed: listening on 0.0.0.0 with no token or OAuth is refused.
if refusal="$(docker run --rm "$image" 2>&1)"; then
    fail "started without authentication"
fi
printf '%s' "$refusal" | grep -q 'refusing to start' || fail "unexpected refusal: $refusal"

token="image-test-$$"
docker run -d --name "$name" \
    -e MCP_AUTH_TOKEN="$token" \
    -v omnimem-image-test-hf:/data/hf-cache \
    -p 127.0.0.1::8765 \
    "$image" >/dev/null
port="$(docker port "$name" 8765/tcp | head -n 1 | sed 's/.*://')"

# Healthy is the image's own HEALTHCHECK passing, after the model is loaded.
for _ in $(seq 1 120); do
    state="$(docker inspect -f '{{.State.Status}} {{.State.Health.Status}}' "$name")"
    case "$state" in
        "running healthy") break ;;
        running*) sleep 3 ;;
        *) fail "container stopped ($state)" ;;
    esac
done
[ "$state" = "running healthy" ] || fail "never became healthy ($state)"

[ "$(curl -sf "http://127.0.0.1:$port/healthz")" = '{"status": "ok"}' ] || fail "/healthz"
status="$(curl -s -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:$port/mcp")"
[ "$status" = 401 ] || fail "/mcp without a token answered $status"
initialize='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"image-test","version":"1"}}}'
reply="$(curl -sf -X POST "http://127.0.0.1:$port/mcp" \
    -H "Authorization: Bearer $token" \
    -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -d "$initialize")" || fail "MCP initialize"
printf '%s' "$reply" | grep -q '"serverInfo"' || fail "MCP initialize answered: $reply"
docker exec "$name" test -s /data/omnimem.db || fail "no database in /data"
echo "The image ($arch) refuses to start without a token, and with one serves MCP from /data"

if [ "${1:-}" = --push ]; then
    target="${2:?usage: docker-image.sh --push REPO}"
    docker tag "$image" "$target:$version-$arch"
    docker push --quiet "$target:$version-$arch" >/dev/null
    echo "Pushed $target:$version-$arch"
fi
