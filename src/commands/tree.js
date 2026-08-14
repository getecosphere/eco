import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import path from "node:path";

const scriptPath = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "../../tree.sh"
);

export async function runTree(args) {
  await new Promise((resolve, reject) => {
    const child = spawn("bash", [scriptPath, ...args], { stdio: "inherit" });
    child.on("error", reject);
    child.on("exit", (code) => (code === 0 ? resolve() : reject(new Error(`tree exited with code ${code}`))));
  });
}
