import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const packageRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");

export async function readRepoCatalog() {
  const reposPath = path.join(packageRoot, "repos.json");
  const content = await readFile(reposPath, "utf8");
  const parsed = JSON.parse(content);
  return parsed.subsystems || [];
}

export async function findRepoByName(name) {
  const subsystems = await readRepoCatalog();
  return subsystems.find((entry) => entry.name === name) || null;
}
