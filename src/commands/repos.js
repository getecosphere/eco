import { readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { execFile } from "node:child_process";
import { createInterface } from "node:readline/promises";
import { stdin as input, stdout as output } from "node:process";
import { promisify } from "node:util";

import { findWorkspaceRoot } from "../lib/workspace.js";

const execFileAsync = promisify(execFile);

function buildDependencyIndex(subsystems) {
  const byName = new Map(subsystems.map((repo) => [repo.name, repo]));
  return { byName };
}

function renderTree(name, byName, prefix = "", isLast = true, trail = new Set(), isRoot = false) {
  const repo = byName.get(name) ?? { name, description: "Referenced dependency not found in repos.json" };
  const connector = isRoot ? "" : (isLast ? "└── " : "├── ");
  const lines = [`${prefix}${connector}${repo.name}`];
  const nextPrefix = isRoot ? "" : `${prefix}${isLast ? "    " : "│   "}`;
  const dependencies = Array.isArray(repo.requires)
    ? [...repo.requires].sort((left, right) => left.localeCompare(right))
    : [];

  if (trail.has(name)) {
    lines.push(`${nextPrefix}└── [cycle detected]`);
    return lines;
  }

  const nextTrail = new Set(trail);
  nextTrail.add(name);

  dependencies.forEach((dependencyName, index) => {
    const dependencyIsLast = index === dependencies.length - 1;
    lines.push(...renderTree(dependencyName, byName, nextPrefix, dependencyIsLast, nextTrail, false));
  });

  return lines;
}

function renderDependencyBlock(repo, byName) {
  const dependencies = Array.isArray(repo.requires)
    ? [...repo.requires].sort((left, right) => left.localeCompare(right))
    : [];

  if (dependencies.length === 0) {
    return "";
  }

  const lines = [];
  dependencies.forEach((dependencyName, index) => {
    const dependencyIsLast = index === dependencies.length - 1;
    lines.push(...renderTree(dependencyName, byName, "", dependencyIsLast, new Set(), false));
  });

  return `  dependency:\n${lines.map((line) => `    ${line}`).join("\n")}\n`;
}

async function loadReposConfig(workspaceRoot) {
  const reposPath = path.join(workspaceRoot, "eco", "repos.json");
  const content = await readFile(reposPath, "utf8");
  const config = JSON.parse(content);
  const subsystems = Array.isArray(config.subsystems) ? config.subsystems : [];
  return { reposPath, config, subsystems };
}

function renderRepoList(subsystems) {
  const { byName } = buildDependencyIndex(subsystems);

  for (const repo of subsystems) {
    const dependencies = Array.isArray(repo.requires) ? repo.requires : [];
    for (const dependency of dependencies) {
      if (!byName.has(dependency)) {
        byName.set(dependency, { name: dependency, description: "Referenced dependency not found in repos.json" });
      }
    }
  }

  let outputText = "";
  for (const repo of subsystems) {
    const description = repo.description ?? "No description";
    const branch = repo.branch ?? "default";
    const source = repo.git ?? "unknown";
    const dependencyBlock = renderDependencyBlock(repo, byName);

    outputText +=
      `\n${repo.name}\n` +
      `  description: ${description}\n` +
      `  repo: ${source}\n` +
      `  branch: ${branch}\n` +
      dependencyBlock;
  }

  return outputText;
}

async function runGit(args, cwd) {
  try {
    const { stdout } = await execFileAsync("git", args, { cwd });
    return stdout.trim();
  } catch (error) {
    throw new Error(`Failed to inspect git repo in ${cwd}: ${error instanceof Error ? error.message : String(error)}`);
  }
}

async function inspectCurrentRepo(cwd) {
  const repoRoot = await runGit(["rev-parse", "--show-toplevel"], cwd);
  const name = path.basename(repoRoot);
  const branch = await runGit(["branch", "--show-current"], repoRoot);
  const git = await runGit(["remote", "get-url", "origin"], repoRoot);
  return { repoRoot, name, branch, git };
}

// Same peer-URL naming convention configure.sh already relies on
// (resolve_peer_base_urls / resolve_frontend_peer_api_urls): a service that
// wants to call another domain declares it as `<PREFIX>_BASE_URL`,
// `<PREFIX>_API_URL`, or `NEXT_PUBLIC_<PREFIX>_API_URL` in its own env file,
// where `<PREFIX>` is the target repo's name upper-cased with `-` as `_`.
// These generic self-referential keys exist in nearly every service's env
// file regardless of what it actually depends on, so they're never treated
// as a dependency signal.
const DEPENDENCY_ENV_KEY_PATTERN = /^(?:NEXT_PUBLIC_)?([A-Z][A-Z0-9]*(?:_[A-Z0-9]+)*)_(?:BASE_URL|API_URL)$/;
const SELF_REFERENTIAL_ENV_KEYS = new Set([
  "API_BASE_URL",
  "AUTH_BASE_URL",
  "NEXT_PUBLIC_API_URL",
  "NEXT_PUBLIC_AUTH_URL",
  "NEXT_PUBLIC_APP_URL"
]);

async function readEnvKeys(repoRoot) {
  for (const filename of [".env.example", ".env"]) {
    let contents;
    try {
      contents = await readFile(path.join(repoRoot, filename), "utf8");
    } catch (error) {
      if (error.code === "ENOENT") continue;
      throw error;
    }

    return contents
      .split("\n")
      .map((line) => line.match(/^([A-Z][A-Z0-9_]*)=/))
      .filter((match) => match !== null)
      .map((match) => match[1]);
  }

  return [];
}

async function detectSiblingDependencies(existingSubsystems, detectedRepo) {
  const knownRepoNames = new Set(existingSubsystems.map((repo) => repo.name));
  const envKeys = await readEnvKeys(detectedRepo.repoRoot);

  const detected = new Set();
  for (const key of envKeys) {
    if (SELF_REFERENTIAL_ENV_KEYS.has(key)) continue;

    const match = key.match(DEPENDENCY_ENV_KEY_PATTERN);
    if (!match) continue;

    const candidateName = match[1].toLowerCase().replace(/_/g, "-");
    if (candidateName === detectedRepo.name) continue;
    if (knownRepoNames.has(candidateName)) {
      detected.add(candidateName);
    }
  }

  return [...detected].sort((left, right) => left.localeCompare(right));
}

async function promptForRepoEntry(existingSubsystems, detectedRepo, requires) {
  const rl = createInterface({ input, output });
  try {
    const knownNames = existingSubsystems.map((repo) => repo.name).join(", ") || "(none)";
    output.write(`Detected repo:\n`);
    output.write(`  path: ${detectedRepo.repoRoot}\n`);
    output.write(`  name: ${detectedRepo.name}\n`);
    output.write(`  repo: ${detectedRepo.git}\n`);
    output.write(`  branch: ${detectedRepo.branch}\n`);
    output.write(`Known repos: ${knownNames}\n`);

    const descriptionInput = await rl.question("Description: ");
    const description = descriptionInput.trim();
    if (!description) {
      throw new Error("Description is required.");
    }

    output.write(`Detected dependencies from .env: ${requires.length > 0 ? requires.join(", ") : "(none)"}\n`);

    const entry = {
      name: detectedRepo.name,
      description,
      git: detectedRepo.git,
      branch: detectedRepo.branch
    };

    if (requires.length > 0) {
      entry.requires = requires;
    }

    output.write(`\nProposed entry:\n`);
    output.write(`${JSON.stringify(entry, null, 2)}\n`);

    const confirmation = (await rl.question("Add this repo to eco/repos.json? [y/N]: ")).trim().toLowerCase();
    if (confirmation !== "y" && confirmation !== "yes") {
      throw new Error("Cancelled.");
    }

    return entry;
  } finally {
    rl.close();
  }
}

async function runReposAdd(workspaceRoot) {
  const { reposPath, config, subsystems } = await loadReposConfig(workspaceRoot);
  const detectedRepo = await inspectCurrentRepo(process.cwd());
  const requires = await detectSiblingDependencies(subsystems, detectedRepo);

  if (subsystems.some((repo) => repo.name === detectedRepo.name)) {
    throw new Error(`Repo "${detectedRepo.name}" already exists in ${reposPath}.`);
  }

  if (subsystems.some((repo) => repo.git === detectedRepo.git)) {
    throw new Error(`Repo "${detectedRepo.git}" already exists in ${reposPath}.`);
  }

  const entry = await promptForRepoEntry(subsystems, detectedRepo, requires);
  config.subsystems = [...subsystems, entry];
  await writeFile(reposPath, `${JSON.stringify(config, null, 2)}\n`, "utf8");
  process.stdout.write(`Added ${entry.name} to ${reposPath}\n`);
}

export async function runRepos(args) {
  const workspaceRoot = await findWorkspaceRoot(process.cwd());
  const [subcommand] = args;

  if (subcommand === "add") {
    await runReposAdd(workspaceRoot);
    return;
  }

  if (subcommand && subcommand !== "list") {
    throw new Error(`Unknown repos subcommand: ${subcommand}`);
  }

  const { reposPath, subsystems } = await loadReposConfig(workspaceRoot);

  process.stdout.write(`Known repos from ${reposPath}\n`);

  if (subsystems.length === 0) {
    process.stdout.write("(none)\n");
    return;
  }

  process.stdout.write(renderRepoList(subsystems));
}
