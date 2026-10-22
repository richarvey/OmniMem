#!/usr/bin/env bash
# Build OmniMem for Linux on this machine's architecture and package it:
#
#   omnimem_<pkgver>_<arch>.deb                    the server for Debian and
#   omnimem-<pkgver>-1.<rpmarch>.rpm               Ubuntu, Fedora and RHEL: the
#                                                  headless binary as a systemd
#                                                  service, configured by
#                                                  /etc/omnimem/omnimem.env
#   omnimem-<version>-linux-<arch>.tar.gz          the same server for any other
#                                                  distribution, with install.sh
#   omnimem-desktop-<version>-linux-<arch>.tar.gz  the tray app
#
# each with a .sha256 beside it, in $OMNIMEM_DIST (default dist/linux).
# <arch> is amd64 or arm64, <rpmarch> x86_64 or aarch64, and <pkgver> the
# version with ~ for -, so 7.0.2~dev sorts before 7.0.2 in dpkg and rpm. The
# service's files are in packaging/linux.
#
# Everything builds in the ci/desktop container (Debian 13), as
# desktop-check.sh does, so nothing is installed on the host. ONNX Runtime's
# prebuilt library is built with GCC 14 against glibc 2.38 (it calls the
# __isoc23_* functions), so nothing that links it runs on an older glibc:
# Debian 13, Ubuntu 24.04 and Fedora 39 are the floor. The headless binary
# links its C++ runtime in (CXXSTDLIB=static=stdc++), so glibc is all it
# needs, and it's run in an Ubuntu 24.04 container before it's packaged, to
# prove it; the .deb is installed and removed in a Debian 13 one. The tray app
# also needs GTK 3, WebKitGTK 4.1 (2.40 or newer) and libayatana-appindicator3.
# The builds use desktop-check.sh's cache folder ($OMNIMEM_DESKTOP_CACHE,
# default ~/.cache/omnimem-desktop), the headless one in a target directory
# of its own.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
cache="${OMNIMEM_DESKTOP_CACHE:-$HOME/.cache/omnimem-desktop}"
dist="${OMNIMEM_DIST:-$repo/dist/linux}"
version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$repo/Cargo.toml" | head -n 1)"
# tr, not ${version//-/~}: bash 3.2 and 5.2 disagree about escaping the ~.
pkgver="$(printf %s "$version" | tr - \~)"
case "$(uname -m)" in
    x86_64 | amd64) arch=amd64 rpmarch=x86_64 ;;
    aarch64 | arm64) arch=arm64 rpmarch=aarch64 ;;
    *)
        echo "no Linux package for $(uname -m)" >&2
        exit 1
        ;;
esac

# The packaging stage, run inside the container by the script itself (below)
# so dpkg-deb and rpmbuild needn't be on the host. /cache/package/server is
# the staged file tree; the packages go to /dist.
if [ "${1:-}" = --packages ]; then
    pack=/cache/package
    deb="$pack/deb"
    rm -rf "$deb" "$pack/rpm"
    cp -a "$pack/server" "$deb"
    mkdir -p "$deb/DEBIAN"
    cat >"$deb/DEBIAN/control" <<EOF
Package: omnimem
Version: $pkgver
Architecture: $arch
Maintainer: Ric Harvey <ric@squarecows.com>
Depends: libc6 (>= 2.38), libgcc-s1, ca-certificates
Section: net
Priority: optional
Homepage: https://omnimem.org
Description: self-hosted semantic memory for AI agents
 OmniMem's MCP server, run as the omnimem systemd service and configured by
 /etc/omnimem/omnimem.env. Memories, feeds and backups live in
 /var/lib/omnimem.
EOF
    echo /etc/omnimem/omnimem.env >"$deb/DEBIAN/conffiles"
    for script in postinst prerm postrm; do
        install -m 0755 "/work/packaging/linux/$script.sh" "$deb/DEBIAN/$script"
    done
    dpkg-deb --root-owner-group --build "$deb" "/dist/omnimem_${pkgver}_${arch}.deb" >/dev/null

    top="$pack/rpm"
    mkdir -p "$top/SPECS" "$top/BUILD" "$top/RPMS"
    cat >"$top/SPECS/omnimem.spec" <<EOF
Name: omnimem
Version: $pkgver
Release: 1
Summary: Self-hosted semantic memory for AI agents
License: MIT
URL: https://omnimem.org
AutoReqProv: no
Requires: glibc >= 2.38
# TLS (the model download, Claude, feeds) checks against the system's CAs.
Requires: ca-certificates

%description
OmniMem's MCP server, run as the omnimem systemd service and configured by
/etc/omnimem/omnimem.env. Memories, feeds and backups live in
/var/lib/omnimem.

%install
cp -a $pack/server/. %{buildroot}/

%files
/usr/bin/omnimem
/usr/lib/systemd/system/omnimem.service
%dir /etc/omnimem
%config(noreplace) %attr(0640, root, root) /etc/omnimem/omnimem.env
/usr/share/doc/omnimem/copyright

%post
$(cat /work/packaging/linux/postinst.sh)

%preun
$(cat /work/packaging/linux/prerm.sh)

%postun
$(cat /work/packaging/linux/postrm.sh)
EOF
    # Its own package database: the system one isn't writable here, and an
    # rpm build of prebuilt files has nothing in it to check against.
    rpmbuild -bb --quiet --nodeps --target "$rpmarch" \
        --define "_topdir $top" \
        --define "_dbpath $top/db" \
        --define "_build_id_links none" \
        --define "debug_package %{nil}" \
        --define "__os_install_post %{nil}" \
        "$top/SPECS/omnimem.spec"
    cp "$top/RPMS/$rpmarch/omnimem-$pkgver-1.$rpmarch.rpm" /dist/
    requires="$(rpm -qp --dbpath "$top/db" --requires "/dist/omnimem-$pkgver-1.$rpmarch.rpm")"
    printf '%s\n' "$requires" | grep -q 'glibc >= 2.38'
    printf '%s\n' "$requires" | grep -qx ca-certificates
    exit 0
fi

mkdir -p "$cache/target" "$cache/target-headless" "$cache/home" "$cache/cargo" \
    "$HOME/.cargo/registry" "$dist"
docker build --quiet --tag omnimem-desktop-build "$repo/ci/desktop" >/dev/null

# in_container <command...>: run as the calling user in the build container,
# with the repository at /work, the cache at /cache and the packages at /dist.
in_container() {
    docker run --rm \
        --user "$(id -u):$(id -g)" \
        --env HOME=/cache/home \
        --env CARGO_HOME=/cache/cargo \
        --env CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" \
        --volume "$repo:/work" \
        --volume "$cache:/cache" \
        --volume "$dist:/dist" \
        --volume "$HOME/.cargo/registry:/cache/cargo/registry" \
        --workdir /work \
        ${extra[@]+"${extra[@]}"} \
        omnimem-desktop-build "$@"
}

# build <target directory> <C++ runtime> <cargo build arguments...>: a release
# build in the container, then `--help` from the binary it made.
build() {
    local target="$1" cxx="$2"
    shift 2
    extra=(--env CARGO_TARGET_DIR="/cache/$target" --env CXXSTDLIB="$cxx")
    in_container bash -euo pipefail -c '
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

# The server's file tree, as both packages install it.
server="$cache/package/server"
rm -rf "$server"
mkdir -p "$server/usr/bin" "$server/usr/lib/systemd/system" "$server/etc/omnimem" \
    "$server/usr/share/doc/omnimem"
install -m 0755 "$headless" "$server/usr/bin/omnimem"
install -m 0644 "$repo/packaging/linux/omnimem.service" "$server/usr/lib/systemd/system/"
install -m 0640 "$repo/packaging/linux/omnimem.env" "$server/etc/omnimem/"
install -m 0644 "$repo/LICENSE" "$server/usr/share/doc/omnimem/copyright"
rm -f "$dist"/omnimem_*.deb "$dist"/omnimem-*.rpm
extra=()
in_container bash /work/scripts/linux-package.sh --packages
deb="omnimem_${pkgver}_${arch}.deb"
rpm="omnimem-$pkgver-1.$rpmarch.rpm"

# The .deb's scripts, for real: installed the way the docs say (apt, which
# brings in ca-certificates, absent from a slim image), used and removed on
# Debian 13.
docker run --rm --volume "$dist/$deb:/tmp/omnimem.deb:ro" debian:13-slim sh -euc '
    apt-get update -qq
    DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends /tmp/omnimem.deb >/dev/null
    test -s /etc/ssl/certs/ca-certificates.crt
    id omnimem >/dev/null
    test "$(stat -c %a:%U:%G /etc/omnimem/omnimem.env)" = 640:root:omnimem
    test "$(stat -c %U /var/lib/omnimem)" = omnimem
    su -s /bin/sh omnimem -c "omnimem --help" >/dev/null
    # Commands read the settings file and find the service'"'"'s database
    # through it, refusing to start an empty one anywhere else.
    su -s /bin/sh omnimem -c "omnimem stats" 2>&1 | grep -q "/var/lib/omnimem/omnimem.db (from OMNIMEM_DB in /etc/omnimem/omnimem.env)"
    dpkg -r omnimem >/dev/null
    test ! -e /usr/bin/omnimem
    test -d /var/lib/omnimem
'
echo "The .deb installs, runs as the omnimem user and removes cleanly on Debian 13"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# The server tarball: a folder with the binary, the service and install.sh.
bundle="omnimem-$version-linux-$arch"
mkdir "$work/$bundle"
cp "$headless" "$work/$bundle/omnimem"
cp "$repo/LICENSE" "$repo/packaging/linux/omnimem.service" "$repo/packaging/linux/omnimem.env" \
    "$work/$bundle/"
install -m 0755 "$repo/packaging/linux/install.sh" "$work/$bundle/install.sh"
# Run on a Mac, tar would add ._ files and extended attributes otherwise.
COPYFILE_DISABLE=1 tar --no-xattrs -czf "$dist/$bundle.tar.gz" -C "$work" "$bundle"

# The tray app: the binary and the licence, as the macOS tarball has them.
desktop_bundle="omnimem-desktop-$version-linux-$arch"
mkdir "$work/desktop"
cp "$desktop" "$work/desktop/omnimem"
cp "$repo/LICENSE" "$work/desktop/LICENSE"
COPYFILE_DISABLE=1 tar --no-xattrs -czf "$dist/$desktop_bundle.tar.gz" -C "$work/desktop" omnimem LICENSE

cd "$dist"
for file in "$deb" "$rpm" "$bundle.tar.gz" "$desktop_bundle.tar.gz"; do
    sha256sum "$file" >"$file.sha256"
done
echo "Packaged OmniMem $version for Linux ($arch) in $dist:"
ls -l "$dist"
