import { spawn } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";

import { defaultRegistryPath, readRegistryAll } from "../lib/registry.js";
import { parseCtMetadata, readEcompose } from "../lib/ecompose.js";

const output = process.stdout;
const color = output.isTTY && !process.env.NO_COLOR;
const bold = (s) => (color ? `\x1b[1m${s}\x1b[0m` : s);
const dim = (s) => (color ? `\x1b[2m${s}\x1b[0m` : s);
const cyan = (s) => (color ? `\x1b[36m${s}\x1b[0m` : s);
const yellow = (s) => (color ? `\x1b[33m${s}\x1b[0m` : s);
const green = (s) => (color ? `\x1b[32m${s}\x1b[0m` : s);
const red = (s) => (color ? `\x1b[31m${s}\x1b[0m` : s);

function runCapture(command, args) {
  return new Promise((resolve) => {
    const child = spawn(command, args, { stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (c) => { stdout += c; });
    child.stderr.on("data", (c) => { stderr += c; });
    child.on("error", () => resolve({ code: -1, stdout, stderr }));
    child.on("exit", (code) => resolve({ code: code ?? -1, stdout, stderr }));
  });
}

async function whichPct() {
  const r = await runCapture("which", ["pct"]);
  return r.code === 0;
}

function parseArgs(args) {
  const positionals = [];
  const options = {};
  for (let i = 0; i < args.length; i++) {
    const token = args[i];
    if (token.startsWith("--")) {
      const eq = token.indexOf("=");
      if (eq !== -1) {
        options[token.slice(2, eq)] = token.slice(eq + 1);
      } else {
        const next = args[i + 1];
        if (next !== undefined && !next.startsWith("--")) {
          options[token.slice(2)] = next;
          i++;
        } else {
          options[token.slice(2)] = true;
        }
      }
    } else {
      positionals.push(token);
    }
  }
  return { positionals, options };
}

function dashboardHelp() {
  output.write(`eco dashboard\n\nUsage:\n  eco dashboard [ctid]\n  eco dashboard --ct <ctid>\n\nDisplay a live summary of every managed estate on a target CT: reserved\nports, each estate's services with their assigned port, binary location,\nand PM2 running state. Reads the estate registry (SQLite) plus PM2.\n\nFrom a Proxmox host the registry and PM2 state are pulled from the CT via\npct; run inside an estate directory (or pass the CT id) to choose the\ntarget. On a dev machine without pct it reads the local registry and PM2.\n`);
}

async function resolveTargetCt(positionals, options, cwd) {
  if (options.ct) return options.ct;
  if (positionals[0]) return positionals[0];

  try {
    const deployment = await readEcompose(".", cwd);
    const ct = parseCtMetadata(deployment.content);
    if (ct.id) return ct.id;
  } catch {}
  return null;
}

function renderPortType(type) {
  switch (type) {
    case "gateway": return "gateway";
    case "index": return "index";
    default: return "service";
  }
}

async function fetchCtRegistry(ctid) {
  const tempDir = await mkdtemp(path.join(tmpdir(), "eco-dash-"));
  const localPath = path.join(tempDir, "registry.db");
  try {
    const pull = await runCapture("pct", ["pull", String(ctid), "/etc/eco/registry.db", localPath]);
    if (pull.code !== 0) {
      return { registryPath: null, reason: pull.stderr.trim() || `pct pull failed with code ${pull.code}` };
    }
    return { registryPath: localPath, tempDir };
  } catch {
    return { registryPath: null, reason: "no registry on CT" };
  }
}

async function fetchCtPm2(ctid) {
  const jlist = await runCapture("pct", ["exec", String(ctid), "--", "pm2", "jlist"]);
  if (jlist.code !== 0) return [];
  try {
    return JSON.parse(jlist.stdout);
  } catch {
    return [];
  }
}

async function fetchLocalPm2() {
  const jlist = await runCapture("pm2", ["jlist"]);
  if (jlist.code !== 0) return [];
  try {
    return JSON.parse(jlist.stdout);
  } catch {
    return [];
  }
}

function pm2AppName(project, service) {
  return `${project}-${service}`;
}

function buildProcessIndex(procs) {
  const index = new Map();
  for (const proc of procs) {
    const name = proc?.name;
    if (!name) continue;
    const env = proc?.pm2_env || {};
    index.set(name, {
      name,
      status: env.status || "unknown",
      restarts: env.restart_time ?? 0,
      uptime: env.pm_uptime,
      pid: proc?.pid,
      memory: proc?.monit?.memory,
      execPath: env.pm_exec_path || env.pm_cwd || ""
    });
  }
  return index;
}

function fmtMemory(bytes) {
  if (!Number.isFinite(bytes)) return "—";
  const mb = bytes / (1024 * 1024);
  if (mb < 1024) return `${mb.toFixed(0)} MB`;
  return `${(mb / 1024).toFixed(2)} GB`;
}

function fmtUptime(ms) {
  if (!ms) return "—";
  const s = Math.floor((Date.now() - ms) / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h`;
  const d = Math.floor(h / 24);
  return `${d}d`;
}

function statusBadge(status) {
  switch (status) {
    case "online": return green("online");
    case "stopped": return yellow("stopped");
    case "errored": return red("errored");
    default: return dim(status || "—");
  }
}

function renderDashboard({ registryPath, reserved, ports, dbs, procs, scopeLabel, sourceLabel }) {
  output.write(`\n${bold("Eco dashboard")}  ${dim(sourceLabel)}\n`);
  output.write(`  ${cyan("registry")}  ${registryPath}\n`);
  if (scopeLabel) {
    output.write(`  ${cyan("scope")}     ${scopeLabel}\n`);
  }

  const processIndex = buildProcessIndex(procs);

  output.write(`\n${bold("Reserved ports")}\n`);
  if (reserved.length === 0) {
    output.write("  (none)\n");
  }
  for (const r of reserved) {
    output.write(`  ${yellow(String(r.port).padEnd(6))}  ${r.label}\n`);
  }

  const byProject = new Map();
  for (const port of ports) {
    if (!byProject.has(port.project)) {
      byProject.set(port.project, { services: [], dbs: [] });
    }
    byProject.get(port.project).services.push(port);
  }
  for (const db of dbs) {
    if (!byProject.has(db.project)) {
      byProject.set(db.project, { services: [], dbs: [] });
    }
    byProject.get(db.project).dbs.push(db);
  }

  if (byProject.size === 0) {
    output.write(`\n${yellow("No estates recorded in the registry yet.")}\n`);
  }

  for (const [project, { services, dbs: projectDbs }] of [...byProject].sort(([a], [b]) => a.localeCompare(b))) {
    output.write(`\n${bold("Estate")}  ${cyan(project)}\n`);

    const sorted = [...services].sort((a, b) => a.port - b.port);
    for (const svc of sorted) {
      const proc = processIndex.get(pm2AppName(project, svc.service));
      const procLabel = proc ? `${statusBadge(proc.status)}  ${dim(fmtUptime(proc.uptime))}  ${dim(fmtMemory(proc.memory))}` : dim("not managed by pm2 here");
      const binary = proc?.execPath || dim("—");
      output.write(`  ${cyan(String(svc.port).padEnd(6))}  ${svc.service.padEnd(22)}  ${renderPortType(svc.type).padEnd(8)}  ${String(svc.env_var).padEnd(12)}  ${binary}\n`);
      output.write(`     ${dim(`pm2: ${pm2AppName(project, svc.service)}`)}  ${procLabel}\n`);
    }

    if (projectDbs.length > 0) {
      output.write(`  ${bold("databases")}\n`);
      for (const db of projectDbs) {
        const dbPart = db.db_name ? ` db=${db.db_name}` : "";
        const userPart = db.username ? ` user=${db.username}` : "";
        output.write(`     ${cyan(String(db.port).padEnd(6))}  ${db.db_type.padEnd(8)}  ${db.service}${dbPart}${userPart}\n`);
      }
    }
  }

  output.write("\n");
}

export async function runDashboard(args) {
  const { positionals, options } = parseArgs(args);
  if (positionals[0] === "help" || positionals[0] === "--help" || positionals[0] === "-h" || options.help) {
    dashboardHelp();
    return;
  }

  const onHost = await whichPct();

  if (onHost) {
    const ctid = await resolveTargetCt(positionals, options, process.cwd());
    if (!ctid) {
      throw new Error("Usage: eco dashboard <ctid> (or run inside an estate directory).");
    }

    const { registryPath, tempDir, reason } = await fetchCtRegistry(ctid);
    const procs = await fetchCtPm2(ctid);

    if (!registryPath) {
      output.write(`\n${yellow(`No registry found on CT ${ctid}${reason ? ` — ${reason}` : ""}.`)}\n`);
      output.write("The estate may not have been configured yet. Run eco up to allocate ports.\n");
      await (tempDir ? rm(tempDir, { recursive: true, force: true }) : Promise.resolve());
      return;
    }

    try {
      const data = await readRegistryAll({ registryPath });
      renderDashboard({
        registryPath,
        reserved: data.reserved,
        ports: data.ports,
        dbs: data.dbs,
        procs,
        scopeLabel: data.ports[0]?.scope || data.dbs[0]?.scope || data.reserved[0]?.scope || "",
        sourceLabel: `CT ${ctid}`
      });
    } finally {
      if (tempDir) {
        await rm(tempDir, { recursive: true, force: true }).catch(() => {});
      }
    }
    return;
  }

  // Dev machine: read the local registry and local PM2.
  const registryPath = defaultRegistryPath();
  const procs = await fetchLocalPm2();
  try {
    const data = await readRegistryAll({ registryPath });
    renderDashboard({
      registryPath,
      reserved: data.reserved,
      ports: data.ports,
      dbs: data.dbs,
      procs,
      scopeLabel: data.ports[0]?.scope || data.dbs[0]?.scope || data.reserved[0]?.scope || "",
      sourceLabel: "local machine"
    });
  } catch (error) {
    output.write(`\n${yellow(`No registry found at ${registryPath}.`)}\n`);
    output.write("Run eco configure / eco up in an estate to allocate ports.\n");
  }
}
