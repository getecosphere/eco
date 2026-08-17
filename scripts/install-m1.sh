#!/bin/bash
# eco-builder setup for the M1 dev machine (run ON the M1, not over SSH).
# Installs: rustup (stable + x86_64 cross targets), zig 0.13, cargo-zigbuild,
# pnpm, and PATH setup. Idempotent; safe to re-run.
set -uo pipefail

log() { printf '\033[1;36m[eco-builder]\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[eco-builder]\033[0m %s\n' "$*"; }

# Homebrew is already installed user-home at ~/homebrew (no sudo).
export PATH="$HOME/homebrew/bin:$PATH"

# 1. Rust — clean install (previous SSH attempts corrupted ~/.rustup)
log "Installing Rust (stable) + x86_64 cross targets..."
pkill -f rustup 2>/dev/null || true
rm -rf "$HOME/.rustup" "$HOME/.cargo"
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
# shellcheck disable=SC1090
. "$HOME/.cargo/env"
log "Adding x86_64 targets (prod CTs are linux/amd64)..."
rustup target add x86_64-unknown-linux-gnu x86_64-unknown-linux-musl
rustc --version && echo "  ✓ rust $(rustc --version)"

# 2. zig 0.13 (pinned, matches eco's cross-compile toolchain)
if ! command -v zig >/dev/null 2>&1 || ! zig version 2>/dev/null | grep -q '^0.13.0'; then
  log "Installing zig 0.13.0..."
  ZIG=zig-0.13.0-aarch64-macos.tar.xz
  curl -fsSL "https://ziglang.org/download/0.13.0/${ZIG}" -o /tmp/zig.tar.xz
  mkdir -p "$HOME/zig"
  tar xJf /tmp/zig.tar.xz -C "$HOME/zig" --strip-components=1
  rm -f /tmp/zig.tar.xz
  export PATH="$HOME/zig:$PATH"
  echo 'export PATH="$HOME/zig:$PATH"' >> "$HOME/.zshrc"
  echo 'export PATH="$HOME/zig:$PATH"' >> "$HOME/.zshenv" 2>/dev/null || true
fi
zig version && echo "  ✓ zig $(zig version)"

# 3. cargo-zigbuild (musl cross-linker driver used by eco up --remote)
if ! command -v cargo-zigbuild >/dev/null 2>&1; then
  log "Installing cargo-zigbuild..."
  cargo install cargo-zigbuild --locked
fi
cargo-zigbuild --version && echo "  ✓ cargo-zigbuild"

# 4. pnpm
if ! command -v pnpm >/dev/null 2>&1; then
  log "Installing pnpm..."
  npm install -g pnpm
fi
pnpm -v && echo "  ✓ pnpm $(pnpm -v)"

# 5. PATH setup for cargo + fix the stale .zshenv reference
export PATH="$HOME/.cargo/bin:$PATH"
grep -q '.cargo/env' "$HOME/.zshenv" 2>/dev/null || echo '. "$HOME/.cargo/env"' >> "$HOME/.zshenv"
grep -q '.cargo/bin' "$HOME/.zshrc" 2>/dev/null || echo 'export PATH="$HOME/.cargo/bin:$PATH"' >> "$HOME/.zshrc"

log "=== eco-builder M1 toolchain ready ==="
echo "  arch:    $(uname -m)"
echo "  node:    $(node -v)"
echo "  pnpm:    $(pnpm -v)"
echo "  rust:    $(rustc --version)"
echo "  zig:     $(zig version)"
echo "  zigbuild:$(cargo-zigbuild --version)"
echo ""
echo "Next steps (I'll do these over SSH once you're done):"
echo "  - clone the superapp workspace, build the eco binary"
echo "  - wire SSH keys + ECO_* env"
echo "  - provision the amd64 frontend-build env (Lima+Rosetta) for linux-x64 Node modules"
