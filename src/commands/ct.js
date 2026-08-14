import { spawn } from "node:child_process";
import { readdir, rename } from "node:fs/promises";
import path from "node:path";
import { parseCtMetadata, readEcompose } from "../lib/ecompose.js";

function ctHelp() {
  process.stdout.write(`eco ct

Manage Proxmox CT lifecycle from eco.

Usage:
  eco ct create <project> [overrides]
  eco ct start <project|ctid>
  eco ct stop <project|ctid>
  eco ct reboot <project|ctid>
  eco ct status <project|ctid>
  eco ct template <project|ctid> --name <name> --clone-id <ctid> [options]

Create override options:
  --id <ctid>             Override CT ID from ecompose.yml
  --hostname <name>       Override CT hostname
  --cores <n>             Override cores
  --memory <mb>           Override memory
  --swap <mb>             Override swap
  --disk <gb>             Override root disk size in GB
  --storage <name>        Override rootfs storage, e.g. local-lvm
  --template <ref>        Override template reference for pct create
  --bridge <name>         Override network bridge, e.g. vmbr0
  --ip <value>            Override IP, default from ecompose.yml
  --gateway <value>       Optional gateway when using static IP
  --unprivileged <0|1>    Override unprivileged flag
  --password <value>      Optional root password
  --start                 Start the CT after creation
  --dry-run               Print the pct commands without executing them

Template options (turns an already-provisioned CT into a reusable
Proxmox vztmpl archive -- see "Custom CT Template Strategy" in
eco/README.md):
  --name <name>            Required. Base name for the archive, e.g. "eco-rust-base"
  --clone-id <ctid>        Required. CT ID to use for the disposable clone this builds from
  --version <version>      Version tag in the filename (default: today, YYYYMMDD)
  --workspace-root <path>  Project workspace to wipe before exporting (default: /opt/projects)
  --mongo-data-dir <path>  MongoDB data dir to wipe before exporting (default: /var/lib/mongodb)
  --dumpdir <path>         Where vzdump writes the archive (default: /var/lib/vz/template/cache)
  --storage <name>         Storage for the clone's rootfs (default: same as source)
  --keep-clone             Don't destroy the disposable clone after export (for debugging)
  --dry-run                Print the plan without executing it

The source CT keeps running untouched -- this clones it, cleans
project-specific state (code, PM2 state, DB data, SSH keys, machine-id)
out of the clone, exports the clone as an archive, then destroys the
clone. Never run this against a CT you want to keep as-is; always
point --clone-id at a fresh, disposable ID.

Examples:
  eco ct create assessment --dry-run
  eco ct create assessment --start
  eco ct status assessment
  eco ct stop 101
  eco ct template 105 --name eco-rust-base --clone-id 199 --dry-run
  eco ct template rust --name eco-rust-base --clone-id 199
`);
}

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
    if (key === "start" || key === "dry-run") {
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

function required(options, key) {
  const value = options[key];
  if (value === undefined || value === "") {
    throw new Error(`Missing required option --${key}`);
  }
  return value;
}

function mergeCtOptions(manifestOptions, cliOptions) {
  return { ...manifestOptions, ...cliOptions };
}

async function resolveProjectCtOptions(projectInput) {
  const { filePath, content } = await readEcompose(projectInput);
  const ctOptions = parseCtMetadata(content);

  if (!ctOptions.id) {
    throw new Error(`Missing ct.id in ${filePath}`);
  }
  if (!ctOptions.template) {
    throw new Error(`Missing ct.template in ${filePath}`);
  }
  if (!ctOptions.storage) {
    throw new Error(`Missing ct.storage in ${filePath}`);
  }
  if (!ctOptions.disk) {
    throw new Error(`Missing ct.disk in ${filePath}`);
  }
  if (!ctOptions.bridge) {
    throw new Error(`Missing ct.bridge in ${filePath}`);
  }

  return { filePath, ctOptions };
}

function runCommand(command, args) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
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

function buildNet0(options) {
  const bridge = required(options, "bridge");
  const ip = options.ip || "dhcp";
  const parts = [`name=eth0`, `bridge=${bridge}`, `ip=${ip}`];
  if (options.gateway) {
    parts.push(`gw=${options.gateway}`);
  }
  return parts.join(",");
}

function buildCreateArgs(name, options) {
  const id = required(options, "id");
  const template = required(options, "template");
  const storage = required(options, "storage");
  const disk = required(options, "disk");
  const hostname = options.hostname || name;
  const cores = options.cores || "2";
  const memory = options.memory || "4096";
  const swap = options.swap || "1024";
  const unprivileged = options.unprivileged || "1";
  const rootfs = `${storage}:${disk}`;

  const args = [
    "create",
    id,
    template,
    "--hostname",
    hostname,
    "--cores",
    String(cores),
    "--memory",
    String(memory),
    "--swap",
    String(swap),
    "--rootfs",
    rootfs,
    "--net0",
    buildNet0(options),
    "--unprivileged",
    String(unprivileged)
  ];

  if (options.password) {
    args.push("--password", options.password);
  }

  return args;
}

async function runCreate(args) {
  const { options, positionals } = parseOptions(args);
  const project = positionals[0];
  if (!project) {
    throw new Error(`Missing project.\n\n${ctCreateUsage()}`);
  }

  const { filePath, ctOptions } = await resolveProjectCtOptions(project);
  const mergedOptions = mergeCtOptions(ctOptions, options);
  const name = mergedOptions.name || mergedOptions.hostname || project;
  const createArgs = buildCreateArgs(name, mergedOptions);
  const commands = [["pct", createArgs]];

  if (mergedOptions.start) {
    commands.push(["pct", ["start", required(mergedOptions, "id")]]);
  }

  if (mergedOptions["dry-run"]) {
    process.stdout.write(`eco ct create plan\n`);
    process.stdout.write(`Manifest: ${filePath}\n\n`);
    commands.forEach(([command, commandArgs]) => {
      process.stdout.write(`${command} ${commandArgs.join(" ")}\n`);
    });
    return;
  }

  for (const [command, commandArgs] of commands) {
    await runCommand(command, commandArgs);
  }
}

function ctCreateUsage() {
  return `Usage:
  eco ct create <project> [overrides]`;
}

async function resolveCtIdInput(input) {
  if (!input) {
    return "";
  }
  if (/^\d+$/.test(input)) {
    return input;
  }
  const { ctOptions } = await resolveProjectCtOptions(input);
  return ctOptions.id;
}

async function runSimplePct(subcommand, args) {
  const target = args[0];
  if (!target) {
    throw new Error(`Missing CT ID for "eco ct ${subcommand}"`);
  }
  const ctid = await resolveCtIdInput(target);
  await runCommand("pct", [subcommand, ctid]);
}

function runCapture(command, args) {
  return new Promise((resolve) => {
    const child = spawn(command, args, { stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => { stdout += chunk; });
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    child.on("error", () => resolve({ code: 1, stdout, stderr }));
    child.on("close", (code) => resolve({ code: code ?? 1, stdout, stderr }));
  });
}

async function waitForCtExec(ctid, { attempts = 20, delayMs = 1000 } = {}) {
  for (let i = 0; i < attempts; i += 1) {
    const result = await runCapture("pct", ["exec", String(ctid), "--", "true"]);
    if (result.code === 0) {
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, delayMs));
  }
  throw new Error(`CT ${ctid} did not become exec-ready in time (waited ${attempts * delayMs}ms).`);
}

function shQuote(value) {
  return `'${String(value).replace(/'/g, `'\\''`)}'`;
}

// Strips everything about a cloned CT that's specific to the estate it
// was cloned from, so the resulting template is safe to reuse: project
// code (re-cloned fresh by `eco up` every time anyway), PM2 process
// state (so a CT booted from this template doesn't try to resurrect the
// source estate's processes), database contents (the *service* should
// be templated, never one estate's actual data), and anything
// machine/identity-specific (SSH host keys, machine-id, and -- the
// important one -- /root/.ssh, which is the source host's real copied-in
// git credentials; baking those into a shared template would leak them
// into every CT anyone ever creates from it).
function buildCleanupScript({ workspaceRoot, mongoDataDir }) {
  return [
    "pm2 delete all >/dev/null 2>&1 || true",
    "rm -f /root/.pm2/dump.pm2",
    "rm -rf /root/.pm2/logs/*",
    `rm -rf ${shQuote(workspaceRoot)}/*`,
    "systemctl stop mongod >/dev/null 2>&1 || true",
    `rm -rf ${shQuote(mongoDataDir)}/*`,
    "rm -rf /root/.ssh",
    "rm -f /etc/ssh/ssh_host_*",
    "truncate -s 0 /etc/machine-id",
    "apt-get clean",
    "rm -f /root/.bash_history",
    "find /var/log -type f \\( -name '*.log' -o -name '*.log.*' \\) -delete 2>/dev/null || true"
  ].join(" && ");
}

async function renameLatestVzdumpArchive({ dumpdir, cloneId, finalArchivePath }) {
  const entries = await readdir(dumpdir);
  const prefix = `vzdump-lxc-${cloneId}-`;
  const matches = entries
    .filter((entry) => entry.startsWith(prefix) && entry.endsWith(".tar.zst"))
    .sort();
  if (matches.length === 0) {
    throw new Error(`No vzdump archive found for CT ${cloneId} in ${dumpdir} (expected a file starting with "${prefix}").`);
  }
  const latest = matches[matches.length - 1];
  await rename(path.join(dumpdir, latest), finalArchivePath);
}

function ctTemplateUsage() {
  return `Usage:
  eco ct template <source> --name <name> --clone-id <ctid> [options]`;
}

async function runTemplate(args) {
  const { options, positionals } = parseOptions(args);
  const source = positionals[0];
  if (!source) {
    throw new Error(`Missing source CT.\n\n${ctTemplateUsage()}`);
  }

  const name = required(options, "name");
  const cloneId = required(options, "clone-id");
  const version = options.version || new Date().toISOString().slice(0, 10).replace(/-/g, "");
  const workspaceRoot = options["workspace-root"] || "/opt/projects";
  const mongoDataDir = options["mongo-data-dir"] || "/var/lib/mongodb";
  const dumpdir = options.dumpdir || "/var/lib/vz/template/cache";
  const keepClone = Boolean(options["keep-clone"]);
  const storage = options.storage;

  const sourceCtid = await resolveCtIdInput(source);
  if (sourceCtid === String(cloneId)) {
    throw new Error("--clone-id must be different from the source CT -- this command destroys the clone when it's done.");
  }

  const finalArchiveName = `${name}_${version}_amd64.tar.zst`;
  const finalArchivePath = path.posix.join(dumpdir, finalArchiveName);

  // `pct clone --full` refuses to run directly against a running
  // container's live rootfs ("Full clone of a running container is only
  // possible from a snapshot"). Rather than require the source to be
  // stopped -- real downtime for whatever estate it's serving -- snapshot
  // it first when it's running and clone from that snapshot instead. A
  // stopped source clones directly as before, no snapshot needed.
  const sourceStatus = await runCapture("pct", ["status", sourceCtid]);
  const sourceIsRunning = sourceStatus.code === 0 && /status:\s+running/.test(sourceStatus.stdout);
  const snapshotName = sourceIsRunning ? `eco-template-${Date.now()}` : null;

  const cloneArgs = ["clone", sourceCtid, String(cloneId), "--hostname", `${name}-template-build`, "--full", "1"];
  if (snapshotName) {
    cloneArgs.push("--snapname", snapshotName);
  }
  if (storage) {
    cloneArgs.push("--storage", storage);
  }

  const cleanupScript = buildCleanupScript({ workspaceRoot, mongoDataDir });

  const steps = [];
  if (snapshotName) {
    steps.push({
      description: `Snapshot running source CT ${sourceCtid} (required for a full clone of a live container)`,
      command: "pct",
      args: ["snapshot", sourceCtid, snapshotName]
    });
  }
  steps.push(
    { description: `Clone CT ${sourceCtid} -> ${cloneId} (full clone, source untouched)`, command: "pct", args: cloneArgs },
    { description: `Start clone CT ${cloneId}`, command: "pct", args: ["start", String(cloneId)] },
    { description: `Wait for CT ${cloneId} to be exec-ready`, wait: true },
    { description: `Clean project-specific state inside CT ${cloneId}`, command: "pct", args: ["exec", String(cloneId), "--", "bash", "-lc", cleanupScript] },
    { description: `Stop clone CT ${cloneId}`, command: "pct", args: ["stop", String(cloneId)] },
    { description: `Export CT ${cloneId} as a template archive`, command: "vzdump", args: [String(cloneId), "--mode", "stop", "--compress", "zstd", "--dumpdir", dumpdir] },
    { description: `Rename exported archive to ${finalArchiveName}`, rename: true }
  );

  if (snapshotName) {
    steps.push({
      description: `Remove temporary snapshot ${snapshotName} from source CT ${sourceCtid}`,
      command: "pct",
      args: ["delsnapshot", sourceCtid, snapshotName]
    });
  }

  if (!keepClone) {
    steps.push({ description: `Destroy temporary clone CT ${cloneId}`, command: "pct", args: ["destroy", String(cloneId)] });
  }

  if (options["dry-run"]) {
    process.stdout.write("eco ct template plan\n");
    process.stdout.write(`Source CT: ${sourceCtid}\n`);
    process.stdout.write(`Clone CT:  ${cloneId}${keepClone ? " (kept after export)" : " (destroyed after export)"}\n`);
    process.stdout.write(`Template:  ${finalArchivePath}\n\n`);
    for (const step of steps) {
      if (step.command) {
        const renderedArgs = step.args.map((arg) => (/\s/.test(arg) ? shQuote(arg) : arg)).join(" ");
        process.stdout.write(`${step.description}\n  ${step.command} ${renderedArgs}\n`);
      } else {
        process.stdout.write(`${step.description}\n`);
      }
    }
    return;
  }

  for (const step of steps) {
    process.stdout.write(`==> ${step.description}\n`);
    if (step.wait) {
      await waitForCtExec(cloneId);
      continue;
    }
    if (step.rename) {
      await renameLatestVzdumpArchive({ dumpdir, cloneId, finalArchivePath });
      continue;
    }
    await runCommand(step.command, step.args);
  }

  process.stdout.write(`\nTemplate ready: ${finalArchivePath}\n`);
  process.stdout.write(`Use it in an ecompose.yml with: ct.template: local:vztmpl/${finalArchiveName}\n`);
  process.stdout.write(`(adjust the "local:" storage prefix if --dumpdir targets a different storage.)\n`);
}

export async function runCt(args) {
  const [subcommand, ...rest] = args;

  if (!subcommand || subcommand === "help" || subcommand === "--help" || subcommand === "-h") {
    ctHelp();
    return;
  }

  switch (subcommand) {
    case "create":
      await runCreate(rest);
      return;
    case "start":
    case "stop":
    case "reboot":
    case "status":
      await runSimplePct(subcommand, rest);
      return;
    case "template":
      await runTemplate(rest);
      return;
    default:
      throw new Error(`Unknown CT subcommand: ${subcommand}\n\nRun "eco ct help" for usage.`);
  }
}
