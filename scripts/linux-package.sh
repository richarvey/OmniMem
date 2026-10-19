#!/usr/bin/env bash
# Build OmniMem for Linux on this machine's architecture and package it:
#
#   omnimem-<version>-linux-<arch>.tar.gz          the headless binary, for
#                                                  `omnimem serve` and the hook
#   omnimem-desktop-<version>-linux-<arch>.tar.gz  the tray app
#
# each with a .sha256 beside it, in $OMNIMEM_DIST (default dist/linux).
# <arch> is amd64 or arm64.
#
# Both build in the ci/desktop container (Debian 13), as desktop-check.sh
# does, so nothing is installed on the host. ONNX Runtime's prebuilt library
# is built with GCC 14 against glibc 2.38 (it calls the __isoc23_* functions),
# so nothing that links it runs on an older glibc: Debian 13, Ubuntu 24.04 and
# Fedora 39 are the floor. The headless binary links its C++ runtime in
# (CXXSTDLIB=static=stdc++), so glibc is all it needs, and it's run in an
# Ubuntu 24.04 container before it's packaged, to prove it. The tray app also
# needs GTK 3, WebKitGTK 4.1 (2.40 or newer) and libayatana-appindicator3.
# Both use desktop-check.sh's cache folder ($OMNIMEM_DESKTOP_CACHE, default
# ~/.cache/omnimem-desktop), the headless build in a target directory of its
# own.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
cache="${OMNIMEM_DESKTOP_CACHE:-$HOME/.cache/omnimem-desktop}"
dist="${OMNIMEM_DIST:-$repo/dist/linux}"
version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$repo/Cargo.toml" | head -n 1)"
case "$(uname -m)" in
    x86_64 | amd64) arch=amd64 ;;
    aarch64 | arm64) arch=arm64 ;;
    *)
        echo "no Linux package for $(uname -m)" >&2
        exit 1
        ;;
esac

mkdir -p "$cache/target" "$cache/target-headless" "$cache/home" "$cache/cargo" \
    "$HOME/.cargo/registry" "$dist"
docker build --quiet --tag omnimem-desktop-build "$repo/ci/desktop" >/dev/null

# build <target directory> <C++ runtime> <cargo build arguments...>: a release
# build in the container, then `--help` from the binary it made.
build() {
    local target="$1" cxx="$2"
    shift 2
    docker run --rm \
        --user "$(id -u):$(id -g)" \
        --env HOME=/cache/home \
        --env CARGO_HOME=/cache/cargo \
        --env CARGO_TARGET_DIR="/cache/$target" \
        --env CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" \
        --env CXXSTDLIB="$cxx" \
        --volume "$repo:/work" \
        --volume "$cache:/cache" \
        --volume "$HOME/.cargo/registry:/cache/cargo/registry" \
        --workdir /work \
        omnimem-desktop-build \
        bash -euo pipefail -c '
            # A static libstdc++ lives in GCC'"'"'s own folder, which rustc
            # doesn'"'"'t search unless told.
            if [ "${CXXSTDLIB#static=}" != "$CXXSTDLIB" ]; then
                RUSTFLAGS="-L native=$(dirname "$(g++ -print-file-name=libstdc++.a)")"
                export RUSTFLAGS
            fi
            cargo build --release --locked "$@"
            "$CARGO_TARGET_DIR/release/omnimem" --help >/dev/null
        ' build "$@"
}

build target-headless static=stdc++ -p omnimem
build target stdc++ -p omnimem --features desktop
headless="$cache/target-headless/release/omnimem"
desktop="$cache/target/release/omnimem"

# The headless binary has to start on the oldest system it's promised to.
docker run --rm --volume "$headless:/omnimem:ro" ubuntu:24.04 /omnimem --help >/dev/null
echo "The headless binary runs on Ubuntu 24.04 (glibc 2.39)"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cp "$repo/LICENSE" "$work/LICENSE"

# package <name> <binary>: <name>.tar.gz holding the binary as `omnimem` and
# the licence, as the macOS tarball does, with its checksum.
package() {
    cp "$2" "$work/omnimem"
    # Run on a Mac, tar would add ._ files and extended attributes otherwise.
    COPYFILE_DISABLE=1 tar --no-xattrs -czf "$dist/$1.tar.gz" -C "$work" omnimem LICENSE
    (cd "$dist" && sha256sum "$1.tar.gz" >"$1.tar.gz.sha256")
}

package "omnimem-$version-linux-$arch" "$headless"
package "omnimem-desktop-$version-linux-$arch" "$desktop"
echo "Packaged OmniMem $version for Linux ($arch) in $dist:"
ls -l "$dist"
