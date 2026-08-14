import { spawn } from "node:child_process";
import { access } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { findEstateRoot, findWorkspaceRoot } from "./workspace.js";

const packageRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");

export async function runBundledScript(scriptName, args, options = {}) {
  const originalCwd = process.cwd();
  const workspaceRoot = await findWorkspaceRoot(originalCwd);
  const estateRoot = await findEstateRoot(originalCwd);
  const scriptPath = path.join(packageRoot, scriptName);

  await access(scriptPath).catch(() => {
    throw new Error(`Cannot find bundled script: ${scriptPath}`);
  });

  await new Promise((resolve, reject) => {
    const child = spawn("bash", [scriptPath, ...args], {
      cwd: originalCwd,
      stdio: "inherit",
      env: {
        ...process.env,
        ...(options.extraEnv || {}),
        ECOLOGY_WORKSPACE_ROOT: options.scope === "estate" ? estateRoot : workspaceRoot
      }
    });

    child.on("error", reject);
    child.on("exit", (code, signal) => {
      if (signal) {
        reject(new Error(`${scriptName} terminated by signal ${signal}`));
        return;
      }

      if (code !== 0) {
        reject(new Error(`${scriptName} exited with code ${code}`));
        return;
      }

      resolve();
    });
  });
}
