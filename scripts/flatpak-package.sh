#!/usr/bin/env bash
# Build OmniMem's desktop app as a Flatpak for this machine's architecture:
#
#   omnimem-desktop-<version>-linux-<arch>.flatpak   <arch> amd64 or arm64
#
# with a .sha256 beside it, in $OMNIMEM_DIST (default dist/linux). It's a
# single-file bundle: `flatpak install --user <file>` installs it and fetches
# the GNOME runtime from Flathub. packaging/flatpak/com.squarecows.OmniMem.yml
# is the manifest; Flathub's shared-modules (the tray library) are fetched
# beside it at a pinned commit.
#
# Everything runs in the ci/flatpak container, privileged because bubblewrap
# needs user namespaces. The SDK, the flatpak-builder state and cargo's caches
# live in $OMNIMEM_FLATPAK_CACHE (default ~/.cache/omnimem-flatpak), so the
# runtimes download once and a rebuild only compiles what changed. The bundle
# is then installed in the container and smoke-tested inside its sandbox.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
cache="${OMNIMEM_FLATPAK_CACHE:-$HOME/.cache/omnimem-flatpak}"
dist="${OMNIMEM_DIST:-$repo/dist/linux}"
version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$repo/Cargo.toml" | head -n 1)"
case "$(uname -m)" in
    x86_64 | amd64) arch=amd64 ;;
    aarch64 | arm64) arch=arm64 ;;
    *)
        echo "no Flatpak for $(uname -m)" >&2
        exit 1
        ;;
esac
bundle="omnimem-desktop-$version-linux-$arch.flatpak"
# flathub/shared-modules, 2026-10-16.
shared_modules=a58e654b9113feffaee113cc7167ebbbdb406bee

mkdir -p "$cache/home" "$cache/flatpak-cargo" "$dist"
# The container runs as the calling user, who has no entry in its user
# database; D-Bus, which installing and running a Flatpak need, won't start a
# session without one. As desktop-check.sh does: a minimal passwd and group.
printf 'root:x:0:0::/root:/bin/sh\nomnimem:x:%s:%s::/cache/home:/bin/sh\n' "$(id -u)" "$(id -g)" >"$cache/passwd"
printf 'root:x:0:\nomnimem:x:%s:\n' "$(id -g)" >"$cache/group"
docker build --quiet --tag omnimem-flatpak-build "$repo/ci/flatpak" >/dev/null

docker run --rm --privileged \
    --user "$(id -u):$(id -g)" \
    --env HOME=/cache/home \
    --env BUNDLE="$bundle" \
    --env SHARED_MODULES="$shared_modules" \
    `# Docker Desktop shows bind-mounted files with another owner, which git` \
    `# then refuses to work in; this marks the checkout safe for this run.` \
    --env GIT_CONFIG_COUNT=1 \
    --env GIT_CONFIG_KEY_0=safe.directory \
    --env GIT_CONFIG_VALUE_0='*' \
    --volume "$repo:/work" \
    --volume "$cache:/cache" \
    --volume "$dist:/dist" \
    --volume "$cache/passwd:/etc/passwd:ro" \
    --volume "$cache/group:/etc/group:ro" \
    --workdir /work \
    omnimem-flatpak-build \
    bash -euo pipefail -c '
        app=com.squarecows.OmniMem
        modules=packaging/flatpak/shared-modules
        if [ "$(git -C "$modules" rev-parse HEAD 2>/dev/null)" != "$SHARED_MODULES" ]; then
            rm -rf "$modules"
            git init -q "$modules"
            git -C "$modules" fetch -q --depth 1 https://github.com/flathub/shared-modules "$SHARED_MODULES"
            git -C "$modules" checkout -q FETCH_HEAD
        fi

        flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
        # No FUSE in a container, hence --disable-rofiles-fuse.
        flatpak-builder --user --install-deps-from=flathub --disable-rofiles-fuse \
            --state-dir=/cache/state --repo=/cache/repo --force-clean \
            /cache/build "packaging/flatpak/$app.yml"
        flatpak build-bundle --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo \
            /cache/repo "/dist/$BUNDLE" "$app"

        # The bundle, installed and run in its sandbox: the tray, the window
        # and the panel over omnimem://, as on the other platforms. Both
        # steps need a D-Bus session. Installing also asks the system bus
        # for parental controls, and the container has no system bus; with
        # it pointed at the session bus the service is simply absent, which
        # flatpak takes as "no controls".
        export XDG_RUNTIME_DIR="$(mktemp -d)"
        BUNDLE="$BUNDLE" app="$app" dbus-run-session -- bash -euo pipefail -c "
            export DBUS_SYSTEM_BUS_ADDRESS=\"\$DBUS_SESSION_BUS_ADDRESS\"
            flatpak install --user -y --noninteractive --reinstall \"/dist/\$BUNDLE\" >/dev/null
            timeout 300 xvfb-run --auto-servernum \
                flatpak run --command=omnimem \"\$app\" desktop --smoke-test
        "
    '

cd "$dist"
sha256sum "$bundle" >"$bundle.sha256"
echo "Packaged OmniMem $version as a Flatpak ($arch) in $dist:"
ls -l "$dist/$bundle"
