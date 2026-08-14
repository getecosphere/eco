import { readFile } from "node:fs/promises";
import path from "node:path";
import { parseCtMetadata, parseDeploy, parseExpose, parseProjectName, parseServices } from "../lib/ecompose.js";

const output = process.stdout;
const color = output.isTTY && !process.env.NO_COLOR;
const bold  = (s) => color ? `\x1b[1m${s}\x1b[0m` : s;
const dim   = (s) => color ? `\x1b[2m${s}\x1b[0m` : s;
const cyan  = (s) => color ? `\x1b[36m${s}\x1b[0m` : s;
const sep   = () => color ? `\x1b[2m${"─".repeat(48)}\x1b[0m` : "─".repeat(48);

async function findEcomposeFile(startDir) {
  let dir = path.resolve(startDir);
  const { root } = path.parse(dir);

  while (true) {
    // direct
    try {
      const candidate = path.join(dir, "ecompose.yml");
      await readFile(candidate, "utf8");
      return candidate;
    } catch {}

    // *_bootstrap sibling (estate root pattern)
    try {
      const { readdir } = await import("node:fs/promises");
      const entries = await readdir(dir, { withFileTypes: true });
      for (const entry of entries) {
        if (entry.isDirectory() && entry.name.endsWith("_bootstrap")) {
          const candidate = path.join(dir, entry.name, "ecompose.yml");
          try {
            await readFile(candidate, "utf8");
            return candidate;
          } catch {}
        }
      }
    } catch {}

    if (dir === root) break;
    dir = path.dirname(dir);
  }

  throw new Error("No ecompose.yml found. Run from inside a project directory.");
}

async function readPortsFromEcosystem(ecosystemPath) {
  try {
    const { createRequire } = await import("node:module");
    const req = createRequire(import.meta.url);
    // bust require cache so stale config isn't returned
    delete req.cache[req.resolve(ecosystemPath)];
    const eco = req(ecosystemPath);
    const ports = {};
    for (const app of (eco.apps || [])) {
      const env = app.env || {};
      const port = env.PORT || env.SERVER_PORT;
      if (port) ports[app.name] = String(port);
    }
    return ports;
  } catch {
    return {};
  }
}

function runtimeLabel(runtimes) {
  if (!runtimes || runtimes.length === 0) return dim("—");
  return runtimes.join(", ");
}

export async function runShow(args) {
  const filePath = await findEcomposeFile(process.cwd());
  const content  = await readFile(filePath, "utf8");

  const projectName = parseProjectName(content);
  const ct          = parseCtMetadata(content);
  const services    = parseServices(content);
  const expose      = parseExpose(content);
  const deploy      = parseDeploy(content);

  // supplement with ports from ecosystem.config.js if available
  const ecosystemPath = path.join(path.dirname(filePath), "ecosystem.config.js");
  const ports = await readPortsFromEcosystem(ecosystemPath);

  output.write(`\n${sep()}\n`);
  output.write(`  ${bold(projectName)}\n`);
  output.write(`  ${dim(filePath)}\n`);
  output.write(`${sep()}\n\n`);

  // smallest port first, so the user knows what to open in the browser first
  const [firstService] = [...services]
    .filter((svc) => ports[`${projectName}-${svc.name}`])
    .sort((a, b) => Number(ports[`${projectName}-${a.name}`]) - Number(ports[`${projectName}-${b.name}`]));
  if (firstService) {
    const firstPort = ports[`${projectName}-${firstService.name}`];
    output.write(`  ${bold("open first")}  ${cyan(`http://localhost:${firstPort}`)}  ${dim(`(${firstService.name})`)}\n\n`);
  }

  // CT
  if (ct.id || ct.hostname) {
    output.write(`  ${bold("Container")}\n`);
    if (ct.id)       output.write(`    ${cyan("id")}        ${ct.id}\n`);
    if (ct.hostname) output.write(`    ${cyan("hostname")}  ${ct.hostname}\n`);
    if (ct.cores)    output.write(`    ${cyan("cores")}     ${ct.cores}\n`);
    if (ct.memory)   output.write(`    ${cyan("memory")}    ${ct.memory} MB\n`);
    output.write("\n");
  }

  // Services
  if (services.length > 0) {
    output.write(`  ${bold("Services")}\n`);
    // smallest port first, so the user knows what to open in the browser first;
    // services without a port sort last
    const sortedServices = [...services].sort((a, b) => {
      const portA = Number(ports[`${projectName}-${a.name}`]) || Infinity;
      const portB = Number(ports[`${projectName}-${b.name}`]) || Infinity;
      return portA - portB;
    });
    for (const svc of sortedServices) {
      const appName = `${projectName}-${svc.name}`;
      const port = ports[appName];
      output.write(`\n    ${bold(svc.name)}\n`);
      output.write(`      ${cyan("path")}      ${svc.path || dim("—")}\n`);
      output.write(`      ${cyan("runtimes")}  ${runtimeLabel(svc.runtimes)}\n`);
      if (port) output.write(`      ${cyan("port")}      ${port}\n`);
    }
    output.write("\n");
  }

  // Expose
  if (expose.enabled === "true" || expose.hostname) {
    output.write(`  ${bold("Expose")}\n`);
    if (expose.hostname) output.write(`    ${cyan("hostname")}  ${expose.hostname}\n`);
    if (expose.service)  output.write(`    ${cyan("service")}   ${expose.service}\n`);
    output.write("\n");
  }

  // Deploy
  const github = deploy.github || {};
  if (github.enabled === "true") {
    output.write(`  ${bold("Deploy")}\n`);
    if (github.branch)       output.write(`    ${cyan("branch")}        ${github.branch}\n`);
    if (github.webhook_port) output.write(`    ${cyan("webhook_port")}  ${github.webhook_port}\n`);
    output.write("\n");
  }

  output.write(`${sep()}\n\n`);
}
