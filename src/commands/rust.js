import { readdir, rm, unlink, stat } from "node:fs/promises";
import { join } from "node:path";

async function findProjectRoot(startDir) {
  let dir = startDir;
  while (true) {
    const entries = await readdir(dir).catch(() => []);
    if (entries.some((e) => e === "ecompose.yml") || entries.some((e) => e === "Cargo.toml" && entries.includes("stuff8_bootstrap"))) {
      return dir;
    }
    // Look for ecompose.yml in subdirs
    for (const entry of entries) {
      if (entry.endsWith(".yml") || entry.endsWith(".yaml")) {
        const content = await import("node:fs/promises").then(m => m.readFile(join(dir, entry), "utf8")).catch(() => "");
        if (content.includes("ecompose") || content.startsWith("# Version")) {
          return dir;
        }
      }
    }
    const parent = join(dir, "..");
    if (parent === dir) throw new Error("Could not find project root (no ecompose.yml found).");
    dir = parent;
  }
}

async function findTargetDirs(rootDir) {
  const targets = [];
  const ignoredDirs = new Set([".git", "node_modules", "eco"]);

  async function scan(dir) {
    let entries;
    try {
      entries = await readdir(dir, { withFileTypes: true });
    } catch {
      return;
    }
    for (const entry of entries) {
      if (!entry.isDirectory()) continue;
      if (ignoredDirs.has(entry.name)) continue;
      const fullPath = join(dir, entry.name);
      if (entry.name === "target") {
        // Only treat as Rust target if sibling Cargo.toml exists
        const hasCargo = entries.some((e) => e.name === "Cargo.toml" || e.name === "Cargo.lock");
        if (hasCargo) {
          targets.push(fullPath);
          continue; // don't recurse into target/
        }
      }
      await scan(fullPath);
    }
  }

  await scan(rootDir);
  return targets;
}

async function findHashFiles(rootDir) {
  const hashFiles = [];
  const ignoredDirs = new Set([".git", "node_modules", "target"]);

  async function scan(dir) {
    let entries;
    try {
      entries = await readdir(dir, { withFileTypes: true });
    } catch {
      return;
    }
    for (const entry of entries) {
      if (entry.isDirectory()) {
        if (!ignoredDirs.has(entry.name)) await scan(join(dir, entry.name));
      } else if (entry.name === ".eco-rust-hash") {
        hashFiles.push(join(dir, entry.name));
      }
    }
  }

  await scan(rootDir);
  return hashFiles;
}

export async function runRust(args) {
  const [subcommand, ...rest] = args;

  if (!subcommand || subcommand === "help" || subcommand === "--help") {
    process.stdout.write(`eco rust\n\nUsage:\n  eco rust cleartarget [--dry-run]  Remove all Rust target/ directories and .eco-rust-hash files to force a full recompile on next eco up.\n`);
    return;
  }

  if (subcommand === "cleartarget") {
    const dryRun = rest.includes("--dry-run");
    const cwd = process.cwd();

    // Find estate root by walking up to find ecompose.yml
    let estateRoot = cwd;
    let found = false;
    // Walk up looking for ecompose.yml in current dir or its subdirectories
    for (let dir = cwd; ; dir = join(dir, "..")) {
      const entries = await readdir(dir).catch(() => []);
      // Direct match
      if (entries.includes("ecompose.yml")) {
        // ecompose.yml is in this dir — if this dir looks like a bootstrap subdir,
        // estate root is the parent (repos are siblings of this dir)
        const parentEntries = await readdir(join(dir, "..")).catch(() => []);
        const parentHasGit = parentEntries.some(e => e === ".git");
        estateRoot = parentHasGit ? dir : join(dir, "..");
        // If parent contains multiple git repos, this bootstrap is a child
        const parentHasMultipleRepos = parentEntries.filter(e => !e.startsWith(".")).length > 3;
        estateRoot = parentHasMultipleRepos ? join(dir, "..") : dir;
        found = true;
        break;
      }
      // Check one level of subdirectories (bootstrap pattern: stuff8/stuff8_bootstrap/ecompose.yml)
      for (const entry of entries) {
        const subEntries = await readdir(join(dir, entry)).catch(() => []);
        if (subEntries.includes("ecompose.yml")) {
          estateRoot = dir;
          found = true;
          break;
        }
      }
      if (found) break;
      const parent = join(dir, "..");
      if (parent === dir) break;
    }
    if (!found) {
      throw new Error("Could not find ecompose.yml. Run from inside an eco project directory.");
    }

    process.stdout.write(`${dryRun ? "[dry-run] " : ""}Clearing Rust build artifacts in: ${estateRoot}\n\n`);

    const targetDirs = await findTargetDirs(estateRoot);
    const hashFiles = await findHashFiles(estateRoot);

    if (targetDirs.length === 0 && hashFiles.length === 0) {
      process.stdout.write("Nothing to clean.\n");
      return;
    }

    for (const dir of targetDirs) {
      const rel = dir.replace(estateRoot + "/", "");
      try {
        const info = await stat(dir);
        process.stdout.write(`  ${dryRun ? "[dry-run] " : ""}rm -rf ${rel}\n`);
        if (!dryRun) await rm(dir, { recursive: true, force: true });
      } catch {
        process.stdout.write(`  skipped (not accessible): ${rel}\n`);
      }
    }

    for (const file of hashFiles) {
      const rel = file.replace(estateRoot + "/", "");
      process.stdout.write(`  ${dryRun ? "[dry-run] " : ""}rm ${rel}\n`);
      if (!dryRun) await unlink(file).catch(() => {});
    }

    process.stdout.write(`\n${dryRun ? "[dry-run] " : ""}Done. Run eco up to trigger a full recompile.\n`);
    return;
  }

  throw new Error(`Unknown rust subcommand: ${subcommand}\n\nRun "eco rust help" for usage.`);
}
