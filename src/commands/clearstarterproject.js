import { access, readdir, rm } from "node:fs/promises";
import { spawn } from "node:child_process";
import path from "node:path";

const output = process.stdout;
const color = output.isTTY && !process.env.NO_COLOR;
const bold = (s) => (color ? `\x1b[1m${s}\x1b[0m` : s);
const dim = (s) => (color ? `\x1b[2m${s}\x1b[0m` : s);
const cyan = (s) => (color ? `\x1b[36m${s}\x1b[0m` : s);
const green = (s) => (color ? `\x1b[32m${s}\x1b[0m` : s);

// Starter scaffold written by `eco startproject` (createCompositionScaffold).
// Each entry removes the placeholder runtime files but keeps the service
// contract files (.gitignore, .env.example) so a replacement can move in.
const STARTER_PATHS = [
  { path: "frontend/package.json", label: "frontend package.json" },
  { path: "frontend/index.js", label: "frontend starter server" },
  { path: "frontend/index.html", label: "frontend starter page" },
  { path: "frontend/images", label: "frontend starter images", dir: true },
  { path: "backend/Cargo.toml", label: "backend Cargo.toml" },
  { path: "backend/src", label: "backend starter source", dir: true }
];

async function pathExists(targetPath) {
  try {
    await access(targetPath);
    return true;
  } catch {
    return false;
  }
}

function findCompositionArg(args) {
  const positionals = args.filter((arg) => !arg.startsWith("--"));
  return positionals[0] || null;
}

async function findCompositionDir(cwd) {
  // Walk up from cwd; a `<project>_composition` directory is the composition repo.
  let dir = path.resolve(cwd);
  const { root } = path.parse(dir);
  while (true) {
    try {
      const entries = await readdir(dir, { withFileTypes: true });
      const composition = entries.find(
        (entry) => entry.isDirectory() && entry.name.endsWith("_composition")
      );
      if (composition) {
        return path.join(dir, composition.name);
      }
    } catch {}
    if (dir === root) break;
    dir = path.dirname(dir);
  }
  return null;
}

async function gitStatusClean(cwd) {
  return new Promise((resolve) => {
    const child = spawn("git", ["status", "--porcelain"], { cwd, stdio: ["ignore", "pipe", "ignore"] });
    let stdout = "";
    child.stdout.on("data", (c) => { stdout += c; });
    child.on("error", () => resolve(null));
    child.on("exit", (code) => {
      if (code !== 0) return resolve(null);
      const lines = stdout.trim().split("\n").filter(Boolean);
      // Ignore the untouched README.md-style untracked docs; flag tracked changes
      // only so an in-progress real app is never silently wiped.
      const tracked = lines.filter((line) => line.startsWith(" M ") || line.startsWith("M ") || line.startsWith(" D ") || line.startsWith("D "));
      resolve(tracked.length === 0);
    });
  });
}

function clearstarterprojectHelp() {
  output.write(`eco clearstarterproject [path]\n\nRemove the placeholder starter runtime files from a <project>_composition\nrepository, leaving the service contract (.gitignore, .env.example) and the\nrepository itself intact so a real frontend/backend can replace the starter.\n\nArguments:\n  path   path to the composition repository (default: nearest *_composition dir)\n\nOptions:\n  --commit   commit the removal in the composition repository\n  --dry-run  list what would be removed without deleting\n`);
}

export async function runClearStarterProject(args) {
  if (args.includes("help") || args.includes("--help") || args.includes("-h")) {
    clearstarterprojectHelp();
    return;
  }

  const dryRun = args.includes("--dry-run");
  const commit = args.includes("--commit");
  const explicitPath = findCompositionArg(args);

  const compositionDir = explicitPath
    ? path.resolve(explicitPath)
    : await findCompositionDir(process.cwd());

  if (!compositionDir) {
    throw new Error("No <project>_composition directory found. Pass the path or run inside the estate.");
  }
  if (!(await pathExists(compositionDir))) {
    throw new Error(`Composition directory not found: ${compositionDir}`);
  }

  const isRepo = await pathExists(path.join(compositionDir, ".git"));
  if (isRepo && !(await gitStatusClean(compositionDir)) && !dryRun) {
    throw new Error(
      `${compositionDir} has uncommitted changes to tracked files. ` +
      "Commit or stash them before clearing the starter project."
    );
  }

  const present = [];
  for (const entry of STARTER_PATHS) {
    if (await pathExists(path.join(compositionDir, entry.path))) {
      present.push(entry);
    }
  }

  output.write(`\n${bold("Eco starter project")}  ${dim(compositionDir)}\n`);
  if (present.length === 0) {
    output.write(`  ${green("Nothing to clear")} — no starter files present.\n\n`);
    return;
  }

  for (const entry of present) {
    output.write(`  ${dryRun ? "would remove" : "removing"}  ${cyan(entry.path)}\n`);
  }

  if (dryRun) {
    output.write("\n");
    return;
  }

  for (const entry of present) {
    await rm(path.join(compositionDir, entry.path), { recursive: true, force: true });
  }

  // Drop empty frontend/backend trees so the repo reflects "no app yet".
  for (const serviceDir of ["frontend", "backend"]) {
    const full = path.join(compositionDir, serviceDir);
    if (await pathExists(full)) {
      const remaining = await readdir(full);
      if (remaining.length === 0) {
        await rm(full, { recursive: true, force: true });
        output.write(`  removing empty   ${cyan(serviceDir + "/")}\n`);
      }
    }
  }

  if (commit && isRepo) {
    await new Promise((resolve, reject) => {
      const child = spawn("git", ["add", "-A"], { cwd: compositionDir, stdio: "inherit" });
      child.on("error", reject);
      child.on("exit", (code) => code === 0 ? resolve() : reject(new Error(`git add exited with code ${code}`)));
    });
    await new Promise((resolve, reject) => {
      const child = spawn("git", ["commit", "-m", "clear: remove starter project scaffold"], { cwd: compositionDir, stdio: "inherit" });
      child.on("error", reject);
      child.on("exit", (code) => code === 0 ? resolve() : reject(new Error(`git commit exited with code ${code}`)));
    });
    output.write(`\n  ${green("Committed")} removal in ${compositionDir}\n`);
  } else if (isRepo && !commit) {
    output.write(`\n  ${dim("Commit the removal with: git -C <composition> commit -m \"clear: remove starter scaffold\"")}\n`);
  }

  output.write("\n");
}
