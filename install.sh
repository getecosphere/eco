#!/usr/bin/env sh
# install.sh — install the eco binary via curl.
#
#   curl -fsSL https://github.com/ecosphere-creator/eco/releases/latest/download/install.sh | sh
#
# Downloads the matching static binary for the caller's OS/arch from GitHub
# Releases, installs it to /usr/local/bin (falling back to ~/.local/bin), and
# prints eco help.
#
# Override:
#   ECO_INSTALL_BASE     download origin (default: GitHub Releases latest)
#   ECO_INSTALL_URL      full download URL (highest priority)
#   ECO_INSTALL_VERSION  pinned version tag (default: latest)
#   ECO_INSTALL_DIR      install directory (default: /usr/local/bin or ~/.local/bin)
set -eu

BASE_URL="${ECO_INSTALL_BASE:-https://github.com/ecosphere-creator/eco/releases/latest/download}"
VERSION="${ECO_INSTALL_VERSION:-latest}"
TARGET="${ECO_INSTALL_TARGET:-}"

# --- detect OS / arch -------------------------------------------------------
os="$(uname -s 2>/dev/null || printf 'unknown')"
arch="$(uname -m 2>/dev/null || printf 'unknown')"

case "$os" in
  Darwin)  os_triple="apple-darwin" ;;
  Linux)   os_triple="unknown-linux-musl" ;;
  *)
    printf 'eco install: unsupported OS "%s"\n' "$os" >&2
    printf 'Supported: macOS, Linux.\n' >&2
    exit 1
    ;;
esac

case "$arch" in
  x86_64|amd64)   arch_triple="x86_64" ;;
  arm64|aarch64)  arch_triple="aarch64" ;;
  *)
    printf 'eco install: unsupported architecture "%s"\n' "$arch" >&2
    printf 'Supported: x86_64, arm64.\n' >&2
    exit 1
    ;;
esac

# aarch64 binaries are not built yet for every target; x86_64 is the shipped set.
if [ -z "$TARGET" ]; then
  TARGET="${arch_triple}-${os_triple}"
fi

EXE_SUFFIX=""
case "$os_triple" in
  *windows*) EXE_SUFFIX=".exe" ;;
esac

# --- resolve install dir ----------------------------------------------------
if [ -n "${ECO_INSTALL_DIR:-}" ]; then
  INSTALL_DIR="$ECO_INSTALL_DIR"
elif [ -w /usr/local/bin ]; then
  INSTALL_DIR="/usr/local/bin"
else
  INSTALL_DIR="${HOME}/.local/bin"
fi
mkdir -p "$INSTALL_DIR"

DEST="$INSTALL_DIR/eco$EXE_SUFFIX"

# --- download ---------------------------------------------------------------
# GitHub Releases layout: <base>/eco-<triple>[.exe] (stable asset names).
# The legacy estate layout <base>/downloads/<target>/eco is the fallback.
URL="${ECO_INSTALL_URL:-$BASE_URL/eco-$TARGET$EXE_SUFFIX}"

printf 'eco install: fetching %s\n' "$URL"
if command -v curl >/dev/null 2>&1; then
  curl -fsSL "$URL" -o "$DEST"
elif command -v wget >/dev/null 2>&1; then
  wget -qO "$DEST" "$URL"
else
  printf 'eco install: need curl or wget\n' >&2
  exit 1
fi

chmod +x "$DEST" 2>/dev/null || true

# --- ensure the install dir is on PATH --------------------------------------
# If eco landed in a directory not on PATH, add it to the user's shell rc so
# `eco` works immediately after install (no manual PATH edits).
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    RC_FILE=""
    if [ -n "${ZSH_VERSION:-}" ] || [ -f "$HOME/.zshrc" ]; then
      RC_FILE="$HOME/.zshrc"
    elif [ -f "$HOME/.bashrc" ]; then
      RC_FILE="$HOME/.bashrc"
    fi
    if [ -n "$RC_FILE" ] && ! grep -qs "export PATH=\"\$HOME/.local/bin" "$RC_FILE"; then
      printf '\n# added by eco installer\nexport PATH="$HOME/.local/bin:$PATH"\n' >> "$RC_FILE"
      printf 'eco install: added %s to PATH in %s\n' "$INSTALL_DIR" "$RC_FILE"
      printf '  (run `source %s` in this shell, or open a new terminal)\n' "$RC_FILE"
    fi
    ;;
esac

printf '\neco installed to %s\n' "$DEST"
if "$DEST" help >/dev/null 2>&1; then
  "$DEST" help | sed -n '1,3p'
  printf '\nRun `eco help` to see all commands. Version: %s\n' \
    "$("$DEST" --version 2>/dev/null || printf '0.2.0')"
else
  printf 'warning: binary installed but did not run -- check `%s help`\n' "$DEST" >&2
fi
