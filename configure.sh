#!/bin/bash
# configure.sh — Multi-service project configurator.
# Scans sibling directories for services, configures .env files (ports, MongoDB),
# and generates a PM2 ecosystem.config.js for one-command startup.
# Lives in eco/configure.sh; run from anywhere.
# Usage: configure.sh [i|info] — pass 'i' or 'info' to display existing config

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="${ECOLOGY_WORKSPACE_ROOT:-$(cd "$SCRIPT_DIR/.." && pwd)}"
DEPLOY_MODE=""
NON_INTERACTIVE=false

CONFIG_FILE=""
PM2_DIR="${PM2_DIR:-}"
INFO_MODE=false
GATEWAY_PORT=""
GATEWAY_FILE=""

for arg in "$@"; do
  case "$arg" in
    i|info)
      INFO_MODE=true
      ;;
    --non-interactive|-y)
      NON_INTERACTIVE=true
      ;;
  esac
done

if [[ "${ECO_NON_INTERACTIVE:-}" =~ ^(1|true|yes|on)$ ]]; then
  NON_INTERACTIVE=true
fi

detect_deploy_mode() {
  if [[ -n "$ECO_DEPLOY_MODE" ]]; then
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

# PROJECT_DIR is exported by init.sh; when running standalone, discover it.
_is_lean_manifest_dir() {
  local dir="$1"
  [[ -f "$dir/ecompose.yml" ]] || return 1
  local sub
  for sub in "$dir"/*/; do
    [[ -d "$sub" ]] || continue
    if [[ -f "$sub/Cargo.toml" || -f "$sub/pom.xml" || -f "$sub/package.json" || -f "$sub/start.sh" ]]; then
      return 1
    fi
  done
  return 0
}

# The registry keys every row by PROJECT_NAME, so that name must be stable
# across how the estate is reached. The manifest's top-level `project:` field
# is the canonical name (what `eco up` passes); the directory basename is only
# a fallback for estates whose manifest predates the field.
manifest_project_name() {
  local manifest_path="$1"
  [[ -f "$manifest_path" ]] || return 1
  awk -v project="project:" '
    /^[ \t]*#/ { next }
    /^project:[ \t]*/ {
      value = $0
      sub(/^project:[ \t]*/, "", value)
      gsub(/[ \t\r]+$/, "", value)
      gsub(/^["'"'"']|["'"'"']$/, "", value)
      if (value != "") print value
      exit
    }
  ' "$manifest_path"
}

resolve_project_dir() {
  if [[ -n "$PROJECT_DIR" ]]; then
    return
  fi

  local current_dir="$PWD"
  while [[ "$current_dir" == "$PROJECT_ROOT"* && "$current_dir" != "/" ]]; do
    if [[ -f "$current_dir/ecompose.yml" ]]; then
      PROJECT_DIR="$current_dir"
      local manifest_name=""
      manifest_name="$(manifest_project_name "$current_dir/ecompose.yml" || true)"
      PROJECT_NAME="${PROJECT_NAME:-${manifest_name:-$(basename "$current_dir")}}"
      echo -e "  Using project from manifest: ${CYAN}${PROJECT_NAME}${RESET}"
      return
    fi
    local parent_dir
    parent_dir="$(dirname "$current_dir")"
    [[ "$parent_dir" == "$current_dir" ]] && break
    current_dir="$parent_dir"
  done

  local -a found_dirs found_names
  for d in "$PROJECT_ROOT"/*/; do
    local bname
    bname="$(basename "$d")"
    if [[ "$bname" == "core" || "$bname" == "eco" || ! -d "$d" ]]; then continue; fi
    found_dirs+=("${d%/}")
    found_names+=("$bname")
  done

  if [[ ${#found_dirs[@]} -eq 0 ]]; then
    echo -e "${RED}No project directories found under $PROJECT_ROOT. Run init.sh first.${RESET}"
    exit 1
  elif [[ ${#found_dirs[@]} -eq 1 ]]; then
    PROJECT_DIR="${found_dirs[0]}"
    PROJECT_NAME="${PROJECT_NAME:-${found_names[0]}}"
    echo -e "  Using project: ${CYAN}${PROJECT_NAME}${RESET}"
  elif ! is_interactive; then
    local pwd_basename candidate_name candidate_dir=""
    pwd_basename="$(basename "$PWD")"

    if [[ -n "$PROJECT_NAME" && -d "$PROJECT_ROOT/$PROJECT_NAME" ]]; then
      candidate_name="$PROJECT_NAME"
      candidate_dir="$PROJECT_ROOT/$PROJECT_NAME"
    elif [[ -d "$PROJECT_ROOT/$pwd_basename" ]]; then
      candidate_name="$pwd_basename"
      candidate_dir="$PROJECT_ROOT/$pwd_basename"
    else
      candidate_name="${found_names[0]}"
      candidate_dir="${found_dirs[0]}"
    fi

    PROJECT_DIR="$candidate_dir"
    PROJECT_NAME="${PROJECT_NAME:-$candidate_name}"
    echo -e "  Using project (non-interactive): ${CYAN}${PROJECT_NAME}${RESET}"
  else
    echo ""
    echo -e "${BOLD}Select a project (↑↓ arrows, Enter to confirm):${RESET}"
    local selected=0
    local count=${#found_names[@]}

    for i in "${!found_names[@]}"; do
      if [[ $i -eq $selected ]]; then
        echo -e "  ${CYAN}❯${RESET} ${found_names[$i]}"
      else
        echo "    ${found_names[$i]}"
      fi
    done

    while true; do
      local key=""
      read -s -n1 key 2>/dev/null
      if [[ "$key" == $'\033' ]]; then
        local rest
        read -s -n2 rest 2>/dev/null
        key="$key$rest"
      fi
      if [[ -z "$key" ]]; then
        PROJECT_DIR="${found_dirs[$selected]}"
        PROJECT_NAME="${PROJECT_NAME:-${found_names[$selected]}}"
        echo ""
        break
      fi
      case "$key" in
        $'\033[A') [[ $selected -gt 0 ]] && selected=$((selected - 1)) ;;
        $'\033[B') [[ $selected -lt $((count - 1)) ]] && selected=$((selected + 1)) ;;
      esac
      echo -en "\033[${count}A"
      for i in "${!found_names[@]}"; do
        if [[ $i -eq $selected ]]; then
          echo -e "  ${CYAN}❯${RESET} ${found_names[$i]}"
        else
          echo "    ${found_names[$i]}"
        fi
      done
    done
  fi
}

BOLD='\033[1m'
CYAN='\033[0;36m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
RESET='\033[0m'

is_interactive() {
  [[ "$NON_INTERACTIVE" != true && -t 0 && -t 1 ]]
}

is_prod_mode() {
  [[ "$DEPLOY_MODE" == "prod" ]]
}

is_truthy() {
  [[ "$1" =~ ^(1|true|yes|on)$ ]]
}

# ─── Registry-backed resource allocation ────────────────────────────────────
#
# Eco keeps the authoritative record of every managed resource (service
# ports, the estate gateway port, the index port, database credentials) in
# a local SQLite registry (see src/lib/registry.js) rather than in
# ephemeral .env files. Ports are assigned once and never change:
# get-or-allocate returns an existing assignment, adopts a legacy port on
# first migration, and only creates a fresh random port for a genuinely new
# service. The registry lives per machine -- ~/.eco/registry.db -- and scopes
# rows by hostname so a shared machine (multiple estates, or the dev laptop)
# has one collision-free port namespace.
REGISTRY_SCOPE="${ECO_REGISTRY_SCOPE:-$(hostname 2>/dev/null || printf 'localhost')}"

registry_node() {
  # The Rust port is the primary implementation: when invoked from the
  # bundled eco binary, forward registry operations to `eco registry`
  # instead of the legacy Node helper. Fall back to the Node CLI only when
  # running straight from the source checkout (no ECO_BIN set), so the
  # migration stays reversible.
  if [[ -n "${ECO_BIN:-}" ]] && command -v "$ECO_BIN" >/dev/null 2>&1; then
    "$ECO_BIN" registry "$@"
  elif [[ -f "$SCRIPT_DIR/src/bin/registry-cli.js" ]]; then
    node "$SCRIPT_DIR/src/bin/registry-cli.js" "$@"
  else
    echo "registry: no eco binary (ECO_BIN) and no Node helper available" >&2
    return 1
  fi
}

# get-or-allocate: returns the existing assignment, or allocates a random
# free port (optionally preferring `preferred` for the very first run).
registry_get_or_allocate() {
  local service="$1" type="$2" env_var="$3" preferred="$4"
  if [[ -n "$preferred" ]]; then
    registry_node get-or-allocate --scope "$REGISTRY_SCOPE" --project "$PROJECT_NAME" --service "$service" --type "$type" --env-var "$env_var" --preferred "$preferred"
  else
    registry_node get-or-allocate --scope "$REGISTRY_SCOPE" --project "$PROJECT_NAME" --service "$service" --type "$type" --env-var "$env_var"
  fi
}

# lookup: prints an existing registry assignment, or nothing if absent.
registry_lookup() {
  local service="$1" type="$2"
  registry_node lookup --scope "$REGISTRY_SCOPE" --project "$PROJECT_NAME" --service "$service" --type "$type"
}

# has-project: prints 1 if the registry already holds any rows for this
# scope+project, 0 otherwise.
registry_has_project() {
  registry_node has-project --scope "$REGISTRY_SCOPE" --project "$PROJECT_NAME"
}

# seed: adopt an existing legacy port into the registry without an in-use
# check (the service is usually running on it right now).
registry_seed() {
  local service="$1" type="$2" env_var="$3" port="$4"
  registry_node seed --scope "$REGISTRY_SCOPE" --project "$PROJECT_NAME" --service "$service" --type "$type" --env-var "$env_var" --port "$port"
}

# pin: enforce an explicit port (errors if reserved, in use, or taken).
registry_pin() {
  local service="$1" type="$2" env_var="$3" port="$4"
  registry_node pin --scope "$REGISTRY_SCOPE" --project "$PROJECT_NAME" --service "$service" --type "$type" --env-var "$env_var" --port "$port"
}

# record-db: record a managed database's metadata (and optional encrypted
# password) in the registry so the estate's data layer is queryable.
registry_record_db() {
  local service="$1" db_type="$2" port="$3" db_name="$4" username="$5" password="$6"
  registry_node record-db --scope "$REGISTRY_SCOPE" --project "$PROJECT_NAME" --service "$service" --db-type "$db_type" --port "$port" --db-name "$db_name" --username "$username" --password "$password"
}

resolve_manifest_path() {
  if [[ -n "$PROJECT_DIR" && -f "$PROJECT_DIR/ecompose.yml" ]]; then
    printf '%s' "$PROJECT_DIR/ecompose.yml"
    return 0
  fi

  # Lean-bootstrap layout (e.g. training_bootstrap): PROJECT_DIR is the
  # estate root (a sibling of the manifest, not its parent) and PM2_DIR is
  # the actual directory holding ecompose.yml -- select_pm2_dir has always
  # resolved this correctly by the time this runs, but nothing here was
  # checking it. Without this, every real `eco up`/`eco configure` run
  # against a *_bootstrap estate silently fails to find the manifest here
  # even though discover_services finds the same one fine via its own
  # sibling-scan fallback -- meaning minio_configured/resolve_minio_endpoint
  # (and anything else built on resolve_manifest_path) silently no-op
  # instead of erroring, which is what made storage.minio wiring look like
  # it worked in isolated tests but never actually fired for real.
  if [[ -n "$PM2_DIR" && -f "$PM2_DIR/ecompose.yml" ]]; then
    printf '%s' "$PM2_DIR/ecompose.yml"
    return 0
  fi

  if [[ -n "$PROJECT_NAME" && -f "$PROJECT_ROOT/$PROJECT_NAME/ecompose.yml" ]]; then
    printf '%s' "$PROJECT_ROOT/$PROJECT_NAME/ecompose.yml"
    return 0
  fi

  return 1
}

parse_manifest_services() {
  local manifest_path="$1"
  [[ -z "$manifest_path" || ! -f "$manifest_path" ]] && return 1

  awk '
    BEGIN { in_services = 0; current = "" }
    /^[[:space:]]*#/ { next }
    /^services:[[:space:]]*$/ { in_services = 1; next }
    in_services && /^[^[:space:]].*:[[:space:]]*$/ { exit }
    in_services && /^  [A-Za-z0-9._-]+:[[:space:]]*$/ {
      current = $1
      sub(/:$/, "", current)
      next
    }
    in_services && current != "" && /^    path:[[:space:]]*/ {
      path = $0
      sub(/^    path:[[:space:]]*/, "", path)
      sub(/[[:space:]]+#.*$/, "", path)
      gsub(/^["'"'"']|["'"'"']$/, "", path)
      print current "|" path
      current = ""
    }
  ' "$manifest_path"
}

# Prints `service_name|domain` for every `lxs: <domain>@<version>` service in
# ecompose.yml. eco installs each of these into <project>/<service_name>/ with
# a generated .env, but configure.sh's source scan never discovers them, so
# gateway routing / port lookups must consult this list to reach them.
parse_manifest_lxs_services() {
  local manifest_path="$1"
  [[ -z "$manifest_path" || ! -f "$manifest_path" ]] && return 1

  awk '
    BEGIN { in_services = 0; current = "" }
    /^[[:space:]]*#/ { next }
    /^services:[[:space:]]*$/ { in_services = 1; next }
    in_services && /^[^[:space:]].*:[[:space:]]*$/ { exit }
    in_services && /^  [A-Za-z0-9._-]+:[[:space:]]*$/ {
      current = $1
      sub(/:$/, "", current)
      next
    }
    in_services && current != "" && /^    lxs:[[:space:]]*/ {
      lxs = $0
      sub(/^    lxs:[[:space:]]*/, "", lxs)
      sub(/[[:space:]]+#.*$/, "", lxs)
      gsub(/^["'"'"']|["'"'"']$/, "", lxs)
      domain = lxs
      sub(/@.*$/, "", domain)
      print current "|" domain
      current = ""
    }
  ' "$manifest_path"
}

# A MongoDB runtime in ecompose.yml means Eco owns a local, estate-scoped
# database for that service. This is intentionally independent of a domain's
# .env.example: older domains may not declare MONGODB_URI at all, but their
# composition still needs a usable URI after `eco configure` / `eco up`.
manifest_service_uses_runtime() {
  local manifest_path="$1" service_name="$2" runtime_prefix="$3"
  [[ -z "$manifest_path" || ! -f "$manifest_path" ]] && return 1

  awk -v service_name="$service_name" -v runtime_prefix="$runtime_prefix" '
    BEGIN { in_services = 0; active = 0; found = 0 }
    /^[[:space:]]*#/ { next }
    /^services:[[:space:]]*$/ { in_services = 1; next }
    in_services && /^[^[:space:]].*:[[:space:]]*$/ { exit }
    in_services && /^  [A-Za-z0-9_-]+:[[:space:]]*$/ {
      name = $1
      sub(/:$/, "", name)
      active = (name == service_name)
      next
    }
    in_services && active && /^      -[[:space:]]*/ {
      runtime = $0
      sub(/^      -[[:space:]]*/, "", runtime)
      if (index(runtime, runtime_prefix) == 1) { found = 1; exit }
    }
    END { exit !found }
  ' "$manifest_path"
}

# Reads a scalar nested two levels deep in ecompose.yml, e.g. section
# "storage", subsection "minio", key "endpoint" for:
#   storage:
#     minio:
#       endpoint: https://minio.example.com
# Prints nothing (not an error) if the manifest, section, subsection, or
# key isn't present -- callers treat "not declared" as "feature not
# opted into for this estate", same as every other optional ecompose block.
ecompose_nested_value() {
  local manifest_path="$1" section="$2" subsection="$3" key="$4"
  [[ -z "$manifest_path" || ! -f "$manifest_path" ]] && return

  awk -v section="$section" -v subsection="$subsection" -v key="$key" '
    BEGIN { in_section = 0; in_sub = 0 }
    /^[[:space:]]*#/ { next }
    $0 ~ "^" section ":[[:space:]]*$" { in_section = 1; in_sub = 0; next }
    in_section && /^[^[:space:]].*:[[:space:]]*$/ { exit }
    in_section && $0 ~ "^  " subsection ":[[:space:]]*$" { in_sub = 1; next }
    in_section && in_sub && /^  [A-Za-z0-9_-]+:[[:space:]]*$/ { in_sub = 0 }
    in_section && in_sub && $0 ~ "^    " key ":[[:space:]]*" {
      val = $0
      sub("^    " key ":[[:space:]]*", "", val)
      sub(/[[:space:]]+#.*$/, "", val)
      gsub(/^["'"'"']|["'"'"']$/, "", val)
      print val
      exit
    }
  ' "$manifest_path"
}

# Whether this estate declared a storage.minio block at all -- absence
# means "not opted into object storage", same meaning as every other
# optional ecompose block.
minio_configured() {
  local manifest_path=""
  manifest_path="$(resolve_manifest_path || true)"
  [[ -z "$manifest_path" ]] && return 1

  awk '
    /^[[:space:]]*#/ { next }
    /^storage:[[:space:]]*$/ { in_storage = 1; next }
    in_storage && /^[^[:space:]].*:[[:space:]]*$/ { exit }
    in_storage && /^  minio:[[:space:]]*$/ { found = 1; exit }
    END { exit !found }
  ' "$manifest_path"
}

# Client settings are deliberately separate from ecompose.yml: the manifest
# names the MinIO capability/CT, while credentials are generated once by Eco
# and stay in a mode-appropriate 0600 file. In production `eco up` copies a
# CT-specific client file here with a private bridge-network endpoint.
minio_client_config_file() {
  if [[ -n "${ECO_MINIO_CLIENT_FILE:-}" ]]; then
    printf '%s' "$ECO_MINIO_CLIENT_FILE"
  elif is_prod_mode; then
    printf '%s' '/etc/eco/minio-client.env'
  else
    printf '%s' "$HOME/.eco-minio/client.env"
  fi
}

# Node treats a `.js` PM2 config as ESM (breaking PM2's own require()-based
# loading, and every require()-based port/service lookup eco itself does
# against it) whenever the nearest package.json declares "type": "module" --
# e.g. chronic_bootstrap's Vite/Phaser package.json. PM2's own docs
# recommend the .cjs extension for exactly this case, since that extension
# always forces CommonJS regardless of package.json. Detected against
# whichever directory the config will actually live in.
pm2_config_is_esm() {
  local dir="$1"
  local pkg="$dir/package.json"
  [[ -f "$pkg" ]] || return 1
  grep -qE '"type"[[:space:]]*:[[:space:]]*"module"' "$pkg"
}

pm2_config_filename() {
  local dir="$1"
  if pm2_config_is_esm "$dir"; then
    printf 'ecosystem.config.cjs'
  else
    printf 'ecosystem.config.js'
  fi
}

# An existing config may be either extension depending on when it was last
# generated -- .cjs takes priority since that's what a fresh ESM-aware
# generate_pm2 run now produces for an ESM project, and generate_pm2 removes
# the other extension's stale file on regeneration (see below) so this
# should only ever find one in practice.
find_pm2_config() {
  local dir="$1"
  [[ -z "$dir" ]] && return 1
  if [[ -f "$dir/ecosystem.config.cjs" ]]; then
    printf '%s' "$dir/ecosystem.config.cjs"
    return 0
  fi
  if [[ -f "$dir/ecosystem.config.js" ]]; then
    printf '%s' "$dir/ecosystem.config.js"
    return 0
  fi
  return 1
}

resolve_existing_pm2_config() {
  local found=""
  if [[ -n "$PM2_DIR" ]] && found="$(find_pm2_config "$PM2_DIR")"; then
    printf '%s' "$found"
    return 0
  fi

  if [[ -n "$PROJECT_DIR" ]] && found="$(find_pm2_config "$PROJECT_DIR")"; then
    printf '%s' "$found"
    return 0
  fi

  return 1
}

lookup_existing_pm2_port() {
  local service_name="$1"
  local config_path=""
  config_path="$(resolve_existing_pm2_config || true)"
  [[ -z "$config_path" || ! -f "$config_path" ]] && return 1

  # Only trust ports from eco-generated files. A file without the eco marker
  # (e.g. a dev-mode ecosystem.config.js committed to the repo) carries
  # hardcoded dev ports (3001, 3002, etc.) that must not be preserved in prod.
  if ! head -n1 "$config_path" | grep -qF "// Generated by eco configure.sh -- do not edit by hand."; then
    return 1
  fi

  CONFIG_PATH="$config_path" PROJECT_LABEL="$PROJECT_NAME" SERVICE_LABEL="$service_name" node - <<'EOF' 2>/dev/null
    const config = require(process.env.CONFIG_PATH);
    const project = process.env.PROJECT_LABEL;
    const service = process.env.SERVICE_LABEL;
    const target = `${project}-${service}`;
    const apps = config.apps || [];
    // Match only apps that belong to this project: exact name, a bare
    // project-less name, or a name scoped under this project's prefix.
    // A bare `endsWith('-'+service)` would let a generic service name like
    // "frontend" steal a same-suffix port from another estate on the machine.
    const inProject = (name) => name === service || name.startsWith(`${project}-`);
    const match = apps.find((app) =>
      inProject(app.name) && (app.name === target || app.name === service || app.name.endsWith('-' + service))
    );
    if (!match) process.exit(1);
    const env = match.env || {};
    const port = env.PORT || env.SERVER_PORT || '';
    if (!String(port).match(/^\d+$/)) process.exit(1);
    process.stdout.write(String(port));
EOF
}

# lookup_live_service_port: harvests the port a service is *actually running
# on right now* from PM2's live process env, independent of any on-disk
# config. This is the authoritative recovery source when the registry DB has
# been deleted: the PM2 daemon keeps the real env the app was started with,
# so a rebuilt registry adopts the exact same port instead of allocating a
# new random one (which would break the tunnel origin, .env, and DB links).
lookup_live_service_port() {
  local service_name="$1"
  local project_label="$PROJECT_NAME"
  local app_name="${project_label}-${service_name}"
  local jlist=""
  jlist="$(pm2 jlist 2>/dev/null || true)"
  [[ -z "$jlist" ]] && return 1

  PROJECT_LABEL="$project_label" SERVICE_LABEL="$service_name" PM2_JLIST="$jlist" node - <<'EOF' 2>/dev/null
    const project = process.env.PROJECT_LABEL;
    const service = process.env.SERVICE_LABEL;
    const target = `${project}-${service}`;
    let apps;
    try { apps = JSON.parse(process.env.PM2_JLIST); }
    catch { process.exit(1); }
    // Match only apps belonging to this project (same scoping as
    // lookup_existing_pm2_port): a generic service name must never adopt a
    // same-suffix port from another estate running on this machine.
    const inProject = (name) => name === service || name.startsWith(`${project}-`);
    const match = apps.find((app) =>
      inProject(app.name) &&
      (app.name === target || app.name === service || app.name.endsWith('-' + service)) &&
      app.pm2_env && app.pm2_env.status !== 'errored'
    );
    if (!match) process.exit(1);
    const env = (match.pm2_env && match.pm2_env.env) || {};
    const port = env.PORT || env.SERVER_PORT || '';
    if (!String(port).match(/^\d+$/)) process.exit(1);
    process.stdout.write(String(port));
EOF
}

parse_expose_value() {
  local key="$1"
  local manifest_path=""
  manifest_path="$(resolve_manifest_path || true)"
  [[ -z "$manifest_path" || ! -f "$manifest_path" ]] && return 1

  awk -v target="$key" '
    BEGIN { in_expose = 0 }
    /^[[:space:]]*#/ { next }
    /^expose:[[:space:]]*$/ { in_expose = 1; next }
    in_expose && /^[^[:space:]].*:[[:space:]]*$/ { exit }
    in_expose {
      pattern = "^[[:space:]]+" target ":[[:space:]]*"
      if ($0 ~ pattern) {
        value = $0
        sub(pattern, "", value)
        sub(/[[:space:]]+#.*$/, "", value)
        gsub(/^["'"'"']|["'"'"']$/, "", value)
        print value
        exit
      }
    }
  ' "$manifest_path"
}

resolve_public_scheme() {
  local scheme="${ECO_PUBLIC_SCHEME:-$(parse_expose_value "scheme" || true)}"
  scheme="${scheme:-https}"
  printf '%s' "$scheme"
}

resolve_public_app_origin() {
  if [[ -n "$ECO_PUBLIC_APP_URL" ]]; then
    printf '%s' "$ECO_PUBLIC_APP_URL"
    return 0
  fi

  local explicit_origin
  explicit_origin="$(parse_expose_value "app_url" || true)"
  if [[ -n "$explicit_origin" ]]; then
    printf '%s' "$explicit_origin"
    return 0
  fi

  explicit_origin="$(parse_expose_value "origin" || true)"
  if [[ -n "$explicit_origin" ]]; then
    printf '%s' "$explicit_origin"
    return 0
  fi

  local hostname
  hostname="$(parse_expose_value "hostname" || true)"
  if [[ -n "$hostname" ]]; then
    printf '%s://%s' "$(resolve_public_scheme)" "$hostname"
    return 0
  fi

  return 1
}

resolve_public_api_base_url() {
  if [[ -n "$ECO_PUBLIC_API_URL" ]]; then
    printf '%s' "$ECO_PUBLIC_API_URL"
    return 0
  fi

  local explicit
  explicit="$(parse_expose_value "api_url" || true)"
  if [[ -n "$explicit" ]]; then
    printf '%s' "$explicit"
    return 0
  fi

  local api_hostname
  api_hostname="$(parse_expose_value "api_hostname" || true)"
  if [[ -n "$api_hostname" ]]; then
    printf '%s://%s/api' "$(resolve_public_scheme)" "$api_hostname"
    return 0
  fi

  local app_origin
  app_origin="$(resolve_public_app_origin || true)"
  if [[ -n "$app_origin" ]]; then
    printf '%s/api' "$app_origin"
    return 0
  fi

  return 1
}

# The public gateway preserves the request path when it forwards /api/* to
# the estate backend. A Rust backend may therefore declare a more specific
# route prefix (for example /api/v1) in .env.example. Keep the ordinary
# public gateway base (/api) for conventional services, but extend it for
# that backend's declared route contract.
public_api_base_url_for_backend() {
  local backend_name="$1"
  local base_url="$2"
  local api_path
  api_path="$(api_base_path_for_backend "$backend_name")"

  if [[ "$base_url" == */api && "$api_path" == /api/* ]]; then
    printf '%s%s' "$base_url" "${api_path#/api}"
    return 0
  fi

  printf '%s' "$base_url"
}

resolve_public_auth_base_url() {
  if [[ -n "$ECO_PUBLIC_AUTH_URL" ]]; then
    printf '%s' "$ECO_PUBLIC_AUTH_URL"
    return 0
  fi

  local explicit
  explicit="$(parse_expose_value "auth_url" || true)"
  if [[ -n "$explicit" ]]; then
    printf '%s' "$explicit"
    return 0
  fi

  local auth_hostname
  auth_hostname="$(parse_expose_value "auth_hostname" || true)"
  if [[ -n "$auth_hostname" ]]; then
    printf '%s://%s/api' "$(resolve_public_scheme)" "$auth_hostname"
    return 0
  fi

  local app_origin
  app_origin="$(resolve_public_app_origin || true)"
  if [[ -n "$app_origin" ]]; then
    printf '%s/auth-api' "$app_origin"
    return 0
  fi

  return 1
}

gateway_enabled() {
  if ! is_prod_mode; then
    return 1
  fi

  local enabled
  enabled="$(parse_expose_value "enabled" || true)"
  is_truthy "$(printf '%s' "$enabled" | tr '[:upper:]' '[:lower:]')"
}

resolve_gateway_port() {
  # The gateway is a registry-managed resource like any service: on a
  # redeploy the same port comes back from the registry so the tunnel origin
  # (which up.js resolves after configure.sh from the PM2 config) stays
  # stable. Explicit expose.gateway_port / expose.target_port pin the first
  # allocation; afterwards the registry owns it and the manifest override is
  # only honored while no registry row exists (a later override cannot
  # silently move a live gateway).
  local existing
  existing="$(registry_lookup "gateway" "gateway")"
  if [[ "$existing" =~ ^[0-9]+$ ]]; then
    ECO_GATEWAY_PORT="$existing"
    printf '%s' "$existing"
    return
  fi

  local configured_port
  configured_port="$(parse_expose_value "gateway_port" || true)"
  if [[ "$configured_port" =~ ^[0-9]+$ ]]; then
    registry_pin "gateway" "gateway" "PORT" "$configured_port" >/dev/null
    ECO_GATEWAY_PORT="$configured_port"
    printf '%s' "$configured_port"
    return
  fi
  configured_port="$(parse_expose_value "target_port" || true)"
  if [[ "$configured_port" =~ ^[0-9]+$ ]]; then
    registry_pin "gateway" "gateway" "PORT" "$configured_port" >/dev/null
    ECO_GATEWAY_PORT="$configured_port"
    printf '%s' "$configured_port"
    return
  fi
  # Adopt the port recorded from a previous run so the gateway stays stable
  # across the migration. On a genuinely first prod deployment (no state and
  # no registry row) pick a random free port so two projects on the same CT
  # never share port 8080.
  if [[ -n "${ECO_GATEWAY_PORT:-}" && "${ECO_GATEWAY_PORT}" =~ ^[0-9]+$ ]]; then
    registry_seed "gateway" "gateway" "PORT" "$ECO_GATEWAY_PORT" >/dev/null
    printf '%s' "$ECO_GATEWAY_PORT"
    return
  fi
  # Recovery: a running gateway's live PM2 env knows the exact port the
  # tunnel origin depends on. Harvest it before allocating anything new so
  # a deleted registry never moves a live gateway (which would break the
  # exposed hostname).
  local live_gateway=""
  live_gateway="$(lookup_live_service_port "gateway" || true)"
  if [[ "$live_gateway" =~ ^[0-9]+$ ]]; then
    registry_seed "gateway" "gateway" "PORT" "$live_gateway" >/dev/null
    printf '%s' "$live_gateway"
    return
  fi
  if is_prod_mode; then
    local random_port
    random_port="$(registry_get_or_allocate "gateway" "gateway" "PORT")"
    ECO_GATEWAY_PORT="$random_port"
    printf '%s' "$random_port"
    return
  fi
  printf '8080'
}

lookup_service_port() {
  local target_name="$1"
  local idx
  for idx in "${!svc_name[@]}"; do
    if [[ "${svc_name[$idx]}" == "$target_name" ]]; then
      printf '%s' "${svc_port[$idx]}"
      return 0
    fi
  done
  # Fall back to an installed LXS service: eco installs each `lxs:` service
  # into <project>/<service-name>/ with a generated .env carrying its PORT
  # (source-discovered services are scanned into svc_name; LXS services are
  # not, so the gateway/configure steps must read the port from their .env).
  local lxs_env=""
  if [[ -n "$PM2_DIR" ]]; then
    lxs_env="$PM2_DIR/$target_name/.env"
  elif [[ -n "$PROJECT_DIR" ]]; then
    lxs_env="$PROJECT_DIR/$target_name/.env"
  fi
  if [[ -n "$lxs_env" && -f "$lxs_env" ]]; then
    local port
    port="$(get_env "$lxs_env" "PORT")"
    if [[ -z "$port" ]]; then
      port="$(get_env "$lxs_env" "SERVER_PORT")"
    fi
    if [[ "$port" =~ ^[0-9]+$ ]]; then
      printf '%s' "$port"
      return 0
    fi
  fi
  return 1
}

# Given a domain name (e.g. `registry`), prints the manifest LXS service that
# provides it (e.g. `lxs-registry`), or nothing. Used by the gateway to route
# a domain's configured paths to its installed LXS backend.
lxs_service_name_for_domain() {
  local want_domain="$1" lxs_manifest="" svc domain
  lxs_manifest="$(resolve_manifest_path || true)"
  [[ -n "$lxs_manifest" && -f "$lxs_manifest" ]] || return 1
  while IFS='|' read -r svc domain; do
    [[ -n "$svc" && "$domain" == "$want_domain" ]] && { printf '%s' "$svc"; return 0; }
  done < <(parse_manifest_lxs_services "$lxs_manifest")
  return 1
}

resolve_gateway_frontend_service() {
  local preferred
  preferred="$(parse_expose_value "service" || true)"
  if [[ -n "$preferred" && "$preferred" != "gateway" ]] && lookup_service_port "$preferred" >/dev/null 2>&1; then
    printf '%s' "$preferred"
    return 0
  fi

  local idx
  for idx in "${!svc_name[@]}"; do
    case "${svc_type[$idx]}" in
      nextjs|vite|astro|nuxt|static)
        printf '%s' "${svc_name[$idx]}"
        return 0
        ;;
    esac
  done

  return 1
}

resolve_gateway_api_service() {
  local preferred="${PROJECT_NAME}-backend"
  if lookup_service_port "$preferred" >/dev/null 2>&1; then
    printf '%s' "$preferred"
    return 0
  fi

  if lookup_service_port "backend" >/dev/null 2>&1; then
    printf '%s' "backend"
    return 0
  fi

  local idx
  for idx in "${!svc_name[@]}"; do
    if [[ "${svc_type[$idx]}" == "spring-boot" && "${svc_name[$idx]}" != "auth-backend" && "${svc_name[$idx]}" != *-auth-backend ]]; then
      printf '%s' "${svc_name[$idx]}"
      return 0
    fi
  done

  return 1
}

# Path prefixes each of eco's split-out domains owns, used to build
# per-domain gateway routing ahead of the generic single-backend fallback
# below. Parallel arrays, not an associative array -- this script targets
# bash 3.2 (macOS's shipped bash), which has no `declare -A`.
#
# Mirrors rwid/lms/frontend's client.ts `inferService()` exactly. If that
# function's routing table changes, this should too -- these are the two
# places (frontend dev-mode routing, gateway prod-mode routing) that need
# to agree on which backend owns which path.
#
# The `-files` entries (e.g. /api/community-files/*) exist because several
# domains each implement their own /files/upload + /files/view/{id}; under
# one shared gateway origin that's ambiguous by path alone, so each domain
# also exposes its file routes under its own prefix (see each backend's
# routes.rs and storage.rs for the matching change). `courses` doesn't
# need a separate entry since its file routes already live under
# /api/courses/*. `/api/storage/*` is the reusable S3/MinIO storage domain
# implemented by the photos repository; its neutral route lets every estate
# use it without exposing repository naming in the public API.
domain_gateway_prefix=(profile courses payments community content site ecobook storage registry email-manager)
domain_gateway_paths=(
  "/api/users/* /api/schools/* /api/interests/* /api/skills/*"
  "/api/courses/*"
  "/api/payments/*"
  "/api/communities/* /api/events/* /api/notifications/* /api/community-files/*"
  "/api/posts/* /api/comments/* /api/likes/* /api/success-stories/* /api/content-files/*"
  "/api/app-config /api/app-config/* /api/homepage-layout/* /api/analytics/* /api/leads/* /api/platforms/* /api/setup/* /api/site-files/*"
  "/api/book/* /api/author-assets/* /api/author-poll-templates/* /api/slides-files/*"
  "/api/storage/*"
  "/api/lxs /api/lxs/*"
  "/api/email /api/email/*"
)

generate_gateway_config() {
  if ! gateway_enabled; then
    return 1
  fi

  local frontend_service auth_service="auth-backend"
  frontend_service="$(resolve_gateway_frontend_service || true)"
  if [[ -z "$frontend_service" ]]; then
    echo -e "  ${YELLOW}⚠${RESET} gateway — missing frontend target, skipping"
    return 1
  fi

  # Discover which of the split-out domains are actually composed into
  # this estate. An estate with none of them (e.g. one still running a
  # single monolith backend) falls through entirely to the generic
  # api_service block, unchanged from before.
  local -a domain_blocks=() routed_backend_names=()
  declare -A emitted_prefixes=()
  local i prefix backend_name backend_port paths
  for i in "${!domain_gateway_prefix[@]}"; do
    prefix="${domain_gateway_prefix[$i]}"
    backend_name="${prefix}-backend"
    backend_port="$(lookup_service_port "$backend_name" || true)"
    if [[ -z "$backend_port" ]]; then
      # The domain may be composed as an installed LXS whose service name is
      # not `<domain>-backend` (e.g. `lxs-registry` for the registry domain).
      # Resolve its port through the LXS service lookup before skipping.
      local lxs_svc_for_domain=""
      lxs_svc_for_domain="$(lxs_service_name_for_domain "$prefix" || true)"
      if [[ -n "$lxs_svc_for_domain" ]]; then
        backend_port="$(lookup_service_port "$lxs_svc_for_domain" || true)"
      fi
    fi
    [[ -z "$backend_port" ]] && continue
    paths="${domain_gateway_paths[$i]}"
    # Caddyfile's `handle` directive takes exactly one path argument --
    # several domains' path lists above have more than one entry (e.g.
    # profile's /api/users/* /api/schools/* /api/interests/* /api/skills/*),
    # which as a single `handle <paths...> {` line is invalid syntax (Caddy
    # errors on the second path token as an unexpected extra argument).
    # Use a named matcher instead, which does accept multiple space-
    # separated path patterns as OR'd alternatives.
    domain_blocks+=("$(printf '\t@gw_%s path %s\n\thandle @gw_%s {\n\t\treverse_proxy 127.0.0.1:%s\n\t}\n' "$prefix" "$paths" "$prefix" "$backend_port")")
    routed_backend_names+=("$backend_name")
    emitted_prefixes[$prefix]=1
  done

  # Every other split domain follows Eco's conventional public route
  # /api/<domain>/*. This prevents browser bundles from needing localhost or
  # one public hostname per backend. The explicit path table above retains
  # priority for legacy domains whose routes are not namespaced this way.
  local idx service_name already_routed
  for idx in "${!svc_name[@]}"; do
    service_name="${svc_name[$idx]}"
    [[ "$service_name" == *-backend ]] || continue
    [[ "$service_name" == "auth-backend" || "$service_name" == *-auth-backend ]] && continue
    already_routed=0
    for backend_name in "${routed_backend_names[@]}"; do
      [[ "$service_name" == "$backend_name" ]] && { already_routed=1; break; }
    done
    [[ "$already_routed" -eq 1 ]] && continue
    prefix="${service_name%-backend}"
    [[ "$prefix" == "$PROJECT_NAME" ]] && continue
    # A domain can be composed both as a cloned source repo (whose backend is
    # scanned into svc_name) and as an installed LXS (also named <domain>-backend).
    # Emit one @gw_<prefix> matcher per prefix regardless -- Caddy rejects a
    # matcher defined more than once, which would otherwise take the whole
    # gateway down whenever an estate declares both forms.
    [[ -n "${emitted_prefixes[$prefix]:-}" ]] && continue
    emitted_prefixes[$prefix]=1
    backend_port="${svc_port[$idx]}"
    # Match the bare /api/<domain> path as well as /api/<domain>/*: some
    # backends expose a resource directly at their API base (e.g. the
    # notifications list at /api/notifications), and without the bare match
    # that exact path falls through to the frontend.
    domain_blocks+=("$(printf '\t@gw_%s path /api/%s /api/%s/*\n\thandle @gw_%s {\n\t\treverse_proxy 127.0.0.1:%s\n\t}\n' "$prefix" "$prefix" "$prefix" "$prefix" "$backend_port")")
  done

  # Installed LXS services are not source-discovered, so they never land in
  # svc_name and the loop above cannot route them. Emit the conventional
  # /api/<domain>/ routes for each manifest `lxs:` service using the port from
  # its installed .env. The static domain_gateway_prefix table above already
  # emits precise path sets for domains that need them (e.g. registry ->
  # /api/lxs /api/lxs/*); skip a domain here if that table already routed it.
  local lxs_manifest=""
  lxs_manifest="$(resolve_manifest_path || true)"
  if [[ -n "$lxs_manifest" && -f "$lxs_manifest" ]]; then
    local lxs_svc lxs_domain lxs_port
    while IFS='|' read -r lxs_svc lxs_domain; do
      [[ -z "$lxs_svc" || -z "$lxs_domain" ]] && continue
      [[ "$lxs_domain" == "$PROJECT_NAME" ]] && continue
      lxs_port="$(lookup_service_port "$lxs_svc" || true)"
      [[ -z "$lxs_port" ]] && continue
      [[ -n "${emitted_prefixes[$lxs_domain]:-}" ]] && continue
      emitted_prefixes[$lxs_domain]=1
      domain_blocks+=("$(printf '\t@gw_%s path /api/%s /api/%s/*\n\thandle @gw_%s {\n\t\treverse_proxy 127.0.0.1:%s\n\t}\n' "$lxs_domain" "$lxs_domain" "$lxs_domain" "$lxs_domain" "$lxs_port")")
    done < <(parse_manifest_lxs_services "$lxs_manifest")
  fi

  local api_service api_port
  api_service="$(resolve_gateway_api_service || true)"
  if [[ -n "$api_service" ]]; then
    api_port="$(lookup_service_port "$api_service" || true)"
  fi

  if [[ ${#domain_blocks[@]} -eq 0 && -z "$api_port" ]]; then
    echo -e "  ${YELLOW}⚠${RESET} gateway — no domain backends or generic api target found, skipping"
    return 1
  fi

  local frontend_port auth_port gateway_port caddyfile
  frontend_port="$(lookup_service_port "$frontend_service" || true)"
  auth_port="$(lookup_service_port "$auth_service" || true)"
  gateway_port="$(resolve_gateway_port)"
  # If resolve_gateway_port picked a new random port, persist it so save_state
  # can write it to .configure-state. The function runs in a subshell so its
  # own ECO_GATEWAY_PORT assignment does not reach the parent; re-assign here.
  if [[ -z "${ECO_GATEWAY_PORT:-}" || ! "${ECO_GATEWAY_PORT}" =~ ^[0-9]+$ ]]; then
    ECO_GATEWAY_PORT="$gateway_port"
  fi
  caddyfile="$PM2_DIR/Caddyfile"

  if [[ -z "$frontend_port" || -z "$auth_port" ]]; then
    # In single-binary mode, all Rust services are merged into one binary.
    # Route all /api/* traffic to the single binary port instead of
    # per-service ports. The unified binary serves every domain route.
    local single_binary_port=""
    single_binary_port="$(lookup_service_port "${PROJECT_NAME}-binary" || true)"
    if [[ -n "$single_binary_port" ]]; then
      auth_port="$single_binary_port"
      domain_blocks_text=""
      caddyfile="$PM2_DIR/Caddyfile"
      {
        echo "{"
        echo "	admin off"
        echo "}"
        echo ""
        echo ":${gateway_port} {"
        echo "	@plain_http header X-Forwarded-Proto http"
        echo "	redir @plain_http https://{host}{uri} 302"
        echo ""
        echo "	# All API traffic → single binary"
        echo "	handle /api/* {"
        echo "		reverse_proxy 127.0.0.1:${single_binary_port}"
        echo "	}"
        echo ""
        echo "	handle {"
        echo "		reverse_proxy 127.0.0.1:${frontend_port}"
        echo "	}"
        echo "}"
      } > "$caddyfile"
      echo -e "  ${GREEN}✓${RESET} gateway — single-binary on :${gateway_port} → :${single_binary_port} (api) + :${frontend_port} (frontend)"
    else
      echo -e "  ${YELLOW}⚠${RESET} gateway — unresolved frontend/auth ports, skipping"
    fi
    return 1
  fi

  local domain_blocks_text=""
  if [[ ${#domain_blocks[@]} -gt 0 ]]; then
    domain_blocks_text="$(printf '%s\n' "${domain_blocks[@]}")"
  fi

  local generic_api_block=""
  if [[ -n "$api_port" ]]; then
    generic_api_block="$(printf '\thandle /api/* {\n\t\treverse_proxy 127.0.0.1:%s\n\t}\n' "$api_port")"
  fi

  cat > "$caddyfile" <<EOF
{
	admin off
}

:${gateway_port} {
	# Cloudflare forwards the original scheme to the tunnel origin.  Keep the
	# public URL canonical and prevent browsers from treating HTTP visits as an
	# insecure variant of an otherwise HTTPS-enabled estate. A temporary 302 is
	# used deliberately: a 308 permanent redirect pointing http->https at the
	# same URL gets cached by browsers, so if the tunnel ever forwards
	# X-Forwarded-Proto: http for an https request the browser follows a cached
	# self-redirect forever (ERR_TOO_MANY_REDIRECTS on every asset). A 302 is
	# never cached, so the loop cannot persist beyond the transient condition.
	@plain_http header X-Forwarded-Proto http
	redir @plain_http https://{host}{uri} 302

	handle /auth-api/* {
		uri replace /auth-api /api
		reverse_proxy 127.0.0.1:${auth_port}
	}

	handle /api/auth/* {
		reverse_proxy 127.0.0.1:${auth_port}
	}

${domain_blocks_text}
${generic_api_block}
	handle {
		reverse_proxy 127.0.0.1:${frontend_port}
	}
}
EOF

  GATEWAY_PORT="$gateway_port"
  GATEWAY_FILE="$caddyfile"
  echo -e "  ${GREEN}✓${RESET} gateway"
  return 0
}

# ─── Info mode: display existing ecosystem.config.js ───────────────────────

show_info() {
  # Find the PM2 config (either extension -- see find_pm2_config) in the
  # current directory or subdirectories.
  local config_path=""
  config_path="$(find_pm2_config ".")"
  if [[ -z "$config_path" ]]; then
    config_path="$(find_pm2_config "..")"
  fi
  if [[ -z "$config_path" ]]; then
    # Search in PROJECT_ROOT subdirectories
    for dir in "$PROJECT_ROOT"/*/; do
      if config_path="$(find_pm2_config "${dir%/}")"; then
        break
      fi
    done
  fi

  if [[ -z "$config_path" ]]; then
    echo -e "${RED}No ecosystem.config.js/.cjs found.${RESET}"
    echo "Run configure.sh without arguments to create one."
    exit 1
  fi

  local config_dir="$(cd "$(dirname "$config_path")" && pwd)"
  local project_name="${PROJECT_NAME:-$(basename "$config_dir")}"
  
  # Try to load project name from state file
  if [[ -f "$config_dir/.configure-state" ]]; then
    source "$config_dir/.configure-state"
  fi
  project_name="${PROJECT_NAME:-$project_name}"

  echo ""
  echo -e "${BOLD}========================================${RESET}"
  echo -e "${BOLD}  Project Services${RESET}"
  echo -e "${BOLD}========================================${RESET}"
  echo ""
  echo -e "  ${CYAN}Project${RESET}        ${project_name}"
  echo -e "  ${CYAN}Config${RESET}         ${config_path}"
  echo ""

  # Parse ecosystem.config.js using Node.js
  node -e "
    const config = require('${config_path}');
    const apps = (config.apps || []).slice().sort((a, b) => {
      const aEnv = a.env || {};
      const bEnv = b.env || {};
      const aPort = Number(aEnv.PORT || aEnv.SERVER_PORT);
      const bPort = Number(bEnv.PORT || bEnv.SERVER_PORT);
      const aHasPort = Number.isFinite(aPort);
      const bHasPort = Number.isFinite(bPort);
      if (aHasPort && bHasPort) return aPort - bPort;
      if (aHasPort) return -1;
      if (bHasPort) return 1;
      return (a.name || '').localeCompare(b.name || '');
    });
    
    apps.forEach((app, index) => {
      const name = app.name.replace('${project_name}-', '');
      const env = app.env || {};
      const port = env.PORT || env.SERVER_PORT || 'N/A';
      const cwd = app.cwd || '';
      const script = app.script || '';
      const args = app.args || '';
      
      // Determine runtime type and URL format
      let url = \`http://localhost:\${port}\`;
      let runtime = 'unknown';
      
      const fs = require('fs');
      const path = require('path');
      if (fs.existsSync(path.join(cwd, 'pom.xml'))) {
        runtime = 'java';
        url += '/api';
      } else if (fs.existsSync(path.join(cwd, 'go.mod'))) {
        runtime = 'go';
      } else if (fs.existsSync(path.join(cwd, 'Cargo.toml'))) {
        runtime = 'rust';
      } else if (fs.existsSync(path.join(cwd, 'package.json'))) {
        const packageJson = JSON.parse(fs.readFileSync(path.join(cwd, 'package.json'), 'utf8'));
        const scripts = packageJson.scripts || {};
        const deps = { ...(packageJson.dependencies || {}), ...(packageJson.devDependencies || {}) };
        if (script.includes('next') || fs.existsSync(path.join(cwd, '.next'))) {
          runtime = 'node/next';
        } else if (deps.astro || String(scripts.dev || '').includes('astro')) {
          runtime = 'node/astro';
        } else if (deps.vite || String(scripts.dev || '').includes('vite')) {
          runtime = 'node/vite';
        } else {
          runtime = 'node';
        }
      }

      let projectType = runtime;
      if (runtime === 'java') {
        projectType = 'spring-boot';
      }

      const envFile = fs.existsSync(path.join(cwd, '.env'))
        ? path.join(cwd, '.env')
        : 'N/A';
      const command = [script, args].filter(Boolean).join(' ');
      let database = null;

      if (envFile !== 'N/A') {
        try {
          const envContents = fs.readFileSync(envFile, 'utf8');
          const mongoMatch = envContents.match(/^MONGODB_URI=(.+)$/m);
          const sqlMatch = envContents.match(/^(DATABASE_URL|DB_URL)=(.+)$/m);
          if (mongoMatch) {
            database = { type: 'MongoDB', value: mongoMatch[1].trim() };
          } else if (sqlMatch) {
            database = { type: 'SQL', value: sqlMatch[2].trim() };
          }
        } catch (_) {}
      }

      const rows = [
        ['Type', projectType],
        ['URL', url],
        ['Port', String(port)],
        ['Dir', cwd || 'N/A'],
        ['Env', envFile]
      ];
      if (database) {
        rows.push(['DB', \`\${database.type}  \${database.value}\`]);
      }
      if (command) {
        rows.push(['Command', command]);
      }
      const serviceLabel = \`Service #\${index + 1}\`;
      const labelWidth = Math.max(serviceLabel.length, ...rows.map(([label]) => label.length));
      const valueWidth = Math.max(name.length, ...rows.map(([, value]) => String(value).length));
      const topBorder = \`  ┌─\${'─'.repeat(labelWidth)}─┬─\${'─'.repeat(valueWidth)}─┐\`;
      const midBorder = \`  ├─\${'─'.repeat(labelWidth)}─┼─\${'─'.repeat(valueWidth)}─┤\`;
      const bottomBorder = \`  └─\${'─'.repeat(labelWidth)}─┴─\${'─'.repeat(valueWidth)}─┘\`;

      console.log(topBorder);
      console.log(\`  │ \${serviceLabel.padEnd(labelWidth)} │ \${name.padEnd(valueWidth)} │\`);
      console.log(midBorder);
      rows.forEach(([label, value]) => {
        console.log(\`  │ \${label.padEnd(labelWidth)} │ \${String(value).padEnd(valueWidth)} │\`);
      });
      console.log(bottomBorder);
      console.log('');
    });
  " 2>/dev/null || {
    # Fallback: simple grep-based parsing
    echo -e "${YELLOW}Note: Install Node.js for richer service details${RESET}"
    echo ""
    grep -A 3 "name:" "$config_path" | grep -E "(name:|PORT:|SERVER_PORT:)" | \
      sed 's/name: "//g; s/",//g; s/env: { PORT: //g; s/env: { SERVER_PORT: //g; s/ }//g' | \
      paste -d' ' - - | sort -k2,2n | awk -v project_name="$project_name" '
        {
          name=$1;
          port=$2;
          sub("^" project_name "-", "", name);
          print NR "|" name "|" port;
        }
      ' | while IFS='|' read -r idx name port; do
        local url="http://localhost:${port}"
        local service_label="Service #${idx}"
        local label_width=${#service_label}
        local value_width=${#name}
        [[ ${#url} -gt $value_width ]] && value_width=${#url}
        [[ ${#port} -gt $value_width ]] && value_width=${#port}
        local value_rule
        value_rule="$(printf '%*s' "$value_width" '' | tr ' ' '─')"
        local label_rule
        label_rule="$(printf '%*s' "$label_width" '' | tr ' ' '─')"
        local top_border="  ┌─${label_rule}─┬─${value_rule}─┐"
        local mid_border="  ├─${label_rule}─┼─${value_rule}─┤"
        local bottom_border="  └─${label_rule}─┴─${value_rule}─┘"
        echo "$top_border"
        printf "  │ %-*s │ %-*s │\n" "$label_width" "$service_label" "$value_width" "$name"
        echo "$mid_border"
        printf "  │ %-*s │ %-*s │\n" "$label_width" "URL" "$value_width" "$url"
        printf "  │ %-*s │ %-*s │\n" "$label_width" "Port" "$value_width" "$port"
        echo "$bottom_border"
        echo ""
      done
  }

  echo ""
  echo -e "  ${GREEN}Run: pm2 start ${config_path}${RESET}"
  echo ""
}

# ─── Helpers ────────────────────────────────────────────────────────────────

check_env_file() {
  if [[ ! -f "$1" ]]; then
    local example="${1}.example"
    if [[ -f "$example" ]]; then
      cp "$example" "$1"
      echo -e "  ${YELLOW}⚠${RESET}  No .env found — copied from .env.example: $1"
    else
      touch "$1"
      echo -e "  ${YELLOW}⚠${RESET}  No .env found — created blank file: $1"
    fi
  fi
}

# check_env_file only handles "no .env exists yet" (fresh copy from
# .env.example). It does nothing for the much more common case: .env
# already exists from an earlier configure, and .env.example has since
# grown a new key (e.g. a new peer dependency's *_BASE_URL, or a new
# NEXT_PUBLIC_*_API_URL for a frontend composing another domain) --
# that key would otherwise never reach the real .env, silently leaving
# it unset forever, since every *_BASE_URL/*_API_URL resolver below only
# ever updates keys that already exist in .env, never adds new ones.
# Runs on every configure, safe to call repeatedly: only ever appends a
# key that's completely absent from .env (matched on key name only, via
# grep, regardless of current value), never touches a key that already
# has any value -- including one deliberately left empty pending
# resolution below.
sync_env_from_example() {
  local env_file="$1"
  local example="${env_file}.example"
  # Not every service has a .env.example (e.g. the Java monolith doesn't)
  # -- that's a normal, valid state, not an error. A bare `return` here
  # would inherit the failed [[ ]] test's exit status (1), which under
  # this script's `set -e` would abort the entire configure run over a
  # missing-but-optional file.
  if [[ ! -f "$example" || ! -f "$env_file" ]]; then
    return 0
  fi

  local line key value
  while IFS= read -r line; do
    [[ -z "$line" || "$line" =~ ^[[:space:]]*# ]] && continue
    [[ "$line" == *=* ]] || continue
    key="${line%%=*}"
    [[ -z "$key" ]] && continue
    if ! grep -qE "^${key}=" "$env_file"; then
      value="${line#*=}"
      echo "${key}=${value}" >> "$env_file"
      echo -e "  ${YELLOW}⚠${RESET}  ${env_file} was missing ${key} (present in .env.example) — added"
    fi
  done < "$example"
}

get_env() {
  grep -E "^${2}=" "$1" 2>/dev/null | cut -d'=' -f2- | tr -d '\r'
}

# Shell-held secrets (e.g. DEEPSEEK_API_KEY exported in ~/.bashrc on the CT)
# are propagated into any service .env that declares the key in its
# .env.example -- the same "operator pins the secret once, eco never generates
# it" convention as DATABASE_PASSWORD. Idempotent: a non-empty shell value
# simply replaces the .env line; an empty/unset shell value leaves the .env
# untouched (so the operator can deploy first and add the key later).
fill_shell_env_secret() {
  local env_file="$1"
  local example="${env_file}.example"
  local key="$2"
  if [[ ! -f "$example" || ! -f "$env_file" ]]; then
    return 0
  fi
  if ! grep -qE "^${key}=" "$example"; then
    return 0
  fi
  local value="${!key:-}"
  if [[ -z "$value" ]]; then
    return 0
  fi
  if grep -qE "^${key}=" "$env_file"; then
    sed -i "s|^${key}=.*|${key}=${value}|" "$env_file"
  else
    echo "${key}=${value}" >> "$env_file"
  fi
  echo -e "  ${YELLOW}✓${RESET}  ${env_file} ${key} filled from shell environment"
}

set_env() {
  local file="$1" key="$2" value="$3"
  if grep -qE "^${key}=" "$file"; then
    sed -i.bak "s|^${key}=.*|${key}=${value}|" "$file" && rm -f "${file}.bak"
  else
    echo "${key}=${value}" >> "$file"
  fi
}

set_env_if_missing() {
  local file="$1" key="$2" value="$3"
  if grep -qE "^${key}=" "$file"; then
    return 0
  fi
  echo "${key}=${value}" >> "$file"
}

generate_shared_jwt_secret() {
  if command -v openssl >/dev/null 2>&1; then
    openssl rand -base64 48 | tr -d '\n'
    return
  fi

  if command -v python3 >/dev/null 2>&1; then
    python3 - <<'PY'
import secrets
print(secrets.token_urlsafe(48))
PY
    return
  fi

  date +%s | sha256sum | cut -d' ' -f1
}

resolve_shared_jwt_secret() {
  local secret=""

  for env_file in "${svc_env[@]}"; do
    if [[ -n "$env_file" && -f "$env_file" ]]; then
      secret="$(get_env "$env_file" "JWT_SECRET")"
      if [[ -n "$secret" ]]; then
        printf '%s' "$secret"
        return
      fi
    fi
  done

  secret="$(get_env "$PM2_DIR/.env" "JWT_SECRET")"
  if [[ -n "$secret" ]]; then
    printf '%s' "$secret"
    return
  fi

  generate_shared_jwt_secret
}

# Inherit mail credentials (BREVO_API_KEY, MAIL_FROM_EMAIL, MAIL_FROM_NAME)
# from the host environment into a service's generated .env when they are not
# already set there. Used by auth-backend (email verification) and
# email-manager-backend (transactional sending). Never blanks a value that a
# previous run already wrote -- host env is a fallback, not an overwrite.
inherit_mail_credentials() {
  local env_file="$1"
  [[ -n "$env_file" && -f "$env_file" ]] || return 0
  if [[ -z "$(get_env "$env_file" "BREVO_API_KEY")" && -n "${BREVO_API_KEY:-}" ]]; then
    set_env "$env_file" "BREVO_API_KEY" "$BREVO_API_KEY"
  fi
  if [[ -z "$(get_env "$env_file" "MAIL_FROM_EMAIL")" && -n "${MAIL_FROM_EMAIL:-}" ]]; then
    set_env "$env_file" "MAIL_FROM_EMAIL" "$MAIL_FROM_EMAIL"
  fi
  if [[ -z "$(get_env "$env_file" "MAIL_FROM_NAME")" && -n "${MAIL_FROM_NAME:-}" ]]; then
    set_env "$env_file" "MAIL_FROM_NAME" "$MAIL_FROM_NAME"
  fi
}

join_by() {
  local delimiter="$1"
  shift
  local first=1
  local value
  for value in "$@"; do
    if [[ $first -eq 1 ]]; then
      printf '%s' "$value"
      first=0
    else
      printf '%s%s' "$delimiter" "$value"
    fi
  done
}

lookup_backend_port() {
  local target_name="$1"
  local idx
  for idx in "${!backend_names[@]}"; do
    if [[ "${backend_names[$idx]}" == "$target_name" ]]; then
      printf '%s' "${backend_port_values[$idx]}"
      return
    fi
  done
}

# A backend may expose its application routes under something other than the
# conventional /api (for example, an existing Actix service mounted at
# /api/v1). Keep that contract in the backend's tracked .env.example; Eco
# then writes every generated peer/frontend URL from the same declaration.
api_base_path_for_backend() {
  local target_name="$1"
  local idx env_file api_path
  for idx in "${!svc_name[@]}"; do
    [[ "${svc_name[$idx]}" == "$target_name" ]] || continue
    env_file="${svc_env[$idx]}"
    api_path="$(get_env "$env_file" "API_BASE_PATH")"
    [[ -z "$api_path" ]] && api_path="$(get_env "${env_file}.example" "API_BASE_PATH")"
    if [[ "$api_path" =~ ^/[[:alnum:]./_-]*$ ]]; then
      printf '%s' "${api_path%/}"
      return
    fi
    break
  done
  printf '%s' "/api"
}

# Browser-facing domain APIs need an unambiguous path when they share the
# primary public gateway. Conventional split domains own /api/<domain>/*;
# a monolith (<project>-backend or plain backend) remains at /api. A domain
# with an explicit non-/api API_BASE_PATH keeps that declared contract.
browser_api_path_for_backend() {
  local target_name="$1"
  local api_path
  api_path="$(api_base_path_for_backend "$target_name")"
  if [[ "$api_path" != "/api" ]]; then
    printf '%s' "$api_path"
    return
  fi

  local prefix="${target_name%-backend}"
  # Legacy Profile exposes several shared resource paths (/api/users,
  # /api/skills, etc.), so its browser base is the gateway's /api rather
  # than an invented /api/profile path.
  if [[ "$prefix" == "profile" ]]; then
    printf '%s' "/api"
    return
  fi
  if [[ "$target_name" == "backend" || "$prefix" == "$PROJECT_NAME" ]]; then
    printf '%s' "/api"
    return
  fi
  printf '/api/%s' "$prefix"
}

# Maps a peer-dependency prefix to the backend that fulfills it. The default
# convention is `<prefix>-backend` (PROFILE -> profile-backend). The STORAGE
# role is eco's canonical object-storage domain -- the Photos repository, see
# the /api/storage/* gateway route and its comment -- so STORAGE resolves to
# photos-backend when that domain is composed, regardless of how the consuming
# domain names the provider.
peer_target_backend_for_prefix() {
  local prefix="$1"
  if [[ "$prefix" == "STORAGE" ]]; then
    if [[ -n "$(lookup_backend_port "storage-backend")" ]]; then
      printf '%s' "storage-backend"
      return
    fi
  fi
  printf '%s' "$(printf '%s' "$prefix" | tr '[:upper:]_' '[:lower:]-')-backend"
}

# Resolves declared peer-domain dependencies for a backend service. Any
# `<PREFIX>_BASE_URL=` or `<PREFIX>_API_URL=` line already present in the
# service's own .env (other than the already-special-cased API_BASE_URL and
# AUTH_BASE_URL) is treated as a declared dependency on a sibling backend:
# `PROFILE_BASE_URL=` in content-backend/.env resolves against a discovered
# `profile-backend` service, and `STORAGE_API_URL=` (used by the chat domain)
# resolves against the Photos/Storage domain. Backend-to-backend URLs always
# stay internal (localhost), in dev and prod alike. Silently skipped if no
# matching service is discovered (e.g. that domain hasn't been composed into
# this estate).
resolve_peer_base_urls() {
  local env_file="$1" self_name="$2"
  [[ -f "$env_file" ]] || return

  local key suffix prefix
  while IFS= read -r key; do
    [[ -z "$key" ]] && continue
    [[ "$key" == "API_BASE_URL" || "$key" == "AUTH_BASE_URL" ]] && continue

    if [[ "$key" == *_BASE_URL ]]; then
      suffix="_BASE_URL"
    elif [[ "$key" == *_API_URL ]]; then
      suffix="_API_URL"
    else
      continue
    fi
    prefix="${key%$suffix}"
    local target_name
    target_name="$(peer_target_backend_for_prefix "$prefix")"
    [[ "$target_name" == "$self_name" ]] && continue

    local target_port
    target_port="$(lookup_backend_port "$target_name")"
    if [[ -n "$target_port" ]]; then
      set_env "$env_file" "$key" "http://localhost:${target_port}$(api_base_path_for_backend "$target_name")"
    else
      echo -e "  ${YELLOW}⚠${RESET}  $self_name wants ${key} but no ${target_name} service was discovered — leaving unset"
    fi
  done < <(grep -oE '^[A-Z][A-Z0-9_]*(_BASE_URL|_API_URL)=' "$env_file" | sed 's/=$//' | sort -u)
}

# Resolves browser-reachable peer URLs for backend services. This is distinct
# from *_BASE_URL: a backend calls peers over localhost, while values such as
# STORAGE_PUBLIC_URL are persisted in user data and must point through the
# estate gateway. Only explicitly declared keys are touched.
resolve_peer_public_urls() {
  local env_file="$1" self_name="$2"
  [[ -f "$env_file" ]] || return 0

  local key prefix target_name target_port public_origin gateway_port
  while IFS= read -r key; do
    [[ -z "$key" ]] && continue
    prefix="${key%_PUBLIC_URL}"
    target_name="$(peer_target_backend_for_prefix "$prefix")"
    [[ "$target_name" == "$self_name" ]] && continue
    target_port="$(lookup_backend_port "$target_name")"
    if [[ -z "$target_port" ]]; then
      echo -e "  ${YELLOW}⚠${RESET}  $self_name wants ${key} but no ${target_name} service was discovered — leaving unset"
      continue
    fi
    if is_prod_mode; then
      public_origin="$(resolve_public_app_origin || true)"
      [[ -n "$public_origin" ]] && set_env "$env_file" "$key" "${public_origin}$(browser_api_path_for_backend "$target_name")"
    else
      gateway_port="$(resolve_gateway_port || true)"
      [[ -n "$gateway_port" ]] && set_env "$env_file" "$key" "http://localhost:${gateway_port}$(browser_api_path_for_backend "$target_name")"
    fi
  done < <(grep -oE '^[A-Z][A-Z0-9_]*_PUBLIC_URL=' "$env_file" | sed 's/=$//' | sort -u)
}

# Same idea as resolve_peer_base_urls, but for Next.js frontends composing
# multiple domain backends directly (browser-facing calls, so the keys need
# the NEXT_PUBLIC_ prefix and can't just reuse the backend-to-backend
# convention). Any `NEXT_PUBLIC_<PREFIX>_API_URL=` line already present in
# the frontend's own .env (other than the already-special-cased
# NEXT_PUBLIC_API_URL/NEXT_PUBLIC_AUTH_URL) is treated as a declared
# dependency on a sibling backend named `<prefix>-backend`, e.g.
# `NEXT_PUBLIC_COURSES_API_URL=` in lms-frontend/.env resolves against a
# discovered `courses-backend` service. Silently skipped if no matching
# service is discovered.
resolve_frontend_peer_api_urls() {
  local env_file="$1"
  local host="${2:-localhost}"

  local key
  while IFS= read -r key; do
    [[ -z "$key" ]] && continue
    [[ "$key" == "NEXT_PUBLIC_API_URL" || "$key" == "NEXT_PUBLIC_AUTH_URL" ]] && continue

    local prefix="${key#NEXT_PUBLIC_}"
    prefix="${prefix%_API_URL}"
    local target_name
    target_name="$(printf '%s' "$prefix" | tr '[:upper:]_' '[:lower:]-')-backend"

    local target_port
    target_port="$(lookup_backend_port "$target_name")"
    if [[ -n "$target_port" ]]; then
      set_env "$env_file" "$key" "http://${host}:${target_port}$(api_base_path_for_backend "$target_name")"
    else
      echo -e "  ${YELLOW}⚠${RESET}  frontend wants ${key} but no ${target_name} service was discovered — leaving unset"
    fi
  done < <(grep -oE '^NEXT_PUBLIC_[A-Z][A-Z0-9_]*_API_URL=' "$env_file" | sed 's/=$//' | sort -u)
}

# Browser-facing non-primary services can be exposed on their own hostname
# through expose.additional. A frontend opts in by declaring
# PUBLIC_<DOMAIN>_URL= in .env.example (for example PUBLIC_CHAT_URL=). Eco
# resolves it to localhost in development and to the matching additional
# hostname in production, so source code never carries a deployment URL.
additional_expose_hostname_for_service() {
  local target_service="$1" manifest_path="$2"
  [[ -n "$manifest_path" && -f "$manifest_path" ]] || return 0
  awk -v target="$target_service" '
    /^expose:[[:space:]]*$/ { in_expose=1; next }
    in_expose && /^[^[:space:]].*:[[:space:]]*$/ { exit }
    in_expose && /^  additional:[[:space:]]*$/ { in_additional=1; next }
    !in_additional { next }
    /^    -[[:space:]]+hostname:[[:space:]]*/ {
      hostname=$0; sub(/^    -[[:space:]]+hostname:[[:space:]]*/, "", hostname); gsub(/[[:space:]]+$/, "", hostname); gsub(/^"|"$|^\047|\047$/, "", hostname); service=""; next
    }
    /^      service:[[:space:]]*/ {
      service=$0; sub(/^      service:[[:space:]]*/, "", service); gsub(/[[:space:]]+$/, "", service); gsub(/^"|"$|^\047|\047$/, "", service)
      if (service == target && hostname != "") { print hostname; exit }
    }
  ' "$manifest_path"
}

resolve_vite_public_peer_urls() {
  local env_file="$1" dev_host="$2" manifest_path="$3"
  [[ -f "$env_file" ]] || return 0
  local key prefix target_name target_port hostname public_url
  while IFS= read -r key; do
    [[ -z "$key" || "$key" == "PUBLIC_API_URL" || "$key" == "PUBLIC_AUTH_URL" ]] && continue
    prefix="${key#PUBLIC_}"
    prefix="${prefix%_URL}"
    target_name="$(printf '%s' "$prefix" | tr '[:upper:]_' '[:lower:]-')-backend"
    target_port="$(lookup_backend_port "$target_name")"
    if [[ -z "$target_port" ]]; then
      echo -e "  ${YELLOW}⚠${RESET}  frontend wants ${key} but no ${target_name} service was discovered — leaving unset"
      continue
    fi
    if is_prod_mode; then
      hostname="$(additional_expose_hostname_for_service "$target_name" "$manifest_path")"
      if [[ -n "$hostname" ]]; then
        public_url="$(resolve_public_scheme)://${hostname}$(api_base_path_for_backend "$target_name")"
      else
        local app_origin
        app_origin="$(resolve_public_app_origin || true)"
        if [[ -z "$app_origin" ]]; then
          echo -e "  ${YELLOW}⚠${RESET}  ${target_name} has no public gateway for ${key} — leaving unset"
          continue
        fi
        public_url="${app_origin}$(browser_api_path_for_backend "$target_name")"
      fi
      set_env "$env_file" "$key" "$public_url"
    else
      set_env "$env_file" "$key" "http://${dev_host}:${target_port}$(api_base_path_for_backend "$target_name")"
    fi
  done < <(grep -oE '^PUBLIC_[A-Z][A-Z0-9_]*_URL=' "$env_file" | sed 's/=$//' | sort -u)
}

# Fills S3_ENDPOINT/S3_REGION/S3_ACCESS_KEY/S3_SECRET_KEY for any service
# whose .env already declares those keys (baked into the .env.example of
# every domain that owns file uploads) -- same "only touch keys the
# service's own .env.example already asks for" convention as
# resolve_peer_base_urls above. S3_BUCKET is left untouched; it's already
# domain-specific in each .env.example.
#
# The storage declaration is intentionally enough: `eco up` / `eco provision`
# owns the lifecycle and writes this client config. No secrets or machine
# endpoint need to be copied into a project artifact.
resolve_minio_s3_config() {
  local env_file="$1"
  # Bare `return` would inherit the failing grep/minio_configured's own
  # non-zero exit status (bash's `return` with no argument returns the
  # last command's status) -- fatal under `set -e` since this function is
  # called unguarded, and both of these are "not applicable" no-ops, not
  # errors. Explicit `return 0` avoids killing the whole script on the
  # first service that doesn't declare S3_ENDPOINT= (auth/profile/
  # payments today).
  grep -qE "^S3_ENDPOINT=" "$env_file" 2>/dev/null || return 0
  minio_configured || return 0

  local client_file
  client_file="$(minio_client_config_file)"
  if [[ ! -r "$client_file" ]]; then
    echo -e "  ${YELLOW}⚠${RESET}  storage.minio is declared but no managed client config exists at ${client_file}. Run eco up so Eco can provision MinIO. Leaving S3_* unset for ${env_file}"
    return
  fi

  local endpoint region access_key secret_key
  endpoint="$(grep -E '^S3_ENDPOINT=' "$client_file" | head -n1 | cut -d= -f2-)"
  region="$(grep -E '^S3_REGION=' "$client_file" | head -n1 | cut -d= -f2-)"
  access_key="$(grep -E '^S3_ACCESS_KEY=' "$client_file" | head -n1 | cut -d= -f2-)"
  secret_key="$(grep -E '^S3_SECRET_KEY=' "$client_file" | head -n1 | cut -d= -f2-)"
  if [[ -z "$endpoint" || -z "$access_key" || -z "$secret_key" ]]; then
    echo -e "  ${YELLOW}⚠${RESET}  Managed MinIO client config is incomplete: ${client_file}. Leaving S3_* unset for ${env_file}"
    return
  fi
  set_env "$env_file" "S3_ENDPOINT" "$endpoint"
  set_env "$env_file" "S3_REGION" "${region:-us-east-1}"
  set_env "$env_file" "S3_ACCESS_KEY" "$access_key"
  set_env "$env_file" "S3_SECRET_KEY" "$secret_key"
}

relpath() {
  python3 -c "import os; print(os.path.relpath('$2', '$1'))"
}

js_escape() {
  printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'
}

STATE_FILE=""

# .configure-state records Eco-owned allocation state as well as PROJECT_NAME.
# A port present in a domain's tracked .env.example is an application template
# default, not an allocation. In particular, a fresh production estate must
# never inherit a familiar value such as 8080 from an Auth template. The state
# also records the deployment mode so a local state file copied with source
# cannot accidentally make a newly deployed production estate look allocated.
# Sourcing it blindly overwrites whatever PROJECT_NAME the *caller* already
# set -- e.g. `eco up`'s remote command passes PROJECT_NAME=<project> as an
# env var, but tarAndPushDir (up.js) pushes projectDir's entire contents to
# the CT wholesale, including any leftover local .configure-state (not
# gitignored, not filtered), so a stale value saved during earlier local
# testing silently wins over the real one on every fresh CT. Restoring an
# already-set PROJECT_NAME after sourcing keeps the "remember my manual
# choice" behavior for interactive runs while never overriding an
# explicitly-provided one.
load_state_file() {
  [[ -f "$STATE_FILE" ]] || return 0
  local preserved_project_name="$PROJECT_NAME"
  source "$STATE_FILE"
  [[ -n "$preserved_project_name" ]] && PROJECT_NAME="$preserved_project_name"
}

select_pm2_dir() {
  if [[ -n "$PM2_DIR" ]]; then
    mkdir -p "$PM2_DIR"
    [[ ! -f "$PM2_DIR/.env" ]] && touch "$PM2_DIR/.env"
    STATE_FILE="$PM2_DIR/.configure-state"
    load_state_file
    return
  fi

  # Lean bootstrap repo: holds ecompose.yml but none of its own immediate
  # subdirs look like a service (no Cargo.toml/pom.xml/package.json/start.sh).
  # ecosystem.config.js stays in this dir; scan the estate root for services.
  # Not name-based (used to require a "_bootstrap" suffix) -- a manifest
  # repo can be named anything, e.g. this session's "rust" estate. The
  # legacy *_bootstrap / streamlined *_core names are both accepted.
  if [[ "$(basename "$PROJECT_DIR")" == *_bootstrap || "$(basename "$PROJECT_DIR")" == *_core ]] || _is_lean_manifest_dir "$PROJECT_DIR"; then
    PM2_DIR="$PROJECT_DIR"
    PROJECT_DIR="$(dirname "$PROJECT_DIR")"
    mkdir -p "$PM2_DIR"
    [[ ! -f "$PM2_DIR/.env" ]] && touch "$PM2_DIR/.env"
    STATE_FILE="$PM2_DIR/.configure-state"
    load_state_file
    return
  fi

  if ! is_interactive; then
    PM2_DIR="$PROJECT_DIR"
    mkdir -p "$PM2_DIR"
    [[ ! -f "$PM2_DIR/.env" ]] && touch "$PM2_DIR/.env"
    STATE_FILE="$PM2_DIR/.configure-state"
    load_state_file
    return
  fi

  echo ""
  echo -e "${BOLD}Where should ecosystem.config.js be stored?${RESET}"
  local -a dirs=("$PROJECT_DIR"/*)
  local -a names=()
  local selected=0
  local count

  # Add project root as option 1 (labeled as ".")
  names+=(".")

  # Gather top-level sibling directories
  for d in "${dirs[@]}"; do
    [[ ! -d "$d" ]] && continue
    local bname="$(basename "$d")"
    names+=("$bname")
  done

  if [[ ${#names[@]} -eq 1 ]]; then
    echo -e "${RED}No directories found in project.${RESET}"
    exit 1
  fi

  count=${#names[@]}

  for i in "${!names[@]}"; do
    if [[ $i -eq $selected ]]; then
      if [[ "${names[$i]}" == "." ]]; then
        echo -e "  ${CYAN}❯${RESET} . (project root)"
      else
        echo -e "  ${CYAN}❯${RESET} ${names[$i]}"
      fi
    else
      if [[ "${names[$i]}" == "." ]]; then
        echo "    . (project root)"
      else
        echo "    ${names[$i]}"
      fi
    fi
  done

  while true; do
    local key=""
    read -s -n1 key 2>/dev/null
    if [[ "$key" == $'\033' ]]; then
      local rest
      read -s -n2 rest 2>/dev/null
      key="$key$rest"
    fi
    if [[ -z "$key" ]]; then
      local selected_name="${names[$selected]}"
      if [[ "$selected_name" == "." ]]; then
        PM2_DIR="$PROJECT_DIR"
      else
        PM2_DIR="$PROJECT_DIR/$selected_name"
      fi
      echo ""
      break
    fi
    case "$key" in
      $'\033[A') [[ $selected -gt 0 ]] && selected=$((selected - 1)) ;;
      $'\033[B') [[ $selected -lt $((count - 1)) ]] && selected=$((selected + 1)) ;;
    esac
    echo -en "\033[${count}A"
    for i in "${!names[@]}"; do
      if [[ $i -eq $selected ]]; then
        if [[ "${names[$i]}" == "." ]]; then
          echo -e "  ${CYAN}❯${RESET} . (project root)"
        else
          echo -e "  ${CYAN}❯${RESET} ${names[$i]}"
        fi
      else
        if [[ "${names[$i]}" == "." ]]; then
          echo "    . (project root)"
        else
          echo "    ${names[$i]}"
        fi
      fi
    done
  done

  mkdir -p "$PM2_DIR"
  [[ ! -f "$PM2_DIR/.env" ]] && touch "$PM2_DIR/.env"
  STATE_FILE="$PM2_DIR/.configure-state"
  load_state_file
}

save_state() {
  local state_tmp="${STATE_FILE}.tmp.$$"
  cat > "$state_tmp" <<EOF
PROJECT_NAME="${PROJECT_NAME}"
ECO_PORTS_CONFIGURED="1"
ECO_PORTS_CONFIGURED_MODE="${DEPLOY_MODE}"
ECO_GATEWAY_PORT="${ECO_GATEWAY_PORT:-}"
EOF
  mv -f "$state_tmp" "$STATE_FILE"
}

prompt_project_name() {
  if [[ -n "$PROJECT_NAME" ]]; then
    return
  fi
  local default_name="${PROJECT_NAME:-$(basename "$PROJECT_DIR")}"
  if ! is_interactive; then
    PROJECT_NAME="$default_name"
    return
  fi
  local input_name=""

  echo ""
  read -p "Project name [${default_name}]: " input_name
  input_name="${input_name//[[:space:]]/}"
  PROJECT_NAME="${input_name:-$default_name}"
}

# ─── Dead code kept for reference — replaced by set_pm2_dir ─────────────────

_unused_select_main_dir() {
  local -a labels dirs
  local -a seen
  for d in "${svc_dir[@]}"; do
    local parent="${d#$PROJECT_DIR/}"
    parent="${parent%%/*}"
    local full="$PROJECT_DIR/$parent"
    local skip=false
    for s in "${seen[@]}"; do
      [[ "$s" == "$parent" ]] && { skip=true; break; }
    done
    $skip && continue
    seen+=("$parent")
    labels+=("$parent")
    dirs+=("$full")
  done
}

# ─── Service discovery ──────────────────────────────────────────────────────

# Discovered services stored as arrays:
#   name  type  dir  port_var  env_file  start_cmd
# Types: spring-boot | rust | go | nextjs | vite | node | static

declare -a svc_name svc_type svc_dir svc_port_var svc_env svc_cmd svc_port

# Domains skipped in local dev (declared `dev: disabled`, or `dev: optional`
# whose runtimes aren't available on the machine). Set by `eco up dev` as a
# comma-separated list; never set in prod, where every domain is mandatory.
dev_domain_skipped() {
  local domain="$1"
  [[ -n "$domain" ]] || return 1
  local raw
  raw="$(printf '%s' "${ECO_DEV_SKIP_DOMAINS:-}" | tr ',' ' ')"
  local skip
  for skip in $raw; do
    [[ "$skip" == "$domain" ]] && return 0
  done
  return 1
}

# Recursively scan directories for service markers.
# Stop recursing into a folder once a project marker is found (no nested projects).
# Usage: _scan_dir_rec <dir> <label> <scan_root> [rel_path]
_scan_dir_rec() {
  local scan_dir="$1" label="$2" scan_root="$3" rel_path="${4:-}"

  # Check for project markers at this level
  if [[ -f "$scan_dir/pom.xml" ]] || [[ -f "$scan_dir/Cargo.toml" ]] || [[ -f "$scan_dir/go.mod" ]] || [[ -f "$scan_dir/package.json" ]]; then
    # Build service name from relative path
    local rel_name="$label"
    if [[ -n "$rel_path" ]]; then
      rel_name="${label}-${rel_path//\//-}"
    fi

    if [[ -f "$scan_dir/pom.xml" ]]; then
      svc_name+=("$rel_name"); svc_type+=("spring-boot"); svc_dir+=("$scan_dir")
      svc_port_var+=("SERVER_PORT"); svc_env+=("$scan_dir/.env"); svc_cmd+=("mvn spring-boot:run")
    elif [[ -f "$scan_dir/Cargo.toml" ]] && [[ -f "$scan_dir/index.html" ]]; then
      # Leptos/Rust frontend: the built static dist is shipped (trunk build);
      # serve it as a static site, not a rust binary.
      svc_name+=("$rel_name"); svc_type+=("static"); svc_dir+=("$scan_dir")
      svc_port_var+=("PORT"); svc_env+=("$scan_dir/.env"); svc_cmd+=("serve-dist")
    elif [[ -f "$scan_dir/Cargo.toml" ]]; then
      svc_name+=("$rel_name"); svc_type+=("rust"); svc_dir+=("$scan_dir")
      svc_port_var+=("SERVER_PORT"); svc_env+=("$scan_dir/.env")
      if is_prod_mode; then
        # Release build for runtime performance; cargo no-ops on an
        # unchanged build so restarts after the first are fast.
        svc_cmd+=("cargo run --release")
      else
        # Debug build for fast iteration during development.
        svc_cmd+=("cargo run")
      fi
    elif [[ -f "$scan_dir/go.mod" ]]; then
      svc_name+=("$rel_name"); svc_type+=("go"); svc_dir+=("$scan_dir")
      svc_port_var+=("PORT"); svc_env+=("$scan_dir/.env"); svc_cmd+=("go run .")
    elif [[ -f "$scan_dir/package.json" ]] && grep -q '"next"' "$scan_dir/package.json" 2>/dev/null; then
      svc_name+=("$rel_name"); svc_type+=("nextjs"); svc_dir+=("$scan_dir")
      svc_port_var+=("PORT"); svc_env+=("$scan_dir/.env"); svc_cmd+=("npm run dev")
    elif [[ -f "$scan_dir/package.json" ]] && grep -q '"astro"' "$scan_dir/package.json" 2>/dev/null; then
      svc_name+=("$rel_name"); svc_type+=("astro"); svc_dir+=("$scan_dir")
      svc_port_var+=("PORT"); svc_env+=("$scan_dir/.env"); svc_cmd+=("npm run dev")
    elif [[ -f "$scan_dir/package.json" ]] && grep -q '"vite"' "$scan_dir/package.json" 2>/dev/null; then
      svc_name+=("$rel_name"); svc_type+=("vite"); svc_dir+=("$scan_dir")
      svc_port_var+=("PORT"); svc_env+=("$scan_dir/.env"); svc_cmd+=("npm run dev")
    elif [[ -f "$scan_dir/package.json" ]] && grep -q '"nuxt"' "$scan_dir/package.json" 2>/dev/null; then
      svc_name+=("$rel_name"); svc_type+=("nuxt"); svc_dir+=("$scan_dir")
      svc_port_var+=("PORT"); svc_env+=("$scan_dir/.env"); svc_cmd+=("npm run dev")
    else
      svc_name+=("$rel_name"); svc_type+=("node"); svc_dir+=("$scan_dir")
      # Plain Node services are commonly small production-ready HTTP servers
      # with a `start` script. Unlike Vite/Next/Astro, they have no separate
      # development-server convention for Eco to invoke.
      svc_port_var+=("PORT"); svc_env+=("$scan_dir/.env"); svc_cmd+=("npm run start")
    fi
    # Found project — don't recurse further
    return
  fi

  # No marker here — recurse into subdirectories
  for subdir in "$scan_dir"/*/ ; do
    [[ ! -d "$subdir" ]] && continue
    local basename
    basename="$(basename "$subdir")"
    [[ "$basename" == "node_modules" || "$basename" == "target" || "$basename" == ".next" ]] && continue
    local new_rel_path="$rel_path/$basename"
    new_rel_path="${new_rel_path#/}"
    _scan_dir_rec "$subdir" "$label" "$scan_root" "$new_rel_path"
  done
}

_scan_dir() {
  local scan_dir="$1" label="$2"
  local initial_count=${#svc_name[@]}
  _scan_dir_rec "$scan_dir" "$label" "$scan_dir"

  # Fallback: start.sh → static server (only if no projects found in this subtree)
  local found_in_subtree=false
  for ((i = initial_count; i < ${#svc_name[@]}; i++)); do
    found_in_subtree=true
    break
  done

  if ! $found_in_subtree && [[ -f "$scan_dir/start.sh" ]]; then
    local static_env="" static_cmd="bash start.sh"
    [[ -f "$scan_dir/.env" ]] && static_env="$scan_dir/.env"
    svc_name+=("$label"); svc_type+=("static"); svc_dir+=("$scan_dir")
    svc_port_var+=("PORT"); svc_env+=("$static_env"); svc_cmd+=("$static_cmd")
  fi
}

discover_services() {
  # Clear previous service entries before discovering
  svc_name=()
  svc_type=()
  svc_dir=()
  svc_port_var=()
  svc_env=()
  svc_cmd=()
  svc_port=()

  local manifest_path=""
  local -a declared_sibling_names=()
  manifest_path="$(resolve_manifest_path || true)"
  if [[ -n "$manifest_path" && -f "$manifest_path" ]]; then
    local manifest_dir=""
    local manifest_root=""
    manifest_dir="$(cd "$(dirname "$manifest_path")" && pwd)"
    # Lean bootstrap manifests are beside domains in development, while
    # production places the manifest directly at /opt/projects/<project>.
    # Resolve both layouts so a declared `auth` path never escapes the estate
    # into the shared /opt/projects directory.
    if [[ -n "$PROJECT_DIR" && "$manifest_dir" == "$(cd "$PROJECT_DIR" && pwd)" ]]; then
      manifest_root="$manifest_dir"
    else
      manifest_root="$(cd "$manifest_dir/.." && pwd)"
    fi
    while IFS='|' read -r service_label service_path; do
      [[ -z "$service_label" || -z "$service_path" ]] && continue
      # A domain skipped in local dev (e.g. rag with onnxruntime) is left out
      # of the generated config even if its clone is still on disk.
      if dev_domain_skipped "${service_path%%/*}"; then
        continue
      fi
      local target_dir="$service_path"
      if [[ "$target_dir" != /* ]]; then
        # Legacy single-repository estates used paths qualified with their
        # own project name (for example `assessment/backend`) even though
        # the manifest and backend live in the same repository root.  In a
        # production CT that repository is /opt/projects/<project>, so do
        # not manufacture /opt/projects/<project>/<project>/backend.  New
        # composed paths keep their first segment as the domain repository.
        local legacy_self_prefix="${PROJECT_NAME:-$(basename "$manifest_root")}/"
        if [[ "$target_dir" == "$legacy_self_prefix"* ]]; then
          target_dir="$manifest_root/${target_dir#"$legacy_self_prefix"}"
        else
          target_dir="$manifest_root/$target_dir"
        fi
      fi
      # A declared service can self-reference the manifest/bootstrap
      # project's own root (e.g. chronic_bootstrap's ecompose.yml declares
      # a service at path: chronic_bootstrap, pointing at the very
      # directory ecompose.yml lives in). Locally that resolves fine --
      # manifest_root/chronic_bootstrap really exists as a sibling
      # directory. On a CT bootstrapped by `eco up`, though, tarAndPushDir
      # (up.js) pushes that same directory's *contents* straight onto the
      # manifest's own directory (renamed to the clean project: name), so
      # no separate manifest_root/chronic_bootstrap directory ever exists
      # there -- the naive path silently resolves to nothing and the
      # service goes missing from PM2 entirely. The manifest's own
      # directory (dirname of manifest_path, already resolved above) is
      # exactly where that self-referenced content actually landed --
      # deliberately not PROJECT_DIR/PM2_DIR here, since select_pm2_dir's
      # lean-bootstrap branch reassigns PROJECT_DIR to manifest_root
      # (their *parent*) whenever PM2_DIR isn't pre-set by the caller
      # (true for `eco up`'s remote command, false for a bare `eco
      # configure` run in the same directory) -- using the manifest's own
      # directory instead of either var means this resolves the same way
      # regardless of which one the caller happened to set.
      local manifest_dir=""
      manifest_dir="$(dirname "$manifest_path")"
      if [[ ! -d "$target_dir" && -d "$manifest_dir" ]]; then
        if [[ -f "$manifest_dir/pom.xml" || -f "$manifest_dir/Cargo.toml" || -f "$manifest_dir/package.json" || -f "$manifest_dir/start.sh" ]]; then
          target_dir="$manifest_dir"
        fi
      fi
      if [[ -d "$target_dir" ]]; then
        _scan_dir "$target_dir" "$service_label"
        # Avoid scanning this sibling twice below. The manifest can name a
        # nested service directory (for example auth/backend), so compare the
        # top-level estate sibling rather than the exact service path.
        if [[ "$target_dir" == "$manifest_root"/* ]]; then
          local relative_target="${target_dir#"$manifest_root"/}"
          declared_sibling_names+=("${relative_target%%/*}")
        fi
      fi
    done < <(parse_manifest_services "$manifest_path")

    # LXS services (`lxs:` in ecompose.yml) have no `path:`, so they are never
    # source-discovered. `eco up dev` installs each as a local binary under
    # <project>/<service_name>/ with a start.sh (see up.rs
    # install_lxs_services_local); register them here so port allocation, env
    # fills, gateway routing, and PM2 all treat them like any other service.
    local -a lxs_scan_names=()
    while IFS='|' read -r lxs_svc lxs_domain; do
      [[ -z "$lxs_svc" || -z "$lxs_domain" ]] && continue
      local lxs_install_dir="$manifest_root/$lxs_svc"
      if [[ -d "$lxs_install_dir" && -f "$lxs_install_dir/start.sh" ]]; then
        local already_known=false
        for i in "${!svc_name[@]}"; do
          if [[ "${svc_name[$i]}" == "$lxs_svc" ]]; then
            already_known=true
            break
          fi
        done
        $already_known && continue
        svc_name+=("$lxs_svc"); svc_type+=("static"); svc_dir+=("$lxs_install_dir")
        svc_port_var+=("SERVER_PORT"); svc_env+=("$lxs_install_dir/.env"); svc_cmd+=("bash start.sh")
        lxs_scan_names+=("$lxs_svc")
      fi
    done < <(parse_manifest_lxs_services "$manifest_path")
    for lxs_svc in "${lxs_scan_names[@]}"; do
      declared_sibling_names+=("$lxs_svc")
    done
  fi

  # An estate commonly keeps reusable domains as immediate siblings. A
  # manifest can declare explicit runtime metadata for some of them, but it
  # must not hide sibling services that are already present in the estate.
  # Scan only siblings that the manifest did not already cover above.
  # Scope this scan to the active project root. In production multiple
  # projects share /opt/projects, so scanning PROJECT_ROOT would rediscover
  # another estate's domains (and legacy flat-layout directories) as ours.
  #
  # ECO_INIT=1, or a `.eco/state.json` marker (a project bootstrapped by
  # `eco init`), disables the sibling scan entirely: the project root is the
  # only scanned directory and every service must be declared in ecompose.yml
  # — no guessing, no flattening, no duplicate discovery.
  if [[ "${ECO_INIT:-}" != "1" && ! -f "$PROJECT_DIR/.eco/state.json" ]]; then
  for sibling_dir in "$PROJECT_DIR"/*/; do
    sibling_dir="${sibling_dir%/}"
    local sibling
    sibling="$(basename "$sibling_dir")"
    # Skip orchestrator/tooling directories and non-directories. node_modules/
    # target/ .next/ are also skipped inside _scan_dir_rec, but when one of
    # them sits directly under PROJECT_DIR it is scanned here as an estate
    # sibling (and _scan_dir_rec only skips them as CHILDREN, not as the scan
    # root) -- without this, every nested package.json under node_modules is
    # turned into a crash-looping PM2 app.
    if [[ "$sibling" == "core" || "$sibling" == "eco" || ! -d "$sibling_dir" ]]; then continue; fi
    if [[ "$sibling" == "node_modules" || "$sibling" == "target" || "$sibling" == ".next" ]]; then continue; fi
    # Skip dev-skipped domains (prod-only on this estate).
    if dev_domain_skipped "$sibling"; then continue; fi
    local declared_sibling
    local already_declared=false
    for declared_sibling in "${declared_sibling_names[@]}"; do
      if [[ "$declared_sibling" == "$sibling" ]]; then
        already_declared=true
        break
      fi
    done
    $already_declared && continue
    _scan_dir "$sibling_dir" "$sibling"
  done
  fi
}

# ─── Single-binary mode ────────────────────────────────────────────────────
#
# When ecompose.yml declares `target_mode: single-binary`, all individual
# Rust services are collapsed into one unified binary built from the
# project-level shim crate (*_binary/). Non-Rust services (Go, Node) are
# left unchanged. The unified binary listens on one port and merges all
# domain routers via tower::Steer dispatch instead of per-service HTTP.
condense_single_binary() {
  local manifest_path
  manifest_path="$(resolve_manifest_path || true)"
  [[ -z "$manifest_path" || ! -f "$manifest_path" ]] && return

  local target_mode
  target_mode="$(sed -n 's/^target_mode: *//p' "$manifest_path" | tr -d \" | tr -d \' | xargs)"
  if [[ "$target_mode" != "single-binary" ]]; then
    return 0
  fi

  local binary_name="${PROJECT_NAME}-binary"

  local shim_dir=""
  for d in "$PROJECT_DIR"/*_binary/; do
    if [[ -f "${d}Cargo.toml" ]]; then
      shim_dir="${d%/}"
      break
    fi
  done

  if [[ -z "$shim_dir" ]]; then
    echo -e "  ${YELLOW}!${RESET} single-binary mode but no *_binary/ crate under $PROJECT_DIR"
    return
  fi

  local rust_count=0
  local -a original_rust_dirs=()
  for i in "${!svc_name[@]}"; do
    if [[ "${svc_type[$i]}" == "rust" ]]; then
      rust_count=$((rust_count + 1))
      original_rust_dirs+=("${svc_dir[$i]}")
    fi
  done

  if [[ $rust_count -lt 2 ]]; then
    echo -e "  ${YELLOW}!${RESET} single-binary needs ≥2 Rust services (found $rust_count); leaving as-is"
    return
  fi

  local -a new_name=() new_type=() new_dir=() new_port_var=() new_env=() new_cmd=()
  for i in "${!svc_name[@]}"; do
    if [[ "${svc_type[$i]}" != "rust" ]]; then
      new_name+=("${svc_name[$i]}")
      new_type+=("${svc_type[$i]}")
      new_dir+=("${svc_dir[$i]}")
      new_port_var+=("${svc_port_var[$i]}")
      new_env+=("${svc_env[$i]}")
      new_cmd+=("${svc_cmd[$i]}")
    fi
  done

  new_name+=("$binary_name")
  new_type+=("rust")
  new_dir+=("$shim_dir")
  new_port_var+=("SERVER_PORT")
  new_env+=("$shim_dir/.env")
  new_cmd+=("cargo")

  svc_name=("${new_name[@]}")
  svc_type=("${new_type[@]}")
  svc_dir=("${new_dir[@]}")
  svc_port_var=("${new_port_var[@]}")
  svc_env=("${new_env[@]}")
  svc_cmd=("${new_cmd[@]}")

  # Remember the original domain directories so generate_rust_workspace can
  # include them as workspace members (the shim depends on them as lib crates).
  ECO_SINGLE_BINARY_DOMAIN_DIRS=("${original_rust_dirs[@]}")

  mkdir -p "$shim_dir"
  local env_example="$shim_dir/.env.example"
  if [[ ! -f "$env_example" ]]; then
    {
      echo "# Generated by eco configure.sh — single-binary mode"
      echo "# All env vars from composed Rust domains are merged here."
      echo ""
      echo "SERVER_PORT="
      echo "MONGODB_URI="
      echo "JWT_SECRET="
      echo "CORS_ALLOWED_ORIGINS="
      echo "REDIS_URL="
      echo "S3_ENDPOINT="
      echo "S3_BUCKET="
      echo "S3_REGION="
      echo "S3_ACCESS_KEY="
      echo "S3_SECRET_KEY="
      echo "BREVO_API_KEY="
      echo "MAIL_FROM_EMAIL="
      echo "MAIL_FROM_NAME="
      echo "NOTIFICATIONS_API_URL="
      echo "PUBLIC_SITE_URL="
    } > "$env_example"
  fi

  echo -e "  ${GREEN}✓${RESET} Single-binary mode: ${rust_count} Rust services → ${binary_name}"
}

# ─── Merge env vars for single-binary ──────────────────────────────────────
#
# After configure_envs has populated per-service .env files, this merges
# all the original Rust domain .env files into the single-binary's .env so
# PM2 passes every required env var to the unified process.
merge_single_binary_envs() {
  local manifest_path
  manifest_path="$(resolve_manifest_path || true)"
  [[ -z "$manifest_path" || ! -f "$manifest_path" ]] && return 0

  local target_mode
  target_mode="$(sed -n 's/^target_mode: *//p' "$manifest_path" | tr -d \" | tr -d \' | xargs)"
  if [[ "$target_mode" != "single-binary" ]]; then
    return 0
  fi

  local binary_env="$PROJECT_DIR/stuff8_binary/.env"
  if [[ ! -f "$binary_env" ]]; then
    return 0
  fi

  # Merge env vars from every original Rust domain .env into the binary .env.
  # The binary's own .env already has the keys from configure_envs; this
  # copies over any additional values (S3_*, BREVO_*, etc.) that only exist
  # in the original per-domain files.
  local merged=0
  if [[ -n "${ECO_SINGLE_BINARY_DOMAIN_DIRS[*]}" ]]; then
    local d domain_env
    for d in "${ECO_SINGLE_BINARY_DOMAIN_DIRS[@]}"; do
      domain_env="$d/.env"
      if [[ -f "$domain_env" ]]; then
        while IFS='=' read -r key value; do
          [[ -z "$key" ]] && continue
          [[ "$key" =~ ^[A-Z] ]] || continue
          local existing
          existing="$(grep "^${key}=" "$binary_env" 2>/dev/null | head -1 || true)"
          if [[ -z "$existing" ]]; then
            echo "${key}=${value}" >> "$binary_env"
            ((merged++)) || true
          elif [[ "$existing" == "${key}=" || "$existing" == "${key}=\"\"" ]]; then
            # Key exists but is empty — fill it from the domain .env
            sed -i "s|^${key}=.*|${key}=${value}|" "$binary_env"
            ((merged++)) || true
          fi
        done < "$domain_env"
      fi
    done
  fi

  if [[ $merged -gt 0 ]]; then
    echo -e "  ${GREEN}✓${RESET} Single-binary env: merged ${merged} additional vars from domain .env files"
  fi
  return 0
}

# ─── Generate a Cargo workspace over the composed rust domains ─────────────
#
# Each rust domain is an independently cloned repo under PROJECT_ROOT with
# its own Cargo.toml/target/ -- with no workspace, a fresh CT recompiles
# shared dependencies (aws-lc-rs, serde_json, tower-service, ...) once per
# domain instead of once total, and N concurrent `cargo run --release`
# processes fight each other for the CT's real core count. A generated
# root-level Cargo.toml with `[workspace] members = [...]` fixes both:
# cargo auto-discovers this ancestor workspace from any member's cwd (no
# svc_cmd change needed), builds shared deps once into one target/ dir,
# and serializes concurrent builds against that dir via its own file lock
# instead of oversubscribing the CPU.
#
# This is purely a composition-layer artifact (like ecosystem.config.js) --
# no domain repo is touched or needs to know it's part of a workspace, so
# a domain built standalone elsewhere is unaffected. Regenerated on every
# configure.sh run so it always matches the currently composed domains.
generate_rust_workspace() {
  # Scope the workspace to the active project directory (e.g.
  # /opt/projects/apindo/Cargo.toml), not PROJECT_ROOT.  Multiple projects
  # share PROJECT_ROOT (/opt/projects) on a single CT -- a root-level
  # workspace would merge domains from all projects into one build graph,
  # causing different projects' same-named services (e.g. rwid-auth-service)
  # to collide: only one binary lands in target/, whichever project built
  # last wins, and the other project silently runs the wrong binary.
  local workspace_file="$PROJECT_DIR/Cargo.toml"
  local marker="# Generated by eco configure.sh -- do not edit by hand."

  if [[ -f "$workspace_file" ]] && ! head -n1 "$workspace_file" | grep -qF "$marker"; then
    # Not ours -- don't touch a hand-authored Cargo.toml at the project root.
    return
  fi

  # Remove any stale workspace file that a previous eco version may have
  # incorrectly generated at PROJECT_ROOT (shared across all projects).
  local stale_root_file="$PROJECT_ROOT/Cargo.toml"
  if [[ -f "$stale_root_file" ]] && head -n1 "$stale_root_file" | grep -qF "$marker"; then
    rm -f "$stale_root_file"
  fi

  local -a rust_members=()
  for i in "${!svc_name[@]}"; do
    if [[ "${svc_type[$i]}" == "rust" ]]; then
      local rel
      case "${svc_dir[$i]}" in
        "$PROJECT_DIR"/*)
          rel="${svc_dir[$i]#"$PROJECT_DIR"/}"
          rust_members+=("$rel")
          ;;
      esac
    fi
  done

  # In single-binary mode the svc arrays only contain the shim crate itself,
  # but the shim depends on all domain lib crates as path dependencies.
  # Include the original domain directories so they become workspace members.
  if [[ ${#ECO_SINGLE_BINARY_DOMAIN_DIRS[@]} -gt 0 ]]; then
    for d in "${ECO_SINGLE_BINARY_DOMAIN_DIRS[@]}"; do
      case "$d" in
        "$PROJECT_DIR"/*)
          local rel="${d#"$PROJECT_DIR"/}"
          local already=false
          for m in "${rust_members[@]}"; do
            [[ "$m" == "$rel" ]] && already=true
          done
          $already || rust_members+=("$rel")
          ;;
      esac
    done
  fi

  # In single-binary mode there is only one Rust service (the unified binary)
  # after collapse, but the workspace must still exist so the shim can depend
  # on the domain lib crates listed above.
  local is_single_binary=false
  [[ -n "${ECO_SINGLE_BINARY_DOMAIN_DIRS[*]}" ]] && is_single_binary=true

  if [[ ${#rust_members[@]} -lt 2 && "$is_single_binary" == false ]]; then
    # Nothing to share -- remove a previously generated workspace file so
    # it doesn't linger listing domains that are no longer composed.
    if [[ -f "$workspace_file" ]]; then
      rm -f "$workspace_file"
    fi
    return
  fi

  # Cargo only honors [profile.*] from the workspace *root* once a package
  # joins a workspace -- each member's own [profile.release] is silently
  # ignored otherwise. Always emit a canonical release profile here so rust
  # domain binaries are built optimized, LTO'd, and without debuginfo; warn
  # if any composed member declares a divergent [profile.release].
  local canonical_release="opt-level = 3\nlto = true\nstrip = true"
  local m member_toml block
  for m in "${rust_members[@]}"; do
    member_toml="$PROJECT_DIR/${m}Cargo.toml"
    [[ -f "$member_toml" ]] || continue
    block="$(awk '
      /^\[profile\.release\]/ { capture=1; next }
      /^\[/ { capture=0 }
      capture && NF { print }
    ' "$member_toml")"
    [[ -n "$block" ]] || continue
    if echo "$block" | grep -qE '^opt-level[[:space:]]*=[[:space:]]*(0|1|2)\b' || \
       echo "$block" | grep -qE '^debug[[:space:]]*=[[:space:]]*true\b' || \
       echo "$block" | grep -qE '^strip[[:space:]]*=[[:space:]]*"none"'; then
      echo -e "  ${YELLOW}!${RESET} ${m}Cargo.toml declares a divergent [profile.release] (not fully optimized or keeps debuginfo). eco applies the canonical workspace profile (opt-level = 3, lto = true, strip = true); align the domain's profile to fix."
    fi
  done

  {
    echo "$marker"
    echo "# Composes the currently-composed rust domains into one Cargo"
    echo "# workspace so they share a single target/ dir and Cargo.lock"
    echo "# instead of each independently recompiling identical shared"
    echo "# dependencies."
    echo ""
    echo "[workspace]"
    echo "resolver = \"2\""
    echo "members = ["
    for m in "${rust_members[@]}"; do
      echo "  \"${m}\","
    done
    echo "]"
    echo ""
    echo "# Canonical release profile (see eco configure.sh): domain binaries"
    echo "# are always optimized, LTO'd, and stripped of debuginfo."
    echo "[profile.release]"
    echo -e "$canonical_release"
  } > "${workspace_file}.tmp.$$"
  mv -f "${workspace_file}.tmp.$$" "$workspace_file"

  echo -e "  ${GREEN}✓${RESET} Generated rust workspace at ${workspace_file} (${#rust_members[@]} members: ${rust_members[*]})"
}

# ─── Ensure MongoDB is actually running ────────────────────────────────────
#
# eco up's own data-bootstrap step (systemctl enable/restart mongod) only
# runs as part of the full up pipeline. Any manual/partial recovery step in
# between
# -- re-running configure.sh directly, or `pm2 start ecosystem.config.js` by
# hand -- skips both, so a service can be fully configured with a correct
# MONGODB_URI and still fail every request with a connection refused error
# because nothing ever started the database. Runs unconditionally here so
# every configure.sh invocation, not just the orchestrated up.js path,
# leaves Mongo actually usable if it's installed at all -- no-ops cleanly
# if MongoDB was never declared/installed on this machine.
ensure_mongod_running() {
  if ! command -v mongod >/dev/null 2>&1; then
    return
  fi

  # Restarting a system service needs root -- on a root-provisioned prod CT
  # that's always true, but on a personal dev machine running as a normal
  # user (no root, no passwordless sudo) it isn't, and letting systemctl's
  # raw polkit/pkttyagent errors spill to the terminal reads like a hard
  # failure even though it's swallowed. Detect that case up front and print
  # one clear, actionable line instead.
  local svc_sudo=""
  if [[ "$(id -u)" != "0" ]]; then
    if command -v sudo >/dev/null 2>&1 && sudo -n true 2>/dev/null; then
      svc_sudo="sudo"
    else
      echo -e "  ${YELLOW}!${RESET} Skipping mongod restart (not root, no passwordless sudo) -- make sure it's running manually, e.g. 'sudo systemctl enable --now mongod'"
      return
    fi
  fi

  if command -v systemctl >/dev/null 2>&1; then
    $svc_sudo systemctl enable mongod >/dev/null 2>&1 || true
    $svc_sudo systemctl restart mongod || true
  elif command -v service >/dev/null 2>&1; then
    $svc_sudo service mongod restart || true
  fi
}

# ─── LAN IP detection (dev mode, remote CT access) ─────────────────────────
#
# Shared by both the Next.js allowedDevOrigins fix and the CORS-origin fix
# below -- a personal dev CT is reached over its own LAN/Tailscale IP
# rather than localhost, and anything that hardcodes "localhost" as the
# only trusted dev origin breaks the moment the browser isn't running on
# the same machine as the services. Echoes one non-loopback IPv4 address
# per line; empty output (nothing echoed) if none are found.
detect_lan_ips() {
  hostname -I 2>/dev/null | tr ' ' '\n' | grep -vE '^(127\.|$)' || true
}

# ─── Next.js dev-server cross-origin allowlist ─────────────────────────────
#
# Next.js's dev server blocks cross-origin HMR/asset requests by default
# (DNS-rebinding protection) unless the requesting origin is listed in
# next.config.js's `allowedDevOrigins` -- this only bites when the browser
# reaches the dev server via something other than localhost, e.g. a
# remote dev machine accessed over its LAN/Tailscale-routed IP. Next.js has no CLI flag or env var for this --
# next.config.js is the only place it's read from -- so staying fully
# automated (no manual per-machine repo edit) means writing there
# directly. Appends a small managed block that mutates the *already
# resolved* module.exports object in place instead of parsing/rewriting
# the file, so an existing hand-authored config (webpack plugins,
# functions, wrapped exports like next-pwa) survives untouched.
# Idempotent (marker-guarded) and a clean no-op in prod mode, where `next
# start` serves a static build with no HMR websocket to protect.
# A convenience feature must never be able to take down the whole deploy --
# wrap the entire body so any unexpected real-world failure (unwritable
# file, unusual filesystem state, odd next.config.js content this wasn't
# tested against) degrades to a warning instead of aborting configure.sh
# under `set -e`. The caller also wraps this call in `|| true` as a second
# layer of defense.
ensure_next_allowed_dev_origins() {
  (
    set -e
    local dir="$1"
    local config_file="$dir/next.config.js"
    local marker="eco: allowedDevOrigins"

    # No file at all (common -- next.config.js is optional, plenty of
    # projects run on Next.js's defaults) means there's nothing to
    # accidentally clobber, so create a minimal one rather than skipping.
    [[ -f "$config_file" ]] || echo "module.exports = {};" > "$config_file"
    grep -q "$marker" "$config_file" && exit 0
    grep -qE '^\s*export default' "$config_file" && exit 0  # ESM config, not module.exports -- skip rather than risk a syntax error

    local ips
    ips="$(detect_lan_ips)"
    [[ -z "$ips" ]] && exit 0

    {
      echo ""
      echo "// ${marker} (auto-managed by eco configure.sh -- do not edit by hand)"
      echo "// Lets the Next.js dev server accept HMR/asset requests when reached over"
      echo "// this machine's own LAN/Tailscale IP instead of localhost."
      echo "if (module.exports && typeof module.exports === \"object\") {"
      echo "  module.exports.allowedDevOrigins = ["
      echo "    ...(module.exports.allowedDevOrigins || []),"
      local ip
      while IFS= read -r ip; do
        [[ -n "$ip" ]] && echo "    \"${ip}\","
      done <<< "$ips"
      echo "  ];"
      echo "}"
    } >> "$config_file"

    echo -e "  ${GREEN}✓${RESET} $(basename "$dir") — added LAN IP(s) to next.config.js allowedDevOrigins ($(echo "$ips" | tr '\n' ' '))"
  ) || echo -e "  ${YELLOW}!${RESET} $(basename "$dir") — could not update next.config.js allowedDevOrigins (non-fatal, skipped)"
}

# ─── Vite preview-server allowedHosts ──────────────────────────────────────
#
# Vite 5.4+ rejects any preview-server request whose Host header isn't in
# preview.allowedHosts (same DNS-rebinding protection as Next.js's
# allowedDevOrigins above), which blocks every plain client-side Vite app
# exposed publicly through `vite preview` (see generate_pm2's vite
# prod-mode fallback) with "Blocked request... not allowed" -- the public
# hostname from expose.hostname is never localhost. Vite has no CLI flag
# or env var for this, only the config file, so staying automated means
# writing there directly -- but unlike Next.js's CJS module.exports
# (mutable after the fact by appending code that runs later), a Vite
# config is ESM (`export default defineConfig({...})`), so there's no
# "append a block that patches the already-exported object" trick
# available. Only handles the common, unambiguous case: inserting into an
# existing `preview: {` block (the wrapper script this pairs with already
# implies one exists, since it's how `preview.port` would normally be
# set). No `preview:` block at all means inserting a brand new top-level
# key into an object literal of unknown shape -- too risky to guess at
# blindly, skipped with a warning instead.
ensure_vite_preview_allowed_hosts() {
  (
    set -e
    local dir="$1" hostname="$2"
    [[ -z "$hostname" ]] && exit 0

    local config_file=""
    local candidate
    for candidate in "$dir/vite.config.ts" "$dir/vite.config.js" "$dir/vite.config.mjs"; do
      if [[ -f "$candidate" ]]; then
        config_file="$candidate"
        break
      fi
    done
    [[ -z "$config_file" ]] && exit 0

    local marker="eco: preview.allowedHosts"
    grep -qF "$marker" "$config_file" && exit 0
    grep -qF "\"$hostname\"" "$config_file" && exit 0
    grep -qF "'$hostname'" "$config_file" && exit 0

    local preview_line
    preview_line="$(grep -nE '^[[:space:]]*preview[[:space:]]*:[[:space:]]*\{' "$config_file" | head -n1 | cut -d: -f1)"
    if [[ -z "$preview_line" ]]; then
      echo -e "  ${YELLOW}!${RESET} $(basename "$dir") — vite.config has no preview: {} block, skipping automatic preview.allowedHosts (add \"${hostname}\" to preview.allowedHosts by hand)"
      exit 0
    fi

    awk -v line="$preview_line" -v hostname="$hostname" -v marker="$marker" '
      NR == line {
        if ($0 ~ /\{[[:space:]]*\}[[:space:]]*$/) {
          sub(/\{[[:space:]]*\}[[:space:]]*$/, "{ allowedHosts: [\"" hostname "\"] } // " marker " (auto-managed by eco configure.sh -- do not edit by hand)")
          print
        } else {
          print
          print "    allowedHosts: [\"" hostname "\"], // " marker " (auto-managed by eco configure.sh -- do not edit by hand)"
        }
        next
      }
      { print }
    ' "$config_file" > "${config_file}.eco-tmp" && mv "${config_file}.eco-tmp" "$config_file"

    echo -e "  ${GREEN}✓${RESET} $(basename "$dir") — added \"${hostname}\" to vite.config preview.allowedHosts"
  ) || echo -e "  ${YELLOW}!${RESET} $(basename "$dir") — could not update vite.config preview.allowedHosts (non-fatal, skipped)"
}

# Astro's static `astro preview` internally starts Vite with `configFile:
# false`, which means neither astro.config.* nor a regular vite.config.* can
# set its `preview.allowedHosts`. Vite's environment escape hatch is also not
# consulted by preview mode. Generate an isolated Vite preview config and run
# that server for Astro's built static dist instead of mutating an estate's
# tracked configuration.
ensure_astro_preview_allowed_hosts() {
  local dir="$1" hostname="$2"
  [[ -z "$hostname" ]] && return 0
  local overlay="$dir/.eco-vite-preview.config.mjs"
  cat > "$overlay" <<EOF
// Auto-generated by Eco from expose.hostname. Do not edit.
export default {
  preview: {
    allowedHosts: ["${hostname}"],
  },
};
EOF
  echo -e "  ${GREEN}✓${RESET} $(basename "$dir") — configured Vite preview host \"${hostname}\" through managed overlay"
}

# ─── Port assignment ────────────────────────────────────────────────────────

assign_ports() {
  local selected_service=0
  local count=${#svc_name[@]}
  local service_name=""

  # The estate gateway (Caddy) binds a registry-managed port. Resolve it
  # first and reserve it in the registry so no service is ever assigned the
  # gateway's port (without this a service can win a port the gateway later
  # picks -- the gateway then serves itself and every / and /api/* request
  # loops through Caddy with a 308). Only when the gateway is actually
  # enabled: an estate without expose never allocates a gateway row.
  if gateway_enabled; then
    local gw_port=""
    gw_port="$(resolve_gateway_port)"
    ECO_GATEWAY_PORT="$gw_port"
  fi

  # Production must allocate independent private random ports for a newly composed
  # estate. Domain .env.example files often carry development examples such
  # as SERVER_PORT=8080; they are never a valid reason to pin a fresh estate
  # to that well-known range. The registry is the durable allocation record:
  # once a row exists for a service it is returned unchanged on every later
  # deploy, so internal routing stays stable without a state-marker heuristic.
  local preserve_existing_ports=true
  if is_prod_mode && [[ "$(registry_has_project)" != "1" ]]; then
    local has_legacy_pm2_allocation=false
    for service_name in "${svc_name[@]}"; do
      local legacy_port=""
      legacy_port="$(lookup_existing_pm2_port "$service_name" || true)"
      if [[ ! "$legacy_port" =~ ^[0-9]+$ ]]; then
        legacy_port="$(lookup_live_service_port "$service_name" || true)"
      fi
      if [[ "$legacy_port" =~ ^[0-9]+$ ]]; then
        has_legacy_pm2_allocation=true
        break
      fi
    done

    if $has_legacy_pm2_allocation; then
      echo -e "  ${CYAN}i${RESET} Existing production service ports detected — preserving and recording their allocation"
    else
      preserve_existing_ports=false
      echo -e "  ${CYAN}i${RESET} New production estate — allocating independently random private service ports"
    fi
  fi

  echo ""
  echo -e "${BOLD}Discovered services:${RESET}"
  for i in "${!svc_name[@]}"; do
    local current=""
    current="$(registry_lookup "${svc_name[$i]}" "service" || true)"
    if [[ ! "$current" =~ ^[0-9]+$ ]] && $preserve_existing_ports; then
      local ecosystem_current=""
      ecosystem_current="$(lookup_existing_pm2_port "${svc_name[$i]}" || true)"
      if [[ "$ecosystem_current" =~ ^[0-9]+$ ]]; then
        current="$ecosystem_current"
      elif [[ -n "${svc_env[$i]}" && -f "${svc_env[$i]}" ]]; then
        current=$(get_env "${svc_env[$i]}" "${svc_port_var[$i]}")
      fi
    fi
    printf "  %2d. ${CYAN}%-20s${RESET} %-12s current: %s\n" $((i+1)) "${svc_name[$i]}" "${svc_type[$i]}" "${current:-not set}"
  done

  if [[ -n "$FIRST_SERVICE" ]]; then
    for i in "${!svc_name[@]}"; do
      if [[ "${svc_name[$i]}" == "$FIRST_SERVICE" ]]; then
        selected_service="$i"
        break
      fi
    done
  elif is_interactive; then
    echo ""
    echo -e "${BOLD}Which service should receive an optional fixed port?${RESET}"
    for i in "${!svc_name[@]}"; do
      if [[ $i -eq $selected_service ]]; then
        echo -e "  ${CYAN}❯${RESET} ${svc_name[$i]}"
      else
        echo "    ${svc_name[$i]}"
      fi
    done

    while true; do
      local key=""
      read -s -n1 key 2>/dev/null
      if [[ "$key" == $'\033' ]]; then
        local rest
        read -s -n2 rest 2>/dev/null
        key="$key$rest"
      fi
      if [[ -z "$key" ]]; then
        echo ""
        break
      fi
      case "$key" in
        $'\033[A') [[ $selected_service -gt 0 ]] && selected_service=$((selected_service - 1)) ;;
        $'\033[B') [[ $selected_service -lt $((count - 1)) ]] && selected_service=$((selected_service + 1)) ;;
      esac
      echo -en "\033[${count}A"
      for i in "${!svc_name[@]}"; do
        if [[ $i -eq $selected_service ]]; then
          echo -e "  ${CYAN}❯${RESET} ${svc_name[$i]}"
        else
          echo "    ${svc_name[$i]}"
        fi
      done
    done
  fi

  local requested_port=""
  if [[ -n "$START_PORT" ]]; then
    requested_port="$START_PORT"
  elif is_interactive; then
    echo ""
    read -p "Optional fixed port for ${svc_name[$selected_service]} (Enter = random): " requested_port
    requested_port="${requested_port//[[:space:]]/}"
  fi

  if [[ -n "$requested_port" && ( ! "$requested_port" =~ ^[1-9][0-9]{0,4}$ || "$requested_port" -gt 65535 ) ]]; then
    echo -e "${RED}Requested port must be a valid TCP port number.${RESET}" >&2
    return 1
  fi

  # Every service's port is a registry row: allocated once and returned
  # unchanged forever after. A requested fixed port pins the first
  # allocation; otherwise a fresh estate gets independent random ports and
  # a preserving run adopts its legacy PM2/.env allocation. The registry's
  # (scope, port) unique index makes a collision between two services (or a
  # service and the gateway) impossible -- allocation never relies on `+1`.
  for i in "${!svc_name[@]}"; do
    local assigned_port=""
    assigned_port="$(registry_lookup "${svc_name[$i]}" "service" || true)"
    if [[ "$assigned_port" =~ ^[0-9]+$ ]]; then
      svc_port[$i]="$assigned_port"
      continue
    fi

    if [[ $i -eq $selected_service && -n "$requested_port" ]]; then
      assigned_port="$(registry_pin "${svc_name[$i]}" "service" "${svc_port_var[$i]}" "$requested_port")" || return 1
    elif $preserve_existing_ports; then
      local legacy=""
      legacy="$(lookup_existing_pm2_port "${svc_name[$i]}" || true)"
      if [[ ! "$legacy" =~ ^[0-9]+$ ]]; then
        legacy="$(lookup_live_service_port "${svc_name[$i]}" || true)"
      fi
      if [[ ! "$legacy" =~ ^[0-9]+$ && -n "${svc_env[$i]}" && -f "${svc_env[$i]}" ]]; then
        legacy="$(get_env "${svc_env[$i]}" "${svc_port_var[$i]}")"
      fi
      if [[ "$legacy" =~ ^[0-9]+$ ]]; then
        assigned_port="$(registry_seed "${svc_name[$i]}" "service" "${svc_port_var[$i]}" "$legacy")" || return 1
      else
        assigned_port="$(registry_get_or_allocate "${svc_name[$i]}" "service" "${svc_port_var[$i]}")" || return 1
      fi
    else
      assigned_port="$(registry_get_or_allocate "${svc_name[$i]}" "service" "${svc_port_var[$i]}")" || return 1
    fi
    svc_port[$i]="$assigned_port"
  done

  # The index port (PM2 directory's own .env) is a registry row too: reuse
  # an existing allocation, adopt a legacy one, or allocate fresh.
  INDEX_PORT="$(registry_lookup "index" "index" || true)"
  if [[ ! "$INDEX_PORT" =~ ^[0-9]+$ ]]; then
    local legacy_index=""
    if [[ -f "$PM2_DIR/.env" ]]; then
      legacy_index="$(get_env "$PM2_DIR/.env" "PORT")"
    fi
    if [[ ! "$legacy_index" =~ ^[0-9]+$ ]]; then
      legacy_index="$(lookup_live_service_port "index" || true)"
    fi
    if [[ "$legacy_index" =~ ^[0-9]+$ ]]; then
      INDEX_PORT="$(registry_seed "index" "index" "PORT" "$legacy_index")" || return 1
    else
      INDEX_PORT="$(registry_get_or_allocate "index" "index" "PORT")" || return 1
    fi
  fi
}

# ─── Configure .env files ───────────────────────────────────────────────────

configure_envs() {
  echo ""
  echo -e "${BOLD}Applying configuration...${RESET}"

  # Pre-scan to find auth and backend ports
  local auth_port="" default_api_port="" default_api_name=""
  local -a backend_names=()
  local -a backend_port_values=()
  local -a cors_origins=()
  local shared_jwt_secret=""
  local public_app_origin=""
  local public_api_base_url=""
  local _lan_ip

  # Opt-in (set only on a personal remote-dev CT, never on a plain local
  # Mac -- ECO_DEV_LAN_ACCESS defaults unset so ordinary local dev keeps
  # using localhost exactly as before). When set, browser-facing dev URLs
  # (the ones a phone/tablet's browser actually has to reach, as opposed
  # to backend-to-backend calls which always stay on localhost regardless
  # of which device the browser is on) use the machine's own detected
  # LAN/Tailscale IP instead, so the app works when accessed from a
  # different device over the host's Tailscale subnet route.
  local dev_host="localhost"
  if is_truthy "${ECO_DEV_LAN_ACCESS:-}"; then
    local _first_lan_ip
    _first_lan_ip="$(detect_lan_ips | head -n1)"
    [[ -n "$_first_lan_ip" ]] && dev_host="$_first_lan_ip"
  fi

  # Same rationale as ensure_next_allowed_dev_origins: a personal dev CT is
  # reached over its LAN/Tailscale IP, not localhost, so backend CORS needs
  # to trust that origin too or the browser blocks every frontend->backend
  # API call. Computed once here, applied below and for INDEX_PORT.
  local -a dev_lan_ips=()
  if ! is_prod_mode; then
    while IFS= read -r _lan_ip; do
      [[ -n "$_lan_ip" ]] && dev_lan_ips+=("$_lan_ip")
    done <<< "$(detect_lan_ips)"
  fi
  local public_auth_base_url=""
  local manifest_path=""
  manifest_path="$(resolve_manifest_path || true)"
  # Estate-level Auth configuration is intentionally separate from the Auth
  # domain's tracked .env.example: it varies per estate. Structured values
  # (email_verification.enabled, ttl_hours, mail_from_*) come from ecompose.yml.
  # Secrets (BREVO_API_KEY, MAIL_FROM_EMAIL, MAIL_FROM_NAME) are inherited from
  # the host environment when present, so a freshly-deployed project picks them
  # up automatically without manual .env editing on the CT.
  local auth_email_verification_enabled=""
  local auth_email_verification_ttl=""
  local auth_mail_from_email=""
  local auth_mail_from_name=""
  if [[ -n "$manifest_path" && -f "$manifest_path" ]]; then
    auth_email_verification_enabled="$(ecompose_nested_value "$manifest_path" "auth" "email_verification" "enabled")"
    auth_email_verification_ttl="$(ecompose_nested_value "$manifest_path" "auth" "email_verification" "ttl_hours")"
    auth_mail_from_email="$(ecompose_nested_value "$manifest_path" "auth" "email_verification" "mail_from_email")"
    auth_mail_from_name="$(ecompose_nested_value "$manifest_path" "auth" "email_verification" "mail_from_name")"
  fi

  if is_prod_mode; then
    public_app_origin="$(resolve_public_app_origin || true)"
    public_api_base_url="$(resolve_public_api_base_url || true)"
    public_auth_base_url="$(resolve_public_auth_base_url || true)"
  fi

  # Precompute the auth backend port before the per-service loop so LXS
  # (static type, ordered last) services can resolve AUTH_BASE_URL regardless
  # of service order.
  for _j in "${!svc_name[@]}"; do
    if [[ "${svc_name[$_j]}" == "auth-backend" || "${svc_name[$_j]}" == *-auth-backend ]]; then
      auth_port="${svc_port[$_j]}"
      break
    fi
  done

  for j in "${!svc_name[@]}"; do
    local sname="${svc_name[$j]}"
    local stype="${svc_type[$j]}"
    local sport="${svc_port[$j]}"
    if [[ "$sname" == "auth-backend" || "$sname" == *-auth-backend ]]; then
      auth_port="${sport}"
    elif [[ "$stype" == "spring-boot" || "$stype" == "rust" || "$stype" == "go" || "$stype" == "static" ]]; then
      backend_names+=("$sname")
      backend_port_values+=("$sport")
      if [[ -z "$default_api_port" ]]; then
        default_api_port="${sport}"
        default_api_name="${sname}"
      fi
    fi

    case "$stype" in
      nextjs|vite|astro|nuxt|node|static)
        if is_prod_mode; then
          if [[ -n "$public_app_origin" ]]; then
            cors_origins+=("$public_app_origin")
          fi
        else
          cors_origins+=("http://localhost:${sport}")
          cors_origins+=("http://127.0.0.1:${sport}")
          for _lan_ip in "${dev_lan_ips[@]}"; do
            cors_origins+=("http://${_lan_ip}:${sport}")
          done
        fi
        ;;
    esac
  done

  if ! is_prod_mode && [[ -n "$INDEX_PORT" ]]; then
    cors_origins+=("http://localhost:${INDEX_PORT}")
    cors_origins+=("http://127.0.0.1:${INDEX_PORT}")
    for _lan_ip in "${dev_lan_ips[@]}"; do
      cors_origins+=("http://${_lan_ip}:${INDEX_PORT}")
    done
  fi

  local cors_allowed_origins=""
  if [[ ${#cors_origins[@]} -gt 0 ]]; then
    local -a unique_cors_origins=()
    local origin
    for origin in "${cors_origins[@]}"; do
      if [[ " ${unique_cors_origins[*]} " != *" ${origin} "* ]]; then
        unique_cors_origins+=("$origin")
      fi
    done
    cors_allowed_origins="$(join_by "," "${unique_cors_origins[@]}")"
  fi
  shared_jwt_secret="$(resolve_shared_jwt_secret)"

  for i in "${!svc_name[@]}"; do
    local name="${svc_name[$i]}"
    local type="${svc_type[$i]}"
    local dir="${svc_dir[$i]}"
    local port="${svc_port[$i]}"
    local port_var="${svc_port_var[$i]}"
    local env_file="${svc_env[$i]}"
    local matched_api_port="$default_api_port"
    local matched_api_name="$default_api_name"

    if [[ "$name" == *-frontend ]]; then
      local domain_prefix="${name%-frontend}"
      local specific_backend_port
      specific_backend_port="$(lookup_backend_port "${domain_prefix}-backend")"
      if [[ -n "$specific_backend_port" ]]; then
        matched_api_port="$specific_backend_port"
        matched_api_name="${domain_prefix}-backend"
      fi
    elif [[ "$name" == "frontend" ]]; then
      local root_backend_port
      root_backend_port="$(lookup_backend_port "backend")"
      if [[ -n "$root_backend_port" ]]; then
        matched_api_port="$root_backend_port"
        matched_api_name="backend"
      fi
    fi

    # Skip if no .env file or path
    if [[ -z "$env_file" ]]; then
      echo -e "  ${YELLOW}⚠${RESET} $name — no .env configured, skipping"
      continue
    fi

    check_env_file "$env_file"
    sync_env_from_example "$env_file"
    fill_shell_env_secret "$env_file" "DEEPSEEK_API_KEY"
    # contact-form notifier target: the estate owner's inbox, pinned once in
    # the CT shell like other operator secrets (NOTIFY_TO, NOTIFY_NAME).
    fill_shell_env_secret "$env_file" "NOTIFY_TO"
    fill_shell_env_secret "$env_file" "NOTIFY_NAME"

    # Configure the service
    set_env "$env_file" "$port_var" "$port"

    # Some backends (e.g. the notifications domain) bind the port they read
    # from `PORT` in their own .env.example, while the registry row for a
    # backend service lives under `SERVER_PORT`. If the service declares
    # PORT= and it differs from the registry env var, mirror the registry
    # port there too so the app binds the assigned port instead of a stale
    # global one (8090). Two estates composing the same domain would otherwise
    # collide on that port. Frontends already use PORT as their registry var
    # (port_var == PORT), so they are untouched by this mirror.
    if [[ "$port_var" != "PORT" ]] && grep -qE "^PORT=" "$env_file" 2>/dev/null; then
      set_env "$env_file" "PORT" "$port"
    fi

    # A declared mongodb@ runtime always gets an estate-local URI, even when
    # the domain predates Eco and has no MONGODB_URI line in .env.example.
    # Explicit unmanaged URIs remain untouched for services without that
    # runtime declaration.
    local mongo_declared=false
    if manifest_service_uses_runtime "$manifest_path" "$name" "mongodb@"; then
      mongo_declared=true
    fi
    if $mongo_declared || grep -qE "^MONGODB_URI=" "$env_file" 2>/dev/null; then
      local db_name="${name//-/_}"
      local managed_mongo_uri
      managed_mongo_uri="$(get_env "$env_file" "ECO_MANAGED_MONGODB_URI")"
      if $mongo_declared || [[ "$managed_mongo_uri" =~ ^(1|true|yes|on)$ ]]; then
        # The service explicitly delegates its local database identity to
        # Eco. This prevents a legacy .env.example value (for another
        # estate) from leaking into the current project.
        set_env "$env_file" "MONGODB_URI" "mongodb://localhost:27017/${db_name}_${PROJECT_NAME}"
        # Record the managed database in the registry: username/port live
        # plaintext (they are not secrets), the password is stored encrypted.
        # A domain that declares mongodb@ has a contractual right to read its
        # own connection metadata back from the registry.
        registry_record_db "$name" "mongodb" 27017 "${db_name}_${PROJECT_NAME}" "" ""
      else
        # A service without the marker owns its Mongo connection string
        # (for example, an externally managed MongoDB cluster).
        set_env_if_missing "$env_file" "MONGODB_URI" "mongodb://localhost:27017/${db_name}_${PROJECT_NAME}"
      fi
    fi

    # Redis is estate-local and private. A domain opts in explicitly with a
    # redis@7 runtime; its connection string is never a project-owned secret.
    if manifest_service_uses_runtime "$manifest_path" "$name" "redis@7"; then
      set_env "$env_file" "REDIS_URL" "redis://127.0.0.1:6379"
      registry_record_db "$name" "redis" 6379 "" "" ""
    fi

    # RAG/embedding services load onnxruntime dynamically (load-dynamic
    # build). On Linux/CTs the shared library is provisioned by eco up into
    # /opt/eco-tools; point ORT_DYLIB_PATH at it. On dev macOS the operator
    # provides the library (brew install onnxruntime) so nothing is forced.
    if manifest_service_uses_runtime "$manifest_path" "$name" "onnxruntime" \
      && [[ "$(uname -s)" != "Darwin" ]]; then
      set_env "$env_file" "ORT_DYLIB_PATH" "/opt/eco-tools/libonnxruntime.so"
    fi

    if grep -qE "^JWT_SECRET=" "$env_file" 2>/dev/null || [[ "$type" == "spring-boot" ]]; then
      set_env "$env_file" "JWT_SECRET" "$shared_jwt_secret"
    fi

    if grep -qE "^APP_KAFKA_ENABLED=" "$env_file" 2>/dev/null; then
      set_env "$env_file" "APP_KAFKA_ENABLED" "false"
    fi

    # No-ops for any service whose .env doesn't already declare S3_ENDPOINT=
    # (auth/profile/payments today), and for estates that never opted into
    # storage.minio at all.
    resolve_minio_s3_config "$env_file"

    case "$type" in
      spring-boot)
        local api_base_url="http://${dev_host}:${port}/api"
        if is_prod_mode; then
          if [[ "$name" == "auth-backend" || "$name" == *-auth-backend ]]; then
            [[ -n "$public_auth_base_url" ]] && api_base_url="$public_auth_base_url"
          elif [[ -n "$public_api_base_url" ]]; then
            api_base_url="$public_api_base_url"
          fi
        fi
        set_env "$env_file" "API_BASE_URL" "$api_base_url"
        if [[ -n "$cors_allowed_origins" ]]; then
          set_env "$env_file" "CORS_ALLOWED_ORIGINS" "$cors_allowed_origins"
          set_env "$env_file" "cors.allowed-origins" "$cors_allowed_origins"
        fi
        if [[ -n "$auth_port" && "$name" != "auth-backend" && "$name" != *-auth-backend ]]; then
          local auth_base_url="http://${dev_host}:${auth_port}/api"
          set_env "$env_file" "AUTH_BASE_URL" "$auth_base_url"
        fi
        if grep -qE "^GOOGLE_REDIRECT_URI=" "$env_file" 2>/dev/null; then
          local google_redirect_uri="http://${dev_host}:${port}/api/auth/oauth/google/callback"
          if is_prod_mode && [[ -n "$public_auth_base_url" ]]; then
            google_redirect_uri="${public_auth_base_url}/auth/oauth/google/callback"
          fi
          set_env "$env_file" "GOOGLE_REDIRECT_URI" "$google_redirect_uri"
        fi
        echo -e "  ${GREEN}✓${RESET} $name"
        ;;
      rust)
        local api_base_url="http://${dev_host}:${port}$(api_base_path_for_backend "$name")"
        if is_prod_mode; then
          if [[ "$name" == "auth-backend" || "$name" == *-auth-backend ]]; then
            [[ -n "$public_auth_base_url" ]] && api_base_url="$public_auth_base_url"
          elif [[ -n "$public_api_base_url" ]]; then
            api_base_url="$(public_api_base_url_for_backend "$name" "$public_api_base_url")"
          fi
        fi
        set_env "$env_file" "API_BASE_URL" "$api_base_url"
        if [[ -n "$cors_allowed_origins" ]]; then
          set_env "$env_file" "CORS_ALLOWED_ORIGINS" "$cors_allowed_origins"
        fi
        # Auth owns email-verification links. The same managed public auth
        # route used by clients is written here, so domain code never needs a
        # hand-maintained hostname in its .env. SMTP/Brevo credentials remain
        # deliberately operator-supplied secrets.
        if [[ "$name" == "auth-backend" || "$name" == *-auth-backend ]]; then
          local auth_public_url="http://${dev_host}:${port}/api"
          if is_prod_mode && [[ -n "$public_auth_base_url" ]]; then
            auth_public_url="$public_auth_base_url"
          fi
          set_env "$env_file" "AUTH_PUBLIC_URL" "$auth_public_url"
          if [[ -n "$auth_email_verification_enabled" ]]; then
            set_env "$env_file" "EMAIL_VERIFICATION_REQUIRED" "$auth_email_verification_enabled"
          fi
          if [[ -n "$auth_email_verification_ttl" ]]; then
            set_env "$env_file" "EMAIL_VERIFICATION_TTL_HOURS" "$auth_email_verification_ttl"
          fi
          if [[ -n "$auth_mail_from_email" ]]; then
            set_env "$env_file" "MAIL_FROM_EMAIL" "$auth_mail_from_email"
          fi
          if [[ -n "$auth_mail_from_name" ]]; then
            set_env "$env_file" "MAIL_FROM_NAME" "$auth_mail_from_name"
          fi
          # Inherit mail credentials from host environment so newly-deployed
          # projects pick them up automatically without manual .env editing.
          # ecompose.yml values (above) take precedence; host env is fallback.
          # Only written when the host actually exports the variable -- avoids
          # blanking a value already set in the .env by a previous run.
          inherit_mail_credentials "$env_file"
          # `eco serve` supplies a short-lived, hostname-scoped relay
          # capability after reserving the public URL. It lets Auth deliver a
          # recovery email without placing either the agent API key or Brevo
          # credentials in the local estate.
          if [[ -n "${ECO_AUTH_EMAIL_RELAY_URL:-}" && -n "${ECO_AUTH_EMAIL_RELAY_TOKEN:-}" ]]; then
            set_env "$env_file" "EMAIL_RELAY_URL" "$ECO_AUTH_EMAIL_RELAY_URL"
            set_env "$env_file" "EMAIL_RELAY_TOKEN" "$ECO_AUTH_EMAIL_RELAY_TOKEN"
          fi
          if [[ -n "${ECO_AUTH_EMAIL_PUBLIC_URL:-}" ]]; then
            set_env "$env_file" "EMAIL_VERIFICATION_PUBLIC_URL" "$ECO_AUTH_EMAIL_PUBLIC_URL"
          fi
          if [[ "$(get_env "$env_file" "EMAIL_VERIFICATION_REQUIRED")" =~ ^(1|true|yes|on)$ ]] && [[ -z "$(get_env "$env_file" "BREVO_API_KEY")" ]]; then
            echo -e "  ${YELLOW}⚠${RESET} $name — email verification is enabled; set BREVO_API_KEY and MAIL_FROM_EMAIL in its Eco-managed .env"
          fi
        fi
        # email-manager also needs the operator-supplied Brevo credentials so
        # the estate's transactional email domain can send (queue, suppress,
        # warm-up) without anyone hand-editing its generated .env.
        # The estate sender declared in ecompose.yml
        # (auth.email_verification.mail_from_*) takes precedence; host env is
        # only a fallback when the manifest does not declare a sender.
        if [[ "$name" == "email-manager-backend" || "$name" == *-email-manager-backend ]]; then
          if [[ -n "$auth_mail_from_email" ]]; then
            set_env "$env_file" "MAIL_FROM_EMAIL" "$auth_mail_from_email"
          fi
          if [[ -n "$auth_mail_from_name" ]]; then
            set_env "$env_file" "MAIL_FROM_NAME" "$auth_mail_from_name"
          fi
          inherit_mail_credentials "$env_file"
        fi
        if [[ -n "$auth_port" && "$name" != "auth-backend" && "$name" != *-auth-backend ]]; then
          local auth_base_url="http://${dev_host}:${auth_port}/api"
          set_env "$env_file" "AUTH_BASE_URL" "$auth_base_url"
        fi
        # Generic peer-domain dependency resolution: a service declares what
        # it needs by having a `<PREFIX>_BASE_URL=` line in its own .env (the
        # dependency is visible right there in the repo, per eco's "explicit
        # dependencies at composition time" principle) and configure.sh
        # resolves it by finding a discovered backend service named
        # `<prefix>-backend`. Backend-to-backend URLs always stay internal
        # (localhost), in dev and prod alike, per eco's own doctrine.
        resolve_peer_base_urls "$env_file" "$name"
        resolve_peer_public_urls "$env_file" "$name"
        echo -e "  ${GREEN}✓${RESET} $name"
        ;;
      go)
        # Go services (go.mod) are configured like Rust backends: their own
        # API base URL, estate CORS, the auth backend URL, and any declared
        # peer-domain dependencies. The binary is started by PM2 via the
        # discovered `go run .` command, which recompiles on start.
        local api_base_url="http://${dev_host}:${port}$(api_base_path_for_backend "$name")"
        if is_prod_mode; then
          if [[ "$name" == "auth-backend" || "$name" == *-auth-backend ]]; then
            [[ -n "$public_auth_base_url" ]] && api_base_url="$public_auth_base_url"
          elif [[ -n "$public_api_base_url" ]]; then
            api_base_url="$(public_api_base_url_for_backend "$name" "$public_api_base_url")"
          fi
        fi
        set_env "$env_file" "API_BASE_URL" "$api_base_url"
        if [[ -n "$cors_allowed_origins" ]]; then
          set_env "$env_file" "CORS_ALLOWED_ORIGINS" "$cors_allowed_origins"
        fi
        if [[ -n "$auth_port" && "$name" != "auth-backend" && "$name" != *-auth-backend ]]; then
          local auth_base_url="http://${dev_host}:${auth_port}/api"
          set_env "$env_file" "AUTH_BASE_URL" "$auth_base_url"
        fi
        resolve_peer_base_urls "$env_file" "$name"
        resolve_peer_public_urls "$env_file" "$name"
        echo -e "  ${GREEN}✓${RESET} $name"
        ;;
      nextjs)
        local nextauth_url="http://${dev_host}:${port}"
        local next_public_app_url="http://${dev_host}:${port}"
        local next_public_api_url="http://${dev_host}:${matched_api_port}$(api_base_path_for_backend "$matched_api_name")"
        local next_public_auth_url="http://${dev_host}:${auth_port}/api"
        if is_prod_mode; then
          [[ -n "$public_app_origin" ]] && nextauth_url="$public_app_origin"
          [[ -n "$public_app_origin" ]] && next_public_app_url="$public_app_origin"
          [[ -n "$public_api_base_url" ]] && next_public_api_url="$(public_api_base_url_for_backend "$matched_api_name" "$public_api_base_url")"
          [[ -n "$public_auth_base_url" ]] && next_public_auth_url="$public_auth_base_url"
        fi
        set_env "$env_file" "NEXTAUTH_URL" "$nextauth_url"
        set_env "$env_file" "NEXT_PUBLIC_APP_URL" "$next_public_app_url"
        if [[ -n "$matched_api_port" ]]; then
          set_env "$env_file" "NEXT_PUBLIC_API_URL" "$next_public_api_url"
        fi
        if [[ -n "$auth_port" ]]; then
          set_env "$env_file" "NEXT_PUBLIC_AUTH_URL" "$next_public_auth_url"
        fi
        # Generic peer-domain dependency resolution for frontends composing
        # multiple domain backends directly, e.g. lms-frontend declaring
        # NEXT_PUBLIC_COURSES_API_URL/NEXT_PUBLIC_PROFILE_API_URL/etc in its
        # own .env.
        if is_prod_mode; then
          # In prod every domain backend's own API_BASE_URL is already set
          # to this same public origin (see the `rust)` case above), and
          # the gateway fans /api/<domain-prefix>/* out to the right
          # backend by path (see generate_gateway_config) -- so every
          # per-domain frontend var collapses to the one public URL rather
          # than needing distinct per-service ports the way dev mode does.
          if [[ -n "$public_api_base_url" ]]; then
            local domain_prefix domain_env_key
            for domain_prefix in "${domain_gateway_prefix[@]}"; do
              domain_env_key="NEXT_PUBLIC_$(printf '%s' "$domain_prefix" | tr '[:lower:]' '[:upper:]')_API_URL"
              if grep -qE "^${domain_env_key}=" "$env_file" 2>/dev/null; then
                set_env "$env_file" "$domain_env_key" "$public_api_base_url"
              fi
            done
          fi
        else
          resolve_frontend_peer_api_urls "$env_file" "$dev_host"
        fi
        echo -e "  ${GREEN}✓${RESET} $name"
        ;;
      static)
        # LXS services are discovered as `static` (start.sh) but are real
        # backends: give them the same env fills as Rust backends — the auth
        # backend URL and any declared <PREFIX>_BASE_URL/<PREFIX>_API_URL peer
        # dependencies.
        if [[ -n "$auth_port" && "$name" != "auth-backend" && "$name" != *-auth-backend ]]; then
          local auth_base_url="http://${dev_host}:${auth_port}/api"
          set_env "$env_file" "AUTH_BASE_URL" "$auth_base_url"
        fi
        resolve_peer_base_urls "$env_file" "$name"
        resolve_peer_public_urls "$env_file" "$name"
        echo -e "  ${GREEN}✓${RESET} $name"
        ;;
      vite|astro|nuxt|node)
        local public_api_url="http://127.0.0.1:${matched_api_port}$(api_base_path_for_backend "$matched_api_name")"
        local public_auth_url="http://127.0.0.1:${auth_port}/api"
        if is_prod_mode; then
          [[ -n "$public_api_base_url" ]] && public_api_url="$(public_api_base_url_for_backend "$matched_api_name" "$public_api_base_url")"
          [[ -n "$public_auth_base_url" ]] && public_auth_url="$public_auth_base_url"
        fi
        if [[ -n "$matched_api_port" ]]; then
          set_env "$env_file" "PUBLIC_API_URL" "$public_api_url"
        fi
        if [[ -n "$auth_port" ]]; then
          set_env "$env_file" "PUBLIC_AUTH_URL" "$public_auth_url"
        fi
        # Fill PUBLIC_SITE_URL (the estate's public origin) so SEO/OpenGraph
        # canonical URLs in frontend pages are absolute, per the frontend UX
        # principles. Eco resolves it from expose.hostname in production and
        # falls back to localhost in dev.
        if [[ -n "$public_app_origin" ]]; then
          set_env "$env_file" "PUBLIC_SITE_URL" "$public_app_origin"
        fi
        resolve_vite_public_peer_urls "$env_file" "$dev_host" "$manifest_path"
        echo -e "  ${GREEN}✓${RESET} $name"
        ;;
    esac
  done
}

# ─── Configure PM2 directory .env ───────────────────────────────────────────

configure_pm2_dir_env() {
  # Reserve INDEX_PORT in PM2_DIR if no services were discovered there
  for name in "${svc_name[@]}"; do
    if [[ "$name" == "index" || "$name" == index-* ]]; then
      return
    fi
  done
  local env_file="$PM2_DIR/.env"
  [[ ! -f "$env_file" ]] && touch "$env_file"
  set_env "$env_file" "PORT" "$INDEX_PORT"
  local shared_jwt_secret
  shared_jwt_secret="$(resolve_shared_jwt_secret)"
  set_env "$env_file" "JWT_SECRET" "$shared_jwt_secret"
  echo -e "  ${GREEN}✓${RESET} $(basename "$PM2_DIR") — reserved port ${INDEX_PORT}"
}

# ─── Generate PM2 ecosystem.config.js ──────────────────────────────────────

PM2_CONFIG_MARKER="// Generated by eco configure.sh -- do not edit by hand."

generate_pm2() {
  echo ""
  echo -e "${BOLD}Generating PM2 config in ${PM2_DIR}...${RESET}"

  CONFIG_FILE="$PM2_DIR/$(pm2_config_filename "$PM2_DIR")"

  # Before this fix, eco only ever wrote to ecosystem.config.js and never
  # touched .cjs, so a hand-authored ecosystem.config.cjs (e.g. chronic's,
  # which wires cloudflared/prod env vars eco doesn't know about) was
  # always safe by construction. Now that an ESM project's config lands on
  # .cjs too, that safety has to be explicit: never overwrite (or delete as
  # the stale-extension cleanup below would) a file that isn't ours.
  local existing
  for existing in "$PM2_DIR/ecosystem.config.js" "$PM2_DIR/ecosystem.config.cjs"; do
    if [[ -f "$existing" ]] && ! head -n1 "$existing" | grep -qF "$PM2_CONFIG_MARKER"; then
      echo -e "  ${YELLOW}!${RESET} $existing already exists and wasn't generated by eco -- leaving it alone, skipping PM2 config generation."
      CONFIG_FILE="$existing"
      return
    fi
  done

  # Drop a stale eco-generated file of the other extension so
  # find_pm2_config never picks up a leftover from a previous run (e.g.
  # package.json's "type" changed, or an older eco wrote the wrong one
  # before this fix existed). Safe unconditionally here -- the loop above
  # already bailed out if either file wasn't eco's own.
  if [[ "$CONFIG_FILE" == *.cjs ]]; then
    rm -f "$PM2_DIR/ecosystem.config.js"
  else
    rm -f "$PM2_DIR/ecosystem.config.cjs"
  fi

  local ordered_indices=()
  local type
  for type in spring-boot rust go nextjs vite astro nuxt node static; do
    for i in "${!svc_name[@]}"; do
      if [[ "${svc_type[$i]}" == "$type" ]]; then
        ordered_indices+=("$i")
      fi
    done
  done

  cat > "$CONFIG_FILE" <<EOF
$PM2_CONFIG_MARKER
EOF
  cat >> "$CONFIG_FILE" <<'EOF'
module.exports = {
  apps: [
EOF

  # Vite (and Astro, which is powered by Vite) reads this official runtime
  # override for its Host-header allowlist. It keeps expose.hostname in Eco's
  # deployment contract instead of mutating a domain's tracked config file.
  local public_hostname=""
  if is_prod_mode; then
    public_hostname="$(parse_expose_value "hostname" || true)"
  fi

  for i in "${ordered_indices[@]}"; do
    local name="${svc_name[$i]}"
    local type="${svc_type[$i]}"
    local dir="${svc_dir[$i]}"
    local port="${svc_port[$i]}"
    local app_name="${PROJECT_NAME}-${name}"

    # Parse command into script + args
    local cmd="${svc_cmd[$i]}"
    local script="${cmd%% *}"
    local args="${cmd#* }"
    [[ "$args" == "$cmd" ]] && args=""

    # Resolve script to absolute path
    local script_path interpreter
    if [[ "$script" == "serve-dist" ]]; then
      # Leptos/static frontend: serve the shipped dist/ as a static site.
      local static_start_script="$dir/.eco-static-start.sh"
      cat > "$static_start_script" <<STATICSTART
#!/bin/bash
exec python3 -m http.server ${port} --directory "\$(dirname "\$0")/dist" --bind 0.0.0.0
STATICSTART
      script_path="$static_start_script"
      args=""
      interpreter="bash"
    elif [[ "$script" == "mvn" ]]; then
      if is_prod_mode; then
        # Do not put a multi-word `bash -lc '… java …'` command in PM2's
        # string-form args. PM2's argument handling can detach the JVM from
        # the tracked shell; a later reload/delete then leaves an orphaned
        # `java -jar` holding the service port. The wrapper uses exec so the
        # PM2 child is the JVM itself, and matches the Astro/Vite wrappers
        # generated below for the same ownership reason.
        local spring_start_script="$dir/.eco-spring-boot-start.sh"
        cat > "$spring_start_script" <<'SPRINGSTART'
#!/bin/bash
set -euo pipefail

jar="$(find target -maxdepth 1 -type f -name '*.jar' ! -name 'original-*.jar' | head -n1)"
if [[ -z "$jar" ]]; then
  echo "No built jar found in target" >&2
  exit 1
fi

exec java -jar "$jar"
SPRINGSTART
        chmod 700 "$spring_start_script"
        script_path="$spring_start_script"
        args=""
        interpreter="bash"
      else
        # Maven is a shell script - use bash as interpreter
        script_path="mvn"
        args="${args}"
        interpreter="bash"
      fi
    elif [[ "$script" == "npm" ]]; then
      if [[ -f "$dir/.eco-bun" ]]; then
        # Bun-compiled node backend: the service is a self-contained linux-x64
        # single binary — run it directly, no node, no node_modules, no preview.
        local bun_name
        bun_name="$(cat "$dir/.eco-bun-name" 2>/dev/null || basename "$dir")"
        script_path="$dir/$bun_name"
        args=""
        interpreter=""
      elif is_prod_mode && [[ "$type" == "nextjs" ]]; then
        script_path="$(command -v npm)"
        args="run start -- --hostname 0.0.0.0 --port ${port}"
        interpreter=""
      elif is_prod_mode && [[ "$type" == "astro" ]]; then
        # Serve Astro's built static dist in production, never `astro dev`.
        # See ensure_astro_preview_allowed_hosts: Astro's own preview command
        # discards Vite preview config, so use its bundled Vite directly.
        # Do not invoke Vite through `npm exec`. npm creates `sh -c vite ...`
        # and PM2 can stop npm while leaving that shell/Vite child orphaned.
        # The orphan keeps the configured port, then every later PM2 restart
        # crash-loops on Vite's --strictPort error. A bash wrapper with exec
        # makes the PM2-tracked process become Vite itself.
        local astro_start_script="$dir/.eco-astro-preview.sh"
        if [[ -n "$public_hostname" ]]; then
          ensure_astro_preview_allowed_hosts "$dir" "$public_hostname"
          cat > "$astro_start_script" <<ASTROSTART
#!/bin/bash
exec node_modules/.bin/vite preview --config .eco-vite-preview.config.mjs --outDir dist --host 0.0.0.0 --port ${port} --strictPort
ASTROSTART
        else
          cat > "$astro_start_script" <<ASTROSTART
#!/bin/bash
exec node_modules/.bin/vite preview --outDir dist --host 0.0.0.0 --port ${port} --strictPort
ASTROSTART
        fi
        script_path="$astro_start_script"
        args=""
        interpreter="bash"
      elif is_prod_mode && [[ "$type" == "vite" ]]; then
        # Some Vite projects bundle a custom SSR/Node server into
        # build/index.js (a vite-plugin-node-style setup); a plain
        # client-side Vite app (e.g. chronic_bootstrap -- a Phaser game
        # with no server component) has no such thing, since its
        # production artifact is just a static dist/ folder -- correctly
        # served by Vite's own `vite preview`, not by node-running a file
        # that was never built. Checked at PM2 start time (not by testing
        # existence here at generation time, since the build step that
        # would produce build/index.js hasn't necessarily run yet when
        # this config is written) via a small generated wrapper script --
        # NOT a `bash -lc '<multi-word command>'` string, because PM2's
        # string-form `args` is naively space-split with no quote
        # awareness, which silently shreds a multi-word -lc command into
        # broken argv tokens and crash-loops the app (confirmed: this is
        # exactly what happened when this branch first shipped as a
        # string arg -- 45 restarts, immediate exit every time).
        if [[ -n "$public_hostname" ]]; then
          ensure_vite_preview_allowed_hosts "$dir" "$public_hostname"
        fi
        local vite_start_script="$dir/.eco-vite-start.sh"
        cat > "$vite_start_script" <<VITESTART
#!/bin/bash
if [ -f build/index.js ]; then
  exec node build/index.js
else
  exec node_modules/.bin/vite preview --host 0.0.0.0 --port ${port} --strictPort
fi
VITESTART
        script_path="$vite_start_script"
        args=""
        interpreter="bash"
      else
        # SvelteKit adapter-node (and vite-plugin-node) builds ship a
        # self-contained server (build/index.js) that serves its own client
        # assets from build/client. Run it directly with node — no npm
        # wrapper, no node_modules needed, and the client dir resolves
        # relative to the entry.
        if [[ "$type" == "node" && -f "$dir/build/index.js" ]]; then
          script_path="$(command -v node)"
          args="build/index.js"
          interpreter=""
        else
          script_path="$(command -v npm)"
          interpreter=""
          if [[ "$type" == "vite" || "$type" == "astro" || "$type" == "nuxt" ]]; then
            args="${args} -- --host 0.0.0.0 --port ${port}"
          elif [[ "$type" == "nextjs" ]]; then
            args="${args} -- --hostname 0.0.0.0 --port ${port}"
            ensure_next_allowed_dev_origins "$dir" || true
          fi
        fi
      fi
    elif [[ "$script" == "bash" ]]; then
      script_path="$(command -v bash)"
      interpreter=""
    elif [[ "$script" == "cargo" ]]; then
      # Production builds are made on the developer machine and shipped;
      # prefer the prebuilt artifact. Starting `cargo run` under every PM2
      # service recompiles concurrently and defeats the shared workspace/build
      # lock.
      local pkg_bin=""
      if [[ -f "$dir/Cargo.toml" ]]; then
        local raw_name
        raw_name="$(grep -m1 '^name[[:space:]]*=' "$dir/Cargo.toml" | cut -d'"' -f2 || true)"
        [[ -z "$raw_name" ]] && raw_name="$(grep -m1 '^name[[:space:]]*=' "$dir/Cargo.toml" | cut -d"'" -f2 || true)"
        if [[ -n "$raw_name" ]]; then
          # Cargo workspaces write build artifacts to the workspace target
          # directory, not necessarily <service>/target. Ask Cargo instead
          # of accidentally selecting an old copied binary from a service
          # directory (which may even have the wrong CPU architecture).
          local cargo_target_dir=""
          cargo_target_dir="$(cd "$dir" && cargo metadata --no-deps --format-version 1 2>/dev/null | node -e 'let raw=""; process.stdin.on("data", chunk => raw += chunk); process.stdin.on("end", () => { try { process.stdout.write(JSON.parse(raw).target_directory || ""); } catch {} });' 2>/dev/null || true)"
          local cargo_candidates=()
          # In prod mode only release binaries are valid. A CT can hold stale
          # debug binaries from an earlier dev/single-binary attempt; picking
          # one silently ships an old build (and even the wrong source
          # revision). If the release artifact is missing, prefer falling back
          # to `cargo` below -- the deploy builds the release binary first.
          [[ -n "$cargo_target_dir" ]] && cargo_candidates+=("$cargo_target_dir/release/$raw_name")
          if ! is_prod_mode; then
            [[ -n "$cargo_target_dir" ]] && cargo_candidates+=("$cargo_target_dir/debug/$raw_name")
            cargo_candidates+=("$PROJECT_DIR/target/debug/$raw_name" "$dir/target/debug/$raw_name" "$PROJECT_ROOT/$PROJECT_NAME/target/debug/$raw_name" "$PROJECT_ROOT/target/debug/$raw_name")
          fi
          cargo_candidates+=("$PROJECT_DIR/target/release/$raw_name" "$dir/target/release/$raw_name")
          for candidate in "${cargo_candidates[@]}"; do
            if [[ -x "$candidate" ]]; then
              pkg_bin="$candidate"
              break
            fi
          done
        fi
      fi
      if [[ -n "$pkg_bin" ]]; then
        script_path="$pkg_bin"
        args=""
        interpreter="none"
      else
        # Dev mode fallback to cargo run: honor whatever `cargo` the user's own PATH resolves
        script_path="$(command -v cargo 2>/dev/null)" || true
        if [[ -z "$script_path" && -x "$HOME/.cargo/bin/cargo" ]]; then
          script_path="$HOME/.cargo/bin/cargo"
        fi
        script_path="${script_path:-cargo}"
        interpreter="none"
      fi
    else
      script_path="$(command -v "$script" 2>/dev/null)" || script_path="$script"
      interpreter=""
    fi

    local runtime_env_extra=""
    if [[ "$type" == "vite" || "$type" == "astro" || "$type" == "nuxt" ]]; then
      runtime_env_extra=', HOST: "0.0.0.0"'
      if is_prod_mode && [[ -n "$public_hostname" ]]; then
        runtime_env_extra+=", __VITE_ADDITIONAL_SERVER_ALLOWED_HOSTS: \"$(js_escape "$public_hostname")\""
      fi
    fi

    local script_path_js args_js
    script_path_js="$(js_escape "$script_path")"
    args_js="$(js_escape "$args")"

    cat >> "$CONFIG_FILE" <<EOF
    {
      name: "${app_name}",
      cwd: "${dir}",
      script: "${script_path_js}",
      args: "${args_js}",
      exec_mode: "fork",
      env: { ${svc_port_var[$i]}: ${port}$([[ "$type" == "rust" && "${svc_port_var[$i]}" != "PORT" ]] && printf ', PORT: %s' "$port")${runtime_env_extra} },
EOF
    # When eco log dev is running, stream this app's stdout into the log FIFO.
    if [[ -n "${ECO_LOG_FIFO:-}" ]]; then
      cat >> "$CONFIG_FILE" <<EOF
      out_file: "${ECO_LOG_FIFO}",
EOF
    fi
    # Add interpreter for mvn (bash)
    if [[ -n "$interpreter" ]]; then
      cat >> "$CONFIG_FILE" <<EOF
      interpreter: "${interpreter}",
EOF
    fi
    if ! is_prod_mode && [[ "$type" == "spring-boot" ]]; then
      cat >> "$CONFIG_FILE" <<EOF
      watch: ["src/main/java/**/*.java", "src/main/resources/**/*.properties", "src/main/resources/**/*.yml"],
      watch_delay: 1500,
      ignore_watch: ["node_modules", "target"],
EOF
    fi
    cat >> "$CONFIG_FILE" <<'EOF'
    },
EOF
  done

  if [[ -n "$GATEWAY_FILE" && -n "$GATEWAY_PORT" ]]; then
    local caddy_path
    caddy_path="$(command -v caddy 2>/dev/null || true)"
    if [[ -n "$caddy_path" ]]; then
      local caddy_path_js gateway_args_js
      caddy_path_js="$(js_escape "$caddy_path")"
      gateway_args_js="$(js_escape "run --config ${GATEWAY_FILE} --adapter caddyfile")"
      cat >> "$CONFIG_FILE" <<EOF
    {
      name: "${PROJECT_NAME}-gateway",
      cwd: "${PM2_DIR}",
      script: "${caddy_path_js}",
      args: "${gateway_args_js}",
      exec_mode: "fork",
      env: { PORT: ${GATEWAY_PORT} },
EOF
      if [[ -n "${ECO_LOG_FIFO:-}" ]]; then
        cat >> "$CONFIG_FILE" <<EOF
      out_file: "${ECO_LOG_FIFO}",
EOF
      fi
      cat >> "$CONFIG_FILE" <<'EOF'
    },
EOF
    else
      echo -e "  ${YELLOW}⚠${RESET} gateway — caddy binary not found while generating PM2 config"
    fi
  fi

  cat >> "$CONFIG_FILE" <<'EOF'
  ]
};
EOF

  echo -e "  ${GREEN}✓${RESET} $CONFIG_FILE"
}

# ─── Summary ────────────────────────────────────────────────────────────────

print_summary() {
  echo ""
  echo -e "${BOLD}Done.${RESET}"
  echo ""
  echo -e "  ${CYAN}Project${RESET}        ${PROJECT_NAME}"
  if [[ -n "$GATEWAY_PORT" ]]; then
    echo -e "  ${CYAN}gateway         ${RESET} → http://localhost:${GATEWAY_PORT}"
  fi
  echo -e "  ${CYAN}$(printf "%-16s" "$(basename "$PM2_DIR")")${RESET} → http://localhost:${INDEX_PORT}"
  for i in "${!svc_name[@]}"; do
    local name="${svc_name[$i]}"
    local type="${svc_type[$i]}"
    local port="${svc_port[$i]}"
    local url
    case "$type" in
      spring-boot) url="http://localhost:${port}/api" ;;
      nextjs) url="http://localhost:${port}" ;;
      vite) url="http://localhost:${port}" ;;
      static) url="http://localhost:${port}" ;;
      *) url="http://localhost:${port}" ;;
    esac
    echo -e "  ${CYAN}$(printf "%-16s" "$name")${RESET} → ${url}"
  done
  for i in "${!svc_type[@]}"; do
    if grep -qE "^MONGODB_URI=" "${svc_env[$i]}" 2>/dev/null; then
      local db_name="${svc_name[$i]//-/_}"
      echo -e "  ${CYAN}MongoDB ${svc_name[$i]}${RESET}  mongodb://localhost:27017/${db_name}_${PROJECT_NAME}"
    fi
  done
  echo ""
  echo -e "  ${GREEN}Run: pm2 start ${CONFIG_FILE:-$(find_pm2_config "$PM2_DIR")}${RESET}"
  echo ""
}

# ─── Systemd units (Phase 3 — replaces PM2) ────────────────────────────────
#
# gated by ECO_SYSTEMD=1: after generate_pm2 writes ecosystem.config.js, parse
# the same app objects with Node and emit one eco-<app>.service per app into
# /etc/systemd/system. The deploy then uses systemctl instead of pm2
# (see up.rs: ECO_SYSTEMD=1 switches the restart path). PM2 stays in place for
# estates that haven't migrated yet.
generate_systemd() {
  [[ "${ECO_SYSTEMD:-}" == "1" ]] || return 0
  local config_file="$PM2_DIR/$(pm2_config_filename "$PM2_DIR")"
  [[ -f "$config_file" ]] || return 0
  local sysd_dir="/etc/systemd/system"
  mkdir -p "$sysd_dir"
  echo ""
  echo -e "${BOLD}Generating systemd units in ${sysd_dir}...${RESET}"
  node - "$config_file" "$sysd_dir" <<'NODE'
const fs = require('fs');
const configFile = process.argv[2];
const dir = process.argv[3];
const config = require(configFile);
for (const app of (config.apps || [])) {
  if (!app.name || !app.script) continue;
  const unit = `eco-${app.name}.service`;
  let exec = app.script;
  if (app.interpreter && app.interpreter !== 'none') exec = `${app.interpreter} ${exec}`;
  if (app.args) exec = `${exec} ${app.args}`;
  const envFile = `${app.cwd}/.env`;
  const unitContent = [
    '[Unit]',
    `Description=${app.name}`,
    'After=network.target',
    'StartLimitIntervalSec=0',
    '',
    '[Service]',
    'Type=simple',
    `WorkingDirectory=${app.cwd}`,
    fs.existsSync(envFile) ? `EnvironmentFile=${envFile}` : '',
    `ExecStart=${exec}`,
    'Restart=always',
    'RestartSec=2',
    'KillSignal=SIGTERM',
    '',
    '[Install]',
    'WantedBy=multi-user.target',
    ''
  ].filter(Boolean).join('\n');
  fs.writeFileSync(`${dir}/${unit}`, unitContent);
  console.log(`  ok ${unit}`);
}
NODE
  echo -e "  ${GREEN}✓${RESET} systemd units written (enable with: systemctl enable eco-${PROJECT_NAME}-*.service)"
}

# ─── Main ───────────────────────────────────────────────────────────────────

# If info mode, just display and exit
if [[ "$INFO_MODE" == true ]]; then
  show_info
  exit 0
fi

echo ""
echo -e "${BOLD}========================================${RESET}"
echo -e "${BOLD}  Multi-Service Project Configurator${RESET}"
echo -e "${BOLD}========================================${RESET}"

resolve_project_dir
select_pm2_dir
prompt_project_name
discover_services
condense_single_binary

if [[ ${#svc_name[@]} -eq 0 ]]; then
  echo -e "${RED}No services discovered.${RESET}"
  exit 1
fi

generate_rust_workspace
ensure_mongod_running
assign_ports
configure_envs
merge_single_binary_envs
configure_pm2_dir_env
generate_gateway_config || true
generate_pm2
generate_systemd || true
save_state
print_summary
