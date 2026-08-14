import { access, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import readline from "node:readline";
import { stdin as input, stdout as output } from "node:process";

import { buildRepoDependencyMaps, confirmWithSingleKey, runChecklist } from "../lib/checklist.js";
import { renderDomainEntry, resolveEcomposeFile } from "../lib/ecompose.js";
import { findRepoByName, readRepoCatalog } from "../lib/repos.js";
import { findWorkspaceRoot } from "../lib/workspace.js";
import { discoverServicesAt, renderServiceBlock } from "./adopt.js";

const execFileAsync = promisify(execFile);

async function pathExists(targetPath) {
  try {
    await access(targetPath);
    return true;
  } catch {
    return false;
  }
}

function splitLines(content) {
  return content.split(/\r?\n/);
}

// Same "does this line start a new top-level key" heuristic parseServices
// already relies on in lib/ecompose.js -- every real top-level key in these
// files is a bare `key:` block header (project: <name> is the one exception,
// but it only ever appears once, before any block this function is asked to
// bound, so it never gets encountered mid-block here).
function isTopLevelKeyLine(line) {
  return /^[^\s#].*:\s*$/.test(line);
}

// Locates a top-level `<key>:` block and returns [headerIndex, end), where
// `end` is the index of the next top-level key line (or EOF) -- i.e. the
// raw lexical extent of the block, still including any trailing blank/
// comment lines before that next key.
function findBlock(lines, key) {
  const headerIndex = lines.findIndex((line) => new RegExp(`^${key}:\\s*$`).test(line));
  if (headerIndex === -1) return null;

  let end = lines.length;
  for (let i = headerIndex + 1; i < lines.length; i++) {
    if (isTopLevelKeyLine(lines[i])) {
      end = i;
      break;
    }
  }
  return { headerIndex, end };
}

// A block's raw lexical end (see findBlock) can include trailing blank
// lines and, worse, a free-floating top-level comment that lexically sits
// between this block's last real entry and the next block's header (e.g.
// training_bootstrap's "lms/backend is retired..." comment between
// domains: and services:) -- naively splicing at `end` would insert new
// content after that unrelated comment instead of after the block's actual
// last item. Walk backward from `end` past blank/comment lines to find the
// real insertion point.
function insertionPointFor(lines, block) {
  for (let i = block.end - 1; i > block.headerIndex; i--) {
    const trimmed = lines[i].trim();
    if (trimmed === "" || trimmed.startsWith("#")) continue;
    return i + 1;
  }
  return block.headerIndex + 1;
}

function domainAlreadyDeclared(lines, repoName) {
  const block = findBlock(lines, "domains");
  if (!block) return false;
  return lines.slice(block.headerIndex + 1, block.end).some((line) => {
    const match = line.match(/^\s*-\s*([A-Za-z0-9_-]+)/);
    return match !== null && match[1] === repoName;
  });
}

// Appends a domain entry to an existing domains: block, or creates one
// (right before services:, or after shared_tools: if services: is also
// missing) if this manifest never had one -- e.g. a single-service estate
// like chronic that only ever needed services: until now.
export function insertDomain(content, repoName, options = {}) {
  const lines = splitLines(content);
  if (domainAlreadyDeclared(lines, repoName)) {
    return content;
  }

  const entryLine = renderDomainEntry(repoName, options);
  const domainsBlock = findBlock(lines, "domains");
  if (domainsBlock) {
    lines.splice(insertionPointFor(lines, domainsBlock), 0, entryLine);
    return lines.join("\n");
  }

  const servicesBlock = findBlock(lines, "services");
  const sharedToolsBlock = findBlock(lines, "shared_tools");
  let insertAt;
  if (servicesBlock) {
    insertAt = servicesBlock.headerIndex;
  } else if (sharedToolsBlock) {
    insertAt = insertionPointFor(lines, sharedToolsBlock);
  } else {
    insertAt = lines.length;
  }
  lines.splice(insertAt, 0, "domains:", entryLine, "");
  return lines.join("\n");
}

// After the branch (default main), each repo gets an environment placement:
// prod is mandatory (locked checkbox), dev is optional and defaults to
// checked. Unchecking dev records `dev: disabled` (prod-only on this
// estate); leaving it checked records `dev: optional`, so `eco up dev` can
// skip the domain gracefully when the machine can't run its runtimes.
async function promptDomainPlacements(repoNames, repoCatalog) {
  const byName = new Map(repoCatalog.map((repo) => [repo.name, repo]));
  const placements = {};
  output.write("\nDomain branches & environments\n");
  output.write("  Press Enter to accept each repo's default branch, or type a\n");
  output.write("  different branch for this estate to track. Then choose where the\n");
  output.write("  repo runs: prod is mandatory (locked); dev is optional, default checked.\n\n");
  for (const repoName of repoNames) {
    const repo = byName.get(repoName);
    const defaultBranch = repo?.branch || "main";
    const branch = await new Promise((resolve) => {
      const rl = readline.createInterface({ input, output });
      rl.question(`  ${repoName} branch [${defaultBranch}]: `, (answer) => {
        rl.close();
        if (typeof input.pause === "function") input.pause();
        resolve(answer.trim());
      });
    });
    const branchOverride = branch && branch !== defaultBranch ? branch : undefined;
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
    placements[repoName] = {
      branch: branchOverride,
      dev: selection.includes("dev") ? "optional" : "disabled"
    };
  }
  return placements;
}

function serviceAlreadyDeclared(lines, serviceName) {
  return lines.some((line) => new RegExp(`^  ${serviceName}:\\s*$`).test(line));
}

// Appends new service blocks to an existing services: block (or creates one
// at EOF if the manifest never had one). Returns only the services that
// weren't already declared, so the caller can report/no-op accordingly.
export function insertServices(content, services) {
  const lines = splitLines(content);
  const newServices = services.filter((service) => !serviceAlreadyDeclared(lines, service.name));
  if (newServices.length === 0) {
    return { content, added: [] };
  }

  const renderedLines = newServices.flatMap((service) => [...renderServiceBlock(service).split("\n"), ""]);

  const servicesBlock = findBlock(lines, "services");
  if (servicesBlock) {
    lines.splice(insertionPointFor(lines, servicesBlock), 0, ...renderedLines);
  } else {
    if (lines.length > 0 && lines[lines.length - 1].trim() !== "") {
      lines.push("");
    }
    lines.push("services:", ...renderedLines);
  }
  return { content: lines.join("\n"), added: newServices };
}

// Replaces only the service declarations belonging to one immediate domain
// directory. This is deliberately narrower than a generic manifest editor:
// it lets a domain change from a workspace root to backend/ (or add/remove
// sibling services) without an operator hand-editing ecompose.yml.
export function replaceDomainServices(content, domainName, services) {
  const lines = splitLines(content);
  const servicesBlock = findBlock(lines, "services");
  if (!servicesBlock) return insertServices(content, services).content;

  const retained = [];
  let index = servicesBlock.headerIndex + 1;
  while (index < servicesBlock.end) {
    const start = index;
    if (!/^  [A-Za-z0-9_-]+:\s*$/.test(lines[index])) {
      retained.push(lines[index]);
      index += 1;
      continue;
    }
    index += 1;
    while (index < servicesBlock.end && !/^  [A-Za-z0-9_-]+:\s*$/.test(lines[index])) index += 1;
    const block = lines.slice(start, index);
    const pathLine = block.find((line) => /^    path:\s+/.test(line));
    const servicePath = pathLine?.replace(/^    path:\s+/, "").trim();
    if (servicePath !== domainName && !servicePath?.startsWith(`${domainName}/`)) retained.push(...block);
  }

  const rendered = services.flatMap((service) => [...renderServiceBlock(service).split("\n"), ""]);
  lines.splice(servicesBlock.headerIndex + 1, servicesBlock.end - servicesBlock.headerIndex - 1, ...rendered, ...retained);
  return lines.join("\n");
}

export function insertAdditionalExposure(content, serviceName, hostname) {
  const lines = splitLines(content);
  const expose = findBlock(lines, "expose");
  if (!expose) throw new Error("No expose: block exists in this manifest.");
  const duplicate = lines.slice(expose.headerIndex + 1, expose.end).some((line) => line.trim() === `hostname: ${hostname}`);
  if (duplicate) return content;
  const additionalIndex = lines.findIndex((line, index) => index > expose.headerIndex && index < expose.end && /^  additional:\s*$/.test(line));
  const rendered = [`    - hostname: ${hostname}`, `      service: ${serviceName}`];
  if (additionalIndex !== -1) {
    lines.splice(insertionPointFor(lines, { headerIndex: additionalIndex, end: expose.end }), 0, ...rendered);
  } else {
    lines.splice(insertionPointFor(lines, expose), 0, "  additional:", ...rendered);
  }
  return lines.join("\n");
}

async function readGitRemote(dir) {
  try {
    const { stdout } = await execFileAsync("git", ["-C", dir, "remote", "get-url", "origin"]);
    return stdout.trim();
  } catch {
    return null;
  }
}

// A catalog repo may already be cloned elsewhere in the workspace -- most
// commonly as a workspace-root-level sibling (e.g. gameserver cloned
// directly under SuperApp/ before any estate composed it), not necessarily
// inside this specific estate's own root. Checked in this order and
// verified by git remote (not just name -- a same-named directory that
// isn't actually this repo must not be silently adopted) so a real existing
// clone is reused in place instead of prompting to clone a redundant copy.
async function findExistingClone(catalogRepo, estateRoot, workspaceRoot) {
  const candidates = [path.join(estateRoot, catalogRepo.name)];
  if (workspaceRoot !== estateRoot) {
    candidates.push(path.join(workspaceRoot, catalogRepo.name));
  }

  for (const candidate of candidates) {
    if (!(await pathExists(path.join(candidate, ".git")))) continue;
    const remote = await readGitRemote(candidate);
    if (remote === catalogRepo.git) {
      return candidate;
    }
  }
  return null;
}

// Resolves one positional argument to either a repos.json catalog repo
// (reusing an existing clone found anywhere in the workspace, or cloned
// fresh into the estate root only if truly not present anywhere) or an
// already-present immediate subdirectory of the estate root. Pure
// resolution, no prompts/side effects, so it can run for every target
// before any confirmation is shown -- whether there's one target
// (positional arg) or several (interactive checklist).
export async function resolveComposeTarget(target, estateRoot, workspaceRoot) {
  const catalogRepo = await findRepoByName(target);

  if (catalogRepo) {
    const existing = await findExistingClone(catalogRepo, estateRoot, workspaceRoot);
    const serviceDir = existing ?? path.join(estateRoot, catalogRepo.name);
    const needsClone = existing === null;
    if (needsClone && (await pathExists(serviceDir))) {
      throw new Error(`Refusing to clone into existing non-git path: ${serviceDir}`);
    }
    return { target, catalogRepo, serviceDir, servicesLabel: catalogRepo.name, needsClone, reusedExisting: existing !== null };
  }

  let candidate = path.resolve(process.cwd(), target);
  if (!(await pathExists(candidate))) {
    candidate = path.resolve(estateRoot, target);
  }
  if (!(await pathExists(candidate))) {
    throw new Error(
      `"${target}" isn't a known repo in eco/repos.json, and no directory was found at it either ` +
        `(checked relative to the current directory and to the estate root ${estateRoot}).`
    );
  }

  const relativeToEstate = path.relative(estateRoot, candidate);
  if (relativeToEstate.startsWith("..") || path.isAbsolute(relativeToEstate)) {
    throw new Error(`${candidate} is outside the estate root ${estateRoot} -- move/clone it under there first.`);
  }
  if (relativeToEstate.includes(path.sep)) {
    throw new Error(
      `${candidate} is nested more than one level under ${estateRoot} -- ` +
        "eco compose add only auto-detects immediate estate-root subdirectories; " +
        "declare a deeper path by hand in ecompose.yml's services: block instead."
    );
  }

  return {
    target,
    catalogRepo: null,
    serviceDir: candidate,
    servicesLabel: path.basename(candidate),
    needsClone: false,
    reusedExisting: false
  };
}

// No positional arg: offer the same arrow-key multi-select checklist
// startproject uses, scoped to eco/repos.json entries not already composed
// into this estate (selecting a repo also auto-selects/locks its
// repos.json `requires`, same dependency-closure behavior as startproject).
async function pickTargetsInteractively(manifestContent) {
  const catalog = await readRepoCatalog();
  const lines = splitLines(manifestContent);
  const items = catalog
    .filter((repo) => repo.name !== "eco" && !domainAlreadyDeclared(lines, repo.name))
    .map((repo) => ({ id: repo.name, label: `${repo.name} - ${repo.description ?? "No description"}` }));

  if (items.length === 0) {
    output.write("Every repo in eco/repos.json is already composed into this estate.\n");
    return [];
  }

  const { requiresByRepo, requiredByRepo } = buildRepoDependencyMaps(catalog);
  return runChecklist({
    items,
    title: "Select repos to compose into this estate",
    hint: "Controls: ↑/↓ move, x or space toggle, Enter confirm",
    requiresByRepo,
    requiredByRepo,
    minSelected: 1
  });
}

export async function runComposeAdd(args) {
  const yesFlag = args.includes("--yes") || args.includes("-y");
  const positionalArgs = args.filter((a) => !a.startsWith("-"));
  const [target] = positionalArgs;

  const manifestPath = await resolveEcomposeFile(".", process.cwd());
  if (!(await pathExists(manifestPath))) {
    throw new Error(`No ecompose.yml found at ${manifestPath} -- run "eco adopt" first to create one.`);
  }

  const manifestDir = path.dirname(manifestPath);
  const estateRoot = path.dirname(manifestDir);
  const workspaceRoot = await findWorkspaceRoot(estateRoot);
  const initialContent = await readFile(manifestPath, "utf8");

  const targets = target ? [target] : await pickTargetsInteractively(initialContent);
  if (targets.length === 0) {
    return;
  }

  const resolvedTargets = [];
  for (const oneTarget of targets) {
    resolvedTargets.push(await resolveComposeTarget(oneTarget, estateRoot, workspaceRoot));
  }

  const reused = resolvedTargets.filter((resolved) => resolved.reusedExisting);
  if (reused.length > 0) {
    output.write("Already cloned elsewhere in the workspace -- reusing in place, not cloning a duplicate:\n");
    for (const resolved of reused) {
      output.write(`  ${resolved.catalogRepo.name} -> ${resolved.serviceDir}\n`);
    }
    output.write("\n");
  }

  const toClone = resolvedTargets.filter((resolved) => resolved.needsClone);
  if (toClone.length > 0) {
    output.write("Will clone:\n");
    for (const resolved of toClone) {
      output.write(
        `  ${resolved.catalogRepo.name} (${resolved.catalogRepo.git}, branch ${resolved.catalogRepo.branch}) -> ${resolved.serviceDir}\n`
      );
    }
    const confirmClone = yesFlag ? true : await confirmWithSingleKey(`\nClone ${toClone.length} repo(s) into the estate root?`);
    if (!confirmClone) {
      throw new Error("Cancelled.");
    }
    for (const resolved of toClone) {
      await execFileAsync("git", ["clone", "--branch", resolved.catalogRepo.branch, resolved.catalogRepo.git, resolved.serviceDir]);
      output.write(`Cloned ${resolved.catalogRepo.name}\n`);
    }
    output.write("\n");
  }

  let content = initialContent;
  const domainsToAdd = [];
  const allAddedServices = [];

  for (const resolved of resolvedTargets) {
    const services = await discoverServicesAt(resolved.servicesLabel, resolved.serviceDir);

    // discoverServicesAt builds each service's path assuming serviceDir is
    // a direct child of estateRoot (path starting with resolved.servicesLabel).
    // A reused clone found elsewhere in the workspace (see findExistingClone)
    // breaks that assumption -- rewrite to the real relative path (e.g.
    // "../gameserver") so ecompose.yml points at where the repo actually is.
    const realRelPath = path.relative(estateRoot, resolved.serviceDir).split(path.sep).join("/");
    if (realRelPath !== resolved.servicesLabel) {
      for (const service of services) {
        service.path =
          service.path === resolved.servicesLabel
            ? realRelPath
            : `${realRelPath}${service.path.slice(resolved.servicesLabel.length)}`;
      }
    }

    if (services.length > 0) {
      output.write(`Detected services for ${resolved.target}:\n`);
      for (const service of services) {
        output.write(`  ${service.name} -- path: ${service.path}, runtimes: ${service.runtimes.join(", ") || "(none)"}\n`);
      }
    } else {
      output.write(`No services detected for ${resolved.target} (looked for pom.xml/Cargo.toml/package.json).\n`);
    }

    if (resolved.catalogRepo && !domainAlreadyDeclared(splitLines(content), resolved.catalogRepo.name)) {
      domainsToAdd.push(resolved.catalogRepo.name);
    }

    const { content: withServices, added } = insertServices(content, services);
    content = withServices;
    allAddedServices.push(...added);
  }

  if (domainsToAdd.length === 0 && allAddedServices.length === 0) {
    output.write(`\nNothing new to add -- everything selected is already declared in ${manifestPath}.\n`);
    return;
  }

  // New domains get a branch + environment placement prompt (branch default
  // main; prod mandatory/locked, dev optional/default checked) before the
  // manifest is touched.
  const placements = domainsToAdd.length > 0
    ? (yesFlag
        ? Object.fromEntries(domainsToAdd.map((name) => [name, { dev: "optional" }]))
        : await promptDomainPlacements(domainsToAdd, await readRepoCatalog()))
    : {};

  output.write("\n");
  if (domainsToAdd.length > 0) {
    output.write("Will add to domains:\n");
    for (const name of domainsToAdd) {
      const placement = placements[name] || {};
      const branchNote = placement.branch ? ` (branch: ${placement.branch})` : "";
      const devNote = placement.dev === "disabled" ? " [prod-only]" : placement.dev === "optional" ? " [dev: optional]" : "";
      output.write(`  - ${name}${branchNote}${devNote}\n`);
    }
  }
  if (allAddedServices.length > 0) {
    output.write(`Will add to services: ${allAddedServices.map((service) => service.name).join(", ")}\n`);
  }

  const confirmWrite = yesFlag ? true : await confirmWithSingleKey(`\nUpdate ${manifestPath}?`);
  if (!confirmWrite) {
    throw new Error("Cancelled.");
  }

  for (const name of domainsToAdd) {
    content = insertDomain(content, name, placements[name]);
  }

  await writeFile(manifestPath, content, "utf8");
  output.write(`Updated ${manifestPath}\n`);
  output.write(`Next: run "eco configure" from ${manifestDir}\n`);
}

export async function runComposeRefresh(args) {
  const yesFlag = args.includes("--yes") || args.includes("-y");
  const positionalArgs = args.filter((a) => !a.startsWith("-"));
  const [target] = positionalArgs;
  if (!target) throw new Error("Usage: eco compose refresh <repo-name-or-path> [--yes]");
  const manifestPath = await resolveEcomposeFile(".", process.cwd());
  const manifestDir = path.dirname(manifestPath);
  const estateRoot = path.dirname(manifestDir);
  const workspaceRoot = await findWorkspaceRoot(estateRoot);
  const resolved = await resolveComposeTarget(target, estateRoot, workspaceRoot);
  const services = await discoverServicesAt(resolved.servicesLabel, resolved.serviceDir);
  if (services.length === 0) throw new Error(`No services detected for ${target}.`);
  const realRelPath = path.relative(estateRoot, resolved.serviceDir).split(path.sep).join("/");
  if (realRelPath !== resolved.servicesLabel) {
    for (const service of services) {
      service.path = service.path === resolved.servicesLabel ? realRelPath : `${realRelPath}${service.path.slice(resolved.servicesLabel.length)}`;
    }
  }
  const original = await readFile(manifestPath, "utf8");
  const next = replaceDomainServices(original, resolved.servicesLabel, services);
  output.write(`Will refresh services for ${resolved.servicesLabel}: ${services.map((service) => service.name).join(", ")}\n`);
  const confirmed = yesFlag ? true : await confirmWithSingleKey(`\nUpdate ${manifestPath}?`);
  if (!confirmed) throw new Error("Cancelled.");
  await writeFile(manifestPath, next, "utf8");
  output.write(`Updated ${manifestPath}\n`);
}

export async function runComposeExpose(args) {
  const [serviceName, hostname] = args;
  if (!serviceName || !hostname) throw new Error("Usage: eco compose expose <service> <hostname>");
  const manifestPath = await resolveEcomposeFile(".", process.cwd());
  const original = await readFile(manifestPath, "utf8");
  const next = insertAdditionalExposure(original, serviceName, hostname);
  if (next === original) {
    output.write(`${hostname} is already exposed.\n`);
    return;
  }
  output.write(`Will expose ${serviceName} at ${hostname} and make it available to declared PUBLIC_<DOMAIN>_URL consumers.\n`);
  const confirmed = await confirmWithSingleKey(`\nUpdate ${manifestPath}?`);
  if (!confirmed) throw new Error("Cancelled.");
  await writeFile(manifestPath, next, "utf8");
  output.write(`Updated ${manifestPath}\n`);
}

export async function runCompose(args) {
  const [subcommand, ...rest] = args;

  if (subcommand === "add") {
    await runComposeAdd(rest);
    return;
  }

  if (subcommand === "refresh") {
    await runComposeRefresh(rest);
    return;
  }

  if (subcommand === "expose") {
    await runComposeExpose(rest);
    return;
  }

  throw new Error(`Unknown compose subcommand: ${subcommand ?? "(none)"}\n\nUsage: eco compose add|refresh|expose ...`);
}
