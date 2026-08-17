#!/usr/bin/env bash
# install-onnxruntime.sh — managed onnxruntime for Eco's RAG/embedding services.
#
# The RAG domain builds with ort's load-dynamic feature: the Rust binary links
# nothing and loads libonnxruntime.so at runtime via ORT_DYLIB_PATH. The
# prebuilt onnxruntime that ort's download-binaries feature ships is built
# against glibc >= 2.38 and fails to link on Debian 12 CTs (glibc 2.36), so
# Eco provisions the official onnxruntime 1.28 linux-x64 shared library here
# instead -- one per CT, shared by every estate that composes the rag domain.
#
# Install location: /opt/eco-tools/libonnxruntime.so (same dir Eco uses for
# the shared sqlx binary). Services declare the runtime token `onnxruntime` in
# ecompose.yml; configure.sh fills ORT_DYLIB_PATH from this path.

set -euo pipefail

QUIET=false
ENSURE_ONLY=false

for arg in "$@"; do
  case "$arg" in
    --ensure) ENSURE_ONLY=true ;;
    --quiet) QUIET=true ;;
    -h|--help|help)
      cat <<'EOF'
Usage: eco install onnxruntime [--ensure] [--quiet]

Installs the onnxruntime shared library used by Eco RAG/embedding services.
`--ensure` is for `eco up`: performs the same idempotent work quietly.

On Linux/CTs: downloads the official onnxruntime 1.28 linux-x64 release and
places libonnxruntime.so at /opt/eco-tools/libonnxruntime.so.
On macOS (local dev): installs via Homebrew (brew install onnxruntime).
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

VERSION="1.28.0"
INSTALL_DIR="/opt/eco-tools"
LIB_PATH="${INSTALL_DIR}/libonnxruntime.so"

need_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    fail "Required command not found: $1"
  fi
}

install_linux() {
  need_cmd curl
  need_cmd tar
  if [[ -f "$LIB_PATH" ]]; then
    ok "onnxruntime already installed: ${LIB_PATH}"
    return 0
  fi
  mkdir -p "$INSTALL_DIR"
  local tmp
  tmp="$(mktemp -d)"
  log "Downloading onnxruntime ${VERSION} (linux-x64)..."
  local url="https://github.com/microsoft/onnxruntime/releases/download/v${VERSION}/onnxruntime-linux-x64-${VERSION}.tgz"
  curl -fsSL -o "${tmp}/ort.tgz" "$url" \
    || fail "Failed to download ${url}"
  tar -xzf "${tmp}/ort.tgz" -C "$tmp"
  local src="${tmp}/onnxruntime-linux-x64-${VERSION}/lib/libonnxruntime.so"
  [[ -f "$src" ]] || fail "libonnxruntime.so not found in downloaded release"
  cp "$src" "$LIB_PATH"
  chmod 755 "$LIB_PATH"
  rm -rf "$tmp"
  ok "Installed onnxruntime: ${LIB_PATH}"
}

install_macos() {
  need_cmd brew
  if brew list onnxruntime >/dev/null 2>&1; then
    ok "onnxruntime already installed via Homebrew."
    return 0
  fi
  log "Installing onnxruntime via Homebrew..."
  brew install onnxruntime
  ok "onnxruntime installed. Set ORT_DYLIB_PATH to the brew library when running rag locally."
}

if [[ "$(uname -s)" == "Darwin" ]]; then
  install_macos
else
  install_linux
fi

$QUIET || true
