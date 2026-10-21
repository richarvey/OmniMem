#!/usr/bin/env bash
# Line coverage of the workspace's tests, in $OMNIMEM_DIST (default
# dist/coverage):
#
#   summary.txt    coverage per file and in total, as cargo llvm-cov prints it
#   lcov.info      for editors and anything else that reads lcov
#   html/          the browsable report
#   coverage.svg   a badge with the total line coverage
#
# It covers the workspace's default members, so everything but the desktop
# crate, whose tests need GTK and a display (scripts/desktop-check.sh runs
# those). Tests that need the real embedding model skip themselves, as in a
# plain `cargo test` with no model cache. It runs in the ci/checks container on
# the Rust version ci/desktop/Dockerfile pins, with its own instrumented target
# directory in $OMNIMEM_CHECKS_CACHE (default ~/.cache/omnimem-checks).
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
cache="${OMNIMEM_CHECKS_CACHE:-$HOME/.cache/omnimem-checks}"
dist="${OMNIMEM_DIST:-$repo/dist/coverage}"
rust="$(sed -n 's/^FROM rust:\([0-9.]*\)-.*/\1/p' "$repo/ci/desktop/Dockerfile")"

mkdir -p "$cache/target" "$cache/home" "$cache/cargo" "$HOME/.cargo/registry" "$dist"
rm -rf "$dist/html"
docker build --quiet --build-arg RUST_VERSION="$rust" --tag omnimem-checks "$repo/ci/checks" >/dev/null

docker run --rm \
    --user "$(id -u):$(id -g)" \
    --env HOME=/cache/home \
    --env CARGO_HOME=/cache/cargo \
    --env CARGO_TARGET_DIR=/cache/target \
    --env CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" \
    --volume "$repo:/work" \
    --volume "$cache:/cache" \
    --volume "$dist:/dist" \
    --volume "$HOME/.cargo/registry:/cache/cargo/registry" \
    --workdir /work \
    omnimem-checks \
    bash -euo pipefail -c '
        cargo llvm-cov --locked --no-report
        cargo llvm-cov report --summary-only >/dist/summary.txt
        cargo llvm-cov report --lcov --output-path /dist/lcov.info
        cargo llvm-cov report --html --output-dir /dist
        cargo llvm-cov report --json --summary-only |
            jq -r ".data[0].totals.lines.percent" >/dist/total.txt
    '

percent="$(printf '%.1f' "$(cat "$dist/total.txt")")"
rm -f "$dist/total.txt"
colour="$(awk -v p="$percent" 'BEGIN {
    if (p >= 90) print "#4c1"; else if (p >= 80) print "#97ca00";
    else if (p >= 70) print "#a4a61d"; else if (p >= 60) print "#dfb317";
    else if (p >= 50) print "#fe7d37"; else print "#e05d44" }')"
value="$percent%"
label_width=61
value_width=$((${#value} * 7 + 10))
width=$((label_width + value_width))
cat >"$dist/coverage.svg" <<EOF
<svg xmlns="http://www.w3.org/2000/svg" width="$width" height="20" role="img" aria-label="coverage: $value">
<title>coverage: $value</title>
<linearGradient id="s" x2="0" y2="100%"><stop offset="0" stop-color="#bbb" stop-opacity=".1"/><stop offset="1" stop-opacity=".1"/></linearGradient>
<clipPath id="r"><rect width="$width" height="20" rx="3" fill="#fff"/></clipPath>
<g clip-path="url(#r)"><rect width="$label_width" height="20" fill="#555"/><rect x="$label_width" width="$value_width" height="20" fill="$colour"/><rect width="$width" height="20" fill="url(#s)"/></g>
<g fill="#fff" text-anchor="middle" font-family="Verdana,Geneva,DejaVu Sans,sans-serif" font-size="11"><text x="$((label_width / 2))" y="14">coverage</text><text x="$((label_width + value_width / 2))" y="14">$value</text></g>
</svg>
EOF

grep -E '^TOTAL' "$dist/summary.txt" || true
echo "Line coverage $value; the report is in $dist"
