import { hostname } from "node:os";
import {
  defaultRegistryPath,
  listDbs,
  listPorts,
  listReserved,
  pinPort,
  releasePort,
  resetProject,
  seedPort
} from "../lib/registry.js";

const output = process.stdout;
const color = output.isTTY && !process.env.NO_COLOR;
const bold = (s) => (color ? `\x1b[1m${s}\x1b[0m` : s);
const dim = (s) => (color ? `\x1b[2m${s}\x1b[0m` : s);
const cyan = (s) => (color ? `\x1b[36m${s}\x1b[0m` : s);
const yellow = (s) => (color ? `\x1b[33m${s}\x1b[0m` : s);

function parseArgs(args) {
  const positionals = [];
  const options = {};
  for (let i = 0; i < args.length; i++) {
    const token = args[i];
    if (token.startsWith("--")) {
      const eq = token.indexOf("=");
      if (eq !== -1) {
        options[token.slice(2, eq)] = token.slice(eq + 1);
        continue;
      }
      const next = args[i + 1];
      if (next !== undefined && !next.startsWith("--")) {
        options[token.slice(2)] = next;
        i++;
      } else {
        options[token.slice(2)] = true;
      }
    } else {
      positionals.push(token);
    }
  }
  return { positionals, options };
}

function scopeFor(options) {
  return options.scope || process.env.ECO_REGISTRY_SCOPE || hostname();
}

function portsHelp() {
  output.write(`eco ports\n\nUsage:\n  eco ports list [--project X] [--scope S] [--path P]\n  eco ports pin <service> <port> [--type service|gateway|index] [--env-var PORT]\n  eco ports release <service>\n  eco ports reset [--project X]\n  eco ports reserved\n  eco ports dbs [--project X] [--secret]\n\nOptions:\n  --scope S   registry scope (default: hostname)\n  --path P    registry database path (default: ~/.eco/registry.db or /etc/eco/registry.db)\n\nPorts are assigned once and never change. The registry is the durable\nrecord; .env and ecosystem.config.js are renders of it.\n`);
}

export async function runPorts(args) {
  const { positionals, options } = parseArgs(args);
  const [action, ...rest] = positionals;
  const path = options.path || defaultRegistryPath();
  const scope = scopeFor(options);

  if (!action || action === "help" || action === "--help" || action === "-h") {
    portsHelp();
    return;
  }

  if (action === "list") {
    const rows = await listPorts({ registryPath: path, scope, project: options.project });
    const reserved = await listReserved({ registryPath: path, scope });
    const used = new Set(rows.map((r) => r.port));
    const reservedRows = reserved.filter((r) => !used.has(r.port));

    output.write(`\n${bold("Registry")}  ${dim(path)}\n`);
    output.write(`  ${cyan("scope")}  ${scope}\n\n`);

    if (reservedRows.length > 0) {
      output.write(`${bold("Reserved")}\n`);
      for (const r of reservedRows) {
        output.write(`  ${yellow(String(r.port).padEnd(6))}  ${r.label}\n`);
      }
      output.write("\n");
    }

    if (rows.length === 0) {
      output.write("No port allocations yet.\n");
      return;
    }

    output.write(`${bold("Allocations")}\n`);
    const sorted = [...rows].sort((a, b) => a.port - b.port);
    for (const row of sorted) {
      const label = row.type === "service" ? `${row.project}/${row.service}` : `${row.project}/${row.service} (${row.type})`;
      output.write(`  ${cyan(String(row.port).padEnd(6))}  ${label.padEnd(32)}  ${dim(row.env_var)}${row.project === options.project ? "" : ""}\n`);
    }
    return;
  }

  if (action === "reserved") {
    const rows = await listReserved({ registryPath: path, scope });
    output.write(`\n${bold("Reserved ports")}  ${dim(path)} (scope: ${scope})\n`);
    for (const r of rows) {
      output.write(`  ${yellow(String(r.port).padEnd(6))}  ${r.label}\n`);
    }
    return;
  }

  if (action === "pin") {
    const [service, portInput] = rest;
    if (!service || !portInput) {
      throw new Error("Usage: eco ports pin <service> <port> [--type service] [--env-var PORT]");
    }
    const port = Number(portInput);
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
      throw new Error(`Invalid port: ${portInput}`);
    }
    const project = options.project;
    if (!project) {
      throw new Error("eco ports pin requires --project <name>");
    }
    await pinPort({
      registryPath: path,
      scope,
      project,
      service,
      type: options.type || "service",
      envVar: options["env-var"] || "PORT",
      port
    });
    output.write(`Pinned ${project}/${service} to port ${port} (${dim(path)}).\n`);
    return;
  }

  if (action === "seed") {
    const [service, portInput] = rest;
    if (!service || !portInput) {
      throw new Error("Usage: eco ports seed <service> <port> [--type service] [--env-var PORT]");
    }
    const port = Number(portInput);
    const project = options.project;
    if (!project) {
      throw new Error("eco ports seed requires --project <name>");
    }
    await seedPort({
      registryPath: path,
      scope,
      project,
      service,
      type: options.type || "service",
      envVar: options["env-var"] || "PORT",
      port
    });
    output.write(`Seeded ${project}/${service} to port ${port} (${dim(path)}).\n`);
    return;
  }

  if (action === "release") {
    const [service] = rest;
    if (!service) {
      throw new Error("Usage: eco ports release <service> [--project X]");
    }
    const project = options.project;
    if (!project) {
      throw new Error("eco ports release requires --project <name>");
    }
    await releasePort({ registryPath: path, scope, project, service });
    output.write(`Released ${project}/${service} (${dim(path)}). Next eco up will allocate a new one-time port.\n`);
    return;
  }

  if (action === "reset") {
    const project = options.project;
    if (!project) {
      throw new Error("eco ports reset requires --project <name>");
    }
    await resetProject({ registryPath: path, scope, project });
    output.write(`Reset all allocations for ${project} (${dim(path)}). Next eco up will reallocate.\n`);
    return;
  }

  if (action === "dbs") {
    const rows = await listDbs({
      registryPath: path,
      scope,
      project: options.project,
      withSecret: options.secret === "1" || options.secret === "true"
    });
    output.write(`\n${bold("Databases")}  ${dim(path)} (scope: ${scope})\n`);
    if (rows.length === 0) {
      output.write("No managed databases recorded.\n");
      return;
    }
    for (const row of rows) {
      const ident = `${row.project}/${row.service}`;
      const dbPart = row.db_name ? ` db=${row.db_name}` : "";
      const userPart = row.username ? ` user=${row.username}` : "";
      const secretPart = row.password ? ` password=${row.password}` : "";
      output.write(`  ${cyan(String(row.port).padEnd(6))}  ${row.db_type.padEnd(8)}  ${ident}${dbPart}${userPart}${secretPart}\n`);
    }
    return;
  }

  throw new Error(`Unknown action: ${action}\n\nRun "eco ports" for usage.`);
}
