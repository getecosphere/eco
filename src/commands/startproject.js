import { access, copyFile, mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { spawn } from "node:child_process";
import path from "node:path";
import readline from "node:readline";
import { fileURLToPath } from "node:url";
import { stdin as input, stdout as output } from "node:process";

import { buildRepoDependencyMaps, confirmWithSingleKey, runChecklist } from "../lib/checklist.js";
import { parseServices, renderDomainEntry } from "../lib/ecompose.js";
import { readRepoCatalog } from "../lib/repos.js";
import { findWorkspaceRoot } from "../lib/workspace.js";

const DEFAULT_CT = {
  id: 0,
  // Custom base image built via `eco ct template` (see eco/README.md's
  // "Custom CT Template Strategy") -- a plain Debian 12 CT plus git/curl/
  // jq/ca-certificates/openssh-client, Node+npm, PM2, Rust+cargo+a C
  // toolchain, sccache (warm cache from whatever's already been compiled
  // against it), and MongoDB installed already. Strictly a superset of the
  // plain Debian base: provision.sh still installs whatever else a given
  // project's ecompose.yml additionally declares (e.g. PostgreSQL for a
  // more complex project), it just skips reinstalling what's already here.
  // Falls back to a plain Debian base automatically if this exact archive
  // isn't present on a given Proxmox host/storage -- see ensureCtTemplate.
  template: "local:vztmpl/eco-npm-rust-mongo_1_amd64.tar.zst",
  fallbackTemplate: "local:vztmpl/debian-12-standard_12.12-1_amd64.tar.zst",
  storage: "local-lvm",
  disk: 16,
  bridge: "vmbr0",
  ip: "dhcp",
  cores: 2,
  memory: 4096,
  swap: 1024,
  unprivileged: 1
};

const DEFAULT_SHARED_TOOLS = ["git", "openssh-client", "curl", "jq", "ca-certificates"];

const moduleDir = path.dirname(fileURLToPath(import.meta.url));
const ECOLOGY_MARK_PATH = path.resolve(moduleDir, "../../assets/ecology-mark.webp");

// The generated composition is intentionally small, but runnable. Keeping
// these two defaults fixed means its first `eco up` needs no language decision
// and gives every new estate the same reliable proof of the complete path.
const COMPOSITION_STARTERS = {
  frontend: { id: "node", label: "Node.js", runtimes: ["node@20", "npm", "pm2"] },
  backend: { id: "rust", label: "Rust", runtimes: ["rust"] }
};


async function pathExists(targetPath) {
  try {
    await access(targetPath);
    return true;
  } catch {
    return false;
  }
}

function runCommand(command, args, cwd = process.cwd()) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd,
      stdio: "inherit",
      env: process.env
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

function hasStagedChanges(cwd) {
  return new Promise((resolve, reject) => {
    const child = spawn("git", ["diff", "--cached", "--quiet"], {
      cwd,
      stdio: "inherit",
      env: process.env
    });
    child.on("error", reject);
    child.on("exit", (code, signal) => {
      if (signal) {
        reject(new Error(`git diff terminated by signal ${signal}`));
      } else if (code === 0) {
        resolve(false);
      } else if (code === 1) {
        resolve(true);
      } else {
        reject(new Error(`git diff exited with code ${code}`));
      }
    });
  });
}

async function githubRequest(pathname, { token, method = "GET", body, allowNotFound = false } = {}) {
  const response = await fetch(`https://api.github.com${pathname}`, {
    method,
    headers: {
      Accept: "application/vnd.github+json",
      Authorization: `Bearer ${token}`,
      "User-Agent": "eco-cli",
      "X-GitHub-Api-Version": "2022-11-28",
      ...(body ? { "Content-Type": "application/json" } : {})
    },
    body: body ? JSON.stringify(body) : undefined
  });

  const text = await response.text();
  const payload = text ? JSON.parse(text) : null;
  if (allowNotFound && response.status === 404) {
    return null;
  }
  if (!response.ok) {
    const message = payload?.message || text || `GitHub API request failed: ${response.status}`;
    throw new Error(`GitHub API request failed for ${pathname}: ${message}`);
  }
  return payload;
}

async function inspectGithubRepositories(names) {
  const token = process.env.ECO_GITHUB_API_KEY;
  const user = await githubRequest("/user", { token });
  return Promise.all(names.map(async (name) => {
    const repository = await githubRequest(`/repos/${user.login}/${name}`, { token, allowNotFound: true });
    return { name, exists: Boolean(repository), repository, login: user.login };
  }));
}

async function createGithubRepository(name) {
  const token = process.env.ECO_GITHUB_API_KEY;
  if (!token) {
    throw new Error("ECO_GITHUB_API_KEY is required to create and push project repositories.");
  }

  return githubRequest("/user/repos", {
    token,
    method: "POST",
    body: {
      name,
      private: true,
      auto_init: false,
      description: `eco-managed repository for ${name}`
    }
  });
}

function authenticatedGithubUrl(repository) {
  const token = process.env.ECO_GITHUB_API_KEY;
  return repository.clone_url.replace(
    "https://",
    `https://x-access-token:${encodeURIComponent(token)}@`
  );
}

// The composition repository's git address is written into ecompose.yml
// (never eco/repos.json -- the catalog is for reusable domains). Its URL is
// deterministic: same GitHub login, `<project>_composition` name. When the
// repo already exists the plan carries its actual remote; otherwise derive
// the same ssh form initialiseAndPushRepository will set as origin.
function compositionGitUrl(plan) {
  if (plan?.repository?.ssh_url) return plan.repository.ssh_url;
  if (plan?.repository?.clone_url) return plan.repository.clone_url;
  return `git@github.com:${plan?.login}/${plan?.name}.git`;
}

async function cloneAndClearRepository(directory, repository) {
  await runCommand("git", ["clone", authenticatedGithubUrl(repository), directory]);
  // Remove tracked and untracked content, preserving only .git so the new
  // scaffold replaces the repository's working tree in its existing history.
  await runCommand("git", ["rm", "-r", "--ignore-unmatch", "."], directory);
  await runCommand("git", ["clean", "-fdx"], directory);
}

async function initialiseAndPushRepository(directory, repositoryName, commitMessage, existingRepository = null) {
  if (!existingRepository) {
    await runCommand("git", ["init", "--initial-branch=main"], directory);
  }
  const repository = existingRepository || await createGithubRepository(repositoryName);
  const authenticatedPushUrl = authenticatedGithubUrl(repository);
  await runCommand("git", ["add", "."], directory);
  if (!(await hasStagedChanges(directory))) {
    output.write(`  ${repositoryName} already matches the generated scaffold; no commit needed.\n`);
    if (existingRepository) {
      await runCommand("git", ["remote", "set-url", "origin", repository.ssh_url || repository.clone_url], directory);
    }
    return repository;
  }
  await runCommand("git", ["commit", "-m", commitMessage], directory);
  if (existingRepository) {
    await runCommand("git", ["remote", "set-url", "origin", authenticatedPushUrl], directory);
  } else {
    await runCommand("git", ["remote", "add", "origin", authenticatedPushUrl], directory);
  }
  await runCommand("git", ["push", "-u", "origin", "main"], directory);
  // Do not leave the credential-bearing remote URL in .git/config.
  await runCommand("git", ["remote", "set-url", "origin", repository.ssh_url || repository.clone_url], directory);
  return repository;
}

function createPrompt() {
  const rl = readline.createInterface({ input, output });
  return {
    ask(question) {
      return new Promise((resolve) => rl.question(question, resolve));
    },
    close() {
      rl.close();
      if (typeof input.pause === "function") {
        input.pause();
      }
    }
  };
}

function parseFlags(args) {
  const flags = {
    yes: false,
    hostname: null,
    cloudflareAccount: null,
    ctId: null,
    stagingCt: null,
    noDeploy: false,
    noStorage: false,
    noStaging: false,
    noEmailVerification: false,
    repos: [],
    branchOverrides: {},
    remaining: []
  };

  let i = 0;
  while (i < args.length) {
    const arg = args[i];
    if (arg === "--yes" || arg === "-y") {
      flags.yes = true;
      i++;
    } else if ((arg === "--hostname" || arg === "-H") && i + 1 < args.length) {
      flags.hostname = args[++i];
      i++;
    } else if ((arg === "--cloudflare-account" || arg === "-c") && i + 1 < args.length) {
      flags.cloudflareAccount = args[++i];
      i++;
    } else if (arg === "--ct-id" && i + 1 < args.length) {
      flags.ctId = parseInt(args[++i], 10);
      i++;
    } else if (arg === "--staging-ct" && i + 1 < args.length) {
      flags.stagingCt = parseInt(args[++i], 10);
      i++;
    } else if (arg === "--no-deploy") {
      flags.noDeploy = true;
      i++;
    } else if (arg === "--no-storage") {
      flags.noStorage = true;
      i++;
    } else if (arg === "--no-staging") {
      flags.noStaging = true;
      i++;
    } else if (arg === "--no-email-verification") {
      flags.noEmailVerification = true;
      i++;
    } else if (arg === "--repo" && i + 1 < args.length) {
      flags.repos.push(args[++i]);
      i++;
    } else if (arg.startsWith("--branch=")) {
      const rest = arg.slice(9);
      const eqIdx = rest.indexOf("=");
      if (eqIdx >= 0) {
        const repo = rest.slice(0, eqIdx);
        const branch = rest.slice(eqIdx + 1);
        if (repo && branch) flags.branchOverrides[repo] = branch;
      }
      i++;
    } else if (arg === "--branch" && i + 1 < args.length) {
      const branchArg = args[++i];
      const eqIdx2 = branchArg.indexOf("=");
      if (eqIdx2 >= 0) {
        const repo = branchArg.slice(0, eqIdx2);
        const branch = branchArg.slice(eqIdx2 + 1);
        if (repo && branch) flags.branchOverrides[repo] = branch;
      }
      i++;
    } else {
      flags.remaining.push(arg);
      i++;
    }
  }

  return flags;
}

async function promptProjectName(args, workspaceRoot) {
  const [projectArg] = args;

  if (projectArg === ".") {
    return {
      projectName: path.basename(process.cwd()),
      targetRoot: process.cwd(),
      currentDirMode: true
    };
  }

  if (projectArg) {
    return {
      projectName: projectArg,
      targetRoot: path.join(workspaceRoot, projectArg),
      currentDirMode: false
    };
  }

  const prompt = createPrompt();
  try {
    const answer = ((await prompt.ask("Project name: ")) || "").trim();
    if (!answer) {
      throw new Error("Project name is required.");
    }
    return {
      projectName: answer,
      targetRoot: path.join(workspaceRoot, answer),
      currentDirMode: false
    };
  } finally {
    prompt.close();
  }
}

async function runRepoChecklist(repoCatalog, projectName, nonInteractive = false, preselectedRepos = []) {
  if (nonInteractive) {
    const available = repoCatalog
      .filter((repo) => repo.name !== "eco")
      .map((repo) => repo.name);
    const selected = preselectedRepos.filter((name) => available.includes(name));
    if (selected.length === 0) {
      output.write("No repos selected (non-interactive).\n");
      return [];
    }
    const resolved = computeDependencyClosure(selected, repoCatalog);
    output.write(`\nSelected repos (non-interactive): ${resolved.join(", ")}\n\n`);
    return resolved;
  }

  const items = repoCatalog
    .filter((repo) => repo.name !== "eco")
    .map((repo) => ({ id: repo.name, label: `${repo.name} - ${repo.description ?? "No description"}` }));
  const { requiresByRepo, requiredByRepo } = buildRepoDependencyMaps(repoCatalog);

  const hint = [
    "Controls: ↑/↓ move, x or space toggle, Enter confirm",
    "",
    "  If a repo is not in this list, it means you are creating a new domain project.",
    "  Navigate into that domain repo directory and run: eco repos add",
    "  It will then appear here."
  ].join("\n");

  return runChecklist({
    items,
    title: `Select repos for ${projectName} (at least one required)`,
    hint,
    requiresByRepo,
    requiredByRepo,
    minSelected: 1
  });
}

async function promptCompositionServices(projectName, nonInteractive = false) {
  if (nonInteractive) {
    output.write("\nComposition starter (non-interactive)\n");
    output.write("  frontend: Node.js (always included)\n");
    output.write("  backend: Rust (always included)\n\n");
    return ["frontend", "backend"];
  }
  return runChecklist({
    items: [
      { id: "frontend", label: "frontend — required Node.js public application" },
      { id: "backend", label: "backend — optional Rust project API" }
    ],
    title: `Select composition services for ${projectName}`,
    hint: "Controls: ↑/↓ move, x or space toggle, Enter confirm\n\n  frontend is required, receives the estate's first port, and starts as a runnable Eco guide.",
    initialSelected: ["frontend"],
    lockedIds: ["frontend"],
    minSelected: 1
  });
}

async function promptSuperadminSetup(nonInteractive = false) {
  if (nonInteractive) {
    return false;
  }
  output.write("\nSuperadmin / Setup flow\n");
  output.write("  When enabled, a fresh deployment shows a /setup page before anything\n");
  output.write("  else. The first visitor claims the superadmin role, then the app\n");
  output.write("  switches to normal login. Modeled after apindo's setup flow:\n");
  output.write("  GET /setup/status → POST /setup/claim.\n\n");
  return confirmWithSingleKey("  Enable superadmin setup flow?", { defaultYes: false });
}

async function promptBackendDatabases(selectedCompositionServices, nonInteractive = false) {
  if (!selectedCompositionServices.includes("backend")) {
    return [];
  }

  if (nonInteractive) {
    output.write("Backend databases (non-interactive): MongoDB 7\n\n");
    return ["mongodb@7"];
  }

  return runChecklist({
    items: [
      { id: "mongodb@7", label: "MongoDB 7" },
      { id: "postgresql@15", label: "PostgreSQL 15" }
    ],
    title: "Select backend databases (optional)",
    hint: "Controls: ↑/↓ move, x or space toggle, Enter confirm\n\n  Select every database this backend needs. Eco provisions the selected runtimes.",
    minSelected: 0
  });
}

function buildCompositionServices(selectedCompositionServices, backendDatabases) {
  return selectedCompositionServices.map((serviceName) => {
    const language = COMPOSITION_STARTERS[serviceName];
    return {
      name: serviceName,
      path: serviceName,
      language: language.id,
      runtimes: [
        ...language.runtimes,
        ...(serviceName === "backend" ? backendDatabases : [])
      ]
    };
  });
}

function computeDependencyClosure(selectedRepos, repoCatalog) {
  const byName = new Map(repoCatalog.map((repo) => [repo.name, repo]));
  const resolved = new Set(selectedRepos);
  const stack = [...selectedRepos];

  while (stack.length > 0) {
    const current = stack.pop();
    const repo = byName.get(current);
    const requires = Array.isArray(repo?.requires) ? repo.requires : [];
    for (const dependency of requires) {
      if (!resolved.has(dependency)) {
        resolved.add(dependency);
        stack.push(dependency);
      }
    }
  }

  return [...resolved].sort((left, right) => left.localeCompare(right));
}

function capitaliseProjectName(projectName) {
  return projectName ? `${projectName.charAt(0).toUpperCase()}${projectName.slice(1)}` : "Eco";
}

// Auth is the first domain with estate-specific, operator-supplied settings.
// Keep the prompt here rather than burying it in a generic service question so
// a new operator understands why registration email needs a sender and a
// transactional-email credential. Other domains can add their own focused
// configuration sections using the same pattern later.
async function promptAuthEmailVerification(selectedRepos, projectName, nonInteractive = false, noEmailVerification = false) {
  if (!selectedRepos.includes("auth")) {
    return null;
  }

  if (nonInteractive) {
    output.write("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    output.write("  Auth domain — registration email verification (non-interactive)\n");
    output.write("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    if (noEmailVerification) {
      output.write("  Email verification disabled.\n");
      return { required: false };
    }
    const defaultMailFromName = capitaliseProjectName(projectName);
    output.write(`  Verification: enabled\n`);
    output.write(`  MAIL_FROM_EMAIL: no-reply@jogjaitcamp.com\n`);
    output.write(`  MAIL_FROM_NAME: ${defaultMailFromName}\n`);
    output.write(`  EMAIL_VERIFICATION_TTL_HOURS: 24\n`);
    output.write(`  BREVO_API_KEY: (set in .env after scaffold)\n\n`);
    return { required: true, mailFromEmail: "no-reply@jogjaitcamp.com", mailFromName: defaultMailFromName, brevoApiKey: "", ttlHours: 24 };
  }

  output.write("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
  output.write("  Auth domain — registration email verification\n");
  output.write("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
  output.write("  Auth can require people to verify their registration email before\n");
  output.write("  they use features that connect them with other people. Eco writes\n");
  output.write("  the non-secret defaults into this estate's manifest and configures\n");
  output.write("  the local Auth .env. The Brevo key remains local and is never committed.\n\n");

  const verifyRegistrationEmail = await confirmWithSingleKey("  Verify registration email", { defaultYes: true });
  if (!verifyRegistrationEmail) {
    return { required: false };
  }

  const prompt = createPrompt();
  try {
    const defaultMailFromName = capitaliseProjectName(projectName);
    const mailFromEmail = (await prompt.ask("  MAIL_FROM_EMAIL [no-reply@jogjaitcamp.com]: ")).trim() || "no-reply@jogjaitcamp.com";
    const mailFromName = (await prompt.ask(`  MAIL_FROM_NAME [${defaultMailFromName}]: `)).trim() || defaultMailFromName;
    const brevoApiKey = (await prompt.ask("  BREVO_API_KEY (Paste given BREVO API KEY): ")).trim();
    const ttlRaw = (await prompt.ask("  EMAIL_VERIFICATION_TTL_HOURS [24]: ")).trim();
    const ttlHours = ttlRaw ? parseInt(ttlRaw, 10) : 24;
    if (!Number.isInteger(ttlHours) || ttlHours <= 0) {
      throw new Error("EMAIL_VERIFICATION_TTL_HOURS must be a positive whole number.");
    }
    return { required: true, mailFromEmail, mailFromName, brevoApiKey, ttlHours };
  } finally {
    prompt.close();
  }
}

async function discoverServiceTemplates(workspaceRoot) {
  const templates = new Map();
  const estateRoots = await readdir(workspaceRoot, { withFileTypes: true });

  for (const estateEntry of estateRoots) {
    if (!estateEntry.isDirectory() || estateEntry.name === "eco" || estateEntry.name === "core") {
      continue;
    }

    const estateRoot = path.join(workspaceRoot, estateEntry.name);
    const projectDirs = await readdir(estateRoot, { withFileTypes: true });
    for (const projectDir of projectDirs) {
      if (!projectDir.isDirectory()) {
        continue;
      }

      const ecomposePath = path.join(estateRoot, projectDir.name, "ecompose.yml");
      if (!(await pathExists(ecomposePath))) {
        continue;
      }

      const content = await readFile(ecomposePath, "utf8");
      for (const service of parseServices(content)) {
        const repoName = service.path.split("/")[0];
        if (!repoName) {
          continue;
        }
        if (!templates.has(repoName)) {
          templates.set(repoName, []);
        }

        const current = templates.get(repoName);
        if (!current.some((entry) => entry.name === service.name && entry.path === service.path)) {
          current.push(service);
        }
      }
    }
  }

  return templates;
}

function renderServiceBlock(service) {
  const runtimeLines = (service.runtimes || []).map((runtime) => `      - ${runtime}`).join("\n");
  return [
    `  ${service.name}:`,
    `    path: ${service.path}`,
    "    runtimes:",
    runtimeLines
  ].join("\n");
}


// eco/repos.json's branch is deliberately always "main" -- the shared
// catalog every estate composes from, not any one estate's working
// state. This estate's ecompose.yml can override the branch for one or
// more selected repos (e.g. testing a feature/rewrite branch before it
// merges to main) without touching that shared catalog or affecting any
// other estate composing the same repos. Casual case (no overrides
// needed): press Enter through every prompt, everything stays on each
// repo's catalog default.
//
// After the branch, each repo also gets an environment placement: prod is
// mandatory (locked checkbox), dev is optional and defaults to checked.
// Unchecking dev records the domain as `dev: disabled` (prod-only on this
// estate); leaving it checked records `dev: optional`, so `eco up dev`
// can skip the domain gracefully when the machine can't run its runtimes.
async function promptRepoPlacements(selectedRepos, repoCatalog, nonInteractive = false, branchOverridesInput = {}) {
  const byName = new Map(repoCatalog.map((repo) => [repo.name, repo]));
  const branchOverrides = {};
  const devFlags = {};

  if (nonInteractive) {
    output.write("\nRepo branches & environments (non-interactive)\n");
    for (const repoName of selectedRepos) {
      const repo = byName.get(repoName);
      const defaultBranch = repo?.branch || "main";
      const branchOverride = branchOverridesInput[repoName];
      if (branchOverride && branchOverride !== defaultBranch) {
        branchOverrides[repoName] = branchOverride;
        output.write(`  ${repoName}: branch=${branchOverride}, dev=optional, prod=enabled\n`);
      } else {
        output.write(`  ${repoName}: branch=${defaultBranch}, dev=optional, prod=enabled\n`);
      }
      devFlags[repoName] = "optional";
    }
    output.write("\n");
    return { branchOverrides, devFlags };
  }

  const prompt = createPrompt();
  try {
    output.write("\nRepo branches & environments\n");
    output.write("  Press Enter to accept each repo's default branch. Only type a\n");
    output.write("  different branch if this estate specifically needs to track one --\n");
    output.write("  e.g. testing a rewrite/feature branch before it merges to main.\n");
    output.write("  This only affects this estate's ecompose.yml, never eco/repos.json's\n");
    output.write("  shared default used by other estates.\n");
    output.write("  Then choose where the repo runs: prod is mandatory (locked); dev is\n");
    output.write("  optional and defaults to checked.\n\n");
    for (const repoName of selectedRepos) {
      const repo = byName.get(repoName);
      const defaultBranch = repo?.branch || "main";
      const input = (await prompt.ask(`  ${repoName} branch [${defaultBranch}]: `)).trim();
      if (input && input !== defaultBranch) {
        branchOverrides[repoName] = input;
      }
      const selection = await runChecklist({
        items: [
          { id: "dev", label: `${repoName} in local dev (optional)` },
          { id: "prod", label: `${repoName} in prod (mandatory)` }
        ],
        title: `  ${repoName} environments`,
        hint: "  Controls: ↑/↓ move, space toggle, Enter confirm",
        initialSelected: ["dev", "prod"],
        lockedIds: ["prod"],
        minSelected: 1
      });
      devFlags[repoName] = selection.includes("dev") ? "optional" : "disabled";
    }
  } finally {
    prompt.close();
  }
  return { branchOverrides, devFlags };
}

function buildEcomposeContent({
  projectName,
  selectedRepos,
  serviceTemplates,
  compositionServices,
  details,
  authEmailVerification,
  branchOverrides = {},
  devFlags = {},
  compositionGit = "",
  superadminSetup = false
}) {
  const lines = [
    `project: ${projectName}`,
    "",
    "ct:",
    `  id: ${details.ctId}`,
    `  hostname: ${projectName}`,
    `  template: ${DEFAULT_CT.template}`,
    `  storage: ${DEFAULT_CT.storage}`,
    `  disk: ${DEFAULT_CT.disk}`,
    `  bridge: ${DEFAULT_CT.bridge}`,
    `  ip: ${DEFAULT_CT.ip}`,
    `  cores: ${DEFAULT_CT.cores}`,
    `  memory: ${DEFAULT_CT.memory}`,
    `  swap: ${DEFAULT_CT.swap}`,
    `  unprivileged: ${DEFAULT_CT.unprivileged}`,
    "",
    "expose:",
    "  enabled: true",
    `  hostname: ${details.hostname}`,
    `  service: ${details.exposeService}`,
    "  proxy_ct: proxy",
    `  cloudflare_account: ${details.cloudflareAccount}`,
    ""
  ];

  if (details.deployEnabled) {
    lines.push(
      "deploy:",
      "  github:",
      "    enabled: true",
      `    branch: ${details.deployBranch}`,
      "    debounce_ms: 15000",
      "    webhook_path: /__eco/github/deploy",
      ""
    );
  } else {
    lines.push(
      "deploy:",
      "  github:",
      "    enabled: false",
      ""
    );
  }

  if (details.stagingEnabled) {
    lines.push(
      "# Staging footprint: a second deployment on a separate CT, exposed at",
      "# a staging-<hostname> and redeployed by a webhook that accepts any",
      "# branch except the prod deploy branch (see docs/cicd.md).",
      "staging:",
      `  ct: ${details.stagingCt}`,
      ""
    );
  }

  if (authEmailVerification) {
    lines.push(
      "# Auth domain settings. Credentials stay in auth/backend/.env and are never committed.",
      "auth:",
      "  email_verification:",
      `    enabled: ${authEmailVerification.required ? "true" : "false"}`
    );
    if (authEmailVerification.required) {
      lines.push(
        `    ttl_hours: ${authEmailVerification.ttlHours}`,
        `    mail_from_email: ${authEmailVerification.mailFromEmail}`,
        `    mail_from_name: ${JSON.stringify(authEmailVerification.mailFromName)}`
      );
    }
    lines.push("");
  }

  if (superadminSetup) {
    lines.push(
      "# When enabled, a fresh deployment shows a /setup page that forces the first",
      "# visitor to claim the superadmin role. Modeled after apindo's setup flow:",
      "# GET  /api/v1/setup/status → check if any admin exists",
      "# POST /api/v1/setup/claim  → create the initial superadmin",
      "setup:",
      "  superadmin: true",
      ""
    );
  }

  if (details.storage) {
    lines.push(
      "# Eco manages MinIO credentials and resolves this CT's private bridge",
      "# address at `eco up`; never commit endpoint or credentials here.",
      "storage:",
      "  minio:",
      `    ct: ${details.storage.ct}`,
      `    region: ${details.storage.region}`,
      ""
    );
  }

  lines.push("shared_tools:");
  for (const tool of DEFAULT_SHARED_TOOLS) {
    lines.push(`  - ${tool}`);
  }

  if (compositionGit) {
    lines.push(
      "",
      "# The composition repository is this estate's own project repo, not a",
      "# shared catalog domain: its git address lives here so a fresh host can",
      "# clone it without committing a project-specific entry to eco/repos.json.",
      "composition:",
      `  git: ${compositionGit}`,
      "  branch: main",
      ""
    );
  }

  lines.push("", "domains:", `  - ${projectName}_composition`);
  for (const repoName of selectedRepos) {
    lines.push(renderDomainEntry(repoName, { branch: branchOverrides[repoName], dev: devFlags[repoName] }));
  }

  lines.push("", "services:");

  const emitted = new Set();
  // The frontend must stay first: configure.sh assigns the first port to
  // the first declared service unless an existing port explicitly overrides it.
  for (const service of compositionServices) {
    const compositionService = {
      ...service,
      path: `${projectName}_composition/${service.path}`
    };
    lines.push(renderServiceBlock(compositionService), "");
    emitted.add(`${compositionService.name}:${compositionService.path}`);
  }

  for (const repoName of selectedRepos) {
    const templates = serviceTemplates.get(repoName) || [];
    for (const service of templates) {
      const key = `${service.name}:${service.path}`;
      if (emitted.has(key)) {
        continue;
      }
      lines.push(renderServiceBlock(service), "");
      emitted.add(key);
    }
  }

  return `${lines.join("\n")}\n`;
}

function buildGitignoreContent() {
  return [
    "# eco-generated runtime files",
    "ecosystem.config.js",
    ".configure-state",
    "",
    "# environment secrets",
    ".env",
    ".env.local",
    ".env.*.local",
    "",
    "# dependencies",
    "node_modules/",
    "",
    "# build output",
    "dist/",
    "build/",
    ".next/",
    "",
    "# logs",
    "*.log",
    "logs/",
    "",
    "# OS",
    ".DS_Store",
    "Thumbs.db",
    "",
    "# editors",
    ".idea/",
    "*.iml",
    "",
    "# AI agent artifacts",
    ".claude/",
    ".codegraph/",
    ".kiro/",
    ".cursor/",
    ".codeium/",
    ".copilot/",
    ".aider*",
    ".continue/",
    ""
  ].join("\n");
}

function buildClaudeContent(projectName) {
  return `# ${projectName}_bootstrap

Estate manifest for the ${projectName} project. Created by \`eco startproject\`.

This is the only repo that needs to be cloned on the Proxmox host before running \`eco up\`.
All domain repos are declared in \`ecompose.yml\` and will be cloned automatically by eco.
`;
}

function buildCompositionReadme(projectName, compositionServices) {
  const serviceList = compositionServices.map((service) => `- \`${service.path}/\``).join("\n");
  return `# ${projectName}_composition

The composition layer for the ${projectName} estate. It owns the project-specific user experience and optional project API.

## Services

${serviceList}
`;
}

function buildServiceGitignoreContent(language) {
  const languageEntries = {
    node: ["node_modules/", "dist/", "build/", ".next/", ".nuxt/", "coverage/", "*.tsbuildinfo", "npm-debug.log*", "yarn-debug.log*", "pnpm-debug.log*"],
    rust: ["/target/", "**/*.rs.bk"],
    java: ["target/", "*.class", ".gradle/", "build/", "out/"],
    go: ["/bin/", "*.test", "coverage.out"]
  };
  const commonEntries = [".env", ".env.local", ".env.*.local", "*.log", "logs/", ".DS_Store", "Thumbs.db", ".idea/", ".vscode/", ".claude/", ".codegraph/", ".cursor/"];
  return [`# ${language} project artifacts`, ...(languageEntries[language] || []), "", "# Local configuration and tooling", ...commonEntries, ""].join("\n");
}

function buildServiceEnvExampleContent(service) {
  if (service.name === "frontend") {
    return [
      "# Assigned by eco configure. Do not hard-code a port in application source.",
      "PORT=",
      "",
      "# Assigned by eco configure to the composed backend API.",
      "# Local: an allocated loopback URL. Production: the public gateway route.",
      "PUBLIC_API_URL=",
      ""
    ].join("\n");
  }

  if (service.language !== "rust") {
    return null;
  }

  return [
    "# Assigned by eco configure. Do not hard-code a port in application source.",
    "SERVER_PORT=",
    "",
    "# Public route prefix implemented by this Rust API.",
    "# Change this only when the application uses a different API prefix.",
    "API_BASE_PATH=/api",
    "",
    "# Eco replaces this with the public application origin in production and",
    "# the allocated frontend URL in local development.",
    "CORS_ALLOWED_ORIGINS=",
    ""
  ].join("\n");
}

function buildStarterFrontendPackageJson(projectName) {
  return `${JSON.stringify({
    name: `${projectName}-frontend`,
    version: "0.1.0",
    private: true,
    scripts: { start: "node index.js" }
  }, null, 2)}\n`;
}

function buildStarterFrontendServer() {
  return `const http = require("node:http");
const { readFile } = require("node:fs/promises");
const { readFileSync } = require("node:fs");
const path = require("node:path");

const root = __dirname;
const envFile = (() => { try { return Object.fromEntries(readFileSync(path.join(root, ".env"), "utf8").split(/\\r?\\n/).filter((line) => line && !line.startsWith("#")).map((line) => { const index = line.indexOf("="); return index < 0 ? [line, ""] : [line.slice(0, index), line.slice(index + 1)]; })); } catch { return {}; } })();
const port = Number(process.env.PORT || envFile.PORT);
const backendUrl = process.env.PUBLIC_API_URL || envFile.PUBLIC_API_URL;

if (!Number.isInteger(port) || port < 1 || port > 65535) {
  throw new Error("PORT is required. Run eco configure so Eco can assign this service port.");
}
if (!backendUrl) {
  throw new Error("PUBLIC_API_URL is required. Run eco configure so Eco can connect this frontend to its backend.");
}

const files = {
  "/": { file: "index.html", type: "text/html; charset=utf-8" },
  "/index.html": { file: "index.html", type: "text/html; charset=utf-8" },
  "/images/ecology-mark.webp": { file: "images/ecology-mark.webp", type: "image/webp" },
  "/runtime-config.js": { type: "application/javascript; charset=utf-8" }
};

http.createServer(async (request, response) => {
  const pathname = new URL(request.url, "http://" + (request.headers.host || "localhost")).pathname;
  const requested = files[pathname];
  if (!requested) {
    response.writeHead(404, { "content-type": "text/plain; charset=utf-8" });
    response.end("Not found");
    return;
  }
  if (pathname === "/runtime-config.js") {
    response.writeHead(200, { "content-type": requested.type, "cache-control": "no-store" });
    response.end("window.__ECO_BACKEND_URL__ = " + JSON.stringify(backendUrl) + ";");
    return;
  }
  try {
    response.writeHead(200, { "content-type": requested.type, "cache-control": "no-cache" });
    response.end(await readFile(path.join(root, requested.file)));
  } catch {
    response.writeHead(500, { "content-type": "text/plain; charset=utf-8" });
    response.end("Starter asset is unavailable.");
  }
}).listen(port, "0.0.0.0", () => {
  console.log("Eco starter frontend listening on http://0.0.0.0:" + port);
});
`;
}

function buildStarterFrontendHtml(projectName) {
  return `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <meta name="description" content="A practical introduction to Eco and Domain-Driven Design." />
  <title>${projectName} · Eco starter</title>
  <style>
    :root { color-scheme: light; --ink:#10203d; --muted:#5f718c; --line:#dce5f0; --paper:#fffdf9; --soft:#f4f7fb; --blue:#3566bf; --blue-soft:#dce9fc; --cream:#fff0d9; --green:#2a8b69; }
    * { box-sizing:border-box; } body { margin:0; background:var(--soft); color:var(--ink); font:16px/1.55 Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }
    .shell { max-width:1120px; margin:0 auto; padding:clamp(24px,5vw,72px) 24px 56px; }
    .hero { display:grid; grid-template-columns:minmax(0,1.2fr) minmax(240px,.8fr); gap:clamp(28px,6vw,80px); align-items:center; padding:clamp(28px,6vw,72px); background:var(--paper); border:1px solid var(--line); border-radius:28px; box-shadow:0 18px 44px rgba(31,57,95,.08); }
    .eyebrow { margin:0 0 14px; color:var(--blue); font-size:.78rem; font-weight:800; letter-spacing:.13em; text-transform:uppercase; }
    h1 { max-width:12ch; margin:0; font-size:clamp(2.5rem,6vw,5.2rem); line-height:.96; letter-spacing:-.065em; } h2 { margin:0 0 12px; font-size:clamp(1.45rem,2.5vw,2.15rem); letter-spacing:-.04em; }
    .lede { max-width:53ch; margin:22px 0 0; color:var(--muted); font-size:clamp(1.03rem,2vw,1.22rem); } .mark { width:min(100%,360px); justify-self:center; }
    .tabs { display:flex; gap:0; margin-top:34px; overflow:auto; border-bottom:1px solid var(--line); } .tab { appearance:none; border:0; border-bottom:3px solid transparent; padding:15px 18px; background:transparent; color:var(--muted); font:inherit; font-weight:750; white-space:nowrap; cursor:pointer; } .tab[aria-selected="true"] { color:var(--ink); border-color:var(--blue); }
    .chapter { display:none; padding:clamp(24px,4vw,52px) 0 0; } .chapter.active { display:block; } .chapter > p { max-width:68ch; color:var(--muted); }
    .grid { display:grid; grid-template-columns:repeat(3,minmax(0,1fr)); gap:16px; margin-top:26px; } .card { min-height:180px; padding:24px; background:var(--paper); border:1px solid var(--line); border-radius:18px; } .card h3 { margin:0 0 8px; font-size:1.08rem; } .card p { margin:0; color:var(--muted); }
    .map { display:grid; grid-template-columns:1fr auto 1fr auto 1fr; gap:12px; align-items:center; margin-top:30px; padding:26px; background:var(--paper); border:1px solid var(--line); border-radius:20px; } .domain { min-height:138px; padding:20px; border:2px solid var(--ink); border-radius:16px; background:var(--cream); } .domain:nth-of-type(3) { background:var(--blue-soft); } .domain:last-child { background:#e3f1eb; } .domain strong { display:block; font-size:1.08rem; } .domain span { display:block; margin-top:8px; color:#405270; font-size:.91rem; } .arrow { color:var(--blue); font-size:1.9rem; font-weight:700; }
    .path { display:grid; grid-template-columns:repeat(4,1fr); gap:12px; margin-top:26px; } .step { position:relative; padding:22px 18px 18px; border-top:3px solid var(--blue); background:var(--paper); } .step small { display:block; margin-bottom:8px; color:var(--blue); font-weight:800; letter-spacing:.08em; text-transform:uppercase; } .step strong { font-size:1.05rem; }
    .proof { margin-top:26px; display:flex; flex-wrap:wrap; gap:14px; align-items:center; padding:20px 22px; background:#ecf7f1; border:1px solid #b9e2cf; border-radius:16px; } .proof strong { color:#17694f; } .proof code { color:#17694f; font-family:ui-monospace, SFMono-Regular, Menlo, monospace; } .status { color:#4c607d; }
    .replace { margin-top:26px; padding:26px; border-left:4px solid var(--blue); background:var(--paper); } .replace code { display:inline-block; padding:2px 6px; background:var(--soft); color:var(--ink); font-family:ui-monospace, SFMono-Regular, Menlo, monospace; }
    @media (max-width:760px) { .hero { grid-template-columns:1fr; } .mark { max-width:230px; } .grid,.path { grid-template-columns:1fr; } .map { grid-template-columns:1fr; } .arrow { transform:rotate(90deg); text-align:center; } }
  </style>
</head>
<body>
  <main class="shell">
    <section class="hero">
      <div><p class="eyebrow">Eco starter composition</p><h1>Build systems that can grow without losing their shape.</h1><p class="lede">Eco turns Domain-Driven Design into a practical workspace: clear domains, explicit runtime contracts, and one composition layer that makes them feel like one application.</p></div>
      <img class="mark" src="/images/ecology-mark.webp" alt="Three connected domains forming an Ecology system" />
    </section>
    <nav class="tabs" aria-label="Eco guide chapters">
      <button class="tab" type="button" role="tab" aria-selected="true" aria-controls="why" id="why-tab">Why Eco</button>
      <button class="tab" type="button" role="tab" aria-selected="false" aria-controls="ddd" id="ddd-tab">DDD map</button>
      <button class="tab" type="button" role="tab" aria-selected="false" aria-controls="path" id="path-tab">Build path</button>
      <button class="tab" type="button" role="tab" aria-selected="false" aria-controls="next" id="next-tab">Your placeholder</button>
    </nav>
    <section class="chapter active" role="tabpanel" id="why" aria-labelledby="why-tab"><p class="eyebrow">Chapter 01</p><h2>Eco keeps the seams visible.</h2><p>A scalable product is not one enormous codebase. It is a set of bounded contexts that can change at their own pace, composed deliberately at the edge. Eco keeps the operational details—repositories, runtimes, CTs, deployment, and service exposure—in that same deliberate shape.</p><div class="grid"><article class="card"><h3>Domains stay focused</h3><p>Each repository owns one meaningful capability and its data contract.</p></article><article class="card"><h3>Composition stays human</h3><p>The project composition owns the experience that connects those capabilities.</p></article><article class="card"><h3>Operations stay explicit</h3><p>The estate manifest describes where services run and what they need.</p></article></div></section>
    <section class="chapter" role="tabpanel" id="ddd" aria-labelledby="ddd-tab"><p class="eyebrow">Chapter 02</p><h2>DDD is a conversation about boundaries.</h2><p>Domains communicate through contracts. They do not reach into one another's implementation or database. The composition layer speaks to those contracts and turns them into a coherent customer journey.</p><div class="map"><div class="domain"><strong>Identity</strong><span>Who is this person? What may they do?</span></div><div class="arrow" aria-hidden="true">→</div><div class="domain"><strong>Inventory</strong><span>What is owned, valued, and available?</span></div><div class="arrow" aria-hidden="true">→</div><div class="domain"><strong>Marketplace</strong><span>What can be discovered or traded?</span></div></div></section>
    <section class="chapter" role="tabpanel" id="path" aria-labelledby="path-tab"><p class="eyebrow">Chapter 03</p><h2>Eco gives the first vertical slice a home.</h2><p>Start with a visible page and a small API. Then replace the placeholder with real domains as your product becomes clearer.</p><div class="path"><div class="step"><small>01</small><strong>Describe the estate</strong></div><div class="step"><small>02</small><strong>Compose services</strong></div><div class="step"><small>03</small><strong>Run with Eco</strong></div><div class="step"><small>04</small><strong>Grow bounded contexts</strong></div></div><div class="proof"><strong>Runtime proof</strong><span id="backend-status" class="status">Contacting the Rust backend…</span></div></section>
    <section class="chapter" role="tabpanel" id="next" aria-labelledby="next-tab"><p class="eyebrow">Chapter 04</p><h2>This starter is meant to be replaced.</h2><p><code>${projectName}_composition/frontend</code> and, if selected, <code>${projectName}_composition/backend</code> are runnable placeholders. Delete them when you are ready to create the actual project with the vibecoding model you choose.</p><div class="replace"><strong>Keep the composition contract.</strong><br />Your future frontend remains the public entry point; your future backend remains a separately-owned API. Eco will continue to provision and run what the manifest declares.</div></section>
  </main>
  <script src="/runtime-config.js"></script>
  <script>
    const tabs = [...document.querySelectorAll('.tab')];
    const chapters = [...document.querySelectorAll('.chapter')];
    tabs.forEach((tab) => tab.addEventListener('click', () => { tabs.forEach((item) => item.setAttribute('aria-selected', String(item === tab))); chapters.forEach((chapter) => chapter.classList.toggle('active', chapter.id === tab.getAttribute('aria-controls'))); }));
    fetch(window.__ECO_BACKEND_URL__ + '/helloworld').then((response) => response.ok ? response.text() : Promise.reject()).then((message) => { document.querySelector('#backend-status').textContent = message; }).catch(() => { document.querySelector('#backend-status').textContent = 'The backend will appear here after the optional Rust service starts.'; });
  </script>
</body>
</html>
`;
}

function buildStarterBackendCargoToml(projectName) {
  return `[package]\nname = "${projectName}-backend"\nversion = "0.1.0"\nedition = "2021"\n\n[dependencies]\naxum = "0.7"\ntokio = { version = "1", features = ["macros", "rt-multi-thread", "net"] }\ntower-http = { version = "0.5", features = ["cors"] }\n`;
}

function buildStarterBackendMain() {
  return `use axum::{routing::get, Router};
use tokio::net::TcpListener;
use tower_http::cors::CorsLayer;

async fn hello_world() -> &'static str {
    "Ecology works! This is coming from Rust Backend!"
}

#[tokio::main]
async fn main() {
    let port = std::env::var("SERVER_PORT")
        .expect("SERVER_PORT is required; run eco configure so Eco can assign this service port");
    let app = Router::new()
        .route("/helloworld", get(hello_world))
        .route("/api/helloworld", get(hello_world))
        .layer(CorsLayer::permissive());
    let listener = TcpListener::bind(format!("0.0.0.0:{port}"))
        .await
        .expect("Eco starter backend could not bind its port");
    println!("Eco starter Rust backend listening on http://0.0.0.0:{port}");
    axum::serve(listener, app).await.expect("Eco starter backend stopped unexpectedly");
}
`;
}

export async function createCompositionScaffold(compositionRepoPath, projectName, compositionServices) {
  await mkdir(compositionRepoPath, { recursive: true });
  await writeFile(path.join(compositionRepoPath, "README.md"), buildCompositionReadme(projectName, compositionServices), "utf8");
  await writeFile(path.join(compositionRepoPath, ".gitignore"), buildGitignoreContent(), "utf8");

  for (const service of compositionServices) {
    const serviceDir = path.join(compositionRepoPath, service.path);
    await mkdir(serviceDir, { recursive: true });
    await writeFile(path.join(serviceDir, ".gitignore"), buildServiceGitignoreContent(service.language), "utf8");
    const envExample = buildServiceEnvExampleContent(service);
    if (envExample) {
      await writeFile(path.join(serviceDir, ".env.example"), envExample, "utf8");
    }
    if (service.name === "frontend") {
      await mkdir(path.join(serviceDir, "images"), { recursive: true });
      await Promise.all([
        writeFile(path.join(serviceDir, "package.json"), buildStarterFrontendPackageJson(projectName), "utf8"),
        writeFile(path.join(serviceDir, "index.js"), buildStarterFrontendServer(), "utf8"),
        writeFile(path.join(serviceDir, "index.html"), buildStarterFrontendHtml(projectName), "utf8"),
        copyFile(ECOLOGY_MARK_PATH, path.join(serviceDir, "images", "ecology-mark.webp"))
      ]);
    }
    if (service.name === "backend") {
      await mkdir(path.join(serviceDir, "src"), { recursive: true });
      await Promise.all([
        writeFile(path.join(serviceDir, "Cargo.toml"), buildStarterBackendCargoToml(projectName), "utf8"),
        writeFile(path.join(serviceDir, "src", "main.rs"), buildStarterBackendMain(), "utf8")
      ]);
    }
  }
}

function buildClonePlan(targetRoot, selectedRepos, repoCatalog, branchOverrides = {}) {
  const byName = new Map(repoCatalog.map((repo) => [repo.name, repo]));
  return selectedRepos.map((repoName) => {
    const repo = byName.get(repoName);
    if (!repo) {
      throw new Error(`Unknown repo in catalog: ${repoName}`);
    }
    return {
      name: repo.name,
      git: repo.git,
      branch: branchOverrides[repoName] || repo.branch,
      targetPath: path.join(targetRoot, repo.name)
    };
  });
}

async function cloneSelectedRepos(clonePlan) {
  for (const item of clonePlan) {
    if (await pathExists(path.join(item.targetPath, ".git"))) {
      continue;
    }
    if (await pathExists(item.targetPath)) {
      throw new Error(`Refusing to clone into existing non-git path: ${item.targetPath}`);
    }
    await runCommand("git", ["clone", "--branch", item.branch, item.git, item.targetPath]);
  }
}

function mergeEnvValues(content, values) {
  const lines = content ? content.split(/\r?\n/) : [];
  for (const [key, value] of Object.entries(values)) {
    const rendered = `${key}=${value ?? ""}`;
    const index = lines.findIndex((line) => line.startsWith(`${key}=`));
    if (index === -1) lines.push(rendered);
    else lines[index] = rendered;
  }
  return `${lines.filter((line, index) => line || index < lines.length - 1).join("\n").replace(/\n*$/, "")}\n`;
}

// Apply the just-entered secret only to the local runtime file. This keeps a
// fresh local estate usable immediately without ever committing a Brevo key to
// the shared auth domain or the bootstrap repository.
async function writeLocalAuthEmailEnv(targetRoot, authEmailVerification) {
  if (!authEmailVerification) return;
  const envFile = path.join(targetRoot, "auth", "backend", ".env");
  const envExample = `${envFile}.example`;
  let content = "";
  try {
    content = await readFile(envFile, "utf8");
  } catch {
    try { content = await readFile(envExample, "utf8"); }
    catch { content = ""; }
  }

  const values = {
    EMAIL_VERIFICATION_REQUIRED: authEmailVerification.required ? "true" : "false"
  };
  if (authEmailVerification.required) {
    Object.assign(values, {
      EMAIL_VERIFICATION_TTL_HOURS: String(authEmailVerification.ttlHours),
      MAIL_FROM_EMAIL: authEmailVerification.mailFromEmail,
      MAIL_FROM_NAME: authEmailVerification.mailFromName,
      BREVO_API_KEY: authEmailVerification.brevoApiKey
    });
  }
  await writeFile(envFile, mergeEnvValues(content, values), "utf8");
}



async function assertScaffoldTargetsAvailable({ primaryRepoPath, compositionRepoPath, nonInteractive = false }) {
  const existingPaths = [];
  if (await pathExists(primaryRepoPath)) existingPaths.push(primaryRepoPath);
  if (await pathExists(compositionRepoPath)) existingPaths.push(compositionRepoPath);
  if (existingPaths.length === 0) {
    return;
  }

  output.write(`\nDirectory already exists:\n${existingPaths.map((targetPath) => `  - ${targetPath}`).join("\n")}\n`);
  if (nonInteractive) {
    output.write("Removing existing scaffold directories (--yes).\n");
    await Promise.all(existingPaths.map((targetPath) => rm(targetPath, { recursive: true, force: true })));
    return;
  }
  const overwrite = await confirmWithSingleKey("Remove the existing scaffold directories and recreate from scratch?");
  if (!overwrite) {
    throw new Error("Cancelled.");
  }

  await Promise.all(existingPaths.map((targetPath) => rm(targetPath, { recursive: true, force: true })));
}

// Returns null (feature not opted into), or the dedicated MinIO CT reference
// and region. `eco up` owns provisioning and private endpoint resolution.
async function promptStorageDetails(nonInteractive = false, noStorage = false) {
  if (nonInteractive) {
    output.write("\nObject storage (non-interactive)\n");
    if (noStorage) {
      output.write("  Not configured.\n\n");
      return null;
    }
    output.write("  MinIO: ct=storage, region=us-east-1\n\n");
    return { ct: "storage", region: "us-east-1" };
  }

  output.write("\nObject storage (MinIO / S3-compatible)\n");
  output.write("  Eco uses managed S3-compatible MinIO. Development provisions it\n");
  output.write("  locally; production provisions it in one dedicated MinIO CT and\n");
  output.write("  keeps application traffic on Proxmox's private bridge.\n\n");

  const useMinio = await confirmWithSingleKey("  Configure object storage for this estate?", { defaultYes: true });
  if (!useMinio) {
    return null;
  }

  const prompt = createPrompt();
  try {
    const ct = (await prompt.ask("  storage.minio.ct (MinIO CT hostname or VMID) [storage]: ")).trim() || "storage";
    const region = (await prompt.ask("  storage.minio.region [us-east-1]: ")).trim() || "us-east-1";
    return { ct, region };
  } finally {
    prompt.close();
  }
}

async function promptEcomposeDetails(projectName, frontendService, nonInteractive = false, flags = {}) {
  if (nonInteractive) {
    output.write("\n");
    output.write("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    output.write("  Estate configuration (non-interactive)\n");
    output.write("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    const ctId = flags.ctId || 101;
    const defaultHostname = `${projectName}.jogjaitcamp.com`;
    const hostname = flags.hostname || defaultHostname;
    const cloudflareAccount = flags.cloudflareAccount || "jogjaitcamp";
    const deployEnabled = !flags.noDeploy;
    const deployBranch = "main";
    const stagingCt = flags.stagingCt || 1000;
    const stagingEnabled = !flags.noStaging;
    const storage = await promptStorageDetails(true, flags.noStorage);

    output.write(`  ct.id: ${ctId}\n`);
    output.write(`  expose.hostname: ${hostname}\n`);
    output.write(`  expose.cloudflare_account: ${cloudflareAccount}\n`);
    output.write(`  expose.service: ${frontendService}\n`);
    output.write(`  github deploy: ${deployEnabled ? "enabled" : "disabled"}\n`);
    output.write(`  staging: ${stagingEnabled ? `ct ${stagingCt}` : "disabled"}\n`);
    output.write(`  storage: ${storage ? `minio CT (${storage.ct})` : "not configured"}\n\n`);

    return { ctId, hostname, cloudflareAccount, exposeService: frontendService, deployEnabled, deployBranch, storage, stagingCt, stagingEnabled };
  }

  const prompt = createPrompt();
  try {
    output.write("\n");
    output.write("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    output.write("  Estate configuration\n");
    output.write("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    output.write("\nCT ID\n");
    output.write("  Each Proxmox container needs a unique numeric ID across the whole node.\n");

    output.write("  Choose an unused CT ID for this estate. Eco does not query Proxmox here.\n");
    const ctIdRaw = (await prompt.ask("  ct.id [101]: ")).trim();
    const ctId = ctIdRaw ? parseInt(ctIdRaw, 10) : 101;

    if (!ctId || ctId <= 0) {
      throw new Error("CT ID must be a positive integer.");
    }

    output.write("\nPublic hostname\n");
    output.write("  The domain name users will visit to reach this estate.\n");
    output.write("  Must be a real DNS name that resolves to your Proxmox proxy CT.\n");
    output.write("  eco will provision a Cloudflare tunnel to this hostname via `eco expose`.\n");
    const defaultHostname = `${projectName}.jogjaitcamp.com`;
    output.write(`  Example: ${defaultHostname}\n`);
    const hostname = (await prompt.ask(`  expose.hostname [${defaultHostname}]: `)).trim() || defaultHostname;
    if (!hostname) {
      throw new Error("Hostname is required.");
    }

    output.write("\nCloudflare account\n");
    output.write("  A short account label selects the Cloudflare credentials on the Proxmox host.\n");
    output.write("  Example: jogjaitcamp uses CF_API_TOKEN_JOGJAITCAMP, CF_ACCOUNT_ID_JOGJAITCAMP,\n");
    output.write("  and CF_ZONE_ID_JOGJAITCAMP. This label is safe to commit; credentials are not.\n");
    const cloudflareAccount = (await prompt.ask("  expose.cloudflare_account [jogjaitcamp]: ")).trim() || "jogjaitcamp";

    output.write("\nPrimary frontend service\n");
    output.write("  The service that receives public HTTP traffic at the estate gateway root (/).\n");
    output.write("  All other services remain internal.\n");

    const exposeService = frontendService;
    output.write(`  Required composition service: ${exposeService}\n`);

    prompt.close();

    output.write("\nGitHub webhook auto-deploy\n");
    output.write("  When enabled, eco registers a webhook on each composed repo.\n");
    output.write("  A push to the deploy branch triggers a pull + PM2 reload automatically.\n");
    output.write("  eco up sets this up — no manual GitHub configuration needed.\n");
    const deployEnabled = await confirmWithSingleKey("  Enable GitHub deploy?", { defaultYes: true });

    let deployBranch = "main";
    if (deployEnabled) {
      const branchPrompt = createPrompt();
      output.write("\nDeploy branch\n");
      output.write("  Pushes to this branch will trigger a redeploy.\n");
      const branchInput = (await branchPrompt.ask("  branch [main]: ")).trim();
      branchPrompt.close();
      if (branchInput) {
        deployBranch = branchInput;
      }
    }

    const storage = await promptStorageDetails();

    output.write("\nStaging deployment\n");
    output.write("  A second footprint of this estate (staging.<hostname>) is deployed\n");
    output.write("  to a separate CT and accepts pushes to any branch except the prod\n");
    output.write("  deploy branch, so feature branches land on staging before main.\n");
    const stagingEnabled = await confirmWithSingleKey("  Enable staging?", { defaultYes: true });
    let stagingCt = 1000;
    if (stagingEnabled) {
      const stagingPrompt = createPrompt();
      output.write("\nStaging CT ID\n");
      output.write("  Must be a different container from the prod ct.id.\n");
      const stagingCtRaw = (await stagingPrompt.ask("  staging.ct [1000]: ")).trim();
      stagingPrompt.close();
      if (stagingCtRaw) {
        stagingCt = parseInt(stagingCtRaw, 10);
        if (!stagingCt || stagingCt <= 0) {
          throw new Error("Staging CT ID must be a positive integer.");
        }
      }
    }

    return { ctId, hostname, cloudflareAccount, exposeService, deployEnabled, deployBranch, storage, stagingCt, stagingEnabled };
  } catch (err) {
    prompt.close();
    throw err;
  }
}

async function confirmPlan({ projectName, targetRoot, currentDirMode, selectedRepos, clonePlan, primaryRepoPath, compositionRepoPath, compositionServices, githubRepositories, details, authEmailVerification, devFlags, nonInteractive = false, superadminSetup = false }) {
  output.write("\nProject scaffold plan:\n");
  output.write(`  project:          ${projectName}\n`);
  output.write(`  estate root:      ${targetRoot}${currentDirMode ? " (current directory)" : ""}\n`);
  output.write(`  bootstrap repo:   ${primaryRepoPath}\n`);
  output.write(`  composition repo: ${compositionRepoPath}\n`);
  output.write(`  composition:      ${compositionServices.map((service) => service.name).join(", ")}\n`);
  output.write(`  ct.id:            ${details.ctId}\n`);
  output.write(`  hostname:         ${details.hostname}\n`);
  output.write(`  cloudflare:       ${details.cloudflareAccount}\n`);
  output.write(`  expose.service:   ${details.exposeService}\n`);
  output.write(`  github deploy:    ${details.deployEnabled ? `enabled (branch: ${details.deployBranch})` : "disabled"}\n`);
  output.write(`  staging:          ${details.stagingEnabled ? `ct ${details.stagingCt}` : "disabled"}\n`);
  output.write(`  object storage:   ${details.storage ? `minio CT (${details.storage.ct})` : "not configured"}\n`);
  if (superadminSetup) {
    output.write(`  superadmin setup: enabled\n`);
  }
  if (authEmailVerification) {
    output.write(`  auth email:       ${authEmailVerification.required ? `verification required (${authEmailVerification.ttlHours}h)` : "verification disabled"}\n`);
  }
  output.write(`  selected repos:   ${selectedRepos.join(", ")}\n`);
  if (selectedRepos.length > 0) {
    const placements = selectedRepos
      .map((repoName) => `${repoName}${devFlags[repoName] === "disabled" ? " (prod-only)" : ""}`)
      .join(", ");
    output.write(`  environments:     ${placements}\n`);
  }
  output.write("  clone plan:\n");
  for (const item of clonePlan) {
    const branchNote = item.branch === "main" ? "" : ` [branch: ${item.branch}]`;
    output.write(`    - ${item.name}: ${item.git} -> ${item.targetPath}${branchNote}\n`);
  }
  const existingRepositories = githubRepositories.filter((item) => item.exists);
  if (existingRepositories.length > 0) {
    output.write("\nWARNING: Existing GitHub repository content will be removed:\n");
    for (const item of existingRepositories) {
      output.write(`  - ${item.repository.html_url}\n`);
    }
    output.write("  Eco will clone each repository, delete its working-tree content, then commit and push this new scaffold.\n");
  }
  if (nonInteractive) {
    output.write("\nProceeding automatically (--yes).\n");
    return true;
  }
  if (existingRepositories.length > 0) {
    return confirmWithSingleKey("Replace the existing GitHub repository content?", { defaultYes: true });
  }
  return confirmWithSingleKey("Create this project scaffold?", { defaultYes: true });
}

export async function runStartProject(args) {
  const flags = parseFlags(args);
  const nonInteractive = flags.yes;
  const workspaceRoot = await findWorkspaceRoot(process.cwd());
  const { projectName, targetRoot, currentDirMode } = await promptProjectName(flags.remaining, workspaceRoot);
  if (!process.env.ECO_GITHUB_API_KEY) {
    throw new Error("ECO_GITHUB_API_KEY is required to create and push the bootstrap and composition repositories.");
  }

  const selectedCompositionServices = await promptCompositionServices(projectName, nonInteractive);
  output.write("\nComposition starter\n  frontend: Node.js\n  backend: Rust (when selected)\n\n");
  const backendDatabases = await promptBackendDatabases(selectedCompositionServices, nonInteractive);
  const compositionServices = buildCompositionServices(selectedCompositionServices, backendDatabases);
  const repoCatalog = await readRepoCatalog();
  const selectedRepos = await runRepoChecklist(repoCatalog, projectName, nonInteractive, flags.repos);
  const selectedReposWithDeps = computeDependencyClosure(selectedRepos, repoCatalog);
  const authEmailVerification = await promptAuthEmailVerification(selectedReposWithDeps, projectName, nonInteractive, flags.noEmailVerification);
  const superadminSetup = await promptSuperadminSetup(nonInteractive);
  const { branchOverrides, devFlags } = await promptRepoPlacements(selectedReposWithDeps, repoCatalog, nonInteractive, flags.branchOverrides);

  const clonePlan = buildClonePlan(targetRoot, selectedReposWithDeps, repoCatalog, branchOverrides);
  const serviceTemplates = await discoverServiceTemplates(workspaceRoot);
  const primaryRepoPath = path.join(targetRoot, `${projectName}_bootstrap`);
  const compositionRepoPath = path.join(targetRoot, `${projectName}_composition`);
  const githubRepositories = await inspectGithubRepositories([
    `${projectName}_bootstrap`,
    `${projectName}_composition`
  ]);

  const details = await promptEcomposeDetails(projectName, "frontend", nonInteractive, flags);

  const confirmed = await confirmPlan({
    projectName,
    targetRoot,
    currentDirMode,
    selectedRepos: selectedReposWithDeps,
    clonePlan,
    primaryRepoPath,
    compositionRepoPath,
    compositionServices,
    githubRepositories,
    details,
    authEmailVerification,
    devFlags,
    nonInteractive,
    superadminSetup
  });

  if (!confirmed) {
    throw new Error("Cancelled.");
  }

  await assertScaffoldTargetsAvailable({ primaryRepoPath, compositionRepoPath, nonInteractive });

  if (!currentDirMode) {
    await mkdir(targetRoot, { recursive: true });
  }

  const bootstrapRepositoryPlan = githubRepositories.find((item) => item.name === `${projectName}_bootstrap`);
  const compositionRepositoryPlan = githubRepositories.find((item) => item.name === `${projectName}_composition`);

  if (bootstrapRepositoryPlan.exists) {
    await cloneAndClearRepository(primaryRepoPath, bootstrapRepositoryPlan.repository);
  } else {
    await mkdir(primaryRepoPath, { recursive: true });
  }

  const ecomposePath = path.join(primaryRepoPath, "ecompose.yml");
  const claudePath = path.join(primaryRepoPath, "README.md");
  const gitignorePath = path.join(primaryRepoPath, ".gitignore");

  const createdClaude = !(await pathExists(claudePath));
  if (createdClaude) {
    await writeFile(claudePath, buildClaudeContent(projectName), "utf8");
  }

  const createdGitignore = !(await pathExists(gitignorePath));
  if (createdGitignore) {
    await writeFile(gitignorePath, buildGitignoreContent(), "utf8");
  }

  await writeFile(
    ecomposePath,
    buildEcomposeContent({ projectName, selectedRepos, serviceTemplates, compositionServices, details, authEmailVerification, branchOverrides, devFlags, compositionGit: compositionGitUrl(compositionRepositoryPlan), superadminSetup }),
    "utf8"
  );

  // The bootstrap is intentionally published before any domain clone: it is
  // the estate's durable source of truth, even if a later clone fails.
  const bootstrapRepository = await initialiseAndPushRepository(
    primaryRepoPath,
    `${projectName}_bootstrap`,
    `init: ${projectName} estate manifest`,
    bootstrapRepositoryPlan.repository
  );

  if (compositionRepositoryPlan.exists) {
    await cloneAndClearRepository(compositionRepoPath, compositionRepositoryPlan.repository);
  }
  await createCompositionScaffold(compositionRepoPath, projectName, compositionServices);
  const compositionRepository = await initialiseAndPushRepository(
    compositionRepoPath,
    `${projectName}_composition`,
    `init: ${projectName} composition`,
    compositionRepositoryPlan.repository
  );

  await cloneSelectedRepos(clonePlan);
  await writeLocalAuthEmailEnv(targetRoot, authEmailVerification);

  const bootstrapDirName = `${projectName}_bootstrap`;

  const color = output.isTTY && !process.env.NO_COLOR;
  const bold  = (s) => color ? `\x1b[1m${s}\x1b[0m` : s;
  const dim   = (s) => color ? `\x1b[2m${s}\x1b[0m` : s;
  const cmd   = (s) => color ? `\x1b[1m\x1b[36m${s}\x1b[0m` : s;
  const sep   = () => color ? `\x1b[2m${"─".repeat(56)}\x1b[0m` : "─".repeat(56);

  output.write(`\n${bold("Project scaffold created")} in ${targetRoot}\n`);
  output.write(`${dim("-")} ${ecomposePath}\n`);
  if (createdClaude)    output.write(`${dim("-")} ${claudePath}\n`);
  if (createdGitignore) output.write(`${dim("-")} ${gitignorePath}\n`);
  output.write(`${dim("-")} ${compositionRepoPath}\n`);
  output.write(`${dim("-")} GitHub: ${bootstrapRepository.html_url}\n`);
  output.write(`${dim("-")} GitHub: ${compositionRepository.html_url}\n`);

  output.write(`
${sep()}
  ${bold("Next steps")}
${sep()}

  ${bold("0. Start local dev environment")}
     From the estate root:

       ${cmd(`cd ${targetRoot}`)}
       ${cmd("eco up")}

     ${dim("Services start and PM2 logs follow automatically.")}
     ${dim("Ctrl+C stops log tailing — services keep running.")}

  ${bold("1. Repositories created and pushed")}
     ${dim("The bootstrap and composition repositories are private GitHub repos.")}
     ${dim("The composition includes frontend first, plus the optional backend you selected.")}

  ${bold("2. Deploy on Proxmox")}
     ${dim("SSH into your Proxmox host and run:")}

       ${cmd(`ssh root@${process.env.PROXMOX_HOST || "<your-proxmox-host>"}`)}
       ${cmd(`git clone ${bootstrapRepository.ssh_url || bootstrapRepository.clone_url} /root/${bootstrapDirName}`)}
       ${cmd(`cd /root/${bootstrapDirName}`)}
       ${cmd("eco up")}

     ${dim(`eco up will create CT ${details.ctId}, clone domain repos, install`)}
     ${dim("runtimes, wire .env files, start PM2 services,")}${details.deployEnabled ? `
     ${dim(`register GitHub webhooks (branch: ${details.deployBranch}),`)}` : ""}
     ${dim(`and expose the estate at https://${details.hostname}`)}

  ${bold("3. Re-deploy after changes")}

       ${cmd(`cd /root/${bootstrapDirName} && git pull && eco up`)}
${details.deployEnabled ? `
     ${dim(`Or push to ${details.deployBranch} — webhook triggers redeploy automatically.`)}` : ""}
${sep()}

  ${bold(`Start working on your ${projectName} project:`)}

       ${cmd(`cd ${primaryRepoPath}`)}
       ${cmd("eco up")}

  ${bold("Happy Vibe Coding!")}

`);
}
