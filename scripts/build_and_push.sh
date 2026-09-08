#!/usr/bin/env bash
# Build and push the three OmniMem images to Docker Hub.
#
# Images:
#   richarvey/omnimem-mcp  ← mcp_server/
#   richarvey/omnimem-web  ← web_ui/
#   richarvey/omnimem-rss  ← rss_worker/ + mcp_server/memory/
#
# Multi-arch (linux/amd64 + linux/arm64) via buildx. Prompts for a tag —
# never publishes :latest.

set -euo pipefail

REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

NAMESPACE="richarvey"
PLATFORMS="linux/amd64,linux/arm64"
BUILDER="omnimem-builder"

NO_CACHE=""
for arg in "$@"; do
  case "$arg" in
    --no-cache) NO_CACHE="--no-cache" ;;
    -h|--help)
      echo "Usage: $0 [--no-cache]"
      exit 0
      ;;
    *)
      echo "error: unknown argument '$arg'" >&2
      exit 1
      ;;
  esac
done

# components: "image_name:context_dir:dockerfile"
# web_ui's and rss_worker's Dockerfiles reference paths outside their
# directories (mcp_server/memory), so they build from the repo root with -f.
COMPONENTS=(
  "omnimem-mcp:mcp_server:mcp_server/Dockerfile"
  "omnimem-web:.:web_ui/Dockerfile"
  "omnimem-rss:.:rss_worker/Dockerfile"
)

read -rp "Tag to publish (e.g. 4.0.1): " TAG
if [[ -z "${TAG// }" ]]; then
  echo "error: tag is required" >&2
  exit 1
fi
if [[ "$TAG" == "latest" ]]; then
  echo "error: refusing to use 'latest' as the version tag (use the 'also tag as latest' prompt instead)" >&2
  exit 1
fi

read -rp "Also tag these images as ':latest'? [y/N] " tag_latest
tag_latest_lc=$(printf '%s' "$tag_latest" | tr '[:upper:]' '[:lower:]')
if [[ "$tag_latest_lc" == "y" || "$tag_latest_lc" == "yes" ]]; then
  ALSO_LATEST=1
else
  ALSO_LATEST=0
fi

echo
echo "About to build and push the following images for tag '$TAG':"
for entry in "${COMPONENTS[@]}"; do
  IFS=':' read -r c_image c_context c_dockerfile <<< "$entry"
  echo "  - $NAMESPACE/$c_image:$TAG  (context: $c_context, dockerfile: $c_dockerfile)"
done
[[ "$ALSO_LATEST" == "1" ]] && echo "Also tagging each image as ':latest'"
echo "Platforms: $PLATFORMS"
echo
read -rp "Proceed? [y/N] " confirm
confirm_lc=$(printf '%s' "$confirm" | tr '[:upper:]' '[:lower:]')
[[ "$confirm_lc" == "y" || "$confirm_lc" == "yes" ]] || { echo "aborted"; exit 0; }

# Ensure a buildx builder exists and is selected
if ! docker buildx inspect "$BUILDER" >/dev/null 2>&1; then
  echo "Creating buildx builder '$BUILDER'..."
  docker buildx create --name "$BUILDER" --driver docker-container --use
else
  docker buildx use "$BUILDER"
fi
docker buildx inspect --bootstrap >/dev/null

# Confirm we're logged in to Docker Hub (push will fail otherwise)
if ! docker info 2>/dev/null | grep -q "Username:"; then
  echo "You don't appear to be logged in to Docker Hub."
  read -rp "Run 'docker login' now? [Y/n] " do_login
  do_login_lc=$(printf '%s' "$do_login" | tr '[:upper:]' '[:lower:]')
  if [[ -z "$do_login_lc" || "$do_login_lc" == "y" || "$do_login_lc" == "yes" ]]; then
    docker login
  fi
fi

for entry in "${COMPONENTS[@]}"; do
  IFS=':' read -r image context dockerfile <<< "$entry"
  full="$NAMESPACE/$image:$TAG"
  tag_args=(--tag "$full")
  if [[ "$ALSO_LATEST" == "1" ]]; then
    tag_args+=(--tag "$NAMESPACE/$image:latest")
  fi
  echo
  latest_note=""
  [[ "$ALSO_LATEST" == "1" ]] && latest_note=" (+ :latest)"
  echo "==> Building and pushing $full$latest_note"
  docker buildx build \
    --platform "$PLATFORMS" \
    $NO_CACHE \
    "${tag_args[@]}" \
    --file "$dockerfile" \
    --push \
    "$context"
done

echo
echo "Done. Published:"
for entry in "${COMPONENTS[@]}"; do
  echo "  $NAMESPACE/${entry%%:*}:$TAG"
  [[ "$ALSO_LATEST" == "1" ]] && echo "  $NAMESPACE/${entry%%:*}:latest"
done
