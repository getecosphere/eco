#!/usr/bin/env bash
# provision.sh — install runtime dependencies declared in a project ecompose.yml

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="${ECOLOGY_WORKSPACE_ROOT:-$(cd "$SCRIPT_DIR/.." && pwd)}"

BOLD='\033[1m'
CYAN='\033[0;36m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
RESET='\033[0m'

declare -a SERVICES=()
declare -a TOKENS=()

ECOMPOSE_INPUT=""
ECOMPOSE_FILE=""
APT_UPDATED=0
PLAN_ONLY=0
MINIO_DECLARED=0

if [[ "${EUID}" -eq 0 ]]; then
  SUDO=""
else
  SUDO="sudo"
fi

log() {
  echo -e "${CYAN}$*${RESET}"
}

ok() {
  echo -e "${GREEN}$*${RESET}"
}

warn() {
  echo -e "${YELLOW}$*${RESET}"
}

fail() {
  echo -e "${RED}$*${RESET}" >&2
  exit 1
}

# Same detection configure.sh uses for its dev/prod PM2-env split -- kept
# in sync so provision.sh installs runtimes into the same place configure.sh
# later points PM2 at. An estate's app CT is always run inside `pct exec`,
# which is itself an LXC container, so this needs no explicit signal from
# up.js: it just detects the container it's actually running in.
detect_deploy_mode() {
  if [[ -n "${ECO_DEPLOY_MODE:-}" ]]; then
    printf '%s' "$ECO_DEPLOY_MODE"
    return
  fi

  if command -v systemd-detect-virt >/dev/null 2>&1; then
    local virt
    virt="$(systemd-detect-virt --container 2>/dev/null || true)"
    case "$virt" in
      lxc|lxc-libvirt)
        printf 'prod'
        return
        ;;
    esac
  fi

  if [[ -f /run/systemd/container ]]; then
    local container_type
    container_type="$(cat /run/systemd/container 2>/dev/null || true)"
    case "$container_type" in
      lxc|lxc-libvirt)
        printf 'prod'
        return
        ;;
    esac
  fi

  printf 'dev'
}

DEPLOY_MODE="$(detect_deploy_mode)"

is_prod_mode() {
  [[ "$DEPLOY_MODE" == "prod" ]]
}

# Runtime tokens to skip provisioning for in local dev. `eco up dev` sets
# this when a `dev: optional` domain's required runtime can't run on the
# machine (e.g. onnxruntime); the domain is still deployed in prod, where
# this env var is never set. Accepts a comma- and/or whitespace-separated
# list.
token_is_skipped() {
  local token="$1"
  [[ -n "$token" ]] || return 1
  local raw
  raw="$(printf '%s' "${ECO_DEV_SKIP_RUNTIMES:-}" | tr ',' ' ')"
  local skip
  for skip in $raw; do
    [[ "$skip" == "$token" ]] && return 0
  done
  return 1
}

usage() {
  cat <<'EOF'
Usage:
  bash eco/provision.sh
  bash eco/provision.sh /path/to/project
  bash eco/provision.sh /path/to/ecompose.yml
  bash eco/provision.sh --plan
  bash eco/provision.sh /path/to/project --plan

Behavior:
  - Resolves a project-level ecompose.yml
  - Reads shared tools and per-service runtime tokens
  - Detects macOS vs Linux
  - Installs required binaries/packages for the current OS
  - `--plan` prints the resolved install plan without executing installs

  A `storage.minio` block provisions managed MinIO automatically in dev.
  In production, `eco up` provisions the named dedicated MinIO CT and gives
  this app CT a private bridge-network S3 endpoint.

Supported runtime tokens:
  java@17
  maven
  node@20
  npm
  pm2
  postgresql@15  (accepts PostgreSQL 15+ on dev machines)
  mongodb@7
  redis@7  (accepts Redis 7+)
  golang
  rust
  git
  openssh-client
  curl
  jq
  ca-certificates
  onnxruntime  (installs libonnxruntime.so for rag/embedding services)
  ffmpeg  (video transcoding dependency via brew/apt)
EOF
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1
}

find_macos_postgres_psql() {
  local candidate
  for candidate in \
    /Applications/Postgres.app/Contents/Versions/15/bin/psql \
    /Applications/Postgres.app/Contents/Versions/latest/bin/psql
  do
    if [[ -x "$candidate" ]]; then
      printf '%s' "$candidate"
      return 0
    fi
  done

  return 1
}

macos_major_version() {
  sw_vers -productVersion 2>/dev/null | cut -d. -f1
}

redis_server_binary() {
  if need_cmd redis-server; then
    command -v redis-server
    return 0
  fi
  return 1
}

major_from_version() {
  printf '%s' "$1" | sed -E 's/^[^0-9]*([0-9]+).*/\1/'
}

is_token_satisfied() {
  local token="$1"
  local version major

  case "$token" in
    git)
      need_cmd git
      ;;
    openssh-client)
      need_cmd ssh
      ;;
    curl)
      need_cmd curl
      ;;
    jq)
      need_cmd jq
      ;;
    ca-certificates)
      case "$(uname -s)" in
        Darwin) return 0 ;;
        Linux) [[ -f /etc/ssl/certs/ca-certificates.crt || -d /etc/ssl/certs ]] ;;
        *) return 1 ;;
      esac
      ;;
    java@17)
      if need_cmd java; then
        version="$(java -version 2>&1 | head -n1)"
        major="$(major_from_version "$version")"
        [[ "$major" == "17" ]]
      else
        return 1
      fi
      ;;
    maven)
      need_cmd mvn
      ;;
    node@20)
      if need_cmd node; then
        version="$(node -v 2>/dev/null)"
        major="$(major_from_version "$version")"
        [[ "$major" == "20" ]]
      else
        return 1
      fi
      ;;
    npm)
      need_cmd npm
      ;;
    pm2)
      need_cmd pm2
      ;;
    postgresql@15)
      if need_cmd psql; then
        version="$(psql --version 2>/dev/null)"
        major="$(printf '%s' "$version" | sed -E 's/.* ([0-9]+)(\.[0-9]+)?.*/\1/')"
        [[ "$major" =~ ^[0-9]+$ ]] && [[ "$major" -ge 15 ]]
      elif [[ "$(uname -s)" == "Darwin" ]]; then
        local macos_psql=""
        macos_psql="$(find_macos_postgres_psql || true)"
        if [[ -n "$macos_psql" ]]; then
          version="$("$macos_psql" --version 2>/dev/null)"
          major="$(printf '%s' "$version" | sed -E 's/.* ([0-9]+)(\.[0-9]+)?.*/\1/')"
          [[ "$major" =~ ^[0-9]+$ ]] && [[ "$major" -ge 15 ]]
        else
          return 1
        fi
      else
        return 1
      fi
      ;;
    mongodb@7)
      if need_cmd mongod; then
        version="$(mongod --version 2>/dev/null | head -n1)"
        major="$(major_from_version "$version")"
        [[ "$major" == "7" ]]
      else
        return 1
      fi
      ;;
    redis@7)
      local redis_server=""
      redis_server="$(redis_server_binary || true)"
      if [[ -z "$redis_server" ]]; then
        return 1
      fi
      version="$("$redis_server" --version 2>/dev/null)"
      major="$(printf '%s' "$version" | sed -nE 's/.*v=([0-9]+)\..*/\1/p')"
      [[ "$major" =~ ^[0-9]+$ ]] && [[ "$major" -ge 7 ]]
      ;;
    onnxruntime|onnxruntime@1.28)
      if [[ "$(uname -s)" == "Darwin" ]]; then
        need_cmd brew && brew list onnxruntime >/dev/null 2>&1
      else
        [[ -f /opt/eco-tools/libonnxruntime.so ]]
      fi
      ;;
    ffmpeg)
      need_cmd ffmpeg
      ;;
    golang|golang@*|go)
      need_cmd go
      ;;
    static)
      # Leptos/static frontends are served by python3 http.server — built-in
      # on macOS and Debian; nothing extra to provision.
      need_cmd python3
      ;;
    rust)
      # Rust is only provisioned on the developer machine (build farm); the
      # production CT never compiles and does not install a Rust toolchain.
      # `cargo` is often rustup's shim -- its presence alone does not mean a
      # compiler is usable, so probe an actual `cargo --version`.
      need_cmd cargo && cargo --version >/dev/null 2>&1
      ;;
    *)
      return 1
      ;;
  esac
}

register_token() {
  local token="$1"
  local existing
  [[ -z "$token" ]] && return
  for existing in "${TOKENS[@]:-}"; do
    [[ "$existing" == "$token" ]] && return
  done
  TOKENS+=("$token")
}

resolve_ecompose_file() {
  if [[ -n "$ECOMPOSE_INPUT" ]]; then
    if [[ -d "$ECOMPOSE_INPUT" && -f "$ECOMPOSE_INPUT/ecompose.yml" ]]; then
      ECOMPOSE_FILE="$(cd "$ECOMPOSE_INPUT" && pwd)/ecompose.yml"
      return
    fi
    if [[ -f "$ECOMPOSE_INPUT" ]]; then
      ECOMPOSE_FILE="$(cd "$(dirname "$ECOMPOSE_INPUT")" && pwd)/$(basename "$ECOMPOSE_INPUT")"
      return
    fi
    fail "Cannot resolve ecompose input: $ECOMPOSE_INPUT"
  fi

  if [[ -f "$PWD/ecompose.yml" ]]; then
    ECOMPOSE_FILE="$PWD/ecompose.yml"
    return
  fi

  local -a found=()
  while IFS= read -r path; do
    found+=("$path")
  done < <(find "$WORKSPACE_ROOT" -maxdepth 2 -name ecompose.yml -type f | sort)

  if [[ ${#found[@]} -eq 1 ]]; then
    ECOMPOSE_FILE="${found[0]}"
    return
  fi

  if [[ ${#found[@]} -eq 0 ]]; then
    fail "No ecompose.yml found. Create one in the project directory first."
  fi

  echo -e "${RED}Multiple ecompose.yml files found:${RESET}" >&2
  printf '  %s\n' "${found[@]}" >&2
  fail "Run provision.sh with an explicit project directory or ecompose.yml path."
}

parse_ecompose() {
  awk '
    function trim(s) {
      sub(/^[[:space:]]+/, "", s)
      sub(/[[:space:]]+$/, "", s)
      sub(/[[:space:]]+#.*$/, "", s)
      gsub(/^["'"'"']|["'"'"']$/, "", s)
      return s
    }

    /^[[:space:]]*#/ || /^[[:space:]]*$/ { next }

    /^shared_tools:[[:space:]]*$/ {
      section = "shared_tools"
      service = ""
      in_runtimes = 0
      next
    }

    /^services:[[:space:]]*$/ {
      section = "services"
      service = ""
      in_runtimes = 0
      next
    }

    /^domains:[[:space:]]*$/ {
      section = "domains"
      service = ""
      in_runtimes = 0
      next
    }

    section == "shared_tools" && /^  - / {
      value = $0
      sub(/^  - /, "", value)
      print "tool\tshared\t" trim(value)
      next
    }

    section == "services" && /^  [A-Za-z0-9_-]+:[[:space:]]*$/ {
      service = $0
      sub(/^  /, "", service)
      sub(/:[[:space:]]*$/, "", service)
      print "service\t" service "\t"
      in_runtimes = 0
      next
    }

    section == "services" && /^    runtimes:[[:space:]]*$/ {
      in_runtimes = 1
      next
    }

    # v2 ecompose: inline runtimes, e.g. `runtimes: [rust, postgresql@15]`
    section == "services" && /^    runtimes:[[:space:]]*\[/ {
      inline = $0
      sub(/^[[:space:]]*runtimes:[[:space:]]*/, "", inline)
      sub(/[[:space:]]*#.*$/, "", inline)
      sub(/^\[/, "", inline)
      sub(/\]$/, "", inline)
      split(inline, parts, /,[[:space:]]*/)
      for (i in parts) {
        print "runtime\t" service "\t" trim(parts[i])
      }
      next
    }

    section == "services" && /^    [A-Za-z0-9_-]+:/ {
      in_runtimes = 0
      next
    }

    section == "services" && in_runtimes && /^      - / {
      value = $0
      sub(/^      - /, "", value)
      print "runtime\t" service "\t" trim(value)
      next
    }
  ' "$ECOMPOSE_FILE"
}

load_tokens_from_ecompose() {
  local line_type service value
  while IFS=$'\t' read -r line_type service value; do
    case "$line_type" in
      service)
        SERVICES+=("$service")
        ;;
      tool|runtime)
        register_token "$value"
        ;;
    esac
  done < <(parse_ecompose)

  if [[ -z "${TOKENS[0]:-}" ]]; then
    fail "No runtime tokens found in $ECOMPOSE_FILE"
  fi
}

manifest_declares_minio() {
  awk '
    /^[[:space:]]*#/ { next }
    /^storage:[[:space:]]*$/ { in_storage = 1; next }
    in_storage && /^[^[:space:]].*:[[:space:]]*$/ { exit }
    in_storage && /^  minio:[[:space:]]*$/ { found = 1; exit }
    END { exit !found }
  ' "$ECOMPOSE_FILE"
}

os_id() {
  if [[ "$(uname -s)" == "Darwin" ]]; then
    echo "macos"
    return
  fi

  if [[ -r /etc/os-release ]]; then
    . /etc/os-release
    echo "${ID:-linux}"
    return
  fi

  echo "unknown"
}

os_version_id() {
  if [[ -r /etc/os-release ]]; then
    . /etc/os-release
    echo "${VERSION_ID:-}"
    return
  fi

  echo ""
}

linux_family() {
  case "$(os_id)" in
    ubuntu|debian)
      echo "debian"
      ;;
    *)
      echo "unknown"
      ;;
  esac
}

apt_update_once() {
  if [[ "$APT_UPDATED" -eq 0 ]]; then
    log "Updating apt package index..."
    $SUDO apt-get update
    APT_UPDATED=1
  fi
}

apt_install() {
  apt_update_once
  DEBIAN_FRONTEND=noninteractive $SUDO apt-get install -y "$@"
}

ensure_apt_repo_prereqs() {
  apt_install ca-certificates curl gnupg lsb-release
}

ensure_nodesource_repo() {
  if [[ -f /etc/apt/sources.list.d/nodesource.list ]]; then
    return
  fi

  ensure_apt_repo_prereqs
  log "Adding NodeSource repository for Node.js 20..."
  $SUDO mkdir -p /etc/apt/keyrings
  curl -fsSL https://deb.nodesource.com/gpgkey/nodesource-repo.gpg.key \
    | $SUDO gpg --dearmor -o /etc/apt/keyrings/nodesource.gpg
  echo "deb [signed-by=/etc/apt/keyrings/nodesource.gpg] https://deb.nodesource.com/node_20.x nodistro main" \
    | $SUDO tee /etc/apt/sources.list.d/nodesource.list >/dev/null
  APT_UPDATED=0
}

ensure_mongodb_repo_debian() {
  if [[ -f /etc/apt/sources.list.d/mongodb-org-7.0.list ]]; then
    return
  fi

  ensure_apt_repo_prereqs
  local id version codename
  id="$(os_id)"
  version="$(os_version_id)"
  codename="$(
    . /etc/os-release
    echo "${VERSION_CODENAME:-}"
  )"

  $SUDO mkdir -p /usr/share/keyrings

  case "$id" in
    ubuntu)
      [[ -n "$codename" ]] || fail "Cannot determine Ubuntu codename for MongoDB repository setup."
      log "Adding MongoDB 7.0 repository for Ubuntu ${codename}..."
      curl -fsSL https://pgp.mongodb.com/server-7.0.asc \
        | $SUDO gpg --dearmor -o /usr/share/keyrings/mongodb-server-7.0.gpg
      echo "deb [ arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/mongodb-server-7.0.gpg ] https://repo.mongodb.org/apt/ubuntu ${codename}/mongodb-org/7.0 multiverse" \
        | $SUDO tee /etc/apt/sources.list.d/mongodb-org-7.0.list >/dev/null
      ;;
    debian)
      case "$version" in
        11)
          log "Adding MongoDB 7.0 repository for Debian 11..."
          curl -fsSL https://pgp.mongodb.com/server-7.0.asc \
            | $SUDO gpg --dearmor -o /usr/share/keyrings/mongodb-server-7.0.gpg
          echo "deb [ arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/mongodb-server-7.0.gpg ] https://repo.mongodb.org/apt/debian bullseye/mongodb-org/7.0 main" \
            | $SUDO tee /etc/apt/sources.list.d/mongodb-org-7.0.list >/dev/null
          ;;
        12)
          log "Adding MongoDB 7.0 repository for Debian 12..."
          curl -fsSL https://pgp.mongodb.com/server-7.0.asc \
            | $SUDO gpg --dearmor -o /usr/share/keyrings/mongodb-server-7.0.gpg
          echo "deb [ arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/mongodb-server-7.0.gpg ] https://repo.mongodb.org/apt/debian bookworm/mongodb-org/7.0 main" \
            | $SUDO tee /etc/apt/sources.list.d/mongodb-org-7.0.list >/dev/null
          ;;
        *)
          fail "MongoDB repository bootstrap is only defined for Debian 11/12 and supported Ubuntu releases."
          ;;
      esac
      ;;
    *)
      fail "MongoDB repository bootstrap is unsupported for OS: $id"
      ;;
  esac

  APT_UPDATED=0
}

ensure_brew() {
  if need_cmd brew; then
    return
  fi
  # Homebrew is the standard runtime provider on macOS. Auto-install it so
  # `eco up dev` provisions the missing runtimes instead of failing with a
  # "install Homebrew first" message. Non-interactive: the official installer
  # runs without prompts when run as the current user on Apple Silicon/Intel.
  log "Homebrew not found — installing it (one-time)…"
  if [[ "$(uname -m)" == "arm64" ]]; then
    NONINTERACTIVE=1 /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
  else
    NONINTERACTIVE=1 /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
  fi
  # Homebrew on Apple Silicon lives under /opt/homebrew; make brew resolve for
  # this shell and future non-interactive SSH sessions via the default PATH.
  if [[ -x /opt/homebrew/bin/brew ]]; then
    export PATH="/opt/homebrew/bin:$PATH"
    # Persist for the user's shells so later `eco up` calls find brew too.
    if [[ -f "$HOME/.zshrc" ]]; then
      grep -q '/opt/homebrew/bin' "$HOME/.zshrc" || echo 'export PATH="/opt/homebrew/bin:$PATH"' >> "$HOME/.zshrc"
    fi
    if [[ -f "$HOME/.bashrc" ]]; then
      grep -q '/opt/homebrew/bin' "$HOME/.bashrc" || echo 'export PATH="/opt/homebrew/bin:$PATH"' >> "$HOME/.bashrc"
    fi
  elif [[ -x /usr/local/bin/brew ]]; then
    export PATH="/usr/local/bin:$PATH"
  fi
  need_cmd brew || fail "Homebrew install failed. Install it manually: https://brew.sh/"
}

# Installs the Rust toolchain on the developer machine (the build farm) as a
# normal per-user install (~/.cargo, ~/.rustup) so cargo/rustc resolve from
# the invoking user's PATH the way brew-installed runtimes do.
#
# Rust is only provisioned on the developer machine (the build farm): the
# production CT never compiles and does not install a Rust toolchain, so this
# always installs the normal per-user way (~/.cargo, ~/.rustup, rustup's own
# default). configure.sh's dev-mode PM2 env deliberately leaves
# RUSTUP_HOME/CARGO_HOME unset so PM2 inherits whatever the user's own shell
# resolves.
install_rust_system_wide() {
  if need_cmd cargo && cargo --version >/dev/null 2>&1; then
    ok "Rust already installed: $(cargo --version)"
  else
    log "Installing Rust (rustup, per-user under \$HOME)..."
    curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env"
  fi

  # A partial/interrupted rustup installation can leave cargo/rustc shims on
  # PATH but no selected compiler, causing every PM2 Rust service to crash
  # with "rustup could not choose a version". Explicitly establish stable
  # after every provisioning run; rustup makes this a cheap no-op once ready.
  if need_cmd rustup; then
    log "Ensuring Rust stable toolchain is selected..."
    rustup toolchain install stable --profile minimal
    rustup default stable
  fi

  cargo --version >/dev/null 2>&1 || fail "Rust cargo is installed but no usable default toolchain is configured."

  # rustup installs rustc/cargo but never a C toolchain -- rustc's own
  # linker step shells out to the system `cc`, which almost every real
  # crate needs indirectly even without any FFI (proc-macro crates like
  # proc-macro2/quote, which nearly everything using #[derive(...)]
  # depends on, have a native build script). A minimal Debian CT doesn't
  # have one by default. Without this, every `cargo build`/`cargo run`
  # fails at the link step with "linker `cc` not found" -- and since
  # rust-runtime PM2 services invoke `cargo run` directly as their start
  # command, that failure crash-loops the service forever instead of
  # just failing once at provision time.
  #
  # This check runs unconditionally (not just in the fresh-install
  # branch above), since a machine can already have cargo installed from an
  # earlier provision run while still being missing the toolchain --
  # exactly the state that produced the crash loop this fixes.
  ensure_c_toolchain

  # Every rust-runtime domain is its own independent Cargo project with
  # its own target/ dir, so a from-scratch machine recompiles shared
  # dependencies (aws-lc-rs, serde_json, tower-service, ...) once per
  # domain instead of once total. sccache caches compiled objects by
  # (source, flags) across all of them, so only the first domain to
  # touch a given dependency version pays the real compile cost.
  ensure_sccache
}

ensure_sccache() {
  if need_cmd sccache; then
    ok "sccache already installed: $(sccache --version)"
  else
    case "$(uname -s)" in
      Darwin)
        # Prefer the prebuilt release binary -- same approach as the Linux
        # branch -- so Homebrew never falls back to source-building llvm +
        # rust for sccache (which happens on macOS releases with no sccache
        # bottle, e.g. Ventura, and can take hours). brew remains the
        # fallback only when the release asset is unavailable.
        local target=""
        case "$(uname -m)" in
          x86_64) target="x86_64-apple-darwin" ;;
          arm64) target="aarch64-apple-darwin" ;;
        esac
        if [[ -n "$target" ]]; then
          local version="v0.17.0"
          local tarball="sccache-${version}-${target}.tar.gz"
          local tmpdir
          tmpdir="$(mktemp -d)"
          if curl --proto "=https" --tlsv1.2 -fsSL \
            "https://github.com/mozilla/sccache/releases/download/${version}/${tarball}" \
            -o "$tmpdir/$tarball"; then
            tar xzf "$tmpdir/$tarball" -C "$tmpdir"
            mkdir -p "$HOME/.local/bin"
            install -m 0755 "$tmpdir/sccache-${version}-${target}/sccache" "$HOME/.local/bin/sccache"
            rm -rf "$tmpdir"
          else
            rm -rf "$tmpdir"
            if need_cmd brew; then
              warn "Prebuilt sccache binary unavailable for ${target}; falling back to brew (may build llvm/rust from source)."
              brew install sccache
            else
              warn "sccache not installed and Homebrew unavailable -- skipping (optional build-cache speedup, not required)."
              return
            fi
          fi
        elif need_cmd brew; then
          log "Installing sccache (brew)..."
          brew install sccache
        else
          warn "sccache not installed and Homebrew unavailable -- skipping (optional build-cache speedup, not required)."
          return
        fi
        ;;
      Linux)
        if [[ "$(uname -m)" != "x86_64" ]]; then
          warn "sccache prebuilt binary only wired up for x86_64 (found $(uname -m)) -- skipping (optional build-cache speedup, not required)."
          return
        fi
        local version="v0.16.0"
        local tarball="sccache-${version}-x86_64-unknown-linux-musl.tar.gz"
        local tmpdir
        tmpdir="$(mktemp -d)"
        log "Installing sccache ${version} (prebuilt binary, caches compiled Rust objects across services)..."
        curl --proto "=https" --tlsv1.2 -sSfL \
          "https://github.com/mozilla/sccache/releases/download/${version}/${tarball}" \
          -o "$tmpdir/$tarball"
        tar xzf "$tmpdir/$tarball" -C "$tmpdir"
        $SUDO install -m 0755 "$tmpdir/sccache-${version}-x86_64-unknown-linux-musl/sccache" /usr/local/bin/sccache
        rm -rf "$tmpdir"
        ;;
      *)
        warn "sccache not installed -- unsupported OS $(uname -s) (optional build-cache speedup, not required)."
        return
        ;;
    esac
  fi

  # The shared /usr/local/sccache-cache dir + /etc/profile.d/cargo.sh
  # RUSTC_WRAPPER export below are a Linux-CT-only convention (every user
  # on the box shares one cache dir, matching install_rust_system_wide's
  # system-wide rustup install) -- /etc/profile.d isn't sourced by macOS's
  # default shell setup, so on Darwin this would just be a no-op $SUDO
  # prompt on every provision run for a file nothing ever reads. A dev Mac
  # gets the sccache binary installed (useful if the user wants to opt in
  # via their own shell profile) but skips the CT-only wiring; configure.sh
  # only sets RUSTC_WRAPPER/SCCACHE_DIR in prod-mode PM2 env anyway.
  if [[ "$(uname -s)" != "Linux" ]]; then
    ok "sccache ready: $(sccache --version 2>/dev/null || echo installed)"
    return
  fi

  $SUDO mkdir -p /usr/local/sccache-cache
  $SUDO chmod a+rwX /usr/local/sccache-cache

  if ! grep -q "RUSTC_WRAPPER" /etc/profile.d/cargo.sh 2>/dev/null; then
    echo 'export RUSTC_WRAPPER=/usr/local/bin/sccache
export SCCACHE_DIR=/usr/local/sccache-cache' | $SUDO tee -a /etc/profile.d/cargo.sh >/dev/null
  fi

  ok "sccache ready, cache dir: /usr/local/sccache-cache"
}

ensure_c_toolchain() {
  if need_cmd cc || need_cmd gcc || need_cmd clang; then
    ok "C toolchain already installed: $(command -v cc || command -v gcc || command -v clang)"
    return
  fi

  case "$(uname -s)" in
    Darwin)
      fail "No C toolchain found (cc/gcc/clang). Run 'xcode-select --install' to install Xcode Command Line Tools, then re-run this script."
      ;;
    Linux)
      log "No C toolchain found -- installing build-essential (required by Rust's linker)..."
      apt_install build-essential
      ;;
    *)
      fail "No C toolchain found (cc/gcc/clang), and don't know how to install one on $(uname -s)."
      ;;
  esac
}

brew_install_formula() {
  local formula="$1"
  log "Installing ${formula}..."
  brew install "$formula"
}

install_token_debian() {
  local token="$1"
  case "$token" in
    git) apt_install git ;;
    openssh-client) apt_install openssh-client ;;
    curl) apt_install curl ;;
    jq) apt_install jq ;;
    ca-certificates) apt_install ca-certificates ;;
    java@17) apt_install openjdk-17-jdk ;;
    maven) apt_install maven ;;
    node@20)
      ensure_nodesource_repo
      apt_install nodejs
      ;;
    npm)
      ensure_nodesource_repo
      apt_install nodejs
      ;;
    pm2)
      ensure_nodesource_repo
      apt_install nodejs
      if need_cmd pm2; then
        ok "PM2 already installed."
      else
        log "Installing PM2 globally via npm..."
        $SUDO npm install -g pm2
      fi
      ;;
    postgresql@15)
      apt_install postgresql-15 postgresql-client-15
      ;;
    mongodb@7)
      ensure_mongodb_repo_debian
      apt_install mongodb-org
      ;;
    redis@7)
      apt_install redis-server
      # The Debian redis-server unit is sandboxed with systemd mount- and
      # user-namespace directives that cannot be applied inside an
      # unprivileged LXC -- systemd aborts with status=226/NAMESPACE and the
      # old `enable --now ... || true` silently left Redis down, which broke
      # every chat message send (502) on the estate. Neutralize the sandbox
      # so the service can start inside a container.
      redis_override=/etc/systemd/system/redis-server.service.d/override.conf
      $SUDO mkdir -p "$(dirname "$redis_override")"
      if [[ ! -f "$redis_override" ]]; then
        log "Writing redis-server systemd override for unprivileged LXC compatibility"
        $SUDO tee "$redis_override" >/dev/null <<'OVERRIDE'
[Service]
PrivateTmp=no
PrivateDevices=no
ProtectHome=no
ProtectSystem=no
ReadWritePaths=
ReadWriteDirectories=
PrivateUsers=no
ProtectProc=default
RestrictNamespaces=no
MemoryDenyWriteExecute=no
NoNewPrivileges=no
ProtectClock=no
ProtectControlGroups=no
ProtectHostname=no
ProtectKernelLogs=no
ProtectKernelModules=no
ProtectKernelTunables=no
LockPersonality=no
RestrictAddressFamilies=
RestrictRealtime=no
RestrictSUIDSGID=no
RemoveIPC=no
SystemCallArchitectures=
SystemCallFilter=
CapabilityBoundingSet=
NoExecPaths=
ExecPaths=
OVERRIDE
      fi
      $SUDO systemctl daemon-reload
      $SUDO systemctl enable --now redis-server
      # Stream durability: the chat backend queues realtime messages through
      # a Redis stream before Mongo; appendonly makes an undrained stream
      # survive a Redis restart. Rewrite persists it into /etc/redis/redis.conf.
      redis-cli -p 6379 config set appendonly yes || true
      $SUDO redis-cli -p 6379 config rewrite || true
      redis-cli -p 6379 ping >/dev/null 2>&1 \
        || fail "Redis did not start on port 6379"
      ;;
    golang|golang@*|go)
      apt_install golang-go
      ;;
    static)
      need_cmd python3 || apt_install python3
      ;;
    rust)
      ensure_apt_repo_prereqs
      install_rust_system_wide
      ;;
    onnxruntime|onnxruntime@1.28)
      bash "$SCRIPT_DIR/install-onnxruntime.sh" --ensure
      ;;
    ffmpeg)
      apt-get update -qq && apt-get install -y -qq ffmpeg
      ;;
    leptos)
      # Leptos/Rust frontend — a static wasm site (trunk dist). No runtime to
      # provision; python3 (the static server) is standard on Debian.
      ;;
    *)
      fail "Unsupported runtime token for Linux: $token"
      ;;
  esac
}

install_macos_redis_binary() {
  # Prefer Homebrew (already used for MongoDB on this machine): a bottled
  # redis formula is the fastest, cleanest path. MacPorts remains the fallback
  # for hosts without Homebrew. Eco refuses to compile Redis from source.
  if need_cmd brew; then
    if ! redis_server_binary >/dev/null 2>&1; then
      log "Installing Redis via Homebrew..."
      brew install redis
    fi
    local server=""
    server="$(redis_server_binary || true)"
    [[ -n "$server" ]] || fail "Homebrew completed but redis-server was not added to PATH."
    local version major
    version="$("$server" --version 2>/dev/null)"
    major="$(printf '%s' "$version" | sed -nE 's/.*v=([0-9]+)\..*/\1/p')"
    [[ "$major" =~ ^[0-9]+$ ]] && [[ "$major" -ge 7 ]] || fail "Homebrew did not install Redis 7 or newer."
    printf '%s' "$server"
    return 0
  fi

  # Force MacPorts binary-only mode. It downloads a signed, prebuilt archive
  # for the host OS and fails if an archive is unavailable; it can never fall
  # back to compiling Redis on a macOS 12 developer machine.
  need_cmd port || fail "Redis on macOS requires Homebrew or MacPorts. Install one of them, then rerun eco provision; Eco refuses a source build."
  log "Refreshing MacPorts metadata before resolving Redis archives..."
  if ! $SUDO port selfupdate; then
    fail "MacPorts metadata refresh failed; Redis was not installed."
  fi
  log "Installing Redis from a MacPorts binary archive..."
  if ! $SUDO port -b install redis; then
    fail "MacPorts has no Redis binary archive for this macOS release. Eco deliberately refuses to compile Redis from source."
  fi

  local server=""
  server="$(redis_server_binary || true)"
  [[ -n "$server" ]] || fail "MacPorts completed but redis-server was not added to PATH."
  local version major
  version="$("$server" --version 2>/dev/null)"
  major="$(printf '%s' "$version" | sed -nE 's/.*v=([0-9]+)\..*/\1/p')"
  [[ "$major" =~ ^[0-9]+$ ]] && [[ "$major" -ge 7 ]] || fail "MacPorts did not install Redis 7 or newer."
  printf '%s' "$server"
}

start_managed_redis() {
  local server="$1" cli=""
  local port="${REDIS_PORT:-6379}"
  # A running instance is left untouched; `--daemonize` is only used for the
  # first start and Redis remains private to the local development machine.
  cli="$(dirname "$server")/redis-cli"
  if [[ ! -x "$cli" ]] && need_cmd redis-cli; then
    cli="$(command -v redis-cli)"
  fi
  if [[ -x "$cli" ]] && "$cli" --raw -p "$port" PING 2>/dev/null | grep -qx 'PONG'; then
    return 0
  fi
  "$server" --daemonize yes --port "$port" --bind 127.0.0.1 --appendonly yes --appendfsync everysec
}

start_managed_mongodb() {
  local server=""
  server="$(command -v mongod || true)"
  [[ -n "$server" ]] || return 0
  local port="${MONGODB_PORT:-27017}"
  # A running instance is left untouched; `--fork` is only used for the first
  # start. Data + log stay under the user's home so `eco up dev` is
  # self-contained and needs no root.
  local dbpath="${HOME}/.eco-mongodb/db"
  local logpath="${HOME}/.eco-mongodb/mongod.log"
  mkdir -p "$dbpath"
  if pgrep -f "mongod.*$dbpath" >/dev/null 2>&1; then
    return 0
  fi
  log "Starting managed MongoDB (data: $dbpath)..."
  "$server" --dbpath "$dbpath" --bind_ip 127.0.0.1 --port "$port" \
    --logpath "$logpath" --fork --logappend
  # Give it a moment, then verify it answers.
  local attempt
  for attempt in {1..15}; do
    if mongosh --quiet --eval 'db.runCommand({ping:1}).ok' "mongodb://127.0.0.1:${port}" 2>/dev/null | grep -qx '1'; then
      ok "Managed MongoDB ready."
      return 0
    fi
    sleep 1
  done
  fail "Managed MongoDB did not become healthy. Check ${logpath}."
}

install_token_macos() {
  local token="$1"
  local macos_major=""
  macos_major="$(macos_major_version)"
  # PostgreSQL on macOS 12 and older is intentionally handled through an
  # installed Postgres.app/psql and must not require Homebrew.
  if [[ "$token" != "postgresql@15" && "$token" != "redis@7" || ! "$macos_major" =~ ^[0-9]+$ || "$macos_major" -gt 12 ]]; then
    ensure_brew
  fi
  case "$token" in
    git) brew_install_formula git ;;
    openssh-client) brew_install_formula openssh ;;
    curl) brew_install_formula curl ;;
    jq) brew_install_formula jq ;;
    ca-certificates) brew_install_formula ca-certificates ;;
    java@17) brew_install_formula openjdk@17 ;;
    maven) brew_install_formula maven ;;
    node@20) brew_install_formula node@20 ;;
    npm) brew_install_formula node@20 ;;
    pm2)
      if ! need_cmd npm; then
        brew_install_formula node@20
      fi
      if need_cmd pm2; then
        ok "PM2 already installed."
      else
        log "Installing PM2 globally via npm..."
        npm install -g pm2
      fi
      ;;
    postgresql@15)
      # On macOS 12 and earlier, use the psql binary supplied by an installed
      # Postgres.app rather than attempting a Homebrew PostgreSQL install.
      # Newer macOS versions use the managed Homebrew formula.
      if [[ "$macos_major" =~ ^[0-9]+$ ]] && [[ "$macos_major" -le 12 ]]; then
        local postgres_psql=""
        postgres_psql="$(find_macos_postgres_psql || true)"
        [[ -n "$postgres_psql" ]] || fail "macOS ${macos_major} requires Postgres.app with psql installed at /Applications/Postgres.app. Install Postgres.app, then rerun provisioning."
        ok "Using Postgres.app psql: $postgres_psql"
      else
        brew_install_formula postgresql@15
      fi
      ;;
    mongodb@7)
      brew tap mongodb/brew
      brew_install_formula mongodb-community@7.0
      start_managed_mongodb
      ;;
    redis@7)
      local redis_server=""
      redis_server="$(redis_server_binary || true)"
      if [[ -z "$redis_server" ]]; then
        redis_server="$(install_macos_redis_binary)"
      fi
      start_managed_redis "$redis_server"
      ok "Managed Redis ready: $redis_server"
      ;;
    golang|golang@*|go)
      brew_install_formula go
      ;;
    static)
      need_cmd python3 || brew_install_formula python@3.12
      ;;
    rust)
      install_rust_system_wide
      ;;
    onnxruntime|onnxruntime@1.28)
      bash "$SCRIPT_DIR/install-onnxruntime.sh" --ensure
      ;;
    ffmpeg)
      need_cmd brew || fail "Homebrew is required to provision ffmpeg on macOS"
      HOMEBREW_NO_AUTO_UPDATE=1 brew install ffmpeg
      ;;
    *)
      fail "Unsupported runtime token for macOS: $token"
      ;;
  esac
}

install_token() {
  local token="$1"
  case "$(uname -s)" in
    Darwin)
      install_token_macos "$token"
      ;;
    Linux)
      case "$(linux_family)" in
        debian)
          install_token_debian "$token"
          ;;
        *)
          fail "Unsupported Linux distribution. Current script supports Debian/Ubuntu style systems."
          ;;
      esac
      ;;
    *)
      fail "Unsupported OS: $(uname -s)"
      ;;
  esac
}

show_plan() {
  echo ""
  echo -e "${BOLD}Resolved ecompose${RESET}"
  echo -e "  ${CYAN}File${RESET}      $ECOMPOSE_FILE"
  if [[ -n "${SERVICES[0]:-}" ]]; then
    echo -e "  ${CYAN}Services${RESET}  ${SERVICES[*]}"
  fi
  echo -e "  ${CYAN}OS${RESET}        $(uname -s)"
  echo ""
  echo -e "${BOLD}Runtime tokens${RESET}"
  for token in "${TOKENS[@]:-}"; do
    if token_is_skipped "$token"; then
      echo "  - $token (skipped: dev-optional runtime unavailable locally)"
    else
      echo "  - $token"
    fi
  done
  if [[ "$MINIO_DECLARED" -eq 1 ]]; then
    echo "  - managed MinIO S3 storage"
  fi
  echo ""
}

show_post_install_notes() {
  case "$(uname -s)" in
    Darwin)
      cat <<'EOF'
macOS notes:
  - `openjdk@17` and `node@20` may require PATH/link setup depending on your Brew prefix.
  - `postgresql@15` means PostgreSQL 15+ on dev machines, and `mongodb-community@7.0` is installed but not auto-started by this script.
EOF
      ;;
    Linux)
      cat <<'EOF'
Linux notes:
  - Packages are installed, but services are not auto-enabled or auto-started by this script.
  - Start PostgreSQL / MongoDB with your CT's service manager when needed.
EOF
      ;;
  esac
}

parse_args() {
  local arg
  for arg in "$@"; do
    case "$arg" in
      -h|--help|help)
        usage
        exit 0
        ;;
      --plan)
        PLAN_ONLY=1
        ;;
      *)
        if [[ -n "$ECOMPOSE_INPUT" ]]; then
          fail "Only one ecompose path/project argument is supported."
        fi
        ECOMPOSE_INPUT="$arg"
        ;;
    esac
  done
}

main() {
  parse_args "$@"

  if [[ "${ECOMPOSE_INPUT:-}" == "-h" || "${ECOMPOSE_INPUT:-}" == "--help" || "${ECOMPOSE_INPUT:-}" == "help" ]]; then
    usage
    exit 0
  fi

  resolve_ecompose_file
  load_tokens_from_ecompose
  if manifest_declares_minio; then
    MINIO_DECLARED=1
  fi
  show_plan

  if [[ "$PLAN_ONLY" -eq 1 ]]; then
    ok "Plan complete. No packages were installed."
    exit 0
  fi

  for token in "${TOKENS[@]:-}"; do
    if [[ "$token" == "rust" && is_prod_mode ]]; then
      ok "Rust is built on the developer machine and shipped as binaries; production CT does not install or build Rust."
      continue
    fi
    if token_is_skipped "$token"; then
      warn "Skipping $token (declared only by a dev-optional domain that can't run locally)."
      continue
    fi
    if is_token_satisfied "$token"; then
      ok "$token already installed."
      # Dev-mode managed runtimes must be running even when the binary was
      # installed on an earlier run; a present binary alone does not mean the
      # daemon is up (or was started by a previous machine reboot).
      if [[ "$token" == "mongodb@7" && ! is_prod_mode ]]; then
        start_managed_mongodb
      elif [[ "$token" == "redis@7" && ! is_prod_mode ]]; then
        local redis_server=""
        redis_server="$(redis_server_binary || true)"
        if [[ -n "$redis_server" ]]; then
          start_managed_redis "$redis_server"
        fi
      fi
      continue
    fi
    log "Provisioning $token ..."
    install_token "$token"
  done

  if [[ "$MINIO_DECLARED" -eq 1 ]]; then
    if is_prod_mode && [[ -n "${ECO_MINIO_CLIENT_FILE:-}" ]]; then
      # `eco up` has already provisioned the dedicated MinIO CT and copied
      # its client credentials into this app CT. Do not start a second object
      # store here.
      ok "Using dedicated MinIO client config: ${ECO_MINIO_CLIENT_FILE}"
    elif is_prod_mode; then
      warn "storage.minio is declared. Production MinIO is provisioned by eco up from storage.minio.ct."
    else
      log "Provisioning managed MinIO for local development..."
      bash "$SCRIPT_DIR/install-minio.sh" --ensure
    fi
  fi

  echo ""
  ok "Provisioning complete."
  show_post_install_notes
}

main "$@"
