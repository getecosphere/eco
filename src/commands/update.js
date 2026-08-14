import { spawn } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const packageRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");

function runCommand(command, args, cwd, { silent = false } = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd,
      stdio: silent ? "pipe" : "inherit",
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

export async function runUpdate() {
  process.stdout.write(`Updating eco from ${packageRoot}\n`);
  await runCommand("git", ["fetch", "origin"], packageRoot);
  await runCommand("git", ["reset", "--hard", "origin/main"], packageRoot, { silent: true });
  await runCommand("git", ["clean", "-xfd"], packageRoot, { silent: true });
  process.stdout.write(`eco is up to date and clean.\n`);
}
