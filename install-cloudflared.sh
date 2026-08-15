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
  local os arch url tmpfile
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
  url="https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-${os}-${arch}"
  tmpfile="$(mktemp)"
  log "Installing cloudflared (${os}-${arch})..."
  curl --proto '=https' --tlsv1.2 -sSfL "$url" -o "$tmpfile"
  chmod +x "$tmpfile"
  if install -m 0755 "$tmpfile" /usr/local/bin/cloudflared 2>/dev/null; then :; else
    $SUDO install -m 0755 "$tmpfile" /usr/local/bin/cloudflared
  fi
  rm -f "$tmpfile"
  ok "cloudflared installed."
}

install_cloudflared_binary
cloudflared --version 2>/dev/null | head -1 || true
