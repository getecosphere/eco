#!/bin/bash
# eco-builder-bootstrap.sh — pinned toolchain for the eco-builder Lima VM.
#
# Run inside the VM (as the default `eco` user):
#   limactl shell eco-builder -- bash /tmp/eco-builder-bootstrap.sh
#
# System packages use sudo; the user toolchain (rust, bun) installs under
# $HOME and is symlinked into /usr/local/bin so non-login `bash -c` shells
# (how eco's builder_exec invokes commands) can find it. Idempotent. Pins
# every version eco's remote builds rely on so two deploys from the same
# commit produce byte-identical artifacts (the reproducibility half of the
# SOC2/ISO27001 story).
set -euo pipefail

# $1 = comma-separated toolchain needs for the current build (rust,node).
# Defaults to the full set so manual runs keep working. The VM is persistent,
# so each run only installs what the current project's build actually needs.
NEEDS="${1:-rust,node}"
need() { [[ ",$NEEDS," == *",$1,"* ]]; }

log() { printf '\033[1;36m[eco-builder]\033[0m %s\n' "$*"; }

ZIG_VERSION=0.13.0
NODE_MAJOR=22

export DEBIAN_FRONTEND=noninteractive
SUDO=""
if [[ "$(id -u)" -ne 0 ]]; then
  SUDO="sudo"
fi

# ── System packages ────────────────────────────────────────────────────────
$SUDO apt-get update -y
$SUDO apt-get install -y build-essential pkg-config curl ca-certificates \
  git xz-utils unzip python3 python3-venv make libssl-dev sqlite3

# ── Rust (stable + x86_64 musl cross target) — only when rust is needed ────
if need rust; then
  if ! command -v rustc >/dev/null 2>&1; then
    log "Installing rustup (stable) for $(whoami)"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
  fi
  export PATH="$HOME/.cargo/bin:$PATH"
  rustup target add x86_64-unknown-linux-musl
  log "rust: $(rustc --version)"

  # cargo-zigbuild (musl cross-linker driver)
  if ! command -v cargo-zigbuild >/dev/null 2>&1; then
    log "Installing cargo-zigbuild (pinned)"
    cargo install cargo-zigbuild --locked
  fi
  log "cargo-zigbuild: $(cargo-zigbuild --version)"

  # zig (pinned, matches eco's cross toolchain)
  if ! command -v zig >/dev/null 2>&1 || ! zig version 2>/dev/null | grep -q "^$ZIG_VERSION"; then
    log "Installing zig $ZIG_VERSION (pinned)"
    ZIG_TARBALL="zig-linux-aarch64-${ZIG_VERSION}.tar.xz"
    curl -fsSL "https://ziglang.org/download/$ZIG_VERSION/$ZIG_TARBALL" -o /tmp/zig.tar.xz
    $SUDO mkdir -p /opt/zig
    $SUDO tar xJf /tmp/zig.tar.xz -C /opt/zig --strip-components=1
    rm -f /tmp/zig.tar.xz
  fi
  $SUDO ln -sf /opt/zig/zig /usr/local/bin/zig
  log "zig: $(zig version)"

  # Symlink the rust toolchain into /usr/local/bin (non-login shells)
  for bin in cargo rustc rustup cargo-zigbuild; do
    if [[ -x "$HOME/.cargo/bin/$bin" ]]; then
      $SUDO ln -sf "$HOME/.cargo/bin/$bin" "/usr/local/bin/$bin"
    fi
  done
fi

# ── Node (LTS via NodeSource) — only when node is needed ───────────────────
if need node; then
  if ! command -v node >/dev/null 2>&1; then
    log "Installing Node $NODE_MAJOR LTS"
    curl -fsSL "https://deb.nodesource.com/setup_${NODE_MAJOR}.x" | $SUDO bash -
    $SUDO apt-get install -y nodejs
  fi
  log "node: $(node -v), npm: $(npm -v)"

  # pnpm (via corepack, enables pnpm-lock.yaml frontends)
  if ! command -v corepack >/dev/null 2>&1; then
    $SUDO npm install -g corepack
  fi
  corepack enable 2>/dev/null || true
  corepack prepare pnpm@latest --activate 2>/dev/null || true

  # bun (Bun-compile SSR node apps into single linux binaries)
  if ! command -v bun >/dev/null 2>&1; then
    log "Installing bun (latest)"
    curl -fsSL https://bun.sh/install | bash
  fi
  $SUDO ln -sf "$HOME/.bun/bin/bun" /usr/local/bin/bun
  log "bun: $(bun --version)"
fi

log "=== eco-builder toolchain ready (needs: $NEEDS) ==="
echo "  arch:   $(uname -m)"
if need rust; then
  echo "  rust:   $(rustc --version)"
  echo "  zig:    $(zig version)"
  echo "  zigbuild: $(cargo-zigbuild --version)"
fi
if need node; then
  echo "  node:   $(node -v)"
  echo "  pnpm:   $(pnpm -v 2>/dev/null || echo n/a)"
  echo "  bun:    $(bun --version)"
fi
