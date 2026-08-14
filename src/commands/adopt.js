import { access, readdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { createInterface } from "node:readline/promises";
import { stdin as input, stdout as output } from "node:process";

// Same defaults `startproject` seeds new estates with -- kept in sync by hand
// since this command generates a manifest for an already-existing project
// rather than scaffolding one from repos.json, so it doesn't share
// startproject's module.
const DEFAULT_CT = {
  template: "local:vztmpl/eco-npm-rust-mongo_1_amd64.tar.zst",
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

const IGNORED_DIR_NAMES = new Set([
  "node_modules",
  "target",
  ".next",
  ".git",
  "dist",
  "build",
  "vendor",
  ".venv",
  "__pycache__"
]);

async function pathExists(targetPath) {
  try {
    await access(targetPath);
    return true;
  } catch {
    return false;
  }
}

async function readTextFile(targetPath) {
  try {
    return await readFile(targetPath, "utf8");
  } catch (error) {
    if (error.code === "ENOENT") return null;
    throw error;
  }
}

// Same runtime-type detection configure.sh's discover_services already does
// at run time (pom.xml/Cargo.toml/package.json, next vs vite vs plain node) --
// mirrored here so the generated ecompose.yml matches what auto-discovery
// would have found anyway, just captured explicitly and persistently.
async function detectServiceType(dir) {
  if (await pathExists(path.join(dir, "pom.xml"))) {
    return { type: "spring-boot", runtimes: ["java@17", "maven"] };
  }
  if (await pathExists(path.join(dir, "Cargo.toml"))) {
    return { type: "rust", runtimes: ["rust"] };
  }
  const packageJsonPath = path.join(dir, "package.json");
  const packageJsonRaw = await readTextFile(packageJsonPath);
  if (packageJsonRaw !== null) {
    let deps = {};
    try {
      const parsed = JSON.parse(packageJsonRaw);
      deps = { ...parsed.dependencies, ...parsed.devDependencies };
    } catch {
      // Malformed package.json -- still a node service, just can't refine the type.
    }
    if (deps.next) return { type: "nextjs", runtimes: ["node@20", "npm", "pm2"] };
    if (deps.vite) return { type: "vite", runtimes: ["node@20", "npm", "pm2"] };
    return { type: "node", runtimes: ["node@20", "npm", "pm2"] };
  }
  return null;
}

// Peer-URL env vars already tell us about cross-service deps (see
// detectSiblingDependencies in repos.js); the same .env.example/.env files
// also tell us which databases a service actually talks to, so the
// generated runtimes list doesn't need mongodb@7/postgresql@15 hand-added
// after the fact.
async function detectDbRuntimes(dir) {
  let contents = await readTextFile(path.join(dir, ".env.example"));
  if (contents === null) contents = await readTextFile(path.join(dir, ".env"));
  const cargoToml = await readTextFile(path.join(dir, "Cargo.toml"));

  const runtimes = [];
  if (/^\s*MONGO(?:DB)?_URI=/m.test(contents || "") || /^\s*mongodb\s*=/m.test(cargoToml || "")) runtimes.push("mongodb@7");
  if (/^\s*REDIS_URL=/m.test(contents || "") || /^\s*redis\s*=/m.test(cargoToml || "")) runtimes.push("redis@7");
  if (/^\s*(?:DATABASE_URL|DB_URL)=.*postgres/im.test(contents || "")) runtimes.push("postgresql@15");
  return runtimes;
}

// Mirrors configure.sh's _scan_dir_rec: stop recursing into a directory the
// moment it looks like a service (a project marker file present), so a
// service's own internal subdirectories are never split into extra entries.
async function scanForServices(scanDir, label, relPath = "") {
  const detected = await detectServiceType(scanDir);
  if (detected) {
    const name = relPath ? `${label}-${relPath.split(path.sep).join("-")}` : label;
    const dbRuntimes = await detectDbRuntimes(scanDir);
    return [
      {
        name,
        path: relPath ? `${label}/${relPath.split(path.sep).join("/")}` : label,
        runtimes: [...detected.runtimes, ...dbRuntimes]
      }
    ];
  }

  const entries = await readdir(scanDir, { withFileTypes: true });
  const services = [];
  for (const entry of entries) {
    if (!entry.isDirectory() || IGNORED_DIR_NAMES.has(entry.name)) continue;
    const nextRelPath = relPath ? path.join(relPath, entry.name) : entry.name;
    services.push(...(await scanForServices(path.join(scanDir, entry.name), label, nextRelPath)));
  }
  return services;
}

export async function discoverEstateServices(estateRoot) {
  const entries = await readdir(estateRoot, { withFileTypes: true });
  const services = [];
  for (const entry of entries) {
    if (!entry.isDirectory() || IGNORED_DIR_NAMES.has(entry.name)) continue;
    services.push(...(await scanForServices(path.join(estateRoot, entry.name), entry.name)));
  }
  return services;
}

// Scans a single already-known directory (e.g. one repo just cloned by
// `eco compose add`), rather than every child of an estate root -- same
// stop-at-marker detection either way, just entered from a single known
// label/dir instead of discoverEstateServices' own directory listing.
export async function discoverServicesAt(label, dirPath) {
  return scanForServices(dirPath, label);
}

export function renderServiceBlock(service) {
  const runtimeLines = service.runtimes.map((runtime) => `      - ${runtime}`).join("\n");
  return [`  ${service.name}:`, `    path: ${service.path}`, "    runtimes:", runtimeLines].join("\n");
}

export function buildEcomposeContent({ projectName, ctId, hostname, services }) {
  const lines = [
    `project: ${projectName}`,
    "",
    "ct:",
    `  id: ${ctId}`,
    `  hostname: ${hostname}`,
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
    "shared_tools:"
  ];

  for (const tool of DEFAULT_SHARED_TOOLS) {
    lines.push(`  - ${tool}`);
  }

  lines.push("", "services:");
  for (const service of services) {
    lines.push(renderServiceBlock(service), "");
  }

  return `${lines.join("\n")}\n`;
}

export async function runAdopt(args) {
  const targetDir = path.resolve(process.cwd(), args[0] ?? ".");
  const estateRoot = path.dirname(targetDir);
  const ecomposePath = path.join(targetDir, "ecompose.yml");

  if (!(await pathExists(targetDir))) {
    throw new Error(`No such directory: ${targetDir}`);
  }
  if (await pathExists(ecomposePath)) {
    throw new Error(`ecompose.yml already exists at ${ecomposePath} -- edit it directly, or remove it first to regenerate.`);
  }

  const services = await discoverEstateServices(estateRoot);

  const rl = createInterface({ input, output });
  try {
    output.write(`Adopting project into eco:\n`);
    output.write(`  manifest dir: ${targetDir}\n`);
    output.write(`  estate root:  ${estateRoot}\n\n`);

    if (services.length === 0) {
      output.write(
        "No services detected (looked for pom.xml/Cargo.toml/package.json in every top-level directory under the estate root).\n" +
          "You can still generate a manifest and add services to it by hand.\n\n"
      );
    } else {
      output.write("Detected services:\n");
      for (const service of services) {
        output.write(`  ${service.name} -- path: ${service.path}, runtimes: ${service.runtimes.join(", ") || "(none)"}\n`);
      }
      output.write("\n");
    }

    const defaultProjectName = path.basename(estateRoot);
    const projectName = ((await rl.question(`Project name [${defaultProjectName}]: `)).trim()) || defaultProjectName;

    const ctIdInput = (await rl.question("Proxmox CT id (leave blank to fill in later): ")).trim();
    const ctId = ctIdInput || 0;

    const hostname = ((await rl.question(`CT hostname [${projectName}]: `)).trim()) || projectName;

    const content = buildEcomposeContent({ projectName, ctId, hostname, services });

    output.write(`\nProposed ${ecomposePath}:\n\n${content}\n`);
    output.write(
      "Note: expose/deploy blocks are intentionally left out -- add them by hand\n" +
        "(see assessment/assessment/ecompose.yml or training/training_bootstrap/ecompose.yml\n" +
        "for examples) once this estate has a public hostname or webhook deploy set up.\n\n"
    );

    const confirmation = (await rl.question(`Write this to ${ecomposePath}? [y/N]: `)).trim().toLowerCase();
    if (confirmation !== "y" && confirmation !== "yes") {
      throw new Error("Cancelled.");
    }

    await writeFile(ecomposePath, content, "utf8");
    process.stdout.write(`Wrote ${ecomposePath}\n`);
    process.stdout.write(`Next: run "eco configure" from ${targetDir}\n`);
  } finally {
    rl.close();
  }
}
