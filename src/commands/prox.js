import { spawn } from "node:child_process";
import { mkdir, mkdtemp, readFile, readdir, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createInterface } from "node:readline/promises";
import { stdin as input, stdout as output } from "node:process";

import { parseStorage, resolveEcomposeFile } from "../lib/ecompose.js";
import { removeDnsRecordForTunnel, removeRemoteTunnel, removeRemoteTunnelHostname, runProxy } from "./proxy.js";

const PACKAGE_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const DEFAULT_TEMPLATE = "newest installed Debian 12/13 template";

function help() {
  process.stdout.write(`eco prox

Usage:
  eco prox prepare rust-builder <ctid-or-hostname> [--dry-run]
  eco prox createct rust-builder <name> [options]
  eco prox clear-rust <builder-ctid-or-name> [--yes] [--dry-run]
  eco prox remove-tunnel [domain|*.domain] [--target <ctid-or-hostname>] [--account <name>] [--dry-run]
  eco prox clearenv [--dry-run]
  eco prox showports
  eco prox rename-pct <ctid> <new-hostname>
  eco prox shrink-pct <ctid> <target-gb> [temp-ctid]
  eco prox size-pct
  eco prox set-ct <ctid> --cores <n> --memory <mb> [--swap <mb>] [--dry-run]
  eco prox archive <vm-or-ct> --output <external-directory> [--format qcow2]
  eco prox unarchive <archive-directory-or-vzdump-archive> --id <new-id> [--storage <storage>]

Rust builder preparation installs Eco's shared Rust toolchain and sccache in
an existing CT. That CT may also run applications; use its name or ID in
ECO_RUST_DEDICATED_BUILDER when running production eco up.

VM archive default: compressed QCOW2 images written directly to external storage.
Eco stops a running VM temporarily for a consistent archive, then starts it again.
CT archives remain native vzdump archives. Use --format vzdump when a native VM
snapshot/suspend backup is explicitly required.

Examples:
  eco prox prepare rust-builder deveko
  eco prox createct rust-builder rust-builder --id 1000
  eco prox clear-rust deveko
  eco prox remove-tunnel
  eco prox remove-tunnel app.example.com --target proxy
  eco prox remove-tunnel '*.example.com' --target proxy
  eco prox remove-tunnel app.example.com --account customer-a
  eco prox clearenv
  eco prox clearenv --dry-run
  eco prox showports
  eco prox rename-pct 100 proxy-edge
  eco prox shrink-pct 101 8
  eco prox shrink-pct 101 8 900
  eco prox size-pct
  eco prox set-ct 101 --cores 10 --memory 6144 --swap 2048
  eco prox set-ct 101 --memory 6144 --dry-run
  eco prox archive Win11 --output /mnt/usb/VM
  eco prox unarchive /mnt/usb/VM/eco-qemu-999-... --id 220 --storage local-lvm

After QCOW2 restore, inspect before starting:
  qm config 220
  qm start 220
`);
  return;
  process.stdout.write(`eco prox\n\nUsage:\n  eco prox createct minio <name> [options]\n  eco prox attach minio <name-or-id> --project <bootstrap-dir> [--yes]\n  eco prox archive <vm-or-ct> --output <external-directory> [--mode snapshot]\n  eco prox unarchive <vzdump-archive> --id <new-id> [--storage <storage>]\n\nManaged MinIO remains private Proxmox infrastructure. Archive/unarchive uses\nProxmox's native vzdump format: Windows VMs become .vma.zst (not .tar.gz),\nincluding their configuration and virtual disks. Archives must be written to\nan external filesystem so a full Proxmox root volume is never made worse.\n\nCreate options:\n  --id <ctid>        CT ID (default: Proxmox next available ID)\n  --hostname <name>  CT hostname (default: the given name)\n  --template <ref>   LXC template (default: ${DEFAULT_TEMPLATE})\n  --storage <name>   Rootfs storage (default: local-lvm)\n  --disk <gb>        Disk size in GB (default: 30)\n  --cores <count>    CPU cores (default: 2)\n  --memory <mb>      Memory in MB (default: 1024)\n  --swap <mb>        Swap in MB (default: 512)\n  --bridge <name>    Private bridge (default: vmbr0)\n  --ip <value>       IP CIDR or dhcp (default: dhcp)\n  --gateway <ip>     Gateway required only for static IP\n  --yes-reinstall    Reset an existing named MinIO CT without prompting\n  --keep-on-failure  Preserve a newly-created CT for diagnostics\n  --dry-run          Print the full plan without changing Proxmox\n\nAttach options:\n  --project <path>   Estate bootstrap directory or ecompose.yml (required)\n  --region <name>    S3 region (default: existing value or us-east-1)\n  --yes              Apply without confirmation\n\nArchive options:\n  --output <dir>     Existing external/mounted directory (required)\n  --mode <mode>      vzdump mode: snapshot, suspend, or stop (default: snapshot)\n  --allow-local      Override the external-filesystem safety check\n\nUnarchive options:\n  --id <id>          New VM/CT ID (required; existing IDs are refused)\n  --storage <name>   Target storage for restored disks/rootfs\n\nExamples:\n  eco prox createct minio storage --id 102\n  eco prox attach minio storage --project /opt/projects/stuff8/stuff8_bootstrap\n  eco prox archive windows11 --output /mnt/usb/proxmox-archives\n  eco prox unarchive /mnt/usb/proxmox-archives/vzdump-qemu-120-*.vma.zst --id 220 --storage local-lvm\n`);
}

function parse(args) {
  const options = {};
  const positionals = [];
  for (let i = 0; i < args.length; i += 1) {
    const arg = args[i];
    if (!arg.startsWith("--")) { positionals.push(arg); continue; }
    const key = arg.slice(2);
    if (key === "dry-run" || key === "help" || key === "yes-reinstall" || key === "keep-on-failure" || key === "yes" || key === "allow-local") { options[key] = true; continue; }
    const value = args[i + 1];
    if (!value || value.startsWith("--")) throw new Error(`Missing value for --${key}`);
    options[key] = value;
    i += 1;
  }
  return { options, positionals };
}

function shellQuote(value) {
  return `'${String(value).replace(/'/g, `'\\''`)}'`;
}

function run(command, args, { capture = false } = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { stdio: capture ? ["ignore", "pipe", "pipe"] : "inherit", env: process.env });
    let stdout = ""; let stderr = "";
    if (capture) { child.stdout.on("data", (chunk) => { stdout += chunk; }); child.stderr.on("data", (chunk) => { stderr += chunk; }); }
    child.on("error", reject);
    child.on("close", (code) => {
      if (code === 0) { resolve({ stdout, stderr }); return; }
      const details = [stderr, stdout].map((value) => value.trim()).filter(Boolean).join("\n");
      reject(new Error(`${command} exited with code ${code}${details ? `:\n${details}` : ""}`));
    });
  });
}

async function nextId() { return (await run("pvesh", ["get", "/cluster/nextid"], { capture: true })).stdout.trim(); }

async function resolveInstalledTemplate(requested) {
  const listing = (await run("pveam", ["list", "local"], { capture: true })).stdout;
  const installed = listing.match(/\S+:vztmpl\/\S+\.tar\.(?:zst|gz|xz)/g) || [];
  if (requested) {
    if (!installed.includes(requested)) {
      throw new Error(`Requested template "${requested}" is not installed. Available templates:\n${installed.join("\n") || "(none)"}`);
    }
    return requested;
  }
  const candidates = installed
    .filter((template) => /debian-(?:12|13)-standard_.*_amd64\.tar\.(?:zst|gz|xz)$/.test(template))
    .sort((left, right) => right.localeCompare(left));
  if (candidates.length === 0) {
    throw new Error("No Debian 12/13 LXC template is installed on local storage. Run `pveam update`, then download one with `pveam download local <template-name>`, and retry.");
  }
  return candidates[0];
}

function pctCreateArgs(id, options, template, hostname) {
  const network = [`name=eth0`, `bridge=${options.bridge || "vmbr0"}`, `ip=${options.ip || "dhcp"}`];
  if (options.gateway) network.push(`gw=${options.gateway}`);
  return ["create", id, template, "--hostname", hostname, "--cores", options.cores || "2", "--memory", options.memory || "1024", "--swap", options.swap || "512", "--rootfs", `${options.storage || "local-lvm"}:${options.disk || "30"}`, "--net0", network.join(","), "--features", "nesting=1", "--unprivileged", "1"];
}

async function installMinio(ctid, { reset = false } = {}) {
  const installer = await readFile(path.join(PACKAGE_ROOT, "install-minio.sh"), "utf8");
  const directory = await mkdtemp(path.join(tmpdir(), "eco-minio-installer-"));
  const source = path.join(directory, "install-minio.sh");
  try {
    process.stdout.write(`[CT ${ctid}] Uploading managed MinIO installer...\n`);
    await writeFile(source, installer, { mode: 0o700 });
    await run("pct", ["push", ctid, source, "/tmp/eco-install-minio.sh"]);
    const resetFlag = reset ? " --reset" : "";
    process.stdout.write(`[CT ${ctid}] Installing and starting MinIO...\n`);
    await run("pct", ["exec", ctid, "--", "bash", "-lc", `chmod 700 /tmp/eco-install-minio.sh && ECO_DEPLOY_MODE=prod bash /tmp/eco-install-minio.sh --ensure${resetFlag} && rm -f /tmp/eco-install-minio.sh`]);
    process.stdout.write(`[CT ${ctid}] Checking MinIO health...\n`);
    try {
      await run("pct", ["exec", ctid, "--", "bash", "-lc", "curl -fsS http://127.0.0.1:9000/minio/health/live >/dev/null"]);
    } catch (error) {
      let diagnostics = "";
      try {
        const result = await run("pct", ["exec", ctid, "--", "bash", "-lc", "systemctl status eco-minio --no-pager -l; journalctl -u eco-minio --no-pager -n 80"], { capture: true });
        diagnostics = [result.stdout, result.stderr].filter(Boolean).join("\n");
      } catch (diagnosticError) {
        diagnostics = diagnosticError.message;
      }
      throw new Error(`${error.message}\n\nMinIO service diagnostics from CT ${ctid}:\n${diagnostics}`);
    }
  } finally { await rm(directory, { recursive: true, force: true }); }
}

async function installRustBuilder(ctid) {
  process.stdout.write(`[CT ${ctid}] Installing Rust build toolchain and shared compiler cache...\n`);
  const command = [
    "export DEBIAN_FRONTEND=noninteractive",
    "apt-get update",
    "apt-get install -y curl ca-certificates build-essential pkg-config libssl-dev",
    "mkdir -p /usr/local/rustup /usr/local/cargo /opt/eco-rust-builds",
    "RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo bash -c 'curl --proto =https --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --no-modify-path'",
    "RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo /usr/local/cargo/bin/rustup toolchain install stable --profile minimal",
    "RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo /usr/local/cargo/bin/rustup default stable",
    // Production PM2 configs always use this exact wrapper path.  Do not
    // merely accept a distro-provided sccache elsewhere on PATH: a prepared
    // builder must satisfy the runtime contract directly and idempotently.
    "if [ ! -x /usr/local/bin/sccache ]; then arch=$(dpkg --print-architecture); [ \"$arch\" = amd64 ] || { echo \"Eco rust-builder requires an amd64 sccache binary (found: $arch)\" >&2; exit 1; }; version=v0.16.0; tmpdir=$(mktemp -d); trap 'rm -rf \"$tmpdir\"' EXIT; curl --proto '=https' --tlsv1.2 -sSfL \"https://github.com/mozilla/sccache/releases/download/$version/sccache-$version-x86_64-unknown-linux-musl.tar.gz\" -o \"$tmpdir/sccache.tar.gz\"; tar xzf \"$tmpdir/sccache.tar.gz\" -C \"$tmpdir\"; install -m 0755 \"$tmpdir/sccache-$version-x86_64-unknown-linux-musl/sccache\" /usr/local/bin/sccache; rm -rf \"$tmpdir\"; trap - EXIT; fi",
    "install -d -m 0777 /usr/local/sccache-cache",
    "rm -f /usr/local/bin/cargo /usr/local/bin/rustc /usr/local/bin/rustup",
    "printf '%s\\n' '#!/bin/sh' 'export RUSTUP_HOME=/usr/local/rustup' 'export CARGO_HOME=/usr/local/cargo' 'exec /usr/local/cargo/bin/cargo \"$@\"' > /usr/local/bin/cargo",
    "printf '%s\\n' '#!/bin/sh' 'export RUSTUP_HOME=/usr/local/rustup' 'export CARGO_HOME=/usr/local/cargo' 'exec /usr/local/cargo/bin/rustc \"$@\"' > /usr/local/bin/rustc",
    "printf '%s\\n' '#!/bin/sh' 'export RUSTUP_HOME=/usr/local/rustup' 'export CARGO_HOME=/usr/local/cargo' 'exec /usr/local/cargo/bin/rustup \"$@\"' > /usr/local/bin/rustup",
    "chmod 755 /usr/local/bin/cargo /usr/local/bin/rustc /usr/local/bin/rustup",
    "printf '%s\\n' 'export RUSTUP_HOME=/usr/local/rustup' 'export CARGO_HOME=/usr/local/cargo' 'export PATH=/usr/local/cargo/bin:$PATH' 'export RUSTC_WRAPPER=/usr/local/bin/sccache' 'export SCCACHE_DIR=/usr/local/sccache-cache' > /etc/profile.d/eco-rust.sh",
    "chmod 644 /etc/profile.d/eco-rust.sh",
    "install -d -m 755 /etc/eco",
    "printf '%s\\n' 'role=rust-builder' 'rustup_home=/usr/local/rustup' 'cargo_home=/usr/local/cargo' 'build_root=/opt/eco-rust-builds' > /etc/eco/rust-builder.env",
    "/usr/local/bin/cargo --version",
    "/usr/local/bin/sccache --version"
  ].join(" && ");
  await run("pct", ["exec", ctid, "--", "bash", "-lc", command]);
}

async function prepareRustBuilder(positionals, options) {
  const reference = positionals[2];
  if (!reference) throw new Error("Usage: eco prox prepare rust-builder <ctid-or-hostname> [--dry-run]");
  const builder = await resolveCtByReference(reference);

  if (options["dry-run"]) {
    process.stdout.write(`eco prox prepare rust-builder plan\n  CT: ${builder.ctid} (${builder.hostname})\n  Ensure running\n  Install/refresh: curl, CA certificates, build-essential, pkg-config, libssl-dev\n  Install/refresh: Rust stable minimal toolchain in /usr/local/rustup and /usr/local/cargo\n  Install/refresh: sccache in /usr/local/bin and its shared cache directory\n  Mark role: /etc/eco/rust-builder.env\n\nUse after success:\n  export ECO_RUST_DEDICATED_BUILDER=${builder.hostname}\n`);
    return;
  }

  await ensureCtRunning(builder.ctid);
  await waitForCtExec(builder.ctid);
  await installRustBuilder(builder.ctid);
  process.stdout.write(`Rust builder CT ${builder.ctid} (${builder.hostname}) is ready.\n\nFor this shell:\n  export ECO_RUST_DEDICATED_BUILDER=${builder.hostname}\n\nPersist it in the Proxmox host environment before running eco up. The builder may also be an application CT; Eco builds in place when it is the destination CT.\n`);
}

async function existingCtHostname(ctid) {
  try {
    const config = await run("pct", ["config", ctid], { capture: true });
    return config.stdout.match(/^hostname:\s*(.+)$/m)?.[1]?.trim() || "";
  } catch {
    return null;
  }
}

async function findCtByHostname(hostname) {
  const listing = await run("pct", ["list"], { capture: true });
  for (const line of listing.stdout.split(/\r?\n/)) {
    const id = line.trim().split(/\s+/)[0];
    if (!/^\d+$/.test(id)) continue;
    if (await existingCtHostname(id) === hostname) return id;
  }
  return null;
}

async function confirmReinstall(ctid, hostname) {
  const prompt = createInterface({ input, output });
  try {
    const answer = (await prompt.question(`CT ${ctid} (${hostname}) already exists. Reinstalling MinIO WILL DELETE all objects and credentials. Type RESET to continue: `)).trim();
    return answer === "RESET";
  } finally { prompt.close(); }
}

async function ensureCtRunning(ctid) {
  const status = await run("pct", ["status", ctid], { capture: true });
  if (!/status:\s+running/.test(status.stdout)) await run("pct", ["start", ctid]);
}

async function waitForCtExec(ctid, { attempts = 30, delayMs = 1000 } = {}) {
  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    try {
      await run("pct", ["exec", ctid, "--", "true"]);
      return;
    } catch {
      if (attempt < attempts) await new Promise((resolve) => setTimeout(resolve, delayMs));
    }
  }
  throw new Error(`CT ${ctid} did not become exec-ready within ${attempts} seconds.`);
}

async function removeNewCt(ctid) {
  await run("pct", ["stop", ctid]).catch(() => undefined);
  await run("pct", ["destroy", ctid, "--purge", "1"]).catch(() => undefined);
}

async function diagnoseCt(ctid) {
  const probes = [
    ["pct", ["status", ctid]],
    ["pct", ["config", ctid]],
    ["pct", ["exec", ctid, "--", "bash", "-lc", "systemctl status eco-minio --no-pager -l || true; journalctl -u eco-minio --no-pager -n 80 || true"]]
  ];
  const report = [];
  for (const [command, args] of probes) {
    try {
      const result = await run(command, args, { capture: true });
      report.push(`$ ${command} ${args.join(" ")}\n${result.stdout}${result.stderr}`);
    } catch (error) {
      report.push(`$ ${command} ${args.join(" ")}\n${error.message}`);
    }
  }
  return report.join("\n");
}

async function resolveCtByReference(reference) {
  if (/^\d+$/.test(reference)) {
    const hostname = await existingCtHostname(reference);
    if (!hostname) throw new Error(`CT ${reference} does not exist.`);
    return { ctid: reference, hostname };
  }
  const ctid = await findCtByHostname(reference);
  if (!ctid) throw new Error(`No CT with hostname "${reference}" exists.`);
  return { ctid, hostname: reference };
}

function managedMinioBlock({ ct, region }) {
  return [
    "# Eco manages MinIO credentials and resolves this CT's private bridge",
    "# address at `eco up`; never commit endpoint or credentials here.",
    "storage:",
    "  minio:",
    `    ct: ${ct}`,
    `    region: ${region}`,
    ""
  ].join("\n");
}

function setMinioStorage(content, { ct, region }) {
  const lines = content.split(/\r?\n/);
  const storageStart = lines.findIndex((line) => /^storage:\s*$/.test(line));
  const block = managedMinioBlock({ ct, region }).split("\n");
  if (storageStart === -1) {
    const suffix = content.endsWith("\n") ? "" : "\n";
    return `${content}${suffix}\n${block.join("\n")}`;
  }

  let storageEnd = lines.length;
  for (let index = storageStart + 1; index < lines.length; index += 1) {
    if (/^[^\s#].*:\s*$/.test(lines[index])) { storageEnd = index; break; }
  }
  const minioStart = lines.findIndex((line, index) => index > storageStart && index < storageEnd && /^  minio:\s*$/.test(line));
  if (minioStart === -1) {
    lines.splice(storageEnd, 0, "  minio:", `    ct: ${ct}`, `    region: ${region}`);
    return lines.join("\n");
  }
  let minioEnd = storageEnd;
  for (let index = minioStart + 1; index < storageEnd; index += 1) {
    if (/^  [A-Za-z0-9_-]+:\s*$/.test(lines[index])) { minioEnd = index; break; }
  }
  lines.splice(minioStart, minioEnd - minioStart, "  minio:", `    ct: ${ct}`, `    region: ${region}`);
  return lines.join("\n");
}

async function confirmAttachment({ ctid, hostname, manifestPath }) {
  const prompt = createInterface({ input, output });
  try {
    const answer = (await prompt.question(`Attach MinIO CT ${ctid} (${hostname}) to ${manifestPath}? [y/N]: `)).trim().toLowerCase();
    return answer === "y" || answer === "yes";
  } finally { prompt.close(); }
}

async function attachMinio(args, options) {
  const reference = args[2];
  if (!reference) throw new Error('Usage: eco prox attach minio <name-or-id> --project <bootstrap-dir>');
  if (!options.project) throw new Error('Missing --project. Give the estate bootstrap directory or ecompose.yml path.');
  const { ctid, hostname } = await resolveCtByReference(reference);
  const manifestPath = await resolveEcomposeFile(options.project, process.cwd());
  const content = await readFile(manifestPath, "utf8");
  const storage = parseStorage(content);
  const region = options.region || storage.minio?.region || "us-east-1";
  const next = setMinioStorage(content, { ct: hostname, region });
  if (next === content) {
    process.stdout.write(`${manifestPath} already uses MinIO CT ${hostname}. Nothing changed.\n`);
    return;
  }
  process.stdout.write(`Will configure storage.minio for ${manifestPath}:\n  ct: ${hostname} (CT ${ctid})\n  region: ${region}\n`);
  if (!options.yes && !(await confirmAttachment({ ctid, hostname, manifestPath }))) {
    process.stdout.write("Estate storage attachment cancelled.\n");
    return;
  }
  await writeFile(manifestPath, next, "utf8");
  process.stdout.write(`Attached MinIO CT ${hostname} to ${manifestPath}. Commit that bootstrap-repository change, then run eco up from the estate.\n`);
}

async function existingVmName(vmid) {
  try {
    const config = await run("qm", ["config", vmid], { capture: true });
    return config.stdout.match(/^name:\s*(.+)$/m)?.[1]?.trim() || `vm-${vmid}`;
  } catch {
    return null;
  }
}

async function findVmByName(name) {
  const listing = await run("qm", ["list"], { capture: true });
  for (const line of listing.stdout.split(/\r?\n/)) {
    const id = line.trim().split(/\s+/)[0];
    if (!/^\d+$/.test(id)) continue;
    if (await existingVmName(id) === name) return id;
  }
  return null;
}

async function resolveArchiveWorkload(reference) {
  if (/^\d+$/.test(reference)) {
    const vmName = await existingVmName(reference);
    if (vmName) return { kind: "qemu", id: String(reference), name: vmName };
    const ctName = await existingCtHostname(reference);
    if (ctName) return { kind: "lxc", id: String(reference), name: ctName };
    throw new Error(`No VM or CT with ID ${reference} exists.`);
  }
  const [vmid, ctid] = await Promise.all([findVmByName(reference), findCtByHostname(reference)]);
  if (vmid && ctid) throw new Error(`"${reference}" matches both VM ${vmid} and CT ${ctid}; use the numeric ID.`);
  if (vmid) return { kind: "qemu", id: vmid, name: reference };
  if (ctid) return { kind: "lxc", id: ctid, name: reference };
  throw new Error(`No VM or CT named "${reference}" exists.`);
}

async function filesystemSource(directory) {
  const result = await run("df", ["-P", directory], { capture: true });
  const line = result.stdout.trim().split(/\r?\n/).at(-1) || "";
  const source = line.trim().split(/\s+/)[0];
  if (!source) throw new Error(`Could not determine filesystem for ${directory}.`);
  return source;
}

async function latestArchive(directory, prefix, startedAt) {
  const entries = await readdir(directory);
  const candidates = [];
  for (const entry of entries) {
    if (!entry.startsWith(prefix)) continue;
    const file = path.join(directory, entry);
    const info = await stat(file);
    if (info.isFile() && info.mtimeMs >= startedAt - 2000) candidates.push({ file, mtimeMs: info.mtimeMs, size: info.size });
  }
  candidates.sort((left, right) => right.mtimeMs - left.mtimeMs);
  return candidates[0] || null;
}

function timestampForFile() {
  return new Date().toISOString().replace(/[:.]/g, "-");
}

function parseQmConfig(config) {
  const lines = config.split(/\r?\n/);
  const name = lines.find((line) => line.startsWith("name: "))?.slice(6).trim() || "archived-vm";
  const disks = [];
  for (const line of lines) {
    const match = line.match(/^(scsi\d+|sata\d+|ide\d+|virtio\d+|efidisk0|tpmstate0):\s*([^,\s]+)(.*)$/);
    if (!match || /^(none|cdrom)$/i.test(match[2])) continue;
    disks.push({ slot: match[1], volume: match[2], options: match[3] || "" });
  }
  return { name, disks };
}

function configValue(config, key) {
  return config.match(new RegExp(`^${key}:\\s*(.+)$`, "m"))?.[1]?.trim();
}

async function waitForVmState(vmid, state, { attempts = 90, delayMs = 2000 } = {}) {
  for (let attempt = 0; attempt < attempts; attempt += 1) {
    const status = (await run("qm", ["status", vmid], { capture: true })).stdout;
    if (new RegExp(`status:\\s+${state}`).test(status)) return;
    await new Promise((resolve) => setTimeout(resolve, delayMs));
  }
  throw new Error(`VM ${vmid} did not become ${state} in time.`);
}

async function archiveQcow2Vm(workload, outputDirectory, options) {
  const mode = options.mode || "stop";
  if (mode !== "stop") throw new Error('QCOW2 VM archives require --mode stop for a consistent disk image. Use --format vzdump for snapshot or suspend mode.');
  const config = (await run("qm", ["config", workload.id], { capture: true })).stdout;
  const parsed = parseQmConfig(config);
  if (parsed.disks.length === 0) throw new Error(`VM ${workload.id} has no archiveable disks.`);
  const wasRunning = /status:\s+running/.test((await run("qm", ["status", workload.id], { capture: true })).stdout);
  const archiveDirectory = path.join(outputDirectory, `eco-qemu-${workload.id}-${timestampForFile()}`);
  await mkdir(archiveDirectory);
  try {
    if (wasRunning) {
      process.stdout.write(`Stopping VM ${workload.id} for a consistent QCOW2 archive...\n`);
      await run("qm", ["shutdown", workload.id, "--timeout", "180"]);
      await waitForVmState(workload.id, "stopped");
    }
    const disks = [];
    for (const disk of parsed.disks) {
      const source = (await run("pvesm", ["path", disk.volume], { capture: true })).stdout.trim();
      if (!source) throw new Error(`Could not resolve storage path for ${disk.volume}.`);
      const filename = `${disk.slot}.qcow2`;
      process.stdout.write(`Compressing ${disk.slot} directly to external storage...\n`);
      await run("qemu-img", ["convert", "-p", "-O", "qcow2", "-c", source, path.join(archiveDirectory, filename)]);
      disks.push({ ...disk, filename });
    }
    await writeFile(path.join(archiveDirectory, "vm.conf"), config, "utf8");
    await writeFile(path.join(archiveDirectory, "eco-archive.json"), `${JSON.stringify({ version: 2, format: "qcow2", createdAt: new Date().toISOString(), kind: "qemu", sourceId: workload.id, sourceName: workload.name, configFile: "vm.conf", disks }, null, 2)}\n`, "utf8");
    process.stdout.write(`Archive complete:\n  ${archiveDirectory}\n\nRestore with:\n  eco prox unarchive ${archiveDirectory} --id <new-id> --storage local-lvm\n`);
  } catch (error) {
    process.stderr.write(`QCOW2 archive did not complete. Partial files are in ${archiveDirectory}; remove that directory from the external drive before retrying.\n`);
    throw error;
  } finally {
    if (wasRunning) {
      process.stdout.write(`Restarting VM ${workload.id}...\n`);
      await run("qm", ["start", workload.id]).catch((error) => process.stderr.write(`Could not restart VM ${workload.id}: ${error.message}\n`));
    }
  }
}

async function archiveWorkload(args, options) {
  const reference = args[1];
  if (!reference && !options.output) {
    process.stdout.write(`eco prox archive\n\nArchive VM/CT Proxmox ke media eksternal. Jangan gunakan filesystem root Proxmox\nkarena arsip dapat memperparah disk yang sudah penuh.\n\n1. Temukan partisi drive USB/NFS yang akan dipakai:\n\n  lsblk -f\n\n2. Mount partisi eksternal (ganti /dev/sdX1 dengan partisi yang benar dari lsblk):\n\n  mkdir -p /mnt/usb\n  mount /dev/sdX1 /mnt/usb\n\n   Untuk NFS, gunakan mount NFS Anda sendiri, misalnya:\n\n  mount -t nfs <server>:/<share> /mnt/usb\n\n3. Siapkan tujuan dan pastikan filesystem-nya berbeda dari /:\n\n  mkdir -p /mnt/usb/proxmox-archives\n  df -h / /mnt/usb/proxmox-archives\n\n4. Arsipkan menggunakan nama VM/CT atau ID-nya:\n\n  eco prox archive windows11 --output /mnt/usb/proxmox-archives\n\nVM default menggunakan QCOW2 terkompresi langsung ke drive eksternal. Eco menghentikan\nVM sementara agar image konsisten, lalu menyalakannya kembali jika sebelumnya berjalan.\nHasilnya adalah direktori eco-qemu-... berisi disk QCOW2, vm.conf, dan eco-archive.json.\n\nRestore ke VM ID baru dengan:\n\n  eco prox unarchive /mnt/usb/proxmox-archives/eco-qemu-... --id <new-id> --storage local-lvm\n\nSetelah restore, lakukan review manual sebelum start:\n\n  qm config <new-id>\n  qm start <new-id>\n\nUntuk backup native snapshot gunakan --format vzdump. CT selalu memakai vzdump.\n`);
    return;
  }
  if (!reference || !options.output) throw new Error('Usage: eco prox archive <vm-or-ct> --output <external-directory>');
  const outputDirectory = path.resolve(options.output);
  let directoryInfo;
  try { directoryInfo = await stat(outputDirectory); } catch { throw new Error(`Archive output directory does not exist: ${outputDirectory}`); }
  if (!directoryInfo.isDirectory()) throw new Error(`Archive output must be a directory: ${outputDirectory}`);
  const [rootSource, outputSource] = await Promise.all([filesystemSource("/"), filesystemSource(outputDirectory)]);
  if (!options["allow-local"] && rootSource === outputSource) {
    throw new Error(`Refusing to archive onto ${outputDirectory}: it is on the same filesystem as Proxmox root (${rootSource}). Mount external storage and retry, or explicitly pass --allow-local.`);
  }
  const workload = await resolveArchiveWorkload(reference);
  const format = options.format || (workload.kind === "qemu" ? "qcow2" : "vzdump");
  if (!["qcow2", "vzdump"].includes(format)) throw new Error("--format must be qcow2 or vzdump.");
  if (workload.kind === "lxc" && format !== "vzdump") throw new Error("CT archives support only --format vzdump.");
  const mode = options.mode || (format === "qcow2" ? "stop" : "snapshot");
  if (!["snapshot", "suspend", "stop"].includes(mode)) throw new Error("--mode must be snapshot, suspend, or stop.");
  const prefix = `vzdump-${workload.kind}-${workload.id}-`;
  if (options["dry-run"]) {
    const command = format === "qcow2" ? "qemu-img convert -O qcow2 -c <source-disk> <external-output>" : `vzdump ${workload.id} --dumpdir ${outputDirectory} --mode ${mode} --compress zstd`;
    process.stdout.write(`Archive plan\n  Workload: ${workload.kind === "qemu" ? "VM" : "CT"} ${workload.id} (${workload.name})\n  Format:   ${format}\n  Output:   ${outputDirectory}\n  Mode:     ${mode}\n  Command:  ${command}\n`);
    return;
  }
  if (format === "qcow2") {
    await archiveQcow2Vm(workload, outputDirectory, options);
    return;
  }
  process.stdout.write(`Archiving ${workload.kind === "qemu" ? "VM" : "CT"} ${workload.id} (${workload.name}) to ${outputDirectory}...\n`);
  const startedAt = Date.now();
  await run("vzdump", [workload.id, "--dumpdir", outputDirectory, "--mode", mode, "--compress", "zstd"]);
  const archive = await latestArchive(outputDirectory, prefix, startedAt);
  if (!archive) throw new Error(`vzdump succeeded but no ${prefix} archive was found in ${outputDirectory}.`);
  const manifestPath = `${archive.file}.eco.json`;
  await writeFile(manifestPath, `${JSON.stringify({ version: 1, createdAt: new Date().toISOString(), kind: workload.kind, sourceId: workload.id, sourceName: workload.name, archive: path.basename(archive.file) }, null, 2)}\n`, "utf8");
  process.stdout.write(`Archive complete:\n  ${archive.file}\n  ${manifestPath}\n\nCopy both files offsite before deleting the original. Restore with:\n  eco prox unarchive ${archive.file} --id <new-id>${options.storage ? ` --storage ${options.storage}` : ""}\n`);
}

async function unarchiveWorkload(args, options) {
  const archive = args[1];
  if (!archive || !options.id) throw new Error('Usage: eco prox unarchive <archive-directory-or-vzdump-archive> --id <new-id> [--storage <storage>]');
  const archivePath = path.resolve(archive);
  let archiveInfo;
  try { archiveInfo = await stat(archivePath); } catch { throw new Error(`Archive does not exist: ${archivePath}`); }
  const targetId = String(options.id);
  if (!/^\d+$/.test(targetId) || Number(targetId) <= 0) throw new Error("--id must be a positive numeric VM/CT ID.");
  if (archiveInfo.isDirectory()) {
    await unarchiveQcow2Vm(archivePath, targetId, options);
    return;
  }
  if (!archiveInfo.isFile()) throw new Error(`Archive is not a regular file or QCOW2 archive directory: ${archivePath}`);
  const filename = path.basename(archivePath);
  const kind = /^vzdump-qemu-\d+-.+\.vma(?:\.(?:zst|gz|lzo))?$/.test(filename) ? "qemu" : /^vzdump-lxc-\d+-.+\.tar(?:\.(?:zst|gz|lzo))?$/.test(filename) ? "lxc" : null;
  if (!kind) throw new Error("Unsupported archive. Use a native Proxmox vzdump-qemu *.vma.zst or vzdump-lxc *.tar.zst file.");
  const exists = kind === "qemu" ? await existingVmName(targetId) : await existingCtHostname(targetId);
  if (exists) throw new Error(`Target ${kind === "qemu" ? "VM" : "CT"} ID ${targetId} already exists. Refusing to overwrite it.`);
  const command = kind === "qemu" ? "qmrestore" : "pct";
  const commandArgs = kind === "qemu" ? [archivePath, targetId, "--unique", "1"] : ["restore", targetId, archivePath];
  if (options.storage) commandArgs.push("--storage", options.storage);
  if (options["dry-run"]) { process.stdout.write(`Restore plan\n  ${command} ${commandArgs.join(" ")}\n`); return; }
  process.stdout.write(`Restoring ${kind === "qemu" ? "VM" : "CT"} ${targetId} from ${archivePath}...\n`);
  await run(command, commandArgs);
  process.stdout.write(`Restore complete. ${kind === "qemu" ? "VM" : "CT"} ${targetId} remains stopped; inspect its configuration before starting it.\n`);
}

function withoutSize(options = "") {
  return options.split(",").filter((entry) => entry && !entry.startsWith("size=")).join(",");
}

async function newestUnusedDisk(vmid) {
  const config = (await run("qm", ["config", vmid], { capture: true })).stdout;
  const line = (config.match(/^unused\d+:\s*(.+)$/gm) || []).at(-1);
  return line?.replace(/^unused\d+:\s*/, "").trim();
}

async function unarchiveQcow2Vm(archivePath, targetId, options) {
  const manifestPath = path.join(archivePath, "eco-archive.json");
  let manifest;
  try { manifest = JSON.parse(await readFile(manifestPath, "utf8")); } catch { throw new Error(`QCOW2 archive is missing a readable ${manifestPath}.`); }
  if (manifest.format !== "qcow2" || manifest.kind !== "qemu" || !Array.isArray(manifest.disks)) throw new Error("Unsupported QCOW2 archive manifest.");
  if (await existingVmName(targetId)) throw new Error(`Target VM ID ${targetId} already exists. Refusing to overwrite it.`);
  const storage = options.storage || "local-lvm";
  const config = await readFile(path.join(archivePath, manifest.configFile || "vm.conf"), "utf8");
  const createArgs = ["create", targetId, "--name", manifest.sourceName || `restored-${targetId}`];
  for (const key of ["memory", "cores", "sockets", "cpu", "machine", "bios", "ostype", "scsihw", "agent", "vga", "tablet", "numa", "balloon", "net0", "net1", "net2", "net3"]) {
    const value = configValue(config, key);
    if (value) createArgs.push(`--${key}`, value);
  }
  if (options["dry-run"]) {
    process.stdout.write(`Restore plan\n  qm ${createArgs.join(" ")}\n  Import ${manifest.disks.length} QCOW2 disk image(s) into ${storage}, then attach them to their original slots.\n\nManual follow-up after restore:\n  1. Verify firmware, boot order, TPM, and network: qm config ${targetId}\n  2. Start only after review: qm start ${targetId}\n`);
    return;
  }
  process.stdout.write(`Creating stopped VM ${targetId}...\n`);
  await run("qm", createArgs);
  try {
    for (const disk of manifest.disks) {
      const image = path.join(archivePath, disk.filename);
      if (!(await stat(image)).isFile()) throw new Error(`Missing disk image ${image}.`);
      process.stdout.write(`Importing ${disk.filename} into ${storage}...\n`);
      await run("qm", ["importdisk", targetId, image, storage]);
      const imported = await newestUnusedDisk(targetId);
      if (!imported) throw new Error(`Could not identify imported disk for ${disk.filename}.`);
      const suffix = withoutSize(disk.options);
      await run("qm", ["set", targetId, `--${disk.slot}`, `${imported}${suffix ? `,${suffix}` : ""}`]);
    }
    const boot = configValue(config, "boot");
    if (boot) await run("qm", ["set", targetId, "--boot", boot]);
  } catch (error) {
    process.stderr.write(`Restore did not complete. VM ${targetId} was intentionally kept for diagnosis; inspect qm config ${targetId} before removing it.\n`);
    throw error;
  }
  process.stdout.write(`Restore complete. VM ${targetId} remains stopped.\n\nManual follow-up:\n  1. Inspect disk slots, boot order, UEFI/OVMF, TPM, and network: qm config ${targetId}\n  2. If Windows asks for recovery, confirm the restored TPM state and boot disk first.\n  3. Start after review: qm start ${targetId}\n`);
}

function parsePctList(outputText) {
  return outputText.split(/\r?\n/).map((line) => {
    const match = line.trim().match(/^(\d+)\s+(\S+)\s+(.+)$/);
    return match ? { id: match[1], status: match[2], hostname: match[3].trim() } : null;
  }).filter(Boolean);
}

const RUST_CLEANUP_REPORT = [
  "set -euo pipefail",
  "paths=(/usr/local/rustup /usr/local/cargo /usr/local/sccache-cache)",
  "before_managed=0",
  "for path in \"${paths[@]}\"; do [ -e \"$path\" ] || continue; size=$(du -sk \"$path\" 2>/dev/null | awk '{print $1}'); before_managed=$((before_managed + ${size:-0})); done",
  "before_root=$(df -Pk / | awk 'NR == 2 {print $3}')",
  "rm -rf /usr/local/rustup /usr/local/cargo /usr/local/sccache-cache",
  "rm -f /usr/local/bin/cargo /usr/local/bin/rustc /usr/local/bin/rustup /usr/local/bin/sccache /etc/profile.d/cargo.sh /etc/profile.d/eco-rust.sh /etc/eco/rust-builder.env",
  "after_managed=0",
  "for path in \"${paths[@]}\"; do [ -e \"$path\" ] || continue; size=$(du -sk \"$path\" 2>/dev/null | awk '{print $1}'); after_managed=$((after_managed + ${size:-0})); done",
  "after_root=$(df -Pk / | awk 'NR == 2 {print $3}')",
  "printf 'ECO_RUST_CLEANUP before_managed_kb=%s after_managed_kb=%s before_root_kb=%s after_root_kb=%s\\n' \"$before_managed\" \"$after_managed\" \"$before_root\" \"$after_root\""
].join("\n");

function humanKiB(kb) {
  const value = Number(kb || 0);
  if (value >= 1024 * 1024) return `${(value / (1024 * 1024)).toFixed(2)} GiB`;
  if (value >= 1024) return `${(value / 1024).toFixed(1)} MiB`;
  return `${value} KiB`;
}

function cleanupMetrics(text) {
  const match = text.match(/ECO_RUST_CLEANUP before_managed_kb=(\d+) after_managed_kb=(\d+) before_root_kb=(\d+) after_root_kb=(\d+)/);
  if (!match) throw new Error(`Rust cleanup completed but did not return a size report.\n${text}`);
  return Object.fromEntries(["beforeManaged", "afterManaged", "beforeRoot", "afterRoot"].map((key, index) => [key, Number(match[index + 1])]));
}

async function confirmRustCleanup(builder, targets) {
  const prompt = createInterface({ input, output });
  try {
    const answer = (await prompt.question(`Remove Eco-managed Rust toolchains and caches from CT ${targets.map((target) => `${target.id} (${target.hostname})`).join(", ")}, while preserving builder CT ${builder.ctid} (${builder.hostname})? Type CLEAR-RUST to continue: `)).trim();
    return answer === "CLEAR-RUST";
  } finally { prompt.close(); }
}

async function clearRust(args, options) {
  const builderReference = args[1] || process.env.ECO_RUST_DEDICATED_BUILDER;
  if (!builderReference) throw new Error("Usage: eco prox clear-rust <builder-ctid-or-name> [--yes] [--dry-run]");
  const builder = await resolveCtByReference(String(builderReference));
  const listed = parsePctList((await run("pct", ["list"], { capture: true })).stdout);
  const skippedStopped = listed.filter((ct) => ct.id !== builder.ctid && ct.status !== "running");
  const targets = listed.filter((ct) => ct.id !== builder.ctid && ct.status === "running");
  if (targets.length === 0) {
    process.stdout.write(`No running CT needs cleanup; builder CT ${builder.ctid} was preserved.${skippedStopped.length ? ` Stopped CTs were not started: ${skippedStopped.map((ct) => ct.id).join(", ")}.` : ""}\n`);
    return;
  }
  if (options["dry-run"]) {
    process.stdout.write(`eco prox clear-rust plan\n  Preserve builder: CT ${builder.ctid} (${builder.hostname})\n  Clean managed Rust toolchains/caches: ${targets.map((target) => `CT ${target.id} (${target.hostname})`).join(", ")}\n  Preserve application binaries and target/ directories.${skippedStopped.length ? `\n  Skip stopped CTs (never start them implicitly): ${skippedStopped.map((target) => `CT ${target.id} (${target.hostname})`).join(", ")}` : ""}\n`);
    return;
  }
  if (!options.yes && !(await confirmRustCleanup(builder, targets))) {
    process.stdout.write("Rust cleanup cancelled.\n");
    return;
  }
  const totals = { beforeManaged: 0, afterManaged: 0, beforeRoot: 0, afterRoot: 0 };
  for (const target of targets) {
    process.stdout.write(`[CT ${target.id}] Removing Eco-managed Rust toolchain and cache...\n`);
    const result = await run("pct", ["exec", target.id, "--", "bash", "-lc", RUST_CLEANUP_REPORT], { capture: true });
    const metric = cleanupMetrics(`${result.stdout}\n${result.stderr}`);
    for (const key of Object.keys(totals)) totals[key] += metric[key];
    const reclaimed = metric.beforeManaged - metric.afterManaged;
    const managedPercent = metric.beforeManaged > 0 ? (reclaimed / metric.beforeManaged) * 100 : 0;
    const rootSaved = Math.max(0, metric.beforeRoot - metric.afterRoot);
    const rootPercent = metric.beforeRoot > 0 ? (rootSaved / metric.beforeRoot) * 100 : 0;
    process.stdout.write(`  managed Rust: ${humanKiB(metric.beforeManaged)} → ${humanKiB(metric.afterManaged)}; reclaimed ${humanKiB(reclaimed)} (${managedPercent.toFixed(1)}%)\n  root filesystem: ${humanKiB(metric.beforeRoot)} used → ${humanKiB(metric.afterRoot)} used; reduced ${humanKiB(rootSaved)} (${rootPercent.toFixed(1)}%)\n`);
  }
  const reclaimed = totals.beforeManaged - totals.afterManaged;
  const managedPercent = totals.beforeManaged > 0 ? (reclaimed / totals.beforeManaged) * 100 : 0;
  const rootSaved = Math.max(0, totals.beforeRoot - totals.afterRoot);
  const rootPercent = totals.beforeRoot > 0 ? (rootSaved / totals.beforeRoot) * 100 : 0;
  process.stdout.write(`\nRust cleanup total (excluding builder CT ${builder.ctid}):\n  managed Rust: ${humanKiB(totals.beforeManaged)} → ${humanKiB(totals.afterManaged)}; reclaimed ${humanKiB(reclaimed)} (${managedPercent.toFixed(1)}%)\n  root filesystem used: ${humanKiB(totals.beforeRoot)} → ${humanKiB(totals.afterRoot)}; reduced ${humanKiB(rootSaved)} (${rootPercent.toFixed(1)}%)\n`);
}

function parseTunnelConfigs(text) {
  return text.split(/\r?\n/).map((line) => {
    const [configPath, tunnel, tunnelId = "", hostnames = ""] = line.split("\t");
    if (!configPath || !tunnel) return null;
    return {
      configPath,
      tunnel,
      tunnelId,
      hostnames: hostnames.split(",").map((hostname) => hostname.trim()).filter(Boolean)
    };
  }).filter(Boolean);
}

async function resolveTunnelTarget(reference) {
  if (/^\d+$/.test(String(reference))) {
    const hostname = await existingCtHostname(String(reference));
    if (!hostname) throw new Error(`No CT with ID ${reference} exists.`);
    return { ctid: String(reference), hostname };
  }
  const ctid = await findCtByHostname(reference);
  if (!ctid) throw new Error(`No CT named "${reference}" exists.`);
  return { ctid, hostname: String(reference) };
}

async function listTunnelConfigs(ctid) {
  const command = [
    "shopt -s nullglob",
    "for file in /etc/cloudflared/config.yml /etc/cloudflared-*/config.yml /root/.cloudflared/config.yml; do",
    "  [ -f \"$file\" ] || continue",
    "  tunnel=$(sed -n 's/^tunnel:[[:space:]]*//p' \"$file\" | head -n 1)",
    "  [ -n \"$tunnel\" ] || continue",
    "  tunnel_id=$(sed -n 's/^# eco-tunnel-id:[[:space:]]*//p' \"$file\" | head -n 1)",
    "  hostnames=$(sed -n 's/^[[:space:]]*-[[:space:]]*hostname:[[:space:]]*//p' \"$file\" | paste -sd, -)",
    "  printf '%s\\t%s\\t%s\\t%s\\n' \"$file\" \"$tunnel\" \"$tunnel_id\" \"$hostnames\"",
    "done"
  ].join("\n");
  const result = await run("pct", ["exec", ctid, "--", "bash", "-lc", command], { capture: true });
  return parseTunnelConfigs(result.stdout);
}

function tunnelServiceForConfig(configPath) {
  const named = configPath.match(/^\/etc\/cloudflared-([^/]+)\/config\.yml$/);
  return named ? `cloudflared-${named[1]}` : "cloudflared";
}

function isActiveTunnelConnectionError(error) {
  return /tunnel has active connections/i.test(String(error?.message || error));
}

async function deleteRemoteTunnelAfterStopping({ ctid, tunnel, account }) {
  const service = tunnelServiceForConfig(tunnel.configPath);
  await run("pct", ["exec", ctid, "--", "bash", "-lc", `systemctl disable --now ${shellQuote(service)} >/dev/null 2>&1 || true`]);
  process.stdout.write(`[CT ${ctid}] Stopped and disabled ${service}; waiting for Cloudflare to drop its tunnel connections.\n`);

  const attempts = 6;
  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    try {
      await removeRemoteTunnel(tunnel.tunnelId, account);
      return;
    } catch (error) {
      if (!isActiveTunnelConnectionError(error) || attempt === attempts) throw error;
      process.stdout.write(`Cloudflare still reports active connections for tunnel ${tunnel.tunnelId}. Retrying in 10 seconds (${attempt}/${attempts - 1})...\n`);
      await new Promise((resolve) => setTimeout(resolve, 10_000));
    }
  }
}

function hostnameMatchesRemoval(hostname, requestedDomain) {
  if (!requestedDomain.startsWith("*.")) return hostname === requestedDomain;
  const suffix = requestedDomain.slice(1);
  return hostname.endsWith(suffix) && hostname.length > suffix.length;
}

async function removeLocalTunnelHostname(ctid, configPath, hostname) {
  const command = [
    "tmp=$(mktemp)",
    "awk -v remove_hostname=" + shellQuote(hostname) + " '",
    "  /^  - hostname:[[:space:]]*/ { value = $0; sub(/^  - hostname:[[:space:]]*/, \"\", value); skip = (value == remove_hostname); if (!skip) print; next }",
    "  /^  - / { skip = 0 }",
    "  !skip { print }",
    "' " + shellQuote(configPath) + " > \"$tmp\"",
    "install -m 0644 \"$tmp\" " + shellQuote(configPath),
    "rm -f \"$tmp\""
  ].join("\n");
  await run("pct", ["exec", ctid, "--", "bash", "-lc", command]);
}

async function removeTunnel(positionals, options) {
  if (positionals.length > 2) {
    throw new Error("Usage: eco prox remove-tunnel [domain] [--target <ctid-or-hostname>] [--account <name>] [--dry-run]");
  }
  const domain = positionals[1];
  const target = await resolveTunnelTarget(options.target || "proxy");
  await ensureCtRunning(target.ctid);
  const tunnels = await listTunnelConfigs(target.ctid);

  if (!domain) {
    if (tunnels.length === 0) {
      process.stdout.write(`No cloudflared tunnel configuration found in CT ${target.ctid} (${target.hostname}).\n`);
      return;
    }
    process.stdout.write(`Cloudflared tunnel configuration in CT ${target.ctid} (${target.hostname}):\n`);
    for (const tunnel of tunnels) {
      process.stdout.write(`  ${tunnel.tunnel}\n    config: ${tunnel.configPath}\n    hostnames: ${tunnel.hostnames.join(", ") || "(none)"}\n`);
    }
    return;
  }

  const matches = tunnels.map((tunnel) => ({
    ...tunnel,
    selectedHostnames: tunnel.hostnames.filter((hostname) => hostnameMatchesRemoval(hostname, domain))
  })).filter((tunnel) => tunnel.selectedHostnames.length > 0);
  if (matches.length === 0) {
    throw new Error(`No configured tunnel for ${domain} was found in CT ${target.ctid} (${target.hostname}). Run \`eco prox remove-tunnel --target ${target.hostname}\` to list configured tunnels.`);
  }

  for (const tunnel of matches) {
    if (!tunnel.tunnelId) {
      throw new Error(`Tunnel ${tunnel.tunnel} at ${tunnel.configPath} is not Eco-managed (missing # eco-tunnel-id). Refusing to delete a remote tunnel without its exact ID.`);
    }
    if (/^\/etc\/cloudflared-[^/]+\/config\.yml$/.test(tunnel.configPath) && !options.account) {
      throw new Error(`Tunnel ${tunnel.tunnel} uses a named Cloudflare account. Pass --account <name> so Eco uses that account's CF_API_TOKEN_<NAME>, CF_ACCOUNT_ID_<NAME>, and CF_ZONE_ID_<NAME>.`);
    }
  }

  const plan = matches.map((tunnel) => {
    const remaining = tunnel.hostnames.filter((hostname) => !tunnel.selectedHostnames.includes(hostname));
    return remaining.length === 0
      ? `Stop/disable ${tunnelServiceForConfig(tunnel.configPath)}, delete remote tunnel ${tunnel.tunnelId} and DNS records, then remove ${tunnel.configPath}`
      : `Remove ${tunnel.selectedHostnames.join(", ")} from remote tunnel ${tunnel.tunnelId}, their DNS records, and ${tunnel.configPath}; preserve ${remaining.join(", ")}`;
  });
  if (options["dry-run"]) {
    process.stdout.write(`eco prox remove-tunnel plan\n  CT: ${target.ctid} (${target.hostname})\n  Domain: ${domain}\n  Cloudflare account: ${options.account || "default"}\n${plan.map((step) => `  ${step}`).join("\n")}\n`);
    return;
  }
  for (const tunnel of matches) {
    const remainingHostnames = tunnel.hostnames.filter((hostname) => !tunnel.selectedHostnames.includes(hostname));
    if (remainingHostnames.length === 0) {
      await deleteRemoteTunnelAfterStopping({ ctid: target.ctid, tunnel, account: options.account });
    } else {
      for (const hostname of tunnel.selectedHostnames) {
        await removeRemoteTunnelHostname(tunnel.tunnelId, hostname, options.account);
        process.stdout.write(`Removed ${hostname} from remote Cloudflare tunnel ${tunnel.tunnelId}.\n`);
      }
    }
    for (const hostname of tunnel.selectedHostnames) {
      if (await removeDnsRecordForTunnel(hostname, tunnel.tunnelId, options.account)) {
        process.stdout.write(`Removed Cloudflare DNS record for ${hostname}.\n`);
      }
    }
    if (remainingHostnames.length === 0) {
      const service = tunnelServiceForConfig(tunnel.configPath);
      process.stdout.write(`Deleted remote Cloudflare tunnel ${tunnel.tunnelId}.\n`);
      await run("pct", ["exec", target.ctid, "--", "bash", "-lc", `rm -f ${shellQuote(tunnel.configPath)} ${shellQuote(`${tunnel.configPath}.bak`)}.*`]);
      process.stdout.write(`[CT ${target.ctid}] Removed local ${service} configuration for tunnel ${tunnel.tunnel}.\n`);
    } else {
      for (const hostname of tunnel.selectedHostnames) {
        await removeLocalTunnelHostname(target.ctid, tunnel.configPath, hostname);
      }
      process.stdout.write(`[CT ${target.ctid}] Preserved shared ${tunnelServiceForConfig(tunnel.configPath)} tunnel configuration for ${remainingHostnames.join(", ")}.\n`);
    }
  }
  process.stdout.write(`Tunnel removal complete. Rerun eco up to bootstrap a replacement only if its tunnel was fully removed.\n`);
}

async function renamePct(positionals) {
  const ctid = positionals[1];
  const newHostname = positionals[2];

  if (!ctid || !newHostname) {
    throw new Error("Usage: eco prox rename-pct <ctid> <new-hostname>");
  }
  if (!/^\d+$/.test(ctid)) throw new Error("CTID must be a number.");
  if (!/^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$/.test(newHostname.toLowerCase())) {
    throw new Error("Invalid hostname.");
  }

  const configResult = await run("pct", ["config", ctid], { capture: true });
  const oldHostname = configResult.stdout.split("\n").find((l) => l.startsWith("hostname:"))?.split(/:\s*/)[1]?.trim();
  if (!oldHostname) throw new Error(`CT ${ctid} not found or has no hostname.`);
  if (oldHostname === newHostname.toLowerCase()) {
    process.stdout.write(`CT ${ctid} already uses hostname ${newHostname}.\n`);
    return;
  }

  const backupDir = "/root/pct-config-backups";
  const timestamp = new Date().toISOString().replace(/[^0-9]/g, "").slice(0, 14);
  const backupFile = `${backupDir}/${ctid}-${oldHostname}-${timestamp}.conf`;
  await run("mkdir", ["-p", backupDir]);
  const { stdout: conf } = await run("pct", ["config", ctid], { capture: true });
  await (await import("node:fs/promises")).writeFile(backupFile, conf);

  process.stdout.write(`CTID          : ${ctid}\nHostname lama : ${oldHostname}\nHostname baru : ${newHostname}\nBackup        : ${backupFile}\n\n`);
  await run("pct", ["set", ctid, "--hostname", newHostname.toLowerCase()]);

  const statusResult = await run("pct", ["status", ctid], { capture: true });
  const status = statusResult.stdout.trim().split(/\s+/)[1];

  if (status === "running") {
    process.stdout.write("Memperbarui /etc/hostname dan /etc/hosts di dalam CT...\n");
    const innerScript = [
      "set -eu",
      `printf '%s\\n' '${newHostname.toLowerCase()}' > /etc/hostname`,
      "[ -f /etc/hosts ] && cp -a /etc/hosts \"/etc/hosts.rename-backup.$(date +%Y%m%d-%H%M%S)\"",
      `awk -v old='${oldHostname}' -v new='${newHostname.toLowerCase()}' '{for(i=1;i<=NF;i++){if($i==old)$i=new};print}' /etc/hosts > /etc/hosts.new && mv /etc/hosts.new /etc/hosts`
    ].join("\n");
    await run("pct", ["exec", ctid, "--", "sh", "-c", innerScript]);
    process.stdout.write("Reboot CT agar hostname kernel ikut berubah...\n");
    await run("pct", ["reboot", ctid]);
    process.stdout.write("Menunggu CT kembali aktif...\n");
    for (let i = 0; i < 30; i++) {
      try { await run("pct", ["exec", ctid, "--", "true"]); break; } catch { await new Promise((r) => setTimeout(r, 1000)); }
    }
  } else {
    process.stdout.write("CT sedang mati. Perubahan Proxmox diterapkan saat CT dinyalakan.\n");
  }

  process.stdout.write("\n=== Verifikasi ===\n");
  const finalConfig = await run("pct", ["config", ctid], { capture: true });
  process.stdout.write(finalConfig.stdout.split("\n").find((l) => l.startsWith("hostname:")) + "\n");
  if (status === "running") {
    const hn = await run("pct", ["exec", ctid, "--", "hostname"], { capture: true });
    process.stdout.write(`Kernel hostname : ${hn.stdout.trim()}\n`);
  }
  process.stdout.write("\nRename selesai.\n");
}

async function shrinkPct(positionals) {
  const ctid = positionals[1];
  const targetGb = positionals[2];
  const tempId = positionals[3] || "900";

  if (!ctid || !targetGb) throw new Error("Usage: eco prox shrink-pct <ctid> <target-gb> [temp-ctid]");

  const backupDir = "/var/lib/vz/dump";
  const maxUsagePct = 70;

  process.stdout.write(`============================================================\nShrink Proxmox LXC\n============================================================\nCT asli      : ${ctid}\nTarget disk  : ${targetGb} GB\nCT sementara : ${tempId}\n\n`);

  // Check temp CT doesn't exist
  try { await run("pct", ["status", tempId]); throw new Error(`CT sementara ${tempId} sudah ada. Pilih ID lain.`); } catch (e) { if (!e.message.includes("does not exist") && !e.message.includes("CT sementara")) throw e; }

  const configResult = await run("pct", ["config", ctid], { capture: true });
  const rootfsLine = configResult.stdout.split("\n").find((l) => l.startsWith("rootfs:"))?.split(/:\s*/)[1]?.trim();
  if (!rootfsLine) throw new Error(`Konfigurasi rootfs CT ${ctid} tidak ditemukan.`);
  const storage = rootfsLine.split(":")[0];
  process.stdout.write(`Storage : ${storage}\n\n`);

  // Check disk usage
  const statusResult = await run("pct", ["status", ctid], { capture: true });
  const ctStatus = statusResult.stdout.trim().split(/\s+/)[1];
  let startedByScript = false;
  if (ctStatus !== "running") {
    process.stdout.write("Menyalakan CT untuk memeriksa penggunaan disk...\n");
    await run("pct", ["start", ctid]);
    startedByScript = true;
    await new Promise((r) => setTimeout(r, 3000));
  }

  const dfResult = await run("pct", ["exec", ctid, "--", "df", "-B1", "--output=used,size", "/"], { capture: true });
  const [usedBytes, totalBytes] = dfResult.stdout.trim().split("\n")[1].trim().split(/\s+/).map(Number);
  const targetBytes = Number(targetGb) * 1024 * 1024 * 1024;
  const usagePct = Math.floor(usedBytes * 100 / targetBytes);

  process.stdout.write(`Pemakaian saat ini: ${Math.round(usedBytes / 1024 / 1024)} MB / Target: ${targetGb} GB (${usagePct}% terpakai)\n\n`);

  if (usedBytes >= targetBytes) throw new Error("Data lebih besar daripada target disk.");
  if (usagePct > maxUsagePct) throw new Error(`Target terlalu sempit: akan terpakai ${usagePct}%. Maksimum ${maxUsagePct}%.`);

  // Backup
  process.stdout.write("Menghentikan CT untuk backup konsisten...\n");
  const currentStatus = (await run("pct", ["status", ctid], { capture: true })).stdout.trim().split(/\s+/)[1];
  if (currentStatus === "running") {
    try { await run("pct", ["shutdown", ctid, "--timeout", "60"]); } catch { await run("pct", ["stop", ctid]); }
  }

  process.stdout.write("Membuat backup...\n");
  await run("vzdump", [ctid, "--dumpdir", backupDir, "--mode", "stop", "--compress", "zstd"]);

  const findResult = await run("find", [backupDir, "-maxdepth", "1", "-type", "f", "-name", `vzdump-lxc-${ctid}-*.tar.zst`], { capture: true });
  const backupFiles = findResult.stdout.trim().split("\n").filter(Boolean).sort();
  const backupFile = backupFiles[backupFiles.length - 1];
  if (!backupFile) throw new Error("File backup tidak ditemukan.");
  process.stdout.write(`Backup: ${backupFile}\n\n`);

  // Restore to temp
  process.stdout.write(`Restore ke CT sementara ${tempId} dengan disk ${targetGb} GB...\n`);
  await run("pct", ["restore", tempId, backupFile, "--storage", storage, "--rootfs", `${storage}:${targetGb}`]);
  await run("pct", ["start", tempId]);
  await new Promise((r) => setTimeout(r, 5000));

  process.stdout.write("\n=== Hasil CT Sementara ===\n");
  const dfNew = await run("pct", ["exec", tempId, "--", "df", "-h", "/"], { capture: true });
  process.stdout.write(dfNew.stdout);

  const ipResult = await run("pct", ["exec", tempId, "--", "hostname", "-I"], { capture: true });
  process.stdout.write(`IP: ${ipResult.stdout.trim()}\n`);

  process.stdout.write(`\nCT asli ${ctid} dalam keadaan mati. CT sementara ${tempId} berjalan.\nUji CT sementara, lalu jalankan:\n  pct stop ${tempId} && pct destroy ${tempId} --purge\n  pct restore ${ctid} ${backupFile} --storage ${storage} --rootfs ${storage}:${targetGb}\n  pct start ${ctid}\n\nBackup rollback tersedia di:\n  ${backupFile}\n`);
}

async function sizePct() {
  const listResult = await run("pct", ["list"], { capture: true });
  const ctids = listResult.stdout.trim().split("\n").slice(1).map((l) => l.trim().split(/\s+/)[0]).filter(Boolean);

  process.stdout.write(`${"CTID".padEnd(6)} ${"NAME".padEnd(20)} ${"SIZE".padEnd(10)} ${"USED".padEnd(10)} ${"FREE".padEnd(10)} FREE%\n`);

  for (const ctid of ctids) {
    const configResult = await run("pct", ["config", ctid], { capture: true });
    const name = configResult.stdout.split("\n").find((l) => l.startsWith("hostname:"))?.split(/:\s*/)[1]?.trim() || ctid;
    try {
      const dfResult = await run("pct", ["exec", ctid, "--", "df", "-h", "/"], { capture: true });
      const parts = dfResult.stdout.trim().split("\n")[1]?.trim().split(/\s+/);
      if (!parts || parts.length < 5) continue;
      const [, size, used, avail, usePct] = parts;
      const freePct = 100 - parseInt(usePct);
      process.stdout.write(`${ctid.padEnd(6)} ${name.padEnd(20)} ${size.padEnd(10)} ${used.padEnd(10)} ${avail.padEnd(10)} ${freePct}%\n`);
    } catch {
      process.stdout.write(`${ctid.padEnd(6)} ${name.padEnd(20)} ${"(stopped)".padEnd(10)}\n`);
    }
  }
}

async function showPorts() {
  const cwd = process.cwd();
  const result = await run("find", [cwd, "-name", ".env", "-type", "f", "-not", "-path", "*/node_modules/*", "-not", "-path", "*/.git/*"], { capture: true });
  const envFiles = result.stdout.trim().split("\n").filter(Boolean);

  if (envFiles.length === 0) {
    process.stdout.write(`No .env files found under ${cwd}\n`);
    return;
  }

  const PORT_KEYS = /^(PORT|SERVER_PORT|APP_PORT|SERVICE_PORT|HTTP_PORT|GRPC_PORT|WS_PORT)/;
  let found = false;

  for (const file of envFiles.sort()) {
    const rel = file.startsWith(cwd) ? file.slice(cwd.length + 1) : file;
    const parts = rel.split("/");
    // derive project label: first two path segments (e.g. "apindo/backend")
    const project = parts.slice(0, Math.min(2, parts.length - 1)).join("/") || ".";

    let content = "";
    try { content = await (await import("node:fs/promises")).readFile(file, "utf8"); } catch { continue; }

    const portLines = content
      .split("\n")
      .filter((line) => PORT_KEYS.test(line.trim()) && !line.trim().startsWith("#"))
      .map((line) => line.trim());

    if (portLines.length === 0) continue;

    found = true;
    process.stdout.write(`\n[${project}]  ${file}\n`);
    portLines.forEach((line) => process.stdout.write(`  ${line}\n`));
  }

  if (!found) {
    process.stdout.write(`No port variables found in any .env under ${cwd}\n`);
  }
}

async function clearEnv(positionals, options) {
  const cwd = process.cwd();
  const result = await run("find", [cwd, "-name", ".env", "-type", "f", "-not", "-path", "*/node_modules/*"], { capture: true });
  const envFiles = result.stdout.trim().split("\n").filter(Boolean);

  // Find all .configure-state files to clear the ports-configured flag
  const stateResult = await run("find", [cwd, "-name", ".configure-state", "-type", "f"], { capture: true });
  const stateFiles = stateResult.stdout.trim().split("\n").filter(Boolean);

  if (envFiles.length === 0 && stateFiles.length === 0) {
    process.stdout.write(`No .env or .configure-state files found in ${cwd}\n`);
    return;
  }

  if (envFiles.length > 0) {
    process.stdout.write(`Found ${envFiles.length} .env file(s):\n`);
    envFiles.forEach((file) => process.stdout.write(`  ${file}\n`));
  }
  if (stateFiles.length > 0) {
    process.stdout.write(`Found ${stateFiles.length} .configure-state file(s) — will clear ECO_PORTS_CONFIGURED:\n`);
    stateFiles.forEach((file) => process.stdout.write(`  ${file}\n`));
  }

  if (options["dry-run"]) {
    process.stdout.write(`\n--dry-run enabled, no files modified.\n`);
    return;
  }

  for (const file of envFiles) {
    await run("rm", [file]);
  }
  if (envFiles.length > 0) process.stdout.write(`\nRemoved ${envFiles.length} .env file(s).\n`);

  for (const file of stateFiles) {
    await run("sed", ["-i", "/^ECO_PORTS_CONFIGURED/d", file]);
    await run("sed", ["-i", "/^ECO_PORTS_CONFIGURED_MODE/d", file]);
  }
  if (stateFiles.length > 0) process.stdout.write(`Cleared ECO_PORTS_CONFIGURED from ${stateFiles.length} .configure-state file(s) — ports will be reallocated on next eco up.\n`);
}

async function setCtResources(positionals, options) {
  const reference = positionals[1];
  if (!reference) throw new Error("Usage: eco prox set-ct <ctid-or-hostname> --cores <n> --memory <mb> [--swap <mb>] [--dry-run]");

  const { ctid, hostname } = await resolveCtByReference(reference);

  const settings = [];
  let desc = [];

  if (options.cores) {
    settings.push("--cores", options.cores);
    desc.push(`cores: ${options.cores}`);
  }
  if (options.memory) {
    settings.push("--memory", options.memory);
    desc.push(`memory: ${options.memory} MB`);
  }
  if (options.swap) {
    settings.push("--swap", options.swap);
    desc.push(`swap: ${options.swap} MB`);
  }

  if (settings.length === 0) {
    throw new Error("At least one of --cores, --memory, or --swap is required.");
  }

  if (options["dry-run"]) {
    process.stdout.write(`eco prox set-ct plan
  CT: ${ctid} (${hostname})
  Set: ${desc.join(", ")}
  Command: pct set ${ctid} ${settings.join(" ")}
`);
    return;
  }

  process.stdout.write(`[CT ${ctid}] Setting ${desc.join(", ")}...\n`);
  await run("pct", ["set", ctid, ...settings]);
  process.stdout.write(`CT ${ctid} (${hostname}) updated.\n`);
}

export async function runProx(args) {
  const { options, positionals } = parse(args);
  if (!positionals[0] || ["help", "--help", "-h"].includes(positionals[0]) || options.help) { help(); return; }
  if (positionals[0] === "attach" && positionals[1] === "minio") {
    await attachMinio(positionals, options);
    return;
  }
  if (positionals[0] === "archive") {
    await archiveWorkload(positionals, options);
    return;
  }
  if (positionals[0] === "unarchive") {
    await unarchiveWorkload(positionals, options);
    return;
  }
  if (positionals[0] === "clear-rust") {
    await clearRust(positionals, options);
    return;
  }
  if (positionals[0] === "remove-tunnel") {
    await removeTunnel(positionals, options);
    return;
  }
  if (positionals[0] === "clearenv") {
    await clearEnv(positionals, options);
    return;
  }
  if (positionals[0] === "showports") {
    await showPorts();
    return;
  }
  if (positionals[0] === "tunnel-replicas") {
    await runProxy(["tunnel-replicas", ...args.slice(1)]);
    return;
  }
  if (positionals[0] === "rename-pct") {
    await renamePct(positionals);
    return;
  }
  if (positionals[0] === "shrink-pct") {
    await shrinkPct(positionals);
    return;
  }
  if (positionals[0] === "size-pct") {
    await sizePct();
    return;
  }
  if (positionals[0] === "set-ct") {
    await setCtResources(positionals, options);
    return;
  }
  if (positionals[0] === "prepare" && positionals[1] === "rust-builder") {
    await prepareRustBuilder(positionals, options);
    return;
  }
  if (positionals[0] === "createct" && positionals[1] === "rust-builder") {
    const requestedName = positionals[2] || options.hostname || "rust-builder";
    const hostname = options.hostname || requestedName;
    const template = await resolveInstalledTemplate(options.template);
    const knownId = options.id || (options["dry-run"] ? null : await findCtByHostname(hostname));
    const id = knownId || (options["dry-run"] ? "<next-available-id>" : await nextId());
    const create = pctCreateArgs(id, { ...options, disk: options.disk || "60", cores: options.cores || "4", memory: options.memory || "8192" }, template, hostname);
    if (options["dry-run"]) {
      process.stdout.write(`eco prox createct rust-builder plan\n  Template: ${template}\n  pct ${create.join(" ")}\n  pct start ${id}\n  pct exec ${id} -- install managed Rust toolchain\n`);
      return;
    }
    const existingHostname = await existingCtHostname(id);
    if (existingHostname && existingHostname !== hostname) throw new Error(`CT ${id} already belongs to hostname "${existingHostname}". Refusing to modify it; choose another --id.`);
    if (!existingHostname) {
      process.stdout.write(`[CT ${id}] Creating ${hostname}...\n`);
      await run("pct", create);
    }
    await ensureCtRunning(id);
    await waitForCtExec(id);
    await installRustBuilder(id);
    process.stdout.write(`Rust builder CT ${id} is ready (${existingHostname ? "reused" : "created"}). Set ECO_RUST_DEDICATED_BUILDER=${hostname} before running eco up.\n`);
    return;
  }
  if (positionals[0] !== "createct" || positionals[1] !== "minio") throw new Error('Usage: eco prox createct minio <name> [options]');
  const requestedName = positionals[2] || options.hostname || "minio";
  const hostname = options.hostname || requestedName;
  const template = await resolveInstalledTemplate(options.template);
  const knownId = options.id || (options["dry-run"] ? null : await findCtByHostname(hostname));
  const id = knownId || (options["dry-run"] ? "<next-available-id>" : await nextId());
  const create = pctCreateArgs(id, options, template, hostname);
  if (options["dry-run"]) {
    process.stdout.write(`eco prox createct minio plan\n  Template: ${template}\n  pct ${create.join(" ")}\n  pct start ${id}\n  pct push ${id} <eco install-minio.sh> /tmp/eco-install-minio.sh\n  pct exec ${id} -- ECO_DEPLOY_MODE=prod eco install minio --ensure\n`);
    return;
  }
  const existingHostname = await existingCtHostname(id);
  if (existingHostname && existingHostname !== hostname) {
    throw new Error(`CT ${id} already belongs to hostname "${existingHostname}". Refusing to modify it; choose another --id.`);
  }
  const createdNow = !existingHostname;
  if (!createdNow) {
    const approved = options["yes-reinstall"] || await confirmReinstall(id, hostname);
    if (!approved) {
      process.stdout.write(`Existing MinIO CT ${id} was left unchanged.\n`);
      return;
    }
  }
  if (createdNow) {
    process.stdout.write(`[CT ${id}] Creating ${hostname}...\n`);
    await run("pct", create);
  }
  try {
    process.stdout.write(`[CT ${id}] Starting and waiting for first boot...\n`);
    await ensureCtRunning(id);
    await waitForCtExec(id);
    await installMinio(id, { reset: !createdNow });
  } catch (error) {
    if (createdNow) {
      const diagnostics = await diagnoseCt(id);
      if (options["keep-on-failure"]) {
        throw new Error(`MinIO setup failed; CT ${id} was intentionally kept for diagnosis.\nCause: ${error.message}\n\nCT diagnostics:\n${diagnostics}\n\nWhen finished, remove it with: pct stop ${id}; pct destroy ${id} --purge 1`);
      }
      await removeNewCt(id);
      throw new Error(`MinIO setup failed; newly created CT ${id} and its volumes were removed.\nCause: ${error.message}\n\nCT diagnostics before rollback:\n${diagnostics}`);
    }
    throw new Error(`MinIO setup failed on existing CT ${id}; it was preserved. ${error.message}`);
  }
  process.stdout.write(`MinIO CT ${id} is healthy (${createdNow ? "created" : "reused"}). Attach it to an estate through Eco before running \`eco up\`.\n`);
}
