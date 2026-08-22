#!/usr/bin/env bash
# install-minio.sh — managed MinIO for Eco's S3-compatible object storage.
#
# In development MinIO belongs to the current user. In an LXC/production
# environment it is a systemd service in the dedicated storage CT. App CTs
# never run this script in production; they consume a client.env supplied by
# `eco up` from that dedicated MinIO CT.

set -euo pipefail

QUIET=false
ENSURE_ONLY=false
RESET_REQUESTED=false

for arg in "$@"; do
  case "$arg" in
    --ensure) ENSURE_ONLY=true ;;
    --reset) RESET_REQUESTED=true ;;
    --quiet) QUIET=true ;;
    -h|--help|help)
      cat <<'EOF'
Usage: eco install minio [--ensure] [--reset] [--quiet]

Installs and starts Eco-managed MinIO on this machine/CT. `--ensure` is for
`eco up`: it performs the same idempotent work without printing credentials.
`--reset` stops MinIO and permanently removes its objects and credentials
before installing a new empty instance. It is only used after explicit Eco confirmation.
EOF
      exit 0
      ;;
    *) echo "Unknown option: $arg" >&2; exit 1 ;;
  esac
done

BOLD='\033[1m'; CYAN='\033[0;36m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RESET='\033[0m'
log() { $QUIET || echo -e "${CYAN}$*${RESET}"; }
ok() { $QUIET || echo -e "${GREEN}$*${RESET}"; }
warn() { echo -e "${YELLOW}$*${RESET}" >&2; }
fail() { echo -e "\033[0;31m$*${RESET}" >&2; exit 1; }

if [[ "${EUID}" -eq 0 ]]; then SUDO=""; else SUDO="sudo"; fi
# A developer normally has no write permission to /usr/local/bin and may not
# have passwordless sudo. Keep the user-local install discoverable for this
# run and all later Eco commands without requiring elevated privileges.
LOCAL_BIN="${HOME}/.local/bin"
export PATH="${LOCAL_BIN}:${PATH}"

detect_deploy_mode() {
  if [[ -n "${ECO_DEPLOY_MODE:-}" ]]; then printf '%s' "$ECO_DEPLOY_MODE"; return; fi
  if command -v systemd-detect-virt >/dev/null 2>&1; then
    case "$(systemd-detect-virt --container 2>/dev/null || true)" in lxc|lxc-libvirt) printf 'prod'; return;; esac
  fi
  case "$(cat /run/systemd/container 2>/dev/null || true)" in lxc|lxc-libvirt) printf 'prod'; return;; esac
  printf 'dev'
}

DEPLOY_MODE="$(detect_deploy_mode)"
if [[ "$DEPLOY_MODE" == "prod" ]]; then
  MINIO_HOME="/var/lib/eco/minio"
  CREDENTIALS_FILE="/etc/eco/minio.env"
  CLIENT_FILE="/etc/eco/minio-client.env"
  # App CTs reach this dedicated storage CT over the private bridge.
  # The console remains loopback-only and Eco never creates public ingress.
  MINIO_API_ADDRESS=":9000"
else
  MINIO_HOME="${HOME}/.eco-minio"
  CREDENTIALS_FILE="${MINIO_HOME}/credentials.env"
  CLIENT_FILE="${MINIO_HOME}/client.env"
  MINIO_API_ADDRESS="127.0.0.1:9000"
fi

reset_minio() {
  if ! $RESET_REQUESTED; then
    return 0
  fi
  warn "Resetting MinIO: all stored objects and credentials will be removed."
  if [[ "$DEPLOY_MODE" == "prod" ]] && command -v systemctl >/dev/null 2>&1; then
    $SUDO systemctl stop eco-minio >/dev/null 2>&1 || true
  fi
  if [[ "$DEPLOY_MODE" == "prod" ]]; then
    $SUDO rm -rf "$MINIO_HOME" "$CREDENTIALS_FILE" "$CLIENT_FILE"
  else
    rm -rf "$MINIO_HOME"
  fi
}

install_minio_binary() {
  if command -v minio >/dev/null 2>&1; then ok "MinIO already installed."; return; fi
  local os arch url tmpfile
  case "$(uname -s)" in Darwin) os=darwin;; Linux) os=linux;; *) fail "Unsupported OS: $(uname -s)";; esac
  case "$(uname -m)" in x86_64|amd64) arch=amd64;; arm64|aarch64) arch=arm64;; *) fail "Unsupported architecture: $(uname -m)";; esac
  url="https://dl.min.io/server/minio/release/${os}-${arch}/minio"
  tmpfile="$(mktemp)"
  log "Installing MinIO (${os}-${arch})..."
  # A stalled release redirect must fail with a useful error instead of
  # leaving `eco install minio` hanging forever on a developer workstation.
  curl --proto '=https' --tlsv1.2 --connect-timeout 15 --max-time 180 -sSfL "$url" -o "$tmpfile"
  chmod +x "$tmpfile"
  if install -m 0755 "$tmpfile" /usr/local/bin/minio 2>/dev/null; then
    :
  elif command -v sudo >/dev/null 2>&1 && sudo -n true 2>/dev/null; then
    sudo install -m 0755 "$tmpfile" /usr/local/bin/minio
  else
    mkdir -p "$LOCAL_BIN"
    install -m 0755 "$tmpfile" "$LOCAL_BIN/minio"
    ok "Installed MinIO in ${LOCAL_BIN} (no sudo required)."
  fi
  rm -f "$tmpfile"
}

ensure_download_prerequisites() {
  # macOS's system curl uses Secure Transport and the macOS trust store; it
  # intentionally does not provide Debian's /etc/ssl/certs/ca-certificates.crt.
  # Requiring that Linux-specific path made a fully working macOS 12 host try
  # (and fail) to invoke apt-get before MinIO could be downloaded.
  if command -v curl >/dev/null 2>&1; then
    case "$(uname -s)" in
      Darwin) return ;;
      Linux) [[ -f /etc/ssl/certs/ca-certificates.crt || -d /etc/ssl/certs ]] && return ;;
    esac
  fi
  if ! command -v apt-get >/dev/null 2>&1; then
    fail "MinIO bootstrap requires curl and CA certificates; no supported package manager was found."
  fi
  log "Installing MinIO download prerequisites..."
  $SUDO apt-get update
  $SUDO env DEBIAN_FRONTEND=noninteractive apt-get install -y curl ca-certificates
}

write_credentials_once() {
  if [[ -f "$CREDENTIALS_FILE" ]]; then return; fi
  local password
  # A fixed-size read avoids `tr | head` under pipefail: head closes the
  # pipe early, turning a successful random-password generation into a
  # misleading broken-pipe failure on fresh CTs.
  password="$(od -An -N32 -tx1 /dev/urandom | tr -d ' \n')"
  if [[ "$DEPLOY_MODE" == "prod" ]]; then $SUDO mkdir -p /etc/eco "$MINIO_HOME/data"; else mkdir -p "$MINIO_HOME/data"; fi
  {
    echo 'MINIO_ROOT_USER=ecoadmin'
    echo "MINIO_ROOT_PASSWORD=${password}"
  } | { if [[ "$DEPLOY_MODE" == "prod" ]]; then $SUDO tee "$CREDENTIALS_FILE" >/dev/null; else cat > "$CREDENTIALS_FILE"; fi; }
  if [[ "$DEPLOY_MODE" == "prod" ]]; then $SUDO chmod 600 "$CREDENTIALS_FILE"; else chmod 600 "$CREDENTIALS_FILE"; fi
}

write_client_credentials() {
  # shellcheck disable=SC1090
  source "$CREDENTIALS_FILE"
  {
    echo 'S3_ENDPOINT=http://127.0.0.1:9000'
    echo 'S3_REGION=us-east-1'
    echo "S3_ACCESS_KEY=${MINIO_ROOT_USER}"
    echo "S3_SECRET_KEY=${MINIO_ROOT_PASSWORD}"
  } | { if [[ "$DEPLOY_MODE" == "prod" ]]; then $SUDO tee "$CLIENT_FILE" >/dev/null; else cat > "$CLIENT_FILE"; fi; }
  if [[ "$DEPLOY_MODE" == "prod" ]]; then $SUDO chmod 600 "$CLIENT_FILE"; else chmod 600 "$CLIENT_FILE"; fi
}

is_healthy() { curl -fsS -o /dev/null http://127.0.0.1:9000/minio/health/live 2>/dev/null; }

ensure_minio_running() {
  if [[ "$DEPLOY_MODE" == "prod" ]] && command -v systemctl >/dev/null 2>&1; then
    # Rewrite/restart the managed unit even when the health endpoint happens
    # to be up: an older Eco version may have bound MinIO to loopback only,
    # which would make it invisible to the other CTs on the bridge.
    local minio_bin
    minio_bin="$(command -v minio)"
    $SUDO tee /etc/systemd/system/eco-minio.service >/dev/null <<EOF
[Unit]
Description=Eco managed MinIO S3 storage
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
EnvironmentFile=${CREDENTIALS_FILE}
ExecStart=${minio_bin} server ${MINIO_HOME}/data --address ${MINIO_API_ADDRESS} --console-address 127.0.0.1:9001
Restart=always
RestartSec=2

[Install]
WantedBy=multi-user.target
EOF
    $SUDO systemctl daemon-reload
    $SUDO systemctl enable eco-minio >/dev/null
    $SUDO systemctl restart eco-minio
  else
    if is_healthy; then ok 'MinIO already running.'; return; fi
    mkdir -p "$MINIO_HOME/data"
    # shellcheck disable=SC1090
    source "$CREDENTIALS_FILE"
    log 'Starting MinIO...'
    nohup env MINIO_ROOT_USER="$MINIO_ROOT_USER" MINIO_ROOT_PASSWORD="$MINIO_ROOT_PASSWORD" \
      minio server "$MINIO_HOME/data" --address "$MINIO_API_ADDRESS" --console-address 127.0.0.1:9001 \
      > "$MINIO_HOME/minio.log" 2>&1 &
    disown || true
  fi

  local attempt
  for attempt in {1..15}; do
    if is_healthy; then ok 'MinIO running.'; return; fi
    sleep 1
  done
  fail "MinIO did not become healthy. Check ${MINIO_HOME}/minio.log or systemctl status eco-minio."
}

print_summary() {
  # shellcheck disable=SC1090
  source "$CREDENTIALS_FILE"
  echo ""
  echo -e "${BOLD}Eco MinIO ready${RESET}"
  echo "  API endpoint: http://127.0.0.1:9000"
  echo "  Console:      http://127.0.0.1:9001"
  echo "  Access key:   ${MINIO_ROOT_USER}"
  echo "  Secret key:   ${MINIO_ROOT_PASSWORD}"
  echo "  Credentials:  ${CREDENTIALS_FILE}"
}

main() {
  reset_minio
  ensure_download_prerequisites
  install_minio_binary
  write_credentials_once
  write_client_credentials
  ensure_minio_running
  if ! $ENSURE_ONLY; then print_summary; fi
}

main
