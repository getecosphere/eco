#!/usr/bin/env bash
# build-release.sh — build the eco binary for every supported platform.
#
# Provisioning rule (docs/working-agreements.md): nothing is installed by
# hand. This script installs the cross-compile toolchain it needs the same
# way eco provisions runtimes — through rustup + a pinned zig download — so
# a fresh machine can reproduce release binaries from this one entrypoint.
#
# Outputs into dist/<target>/eco[.exe], plus a dist/eco-<triple> shim for
# install.sh to download.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUST_DIR="$SCRIPT_DIR/rust"
DIST_DIR="$SCRIPT_DIR/dist"

VERSION="$(grep '^version' "$RUST_DIR/Cargo.toml" | head -n1 | awk -F'"' '{print $2}')"
ZIG_VERSION="0.13.0"

# Targets: host (macOS) stays native; the other macOS arch is cross-built via
# zig; Linux uses static musl so install.sh works on any distro without glibc
# matching; Windows uses GNU.
HOST_TARGET="$(rustc -vV 2>/dev/null | sed -n 's/^host: //p')"
LINUX_TARGET="x86_64-unknown-linux-musl"
WINDOWS_TARGET="x86_64-pc-windows-gnu"
APPLE_OTHER_TARGET="$(if [[ "$HOST_TARGET" == aarch64-apple-darwin ]]; then printf 'x86_64-apple-darwin'; else printf 'aarch64-apple-darwin'; fi)"

log() { printf '\033[1;36m[build-release]\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[build-release]\033[0m %s\n' "$*"; }

ensure_rustup_targets() {
  for t in "$HOST_TARGET" "$LINUX_TARGET" "$WINDOWS_TARGET" "$APPLE_OTHER_TARGET"; do
    if ! rustup target list --installed | grep -qx "$t"; then
      log "installing rustup target $t"
      rustup target add "$t"
    fi
  done
}

ensure_zig() {
  if command -v zig >/dev/null 2>&1 && [[ "$(zig version 2>/dev/null)" == "0.13.0" ]]; then
    log "zig 0.13.0 present"
    return
  fi
  local os_arch zig_triple zig_tarball
  case "$(uname -s)-$(uname -m)" in
    Darwin-x86_64)  zig_triple="macos-x86_64" ;;
    Darwin-arm64)   zig_triple="macos-aarch64" ;;
    Linux-x86_64)   zig_triple="linux-x86_64" ;;
    Linux-aarch64)  zig_triple="linux-aarch64" ;;
    *) warn "unsupported host for zig cross toolchain: $(uname -s)-$(uname -m)"; return ;;
  esac
  zig_tarball="zig-${zig_triple}-${ZIG_VERSION}.tar.xz"
  local install_dir="$SCRIPT_DIR/.zig"
  mkdir -p "$install_dir"
  log "downloading $zig_tarball (pinned $ZIG_VERSION)"
  curl -fsSL "https://ziglang.org/download/$ZIG_VERSION/$zig_tarball" -o "$install_dir/$zig_tarball"
  tar xf "$install_dir/$zig_tarball" -C "$install_dir"
  # expose on PATH for this script only
  export PATH="$install_dir/zig-${zig_triple}-${ZIG_VERSION}:$PATH"
  zig version
}

ensure_cargo_zigbuild() {
  if command -v cargo-zigbuild >/dev/null 2>&1; then
    log "cargo-zigbuild present"
    return
  fi
  log "installing cargo-zigbuild via cargo (pinned)"
  cargo install cargo-zigbuild --locked
}

build_target() {
  local target="$1"
  log "building $target"
  (cd "$RUST_DIR" && cargo zigbuild --release --target "$target")
  local src
  src="$RUST_DIR/target/$target/release/eco"
  [[ "$target" == *windows* ]] && src="$src.exe"
  [[ -f "$src" ]] || { warn "missing artifact: $src"; return; }
  local out="$DIST_DIR/$target"
  mkdir -p "$out"
  cp "$src" "$out/eco"
  [[ "$target" == *windows* ]] && mv "$out/eco" "$out/eco.exe"
  # GitHub Releases asset with a stable name: eco-<triple>[.exe]
  local asset_name="eco-${target}"
  [[ "$target" == *windows* ]] && asset_name="${asset_name}.exe"
  cp "$out/eco" "$DIST_DIR/$asset_name" 2>/dev/null || cp "$src" "$DIST_DIR/$asset_name"
  log "dist/$target/eco ready (+ $asset_name)"
}

build_host() {
  log "building host target $HOST_TARGET"
  (cd "$RUST_DIR" && cargo build --release)
  local out="$DIST_DIR/$HOST_TARGET"
  mkdir -p "$out"
  cp "$RUST_DIR/target/release/eco" "$out/eco"
  cp "$RUST_DIR/target/release/eco" "$DIST_DIR/eco-${HOST_TARGET}"
  log "dist/$HOST_TARGET/eco ready (+ eco-${HOST_TARGET})"
}

main() {
  ensure_rustup_targets
  ensure_zig
  ensure_cargo_zigbuild
  build_host
  build_target "$LINUX_TARGET"
  build_target "$WINDOWS_TARGET"
  build_target "$APPLE_OTHER_TARGET"
  log "release $VERSION built:"
  (cd "$DIST_DIR" && du -sh */eco* 2>/dev/null || true)
}

main "$@"
