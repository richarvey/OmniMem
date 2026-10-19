#!/usr/bin/env bash
# Attach build files to a Forgejo release from CI, creating the release if
# the tag has none yet.
#
#   forgejo-release.sh [--replace] [--prerelease] <tag> <title> <body> <file>...
#
# --replace deletes the tag's release and the tag itself first, then makes
# both afresh on $GITHUB_SHA: that's the rolling dev release a branch keeps.
# --prerelease marks a release this script creates as a pre-release.
#
# Reads GITHUB_SERVER_URL, GITHUB_REPOSITORY and GITHUB_SHA, which the runner
# sets, and FORGEJO_TOKEN, a token that may write releases and tags.
set -euo pipefail

replace=false
prerelease=false
while [ $# -gt 0 ]; do
    case "$1" in
        --replace) replace=true ;;
        --prerelease) prerelease=true ;;
        *) break ;;
    esac
    shift
done
if [ $# -lt 4 ]; then
    echo "usage: $0 [--replace] [--prerelease] <tag> <title> <body> <file>..." >&2
    exit 2
fi
tag="$1" title="$2" body="$3"
shift 3

repo_api="$GITHUB_SERVER_URL/api/v1/repos/$GITHUB_REPOSITORY"
auth="Authorization: token $FORGEJO_TOKEN"
response="$(mktemp)"
trap 'rm -f "$response"' EXIT

# A release's JSON starts with its id.
release_id() {
    sed -n 's/^{ *"id" *: *\([0-9]*\).*/\1/p' "$response"
}

# JSON string escaping for the title and body: backslashes, quotes, newlines.
json_string() {
    printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' | awk '{printf "%s%s", sep, $0; sep="\\n"}'
}

if curl -sf -H "$auth" "$repo_api/releases/tags/$tag" -o "$response"; then
    id="$(release_id)"
    if $replace; then
        curl -sSf -X DELETE -H "$auth" "$repo_api/releases/$id"
        id=""
    fi
else
    id=""
fi
if [ -z "$id" ]; then
    if $replace; then
        # A release's deletion leaves its tag, which may point at an older commit.
        curl -sS -o /dev/null -X DELETE -H "$auth" "$repo_api/tags/$tag" || true
    fi
    curl -sSf -H "$auth" -H "Content-Type: application/json" -o "$response" \
        -d "{\"tag_name\": \"$tag\", \"target_commitish\": \"$GITHUB_SHA\", \"name\": \"$(json_string "$title")\", \"body\": \"$(json_string "$body")\", \"prerelease\": $prerelease}" \
        "$repo_api/releases"
    id="$(release_id)"
fi
if [ -z "$id" ]; then
    echo "Couldn't read the release id for $tag" >&2
    exit 1
fi

for file in "$@"; do
    curl -sSf -H "$auth" -F "attachment=@$file" -o /dev/null \
        "$repo_api/releases/$id/assets?name=$(basename "$file")"
    echo "Attached $(basename "$file") to $tag"
done
echo "$GITHUB_SERVER_URL/$GITHUB_REPOSITORY/releases/tag/$tag"
