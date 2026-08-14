import { mkdtemp, readFile, readdir, rm, stat, writeFile } from "node:fs/promises";
import crypto from "node:crypto";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline/promises";
import { stdin as input, stdout as output } from "node:process";

import { parseCtMetadata, parseComposition, parseDeploy, parseExpose, parseProjectName, parseServices, parseStaging, parseStorage, readEcompose } from "../lib/ecompose.js";
import { parseGithubRepoCoordinates, syncGithubPushWebhook } from "../lib/github.js";
import { findRepoByName } from "../lib/repos.js";
import { runBundledScript } from "../lib/run-bundled-script.js";
import {
  cloudflaredConfigPathForAccount,
  cloudflaredServiceNameForAccount,
  ensureProxyTunnel,
  hasCloudflareApiEnv,
  overwriteDnsRecordForTunnel,
  putRemoteTunnelConfig
} from "./proxy.js";

const packageRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");

function parseOptions(args) {
  const options = {};
  const positionals = [];

  for (let i = 0; i < args.length; i += 1) {
    const arg = args[i];
    if (!arg.startsWith("--")) {
      positionals.push(arg);
      continue;
    }

    const key = arg.slice(2);
    if (key === "dry-run" || key === "staging" || key === "prod-only") {
      options[key] = true;
      continue;
    }

    const value = args[i + 1];
    if (!value || value.startsWith("--")) {
      throw new Error(`Missing value for option --${key}`);
    }
    options[key] = value;
    i += 1;
  }

  return { options, positionals };
}

function runCommand(command, args, cwd = process.cwd(), extraEnv = process.env) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd,
      stdio: "inherit",
      env: extraEnv
    });

    child.on("error", reject);
    child.on("exit", (code, signal) => {
      if (signal) {
        reject(new Error(`${command} terminated by signal ${signal}`));
        return;
      }
      if (code !== 0) {
        reject(new Error(`${command} exited with code ${code}`));
        return;
      }
      resolve();
    });
  });
}

function runCapture(command, args, cwd = process.cwd(), extraEnv = process.env) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd,
      stdio: ["ignore", "pipe", "pipe"],
      env: extraEnv
    });

    let stdout = "";
    let stderr = "";

    child.stdout.on("data", (chunk) => {
      stdout += chunk;
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk;
    });

    child.on("error", reject);
    child.on("exit", (code, signal) => {
      if (signal) {
        reject(new Error(`${command} terminated by signal ${signal}`));
        return;
      }
      resolve({ code: code ?? 1, stdout, stderr });
    });
  });
}

function isPeerDependencyResolutionError(result) {
  const text = `${result.stdout || ""}\n${result.stderr || ""}`;
  return /ERESOLVE|peer dependency|legacy-peer-deps/i.test(text);
}

function buildNet0(ct) {
  const parts = [`name=eth0`, `bridge=${ct.bridge}`, `ip=${ct.ip || "dhcp"}`];
  if (ct.gateway) {
    parts.push(`gw=${ct.gateway}`);
  }
  return parts.join(",");
}

function uniqueDomainsFromEcompose(content, project) {
  const domains = new Set([project]);
  let inDomains = false;
  let inServices = false;
  let currentService = "";

  for (const rawLine of content.split(/\r?\n/)) {
    const line = rawLine.replace(/[ \t]+$/, "");
    if (!line || /^\s*#/.test(line)) {
      continue;
    }

    if (/^domains:\s*$/.test(line)) {
      inDomains = true;
      inServices = false;
      continue;
    }

    if (/^services:\s*$/.test(line)) {
      inServices = true;
      inDomains = false;
      continue;
    }

    if (inDomains && /^[^\s].*:\s*$/.test(line)) {
      inDomains = false;
    }

    if (inDomains) {
      const match = line.match(/^  -\s*(.+)\s*$/);
      if (match) {
        const raw = match[1].replace(/^["']|["']$/g, "");
        // Entries may be a plain domain name ("- auth") or a per-estate
        // branch override ("- auth: rust-implementation") -- see
        // domainBranchOverridesFromEcompose. Either way, only the part
        // before ":" is the domain name.
        const value = raw.split(":")[0].trim();
        if (value) {
          domains.add(value);
        }
      }
      continue;
    }

    if (inServices && /^[^\s].*:\s*$/.test(line)) {
      break;
    }

    if (inServices && /^  [A-Za-z0-9_-]+:\s*$/.test(line)) {
      currentService = line.trim().slice(0, -1);
      continue;
    }

    if (inServices && currentService && /^    path:\s*(.+)\s*$/.test(line)) {
      const match = line.match(/^    path:\s*(.+)\s*$/);
      const value = match[1].replace(/^["']|["']$/g, "");
      const firstSegment = value.split("/")[0];
      if (firstSegment) {
        domains.add(firstSegment);
      }
    }
  }

  return [...domains];
}

// eco/repos.json's `branch` field is deliberately always "main" -- it's
// the shared catalog every estate composes from, not any one estate's
// working state. An individual estate (this ecompose.yml) can override
// the branch for one or more of its domains without touching that
// shared catalog, e.g. to test a feature branch in this estate only
// without affecting other estates that compose the same repo and
// expect main. Written as `- <domain>: <branch>` instead of the plain
// `- <domain>` form in the `domains:` list, or as a block entry:
//
//   - rag:
//       branch: main
//       dev: optional
//
// (The block form is also where a domain's per-environment placement is
// recorded -- see domainDevFlagsFromEcompose below.)
function domainBranchOverridesFromEcompose(content) {
  const overrides = {};
  let inDomains = false;
  let blockDomain = "";

  for (const rawLine of content.split(/\r?\n/)) {
    const line = rawLine.replace(/[ \t]+$/, "");
    if (!line || /^\s*#/.test(line)) {
      continue;
    }

    if (/^domains:\s*$/.test(line)) {
      inDomains = true;
      continue;
    }

    if (inDomains && /^[^\s].*:\s*$/.test(line)) {
      inDomains = false;
      continue;
    }

    if (!inDomains) {
      continue;
    }

    const itemMatch = line.match(/^  -\s*(.+)\s*$/);
    if (itemMatch) {
      const raw = itemMatch[1].replace(/^["']|["']$/g, "");
      const colonIndex = raw.indexOf(":");
      if (colonIndex === -1) {
        blockDomain = raw.trim();
      } else {
        blockDomain = raw.slice(0, colonIndex).trim();
        const branch = raw.slice(colonIndex + 1).trim().replace(/^["']|["']$/g, "");
        // An empty value after the colon means the domain opens a nested
        // block (`- rag:` + indented keys below); a non-empty value is the
        // legacy single-line `- <domain>: <branch>` override.
        if (branch) {
          overrides[blockDomain] = branch;
        }
      }
      continue;
    }

    // Nested keys of a block entry (`- rag:` followed by indented lines).
    if (blockDomain) {
      const branchMatch = line.match(/^ {4,}branch:\s*(.+)\s*$/);
      if (branchMatch) {
        overrides[blockDomain] = branchMatch[1].replace(/^["']|["']$/g, "").trim();
      }
    }
  }

  return overrides;
}

// A domain may be optional or disabled in local dev while remaining
// mandatory in prod. Written as a block entry in the domains: list:
//
//   domains:
//     - rag:
//         dev: optional     # include locally by default, skip gracefully if the machine can't run it
//     - photos              # no flag: required in dev AND prod (legacy behavior)
//     - mining:
//         dev: disabled     # prod-only on this estate; never provisioned locally
//
// `dev: optional` is what eco startproject / eco compose add write when the
// dev checkbox is left checked (the default). `dev: disabled` is written when
// it's unchecked. Either way prod always requires the domain.
function domainDevFlagsFromEcompose(content) {
  const flags = {};
  let inDomains = false;
  let blockDomain = "";

  for (const rawLine of content.split(/\r?\n/)) {
    const line = rawLine.replace(/[ \t]+$/, "");
    if (!line || /^\s*#/.test(line)) {
      continue;
    }

    if (/^domains:\s*$/.test(line)) {
      inDomains = true;
      continue;
    }

    if (inDomains && /^[^\s].*:\s*$/.test(line)) {
      inDomains = false;
      continue;
    }

    if (!inDomains) {
      continue;
    }

    const itemMatch = line.match(/^  -\s*(.+)\s*$/);
    if (itemMatch) {
      const raw = itemMatch[1].replace(/^["']|["']$/g, "");
      const colonIndex = raw.indexOf(":");
      if (colonIndex === -1) {
        blockDomain = raw.trim();
      } else if (raw.slice(colonIndex + 1).trim() === "") {
        blockDomain = raw.slice(0, colonIndex).trim();
      } else {
        // Single-line branch override -- no nested block follows.
        blockDomain = "";
      }
      continue;
    }

    if (!blockDomain) {
      continue;
    }

    const devMatch = line.match(/^ {4,}dev:\s*(.+)\s*$/);
    if (devMatch) {
      const value = devMatch[1].replace(/^["']|["']$/g, "").trim();
      if (value === "optional" || value === "disabled") {
        flags[blockDomain] = value;
      }
    }
  }

  return flags;
}

// Runtime tokens required by one domain, collected from the declared
// services whose `path` first segment is that domain. Matches how
// uniqueDomainsFromEcompose infers a domain from a service path.
function domainRuntimesFromServices(domain, services) {
  const runtimes = new Set();
  for (const service of services) {
    const firstSegment = String(service?.path || "").split("/")[0];
    if (firstSegment === domain) {
      for (const token of service.runtimes || []) {
        runtimes.add(token);
      }
    }
  }
  return [...runtimes];
}

// Whether a runtime token can actually run on this machine. Only the
// machine-dependent runtime (onnxruntime) is probed here; every other token
// is assumed installable by provision.sh, which stays the source of truth.
// Used to decide, for a `dev: optional` domain, whether to skip it locally
// instead of failing `eco up dev` on a runtime the machine can't provide.
async function runtimeSatisfiable(token) {
  if (token !== "onnxruntime" && token !== "onnxruntime@1.28") {
    return true;
  }
  if (process.platform === "darwin") {
    const result = await runCapture("brew", ["list", "onnxruntime"]);
    return result.code === 0;
  }
  return statExists("/opt/eco-tools/libonnxruntime.so");
}

function shellSingleQuote(value) {
  return `'${String(value).replace(/'/g, `'\\''`)}'`;
}

function sqlDatabaseNameForService(service, project) {
  if (service.name === `${project}-backend`) {
    return project;
  }
  return `${service.name.replace(/-/g, "_")}_${project}`;
}

function usesJavaDatabaseConfiguration(service) {
  return service.runtimes.includes("java@17") || service.runtimes.includes("maven");
}

function envUpsertCommand(filePath, key, value) {
  const quotedFile = shellSingleQuote(filePath);
  const quotedKey = shellSingleQuote(key);
  const quotedValue = shellSingleQuote(value);
  return [
    `touch ${quotedFile}`,
    `if grep -qE "^${key}=" ${quotedFile}; then`,
    `  sed -i "s|^${key}=.*|${key}=${value}|" ${quotedFile}`,
    "else",
    `  printf "%s\\n" "${key}=${value}" >> ${quotedFile}`,
    "fi"
  ].join("\n");
}

function envSetIfMissingCommand(filePath, key, value) {
  const quotedFile = shellSingleQuote(filePath);
  return [
    `touch ${quotedFile}`,
    `if ! grep -qE "^${key}=" ${quotedFile}; then`,
    `  printf "%s\\n" "${key}=${value}" >> ${quotedFile}`,
    "fi"
  ].join("\n");
}

function buildNpmInstallCommand(serviceDir) {
  return [
    `cd "${serviceDir}"`,
    // npm ci wipes node_modules and installs strictly from package-lock.json
    // -- reproducible, and doesn't silently drift from the lockfile the way
    // a plain `npm install` can. Falls back to `npm install` when there's no
    // lockfile at all (npm ci requires one to run).
    "if [ -f package-lock.json ]; then",
    "  if ! npm ci; then",
    "    echo '[eco up] npm ci failed, retrying with npm install --legacy-peer-deps' >&2",
    "    npm install --legacy-peer-deps",
    "  fi",
    "elif [ -f package.json ]; then",
    "  if ! npm install; then",
    "    echo '[eco up] npm install failed, retrying with --legacy-peer-deps' >&2",
    "    npm install --legacy-peer-deps",
    "  fi",
    "fi"
  ].join("\n");
}

function buildGitForceSyncCommand({ repoPath, branch, gitUrl, preservePaths = [], runtimeBranch = false }) {
  const cleanExcludes = [
    "-e .eco/",
    "-e .env",
    "-e .configure-state",
    "-e target/",
    "-e .eco-rust-hash",
    // Eco-generated PM2 start wrappers live untracked inside service dirs.
    // git clean -ffd removes them on every deploy, which crashes the PM2 app
    // ("Script not found") until a manual configure regenerates the file. Keep
    // them so a webhook deploy's configure.sh-produced ecosystem finds them.
    "-e .eco-vite-start.sh",
    "-e .eco-astro-preview.sh",
    "-e .eco-spring-boot-start.sh"
  ];
  for (const preservedPath of preservePaths) {
    cleanExcludes.push(`-e ${shellSingleQuote(`${preservedPath.replace(/\/+$/, "")}/`)}`);
  }

  const branchExpr = runtimeBranch ? `"$ECO_DEPLOY_REPO_BRANCH"` : `"${branch}"`;
  const branchResolutionLines = runtimeBranch
    ? [
        // The staging receiver forwards the pushed branch as
        // ECO_DEPLOY_TRIGGER_BRANCH. This repo must sync to that branch when
        // the push actually touched it; otherwise it stays on its own default
        // branch. Resolve at runtime rather than baking the trigger in so a
        // single estate-wide redeploy handles repos that have the feature
        // branch and repos that do not.
        `ECO_DEPLOY_REPO_BRANCH=${shellSingleQuote(branch)}`,
        `if [ -n "\${ECO_DEPLOY_TRIGGER_BRANCH:-}" ]; then`,
        `  if git ls-remote --heads origin "\${ECO_DEPLOY_TRIGGER_BRANCH}" | grep -q "refs/heads/\${ECO_DEPLOY_TRIGGER_BRANCH}"; then`,
        `    ECO_DEPLOY_REPO_BRANCH="\${ECO_DEPLOY_TRIGGER_BRANCH}"`,
        "  fi",
        "fi"
      ]
    : [];

  return [
    // A webhook deploy must be idempotent. The CT filesystem is a runtime
    // cache, not a deployment source: it may contain a prior checkout,
    // extracted files, or a manually-created directory. Initialising Git in
    // place avoids `git clone` rejecting a non-empty destination, while the
    // forced checkout below makes the selected remote branch authoritative.
    `if [ -e "${repoPath}" ] && [ ! -d "${repoPath}" ]; then rm -f "${repoPath}"; fi`,
    `mkdir -p "${repoPath}"`,
    `cd "${repoPath}"`,
    // `git rev-parse --is-inside-work-tree` is not sufficient here: a
    // missing domain .git directory can inherit the bootstrap repository
    // above it. Require this directory itself to be the worktree root before
    // we reuse Git state; otherwise initialise an independent domain repo.
    "repo_git_root=$(git rev-parse --show-toplevel 2>/dev/null || true)",
    "if [ \"$repo_git_root\" != \"$(pwd -P)\" ]; then",
    "  rm -rf .git",
    "  git init -q",
    "fi",
    `if git remote get-url origin >/dev/null 2>&1; then git remote set-url origin "${gitUrl}"; else git remote add origin "${gitUrl}"; fi`,
    ...branchResolutionLines,
    `git fetch --force --prune origin ${branchExpr}`,
    `git checkout --force -B ${branchExpr} "origin/${branchExpr}"`,
    `git reset --hard "origin/${branchExpr}"`,
    // These paths are generated by Eco and are deliberately not part of the
    // repository. Keeping them is essential when the bootstrap repository
    // itself is refreshed: the currently running webhook receiver and its
    // redeploy configuration live under .eco/deploy. The bootstrap checkout
    // also contains sibling composed repositories, so its clean command must
    // never remove those independently-synced directories.
    `git clean -ffd ${cleanExcludes.join(" ")}`
  ].join("\n");
}

// Older single-repository estates sometimes qualified their own services
// with the project name (for example `assessment/backend`).  The repository
// itself is already extracted at ctProjectRoot, so that legacy spelling must
// resolve to ctProjectRoot/backend, not a non-existent nested
// ctProjectRoot/assessment/backend.  New composed paths keep their first
// segment because it names a separately-synced domain repository.
function relativeCtServicePath(servicePath, project, projectDir) {
  const segments = String(servicePath || "").split("/").filter(Boolean);
  const selfNames = new Set([project, path.basename(projectDir)]);
  if (segments.length > 0 && selfNames.has(segments[0])) {
    segments.shift();
  }
  return segments.join("/");
}

// The project repository is extracted directly into ctProjectRoot. Every
// composed sibling domain is cloned below it so one CT can safely host
// several Eco projects: /opt/projects/<project>/<domain>/<service>.
function resolveCtServiceDir(service, { ctWorkspaceRoot, ctProjectRoot, projectDir }) {
  const relativePath = relativeCtServicePath(service.path, path.basename(ctProjectRoot), projectDir);
  if (!relativePath) {
    return ctProjectRoot;
  }
  return `${ctProjectRoot}/${relativePath}`;
}

function buildDataBootstrapPlan({ services, ctWorkspaceRoot, ctProjectRoot, projectDir, project }) {
  const commands = [];
  const hasMongo = services.some((service) => service.runtimes.includes("mongodb@7"));
  const sqlServices = services.filter((service) => service.runtimes.includes("postgresql@15"));

  if (hasMongo) {
    commands.push([
      "if command -v systemctl >/dev/null 2>&1; then",
      "  systemctl enable mongod >/dev/null 2>&1 || true;",
      "  systemctl restart mongod;",
      "elif command -v service >/dev/null 2>&1; then",
      "  service mongod restart || true;",
      "fi"
    ].join("\n"));
  }

  if (sqlServices.length > 0) {
    commands.push([
      "if command -v systemctl >/dev/null 2>&1; then",
      "  systemctl enable postgresql >/dev/null 2>&1 || true;",
      "  systemctl restart postgresql;",
      "elif command -v service >/dev/null 2>&1; then",
      "  service postgresql restart || true;",
      "fi"
    ].join("\n"));
  }

  for (const service of sqlServices) {
    const dbName = sqlDatabaseNameForService(service, project);
    const dbRole = `${project}_user`;
    const envFile = `${resolveCtServiceDir(service, { ctWorkspaceRoot, ctProjectRoot, projectDir })}/.env`;
    const quotedEnvFile = shellSingleQuote(envFile);
    const isJavaService = usesJavaDatabaseConfiguration(service);
    const commandsForService = [
      `touch ${quotedEnvFile}`,
      // DATABASE_PASSWORD is normally pinned once in ~/.bashrc on the CT
      // (operator-pinned, eco never generates or rotates it). But webhook
      // deploys run redeploy.sh without that interactive shell env, so fall
      // back to the value eco already persisted in the service .env -- it was
      // written alongside the ALTER ROLE below, so it is always the working
      // password, never a freshly generated or wrong one. A fresh estate with
      // no persisted value still fails loudly.
      "if [[ -z \"${DATABASE_PASSWORD:-}\" ]]; then",
      `  db_password=\"$(grep -E '^DATABASE_PASSWORD=' ${quotedEnvFile} 2>/dev/null | cut -d'=' -f2- | tr -d '\\r' || true)\";`,
      "  if [[ -z \"$db_password\" ]]; then",
      "    echo 'ERROR: DATABASE_PASSWORD is not exported in the shell environment and no value is persisted in the service .env. Add it to ~/.bashrc on the CT.' >&2;",
      "    exit 1;",
      "  fi",
      `  echo \"[eco up] DATABASE_PASSWORD not in shell env -- reusing the value already persisted in ${envFile} (no rotation)\";`,
      "else",
      "  db_password=\"${DATABASE_PASSWORD}\";",
      "fi",
      `sed -i '/^DATABASE_PASSWORD=/d' ${quotedEnvFile};`,
      `printf 'DATABASE_PASSWORD=%s\\n' "$db_password" >> ${quotedEnvFile};`,
      // Create the per-project role if it doesn't exist, then set its password.
      `runuser -u postgres -- psql -v ON_ERROR_STOP=1 -c "DO \\$\\$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '${dbRole}') THEN CREATE ROLE ${dbRole} WITH LOGIN; END IF; END \\$\\$;"`,
      `runuser -u postgres -- psql -v ON_ERROR_STOP=1 -c "ALTER ROLE ${dbRole} WITH LOGIN PASSWORD '$db_password';"`,
      `PGPASSWORD="$db_password" psql -h 127.0.0.1 -U ${dbRole} -d postgres -v ON_ERROR_STOP=1 -c 'SELECT 1' >/dev/null`,
      // Create the database owned by the project role.
      `runuser -u postgres -- psql -tAc "SELECT 1 FROM pg_database WHERE datname = '${dbName}'" | grep -q 1 || runuser -u postgres -- createdb -O ${dbRole} ${shellSingleQuote(dbName)}`,
      // Grant all privileges on the database and all existing + future
      // objects to the project role. This covers tables created by
      // migrations run as the postgres superuser (e.g. _sqlx_migrations)
      // which would otherwise be inaccessible to the project role.
      `runuser -u postgres -- psql -v ON_ERROR_STOP=1 -d ${shellSingleQuote(dbName)} -c "GRANT ALL PRIVILEGES ON DATABASE ${dbName} TO ${dbRole};"`,
      `runuser -u postgres -- psql -v ON_ERROR_STOP=1 -d ${shellSingleQuote(dbName)} -c "GRANT ALL ON SCHEMA public TO ${dbRole};"`,
      `runuser -u postgres -- psql -v ON_ERROR_STOP=1 -d ${shellSingleQuote(dbName)} -c "GRANT ALL PRIVILEGES ON ALL TABLES IN SCHEMA public TO ${dbRole};"`,
      `runuser -u postgres -- psql -v ON_ERROR_STOP=1 -d ${shellSingleQuote(dbName)} -c "GRANT ALL PRIVILEGES ON ALL SEQUENCES IN SCHEMA public TO ${dbRole};"`,
      `runuser -u postgres -- psql -v ON_ERROR_STOP=1 -d ${shellSingleQuote(dbName)} -c "ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT ALL ON TABLES TO ${dbRole};"`,
      `runuser -u postgres -- psql -v ON_ERROR_STOP=1 -d ${shellSingleQuote(dbName)} -c "ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT ALL ON SEQUENCES TO ${dbRole};"`,
      // DATABASE_USERNAME is Eco-managed: the per-project role
      // (<project>_user) is the only credential that matches the role and
      // password provisioned above. Force it (not set-if-missing) so a stale
      // value like "postgres" from an earlier configure can never wedge a
      // Spring service into an auth-failure restart loop.
      `sed -i '/^DATABASE_USERNAME=/d' ${quotedEnvFile};`,
      `printf 'DATABASE_USERNAME=%s\\n' "${dbRole}" >> ${quotedEnvFile};`
    ];
    if (isJavaService) {
      commandsForService.push(envSetIfMissingCommand(envFile, "DATABASE_URL", `jdbc:postgresql://localhost:5432/${dbName}`));
    } else {
      commandsForService.push([
        // PostgreSQL declared in ecompose.yml is Eco-managed. Always align
        // the URL with the role and password above.
        `sed -i '/^DATABASE_URL=/d' ${quotedEnvFile}`,
        `printf 'DATABASE_URL=postgresql://${dbRole}:%s@127.0.0.1:5432/${dbName}\\n' "$db_password" >> ${quotedEnvFile}`
      ].join("\n"));
    }
    commands.push(commandsForService.join("\n"));
  }

  // Shell-held API secrets exported on the Proxmox host (e.g.
  // DEEPSEEK_API_KEY in ~/.bashrc, same convention as BREVO_API_KEY) are
  // copied into the .env of every service that declares the key in its
  // .env.example. The operator pins the secret once on the host; eco copies
  // it per estate so service .envs stay generated state.
  for (const service of services) {
    const key = "DEEPSEEK_API_KEY";
    const value = process.env[key];
    if (!value) {
      continue;
    }
    const envFile = `${resolveCtServiceDir(service, { ctWorkspaceRoot, ctProjectRoot, projectDir })}/.env`;
    const quotedEnvFile = shellSingleQuote(envFile);
    const quotedExample = shellSingleQuote(`${envFile}.example`);
    commands.push([
      `if [ -f ${quotedExample} ] && grep -qE "^${key}=" ${quotedExample}; then`,
      `  touch ${quotedEnvFile}`,
      `  sed -i '/^${key}=/d' ${quotedEnvFile}`,
      `  printf '${key}=%s\\n' ${shellSingleQuote(value)} >> ${quotedEnvFile}`,
      "fi"
    ].join("\n"));
  }

  return commands;
}

function buildRustMigrationPlan({ services, ctWorkspaceRoot, ctProjectRoot, projectDir }) {
  return services
    .filter((service) => service.runtimes.includes("rust") && service.runtimes.includes("postgresql@15"))
    .map((service) => {
      const serviceDir = resolveCtServiceDir(service, { ctWorkspaceRoot, ctProjectRoot, projectDir });
      return [
        `if [ -d ${shellSingleQuote(`${serviceDir}/migrations`)} ]; then`,
        `  cd ${shellSingleQuote(serviceDir)}`,
        "  set -a; . ./.env; set +a",
        "  sqlx_bin=\"${ECO_SQLX_BIN:-}\"",
        "  if [[ -z \"$sqlx_bin\" ]]; then sqlx_bin=\"$(command -v sqlx 2>/dev/null || true)\"; fi",
        "  if [[ -z \"$sqlx_bin\" ]]; then",
        "    command -v cargo >/dev/null 2>&1 || { echo 'sqlx is unavailable and this CT has no Cargo; configure ECO_RUST_DEDICATED_BUILDER.' >&2; exit 1; }",
        "    cargo install sqlx-cli --no-default-features --features postgres,rustls",
        "    sqlx_bin=\"$(command -v sqlx)\"",
        "  fi",
        "  \"$sqlx_bin\" migrate run --source migrations",
        "fi"
      ].join("\n");
    });
}

// A shared CT may host several independent Eco estates.  Before replacing an
// estate's sources, cancel only Cargo compilation processes whose working
// directory belongs to one of *this* estate's Rust services.  Do not use
// pkill/pkill cargo: that would interrupt unrelated estates in the same CT.
function buildStopEstateRustBuildsCommand({ services, ctWorkspaceRoot, ctProjectRoot, projectDir }) {
  const serviceDirs = [...new Set(
    services
      .filter((service) => service.runtimes.includes("rust"))
      .map((service) => resolveCtServiceDir(service, { ctWorkspaceRoot, ctProjectRoot, projectDir }))
  )];
  if (serviceDirs.length === 0) return "true";

  const quotedDirs = serviceDirs.map((serviceDir) => shellSingleQuote(serviceDir)).join(" ");
  return [
    `set -- ${quotedDirs}`,
    "stopped_pids=()",
    "for proc in /proc/[0-9]*; do",
    "  pid=${proc#/proc/}",
    "  [ \"$pid\" = \"$$\" ] && continue",
    "  cwd=$(readlink -f \"$proc/cwd\" 2>/dev/null || true)",
    "  belongs=0",
    "  for service_dir in \"$@\"; do",
    "    case \"$cwd\" in \"$service_dir\"|\"$service_dir\"/*) belongs=1; break ;; esac",
    "  done",
    "  [ \"$belongs\" -eq 1 ] || continue",
    "  executable=$(readlink -f \"$proc/exe\" 2>/dev/null || true)",
    "  case \"$executable\" in",
    "    */rustc) ;; # child compiler of a Cargo build; stop it with its parent",
    "    */cargo|*/rustup)",
    "      arguments=$(tr '\\000' ' ' < \"$proc/cmdline\" 2>/dev/null || true)",
    "      case \"$arguments\" in *\"cargo build\"*|*\"cargo check\"*|*\"cargo test\"*|*\"cargo install\"*|*\"cargo run\"*) ;; *) continue ;; esac",
    "      ;;",
    "    *) continue ;;",
    "  esac",
    "  stopped_pids+=(\"$pid\")",
    "done",
    "if [ \"${#stopped_pids[@]}\" -eq 0 ]; then exit 0; fi",
    "echo \"[eco up] Stopping in-progress Rust build(s) for this estate: ${stopped_pids[*]}\"",
    "for pid in \"${stopped_pids[@]}\"; do kill -TERM \"$pid\" 2>/dev/null || true; done",
    "for _ in 1 2 3; do",
    "  still_running=0",
    "  for pid in \"${stopped_pids[@]}\"; do [ -d \"/proc/$pid\" ] && still_running=1; done",
    "  [ \"$still_running\" -eq 0 ] && exit 0",
    "  sleep 1",
    "done",
    "for pid in \"${stopped_pids[@]}\"; do [ -d \"/proc/$pid\" ] && kill -KILL \"$pid\" 2>/dev/null || true; done"
  ].join("\n");
}

function createPctArgs(project, ct, options) {
  const merged = { ...ct, ...options };
  return [
    "create",
    merged.id,
    merged.template,
    "--hostname",
    merged.hostname || project,
    "--cores",
    String(merged.cores || 2),
    "--memory",
    String(merged.memory || 4096),
    "--swap",
    String(merged.swap || 1024),
    "--rootfs",
    `${merged.storage}:${merged.disk}`,
    "--net0",
    buildNet0(merged),
    "--unprivileged",
    String(merged.unprivileged || 1)
  ];
}

// Kept in sync with startproject.js's DEFAULT_CT.fallbackTemplate -- not
// imported from there to avoid coupling up.js to startproject.js for one
// stable string. This is the template every Proxmox host is assumed to
// already have (it's what `pveam download` / Proxmox itself ships), used
// when a project's configured ct.template (e.g. a custom `eco ct template`
// image) isn't present on *this* particular host's storage yet.
const FALLBACK_CT_TEMPLATE = "local:vztmpl/debian-12-standard_12.12-1_amd64.tar.zst";

// Checks whether ct.template actually exists on this Proxmox host's
// storage before we try to `pct create` from it. A custom template built
// with `eco ct template` only exists on whichever host it was built on --
// scaffolding a new project elsewhere (or before that command has been
// run at all) would otherwise fail `pct create` with an opaque Proxmox
// error. Falls back to the known-present Debian base with a clear warning
// instead of hard-failing, so `eco up` stays usable out of the box.
async function resolveAvailableTemplate(requestedTemplate) {
  const match = /^([^:]+):vztmpl\/(.+)$/.exec(requestedTemplate || "");
  if (!match) {
    return requestedTemplate;
  }
  const [, templateStorage, archiveName] = match;
  const listResult = await runCapture("pvesm", ["list", templateStorage, "--content", "vztmpl"]);
  if (listResult.code === 0 && listResult.stdout.includes(archiveName)) {
    return requestedTemplate;
  }
  if (requestedTemplate === FALLBACK_CT_TEMPLATE) {
    return requestedTemplate;
  }
  process.stderr.write(
    `\n[eco up] WARNING: template "${requestedTemplate}" not found on this Proxmox host's storage -- falling back to ${FALLBACK_CT_TEMPLATE} for this CT. Build the custom template with 'eco ct template <source-ctid> --name <name> --clone-id <id>' to speed up provisioning here.\n\n`
  );
  return FALLBACK_CT_TEMPLATE;
}

function printStep(message) {
  process.stdout.write(`\n[eco up] ${message}\n`);
}

// Best-effort: pick the estate's primary frontend URL from the generated PM2
// config so the final "your app is live" line points at something openable.
async function resolveLocalFrontendUrl(configPath) {
  try {
    const { createRequire } = await import("node:module");
    const req = createRequire(import.meta.url);
    const { apps = [] } = req(configPath);
    const frontend =
      apps.find((app) => {
        const name = String(app.name || "");
        return name === "frontend" || name.endsWith("-frontend");
      }) ||
      apps.find((app) => {
        const haystack = `${app.args || ""} ${app.script || ""}`;
        return /(next dev|vite|astro dev|npm run dev)/.test(haystack);
      });
    if (!frontend) {
      return null;
    }
    const port = frontend.env?.PORT || frontend.env?.SERVER_PORT;
    if (!port) {
      return null;
    }
    return `http://localhost:${port}`;
  } catch {
    return null;
  }
}

function openInBrowser(url) {
  try {
    const opener =
      process.platform === "darwin" ? "open"
      : process.platform === "win32" ? "cmd"
      : "xdg-open";
    const args = process.platform === "win32" ? ["/c", "start", "", url] : [url];
    const child = spawn(opener, args, { stdio: "ignore", detached: true });
    child.unref();
  } catch {}
}

function toBool(value) {
  return /^(1|true|yes|on)$/i.test(String(value || ""));
}

function deriveWebhookHostname(appHostname) {
  if (!appHostname) {
    return "";
  }

  // The webhook hostname must resolve under the estate's own DNS zone so eco
  // can create a CNAME for it, AND it must be covered by Cloudflare Universal
  // SSL. Universal SSL covers the apex and ONE label below it (`*.zone`), so
  // the webhook hostname must be a single-level subdomain of the zone -- never
  // a nested one.
  //
  //   apex        stuff8.com                  -> hooks.stuff8.com          (single level, works)
  //   subdomain   assessment.jogjaitcamp.com  -> hooks-assessment.jogjaitcamp.com  (single level, works)
  //   BROKEN      eco.stuff8.com              -> hooks.eco.stuff8.com      (2 levels: no cert, TLS handshake failure)
  //
  // For an apex hostname prefix `hooks.` (a single label). For a subdomain
  // hostname, replace the leftmost label with `hooks-<label>` so the result
  // stays one label under the zone (e.g. `hooks-eco.stuff8.com`), matching the
  // working assessment/apindo convention instead of nesting two levels deep.
  const labels = String(appHostname).replace(/^\.+|\.+$/g, "").split(".");
  if (labels.length <= 2) {
    return `hooks.${appHostname}`;
  }
  const [head, ...rest] = labels;
  return `hooks-${head}.${rest.join(".")}`;
}

// The staging hostname is the apex-safe `staging-` derivation of the prod
// hostname, mirroring deriveWebhookHostname's single-level-under-the-zone
// rule so Cloudflare Universal SSL still covers it:
//
//   apex        stuff8.com                  -> staging.stuff8.com
//   subdomain   assessment.jogjaitcamp.com  -> staging-assessment.jogjaitcamp.com
function deriveStagingHostname(appHostname) {
  if (!appHostname) {
    return "";
  }
  const labels = String(appHostname).replace(/^\.+|\.+$/g, "").split(".");
  if (labels.length <= 2) {
    return `staging.${appHostname}`;
  }
  const [head, ...rest] = labels;
  return `staging-${head}.${rest.join(".")}`;
}

// The staging CT hosts a second copy of the same estate, but configure.sh and
// the gateway must be driven by a manifest whose hostname and CT id describe
// the staging footprint -- not the prod one that shipped with the source. This
// rewrites the two lines that differ (expose.hostname and ct.id) and stamps the
// file so the staging CT's .env and ecosystem config derive staging URLs.
function deriveStagingEcomposeContent(content, stagingConfig, stagingHostname) {
  const rewritten = [];
  let inExpose = false;
  let inCt = false;
  for (const rawLine of content.split(/\r?\n/)) {
    const line = rawLine.replace(/[ \t]+$/, "");
    if (/^ct:\s*$/.test(line)) {
      inCt = true;
      rewritten.push(line);
      continue;
    }
    if (/^expose:\s*$/.test(line)) {
      inExpose = true;
      rewritten.push(line);
      continue;
    }
    if (inCt && !/^  /.test(line)) {
      inCt = false;
    }
    if (inExpose && !/^  /.test(line)) {
      inExpose = false;
    }
    if (inCt) {
      const fieldMatch = line.match(/^  ([A-Za-z0-9_-]+):\s*(.+)\s*$/);
      if (fieldMatch && fieldMatch[1] === "id") {
        rewritten.push(`  id: ${stagingConfig.ct}`);
        continue;
      }
      rewritten.push(line);
      continue;
    }
    if (inExpose) {
      const fieldMatch = line.match(/^  ([A-Za-z0-9_-]+):\s*(.+)\s*$/);
      if (fieldMatch && fieldMatch[1] === "hostname") {
        rewritten.push(`  hostname: ${stagingHostname}`);
        continue;
      }
      rewritten.push(line);
      continue;
    }
    rewritten.push(line);
  }
  const base = rewritten.join("\n");
  const marker = `# staging footprint: deployed to CT ${stagingConfig.ct} at ${stagingHostname}\n# All other settings mirror the prod manifest from which this was derived.\n`;
  return `${marker}${base}\n`;
}



export function resolveDeployGithubConfig({ project, expose, deploy }) {
  const github = deploy?.github || {};
  if (!toBool(github.enabled)) {
    return null;
  }

  const scheme = github.scheme || expose.scheme || "https";
  const derived = deriveWebhookHostname(expose.hostname);
  const hostname = github.webhook_hostname || derived;
  // Legacy eco versions derived the webhook hostname as `hooks.<appHostname>`
  // unconditionally -- a nested two-level form (`hooks.eco.stuff8.com`) that
  // Cloudflare Universal SSL does not cover, so GitHub webhooks failed with a
  // TLS handshake error. When the current derivation differs from that legacy
  // form, hand it to the webhook sync so any stale broken hook is purged.
  // Only applies to derived hostnames: an explicit webhook_hostname is the
  // operator's own choice and must never trigger a purge.
  const legacyHostname = !github.webhook_hostname && expose.hostname
    ? `hooks.${String(expose.hostname).replace(/^\.+|\.+$/g, "")}`
    : "";
  // Ports belong to an estate, not the manifest: a static webhook port
  // collides as soon as two estates share a CT. It is allocated later, once
  // the CT is available, and persisted in github-webhook.json.
  const port = /^\d+$/.test(String(github.webhook_port || ""))
    && Number(github.webhook_port) >= 20000
    && Number(github.webhook_port) <= 27999
    ? Number(github.webhook_port)
    : null;
  const debounceMs = /^\d+$/.test(String(github.debounce_ms || ""))
    ? Number(github.debounce_ms)
    : 15000;
  const pathName = github.webhook_path || "/__eco/github/deploy";
  const branch = github.branch || "main";
  const proxyCtInput = github.proxy_ct || expose.proxy_ct || expose.proxy_ctid || expose.via;

  if (!hostname) {
    throw new Error(`Deploy webhook is enabled for ${project}, but no hostname could be resolved.`);
  }

  if (!proxyCtInput) {
    throw new Error(`Deploy webhook is enabled for ${project}, but no proxy CT is configured.`);
  }

  return {
    branch,
    debounceMs,
    path: pathName.startsWith("/") ? pathName : `/${pathName}`,
    port,
    proxyCtInput,
    scheme,
    webhookHostname: hostname,
    staleWebhookHostname: legacyHostname !== hostname ? legacyHostname : "",
    webhookUrl: `${scheme}://${hostname}${pathName.startsWith("/") ? pathName : `/${pathName}`}`
  };
}

async function resolveEstateWebhookPort({ ctid, ctProjectRoot, githubDeploy }) {
  if (Number.isInteger(githubDeploy.port)) return githubDeploy.port;
  const configPath = `${ctProjectRoot}/.eco/deploy/github-webhook.json`;
  const existing = await pctExecCapture(ctid, `node -e ${JSON.stringify(`try { const value = require(${JSON.stringify(configPath)}).port; if (Number.isInteger(value) && value >= 20000 && value <= 27999) process.stdout.write(String(value)); } catch {}`)}`);
  const saved = Number(existing.trim());
  if (Number.isInteger(saved) && saved >= 20000 && saved <= 27999) return saved;
  for (let attempt = 0; attempt < 200; attempt += 1) {
    const candidate = crypto.randomInt(20000, 28000);
    const listeners = await pctExecCapture(ctid, `ss -ltn 'sport = :${candidate}' | tail -n +2`);
    if (!listeners.trim()) return candidate;
  }
  throw new Error(`Could not allocate a free private webhook port for ${githubDeploy.webhookHostname}.`);
}

// The webhook HMAC secret must stay stable across `eco up` runs: it is also
// PATCHed into the GitHub hooks, and re-rolling it on every run silently
// breaks the estate's CI/CD whenever that sync step fails (a push then comes
// back signature-invalid). Reuse the secret already persisted in the estate's
// github-webhook.json when present, exactly like the webhook port above.
async function resolveEstateWebhookSecret({ ctid, ctProjectRoot, fallback }) {
  const configPath = `${ctProjectRoot}/.eco/deploy/github-webhook.json`;
  const existing = await pctExecCapture(ctid, `node -e ${JSON.stringify(`try { const value = require(${JSON.stringify(configPath)}).secret; if (typeof value === "string" && value.length >= 16) process.stdout.write(value); } catch {}`)}`);
  const saved = existing.trim();
  if (saved) return saved;
  return fallback;
}

// uniqueDomainsFromEcompose infers a "domain" from every services: entry's
// path first segment, including one that self-references the root/bootstrap
// project's own directory (e.g. chronic_bootstrap declaring a service at
// path: chronic_bootstrap) -- that domain was never meant to need a
// repos.json catalog entry, since it isn't a separate repo to clone at all.
// `project` (ecompose.yml's clean project: name, e.g. "chronic") only
// catches this when it happens to match the directory name; comparing
// against the actual projectDir basename (e.g. "chronic_bootstrap") catches
// it even when the two differ, which is the normal case for a manifest
// dir that also declares itself as a service.
function isSelfDomain(domain, project, projectDir) {
  return domain === project || domain === path.basename(projectDir);
}

// Resolves a domain's git remote. Shared domains come from the repos.json
// catalog. A `_composition` domain is the estate's own project repo, which
// is deliberately NOT committed to repos.json (the catalog is for reusable
// domains); its git address lives in ecompose.yml's `composition:` block.
// For backward compatibility, repos.json is still checked first, so estates
// registered before the block existed (or that kept a catalog entry) keep
// working. Returns { git, branch } or null.
async function resolveDomainGit(domain, { project, content }) {
  const repo = await findRepoByName(domain);
  if (repo?.git) {
    return { git: repo.git, branch: repo.branch || "main" };
  }

  if (domain === `${project}_composition`) {
    const composition = parseComposition(content);
    if (composition?.git) {
      return { git: composition.git, branch: composition.branch || "main" };
    }
  }

  return null;
}

async function resolveProjectGitRemote(projectDir, project) {
  const result = await runCapture("git", ["config", "--get", "remote.origin.url"], projectDir);
  const remote = result.stdout.trim();
  if (result.code !== 0 || !remote) {
    throw new Error(
      `Deploy webhook is enabled for ${project}, but no git remote origin could be resolved from ${projectDir}.`
    );
  }
  return remote;
}

async function resolveDeployGithubReposForProject({ domains, project, projectDir, branch, domainBranchOverrides = {}, content }) {
  const repos = [];
  for (const domain of domains) {
    if (isSelfDomain(domain, project, projectDir)) {
      const git = await resolveProjectGitRemote(projectDir, project);
      repos.push({
        domain,
        git,
        branch: branch || "main",
        ...parseGithubRepoCoordinates(git)
      });
      continue;
    }

    const repo = await resolveDomainGit(domain, { project, content });
    if (!repo?.git) {
      throw new Error(`No git remote found for domain "${domain}" (not in eco/repos.json and no composition: block in ecompose.yml)`);
    }

    repos.push({
      domain,
      git: repo.git,
      branch: domainBranchOverrides[domain] || repo.branch || "main",
      ...parseGithubRepoCoordinates(repo.git)
    });
  }

  return repos;
}

function buildDeployReceiverFiles({
  project,
  projectDir,
  ctProjectRoot,
  ctProjectParent,
  ctWorkspaceRoot,
  ctEcoRoot,
  ctConfigPath,
  githubDeploy,
  githubRepos,
  webhookSecret,
  dependencyInstallSteps,
  dataBootstrapSteps,
  migrationSteps,
  buildSteps,
  services,
  usesDedicatedRustBuilder,
  rustBuilderIsApplication = false,
  staging = false,
  stagingEcomposeContent = ""
}) {
  const deployRoot = `${ctProjectRoot}/.eco/deploy`;
  const deployScriptPath = `${deployRoot}/redeploy.sh`;
  const configPath = `${deployRoot}/github-webhook.json`;
  // Same filename ctConfigPath already resolved (see isEsmProject/
  // loadProjectDeployment) -- the redeploy script below requires() the
  // main app's config by this exact basename, so it has to agree.
  const mainPm2ConfigName = ctConfigPath.split("/").pop();
  const webhookPm2ConfigName = mainPm2ConfigName.endsWith(".cjs")
    ? "webhook-ecosystem.config.cjs"
    : "webhook-ecosystem.config.js";
  const pm2ConfigPath = `${deployRoot}/${webhookPm2ConfigName}`;

  const lines = [
    "#!/bin/bash",
    "set -euo pipefail",
    "",
    `PROJECT_ROOT=${shellSingleQuote(ctProjectRoot)}`,
    `WORKSPACE_ROOT=${shellSingleQuote(ctWorkspaceRoot)}`,
    `ECO_ROOT=${shellSingleQuote(ctEcoRoot)}`,
    `PROJECT_NAME=${shellSingleQuote(project)}`,
    `DEPLOY_LOCK=${shellSingleQuote(`${ctProjectRoot}/.eco/deploy/redeploy.lock`)}`,
    "mkdir -p \"$(dirname \"$DEPLOY_LOCK\")\"",
    "exec 9>\"$DEPLOY_LOCK\"",
    "if ! flock -n 9; then echo \"[eco deploy] another deploy owns this estate lock; exiting\" >&2; exit 0; fi",
    "# Keep a small, configurable buffer so a CT never reaches emergency read-only mode.",
    "# Only disposable logs and debug build outputs are reclaimed automatically; release",
    "# artifacts remain intact so the currently running application is always restartable.",
    "ECO_MIN_FREE_MB=${ECO_MIN_FREE_MB:-4096}",
    "free_kb() { df -Pk \"$PROJECT_ROOT\" | awk 'NR == 2 { print $4 }'; }",
    "reclaim_safe_cache() {",
    "  echo \"[eco deploy] low disk space; reclaiming safe cache for $PROJECT_NAME\" >&2",
    "  find \"$PROJECT_ROOT\" -type d -path '*/target/debug' -prune -print -exec rm -rf {} +",
    "  find \"$PROJECT_ROOT\" -type d -path '*/target/incremental' -prune -print -exec rm -rf {} +",
    "  find \"$HOME/.pm2/logs\" -type f -size +10M -print -exec truncate -s 0 {} + 2>/dev/null || true",
    "}",
    "MIN_FREE_KB=$((ECO_MIN_FREE_MB * 1024))",
    "if [ \"$(free_kb)\" -lt \"$MIN_FREE_KB\" ]; then reclaim_safe_cache; fi",
    "if [ \"$(free_kb)\" -lt \"$MIN_FREE_KB\" ]; then",
    "  echo \"[eco deploy] refusing deploy: less than ${ECO_MIN_FREE_MB}MB free after safe cleanup. Reclaim release build cache during a maintenance window.\" >&2",
    "  exit 1",
    "fi",
    "TRIGGER_REPO=${1:-manual}",
    "echo \"[eco deploy] trigger: ${TRIGGER_REPO}\"",
    ""
  ];

  const composedDomainPaths = githubRepos
    .filter((repo) => !isSelfDomain(repo.domain, project, projectDir))
    .map((repo) => repo.domain);

  for (const repo of githubRepos) {
    // Keep every composed domain inside this estate root. Production CTs may
    // host multiple projects, so /opt/projects/<domain> is not safe.
    const repoPath = isSelfDomain(repo.domain, project, projectDir) ? ctProjectRoot : `${ctProjectRoot}/${repo.domain}`;
    lines.push(
      `# sync repo: ${repo.domain}`,
      buildGitForceSyncCommand({
        repoPath,
        branch: repo.branch,
        gitUrl: repo.git,
        // The bootstrap repository lives at ctProjectRoot and its composed
        // domains are child directories there. They are separate Git sources
        // of truth and are synced by their own loop iteration below.
        preservePaths: isSelfDomain(repo.domain, project, projectDir) ? composedDomainPaths : [],
        // A staging deploy must check out the pushed branch when the repo has
        // it, and fall back to its default branch otherwise.
        runtimeBranch: staging
      }),
      ""
    );
  }

  // The bootstrap force-sync above restores the *prod* ecompose.yml from git.
  // On the staging footprint the running manifest must carry the staging
  // hostname and CT id (the gateway and configure.sh derive public URLs from
  // it), so re-apply the staging manifest after every repo sync.
  if (staging && stagingEcomposeContent) {
    lines.push(
      "# re-apply staging ecompose.yml (repo sync restored the prod manifest)",
      `install -D -m 0644 ${shellSingleQuote(`${deployRoot}/staging-ecompose.yml`)} ${shellSingleQuote(`${ctProjectRoot}/ecompose.yml`)}`,
      ""
    );
  }

  for (const step of dependencyInstallSteps) {
    lines.push(`# install deps: ${step.name}`, step.command, "");
  }

  // SQLx query macros inspect the live schema at compile time. Bootstrap and
  // migrate before configure.sh runs the Rust test gate so a fresh estate
  // never compiles against an empty database.
  for (const command of dataBootstrapSteps) {
    lines.push("# bootstrap data services", command, "");
  }
  for (const command of migrationSteps) {
    lines.push(
      "# apply Rust database migrations",
      `${usesDedicatedRustBuilder ? "export ECO_SQLX_BIN=/opt/eco-tools/sqlx; " : ""}${command}`,
      ""
    );
  }

  lines.push(
    "# refresh eco CLI inside CT",
    `cd ${shellSingleQuote(ctEcoRoot)}`,
    "npm install",
    "npm link",
    "",
    "# regenerate estate configuration (also runs each rust service's",
    "# integration tests and records any failures -- see",
    "# run_test_gates_before_deploy in configure.sh)",
    `cd ${shellSingleQuote(ctWorkspaceRoot)}`,
    `ECO_DEPLOY_MODE=prod ECO_NON_INTERACTIVE=1 ECO_RUN_TESTS_BEFORE_DEPLOY=1 PROJECT_DIR=${shellSingleQuote(ctProjectRoot)} PROJECT_NAME=${shellSingleQuote(project)} PM2_DIR=${shellSingleQuote(ctProjectRoot)} bash ${shellSingleQuote(`${ctEcoRoot}/configure.sh`)}`,
    ""
  );

  for (const step of buildSteps) {
    lines.push(`# build artifact: ${step.name}`, step.command, "");
  }

  // Rebuild Rust services whose source changed since the last deploy.
  // Uses the same hash-based skip logic as `eco up` so unchanged services
  // are never recompiled unnecessarily.
  //
  // When target_mode is single-binary, all Rust domains are merged into
  // one binary built from the project-level shim crate (*_binary/). The
  // workspace-level Cargo.toml must include the shim as a member.
  const targetMode = (services._targetMode || "").trim();
  if (targetMode === "single-binary") {
    const binaryName = `${project}-binary`;
    lines.push(
      `# ── single-binary mode: build ${binaryName} from workspace root ──`,
      "# All Rust domains are compiled together into one binary that merges",
      "# every domain router via tower::Steer dispatch on a single port.",
      `if [ -d ${shellSingleQuote(`${ctProjectRoot}/${project}_binary`)} ] || [ -d ${shellSingleQuote(`${ctProjectRoot}/stuff8_binary`)} ]; then`,
      "  SHIM_DIR=\"\"",
      `  for d in ${shellSingleQuote(`${ctProjectRoot}/${project}_binary`)} ${shellSingleQuote(`${ctProjectRoot}/stuff8_binary`)}; do`,
      "    if [ -d \"$d\" ] && [ -f \"$d/Cargo.toml\" ]; then SHIM_DIR=\"$d\"; break; fi",
      "  done",
      `  cd ${shellSingleQuote(ctProjectRoot)}`,
      `  ${buildConditionalRustCommand(`${ctProjectRoot}`, `RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo PATH=/usr/local/cargo/bin:\\$PATH RUSTC_WRAPPER= cargo build --release -p ${shellSingleQuote(binaryName)}`)}`,
      "else",
      `  echo \"[eco deploy] single-binary mode but no *_binary/ crate found — skipping Rust build\" >&2`,
      "fi",
      ""
    );
  } else {
    const rustServices = (services || []).filter((s) => s.runtimes && s.runtimes.includes("rust") && s.path);
    if (usesDedicatedRustBuilder && !rustBuilderIsApplication) {
      // External dedicated Rust builder: prod/app CT has no Rust toolchain
      // (ECO_RUST_DEDICATED_BUILDER points at another CT). Building Rust here
      // would fail, so skip it. The operator runs `eco up` on the Proxmox host
      // with the builder env; eco builds on the builder CT and transfers the
      // release binaries to this CT. This deploy just syncs source and
      // restarts services with the already-shipped binaries.
      lines.push(
        "# ── external dedicated Rust builder ──",
        "# Rust artifacts are built on the dedicated builder CT and shipped to",
        "# this CT by host-side `eco up` (ECO_RUST_DEDICATED_BUILDER). The webhook",
        "# redeploy does not compile Rust; it only syncs source and restarts",
        "# services against the shipped release binaries.",
        `for _svc in ${rustServices.map((s) => s.name).join(" ")}; do`,
        `  echo "[eco deploy] \${_svc}: Rust built externally on ECO_RUST_DEDICATED_BUILDER; using shipped release binary"`,
        "done",
        ""
      );
    } else {
      for (const service of rustServices) {
        const serviceDir = `${ctProjectRoot}/${service.path}`;
        const buildCommand = `cd ${shellSingleQuote(serviceDir)} && RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo PATH=/usr/local/cargo/bin:$PATH RUSTC_WRAPPER= cargo build --release`;
        lines.push(
          `# rebuild Rust service if source changed: ${service.name}`,
          buildConditionalRustCommand(serviceDir, buildCommand),
          ""
        );
      }
    }
  }

  lines.push(
    "# restart app services -- skip any service whose integration tests",
    "# just failed (see run_test_gates_before_deploy in configure.sh), so a",
    "# broken domain keeps running its last-good build instead of either",
    "# blocking the rest of the estate's deploy or being torn down with no",
    "# working replacement.",
    `cd ${shellSingleQuote(ctProjectRoot)}`,
    "# Host env (BREVO_API_KEY/MAIL_FROM_*) is operator fallback for .env generation,",
    "# never a source for service process env. Unset so services read their own .env",
    "# (where an ecompose.yml-declared sender wins) instead of inheriting host values.",
    "unset BREVO_API_KEY MAIL_FROM_EMAIL MAIL_FROM_NAME 2>/dev/null || true",
    `FAILED_SERVICES_FILE=${shellSingleQuote(`${ctProjectRoot}/.eco-test-failures`)}`,
    "FAILED_SERVICES=\"\"",
    "if [ -s \"$FAILED_SERVICES_FILE\" ]; then",
    "  FAILED_SERVICES=$(tr '\\n' ' ' < \"$FAILED_SERVICES_FILE\")",
    "  echo \"[eco deploy] skipping (tests failed): ${FAILED_SERVICES}\" >&2",
    "fi",
    `ALL_APPS=$(node -e "console.log(require('./${mainPm2ConfigName}').apps.map(a => a.name).join(' '))")`,
    "RESTART_APPS=\"\"",
    "for app in $ALL_APPS; do",
    "  skip=0",
    "  for failed in $FAILED_SERVICES; do",
    "    if [ \"$app\" = \"$failed\" ]; then skip=1; fi",
    "  done",
    "  if [ \"$skip\" -eq 0 ]; then RESTART_APPS=\"$RESTART_APPS $app\"; fi",
    "done",
    "if [ -n \"$RESTART_APPS\" ]; then",
    "  RESTART_LIST=$(echo \"$RESTART_APPS\" | tr ' ' ',' | sed 's/^,//')",
    `  pm2 startOrReload ${mainPm2ConfigName} --update-env --only "$RESTART_LIST"`,
    "else",
    "  echo \"[eco deploy] no services passed their test gate, nothing to restart\" >&2",
    "fi",
    `pm2 startOrReload ${shellSingleQuote(pm2ConfigPath)} --update-env`,
    ""
  );

  const receiverConfig = {
    branch: githubDeploy.branch,
    // Prod receivers deploy only pushes to the estate's single deploy branch.
    // Staging receivers accept any branch except that prod branch, so a push
    // from the simplified git flow triggers a staging redeploy instead.
    branchPolicy: staging ? "any-except-main" : "fixed",
    debounceMs: githubDeploy.debounceMs,
    deployCommand: deployScriptPath,
    path: githubDeploy.path,
    port: githubDeploy.port,
    project,
    projectRoot: ctProjectRoot,
    repos: githubRepos.map((repo) => ({
      branch: githubDeploy.branch,
      domain: repo.domain,
      fullName: repo.fullName
    })),
    secret: webhookSecret
  };

  const pm2Config = `module.exports = {
  apps: [
    {
      name: ${JSON.stringify(`${project}-${staging ? "staging-" : ""}deploy-webhook`)},
      cwd: ${JSON.stringify(ctProjectRoot)},
      script: ${JSON.stringify(`${ctEcoRoot}/src/runtime/github-webhook-receiver.js`)},
      interpreter: "node",
      env: {
        ECO_WEBHOOK_CONFIG: ${JSON.stringify(configPath)},
        ECO_WEBHOOK_PORT: ${JSON.stringify(String(githubDeploy.port))}
      }
    }
  ]
};
`;

  return {
    configPath,
    deployRoot,
    deployScriptPath,
    files: {
      [configPath]: `${JSON.stringify(receiverConfig, null, 2)}\n`,
      [deployScriptPath]: `${lines.join("\n")}\n`,
      [pm2ConfigPath]: pm2Config,
      ...(staging && stagingEcomposeContent
        ? { [`${deployRoot}/staging-ecompose.yml`]: stagingEcomposeContent }
        : {})
    },
    pm2ConfigPath
  };
}

function detectServiceType(serviceDir, packageJsonText) {
  if (packageJsonText.includes('"next"')) {
    return "nextjs";
  }
  if (packageJsonText.includes('"astro"')) {
    return "astro";
  }
  if (packageJsonText.includes('"vite"')) {
    return "vite";
  }
  return "node";
}

async function discoverLocalServices(estateRoot, scope = {}) {
  const services = [];

  async function scanDir(scanPath, label, relPath = "") {
    const pomPath = path.join(scanPath, "pom.xml");
    const cargoPath = path.join(scanPath, "Cargo.toml");
    const goModPath = path.join(scanPath, "go.mod");
    const pkgPath = path.join(scanPath, "package.json");

    if (await statExists(pomPath)) {
      services.push({
        name: relPath ? `${label}-${relPath.replaceAll("/", "-")}` : label,
        type: "spring-boot",
        dir: scanPath
      });
      return;
    }

    if (await statExists(cargoPath)) {
      services.push({
        name: relPath ? `${label}-${relPath.replaceAll("/", "-")}` : label,
        type: "rust",
        dir: scanPath
      });
      return;
    }

    if (await statExists(goModPath)) {
      services.push({
        name: relPath ? `${label}-${relPath.replaceAll("/", "-")}` : label,
        type: "go",
        dir: scanPath
      });
      return;
    }

    if (await statExists(pkgPath)) {
      const packageJson = await readFile(pkgPath, "utf8");
      services.push({
        name: relPath ? `${label}-${relPath.replaceAll("/", "-")}` : label,
        type: detectServiceType(scanPath, packageJson),
        dir: scanPath
      });
      return;
    }

    const entries = await readdir(scanPath, { withFileTypes: true }).catch(() => []);
    for (const entry of entries) {
      if (!entry.isDirectory()) {
        continue;
      }
      if (["node_modules", "target", ".next", ".git"].includes(entry.name)) {
        continue;
      }
      const childRel = relPath ? `${relPath}/${entry.name}` : entry.name;
      await scanDir(path.join(scanPath, entry.name), label, childRel);
    }
  }

  const topLevel = await readdir(estateRoot, { withFileTypes: true });
  for (const entry of topLevel) {
    if (!entry.isDirectory()) {
      continue;
    }
    await scanDir(path.join(estateRoot, entry.name), entry.name);
  }

  // Dev-mode discovery must only build/install the current estate's own
  // services. A shared workspace -- or a container hosting several estates
  // (e.g. /opt/projects/{apindo,assessment,stuff8}) -- otherwise turns every
  // sibling project into a "discovered service", including other estates,
  // virtual-workspace manifests, and the eco repo itself, all of which dev
  // mode would then try to compile.
  if (!scope.declaredServices || !scope.declaredServices.length) {
    return services;
  }
  const allowed = estateServiceDirs(scope.declaredServices, {
    estateRoot,
    projectDir: scope.projectDir,
    project: scope.project
  });
  return services.filter((service) => allowed.has(path.resolve(service.dir)));
}

// Maps the estate's declared service paths (ecompose `services:` entries) to
// concrete directories in either dev layout: domains checked out as siblings
// of the bootstrap repo (<estateRoot>/<domain>/...) on a dev machine, or, in
// a container, as children of the deployed project root
// (<projectDir>/<domain>/...). Both candidates are accepted so the same
// declared paths resolve regardless of which layout the workspace uses.
function estateServiceDirs(declaredServices, { estateRoot, projectDir, project }) {
  const dirs = new Set([path.resolve(projectDir)]);
  for (const service of declaredServices) {
    if (!service.path) {
      continue;
    }
    const relative = relativeCtServicePath(service.path, project, projectDir) || service.path;
    dirs.add(path.resolve(path.join(estateRoot, relative)));
    dirs.add(path.resolve(path.join(projectDir, relative)));
  }
  return dirs;
}

async function statExists(filePath) {
  try {
    await stat(filePath);
    return true;
  } catch {
    return false;
  }
}

// Mirrors configure.sh's find_pm2_config: an ESM project (package.json
// "type": "module") gets its PM2 config generated as ecosystem.config.cjs
// instead of .js (Node treats a plain .js as ESM under "type": "module",
// which breaks PM2's/our own require()-based loading of it -- PM2's own
// docs recommend .cjs for exactly this case). .cjs takes priority when
// both happen to exist since that's what a fresh ESM-aware configure.sh
// run produces.
async function resolveLocalPm2ConfigPath(dir) {
  const cjsPath = path.join(dir, "ecosystem.config.cjs");
  if (await statExists(cjsPath)) {
    return cjsPath;
  }
  return path.join(dir, "ecosystem.config.js");
}

async function ensureLocalDomainRepos(estateRoot, domains, project, domainBranchOverrides = {}, content) {
  for (const domain of domains) {
    if (domain === project) {
      continue;
    }
    // A domain only needs a git remote to know where to clone it FROM. If
    // it's already checked out here, that question never comes up -- most
    // notably the root/bootstrap project's own directory, which
    // uniqueDomainsFromEcompose infers as a "domain" whenever a services:
    // entry's path self-references it (e.g. chronic_bootstrap declaring a
    // service at path: chronic_bootstrap), even though a root project repo
    // isn't meant to be a reusable catalog entry at all. Only repos
    // genuinely missing from disk need the remote to clone them.
    const targetPath = path.join(estateRoot, domain);
    if (await statExists(path.join(targetPath, ".git"))) {
      continue;
    }

    const repo = await resolveDomainGit(domain, { project, content });
    if (!repo) {
      throw new Error(`No git remote found for domain "${domain}" (not in eco/repos.json and no composition: block in ecompose.yml)`);
    }
    const branch = domainBranchOverrides[domain] || repo.branch;
    if (await statExists(targetPath)) {
      throw new Error(`Refusing to clone ${domain} into existing non-git path: ${targetPath}`);
    }
    printStep(`Cloning repo: ${domain}${branch !== repo.branch ? ` (branch override: ${branch})` : ""}`);
    await runCommand("git", ["clone", "--branch", branch, repo.git, targetPath], estateRoot);
  }
}

async function installLocalDependencies(services) {
  for (const service of services) {
    if (service.type !== "nextjs" && service.type !== "vite" && service.type !== "node") {
      continue;
    }
    const packageLockPath = path.join(service.dir, "package-lock.json");
    const packageJsonPath = path.join(service.dir, "package.json");
    if (!(await statExists(packageJsonPath))) {
      continue;
    }
    printStep(`Installing npm dependencies: ${service.name}`);
    const installResult = await runCapture("npm", ["install"], service.dir);
    if (installResult.code !== 0) {
      if (isPeerDependencyResolutionError(installResult)) {
        printStep(`Retrying npm install with --legacy-peer-deps: ${service.name}`);
        await runCommand("npm", ["install", "--legacy-peer-deps"], service.dir);
      } else {
        throw new Error(`npm install failed for ${service.name}`);
      }
    }
    if (!(await statExists(packageLockPath))) {
      continue;
    }
  }
}

async function clearLocalNextDevelopmentCaches(services) {
  for (const service of services) {
    if (service.type !== "nextjs") {
      continue;
    }

    const cacheDir = path.join(service.dir, ".next");
    if (!(await statExists(cacheDir))) {
      continue;
    }

    // Turbopack's development manifest can retain references to modules
    // removed by a source update. PM2 is stopped before this runs, so only
    // the regenerable Next development cache is removed.
    printStep(`Clearing Next.js development cache: ${service.name}`);
    await rm(cacheDir, { recursive: true, force: true });
  }
}

function isPlaceholderDatabaseUrl(value) {
  return !value
    || /(?:<|>|\btodo\b|\bexample\b|\byour[_-]?|\bpassword\b)/i.test(value);
}

async function localPostgresClient() {
  const onPath = await runCapture("which", ["psql"]);
  if (onPath.code === 0 && onPath.stdout.trim()) {
    return onPath.stdout.trim();
  }

  // Postgres.app intentionally does not always add its binaries to PATH.
  // Provisioning already recognises this installation, so local `eco up`
  // should use the same client rather than asking the developer to do it.
  for (const candidate of [
    "/Applications/Postgres.app/Contents/Versions/15/bin/psql",
    "/Applications/Postgres.app/Contents/Versions/latest/bin/psql"
  ]) {
    if (await statExists(candidate)) return candidate;
  }
  throw new Error("PostgreSQL is declared in ecompose.yml but psql was not found. Run `eco provision` first.");
}

async function writeLocalDatabaseUrl(envFile, databaseUrl) {
  let content = "";
  try {
    content = await readFile(envFile, "utf8");
  } catch {}

  const existing = content.match(/^DATABASE_URL=(.*)$/m);
  if (existing && !isPlaceholderDatabaseUrl(existing[1].trim())) {
    return false;
  }

  const nextLine = `DATABASE_URL=${databaseUrl}`;
  const nextContent = existing
    ? content.replace(/^DATABASE_URL=.*$/m, nextLine)
    : `${content}${content && !content.endsWith("\n") ? "\n" : ""}${nextLine}\n`;
  await writeFile(envFile, nextContent, "utf8");
  return true;
}

async function bootstrapLocalPostgres({ services, estateRoot, project }) {
  const sqlServices = services.filter((service) => service.runtimes.includes("postgresql@15"));
  if (sqlServices.length === 0) return;

  const psql = await localPostgresClient();
  const authArgs = ["-h", "localhost", "-d", "postgres", "-Atqc"];
  const currentUser = await runCapture(psql, [...authArgs, "SELECT current_user"]);
  if (currentUser.code !== 0 || !currentUser.stdout.trim()) {
    throw new Error("Could not connect to local PostgreSQL. Start PostgreSQL and configure a local role, then rerun `eco up`.");
  }
  const username = currentUser.stdout.trim();

  for (const service of sqlServices) {
    const dbName = sqlDatabaseNameForService(service, project);
    if (!/^[A-Za-z0-9_]+$/.test(dbName)) {
      throw new Error(`Unsafe generated PostgreSQL database name: ${dbName}`);
    }
    const exists = await runCapture(psql, [...authArgs, `SELECT 1 FROM pg_database WHERE datname = '${dbName}'`]);
    if (exists.code !== 0) {
      throw new Error(`Could not inspect local PostgreSQL database ${dbName}.`);
    }
    if (!exists.stdout.trim()) {
      await runCommand(psql, ["-h", "localhost", "-d", "postgres", "-v", "ON_ERROR_STOP=1", "-c", `CREATE DATABASE \"${dbName}\"`]);
      printStep(`Created local PostgreSQL database ${dbName}`);
    }

    const envFile = path.join(estateRoot, service.path, ".env");
    const databaseUrl = `postgresql://${encodeURIComponent(username)}@localhost:5432/${dbName}`;
    if (await writeLocalDatabaseUrl(envFile, databaseUrl)) {
      printStep(`Configured DATABASE_URL for ${service.name}`);
    }
  }
}

async function runLocalRustMigrations({ services, estateRoot }) {
  const rustSqlServices = services.filter((service) => service.runtimes.includes("rust") && service.runtimes.includes("postgresql@15"));
  for (const service of rustSqlServices) {
    const serviceDir = path.join(estateRoot, service.path);
    const migrationsDir = path.join(serviceDir, "migrations");
    if (!(await statExists(migrationsDir))) continue;

    const envContent = await readFile(path.join(serviceDir, ".env"), "utf8");
    const databaseUrl = envContent.match(/^DATABASE_URL=(.*)$/m)?.[1].trim();
    if (!databaseUrl) {
      throw new Error(`DATABASE_URL is required to run migrations for ${service.name}.`);
    }

    const sqlx = await runCapture("which", ["sqlx"]);
    if (sqlx.code !== 0) {
      printStep("Installing sqlx-cli for Rust migrations");
      await runCommand("cargo", ["install", "sqlx-cli", "--no-default-features", "--features", "postgres,rustls"], process.cwd(), cargoRunEnv());
    }
    printStep(`Running Rust migrations: ${service.name}`);
    await runCommand("sqlx", ["migrate", "run", "--source", "migrations"], serviceDir, {
      ...process.env,
      DATABASE_URL: databaseUrl
    });
  }
}

function cargoPackageName(cargoToml) {
  let inPackage = false;
  for (const line of cargoToml.split(/\r?\n/)) {
    if (/^\[package\]\s*$/.test(line)) {
      inPackage = true;
      continue;
    }
    if (inPackage && /^\[/.test(line)) break;
    const nameMatch = inPackage && line.match(/^\s*name\s*=\s*"([^"]+)"\s*$/);
    if (nameMatch) return nameMatch[1];
  }
  return null;
}

async function findRustArtifact(serviceDir, packageName) {
  // Always prefer the release binary -- eco up builds with --release on CT
  // (prod) so the artifact lives in target/release/. The debug path is kept
  // as a fallback for local dev builds.
  const metadataResult = await runCapture("cargo", ["metadata", "--no-deps", "--format-version", "1"], serviceDir, cargoRunEnv());
  if (metadataResult.code === 0) {
    try {
      const targetDirectory = JSON.parse(metadataResult.stdout).target_directory;
      if (targetDirectory) {
        for (const profile of ["release", "debug"]) {
          const candidate = path.join(targetDirectory, profile, packageName);
          if (!(await statExists(candidate))) continue;
          const metadata = await stat(candidate);
          const result = await runCapture("file", [candidate]);
          return { path: candidate, mtimeMs: metadata.mtimeMs, fileDescription: result.stdout };
        }
        return null;
      }
    } catch {
      // Fall back to conventional target locations for an invalid/older Cargo output.
    }
  }

  const candidates = [
    path.join(serviceDir, "target", "release", packageName),
    path.join(path.dirname(serviceDir), "target", "release", packageName),
    path.join(path.dirname(path.dirname(serviceDir)), "target", "release", packageName),
    path.join(serviceDir, "target", "debug", packageName),
    path.join(path.dirname(serviceDir), "target", "debug", packageName),
    path.join(path.dirname(path.dirname(serviceDir)), "target", "debug", packageName)
  ];

  for (const candidate of candidates) {
    if (!(await statExists(candidate))) continue;
    const metadata = await stat(candidate);
    const result = await runCapture("file", [candidate]);
    return { path: candidate, mtimeMs: metadata.mtimeMs, fileDescription: result.stdout };
  }
  return null;
}

async function newestRustInputMtime(directory) {
  let newest = 0;
  const ignoredDirectories = new Set([".git", "target", "node_modules"]);

  async function scan(scanDir) {
    const entries = await readdir(scanDir, { withFileTypes: true }).catch(() => []);
    for (const entry of entries) {
      if (entry.isDirectory()) {
        if (!ignoredDirectories.has(entry.name)) await scan(path.join(scanDir, entry.name));
      } else if (entry.isFile()) {
        const metadata = await stat(path.join(scanDir, entry.name));
        newest = Math.max(newest, metadata.mtimeMs);
      }
    }
  }

  await scan(directory);
  return newest;
}

async function rustBuildState(serviceDir) {
  let cargoToml;
  try {
    cargoToml = await readFile(path.join(serviceDir, "Cargo.toml"), "utf8");
  } catch {
    return { needsBuild: false, reason: "no Cargo.toml" };
  }
  const packageName = cargoPackageName(cargoToml);
  if (!packageName) return { needsBuild: true, reason: "unknown package binary", clean: false };

  const artifact = await findRustArtifact(serviceDir, packageName);
  if (!artifact) return { needsBuild: true, reason: "binary is missing", clean: false };

  // A copied target/ directory can contain a perfectly fresh-looking binary
  // built on the other Mac architecture. Cargo may then skip relinking it,
  // but macOS refuses to execute it with spawn error -86.
  if (process.platform === "darwin" && /Mach-O/.test(artifact.fileDescription)) {
    const expectedArchitecture = process.arch === "x64" ? "x86_64" : process.arch;
    if (!artifact.fileDescription.includes(expectedArchitecture)) {
      return { needsBuild: true, reason: "binary has a non-native architecture", clean: true };
    }
  }

  let newestInput = await newestRustInputMtime(serviceDir);
  // A backend can belong to a workspace whose shared Cargo.toml/Cargo.lock
  // lives above the service directory. Those changes must invalidate the
  // service binary too, while an unchanged sibling service remains skipped.
  for (const directory of [path.dirname(serviceDir), path.dirname(path.dirname(serviceDir))]) {
    for (const filename of ["Cargo.toml", "Cargo.lock"]) {
      const inputPath = path.join(directory, filename);
      if (await statExists(inputPath)) {
        newestInput = Math.max(newestInput, (await stat(inputPath)).mtimeMs);
      }
    }
  }
  if (newestInput > artifact.mtimeMs) {
    return { needsBuild: true, reason: "source is newer than the binary", clean: false };
  }
  return { needsBuild: false, reason: "native binary is current" };
}

async function buildLocalRustServices(services) {
  for (const service of services.filter((entry) => entry.type === "rust")) {
    const serviceDir = service.dir;
    if (!(await statExists(path.join(serviceDir, "Cargo.toml")))) continue;
    const buildState = await rustBuildState(serviceDir);
    if (!buildState.needsBuild) {
      printStep(`Rust service is current; skipping build: ${service.name}`);
      continue;
    }
    if (buildState.reason === "unknown package binary") {
      printStep(`Skipping Rust build for ${service.name}: no [package] binary (virtual workspace or malformed manifest)`);
      continue;
    }
    if (buildState.clean) {
      printStep(`Removing non-native Rust build cache: ${service.name}`);
      await runCommand("cargo", ["clean"], serviceDir, cargoRunEnv());
    }
    printStep(`Building Rust service (${buildState.reason}): ${service.name}`);
    await runCommand("cargo", ["build"], serviceDir, cargoRunEnv());
  }
}

// Bare `cargo` spawns must survive shells whose PATH omits the Rust toolchain
// (for example a container's non-login exec PATH). Rustup installs live in
// ~/.cargo/bin on developer machines and /usr/local/cargo/bin on managed CTs.
function cargoRunEnv() {
  const pathEntries = new Set((process.env.PATH || "").split(":").filter(Boolean));
  for (const candidate of [
    path.join(process.env.HOME || "/root", ".cargo", "bin"),
    "/usr/local/cargo/bin"
  ]) {
    pathEntries.add(candidate);
  }
  return { ...process.env, PATH: [...pathEntries].join(":") };
}

async function runUpDev(args) {
  const { options, positionals } = parseOptions(args);
  const input = positionals[0] || ".";
  const deployment = await loadProjectDeployment(input);
  const estateRoot = path.dirname(deployment.projectDir);
  const domains = uniqueDomainsFromEcompose(deployment.content, deployment.project);
  const domainBranchOverrides = domainBranchOverridesFromEcompose(deployment.content);
  const domainDevFlags = domainDevFlagsFromEcompose(deployment.content);

  // Domains marked `dev: disabled` (or `dev: optional` whose required
  // runtimes can't run on this machine, e.g. onnxruntime) are skipped in
  // local dev only -- prod always composes them. Skipped runtimes are
  // passed to provision.sh so it doesn't fail on them either.
  const skippedDomains = new Set();
  const skippedRuntimes = new Set();
  for (const domain of domains) {
    if (domain === deployment.project) {
      continue;
    }
    const flag = domainDevFlags[domain];
    if (flag === "disabled") {
      skippedDomains.add(domain);
      continue;
    }
    if (flag === "optional") {
      const unsatisfied = [];
      for (const token of domainRuntimesFromServices(domain, deployment.services)) {
        if (!(await runtimeSatisfiable(token))) {
          unsatisfied.push(token);
        }
      }
      if (unsatisfied.length > 0) {
        skippedDomains.add(domain);
        unsatisfied.forEach((token) => skippedRuntimes.add(token));
      }
    }
  }
  const devDomains = domains.filter((domain) => !skippedDomains.has(domain));
  const devServices = deployment.services.filter((service) => {
    const firstSegment = String(service?.path || "").split("/")[0];
    return !skippedDomains.has(firstSegment);
  });
  const skipNotice = skippedDomains.size > 0
    ? `\nSkipped optional domain(s) in local dev (still deployed in prod): ${[...skippedDomains].join(", ")}` +
      `${skippedRuntimes.size > 0 ? ` -- runtime(s) not available on this machine: ${[...skippedRuntimes].join(", ")}` : ""}\n`
    : "";

  const services = await discoverLocalServices(estateRoot, {
    projectDir: deployment.projectDir,
    project: deployment.project,
    declaredServices: devServices
  });

  const devPlan = [
    `estate root: ${estateRoot}`,
    ...devDomains.filter((domain) => domain !== deployment.project).map((domain) => `clone repo if missing: ${domain}`),
    `provision local runtimes from manifest: ${deployment.projectDir}`,
    ...devServices
      .filter((service) => service.runtimes.includes("postgresql@15"))
      .map((service) => `ensure local PostgreSQL database: ${sqlDatabaseNameForService(service, deployment.project)}`),
    ...devServices
      .filter((service) => service.runtimes.includes("rust") && service.runtimes.includes("postgresql@15"))
      .map((service) => `run Rust migrations if present: ${service.name}`),
    ...services
      .filter((service) => service.type === "rust")
      .map((service) => `build Rust service: ${service.name}`),
    ...services
      .filter((service) => ["nextjs", "vite", "node"].includes(service.type))
      .map((service) => `npm install: ${service.name} (${service.dir})`),
    `configure locally in estate scope: ${deployment.projectDir}`,
    `delete existing PM2 services declared by ${await resolveLocalPm2ConfigPath(deployment.projectDir)}`,
    // Actual filename (.js vs .cjs) isn't known until after configure.sh
    // runs (see resolveLocalPm2ConfigPath) -- best-effort guess from
    // whatever's already on disk, purely informational for --dry-run.
    `pm2 startOrReload ${await resolveLocalPm2ConfigPath(deployment.projectDir)} --update-env`
  ];

  if (options["dry-run"]) {
    process.stdout.write("eco up dev plan\n");
    process.stdout.write(`Manifest: ${deployment.filePath}\n`);
    process.stdout.write(`Project root: ${deployment.projectDir}\n\n`);
    if (skipNotice) {
      process.stdout.write(skipNotice);
    }
    devPlan.forEach((line) => process.stdout.write(`${line}\n`));
    return;
  }

  if (skipNotice) {
    process.stdout.write(skipNotice);
  }
  await ensureLocalDomainRepos(estateRoot, devDomains, deployment.project, domainBranchOverrides, deployment.content);
  printStep(`Provisioning local runtimes for ${deployment.project}`);
  const provisionEnv = {
    ...process.env,
    ...(skippedRuntimes.size > 0 ? { ECO_DEV_SKIP_RUNTIMES: [...skippedRuntimes].join(",") } : {})
  };
  await runBundledScript("provision.sh", [deployment.projectDir], { scope: "estate", extraEnv: provisionEnv });
  await bootstrapLocalPostgres({
    services: devServices,
    estateRoot,
    project: deployment.project
  });
  await runLocalRustMigrations({ services: devServices, estateRoot });
  // Configure before local builds so its generated Cargo workspace includes
  // every sibling Rust domain detected for the estate. Cargo otherwise finds
  // a stale workspace and rejects a newly discovered member.
  printStep(`Generating local ecosystem config for ${deployment.project}`);
  const configureEnv = {
    ...process.env,
    ECO_NON_INTERACTIVE: "1",
    PROJECT_DIR: estateRoot,
    PROJECT_NAME: deployment.project,
    PM2_DIR: deployment.projectDir,
    ...(skippedDomains.size > 0 ? { ECO_DEV_SKIP_DOMAINS: [...skippedDomains].join(",") } : {})
  };
  await runBundledScript("configure.sh", [], { scope: "estate", extraEnv: configureEnv });
  const refreshedServices = await discoverLocalServices(estateRoot, {
    projectDir: deployment.projectDir,
    project: deployment.project,
    declaredServices: devServices
  });
  await buildLocalRustServices(refreshedServices);
  await installLocalDependencies(refreshedServices);
  printStep(`Starting PM2 services for ${deployment.project}`);
  const ecosystemConfig = await resolveLocalPm2ConfigPath(deployment.projectDir);
  printStep(`Removing existing PM2 services for ${deployment.project}`);
  await deleteLocalDeclaredPm2Apps(ecosystemConfig, deployment.projectDir);

  // Collect ports declared in ecosystem.config.js
  const servicePorts = new Set();
  try {
    const { createRequire } = await import("node:module");
    const req = createRequire(import.meta.url);
    const eco = req(ecosystemConfig);
    for (const app of (eco.apps || [])) {
      for (const val of Object.values(app.env || {})) {
        const port = parseInt(val, 10);
        if (port > 0) servicePorts.add(port);
      }
    }
  } catch {}

  // Delete any running PM2 process occupying those ports
  if (servicePorts.size > 0) {
    const jlistResult = await runCapture("pm2", ["jlist"]);
    if (jlistResult.code === 0) {
      try {
        const procs = JSON.parse(jlistResult.stdout);
        for (const proc of procs) {
          const env = proc.pm2_env || {};
          for (const val of Object.values(env)) {
            const port = parseInt(val, 10);
            if (servicePorts.has(port)) {
              await runCapture("pm2", ["delete", proc.name]);
              break;
            }
          }
        }
      } catch {}
    }
  }

  await clearLocalNextDevelopmentCaches(refreshedServices);
  await runCommand("pm2", ["start", ecosystemConfig, "--update-env"], deployment.projectDir);
  await assertLocalPm2AppsPresent(ecosystemConfig, deployment.projectDir);
  printStep(`Completed local dev bootstrap for ${deployment.project}`);

  const frontendUrl = await resolveLocalFrontendUrl(ecosystemConfig);
  if (frontendUrl) {
    process.stdout.write(`\n  ${deployment.project} is live → ${frontendUrl}\n`);
    if (process.stdout.isTTY && !process.env.ECO_NO_OPEN) {
      openInBrowser(frontendUrl);
    }
  }

  process.stdout.write("\n[eco up] Following PM2 logs — press Ctrl+C to stop\n\n");
  await new Promise((resolve) => {
    const child = spawn("pm2", ["logs", "--lines", "50"], { stdio: "inherit", env: process.env });
    child.on("error", resolve);
    child.on("exit", resolve);
  });
}

async function tarAndPushDir(ctid, sourceDir, targetTarName) {
  const tempDir = await mkdtemp(path.join(tmpdir(), "eco-up-"));
  const tarPath = path.join(tempDir, `${targetTarName}.tar`);
  const parentDir = path.dirname(sourceDir);
  const baseName = path.basename(sourceDir);
  const tarArgs = ["-C", parentDir];
  if (baseName !== targetTarName) {
    tarArgs.push("--transform", `s|^${baseName}|${targetTarName}|`);
  }
  // A deployment transfers source only. Local .env files can contain both
  // secrets and database URLs for another machine; the CT regenerates its
  // own runtime environment from tracked .env.example files instead.
  tarArgs.push(
    "--exclude=.env",
    "--exclude=*/.env",
    "--exclude=.env.local",
    "--exclude=*/.env.local",
    "--exclude=.env.*.local",
    "--exclude=*/.env.*.local",
    "--exclude=.configure-state",
    "--exclude=*/.configure-state",
    "--exclude=ecosystem.config.js",
    "--exclude=*/ecosystem.config.js",
    "--exclude=ecosystem.config.cjs",
    "--exclude=*/ecosystem.config.cjs",
    "--exclude=Caddyfile",
    "--exclude=*/Caddyfile",
    "--exclude=node_modules",
    "--exclude=*/node_modules",
    "--exclude=target",
    "--exclude=*/target",
    "--exclude=.git",
    "--exclude=*/.git"
  );
  tarArgs.push("-cf", tarPath, baseName);

  try {
    await runCommand("tar", tarArgs);
    await runCommand("pct", ["push", String(ctid), tarPath, `/tmp/${targetTarName}.tar`]);
  } finally {
    await rm(tempDir, { recursive: true, force: true });
  }
}

// Returns a bash command that builds the Rust service only when source has
// changed since the last successful build. A SHA-256 hash of all .rs files
// and Cargo manifests is stored in <serviceDir>/.eco-rust-hash. If the hash
// matches the stored value the build is skipped entirely, preserving the
// existing release binary and saving significant compilation time.
//
// Only inputs that actually drive the compile are hashed. target/ is pruned
// because it holds build artifacts -- including build-script generated .rs
// files under target/debug/build/*/out/ whose presence and content change
// with the build state, not the source -- and would otherwise make the hash
// unstable. node_modules and .git are pruned for the same reason. A service
// inside a Cargo workspace is compiled from the workspace root, so ancestor
// Cargo.toml/Cargo.lock must invalidate the service binary too.
//
// The stored hash is updated only after a successful build. Recording it on
// failure would poison change detection: every later `eco up` would see the
// hash match and permanently skip the rebuild, silently deploying the stale
// binary. On failure the hash is left untouched so the next run retries, and
// the error is reported loudly.
function buildConditionalRustCommand(serviceDir, buildCommand) {
  const hashFile = `${serviceDir}/.eco-rust-hash`;
  const ancestorDirs = [
    serviceDir,
    path.dirname(serviceDir),
    path.dirname(path.dirname(serviceDir))
  ];
  return [
    `_eco_inputs=$(find ${shellSingleQuote(serviceDir)} -path '*/target' -prune -o -path '*/node_modules' -prune -o -path '*/.git' -prune -o \\( -name '*.rs' -o -name 'Cargo.toml' -o -name 'Cargo.lock' \\) -print | sort)`,
    `for _eco_input_dir in ${ancestorDirs.map(shellSingleQuote).join(" ")}; do`,
    `  for _eco_manifest in Cargo.toml Cargo.lock; do`,
    `    if [ -f "$_eco_input_dir/$_eco_manifest" ]; then _eco_inputs="$_eco_inputs"$'\\n'"$_eco_input_dir/$_eco_manifest"; fi`,
    `  done`,
    `done`,
    `_eco_rust_hash=$(printf '%s' "$_eco_inputs" | sort | xargs sha256sum 2>/dev/null | sha256sum | cut -d' ' -f1)`,
    `_eco_rust_stored=$(cat ${shellSingleQuote(hashFile)} 2>/dev/null || true)`,
    `if [[ "$_eco_rust_hash" == "$_eco_rust_stored" && -n "$_eco_rust_hash" ]]; then`,
    `  echo "[eco up] Rust source unchanged for ${shellSingleQuote(serviceDir)} -- skipping recompile"`,
    `else`,
    `  echo "[eco up] Rust source changed for ${shellSingleQuote(serviceDir)} -- recompiling"`,
    `  if ${buildCommand}; then`,
    `    echo "$_eco_rust_hash" > ${shellSingleQuote(hashFile)}`,
    `  else`,
    `    echo "[eco up] Rust build failed for ${shellSingleQuote(serviceDir)} -- source hash not recorded, will retry next run" >&2`,
    `  fi`,
    `fi`
  ].join("\n");
}

async function buildRustInDedicatedCt({ builderInput, appCtid, estateRoot, project, services, ctWorkspaceRoot, ctProjectRoot, projectDir }) {
  const builderCtid = await resolveCtInput(builderInput);
  if (String(builderCtid) === String(appCtid)) {
    // A project may deliberately use its application CT as the Rust builder.
    // In that layout there is no artifact transfer step: build in the same
    // workspace that PM2 will execute, while still keeping the managed Rust
    // toolchain/cache locations explicit and preparing sqlx for migrations.
    process.stdout.write(`[CT ${appCtid}] Rust builder is the application CT; building in place\n`);
    for (const service of services.filter((entry) => entry.runtimes.includes("rust"))) {
      const serviceDir = resolveCtServiceDir(service, { ctWorkspaceRoot, ctProjectRoot, projectDir });
      process.stdout.write(`[CT ${appCtid}] Building Rust service in place: ${service.name}\n`);
      // ctProjectRoot is the bootstrap repository. Unlike the full local
      // estate source it does not necessarily contain the generated Cargo
      // workspace manifest, so build each domain at its actual Cargo.toml.
      // This also writes the artifact exactly where the PM2 service expects
      // it: <domain>/backend/target/release.
      await pctExec(
        appCtid,
        buildConditionalRustCommand(
          serviceDir,
          `cd ${shellSingleQuote(serviceDir)} && RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo PATH=/usr/local/cargo/bin:$PATH RUSTC_WRAPPER= cargo build --release`
        )
      );
    }
    return builderCtid;
  }
  await ensureCtRunning(builderCtid);
  const builderRoot = `/opt/eco-rust-builds/${project}`;
  process.stdout.write(`[CT ${builderCtid}] Syncing Rust build workspace for ${project}\n`);
  // The full estate source (all composed domains plus the generated Cargo
  // workspace manifest) lives on the application CT at ctProjectRoot, not on
  // the host's slim bootstrap checkout (which contains only the bootstrap
  // repo and no domains). Tar it from the app CT, stage through the host,
  // and extract on the builder so `cargo build --workspace` sees everything.
  const syncTempDir = await mkdtemp(path.join(tmpdir(), "eco-rust-sync-"));
  const tarPath = path.join(syncTempDir, `${project}.tar`);
  try {
    await pctExec(appCtid, `cd ${shellSingleQuote(path.dirname(ctProjectRoot))} && tar --exclude='.env' --exclude='*/.env' --exclude='*/node_modules' --exclude='*/target' --exclude='.git' --exclude='*/.git' --exclude='.eco' -cf /tmp/${project}.tar ${shellSingleQuote(path.basename(ctProjectRoot))}`);
    await runCommand("pct", ["pull", String(appCtid), `/tmp/${project}.tar`, tarPath]);
    await runCommand("pct", ["push", String(builderCtid), tarPath, `/tmp/${project}.tar`]);
  } finally {
    await rm(syncTempDir, { recursive: true, force: true });
  }
  await pctExec(builderCtid, `rm -rf ${shellSingleQuote(builderRoot)} && mkdir -p /opt/eco-rust-builds && cd /opt/eco-rust-builds && tar -xf /tmp/${project}.tar && rm -f /tmp/${project}.tar`);
  process.stdout.write(`[CT ${builderCtid}] Building Rust artifacts for ${project}\n`);
  await pctExec(
    builderCtid,
    buildConditionalRustCommand(
      builderRoot,
      `cd ${shellSingleQuote(builderRoot)} && RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo PATH=/usr/local/cargo/bin:$PATH RUSTC_WRAPPER= cargo build --release --workspace`
    )
  );

  const tempDir = await mkdtemp(path.join(tmpdir(), "eco-rust-artifacts-"));
  try {
    for (const service of services.filter((entry) => entry.runtimes.includes("rust"))) {
      // Read the Cargo manifest from the app CT (the host bootstrap layout
      // differs: service.path is relative to the CT estate root, and the host
      // checkout doesn't carry every composed domain). Fall back to the
      // builder workspace if the app CT path is unavailable.
      const appManifest = path.join(ctProjectRoot, service.path, "Cargo.toml");
      let manifest = "";
      try {
        manifest = await pctExecCapture(appCtid, `cat ${shellSingleQuote(appManifest)}`);
      } catch {}
      if (!manifest) {
        try {
          manifest = await pctExecCapture(builderCtid, `cat ${shellSingleQuote(`${builderRoot}/${path.relative(ctProjectRoot, appManifest)}`)}`);
        } catch { continue; }
      }
      const packageName = cargoPackageName(manifest);
      if (!packageName) throw new Error(`Cannot determine Rust package name for ${service.name}.`);
      const builderArtifact = `${builderRoot}/target/release/${packageName}`;
      const localArtifact = path.join(tempDir, packageName);
      // The generated Cargo workspace composes every rust domain into a single
      // target/ at the estate root, and configure.sh points each PM2 app at
      // ctProjectRoot/target/release/<package>. Transfer there (not into each
      // domain's own target/, which the workspace does not populate).
      const targetDir = `${ctProjectRoot}/target/release`;
      process.stdout.write(`[CT ${builderCtid}] Transferring ${service.name} artifact to CT ${appCtid}\n`);
      await runCommand("pct", ["pull", String(builderCtid), builderArtifact, localArtifact]);
      // The running service holds the existing binary open, so a direct
      // pct push to the same path fails with "Text file busy". Push to a
      // temp name then rename over it (rename is a directory-entry change
      // and succeeds even while the old inode is in use).
      await pctExec(appCtid, `mkdir -p ${shellSingleQuote(targetDir)}`);
      await runCommand("pct", ["push", String(appCtid), localArtifact, `${targetDir}/${packageName}.new`]);
      await pctExec(appCtid, `mv -f ${shellSingleQuote(`${targetDir}/${packageName}.new`)} ${shellSingleQuote(`${targetDir}/${packageName}`)} && chmod 755 ${shellSingleQuote(`${targetDir}/${packageName}`)}`);
    }
  } finally {
    await rm(tempDir, { recursive: true, force: true });
  }
  return builderCtid;
}

// A Rust SQLx query macro needs the tables created by migrations before its
// crate can compile. Dedicated builders have Cargo but the application CT may
// not, so stage only sqlx-cli first; artifact compilation follows migrations.
async function prepareDedicatedSqlx({ builderInput, appCtid }) {
  const builderCtid = await resolveCtInput(builderInput);
  await ensureCtRunning(builderCtid);
  const installCommand = "RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo PATH=/usr/local/cargo/bin:$PATH cargo install sqlx-cli --no-default-features --features postgres,rustls";
  let alreadyInstalled = false;
  try {
    await pctExecCapture(builderCtid, "test -x /opt/eco-tools/sqlx || test -x /usr/local/cargo/bin/sqlx");
    alreadyInstalled = true;
  } catch {}
  if (alreadyInstalled) {
    process.stdout.write(`[CT ${builderCtid}] sqlx migration tool already present; skipping install\n`);
    if (String(builderCtid) === String(appCtid)) {
      return;
    }
    await ensureBuilderSqlxCopied({ builderCtid, appCtid });
    return;
  }
  if (String(builderCtid) === String(appCtid)) {
    process.stdout.write(`[CT ${appCtid}] Preparing sqlx migration tool in place\n`);
    await pctExec(appCtid, `${installCommand} && mkdir -p /opt/eco-tools && cp /usr/local/cargo/bin/sqlx /opt/eco-tools/sqlx && chmod 755 /opt/eco-tools/sqlx`);
    return;
  }

  process.stdout.write(`[CT ${builderCtid}] Preparing sqlx migration tool for CT ${appCtid}\n`);
  await pctExec(builderCtid, installCommand);
  await ensureBuilderSqlxCopied({ builderCtid, appCtid });
}

async function ensureBuilderSqlxCopied({ builderCtid, appCtid }) {
  const tempDir = await mkdtemp(path.join(tmpdir(), "eco-sqlx-"));
  try {
    const localSqlx = path.join(tempDir, "sqlx");
    await runCommand("pct", ["pull", String(builderCtid), "/usr/local/cargo/bin/sqlx", localSqlx]);
    await pctExec(appCtid, "mkdir -p /opt/eco-tools");
    await runCommand("pct", ["push", String(appCtid), localSqlx, "/opt/eco-tools/sqlx"]);
    await pctExec(appCtid, "chmod 755 /opt/eco-tools/sqlx");
  } finally {
    await rm(tempDir, { recursive: true, force: true });
  }
}

async function pctExec(ctid, command) {
  await runCommand("pct", ["exec", String(ctid), "--", "bash", "-lc", command]);
}

export async function pctExecCapture(ctid, command) {
  const result = await runCapture("pct", ["exec", String(ctid), "--", "bash", "-lc", command]);
  if (result.code !== 0) {
    throw new Error(`pct exec ${ctid} failed with code ${result.code}: ${result.stderr || result.stdout}`.trim());
  }
  return result.stdout;
}

function serviceSourceMarker(service) {
  if (service.runtimes.includes("rust")) return "Cargo.toml";
  if (service.runtimes.some((runtime) => runtime === "npm" || runtime.startsWith("node@"))) return "package.json";
  if (service.runtimes.includes("maven")) return "pom.xml";
  return "";
}

async function ensureCtServiceSources({ ctid, services, repoCloneSteps, ctWorkspaceRoot, ctProjectRoot, projectDir }) {
  const repairedDomains = new Set();
  for (const service of services.filter((entry) => entry.path)) {
    const serviceDir = resolveCtServiceDir(service, { ctWorkspaceRoot, ctProjectRoot, projectDir });
    const marker = serviceSourceMarker(service);
    const expectedPath = marker ? `${serviceDir}/${marker}` : serviceDir;
    const exists = await pctExecCapture(ctid, `test -e ${shellSingleQuote(expectedPath)}; echo $?`);
    if (exists.trim().endsWith("0")) continue;

    const domain = String(service.path).split("/").filter(Boolean)[0];
    const repoStep = repoCloneSteps.find((step) => step.domain === domain);
    if (!repoStep) {
      throw new Error(
        `Required source for ${service.name} is missing at ${expectedPath}, and Eco cannot recover it because ${domain || service.path} is not a composed repository.`
      );
    }
    if (!repairedDomains.has(domain)) {
      printStep(`[CT ${ctid}] Restoring missing source domain: ${domain}`);
      await pctExec(ctid, `cd ${ctWorkspaceRoot} && ${repoStep.command}`);
      repairedDomains.add(domain);
    }
    const restored = await pctExecCapture(ctid, `test -e ${shellSingleQuote(expectedPath)}; echo $?`);
    if (!restored.trim().endsWith("0")) {
      throw new Error(`Repository ${domain} synced but ${expectedPath} is still missing for ${service.name}.`);
    }
  }
}

async function resolveHostSshDir() {
  const homeDir = process.env.HOME;
  if (!homeDir) {
    throw new Error("HOME is not set. Cannot locate host .ssh directory.");
  }

  const sshDir = path.join(homeDir, ".ssh");
  let info;
  try {
    info = await stat(sshDir);
  } catch {
    throw new Error(`Host SSH directory not found: ${sshDir}`);
  }

  if (!info.isDirectory()) {
    throw new Error(`Host SSH path is not a directory: ${sshDir}`);
  }

  return sshDir;
}

async function ensureCtRunning(ctid) {
  const status = await runCapture("pct", ["status", String(ctid)]);
  if (status.code === 0 && /status:\s+running/.test(status.stdout)) {
    return;
  }
  await runCommand("pct", ["start", String(ctid)]);
}

// A plain .js PM2 config is unloadable via require() (both PM2's own load
// and eco's own port/service lookups) once the project's package.json
// declares "type": "module" -- Node then treats it as ESM. .cjs always
// forces CommonJS regardless, which is what configure.sh's pm2_config_filename
// independently decides too (same package.json, same rule) when it
// actually generates the file remotely -- computed here from the same
// local projectDir so every ctConfigPath-derived remote command already
// points at the filename that'll actually exist, with no runtime guessing.
async function isEsmProject(projectDir) {
  try {
    const raw = await readFile(path.join(projectDir, "package.json"), "utf8");
    return JSON.parse(raw).type === "module";
  } catch {
    return false;
  }
}

export async function loadProjectDeployment(input) {
  const { filePath, content } = await readEcompose(input);
  const projectDir = path.dirname(filePath);
  const project = parseProjectName(content) || path.basename(projectDir);
  const ct = parseCtMetadata(content);
  const deploy = parseDeploy(content);
  const expose = parseExpose(content);
  const services = parseServices(content);
  const storage = parseStorage(content);

  if (!ct.id || !ct.template || !ct.storage || !ct.disk || !ct.bridge) {
    throw new Error(`Missing required ct metadata in ${filePath}`);
  }

  const ctid = String(ct.id);
  const ctWorkspaceRoot = `/opt/projects`;
  const ctProjectRoot = `${ctWorkspaceRoot}/${project}`;
  const ctProjectParent = path.posix.dirname(ctProjectRoot);
  const ctEcoRoot = `${ctWorkspaceRoot}/eco`;
  const pm2ConfigFilename = (await isEsmProject(projectDir)) ? "ecosystem.config.cjs" : "ecosystem.config.js";
  const ctConfigPath = `${ctProjectRoot}/${pm2ConfigFilename}`;
  const createArgs = createPctArgs(project, ct, { start: true });

  return {
    filePath,
    content,
    projectDir,
    project,
    ct,
    deploy,
    expose,
    services,
    storage,
    ctid,
    ctWorkspaceRoot,
    ctProjectRoot,
    ctProjectParent,
    ctEcoRoot,
    pm2ConfigFilename,
    ctConfigPath,
    createArgs
  };
}

async function resolveCtIdByHostname(hostname) {
  const result = await runCapture("pct", ["list"]);
  if (result.code !== 0) {
    throw new Error(`pct list failed with code ${result.code}: ${result.stderr}`.trim());
  }

  for (const rawLine of result.stdout.split(/\r?\n/)) {
    const line = rawLine.trim();
    if (!line || /^VMID\s+/i.test(line)) {
      continue;
    }
    const parts = line.split(/\s+/);
    if (parts.length < 1) {
      continue;
    }
    const ctid = parts[0];
    if (!/^\d+$/.test(ctid)) {
      continue;
    }

    const config = await runCapture("pct", ["config", ctid]);
    if (config.code !== 0) {
      continue;
    }

    const hostnameMatch = config.stdout.match(/^hostname:\s*(.+)\s*$/m);
    if (hostnameMatch && hostnameMatch[1].trim() === hostname) {
      return ctid;
    }
  }

  throw new Error(`Cannot resolve CT by hostname "${hostname}" from pct list.`);
}

async function resolveCtInput(input) {
  if (!input) {
    throw new Error("Missing CT identifier.");
  }
  if (/^\d+$/.test(String(input))) {
    return String(input);
  }
  return resolveCtIdByHostname(String(input));
}

function escapeSingleQuotes(value) {
  return String(value).replace(/'/g, `'\\''`);
}

async function pushTextFileToCt(ctid, targetPath, content, label) {
  const tempDir = await mkdtemp(path.join(tmpdir(), "eco-up-file-"));
  const sourcePath = path.join(tempDir, `${label}.tmp`);

  try {
    await writeFile(sourcePath, content, "utf8");
    await runCommand("pct", ["push", String(ctid), sourcePath, targetPath]);
  } finally {
    await rm(tempDir, { recursive: true, force: true });
  }
}

function minioCtReference(storage) {
  const ref = storage?.minio?.ct;
  if (!ref) {
    throw new Error(
      "storage.minio requires `ct: <MinIO CT hostname or ID>` for production. " +
      "Eco keeps S3 traffic on Proxmox's private CT bridge and never routes it through public ingress."
    );
  }
  return String(ref);
}

function minioClientConfig({ endpoint, region, accessKey, secretKey }) {
  for (const [name, value] of Object.entries({ endpoint, region, accessKey, secretKey })) {
    if (!value || /[\r\n]/.test(value)) {
      throw new Error(`Invalid managed MinIO ${name} value.`);
    }
  }
  return [
    `S3_ENDPOINT=${endpoint}`,
    `S3_REGION=${region}`,
    `S3_ACCESS_KEY=${accessKey}`,
    `S3_SECRET_KEY=${secretKey}`,
    ""
  ].join("\n");
}

async function provisionDedicatedMinio({ storage, appCtid }) {
  if (!storage?.minio) return null;
  const minioCtid = await resolveCtInput(minioCtReference(storage));
  if (minioCtid === String(appCtid)) {
    throw new Error("storage.minio.ct must name a dedicated MinIO CT, not the application CT.");
  }
  await ensureCtRunning(minioCtid);

  // The storage CT is deliberately independent of any estate checkout. Push
  // the currently-running Eco installer so every application can share this
  // managed, idempotent MinIO lifecycle without cloning its own Eco repo.
  const installer = await readFile(path.join(packageRoot, "install-minio.sh"), "utf8");
  const installerPath = "/tmp/eco-install-minio.sh";
  await pushTextFileToCt(minioCtid, installerPath, installer, "eco-install-minio.sh");
  await pctExec(minioCtid, `chmod 700 ${installerPath} && ECO_DEPLOY_MODE=prod bash ${installerPath} --ensure && rm -f ${installerPath}`);

  const credentials = await pctExecCapture(
    minioCtid,
    "awk -F= '$1 == \"S3_ACCESS_KEY\" || $1 == \"S3_SECRET_KEY\" { print }' /etc/eco/minio-client.env"
  );
  const values = Object.fromEntries(credentials.trim().split(/\r?\n/).filter(Boolean).map((line) => {
    const index = line.indexOf("=");
    return [line.slice(0, index), line.slice(index + 1)];
  }));
  const ip = await resolveCtPrimaryIp(minioCtid);
  const region = String(storage.minio.region || "us-east-1");
  const clientConfig = minioClientConfig({
    endpoint: `http://${ip}:9000`,
    region,
    accessKey: values.S3_ACCESS_KEY,
    secretKey: values.S3_SECRET_KEY
  });

  return { ctid: minioCtid, endpoint: `http://${ip}:9000`, clientConfig };
}

async function installMinioClientConfig({ ctid, minio }) {
  if (!minio) return;
  await pctExec(ctid, "mkdir -p /etc/eco");
  await pushTextFileToCt(ctid, "/etc/eco/minio-client.env", minio.clientConfig, "minio-client.env");
  await pctExec(ctid, "chmod 600 /etc/eco/minio-client.env");
}

function parseCloudflaredConfig(content) {
  const lines = content.split(/\r?\n/);
  const topLines = [];
  const rules = [];
  let inIngress = false;
  let currentRule = null;

  for (const line of lines) {
    if (!inIngress && /^ingress:\s*$/.test(line)) {
      inIngress = true;
      continue;
    }

    if (!inIngress) {
      topLines.push(line);
      continue;
    }

    const hostMatch = line.match(/^\s*-\s*hostname:\s*(.+)\s*$/);
    if (hostMatch) {
      currentRule = { hostname: hostMatch[1].trim(), service: "" };
      rules.push(currentRule);
      continue;
    }

    const serviceMatch = line.match(/^\s*service:\s*(.+)\s*$/);
    if (serviceMatch) {
      if (currentRule && !currentRule.service) {
        currentRule.service = serviceMatch[1].trim();
      } else {
        rules.push({ hostname: "", service: serviceMatch[1].trim() });
      }
      continue;
    }
  }

  return { topLines, rules };
}

function serializeCloudflaredConfig(parsed) {
  const lines = [...parsed.topLines];
  if (lines.length && lines[lines.length - 1] !== "") {
    lines.push("");
  }
  lines.push("ingress:");
  parsed.rules.forEach((rule) => {
    if (rule.hostname) {
      lines.push(`  - hostname: ${rule.hostname}`);
      lines.push(`    service: ${rule.service}`);
      return;
    }
    lines.push(`  - service: ${rule.service}`);
  });
  return `${lines.join("\n").replace(/\n+$/g, "")}\n`;
}

function upsertCloudflaredHostname(content, hostname, serviceUrl) {
  const parsed = parseCloudflaredConfig(content);
  const nonFallback = parsed.rules.filter((rule) => rule.hostname || rule.service !== "http_status:404");
  const fallback = parsed.rules.find((rule) => !rule.hostname && rule.service === "http_status:404") || {
    hostname: "",
    service: "http_status:404"
  };

  let replaced = false;
  const nextRules = nonFallback.map((rule) => {
    if (rule.hostname === hostname) {
      replaced = true;
      return { hostname, service: serviceUrl };
    }
    return rule;
  });

  if (!replaced) {
    nextRules.push({ hostname, service: serviceUrl });
  }

  return serializeCloudflaredConfig({
    topLines: parsed.topLines,
    rules: [...nextRules, fallback]
  });
}

function isTokenBasedTunnelConfig(content) {
  return /^tunnel:\s*\S+/m.test(content) && !/^credentials-file:\s*\S+/m.test(content);
}

function isEcoManagedTokenTunnel(content) {
  return isTokenBasedTunnelConfig(content) && /^# eco-tunnel-id:\s*\S+/m.test(content);
}

// configPath is always computed as a static "ecosystem.config.js" path
// before configure.sh ever runs remotely -- but configure.sh itself (see
// pm2_config_filename in configure.sh) may have generated .cjs instead for
// an ESM project (package.json "type": "module"), since a plain .js there
// isn't loadable via require() at all. Try the .cjs sibling first, falling
// back to the path as given, so callers don't need to know which one a
// given CT's project actually ended up with.
function requirePm2ConfigSnippet(configPath) {
  const cjsPath = configPath.replace(/\.js$/, ".cjs");
  return `const config = (() => { try { return require(${JSON.stringify(cjsPath)}); } catch (e) { return require(${JSON.stringify(configPath)}); } })();`;
}

// `pm2 startOrReload` only touches apps declared in the *current* config --
// an app that was renamed or removed across runs (e.g. chronic_bootstrap's
// PM2 apps went through chronic_bootstrap-chronic_bootstrap ->
// chronic-chronic_bootstrap as earlier fixes landed) keeps running
// under its old name forever, still bound to whatever port it grabbed.
// With `vite preview --strictPort` (or any strict-bind server), the *new*
// correctly-named process then fails outright on that same port instead of
// falling back to a different one -- a repeat `eco up` crash-looping
// indefinitely until someone notices and manually `pm2 delete`s the
// orphan. Mirrors runUpDev's local "delete any running PM2 process
// occupying those ports" step (see resolveLocalPm2ConfigPath's caller) --
// same fix, just built as a remote node -e snippet since this runs via
// pctExec instead of directly on this machine.
function buildPruneConflictingPortsCommand(configPath) {
  const js = [
    requirePm2ConfigSnippet(configPath),
    "const { execSync } = require('child_process');",
    "const ports = new Set();",
    "for (const app of (config.apps || [])) { for (const val of Object.values(app.env || {})) { const p = parseInt(val, 10); if (p > 0) ports.add(p); } }",
    "if (ports.size > 0) {",
    "  let procs = [];",
    "  try { procs = JSON.parse(execSync('pm2 jlist').toString()); } catch (e) {}",
    "  for (const proc of procs) {",
    "    const env = proc.pm2_env || {};",
    "    let hit = false;",
    "    for (const val of Object.values(env)) { const p = parseInt(val, 10); if (ports.has(p)) { hit = true; break; } }",
    "    if (hit) { try { execSync('pm2 delete ' + JSON.stringify(proc.name)); } catch (e) {} }",
    "  }",
    "}"
  ].join(" ");
  return `node -e ${JSON.stringify(js)}`;
}

// PM2's startOrReload only restarts apps it can match in the new config. Run
// this first so an old process cannot continue serving stale code (or retain
// a port) when an estate is redeployed.
function deleteDeclaredPm2AppsJs(configPath) {
  return [
    requirePm2ConfigSnippet(configPath),
    "const { execSync } = require('child_process');",
    "for (const app of (config.apps || [])) {",
    "  if (!app.name) continue;",
    "  try { execSync('pm2 delete ' + JSON.stringify(app.name), { stdio: 'ignore' }); } catch (e) {}",
    "}"
  ].join(" ");
}

function buildDeleteDeclaredPm2AppsCommand(configPath) {
  const js = deleteDeclaredPm2AppsJs(configPath);
  return `node -e ${JSON.stringify(js)}`;
}

async function deleteLocalDeclaredPm2Apps(configPath, cwd) {
  await runCommand("node", ["-e", deleteDeclaredPm2AppsJs(configPath)], cwd);
}

// `pm2 start` can exit successfully even when it silently leaves a config
// entry out (for example, when its default Node interpreter cannot launch a
// native executable). Do not present that as a successful `eco up`: every
// generated app must at least be registered in PM2.
async function assertLocalPm2AppsPresent(configPath, cwd) {
  const { createRequire } = await import("node:module");
  const req = createRequire(import.meta.url);
  const config = req(configPath);
  const expected = new Set((config.apps || []).map((app) => app.name).filter(Boolean));
  if (expected.size === 0) return;

  const result = await runCapture("pm2", ["jlist"], cwd);
  if (result.code !== 0) {
    throw new Error(`Unable to verify PM2 services after startup: ${result.stderr || result.stdout}`.trim());
  }

  let processes;
  try {
    processes = JSON.parse(result.stdout);
  } catch {
    throw new Error("Unable to parse PM2 service list after startup.");
  }
  const actual = new Set(processes.map((process) => process.name));
  const missing = [...expected].filter((name) => !actual.has(name));
  if (missing.length > 0) {
    throw new Error(`PM2 did not register declared service(s): ${missing.join(", ")}. Check the generated ecosystem config and service logs.`);
  }
}

async function resolveServicePortFromCt(ctid, configPath, serviceName) {
  const js = [
    requirePm2ConfigSnippet(configPath),
    `const target = ${JSON.stringify(serviceName)};`,
    `const apps = config.apps || [];`,
    `const match = apps.find((app) => app.name === target || app.name.endsWith("-" + target));`,
    `if (!match) { process.stderr.write("Missing service " + target); process.exit(1); }`,
    `const env = match.env || {};`,
    `const port = env.PORT || env.SERVER_PORT || "";`,
    `process.stdout.write(String(port));`
  ].join(" ");
  const output = await pctExecCapture(ctid, `node -e ${JSON.stringify(js)}`);
  const port = output.trim();
  if (!/^\d+$/.test(port)) {
    throw new Error(`Cannot resolve port for exposed service "${serviceName}" from ${configPath}.`);
  }
  return port;
}

async function ctConfigHasService(ctid, configPath, serviceName) {
  const js = [
    requirePm2ConfigSnippet(configPath),
    `const target = ${JSON.stringify(serviceName)};`,
    `const apps = config.apps || [];`,
    `const match = apps.find((app) => app.name === target || app.name.endsWith("-" + target));`,
    `process.stdout.write(match ? "yes" : "no");`
  ].join(" ");
  const output = await pctExecCapture(ctid, `node -e ${JSON.stringify(js)}`);
  return output.trim() === "yes";
}

async function resolveCtPrimaryIp(ctid) {
  // `mawk` in minimal Debian CT images does not consistently enable
  // interval-regex syntax such as `{3}`. Ask iproute2 for a global IPv4
  // address first, then retain a field-based hostname fallback without any
  // non-portable regular-expression extensions.
  const output = await pctExecCapture(ctid, "ip=$(ip -4 -o addr show scope global | awk '{ split($4, address, \"/\"); print address[1]; exit }'); if [ -n \"$ip\" ]; then printf '%s\\n' \"$ip\"; else hostname -I | awk '{ for (i = 1; i <= NF; i++) { if (split($i, octets, \".\") == 4) { print $i; exit } } }'; fi");
  const ip = output.trim();
  if (!ip) {
    throw new Error(`Cannot resolve primary IP for CT ${ctid}.`);
  }
  return ip;
}

async function ensureProxyCloudflared(proxyCtid) {
  await pctExec(
    proxyCtid,
    [
      "if ! command -v cloudflared >/dev/null 2>&1; then",
      "  apt-get update;",
      "  apt-get install -y curl ca-certificates;",
      "  arch=$(dpkg --print-architecture);",
      "  case \"$arch\" in",
      "    amd64) pkg=cloudflared-linux-amd64.deb ;;",
      "    arm64) pkg=cloudflared-linux-arm64.deb ;;",
      "    *) echo \"Unsupported architecture for cloudflared: $arch\" >&2; exit 1 ;;",
      "  esac;",
      "  curl -fsSL \"https://github.com/cloudflare/cloudflared/releases/latest/download/${pkg}\" -o /tmp/cloudflared.deb;",
      "  apt-get install -y /tmp/cloudflared.deb;",
      "  rm -f /tmp/cloudflared.deb;",
      "fi"
    ].join("\n")
  );
}

async function ensureCtCaddy(ctid) {
  await pctExec(
    ctid,
    [
      "if ! command -v caddy >/dev/null 2>&1; then",
      "  apt-get update;",
      "  apt-get install -y caddy;",
      "fi"
    ].join("\n")
  );
}

async function ensureProxyHostname({
  dryRun,
  proxyCtInput,
  hostname,
  serviceUrl,
  cloudflaredConfig,
  tunnelName,
  cloudflareAccount,
  tunnelReplicas,
  nonInteractive
}) {
  const defaultConfigPath = cloudflaredConfigPathForAccount(cloudflareAccount);
  const serviceName = cloudflaredServiceNameForAccount(cloudflareAccount);

  if (dryRun) {
    return [
      `# expose: ${hostname} -> ${serviceUrl} via proxy CT ${proxyCtInput}${cloudflareAccount ? ` (Cloudflare account "${cloudflareAccount}")` : ""}`,
      `pct exec ${proxyCtInput} -- bash -lc 'mkdir -p ${path.posix.dirname(defaultConfigPath)}'`,
      `pct exec ${proxyCtInput} -- bash -lc '# auto-detect cloudflared config path, update ingress for ${hostname}'`,
      `pct exec ${proxyCtInput} -- bash -lc '# create DNS route only when tunnel auth supports cloudflared tunnel route dns'`,
      `pct exec ${proxyCtInput} -- bash -lc 'systemctl restart ${serviceName} || service ${serviceName} restart'`
    ];
  }

  const proxyCtid = await resolveCtInput(proxyCtInput);
  await ensureCtRunning(proxyCtid);
  printStep(`Exposing ${hostname} via proxy CT ${proxyCtid}${cloudflareAccount ? ` (Cloudflare account "${cloudflareAccount}")` : ""}`);
  await ensureProxyCloudflared(proxyCtid);
  await pctExec(proxyCtid, `mkdir -p ${path.posix.dirname(defaultConfigPath)}`);

  let cloudflaredConfigPath = "";
  try {
    cloudflaredConfigPath = await resolveProxyCloudflaredConfigPath(proxyCtid, cloudflaredConfig, cloudflareAccount);
  } catch {
    printStep(`Proxy CT ${proxyCtid} has no cloudflared config${cloudflareAccount ? ` for account "${cloudflareAccount}"` : ""}. Bootstrapping dedicated tunnel for ${hostname}`);
    const bootstrap = await ensureProxyTunnel({
      target: proxyCtid,
      hostname,
      tunnelName,
      serviceUrl,
      nonInteractive: true,
      cloudflareAccount
    });
    cloudflaredConfigPath = bootstrap.configPath || defaultConfigPath;
  }

  let existingConfig = "";
  try {
    existingConfig = await pctExecCapture(proxyCtid, `cat ${cloudflaredConfigPath}`);
  } catch {
    printStep(`Proxy CT ${proxyCtid} cloudflared config is missing at ${cloudflaredConfigPath}. Rebuilding tunnel automation for ${hostname}`);
    const bootstrap = await ensureProxyTunnel({
      target: proxyCtid,
      hostname,
      tunnelName,
      serviceUrl,
      nonInteractive: true,
      cloudflareAccount
    });
    cloudflaredConfigPath = bootstrap.configPath || defaultConfigPath;
    existingConfig = await pctExecCapture(proxyCtid, `cat ${cloudflaredConfigPath}`);
  }

  if (isTokenBasedTunnelConfig(existingConfig) && !isEcoManagedTokenTunnel(existingConfig)) {
    printStep(`Proxy CT ${proxyCtid} is using a legacy token-based cloudflared config. Replacing it with a dedicated eco-managed tunnel for ${hostname}`);
    const bootstrap = await ensureProxyTunnel({
      target: proxyCtid,
      hostname,
      tunnelName,
      serviceUrl,
      nonInteractive: true,
      cloudflareAccount
    });
    cloudflaredConfigPath = bootstrap.configPath || defaultConfigPath;
    existingConfig = await pctExecCapture(proxyCtid, `cat ${cloudflaredConfigPath}`);
  }

  if (!/^tunnel:\s*\S+/m.test(existingConfig)) {
    throw new Error(`cloudflared config in proxy CT ${proxyCtid} is missing a tunnel: entry.`);
  }

  // A configured token tunnel is a long-lived infrastructure resource. Its
  // hostname and remote ingress must not be rewritten simply because an
  // estate is brought up again: changing an estate's domain is an explicit
  // tunnel replacement operation.  `eco prox remove-tunnel <domain>` clears
  // the local CT configuration first; the next `eco up` then bootstraps the
  // replacement tunnel deliberately.
  //
  // Exception: if the tunnel is eco-managed AND the requested hostname is not
  // yet present in the local ingress (or is present but pointing to a
  // different serviceUrl -- e.g. a redeployed app on a new IP/port), we must
  // update it.  This handles: first hostname bootstrapping the tunnel, a
  // second hostname being added, and a redeployed app whose CT IP or gateway
  // port changed.
  if (isTokenBasedTunnelConfig(existingConfig) && !isEcoManagedTokenTunnel(existingConfig)) {
    printStep(`Proxy CT ${proxyCtid} already has a token tunnel configured; skipping tunnel configuration for ${hostname}`);
    return [];
  }

  if (isEcoManagedTokenTunnel(existingConfig)) {
    const parsed = parseCloudflaredConfig(existingConfig);
    const existingRule = parsed.rules.find((rule) => rule.hostname === hostname);
    if (existingRule && existingRule.service === serviceUrl) {
      // The local ingress already matches, but the DNS record and the remote
      // Cloudflare tunnel configuration can still be missing or stale (e.g. a
      // hook hostname that moved zones, or a tunnel rebuilt from scratch). The
      // helpers below are idempotent, so reconcile them on every `eco up`.
      printStep(`Proxy CT ${proxyCtid} already has ${hostname} -> ${serviceUrl}; reconciling DNS and remote tunnel config`);
      const tunnelId = (existingConfig.match(/^# eco-tunnel-id:\s*(\S+)/m) || [])[1];
      if (tunnelId && hasCloudflareApiEnv(cloudflareAccount)) {
        await overwriteDnsRecordForTunnel(hostname, tunnelId, cloudflareAccount);
        await putRemoteTunnelConfig(tunnelId, hostname, serviceUrl, cloudflareAccount);
      } else {
        const resolvedTunnelName = (existingConfig.match(/^tunnel:\s*(.+)\s*$/m) || [])[1];
        if (resolvedTunnelName) {
          await pctExec(
            proxyCtid,
            `cloudflared tunnel route dns ${escapeSingleQuotes(resolvedTunnelName)} ${escapeSingleQuotes(hostname)} || true`
          );
        }
      }
      if (tunnelReplicas && tunnelReplicas > 1) {
        const finalReplicas = (!nonInteractive && process.stdin.isTTY) ? await promptForReplicas(tunnelReplicas) : tunnelReplicas;
        if (finalReplicas > 1) {
          await ensureTunnelReplicas({ proxyCtid, account: cloudflareAccount || "default", count: finalReplicas, cloudflaredConfigPath, dryRun });
        }
      }
      return [];
    }

    // Hostname is missing from the eco-managed tunnel's ingress. Add it to
    // both the local config and the remote Cloudflare tunnel configuration.
    printStep(`Adding ${hostname} to existing eco-managed tunnel in proxy CT ${proxyCtid}`);
    const tunnelId = (existingConfig.match(/^# eco-tunnel-id:\s*(\S+)/m) || [])[1];
    const nextConfig = upsertCloudflaredHostname(existingConfig, hostname, serviceUrl);
    await pushTextFileToCt(proxyCtid, "/tmp/eco-cloudflared-config.yml", nextConfig, "cloudflared-config");
    await pctExec(
      proxyCtid,
      `install -D -m 0644 /tmp/eco-cloudflared-config.yml ${cloudflaredConfigPath} && rm -f /tmp/eco-cloudflared-config.yml`
    );

    if (tunnelId && hasCloudflareApiEnv(cloudflareAccount)) {
      await overwriteDnsRecordForTunnel(hostname, tunnelId, cloudflareAccount);
      await putRemoteTunnelConfig(tunnelId, hostname, serviceUrl, cloudflareAccount);
    } else {
      // Fallback: use cloudflared CLI inside the CT to upsert the DNS route
      const resolvedTunnelName = (existingConfig.match(/^tunnel:\s*(.+)\s*$/m) || [])[1];
      if (resolvedTunnelName) {
        await pctExec(
          proxyCtid,
          `cloudflared tunnel route dns ${escapeSingleQuotes(resolvedTunnelName)} ${escapeSingleQuotes(hostname)} || true`
        );
      }
    }

    try {
      await pctExec(proxyCtid, `systemctl restart ${serviceName} || service ${serviceName} restart`);
    } catch {
      printStep(`${serviceName} restart failed in proxy CT ${proxyCtid}`);
    }

    printStep(`Expose complete for ${hostname}`);
    if (tunnelReplicas && tunnelReplicas > 1) {
      const finalReplicas = (!nonInteractive && process.stdin.isTTY) ? await promptForReplicas(tunnelReplicas) : tunnelReplicas;
      if (finalReplicas > 1) {
        await ensureTunnelReplicas({ proxyCtid, account: cloudflareAccount || "default", count: finalReplicas, cloudflaredConfigPath, dryRun });
      }
    }
    return [];
  }

  const nextConfig = upsertCloudflaredHostname(existingConfig, hostname, serviceUrl);
  await pushTextFileToCt(proxyCtid, "/tmp/eco-cloudflared-config.yml", nextConfig, "cloudflared-config");
  await pctExec(
    proxyCtid,
    `install -D -m 0644 /tmp/eco-cloudflared-config.yml ${cloudflaredConfigPath} && rm -f /tmp/eco-cloudflared-config.yml`
  );

  const resolvedTunnelName = (existingConfig.match(/^tunnel:\s*(.+)\s*$/m) || [])[1];
  if (!resolvedTunnelName) {
    throw new Error(`Cannot resolve tunnel name/id from ${cloudflaredConfigPath} in proxy CT ${proxyCtid}.`);
  }
  await pctExec(
    proxyCtid,
    `cloudflared tunnel route dns ${escapeSingleQuotes(resolvedTunnelName)} ${escapeSingleQuotes(hostname)} || true`
  );

  try {
    await pctExec(proxyCtid, `systemctl restart ${serviceName} || service ${serviceName} restart`);
  } catch {
    printStep(`${serviceName} restart failed in proxy CT ${proxyCtid}. Recreating the dedicated tunnel for ${hostname}`);
    await ensureProxyTunnel({
      target: proxyCtid,
      hostname,
      tunnelName,
      serviceUrl,
      nonInteractive: true,
      cloudflareAccount
    });
  }

  printStep(`Expose complete for ${hostname}`);
  if (tunnelReplicas && tunnelReplicas > 1) {
    const finalReplicas = (!nonInteractive && process.stdin.isTTY) ? await promptForReplicas(tunnelReplicas) : tunnelReplicas;
    if (finalReplicas > 1) {
      await ensureTunnelReplicas({ proxyCtid, account: cloudflareAccount || "default", count: finalReplicas, cloudflaredConfigPath, dryRun });
    }
  }
  return [];
}

async function promptForReplicas(defaultValue) {
  const rl = createInterface({ input, output });
  try {
    const answer = await rl.question(`Number of cloudflared tunnel replicas? [${defaultValue}]: `);
    const parsed = parseInt(String(answer).trim(), 10);
    return Number.isNaN(parsed) || parsed < 1 ? defaultValue : parsed;
  } finally {
    rl.close();
  }
}

async function ensureTunnelReplicas({ proxyCtid, account, count, cloudflaredConfigPath, dryRun }) {
  if (dryRun) {
    return;
  }

  const serviceName = cloudflaredServiceNameForAccount(account === "default" ? "" : account);
  const templateUnitName = `${serviceName}@.service`;

  const configContent = await pctExecCapture(proxyCtid, `cat ${cloudflaredConfigPath}`);
  const tokenMatch = configContent.match(/^tunnel:\s*(.+)$/m);
  const token = tokenMatch ? tokenMatch[1].trim() : "";
  if (!token) {
    printStep(`Cannot read tunnel token from ${cloudflaredConfigPath} in proxy CT ${proxyCtid}; skipping replicas setup`);
    return;
  }

  const unitContent = `[Unit]
Description=cloudflared ${account} replica %i
After=network-online.target
Wants=network-online.target

[Service]
TimeoutStartSec=15
Type=notify
ExecStart=/usr/bin/cloudflared --no-autoupdate --config ${cloudflaredConfigPath} tunnel run --token ${token}
Restart=on-failure
RestartSec=5s

[Install]
WantedBy=multi-user.target
`;

  await pushTextFileToCt(proxyCtid, `/etc/systemd/system/${templateUnitName}`, unitContent, `cloudflared-template`);
  await pctExec(proxyCtid, "systemctl daemon-reload");

  const currentRaw = await pctExecCapture(
    proxyCtid,
    `systemctl list-units '${serviceName}@*' --no-legend --state=active 2>/dev/null | wc -l`
  );
  const current = parseInt(currentRaw.trim()) || 0;

  if (count > current) {
    printStep(`Scaling cloudflared ${account} from ${current} to ${count} replica(s)`);
    for (let i = current + 1; i <= count; i++) {
      await pctExec(proxyCtid, `systemctl enable --now ${serviceName}@${i}`);
    }
  } else if (count < current) {
    printStep(`Scaling cloudflared ${account} from ${current} to ${count} replica(s)`);
    for (let i = current; i > count; i--) {
      await pctExec(proxyCtid, `systemctl disable --now ${serviceName}@${i}`);
    }
  }
}

async function resolveProxyCloudflaredConfigPath(proxyCtid, preferredPath, cloudflareAccount) {
  const candidates = [];
  if (preferredPath) {
    candidates.push(preferredPath);
  }
  if (cloudflareAccount) {
    // Named accounts live at a deterministic, eco-managed path -- checked
    // below and nowhere else. Falling through to the default-account
    // candidates or the broad systemd/find discovery further down would
    // find the *other* account's cloudflared config (it's genuinely
    // present and valid, just for the wrong Cloudflare account) and
    // silently attach this hostname to the wrong tunnel/account.
    candidates.push(cloudflaredConfigPathForAccount(cloudflareAccount));
  } else {
    candidates.push("/etc/cloudflared/config.yml", "/root/.cloudflared/config.yml");
  }

  for (const candidate of candidates) {
    const result = await runCapture("pct", [
      "exec",
      String(proxyCtid),
      "--",
      "bash",
      "-lc",
      `test -f ${candidate}`
    ]);
    if (result.code === 0) {
      return candidate;
    }
  }

  if (cloudflareAccount) {
    throw new Error(
      `Cannot locate cloudflared config for account "${cloudflareAccount}" in proxy CT ${proxyCtid} at ${cloudflaredConfigPathForAccount(cloudflareAccount)}.`
    );
  }

  const systemctlResult = await runCapture("pct", [
    "exec",
    String(proxyCtid),
    "--",
    "bash",
    "-lc",
    "systemctl cat cloudflared 2>/dev/null || true"
  ]);
  const unitText = `${systemctlResult.stdout}\n${systemctlResult.stderr}`;
  const configMatch = unitText.match(/--config(?:=|\s+)(\S+)/);
  if (configMatch) {
    const detectedPath = configMatch[1].trim().replace(/^['"]|['"]$/g, "");
    const exists = await runCapture("pct", [
      "exec",
      String(proxyCtid),
      "--",
      "bash",
      "-lc",
      `test -f ${detectedPath}`
    ]);
    if (exists.code === 0) {
      return detectedPath;
    }
  }

  const findResult = await runCapture("pct", [
    "exec",
    String(proxyCtid),
    "--",
    "bash",
    "-lc",
    "find /etc/cloudflared /root/.cloudflared /home -maxdepth 3 -type f -name 'config.yml' 2>/dev/null | head -n 1"
  ]);
  const found = findResult.stdout.trim();
  if (found) {
    return found;
  }

  throw new Error(
    `Cannot locate cloudflared config in proxy CT ${proxyCtid}. Checked common paths and systemd unit config.`
  );
}

async function exposeViaProxyCt({
  dryRun,
  expose,
  project,
  appCtid,
  appConfigPath
}) {
  if (!toBool(expose.enabled)) {
    return [];
  }

  const hostname = expose.hostname || `${project}.ktt.my.id`;
  let serviceName = expose.service || `${project}-frontend`;
  const proxyCtInput = expose.proxy_ct || expose.proxy_ctid || expose.via;
  if (!proxyCtInput) {
    throw new Error(`Expose is enabled for ${project}, but expose.proxy_ct is missing.`);
  }

  const results = [];

  if (dryRun) {
    const servicePort = expose.target_port && /^\d+$/.test(String(expose.target_port))
      ? String(expose.target_port)
      : `<service-port:${serviceName}>`;
    results.push(...(await ensureProxyHostname({
      dryRun,
      proxyCtInput,
      hostname,
      serviceUrl: `http://<app-ct-ip>:${servicePort}`,
      cloudflaredConfig: expose.cloudflared_config,
      tunnelName: expose.tunnel_name,
      cloudflareAccount: expose.cloudflare_account,
      tunnelReplicas: expose.tunnel_replicas
    })));
  } else {
    const gatewayServiceName = `${project}-gateway`;
    const hasGatewayService = await ctConfigHasService(appCtid, appConfigPath, gatewayServiceName).catch(() => false);
    if (hasGatewayService) {
      serviceName = gatewayServiceName;
    }
    const port = !hasGatewayService && expose.target_port && /^\d+$/.test(String(expose.target_port))
      ? String(expose.target_port)
      : await resolveServicePortFromCt(appCtid, appConfigPath, serviceName);
    const appIp = await resolveCtPrimaryIp(appCtid);
    const serviceUrl = `http://${appIp}:${port}`;

    await ensureProxyHostname({
      dryRun,
      proxyCtInput,
      hostname,
      serviceUrl,
      cloudflaredConfig: expose.cloudflared_config,
      tunnelName: expose.tunnel_name,
      cloudflareAccount: expose.cloudflare_account,
      tunnelReplicas: expose.tunnel_replicas
    });
  }

  // expose.additional: more hostname/service pairs sharing the same
  // tunnel -- e.g. a WebSocket/game server on its own subdomain, separate
  // from the main frontend. Same merge-not-overwrite tunnel config
  // ensureProxyHostname already does for webhook-ingress hostnames, just
  // driven by ecompose.yml instead of the deploy webhook.
  const additionalEntries = Array.isArray(expose.additional) ? expose.additional : [];
  for (const entry of additionalEntries) {
    const entryHostname = entry.hostname;
    const entryServiceName = entry.service;
    if (!entryHostname || !entryServiceName) {
      continue;
    }

    if (dryRun) {
      const servicePort = entry.target_port && /^\d+$/.test(String(entry.target_port))
        ? String(entry.target_port)
        : `<service-port:${entryServiceName}>`;
      results.push(...(await ensureProxyHostname({
        dryRun,
        proxyCtInput,
        hostname: entryHostname,
        serviceUrl: `http://<app-ct-ip>:${servicePort}`,
        cloudflaredConfig: entry.cloudflared_config || expose.cloudflared_config,
        tunnelName: entry.tunnel_name || expose.tunnel_name,
        cloudflareAccount: entry.cloudflare_account || expose.cloudflare_account
      })));
      continue;
    }

    const entryPort = entry.target_port && /^\d+$/.test(String(entry.target_port))
      ? String(entry.target_port)
      : await resolveServicePortFromCt(appCtid, appConfigPath, entryServiceName);
    const entryAppIp = await resolveCtPrimaryIp(appCtid);
    const entryServiceUrl = `http://${entryAppIp}:${entryPort}`;

    await ensureProxyHostname({
      dryRun,
      proxyCtInput,
      hostname: entryHostname,
      serviceUrl: entryServiceUrl,
      cloudflaredConfig: entry.cloudflared_config || expose.cloudflared_config,
      tunnelName: entry.tunnel_name || expose.tunnel_name,
      cloudflareAccount: entry.cloudflare_account || expose.cloudflare_account
    });
  }

  return results;
}

async function installDeployReceiver({ ctid, receiverSetup }) {
  await pctExec(ctid, `mkdir -p ${shellSingleQuote(receiverSetup.deployRoot)}`);

  for (const [targetPath, fileContent] of Object.entries(receiverSetup.files)) {
    await pushTextFileToCt(ctid, targetPath, fileContent, path.basename(targetPath));
  }

  await pctExec(
    ctid,
    `chmod 700 ${shellSingleQuote(receiverSetup.deployScriptPath)} && pm2 startOrReload ${shellSingleQuote(receiverSetup.pm2ConfigPath)} --update-env`
  );
}

async function syncGithubDeployWebhooks({ githubRepos, githubDeploy, webhookSecret }) {
  const token = process.env.GITHUB_TOKEN || "";
  if (!token) {
    throw new Error("GITHUB_TOKEN is required when deploy.github.enabled is true.");
  }

  const results = [];
  for (const repo of githubRepos) {
    const result = await syncGithubPushWebhook({
      token,
      owner: repo.owner,
      repo: repo.repo,
      webhookUrl: githubDeploy.webhookUrl,
      secret: webhookSecret,
      staleWebhookHostname: githubDeploy.staleWebhookHostname || ""
    });
    results.push({ repo: repo.fullName, ...result });
  }
  return results;
}

export async function runExpose(args) {
  const { options, positionals } = parseOptions(args);
  const input = positionals[0] || ".";
  const deployment = await loadProjectDeployment(input);

  if (options["dry-run"]) {
    const exposurePlan = await exposeViaProxyCt({
      dryRun: true,
      expose: deployment.expose,
      project: deployment.project,
      appCtid: deployment.ctid,
      appConfigPath: deployment.ctConfigPath
    });
    process.stdout.write(`eco expose plan\n`);
    process.stdout.write(`Manifest: ${deployment.filePath}\n`);
    process.stdout.write(`Project root: ${deployment.projectDir}\n\n`);
    exposurePlan.forEach((command) => process.stdout.write(`${command}\n`));
    return;
  }

  await ensureCtRunning(deployment.ctid);
  await exposeViaProxyCt({
    dryRun: false,
    expose: deployment.expose,
    project: deployment.project,
    appCtid: deployment.ctid,
    appConfigPath: deployment.ctConfigPath
  });
}

async function isOnProxmoxHost() {
  const result = await runCapture("which", ["pct"]);
  return result.code === 0;
}

// A container running deployed estates installs the eco repo at
// /opt/projects/eco and extracts each estate below /opt/projects/<project>.
// A developer laptop never has that layout, so this reliably distinguishes a
// Proxmox CT from dev mode when pct itself is absent (pct only exists on the
// hypervisor host). Running `eco up` inside such a container must never
// silently dev-mode: dev mode would re-provision runtimes, rebuild every
// Rust service it finds, and restart the estate's PM2 processes with local
// config, which is destructive against a live production estate.
async function isCtEstateContext(input) {
  if (!(await statExists("/opt/projects/eco"))) {
    return false;
  }
  return path.resolve(input).startsWith("/opt/projects");
}

export async function runUp(args) {
  if (args[0] === "dev") {
    await runUpDev(args.slice(1));
    return;
  }

  if (!(await isOnProxmoxHost())) {
    const input = args.find((arg) => !arg.startsWith("--")) || ".";
    if (await isCtEstateContext(input)) {
      throw new Error(
        "This looks like a deployed estate inside a container (no 'pct' here), so 'eco up' would fall back to local dev mode and rebuild/restart the production estate as if it were a dev machine.\n" +
        "Run 'eco up' from the Proxmox host (where pct is available), or trigger the estate's GitHub deploy webhook (redeploy.sh) instead."
      );
    }
    process.stdout.write("Not on a Proxmox host (pct not found) — running in dev mode.\n");
    await runUpDev(args);
    return;
  }

  const { options, positionals } = parseOptions(args);
  const input = positionals[0] || ".";
  const deployment = await loadProjectDeployment(input);
  const stagingConfig = parseStaging(deployment.content);

  if (options.staging) {
    if (!stagingConfig.ct) {
      throw new Error(`--staging requested for ${deployment.project}, but ecompose.yml has no staging.ct declared. Add a staging: block (staging.ct: 1000).`);
    }
    await provisionEstate(deployment, { options, staging: true, stagingConfig });
    return;
  }

  await provisionEstate(deployment, { options, staging: false, stagingConfig });
  if (stagingConfig.ct && !options["prod-only"]) {
    process.stdout.write(`\n[eco up] staging block declared (ct ${stagingConfig.ct}) — provisioning the staging footprint.\n`);
    await provisionEstate(deployment, { options, staging: true, stagingConfig });
  }
}

async function provisionEstate(deployment, { options, staging, stagingConfig }) {
  const {
    filePath,
    content,
    projectDir,
    project,
    ct: baseCt,
    deploy,
    expose: baseExpose,
    services,
    storage,
    ctid: baseCtid,
    ctWorkspaceRoot,
    ctProjectRoot,
    ctProjectParent,
    ctEcoRoot,
    ctConfigPath
  } = deployment;

  const ctid = staging ? String(stagingConfig.ct) : baseCtid;
  const ct = staging ? { ...baseCt, id: stagingConfig.ct, hostname: `${project}-staging` } : baseCt;
  const expose = staging
    ? {
        ...baseExpose,
        hostname: stagingConfig.hostname || deriveStagingHostname(baseExpose.hostname),
        // Additional hostnames (e.g. photos.stuff8.com) belong to the prod
        // footprint. Re-exposing them on the staging CT would repoint those
        // DNS routes at staging, hijacking prod traffic. Staging exposes only
        // its own staging hostname.
        additional: []
      }
    : baseExpose;
  const createArgs = createPctArgs(project, ct, { start: true });
  const webhookTunnelName = `${project}-${staging ? "staging-" : ""}deploy-webhook`;
  const stagingEcomposeContent = staging
    ? deriveStagingEcomposeContent(content, stagingConfig, expose.hostname)
    : "";
  const stagingEcomposePath = `${ctProjectRoot}/ecompose.yml`;
  const domains = uniqueDomainsFromEcompose(content, project);
  const domainBranchOverrides = domainBranchOverridesFromEcompose(content);
  const hostSshDir = await resolveHostSshDir();
  const githubDeploy = resolveDeployGithubConfig({ project, expose, deploy });
  const githubRepos = githubDeploy
    ? await resolveDeployGithubReposForProject({ domains, project, projectDir, branch: githubDeploy.branch, domainBranchOverrides, content })
    : [];
  const webhookSecret = githubDeploy
    ? await resolveEstateWebhookSecret({
        ctid,
        ctProjectRoot,
        fallback: crypto.randomBytes(32).toString("hex")
      })
    : "";
  const bootstrapRepo = githubRepos.find((repo) => isSelfDomain(repo.domain, project, projectDir));
  const bootstrapSourceSync = bootstrapRepo
    ? {
        domain: bootstrapRepo.domain,
        command: buildGitForceSyncCommand({
          repoPath: ctProjectRoot,
          branch: bootstrapRepo.branch,
          gitUrl: bootstrapRepo.git,
          // Each composed repository is a direct child of the bootstrap
          // worktree and will be force-synced independently below.
          preservePaths: domains.filter((domain) => !isSelfDomain(domain, project, projectDir))
        })
      }
    : null;

  const repoCloneSteps = [];
  for (const domain of domains) {
    if (isSelfDomain(domain, project, projectDir)) {
      continue;
    }
    const repo = await resolveDomainGit(domain, { project, content });
    if (!repo) {
      throw new Error(`No git remote found for domain "${domain}" (not in eco/repos.json and no composition: block in ecompose.yml)`);
    }
    repoCloneSteps.push({
      domain,
      command: buildGitForceSyncCommand({
        repoPath: `${ctProjectRoot}/${domain}`,
        branch: domainBranchOverrides[domain] || repo.branch,
        gitUrl: repo.git
      })
    });
  }

  const dependencyInstallSteps = services
    .filter((service) => service.path)
    .filter((service) => service.runtimes.some((runtime) => runtime === "npm" || runtime.startsWith("node@")))
    .map((service) => {
      const serviceDir = resolveCtServiceDir(service, { ctWorkspaceRoot, ctProjectRoot, projectDir });
      return {
        name: service.name,
        path: service.path,
        command: buildNpmInstallCommand(serviceDir)
      };
    });

  const buildSteps = services
    .filter((service) => service.path)
    .flatMap((service) => {
      const serviceDir = resolveCtServiceDir(service, { ctWorkspaceRoot, ctProjectRoot, projectDir });
      const steps = [];

      if (service.runtimes.some((runtime) => runtime === "npm" || runtime.startsWith("node@"))) {
        steps.push({
          name: service.name,
          path: service.path,
          // ECO_DEPLOY_MODE=prod matters for frontends that branch their build
          // on it (e.g. Astro + @astrojs/cloudflare): the adapter that
          // generates _routes.json only runs in prod mode, so a build without
          // it silently produces a partial artifact and then aborts any
          // downstream post-build step (patch-routes.js) that needs the file.
          command: `if [ -f "${serviceDir}/package.json" ]; then cd "${serviceDir}" && if [ -f ".env" ]; then set -a && . ./.env && set +a; fi && ECO_DEPLOY_MODE=prod npm run build --if-present; fi`
        });
      }

      if (service.runtimes.includes("maven")) {
        steps.push({
          name: service.name,
          path: service.path,
          command: `if [ -f "${serviceDir}/pom.xml" ]; then cd "${serviceDir}" && mvn -DskipTests package; fi`
        });
      }

      return steps;
    });

  const dataBootstrapSteps = buildDataBootstrapPlan({ services, ctWorkspaceRoot, ctProjectRoot, projectDir, project });
  const migrationSteps = buildRustMigrationPlan({ services, ctWorkspaceRoot, ctProjectRoot, projectDir });
  const dedicatedRustBuilder = process.env.ECO_RUST_DEDICATED_BUILDER?.trim() || "";
  const dedicatedRustBuilderCtid = dedicatedRustBuilder ? await resolveCtInput(dedicatedRustBuilder) : "";
  const rustBuilderIsApplication = dedicatedRustBuilder
    && String(dedicatedRustBuilderCtid) === String(ctid);
  const externalRustBuilder = dedicatedRustBuilder && !rustBuilderIsApplication;
  const stopEstateRustBuildsCommand = buildStopEstateRustBuildsCommand({
    services,
    ctWorkspaceRoot,
    ctProjectRoot,
    projectDir
  });
  let receiverSetup = null;

  const minioPlan = storage.minio
    ? [
        `# managed object storage stays on Proxmox's private CT bridge`,
        `pct start ${minioCtReference(storage)}`,
        `pct exec ${minioCtReference(storage)} -- bash -lc '<install and ensure eco-minio.service>'`,
        `pct push ${ctid} <managed-minio-client-config> /etc/eco/minio-client.env`
      ]
    : [];

  const commands = [
    `pct status ${ctid} || pct create ${createArgs.slice(1).join(" ")}`,
    `pct start ${ctid}`,
    `pct exec ${ctid} -- bash -lc 'mkdir -p ${ctWorkspaceRoot}'`,
    `# stop only in-progress Cargo builds belonging to this estate (never other estates in the CT)`,
    `pct exec ${ctid} -- bash -lc ${shellSingleQuote(stopEstateRustBuildsCommand)}`,
    `pct push ${ctid} <temp-tar:project:${project}> /tmp/${project}.tar`,
    `pct push ${ctid} <temp-tar:eco> /tmp/eco.tar`,
    `pct push ${ctid} <temp-tar:.ssh:${hostSshDir}> /tmp/.ssh.tar`,
    `pct exec ${ctid} -- bash -lc 'cd ${ctWorkspaceRoot} && tar -xf /tmp/${project}.tar && tar -xf /tmp/eco.tar && rm -f /tmp/${project}.tar /tmp/eco.tar && mkdir -p /root && tar -xf /tmp/.ssh.tar -C /root && rm -f /tmp/.ssh.tar && chmod 700 /root/.ssh && find /root/.ssh -type d -exec chmod 700 {} \\; && find /root/.ssh -type f -exec chmod 600 {} \\;'`,
    ...(bootstrapSourceSync ? [`# force-sync bootstrap source\npct exec ${ctid} -- bash -lc '${bootstrapSourceSync.command}'`] : []),
    ...(staging ? [`# write staging ecompose.yml (staging hostname + ct) after bootstrap sync restores the prod manifest\npct push ${ctid} <staging-ecompose> ${stagingEcomposePath}`] : []),
    `pct exec ${ctid} -- bash -lc 'cd ${ctWorkspaceRoot} && ${externalRustBuilder ? "ECO_RUST_DEDICATED_BUILDER=managed " : ""}bash ${ctEcoRoot}/provision.sh ${project}'`,
    ...(toBool(expose.enabled) ? [`pct exec ${ctid} -- bash -lc 'if ! command -v caddy >/dev/null 2>&1; then apt-get update && apt-get install -y caddy; fi'`] : []),
    ...repoCloneSteps.map(({ domain, command }) => `# sync repo: ${domain}\npct exec ${ctid} -- bash -lc '${command}'`),
    ...dependencyInstallSteps.map(({ name, path: servicePath, command }) => `# install deps: ${name} (${servicePath})\npct exec ${ctid} -- bash -lc '${command}'`),
    ...dataBootstrapSteps.map((command, index) => `# bootstrap data service #${index + 1}\npct exec ${ctid} -- bash -lc '${command}'`),
    ...migrationSteps.map((command, index) => `# apply Rust migration set #${index + 1}\npct exec ${ctid} -- bash -lc '${dedicatedRustBuilder ? "export ECO_SQLX_BIN=/opt/eco-tools/sqlx; " : ""}${command}'`),
    `pct exec ${ctid} -- bash -lc 'cd ${ctEcoRoot} && npm install && npm link'`,
    `pct exec ${ctid} -- bash -lc 'cd ${ctWorkspaceRoot} && ECO_DEPLOY_MODE=prod ECO_NON_INTERACTIVE=1 PROJECT_DIR=${ctProjectRoot} PROJECT_NAME=${project} PM2_DIR=${ctProjectRoot} bash ${ctEcoRoot}/configure.sh'`,
    ...buildSteps.map(({ name, path: servicePath, command }) => `# build artifact: ${name} (${servicePath})\npct exec ${ctid} -- bash -lc '${command}'`),
    `# remove the estate's current PM2 services, then prune any old process that still holds a configured port`,
    `pct exec ${ctid} -- bash -lc '${buildDeleteDeclaredPm2AppsCommand(ctConfigPath)}'`,
    `pct exec ${ctid} -- bash -lc 'pm2 startOrReload ${ctConfigPath} --update-env'`,
    ...(receiverSetup
      ? [
          `# install deploy webhook receiver\npct exec ${ctid} -- bash -lc 'mkdir -p ${receiverSetup.deployRoot}'`,
          `# write deploy receiver assets under ${receiverSetup.deployRoot}`,
          `pct exec ${ctid} -- bash -lc 'chmod 700 ${receiverSetup.deployScriptPath} && pm2 startOrReload ${receiverSetup.pm2ConfigPath} --update-env'`
        ]
      : [])
  ];

  const exposurePlan = await exposeViaProxyCt({
    dryRun: true,
    expose,
    project,
    appCtid: ctid,
    appConfigPath: ctConfigPath,
    ctWorkspaceRoot
  }).catch((error) => {
    if (toBool(expose.enabled)) {
      throw error;
    }
    return [];
  });
  const deployExposurePlan = githubDeploy
    ? await ensureProxyHostname({
        dryRun: true,
        proxyCtInput: githubDeploy.proxyCtInput,
        hostname: githubDeploy.webhookHostname,
        serviceUrl: `http://<app-ct-ip>:${githubDeploy.port}`,
        cloudflaredConfig: expose.cloudflared_config,
        tunnelName: webhookTunnelName,
        cloudflareAccount: expose.cloudflare_account
      })
    : [];
  const webhookPlan = githubDeploy
    ? githubRepos.map((repo) => `# sync GitHub webhook: ${repo.fullName} -> ${githubDeploy.webhookUrl}`)
    : [];

  if (options["dry-run"]) {
    process.stdout.write(`eco up plan${staging ? " (staging)" : ""}\n`);
    process.stdout.write(`Manifest: ${filePath}\n`);
    process.stdout.write(`Project root: ${projectDir}\n`);
    process.stdout.write(`CT workspace root: ${ctWorkspaceRoot}\n`);
    process.stdout.write(`CT ID: ${ctid}\n`);
    process.stdout.write(`Hostname: ${expose.hostname || "(none)"}\n`);
    process.stdout.write(`Domains: ${domains.join(", ")}\n\n`);
    [...minioPlan, ...commands, ...deployExposurePlan, ...webhookPlan, ...exposurePlan].forEach((command) => process.stdout.write(`${command}\n`));
    return;
  }

  const status = await runCapture("pct", ["status", ctid]);
  if (status.code !== 0) {
    const availableTemplate = await resolveAvailableTemplate(ct.template);
    const resolvedCreateArgs = availableTemplate === ct.template
      ? createArgs
      : createPctArgs(project, ct, { start: true, template: availableTemplate });
    await runCommand("pct", resolvedCreateArgs);
  }

  await ensureCtRunning(ctid);
  printStep(`CT ${ctid} is running`);
  const minio = await provisionDedicatedMinio({ storage, appCtid: ctid });
  if (minio) {
    printStep(`[CT ${minio.ctid}] MinIO is ready at its private bridge endpoint`);
    await installMinioClientConfig({ ctid, minio });
  }
  await pctExec(ctid, `mkdir -p ${ctWorkspaceRoot}`);
  printStep(`[CT ${ctid}] Stopping in-progress Rust builds for ${project}`);
  await pctExec(ctid, stopEstateRustBuildsCommand);
  printStep(`[CT ${ctid}] Pushing project repo: ${project}`);
  await tarAndPushDir(ctid, projectDir, project);
  printStep(`[CT ${ctid}] Pushing eco repo`);
  await tarAndPushDir(ctid, packageRoot, "eco");
  printStep(`[CT ${ctid}] Copying host SSH credentials`);
  // Target name must be ".ssh", not a renamed placeholder like "host-ssh"
  // -- tarAndPushDir renames the top-level dir to match the target name
  // *inside the tar* (right, and needed, for the project/eco pushes above,
  // which want their content to land under a specific different name).
  // The extraction below expects the content to land at literally
  // /root/.ssh, so the name given here has to already be ".ssh" or the
  // chmod/find calls below silently operate on an empty/nonexistent
  // /root/.ssh while the real key material sits under /root/host-ssh
  // instead -- which is exactly the bug this fixes: `eco up` would
  // "succeed" with no error, but no SSH key ever actually reached the CT.
  await tarAndPushDir(ctid, hostSshDir, ".ssh");
  await pctExec(
    ctid,
    `cd ${ctWorkspaceRoot} && tar -xf /tmp/${project}.tar && tar -xf /tmp/eco.tar && rm -f /tmp/${project}.tar /tmp/eco.tar && mkdir -p /root && tar -xf /tmp/.ssh.tar -C /root && rm -f /tmp/.ssh.tar && chmod 700 /root/.ssh && find /root/.ssh -type d -exec chmod 700 {} \\; && find /root/.ssh -type f -exec chmod 600 {} \\;`
  );
  if (bootstrapSourceSync) {
    printStep(`[CT ${ctid}] Syncing bootstrap source: ${bootstrapSourceSync.domain}`);
    await pctExec(ctid, `cd ${ctWorkspaceRoot} && ${bootstrapSourceSync.command}`);
  }
  if (staging) {
    // The bootstrap force-sync restored the prod manifest from git; overwrite
    // it with the staging-flavored manifest so configure.sh and the gateway
    // derive staging URLs instead of prod ones.
    printStep(`[CT ${ctid}] Writing staging ecompose.yml (${expose.hostname})`);
    await pushTextFileToCt(ctid, stagingEcomposePath, stagingEcomposeContent, "staging-ecompose");
  }
  printStep(`[CT ${ctid}] Provisioning runtimes for ${project}`);
  await pctExec(ctid, `cd ${ctWorkspaceRoot} && ${externalRustBuilder ? "ECO_RUST_DEDICATED_BUILDER=managed " : ""}bash ${ctEcoRoot}/provision.sh ${project}`);
  if (toBool(expose.enabled)) {
    printStep(`[CT ${ctid}] Ensuring caddy is installed for gateway`);
    await ensureCtCaddy(ctid);
  }

  for (const step of repoCloneSteps) {
    printStep(`[CT ${ctid}] Syncing repo: ${step.domain}`);
    await pctExec(ctid, `cd ${ctWorkspaceRoot} && ${step.command}`);
  }
  await ensureCtServiceSources({
    ctid,
    services,
    repoCloneSteps,
    ctWorkspaceRoot,
    ctProjectRoot,
    projectDir
  });

  for (const step of dependencyInstallSteps) {
    printStep(`[CT ${ctid}] Installing npm dependencies: ${step.name} (${step.path})`);
    await pctExec(ctid, `cd ${ctWorkspaceRoot} && ${step.command}`);
  }

  printStep(`[CT ${ctid}] Installing eco CLI`);
  await pctExec(ctid, `cd ${ctEcoRoot} && npm install && npm link`);
  // A Rust SQLx query macro needs DATABASE_URL while Cargo expands the
  // macro. This must precede configure.sh's optional test gate and the
  // dedicated-builder compilation below, not merely migrations/PM2 startup.
  for (let index = 0; index < dataBootstrapSteps.length; index += 1) {
    printStep(`[CT ${ctid}] Bootstrapping data service ${index + 1}`);
    // Set a valid locale for the data bootstrap scripts. CTs often have
    // LANG=en_US.UTF-8 set but the locale not generated, so every perl
    // invocation (e.g. Debian's postgresql-common cluster scripts) spews
    // "Setting locale failed" warnings.
    await pctExec(ctid, `export LANG=C.UTF-8 LC_ALL=C.UTF-8 PERL_BADLANG=0\n${dataBootstrapSteps[index]}`);
  }
  if (dedicatedRustBuilder && services.some((service) => service.runtimes.includes("rust") && service.runtimes.includes("postgresql@15"))) {
    await prepareDedicatedSqlx({ builderInput: dedicatedRustBuilder, appCtid: ctid });
  }
  for (let index = 0; index < migrationSteps.length; index += 1) {
    printStep(`[CT ${ctid}] Applying Rust migration set ${index + 1}`);
    await pctExec(ctid, `${dedicatedRustBuilder ? "export ECO_SQLX_BIN=/opt/eco-tools/sqlx; " : ""}${migrationSteps[index]}`);
  }
  printStep(`[CT ${ctid}] Generating ecosystem config for ${project}`);
  await pctExec(
    ctid,
    `cd ${ctWorkspaceRoot} && ECO_DEPLOY_MODE=prod ECO_NON_INTERACTIVE=1 PROJECT_DIR=${ctProjectRoot} PROJECT_NAME=${project} PM2_DIR=${ctProjectRoot} bash ${ctEcoRoot}/configure.sh`
  );
  if (dedicatedRustBuilder && services.some((service) => service.runtimes.includes("rust"))) {
    const builderCtid = await buildRustInDedicatedCt({
      builderInput: dedicatedRustBuilder,
      appCtid: ctid,
      estateRoot: projectDir,
      project,
      services,
      ctWorkspaceRoot,
      ctProjectRoot,
      projectDir
    });
    printStep(`[CT ${ctid}] Rewriting PM2 config to use Rust artifacts from builder CT ${builderCtid}`);
    await pctExec(
      ctid,
      `cd ${ctWorkspaceRoot} && ECO_DEPLOY_MODE=prod ECO_NON_INTERACTIVE=1 PROJECT_DIR=${ctProjectRoot} PROJECT_NAME=${project} PM2_DIR=${ctProjectRoot} bash ${ctEcoRoot}/configure.sh`
    );
  }
  if (githubDeploy) {
    githubDeploy.port = await resolveEstateWebhookPort({ ctid, ctProjectRoot, githubDeploy });
    printStep(`[CT ${ctid}] Allocated deploy webhook port ${githubDeploy.port}`);
    receiverSetup = buildDeployReceiverFiles({
      project, projectDir, ctProjectRoot, ctProjectParent, ctWorkspaceRoot,
      ctEcoRoot, ctConfigPath, githubDeploy, githubRepos, webhookSecret,
      dependencyInstallSteps, dataBootstrapSteps, migrationSteps, buildSteps,
      services: Object.assign(services, {
        _targetMode: (content.match(/^target_mode:\s*(\S+)/m) || [])[1] || ""
      }),
      usesDedicatedRustBuilder: Boolean(dedicatedRustBuilder),
      rustBuilderIsApplication,
      staging,
      stagingEcomposeContent
    });
  }
  // Everything above this point is foundational (CT exists, repos synced,
  // runtimes provisioned, ecosystem config generated) -- if any of that
  // fails there's nothing meaningful left to build or expose, so it stays
  // a hard stop. Everything below is best-effort: a failure in one step
  // (a flaky data-bootstrap restart, a corrupted local build cache, PM2
  // erroring on one service) shouldn't prevent the *other* independent
  // steps from running -- and specifically shouldn't ever prevent the
  // final expose step, which is what actually makes the estate reachable
  // and has no real dependency on the build/PM2 steps succeeding. Before
  // this, a single failure anywhere in this tail threw and aborted the
  // whole function silently, which is exactly what left this estate
  // unreachable for hours despite `eco expose` being fully able to fix it
  // on its own once run.
  const failures = [];
  async function softStep(label, fn) {
    try {
      await fn();
    } catch (error) {
      failures.push({ label, error });
      process.stderr.write(`\n[eco up] [CT ${ctid}] WARNING: ${label} failed, continuing: ${error.message}\n\n`);
    }
  }

  for (const step of buildSteps) {
    await softStep(`Building artifact: ${step.name} (${step.path})`, async () => {
      printStep(`[CT ${ctid}] Building artifact: ${step.name} (${step.path})`);
      await pctExec(ctid, `cd ${ctWorkspaceRoot} && ${step.command}`);
    });
  }
  await softStep(`Starting PM2 services for ${project}`, async () => {
    printStep(`[CT ${ctid}] Starting PM2 services for ${project}`);
    await pctExec(ctid, buildDeleteDeclaredPm2AppsCommand(ctConfigPath));
    await pctExec(ctid, buildPruneConflictingPortsCommand(ctConfigPath));
    await pctExec(ctid, `pm2 startOrReload ${ctConfigPath} --update-env`);
  });
  if (receiverSetup && githubDeploy) {
    await softStep(`Installing deploy webhook receiver for ${project}`, async () => {
      printStep(`[CT ${ctid}] Installing deploy webhook receiver for ${project}${staging ? " (staging)" : ""}`);
      await installDeployReceiver({ ctid, receiverSetup });
      const appIp = await resolveCtPrimaryIp(ctid);
      await ensureProxyHostname({
        dryRun: false,
        proxyCtInput: githubDeploy.proxyCtInput,
        hostname: githubDeploy.webhookHostname,
        serviceUrl: `http://${appIp}:${githubDeploy.port}`,
        cloudflaredConfig: expose.cloudflared_config,
        tunnelName: webhookTunnelName,
        cloudflareAccount: expose.cloudflare_account
      });
      printStep(`[CT ${ctid}] Syncing GitHub webhooks for ${project}`);
      await syncGithubDeployWebhooks({ githubRepos, githubDeploy, webhookSecret });
    });
  }
  await softStep(`Exposing ${project} via proxy CT`, async () => {
    await exposeViaProxyCt({
      dryRun: false,
      expose,
      project,
      appCtid: ctid,
      appConfigPath: ctConfigPath,
      ctWorkspaceRoot
    });
  });

  if (failures.length > 0) {
    process.stdout.write(`\n[eco up] Completed ${project} with ${failures.length} failed step(s):\n`);
    for (const failure of failures) {
      process.stdout.write(`  - ${failure.label}: ${failure.error.message}\n`);
    }
    process.stdout.write(`\nEverything else succeeded, including exposing the estate if enabled. Fix the failed step(s) above and re-run 'eco up' (or the specific step manually) to clear them.\n`);
    process.exitCode = 1;
  } else {
    printStep(`Completed ${project}`);
  }
}
