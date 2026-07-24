#!/usr/bin/env bash
#
# OmniMem installer — sets up a self-hosted semantic memory server for
# Claude Code using the pre-built Docker Hub images.
#
# Works on macOS and Linux. Run it directly:
#
#   curl -fsSL https://code.squarecows.com/ric/omnimem/raw/branch/main/install.sh | bash
#
# or download it first and run `bash install.sh`.
#
# Overrides (set as environment variables before running):
#   OMNIMEM_DIR       install directory            (default: ./omnimem)
#   OMNIMEM_BRANCH    repo branch to fetch from    (default: main)
#   OMNIMEM_RAW_BASE  base URL for raw files       (default: Squarecows raw URL)

set -euo pipefail

BRANCH="${OMNIMEM_BRANCH:-main}"
RAW_BASE="${OMNIMEM_RAW_BASE:-https://code.squarecows.com/ric/omnimem/raw/branch/${BRANCH}}"
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

STEP=0
STEP_TOTAL=6
step() {
  STEP=$((STEP + 1))
  printf '\n%s\n' "${GREEN}==>${RESET} ${BOLD}[${STEP}/${STEP_TOTAL}] $*${RESET}"
}

# When piped through `curl | bash`, stdin is the script itself, and bash is
# still reading it — NEVER `exec < /dev/tty` here, or bash starts reading the
# rest of the script from the keyboard and appears to hang. Instead, each
# prompt reads from /dev/tty directly, leaving the script stream alone.
INTERACTIVE=0
PROMPT_TTY=""
if [ -t 0 ]; then
  INTERACTIVE=1
elif (exec < /dev/tty) 2>/dev/null; then   # probe only, in a subshell
  INTERACTIVE=1
  PROMPT_TTY="/dev/tty"
fi

# prompt_read "prompt text" varname — read a reply from the terminal
prompt_read() {
  if [ -n "$PROMPT_TTY" ]; then
    read -r -p "$1" "$2" < "$PROMPT_TTY"
  else
    read -r -p "$1" "$2"
  fi
}

# ask "question" "default" -> echoes the answer
ask() {
  local question="$1" default="$2" answer=""
  if [ "$INTERACTIVE" -eq 1 ]; then
    prompt_read "${question} [${default}]: " answer || answer=""
    printf '%s' "${answer:-$default}"
  else
    printf '%s' "$default"
  fi
}

# confirm "question" "y"|"n" -> returns 0 for yes
confirm() {
  local question="$1" default="$2" hint answer=""
  if [ "$default" = "y" ]; then hint="Y/n"; else hint="y/N"; fi
  if [ "$INTERACTIVE" -eq 1 ]; then
    prompt_read "${question} [${hint}]: " answer || answer=""
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

# wait_for_url <label> <url> — poll until the URL answers HTTP at all (any
# status code counts: 401/406 still means the service is up), printing a dot
# every 2s so there's visible progress. Times out after ~5 minutes.
wait_for_url() {
  local label="$1" url="$2" tries=150 rc code
  printf '    %s ' "$label"
  while [ "$tries" -gt 0 ]; do
    if command -v curl >/dev/null 2>&1; then
      code="$(curl -s -o /dev/null -w '%{http_code}' --max-time 2 "$url" 2>/dev/null || true)"
      if [ "$code" != "000" ] && [ -n "$code" ]; then
        printf ' %sready%s\n' "$GREEN" "$RESET"
        return 0
      fi
    else
      rc=0
      wget -q -T 2 -t 1 -O /dev/null "$url" 2>/dev/null || rc=$?
      # 0 = OK, 8 = server answered with an error status — either way it's up
      if [ "$rc" -eq 0 ] || [ "$rc" -eq 8 ]; then
        printf ' %sready%s\n' "$GREEN" "$RESET"
        return 0
      fi
    fi
    printf '.'
    sleep 2
    tries=$((tries - 1))
  done
  printf ' %sstill starting%s\n' "$YELLOW" "$RESET"
  return 1
}

# --------------------------------------------------------------- checks ----

OS="$(uname -s)"
case "$OS" in
  Darwin) OS_NAME="macOS" ;;
  Linux)  OS_NAME="Linux" ;;
  *)      die "unsupported platform '$OS' — OmniMem's installer supports macOS and Linux" ;;
esac

say "OmniMem installer (${OS_NAME})"

step "Checking prerequisites"

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

step "Choosing an install directory"

INSTALL_DIR="$(ask 'Where should OmniMem be installed?' "${OMNIMEM_DIR:-./omnimem}")"
mkdir -p "$INSTALL_DIR"
cd "$INSTALL_DIR"
INSTALL_DIR="$(pwd)"
note "Installing into ${INSTALL_DIR}"

# --------------------------------------------------------- compose file ----

step "Fetching configuration files"

if [ -f docker-compose.yml ]; then
  if confirm "docker-compose.yml already exists — overwrite it?" "n"; then
    note "Downloading docker-compose.yml from ${RAW_BASE} ..."
    fetch "${RAW_BASE}/${COMPOSE_FILE_NAME}" docker-compose.yml
    note "Refreshed docker-compose.yml"
  else
    note "Keeping the existing docker-compose.yml"
  fi
else
  note "Downloading docker-compose.yml from ${RAW_BASE} ..."
  fetch "${RAW_BASE}/${COMPOSE_FILE_NAME}" docker-compose.yml
  note "Downloaded docker-compose.yml (Docker Hub images, :latest tag)"
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

step "Configuration"

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
# Full reference: https://code.squarecows.com/ric/omnimem/src/branch/${BRANCH}/.env.example

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
  note "Wrote .env (permissions 600)"
fi

# ----------------------------------------------------------------- start ----

step "Pulling images and starting services"

if confirm "Pull the images and start OmniMem now?" "y"; then
  note "Pulling images (first pull is ~2 GB, mostly PyTorch — grab a coffee)"
  "${COMPOSE[@]}" pull < /dev/null
  note "Starting services ..."
  "${COMPOSE[@]}" up -d < /dev/null
  STARTED=1
else
  note "Skipped — start later with: cd ${INSTALL_DIR} && ${COMPOSE[*]} up -d"
  STARTED=0
fi

# ------------------------------------------------------------- readiness ----

step "Waiting for services to come up"

if [ "$STARTED" -eq 1 ]; then
  # Ports may come from an existing .env the user chose to keep.
  MCP_PORT_VAL="$(sed -n 's/^MCP_PORT=//p' .env 2>/dev/null | tail -1)"
  WEB_PORT_VAL="$(sed -n 's/^WEB_PORT=//p' .env 2>/dev/null | tail -1)"
  MCP_PORT_VAL="${MCP_PORT_VAL:-8765}"
  WEB_PORT_VAL="${WEB_PORT_VAL:-8080}"

  note "First start downloads the embedding model inside the containers,"
  note "so this can take a few minutes. Each dot is a health check:"
  ALL_READY=1
  if ! wait_for_url "MCP server    " "http://localhost:${MCP_PORT_VAL}/mcp"; then ALL_READY=0; fi
  if ! wait_for_url "Web dashboard " "http://localhost:${WEB_PORT_VAL}/"; then ALL_READY=0; fi
  if [ "$ALL_READY" -eq 0 ]; then
    warn "some services are still starting — watch their logs with:"
    note "  cd ${INSTALL_DIR} && ${COMPOSE[*]} logs -f"
  fi
else
  note "Skipped — services were not started"
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
note "Guides: https://code.squarecows.com/ric/omnimem/src/branch/${BRANCH}/guides"
