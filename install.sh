#!/usr/bin/env bash
#
# OmniMem's one-line installer from the 6.x days:
#
#   curl -fsSL https://code.squarecows.com/ric/omnimem/raw/branch/main/install.sh | bash
#
# It set up the 6.x Docker stack (Valkey, the MCP server, the web UI and the
# RSS worker) from files on main. OmniMem 7 replaced that stack with one app,
# so main doesn't carry those files any more. This keeps the command working
# for everyone who has it in a README or a blog post: it says where 7 is, then
# runs the 6.7 installer from the v6.7.x branch, which fetches its compose file
# from there as well.
#
# The 6.x installer's own overrides (OMNIMEM_DIR, OMNIMEM_BRANCH,
# OMNIMEM_RAW_BASE) pass straight through.
set -euo pipefail

# Everything is in a function that runs on the last line, so bash has read the
# whole script before it starts: under `curl | bash` the rest of this file is
# still sitting on stdin.
main() {
    local releases="https://code.squarecows.com/ric/omnimem/releases/latest"
    local branch="${OMNIMEM_BRANCH:-v6.7.x}"
    # main is 7 now, so a 6.x install can't fetch from it.
    [ "$branch" = main ] && branch=v6.7.x
    local installer="https://code.squarecows.com/ric/omnimem/raw/branch/$branch/install.sh"

    cat >&2 <<EOF

OmniMem 7 is out: one app instead of four containers, with installers for
macOS, Windows and Linux, server packages and a Docker image:

    $releases

This script installs OmniMem 6.7 as the Docker stack, the way it always has.

EOF

    local script
    script="$(mktemp "${TMPDIR:-/tmp}/omnimem-install.XXXXXX")"
    trap 'rm -f "$script"' EXIT
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$installer" -o "$script"
    elif command -v wget >/dev/null 2>&1; then
        wget -q "$installer" -O "$script"
    else
        echo "error: neither curl nor wget is available" >&2
        exit 1
    fi
    # stdin closed: the 6.x installer reads its prompts from /dev/tty itself.
    OMNIMEM_BRANCH="$branch" bash "$script" "$@" </dev/null
}

main "$@"
