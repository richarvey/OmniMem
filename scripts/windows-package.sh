#!/usr/bin/env bash
# Cross-build OmniMem's Windows app (x86_64) without Windows, and package it:
#
#   OmniMem-<version>-x64.msi   a per-user install with a Start menu shortcut
#                               (packaging/windows/omnimem.wxs)
#
# with a .sha256 beside it, in $OMNIMEM_DIST (default dist/windows).
#
# Everything runs in the ci/windows container, on the Rust version
# ci/desktop/Dockerfile pins: cargo-xwin compiles for the MSVC target with
# clang, fetching Microsoft's CRT and Windows SDK into the cache the first
# time (which accepts Microsoft's licence for them), and wixl turns the exe
# into an MSI. Nothing here can run what it builds, so the checks are that
# the exe is a windowed x86-64 Windows program and that the MSI holds it and
# its shortcut; the smoke test waits for a Windows runner. The build cache is
# $OMNIMEM_WINDOWS_CACHE (default ~/.cache/omnimem-windows).
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
cache="${OMNIMEM_WINDOWS_CACHE:-$HOME/.cache/omnimem-windows}"
dist="${OMNIMEM_DIST:-$repo/dist/windows}"
version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$repo/Cargo.toml" | head -n 1)"
rust="$(sed -n 's/^FROM rust:\([0-9.]*\)-.*/\1/p' "$repo/ci/desktop/Dockerfile")"
msi="OmniMem-$version-x64.msi"

mkdir -p "$cache/target" "$cache/home" "$cache/cargo" "$HOME/.cargo/registry" "$dist"
docker build --quiet --build-arg RUST_VERSION="$rust" --tag omnimem-windows-build \
    "$repo/ci/windows" >/dev/null

docker run --rm \
    --user "$(id -u):$(id -g)" \
    --env HOME=/cache/home \
    --env CARGO_HOME=/cache/cargo \
    --env CARGO_TARGET_DIR=/cache/target \
    --env CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" \
    --env XWIN_CACHE_DIR=/cache/xwin \
    --env VERSION="$version" \
    --env MSI="$msi" \
    --volume "$repo:/work" \
    --volume "$cache:/cache" \
    --volume "$dist:/dist" \
    --volume "$HOME/.cargo/registry:/cache/cargo/registry" \
    --workdir /work \
    omnimem-windows-build \
    bash -euo pipefail -c '
        target=x86_64-pc-windows-msvc
        cargo xwin build --release --locked -p omnimem --features desktop --target "$target"
        release="$CARGO_TARGET_DIR/$target/release"
        exe="$release/omnimem.exe"
        # A windowed x86-64 program: no console opening behind the tray app.
        file "$exe" | tee /dev/stderr | grep -q "PE32+ executable.*(GUI).*x86-64"
        # Every DLL it loads at start has to come from Windows, the Visual
        # C++ runtime (which the MSI checks for) or the MSI itself, which
        # carries DirectML.dll. A new one fails here, not on someone'"'"'s PC.
        unknown="$(llvm-objdump -p "$exe" | sed -n "s/^ *DLL Name: //p" | tr "[:upper:]" "[:lower:]" | sort -u |
            grep -vxE "api-ms-win-[a-z0-9-]+\.dll|(msvcp140|msvcp140_1|vcruntime140|vcruntime140_1)\.dll|directml\.dll|(advapi32|bcryptprimitives|combase|comctl32|crypt32|d3d12|dbghelp|dwmapi|dxgi|gdi32|imm32|kernel32|ntdll|ole32|oleaut32|setupapi|shell32|shlwapi|user32|userenv|ws2_32|bcrypt|secur32|ncrypt|iphlpapi|winhttp|version|uxtheme|propsys|dwrite|d2d1|windowscodecs|msimg32|winmm|powrprof|credui|netapi32|mswsock|psapi)\.dll" || true)"
        if [ -n "$unknown" ]; then
            echo "omnimem.exe now loads DLLs nothing provides: $unknown" >&2
            exit 1
        fi
        test -f "$release/DirectML.dll"
        wixl --arch x64 \
            --define Version="${VERSION%%[-+]*}" \
            --define FullVersion="$VERSION" \
            --define Exe="$exe" \
            --define DirectML="$release/DirectML.dll" \
            --define Icon=/work/packaging/windows/OmniMem.ico \
            --output "/dist/$MSI" \
            /work/packaging/windows/omnimem.wxs
        # The MSI holds the exe, DirectML and the shortcut, and checks for
        # the Visual C++ runtime.
        msiinfo export "/dist/$MSI" File | grep -q omnimem.exe
        msiinfo export "/dist/$MSI" File | grep -q DirectML.dll
        msiinfo export "/dist/$MSI" Shortcut | grep -q OmniMem
        msiinfo export "/dist/$MSI" LaunchCondition | grep -q vc_redist
    '

cd "$dist"
sha256sum "$msi" >"$msi.sha256"
echo "Packaged OmniMem $version for Windows (x64) in $dist:"
ls -l "$dist"
