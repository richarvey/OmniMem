#!/usr/bin/env bash
# The security scans, both run every time and the script failing at the end
# if either found something:
#
#   cargo audit   Cargo.lock against the RustSec advisory database. A known
#                 vulnerability fails; warnings (an unmaintained or yanked
#                 crate) are listed without failing. .cargo/audit.toml lists
#                 the advisories ignored, each with why it can't reach us.
#   gitleaks      the whole git history for committed secrets, with
#                 .gitleaks.toml's allowlist for documentation placeholders
#                 and .gitleaksignore for findings checked and cleared.
#
# cargo audit runs in the ci/checks container, with the advisory database
# cached in $OMNIMEM_CHECKS_CACHE (default ~/.cache/omnimem-checks). gitleaks is
# a pinned release, checked against its published checksums, kept in the same
# cache. A shallow clone has no history to scan, so CI checks out in full.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
cache="${OMNIMEM_CHECKS_CACHE:-$HOME/.cache/omnimem-checks}"
rust="$(sed -n 's/^FROM rust:\([0-9.]*\)-.*/\1/p' "$repo/ci/desktop/Dockerfile")"
gitleaks_version=8.30.1

failed=()

mkdir -p "$cache/home" "$cache/cargo"
docker build --quiet --build-arg RUST_VERSION="$rust" --tag omnimem-checks "$repo/ci/checks" >/dev/null
if docker run --rm \
    --user "$(id -u):$(id -g)" \
    --env HOME=/cache/home \
    --env CARGO_HOME=/cache/cargo \
    --volume "$repo:/work:ro" \
    --volume "$cache:/cache" \
    --workdir /work \
    omnimem-checks \
    cargo audit --file /work/Cargo.lock; then
    echo "cargo audit: no known vulnerabilities in Cargo.lock"
else
    failed+=("cargo audit")
fi

case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) platform=linux_x64 ;;
    Linux-aarch64) platform=linux_arm64 ;;
    Darwin-arm64) platform=darwin_arm64 ;;
    Darwin-x86_64) platform=darwin_x64 ;;
    *)
        echo "no gitleaks build for $(uname -sm)" >&2
        exit 1
        ;;
esac
tools="$cache/gitleaks-$gitleaks_version"
gitleaks="$tools/gitleaks"
if [ ! -x "$gitleaks" ]; then
    download="$(mktemp -d)"
    trap 'rm -rf "$download"' EXIT
    base="https://github.com/gitleaks/gitleaks/releases/download/v$gitleaks_version"
    archive="gitleaks_${gitleaks_version}_$platform.tar.gz"
    curl -sSfL -o "$download/$archive" "$base/$archive"
    curl -sSfL -o "$download/checksums.txt" "$base/gitleaks_${gitleaks_version}_checksums.txt"
    expected="$(awk -v f="$archive" '$2 == f { print $1 }' "$download/checksums.txt")"
    if command -v sha256sum >/dev/null; then
        actual="$(sha256sum "$download/$archive" | cut -d' ' -f1)"
    else
        actual="$(shasum -a 256 "$download/$archive" | cut -d' ' -f1)"
    fi
    [ -n "$expected" ] && [ "$expected" = "$actual" ] || {
        echo "gitleaks $archive doesn't match its published checksum" >&2
        exit 1
    }
    mkdir -p "$tools"
    tar -xzf "$download/$archive" -C "$tools" gitleaks
fi
if "$gitleaks" git --no-banner --redact --verbose --exit-code 1 --config "$repo/.gitleaks.toml" \
    --gitleaks-ignore-path "$repo/.gitleaksignore" "$repo"; then
    echo "gitleaks: no secrets in the history"
else
    failed+=(gitleaks)
fi

if [ "${#failed[@]}" -gt 0 ]; then
    echo "Security scans failed: ${failed[*]}" >&2
    exit 1
fi
