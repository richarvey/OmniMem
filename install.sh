#!/usr/bin/env bash
#
# OmniMem installer — sets up a self-hosted semantic memory server for
# Claude Code using the pre-built Docker Hub images.
#
# Works on macOS and Linux. Run it directly:
#
#   curl -fsSL https://codeberg.org/ric_harvey/omnimem/raw/branch/main/install.sh | bash
#
# or download it first and run `bash install.sh`.
#
# Overrides (set as environment variables before running):
#   OMNIMEM_DIR       install directory            (default: ./omnimem)
#   OMNIMEM_BRANCH    repo branch to fetch from    (default: main)
#   OMNIMEM_RAW_BASE  base URL for raw files       (default: Codeberg raw URL)

set -euo pipefail

BRANCH="${OMNIMEM_BRANCH:-main}"
RAW_BASE="${OMNIMEM_RAW_BASE:-https://codeberg.org/ric_harvey/omnimem/raw/branch/${BRANCH}}"
COMPOSE_FILE_NAME="docker-compose.hub.yml"

# ---------------------------------------------------------------- output ----

if [ -t 1 ]; then
  BOLD=$(printf '\033[1m'); GREEN=$(printf '\033[32m')
  YELLOW=$(printf '\033[33m'); RED=$(printf '\033[31m'); RESET=$(printf '\033[0m')
else
  BOLD=''; GREEN=''; YELLOW=''; RED=''; RESET=''
fi

say()  { printf '%s\n' "${GREEN}==>${RESET} ${BOLD}$*${RESET}"; }
note() { printf '%s\n' "    $*"; }
warn() { printf '%s\n' "${YELLOW}warning:${RESET} $*" >&2; }
die()  { printf '%s\n' "${RED}error:${RESET} $*" >&2; exit 1; }

# When piped through `curl | bash`, stdin is the script itself — reattach
# prompts to the terminal so `read` still works. Probe in a subshell first:
# a failed redirection on `exec` would kill the script outright.
if [ ! -t 0 ] && (exec < /dev/tty) 2>/dev/null; then
  exec < /dev/tty
fi
INTERACTIVE=0
[ -t 0 ] && INTERACTIVE=1

# ask "question" "default" -> echoes the answer
ask() {
  local question="$1" default="$2" answer
  if [ "$INTERACTIVE" -eq 1 ]; then
    read -r -p "${question} [${default}]: " answer || answer=""
    printf '%s' "${answer:-$default}"
  else
    printf '%s' "$default"
  fi
}

# confirm "question" "y"|"n" -> returns 0 for yes
confirm() {
  local question="$1" default="$2" hint answer
  if [ "$default" = "y" ]; then hint="Y/n"; else hint="y/N"; fi
  if [ "$INTERACTIVE" -eq 1 ]; then
    read -r -p "${question} [${hint}]: " answer || answer=""
  else
    answer=""
  fi
  answer="${answer:-$default}"
  case "$answer" in
    [Yy]*) return 0 ;;
    *)     return 1 ;;
  esac
}

# ------------------------------------------------------------- utilities ----

gen_secret() {
  # Alphanumeric secret, safe to paste anywhere. Avoids head-of-pipe SIGPIPE
  # weirdness under `set -o pipefail` by trimming with substring expansion.
  local len="${1:-32}" raw=""
  while [ "${#raw}" -lt "$len" ]; do
    if command -v openssl >/dev/null 2>&1; then
      raw="${raw}$(openssl rand -base64 96 | LC_ALL=C tr -dc 'A-Za-z0-9')"
    else
      raw="${raw}$(dd if=/dev/urandom bs=1024 count=1 2>/dev/null | LC_ALL=C tr -dc 'A-Za-z0-9')"
    fi
  done
  printf '%s' "${raw:0:$len}"
}

fetch() {
  # fetch <url> <destination>
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$1" -o "$2"
  elif command -v wget >/dev/null 2>&1; then
    wget -q "$1" -O "$2"
  else
    die "neither curl nor wget is available — install one and re-run"
  fi
}

# --------------------------------------------------------------- checks ----

OS="$(uname -s)"
case "$OS" in
  Darwin) OS_NAME="macOS" ;;
  Linux)  OS_NAME="Linux" ;;
  *)      die "unsupported platform '$OS' — OmniMem's installer supports macOS and Linux" ;;
esac

say "OmniMem installer (${OS_NAME})"

if ! command -v docker >/dev/null 2>&1; then
  warn "Docker is not installed."
  if [ "$OS_NAME" = "macOS" ]; then
    note "Install Docker Desktop: https://docs.docker.com/desktop/setup/install/mac-install/"
    note "Or via Homebrew:        brew install --cask docker"
  else
    note "Install Docker Engine:  https://docs.docker.com/engine/install/"
    note "Quick install:          curl -fsSL https://get.docker.com | sh"
  fi
  die "install Docker, then re-run this script"
fi

if docker compose version >/dev/null 2>&1; then
  COMPOSE=(docker compose)
elif command -v docker-compose >/dev/null 2>&1; then
  COMPOSE=(docker-compose)
else
  warn "Docker is installed but Docker Compose is missing."
  if [ "$OS_NAME" = "macOS" ]; then
    note "Docker Desktop includes Compose — update or reinstall it:"
    note "https://docs.docker.com/desktop/setup/install/mac-install/"
  else
    note "Install the Compose plugin: https://docs.docker.com/compose/install/linux/"
  fi
  die "install Docker Compose, then re-run this script"
fi

if ! docker info >/dev/null 2>&1; then
  if [ "$OS_NAME" = "macOS" ]; then
    die "the Docker daemon isn't running — start Docker Desktop and re-run"
  else
    die "cannot talk to the Docker daemon — is it running, and is your user in the 'docker' group?"
  fi
fi

note "Docker and Compose found: $(docker --version)"

# ------------------------------------------------------------ directory ----

INSTALL_DIR="$(ask 'Where should OmniMem be installed?' "${OMNIMEM_DIR:-./omnimem}")"
mkdir -p "$INSTALL_DIR"
cd "$INSTALL_DIR"
INSTALL_DIR="$(pwd)"
say "Installing into ${INSTALL_DIR}"

# --------------------------------------------------------- compose file ----

if [ -f docker-compose.yml ]; then
  if confirm "docker-compose.yml already exists — overwrite it?" "n"; then
    fetch "${RAW_BASE}/${COMPOSE_FILE_NAME}" docker-compose.yml
    say "Refreshed docker-compose.yml"
  else
    note "Keeping the existing docker-compose.yml"
  fi
else
  fetch "${RAW_BASE}/${COMPOSE_FILE_NAME}" docker-compose.yml
  say "Downloaded docker-compose.yml (Docker Hub images, :latest tag)"
fi

mkdir -p backups
if [ ! -f feeds.yml ]; then
  cat > feeds.yml << 'EOF'
# RSS feeds for the knowledge base. The worker picks up changes to this file
# automatically — no restart needed. Leave it empty to skip RSS ingestion.
#
# feeds:
#   - url: https://blog.rust-lang.org/feed.xml
#     name: Rust Official Blog
#     topics: [rust, systems, language]
EOF
  note "Created a starter feeds.yml (edit it to add RSS feeds)"
fi

# ------------------------------------------------------------- questions ----

if [ -f .env ]; then
  if ! confirm ".env already exists — overwrite it with fresh settings?" "n"; then
    note "Keeping the existing .env — configuration questions skipped"
    KEEP_ENV=1
  else
    KEEP_ENV=0
  fi
else
  KEEP_ENV=0
fi

if [ "$KEEP_ENV" -eq 0 ]; then
  say "Configuration"

  # Valkey password — required, so generate unless the user wants their own.
  if confirm "Generate a secure Valkey database password?" "y"; then
    VALKEY_PASSWORD="$(gen_secret 32)"
    note "Generated Valkey password"
  else
    VALKEY_PASSWORD="$(ask 'Enter a Valkey password' '')"
    [ -n "$VALKEY_PASSWORD" ] || die "Valkey password cannot be empty"
  fi

  # Network exposure changes how the MCP port is bound on the host.
  MCP_BIND="127.0.0.1"
  MCP_AUTH_TOKEN=""
  if confirm "Open the MCP server to your network (other machines can connect)?" "n"; then
    MCP_BIND="0.0.0.0"
    warn "the MCP server will listen on all interfaces"
    if confirm "Generate an auth token so only clients with the token can connect? (recommended)" "y"; then
      MCP_AUTH_TOKEN="$(gen_secret 48)"
      note "Generated MCP auth token"
    fi
  fi

  # Web UI login reuses the OAuth admin credentials.
  OAUTH_ADMIN_USER=""
  OAUTH_ADMIN_PASSWORD=""
  if confirm "Protect the web dashboard with a login page?" "y"; then
    OAUTH_ADMIN_USER="$(ask 'Admin username' 'admin')"
    OAUTH_ADMIN_PASSWORD="$(gen_secret 24)"
    note "Generated web dashboard password"
  fi

  # Optional — unlocks Haiku-powered RSS summaries and fact extraction.
  ANTHROPIC_API_KEY="$(ask 'Anthropic API key (optional, Enter to skip)' '')"

  # ------------------------------------------------------------- .env ----

  cat > .env << EOF
# OmniMem configuration — generated by install.sh on $(date '+%Y-%m-%d %H:%M')
# Full reference: https://codeberg.org/ric_harvey/omnimem/src/branch/${BRANCH}/.env.example

# --- Valkey (vector database) ---
VALKEY_HOST=valkey
VALKEY_PORT=6379
VALKEY_PASSWORD=${VALKEY_PASSWORD}

# --- MCP server ---
MCP_PORT=8765
# Host interface the MCP port binds to: 127.0.0.1 = this machine only,
# 0.0.0.0 = reachable from your network.
MCP_BIND=${MCP_BIND}
MCP_TRANSPORT=http
$(if [ -n "$MCP_AUTH_TOKEN" ]; then
    printf 'MCP_AUTH_TOKEN=%s\n' "$MCP_AUTH_TOKEN"
  else
    printf '# MCP_AUTH_TOKEN=          # Set to require a bearer token on the MCP endpoint\n'
  fi)
# --- Web dashboard ---
WEB_PORT=8080
WEB_BIND=127.0.0.1
$(if [ -n "$OAUTH_ADMIN_USER" ]; then
    printf 'OAUTH_ADMIN_USER=%s\nOAUTH_ADMIN_PASSWORD=%s\n' "$OAUTH_ADMIN_USER" "$OAUTH_ADMIN_PASSWORD"
  else
    printf '# OAUTH_ADMIN_USER=admin   # Set user + password to enable the dashboard login page\n# OAUTH_ADMIN_PASSWORD=\n'
  fi)
# --- Anthropic API (optional: RSS summaries, fact extraction, contradiction checks) ---
ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY}

# --- Sensible defaults (tune later if needed) ---
EMBEDDING_MODEL=all-MiniLM-L6-v2
INGEST_MODE=full
MEMORY_RECALL_TOP_K=5
DEPRIORITISED_WEIGHT=0.2
RECENCY_DECAY_DAYS=90
RECALL_EXPAND_QUERIES=false
RSS_SCHEDULE_HOURS=6
RSS_MAX_ARTICLES_PER_FEED=20
DEDUP_SIMILARITY_THRESHOLD=0.92
CONTRADICTION_SIMILARITY_THRESHOLD=0.7
STALE_MEMORY_DAYS=30
AUTO_MAINTENANCE_INTERVAL=10
BACKUP_DIR=/app/backups
EOF
  chmod 600 .env
  say "Wrote .env (permissions 600)"
fi

# ----------------------------------------------------------------- start ----

if confirm "Pull the images and start OmniMem now?" "y"; then
  say "Pulling images (first pull is ~2 GB, mostly PyTorch — grab a coffee)"
  "${COMPOSE[@]}" pull
  say "Starting services"
  "${COMPOSE[@]}" up -d
  STARTED=1
else
  STARTED=0
fi

# --------------------------------------------------------------- summary ----

echo
say "OmniMem is installed in ${INSTALL_DIR}"
if [ "${KEEP_ENV}" -eq 0 ]; then
  note "MCP server:    http://localhost:8765/mcp $([ "$MCP_BIND" = "0.0.0.0" ] && printf '(also reachable on your network)')"
  note "Web dashboard: http://localhost:8080"
  if [ -n "${OAUTH_ADMIN_USER}" ]; then
    note "Dashboard login: ${OAUTH_ADMIN_USER} / ${OAUTH_ADMIN_PASSWORD}"
  fi
  if [ -n "${MCP_AUTH_TOKEN}" ]; then
    note "MCP auth token:  ${MCP_AUTH_TOKEN}"
  fi
  note "All credentials are saved in ${INSTALL_DIR}/.env — keep it safe."
fi
if [ "$STARTED" -eq 1 ]; then
  note "Check status:  cd ${INSTALL_DIR} && ${COMPOSE[*]} ps"
else
  note "Start later:   cd ${INSTALL_DIR} && ${COMPOSE[*]} up -d"
fi
echo
note "Connect Claude Code to it with:"
if [ "${KEEP_ENV}" -eq 0 ] && [ -n "${MCP_AUTH_TOKEN}" ]; then
  note "  claude mcp add --transport http omnimem http://localhost:8765/mcp \\"
  note "    --header \"Authorization: Bearer ${MCP_AUTH_TOKEN}\""
else
  note "  claude mcp add --transport http omnimem http://localhost:8765/mcp"
fi
note "Guides: https://codeberg.org/ric_harvey/omnimem/src/branch/${BRANCH}/guides"
