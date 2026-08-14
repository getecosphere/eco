import { access } from "node:fs/promises";
import path from "node:path";

async function pathExists(targetPath) {
  try {
    await access(targetPath);
    return true;
  } catch {
    return false;
  }
}

export async function findWorkspaceRoot(startDir) {
  let current = path.resolve(startDir);

  while (true) {
    const ecoDir = path.join(current, "eco");
    const coreDir = path.join(current, "core");
    if ((await pathExists(ecoDir)) || (await pathExists(coreDir))) {
      return current;
    }

    const parent = path.dirname(current);
    if (parent === current) {
      break;
    }

    current = parent;
  }

  throw new Error(
    "Could not find a workspace root containing eco/ or core/. Run this command from inside a SuperApp workspace."
  );
}

export async function findEstateRoot(startDir) {
  const workspaceRoot = await findWorkspaceRoot(startDir);
  const resolvedStart = path.resolve(startDir);
  const relative = path.relative(workspaceRoot, resolvedStart);

  if (!relative || relative === "") {
    return workspaceRoot;
  }

  const [firstSegment] = relative.split(path.sep);
  if (!firstSegment || firstSegment === "." || firstSegment === "eco" || firstSegment === "core") {
    return workspaceRoot;
  }

  return path.join(workspaceRoot, firstSegment);
}
