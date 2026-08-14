#!/usr/bin/env node
import { hostname } from "node:os";
import {
  getOrAllocatePort,
  listDbs,
  listPorts,
  listReserved,
  lookupPort,
  pinPort,
  projectHasRegistryRows,
  recordDb,
  releasePort,
  renameProject,
  resetProject,
  seedPort
} from "../lib/registry.js";

function parseArgs(argv) {
  const args = {};
  for (let i = 0; i < argv.length; i++) {
    const token = argv[i];
    if (!token.startsWith("--")) {
      continue;
    }
    const key = token.slice(2);
    const next = argv[i + 1];
    if (next !== undefined && !next.startsWith("--")) {
      args[key] = next;
      i++;
    } else {
      args[key] = true;
    }
  }
  return args;
}

function scopeFor(args) {
  return args.scope || process.env.ECO_REGISTRY_SCOPE || hostname();
}

async function main() {
  const [op, ...rest] = process.argv.slice(2);
  const args = parseArgs(rest);
  const common = { registryPath: args.path, scope: scopeFor(args) };

  switch (op) {
    case "get-or-allocate": {
      const result = await getOrAllocatePort({
        ...common,
        project: args.project,
        service: args.service,
        type: args.type || "service",
        envVar: args["env-var"] || "PORT",
        preferred: args.preferred
      });
      process.stdout.write(String(result.port));
      return;
    }
    case "pin": {
      const result = await pinPort({        ...common,
        project: args.project,
        service: args.service,
        type: args.type || "service",
        envVar: args["env-var"] || "PORT",
        port: Number(args.port)
      });
      process.stdout.write(String(result.port));
      return;
    }
    case "seed": {
      const result = await seedPort({        ...common,
        project: args.project,
        service: args.service,
        type: args.type || "service",
        envVar: args["env-var"] || "PORT",
        port: Number(args.port)
      });
      process.stdout.write(String(result.port));
      return;
    }
    case "lookup": {
      const port = await lookupPort({
        ...common,
        project: args.project,
        service: args.service,
        type: args.type || "service"
      });
      if (port !== null) {
        process.stdout.write(String(port));
      }
      return;
    }
    case "has-project": {
      const present = await projectHasRegistryRows({
        ...common,
        project: args.project
      });
      process.stdout.write(present ? "1" : "0");
      return;
    }
    case "release":
      await releasePort({ ...common, project: args.project, service: args.service });
      return;
    case "reset":
      await resetProject({ ...common, project: args.project });
      return;
    case "rename-project":
      await renameProject({ ...common, from: args.from, to: args.to });
      return;
    case "list": {
      const ports = await listPorts({ ...common, project: args.project });
      process.stdout.write(JSON.stringify(ports, null, 2));
      return;
    }
    case "reserved": {
      const reserved = await listReserved(common);
      process.stdout.write(JSON.stringify(reserved, null, 2));
      return;
    }
    case "record-db":
      await recordDb({
        ...common,
        project: args.project,
        service: args.service,
        dbType: args["db-type"],
        port: Number(args.port),
        dbName: args["db-name"],
        username: args.username,
        password: args.password
      });
      return;
    case "list-dbs": {
      const dbs = await listDbs({
        ...common,
        project: args.project,
        withSecret: args.secret === "1" || args.secret === "true"
      });
      process.stdout.write(JSON.stringify(dbs, null, 2));
      return;
    }
    default:
      throw new Error(
        `Unknown registry op: ${op}\nAvailable: get-or-allocate, seed, lookup, has-project, pin, release, reset, rename-project, list, reserved, record-db, list-dbs`
      );
  }
}

main().catch((error) => {
  process.stderr.write(`registry: ${error instanceof Error ? error.message : String(error)}\n`);
  process.exit(1);
});
