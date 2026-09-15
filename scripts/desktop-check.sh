#!/usr/bin/env bash
# Lint, test, build and smoke-test the desktop app on Linux inside a
# container (ci/desktop/Dockerfile), so the GTK, WebKitGTK and AppIndicator
# development libraries never need installing on the host.
#
# The smoke test runs `omnimem desktop --smoke-test` under Xvfb with a D-Bus
# session: it creates the tray icon and settings window, loads the settings
# page over the omnimem:// protocol, and exits 0 once the page has answered
# over IPC.
#
# Build output and the cargo cache live in $OMNIMEM_DESKTOP_CACHE
# (default ~/.cache/omnimem-desktop), outside the repository, and the
# container runs as the calling user so nothing it writes is owned by root.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
cache="${OMNIMEM_DESKTOP_CACHE:-$HOME/.cache/omnimem-desktop}"
image="omnimem-desktop-build"

mkdir -p "$cache/target" "$cache/home" "$cache/cargo" "$HOME/.cargo/registry"

# The container runs as the calling user, who has no entry in its user
# database; D-Bus refuses to start a session without one. A minimal passwd
# and group for that user keeps the host's own files out of the container.
printf 'root:x:0:0::/root:/bin/sh\nomnimem:x:%s:%s::/cache/home:/bin/sh\n' "$(id -u)" "$(id -g)" > "$cache/passwd"
printf 'root:x:0:\nomnimem:x:%s:\n' "$(id -g)" > "$cache/group"
docker build --quiet --tag "$image" "$repo/ci/desktop" >/dev/null

docker run --rm \
    --user "$(id -u):$(id -g)" \
    --env HOME=/cache/home \
    --env CARGO_HOME=/cache/cargo \
    --env CARGO_TARGET_DIR=/cache/target \
    --env CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" \
    --volume "$repo:/work" \
    --volume "$cache:/cache" \
    --volume "$HOME/.cargo/registry:/cache/cargo/registry" \
    --volume "$cache/passwd:/etc/passwd:ro" \
    --volume "$cache/group:/etc/group:ro" \
    --workdir /work \
    "$image" \
    bash -euo pipefail -c '
        cargo clippy -p omnimem-desktop -p omnimem --features omnimem/desktop --all-targets -- -D warnings
        cargo test -p omnimem-desktop
        cargo build -p omnimem --features desktop
        dbus-run-session -- xvfb-run --auto-servernum /cache/target/debug/omnimem desktop --smoke-test
    '
