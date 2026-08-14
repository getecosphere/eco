import { spawn } from "node:child_process";
import { access } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const packageRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");

// eco install is infra-level: unlike configure/provision/up, it deliberately
// doesn't resolve a workspace/estate root -- installing MinIO isn't tied to
// any one project's ecompose.yml, it's a single shared instance for
// whatever machine/CT you run this on. See eco/install-minio.sh.
const INSTALLERS = {
  minio: "install-minio.sh",
  onnxruntime: "install-onnxruntime.sh"
};

function installHelp() {
  process.stdout.write(`eco install

Install infra-level tooling that isn't tied to any one project's
ecompose.yml -- run once per machine/CT, shared by every estate on it.

Usage:
  eco install minio

  minio         Installs MinIO (prebuilt binary) and starts it running locally.
                Prints the endpoint/credentials to paste into "eco startproject"'s
                object storage prompt, or directly into an existing
                ecompose.yml's storage.minio block.
  onnxruntime   Installs the onnxruntime shared library used by RAG/embedding
                services (rag domain). On Linux/CTs it is placed at
                /opt/eco-tools/libonnxruntime.so; on macOS via Homebrew.
`);
}

async function runScript(scriptName) {
  const scriptPath = path.join(packageRoot, scriptName);
  await access(scriptPath).catch(() => {
    throw new Error(`Cannot find bundled script: ${scriptPath}`);
  });

  await new Promise((resolve, reject) => {
    const child = spawn("bash", [scriptPath], { stdio: "inherit", env: process.env });

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

export async function runInstall(args) {
  const [tool] = args;

  if (!tool || tool === "help" || tool === "--help" || tool === "-h") {
    installHelp();
    return;
  }

  const scriptName = INSTALLERS[tool];
  if (!scriptName) {
    throw new Error(`Unknown install target: ${tool}\n\nRun "eco install help" for usage.`);
  }

  await runScript(scriptName);
}
