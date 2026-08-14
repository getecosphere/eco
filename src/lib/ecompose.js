import { readFile, readdir, stat } from "node:fs/promises";
import path from "node:path";

import { findWorkspaceRoot } from "./workspace.js";

function getProjectsRoot() {
  return process.env.ECO_PROJECTS_ROOT || path.join(process.env.HOME || "", "projects");
}

function stripQuotes(value) {
  return value.replace(/^["']|["']$/g, "");
}

export async function resolveEcomposeFile(input, startDir = process.cwd()) {
  if (!input) {
    throw new Error("Missing project or ecompose.yml path.");
  }

  const absoluteInput = path.resolve(startDir, input);
  if (absoluteInput.endsWith("ecompose.yml")) {
    return absoluteInput;
  }
  try {
    const info = await stat(absoluteInput);
    if (info.isDirectory()) {
      const directPath = path.join(absoluteInput, "ecompose.yml");
      try {
        await readFile(directPath, "utf8");
        return directPath;
      } catch {}

      const entries = await readdir(absoluteInput, { withFileTypes: true });
      const nestedMatches = [];
      for (const entry of entries) {
        if (!entry.isDirectory()) {
          continue;
        }
        const candidate = path.join(absoluteInput, entry.name, "ecompose.yml");
        try {
          await readFile(candidate, "utf8");
          nestedMatches.push(candidate);
        } catch {}
      }

      if (nestedMatches.length === 1) {
        return nestedMatches[0];
      }

      return directPath;
    }
  } catch {}
  if (path.isAbsolute(input)) {
    return path.join(input, "ecompose.yml");
  }

  const hostProjectPath = path.join(getProjectsRoot(), input, "ecompose.yml");
  try {
    await readFile(hostProjectPath, "utf8");
    return hostProjectPath;
  } catch {}

  const workspaceRoot = await findWorkspaceRoot(startDir);
  return path.join(workspaceRoot, input, "ecompose.yml");
}

export async function readEcompose(input, startDir = process.cwd()) {
  const filePath = await resolveEcomposeFile(input, startDir);
  const content = await readFile(filePath, "utf8");
  return { filePath, content };
}

export function parseProjectName(content) {
  for (const rawLine of content.split(/\r?\n/)) {
    const line = rawLine.replace(/[ \t]+$/, "");
    if (!line || /^\s*#/.test(line)) {
      continue;
    }
    const match = line.match(/^project:\s*(.+)\s*$/);
    if (match) {
      return stripQuotes(match[1]);
    }
  }
  return "";
}

// Renders one `domains:` list entry. Plain `- <name>` stays the legacy
// required-everywhere form; a branch override is `- <name>: <branch>`;
// per-environment placement becomes a nested block:
//
//   domains:
//     - rag:
//         branch: main
//         dev: optional
//
// `dev` may be "optional" (include locally by default, skip gracefully when
// the machine can't run the domain's runtimes) or "disabled" (prod-only).
export function renderDomainEntry(name, { branch, dev } = {}) {
  if (!branch && !dev) {
    return `  - ${name}`;
  }
  const lines = [`  - ${name}:`];
  if (branch) lines.push(`      branch: ${branch}`);
  if (dev) lines.push(`      dev: ${dev}`);
  return lines.join("\n");
}

// The composition repository is the estate's own project-specific repo (a
// `_composition` domain). Unlike reusable catalog domains, its git address
// is not committed to eco/repos.json (that catalog is for shared domains);
// instead it may be declared in ecompose.yml so a fresh host can clone it:
//
//   composition:
//     git: git@github.com:kelastanpatembok/ecosphere_composition.git
//     branch: main
//
// eco up resolves a composition domain by checking repos.json first (for
// backward compatibility with estates registered before this block existed),
// then falls back to this block.
export function parseComposition(content) {
  const composition = {};
  let inComposition = false;

  for (const rawLine of content.split(/\r?\n/)) {
    const line = rawLine.replace(/[ \t]+$/, "");
    if (!line || /^\s*#/.test(line)) {
      continue;
    }

    if (/^composition:\s*$/.test(line)) {
      inComposition = true;
      continue;
    }

    if (!inComposition) {
      continue;
    }

    if (/^[^\s].*:\s*$/.test(line)) {
      break;
    }

    const match = line.match(/^  ([A-Za-z0-9_-]+):\s*(.+)\s*$/);
    if (!match) {
      continue;
    }

    composition[match[1]] = stripQuotes(match[2]);
  }

  return composition;
}

export function parseCtMetadata(content) {
  const metadata = {};
  let inCt = false;

  for (const rawLine of content.split(/\r?\n/)) {
    const line = rawLine.replace(/[ \t]+$/, "");
    if (!line || /^\s*#/.test(line)) {
      continue;
    }

    if (/^ct:\s*$/.test(line)) {
      inCt = true;
      continue;
    }

    if (!inCt) {
      continue;
    }

    if (/^[^\s].*:\s*$/.test(line)) {
      break;
    }

    const match = line.match(/^  ([A-Za-z0-9_-]+):\s*(.+)\s*$/);
    if (!match) {
      continue;
    }

    metadata[match[1]] = stripQuotes(match[2]);
  }

  return metadata;
}

// Storage is estate infrastructure rather than a service runtime. Keeping it
// in the manifest parser lets `eco up` provision it before application
// services are configured, while individual domains remain unaware of where
// the object store is hosted.
export function parseStorage(content) {
  const storage = {};
  let inStorage = false;
  let currentProvider = null;

  for (const rawLine of content.split(/\r?\n/)) {
    const line = rawLine.replace(/[ \t]+$/, "");
    if (!line || /^\s*#/.test(line)) continue;

    if (/^storage:\s*$/.test(line)) {
      inStorage = true;
      continue;
    }
    if (inStorage && /^[^\s].*:\s*$/.test(line)) break;
    if (!inStorage) continue;

    const providerMatch = line.match(/^  ([A-Za-z0-9_-]+):\s*$/);
    if (providerMatch) {
      currentProvider = providerMatch[1];
      storage[currentProvider] = {};
      continue;
    }
    if (!currentProvider) continue;

    const valueMatch = line.match(/^    ([A-Za-z0-9_-]+):\s*(.+)\s*$/);
    if (valueMatch) {
      storage[currentProvider][valueMatch[1]] = stripQuotes(valueMatch[2]);
    }
  }

  return storage;
}

export function parseServices(content) {
  const services = [];
  let inServices = false;
  let currentService = null;
  let inRuntimes = false;

  for (const rawLine of content.split(/\r?\n/)) {
    const line = rawLine.replace(/[ \t]+$/, "");
    if (!line || /^\s*#/.test(line)) {
      continue;
    }

    if (/^services:\s*$/.test(line)) {
      inServices = true;
      continue;
    }

    if (!inServices) {
      continue;
    }

    if (/^[^\s].*:\s*$/.test(line)) {
      break;
    }

    const serviceMatch = line.match(/^  ([A-Za-z0-9_-]+):\s*$/);
    if (serviceMatch) {
      if (currentService) {
        services.push(currentService);
      }
      currentService = {
        name: serviceMatch[1],
        path: "",
        runtimes: []
      };
      inRuntimes = false;
      continue;
    }

    if (!currentService) {
      continue;
    }

    const pathMatch = line.match(/^    path:\s*(.+)\s*$/);
    if (pathMatch) {
      currentService.path = stripQuotes(pathMatch[1]);
      inRuntimes = false;
      continue;
    }

    if (/^    runtimes:\s*$/.test(line)) {
      inRuntimes = true;
      continue;
    }

    const runtimeMatch = line.match(/^      -\s*(.+)\s*$/);
    if (inRuntimes && runtimeMatch) {
      currentService.runtimes.push(stripQuotes(runtimeMatch[1]));
      continue;
    }

    if (/^    [A-Za-z0-9_-]+:\s*/.test(line)) {
      inRuntimes = false;
    }
  }

  if (currentService) {
    services.push(currentService);
  }

  return services;
}

// expose.additional lets one estate expose more than one service under
// more than one public hostname through the same Cloudflare tunnel, e.g.
// a WebSocket/game server that needs its own subdomain separate from the
// main frontend (chronic's gameserver at ws-chronic.battlerivals.online,
// alongside the frontend at chronic.battlerivals.online):
//
//   expose:
//     hostname: chronic.battlerivals.online
//     service: chronic_bootstrap
//     additional:
//       - hostname: ws-chronic.battlerivals.online
//         service: gameserver
//
// Each entry needs its own DNS record and ingress rule merged into the
// same tunnel (see up.js's upsertCloudflaredHostname/overwriteDnsRecordForTunnel,
// already used for merging webhook-ingress hostnames the same way) rather
// than a second tunnel -- one estate, one Cloudflare tunnel, N hostnames.
export function parseExpose(content) {
  const expose = {};
  let inExpose = false;
  let inAdditional = false;
  let currentAdditional = null;
  const additional = [];

  const flushCurrent = () => {
    if (currentAdditional) {
      additional.push(currentAdditional);
      currentAdditional = null;
    }
  };

  for (const rawLine of content.split(/\r?\n/)) {
    const line = rawLine.replace(/[ \t]+$/, "");
    if (!line || /^\s*#/.test(line)) {
      continue;
    }

    if (/^expose:\s*$/.test(line)) {
      inExpose = true;
      continue;
    }

    if (!inExpose) {
      continue;
    }

    if (/^[^\s].*:\s*$/.test(line)) {
      break;
    }

    if (/^  additional:\s*$/.test(line)) {
      inAdditional = true;
      continue;
    }

    if (inAdditional) {
      const itemMatch = line.match(/^    -\s*([A-Za-z0-9_-]+):\s*(.+)\s*$/);
      if (itemMatch) {
        flushCurrent();
        currentAdditional = { [itemMatch[1]]: stripQuotes(itemMatch[2]) };
        continue;
      }

      const fieldMatch = line.match(/^      ([A-Za-z0-9_-]+):\s*(.+)\s*$/);
      if (fieldMatch && currentAdditional) {
        currentAdditional[fieldMatch[1]] = stripQuotes(fieldMatch[2]);
        continue;
      }

      // A line here that isn't part of an additional: entry means that
      // block has ended -- fall through to normal top-level key handling.
      inAdditional = false;
      flushCurrent();
    }

    const match = line.match(/^  ([A-Za-z0-9_-]+):\s*(.+)\s*$/);
    if (!match) {
      continue;
    }

    expose[match[1]] = stripQuotes(match[2]);
  }

  flushCurrent();
  if (additional.length > 0) {
    expose.additional = additional;
  }

  if (expose.tunnel_replicas !== undefined) {
    const parsed = parseInt(expose.tunnel_replicas, 10);
    if (!Number.isNaN(parsed) && parsed > 0) {
      expose.tunnel_replicas = parsed;
    }
  }

  return expose;
}

export function parseDeploy(content) {
  const deploy = {};
  let inDeploy = false;
  let currentSection = "";

  for (const rawLine of content.split(/\r?\n/)) {
    const line = rawLine.replace(/[ \t]+$/, "");
    if (!line || /^\s*#/.test(line)) {
      continue;
    }

    if (/^deploy:\s*$/.test(line)) {
      inDeploy = true;
      continue;
    }

    if (!inDeploy) {
      continue;
    }

    if (/^[^\s].*:\s*$/.test(line)) {
      break;
    }

    const sectionMatch = line.match(/^  ([A-Za-z0-9_-]+):\s*$/);
    if (sectionMatch) {
      currentSection = sectionMatch[1];
      if (!deploy[currentSection]) {
        deploy[currentSection] = {};
      }
      continue;
    }

    const valueMatch = line.match(/^    ([A-Za-z0-9_-]+):\s*(.+)\s*$/);
    if (currentSection && valueMatch) {
      deploy[currentSection][valueMatch[1]] = stripQuotes(valueMatch[2]);
    }
  }

  return deploy;
}

// The staging footprint is a second deployment of the same estate on a
// separate CT, exposed at a `staging-` prefixed hostname, that tracks any
// pushed branch except the prod deploy branch. Declared like:
//
//   staging:
//     ct: 1000
//     hostname: staging.stuff8.com   # derived from expose.hostname if omitted
//
// `ct` is required (a different container from the prod ct.id). `hostname`
// defaults to the apex-safe `staging-` derivation of expose.hostname.
export function parseStaging(content) {
  const staging = {};
  let inStaging = false;

  for (const rawLine of content.split(/\r?\n/)) {
    const line = rawLine.replace(/[ \t]+$/, "");
    if (!line || /^\s*#/.test(line)) {
      continue;
    }

    if (/^staging:\s*$/.test(line)) {
      inStaging = true;
      continue;
    }

    if (!inStaging) {
      continue;
    }

    if (/^[^\s].*:\s*$/.test(line)) {
      break;
    }

    const match = line.match(/^  ([A-Za-z0-9_-]+):\s*(.+)\s*$/);
    if (!match) {
      continue;
    }

    staging[match[1]] = stripQuotes(match[2]);
  }

  return staging;
}
