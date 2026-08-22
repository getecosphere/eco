#!/usr/bin/env bash
# install-cloudflared.sh — managed cloudflared for Eco.
#
# Used by `eco serve` (temporary public *.getecosphere.com tunnels) and by
# `eco proxy`/`eco up` inside the proxy CT. Downloads the prebuilt binary from
# Cloudflare's release feed — no package manager needed, so it works the same
# on macOS dev machines and Linux CTs.

set -euo pipefail

log() { echo -e "\033[0;36m$*\033[0m"; }
ok() { echo -e "\033[0;32m$*\033[0m"; }
warn() { echo -e "\033[1;33m$*\033[0m" >&2; }
fail() { echo -e "\033[0;31m$*\033[0m" >&2; exit 1; }

if [[ "${EUID}" -eq 0 ]]; then SUDO=""; else SUDO="sudo"; fi

install_cloudflared_binary() {
  if command -v cloudflared >/dev/null 2>&1; then
    ok "cloudflared already installed ($(cloudflared --version 2>/dev/null | head -1 || true))."
    return
  fi
  local os arch url tmpfile ext
  case "$(uname -s)" in
    Darwin) os=darwin ;;
    Linux) os=linux ;;
    *) fail "Unsupported OS: $(uname -s)" ;;
  esac
  case "$(uname -m)" in
    x86_64|amd64) arch=amd64 ;;
    arm64|aarch64) arch=arm64 ;;
    *) fail "Unsupported architecture: $(uname -m)" ;;
  esac
  ext=""
  [[ "$os" == "darwin" ]] && ext=".tgz"
  url="https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-${os}-${arch}${ext}"
  tmpfile="$(mktemp)"
  log "Installing cloudflared (${os}-${arch})..."
  curl --proto '=https' --tlsv1.2 -sSfL "$url" -o "$tmpfile"
  if [[ "$os" == "darwin" ]]; then
    mkdir -p /tmp/cloudflared-install
    tar -xzf "$tmpfile" -C /tmp/cloudflared-install
    if [[ -f /tmp/cloudflared-install/cloudflared ]]; then
      tmpfile="/tmp/cloudflared-install/cloudflared"
    else
      find /tmp/cloudflared-install -type f -name cloudflared -exec mv {} /tmp/cloudflared-install/cloudflared \; 2>/dev/null || true
      tmpfile="/tmp/cloudflared-install/cloudflared"
    fi
  fi
  chmod +x "$tmpfile"
  # Prefer /usr/local/bin, but fall back to ~/.local/bin (on PATH) so a
  # non-root / detached (no terminal for sudo) install still works — the
  # `eco serve` tunnel needs cloudflared without prompting for a password.
  local bin_dir="/usr/local/bin"
  if [[ ! -w "$bin_dir" ]]; then
    bin_dir="$HOME/.local/bin"
  fi
  mkdir -p "$bin_dir"
  install -m 0755 "$tmpfile" "$bin_dir/cloudflared"
  rm -rf /tmp/cloudflared-install
  rm -f "$tmpfile"
  ok "cloudflared installed to $bin_dir."
}

install_cloudflared_binary
cloudflared --version 2>/dev/null | head -1 || true
