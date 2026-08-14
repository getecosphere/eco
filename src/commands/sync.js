import { accessSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { spawn } from "node:child_process";
import path from "node:path";

import { parseCtMetadata, parseProjectName, parseServices, parseStaging, readEcompose } from "../lib/ecompose.js";

function readEnvValue(content, key) {
  const line = content.split(/\r?\n/).find((entry) => entry.startsWith(`${key}=`));
  return line ? line.slice(key.length + 1).trim() : "";
}

function declaredEngine(service) {
  if (service.runtimes.some((runtime) => runtime.startsWith("mongodb@"))) return "mongo";
  if (service.runtimes.includes("postgresql@15")) return "postgres";
  return "";
}

function mongoDatabaseName(uri) {
  const withoutQuery = uri.split("?")[0].split("#")[0];
  const database = withoutQuery.slice(withoutQuery.lastIndexOf("/") + 1).trim();
  return decodeURIComponent(database);
}

// Extracts the database name from either a JDBC URL
// (jdbc:postgresql://localhost:5432/assessment) or a libpq URL
// (postgresql://role:pass@127.0.0.1:5432/assessment).
function postgresDatabaseName(uri) {
  const stripped = String(uri || "")
    .replace(/^jdbc:/, "")
    .split("?")[0]
    .split("#")[0];
  const pathPart = stripped.slice(stripped.lastIndexOf("/") + 1);
  return pathPart || "";
}

// Builds the connection details for a postgres service from its .env
// contents. The role and password are Eco-managed (DATABASE_USERNAME /
// DATABASE_PASSWORD); the database comes from DATABASE_URL / DB_URL.
function postgresConnection(envContents) {
  const url = readEnvValue(envContents, "DATABASE_URL") || readEnvValue(envContents, "DB_URL");
  const database = postgresDatabaseName(url);
  const username = readEnvValue(envContents, "DATABASE_USERNAME") || "postgres";
  const password = readEnvValue(envContents, "DATABASE_PASSWORD") || "";
  return { database, username, password, url };
}

// Resolves each DB-backed service's connection details.
// `remoteEnvResolver(servicePath)` returns the service's .env contents from
// the source CT, or null to fall back to local. Mongo consults it only in
// --staging mode (local dev uses dev DB names from .env.example); postgres
// always prefers the source CT's .env because prod and dev share the same
// database name and role.
async function databaseTargets(deployment, { remoteEnvResolver = null, useRemoteForMongo = false } = {}) {
  const estateRoot = path.dirname(path.dirname(deployment.filePath));
  const project = parseProjectName(deployment.content);
  const services = parseServices(deployment.content);
  const targets = [];

  for (const service of services) {
    const engine = declaredEngine(service);
    if (!engine) continue;

    let remoteEnv = "";
    if (remoteEnvResolver && (engine !== "mongo" || useRemoteForMongo)) {
      remoteEnv = await remoteEnvResolver(service.path);
    }

    if (engine === "mongo") {
      let configuredUri = "";
      if (remoteEnv) {
        configuredUri = readEnvValue(remoteEnv, "MONGODB_URI") || readEnvValue(remoteEnv, "MONGO_URI");
      }
      if (!configuredUri) {
        const envExamplePath = path.join(estateRoot, service.path, ".env.example");
        try {
          const envExample = await readFile(envExamplePath, "utf8");
          configuredUri = readEnvValue(envExample, "MONGODB_URI") || readEnvValue(envExample, "MONGO_URI");
        } catch {}
      }
      const dbName = service.name.replaceAll("-", "_") + "_" + project;
      const uri = configuredUri || `mongodb://localhost:27017/${dbName}`;
      const database = mongoDatabaseName(uri);
      targets.push({ service, engine, database, uri });
      continue;
    }

    // postgres
    let conn;
    // Postgres database names and credentials are the same in prod and dev
    // (eco provisions <db> with role <project>_user identically), so the
    // source CT's real .env is authoritative in both local and --staging
    // modes. Fall back to local .env.example only when the remote file is
    // absent (e.g. never-deployed estate).
    if (remoteEnv) {
      conn = postgresConnection(remoteEnv);
    } else {
      const envExamplePath = path.join(estateRoot, service.path, ".env.example");
      let envContents = "";
      try {
        envContents = await readFile(envExamplePath, "utf8");
      } catch {}
      conn = postgresConnection(envContents);
      if (!conn.database) {
        conn.database = service.name.replaceAll("-", "_") + "_" + project;
      }
    }
    if (!conn.database) {
      conn.database = service.name.replaceAll("-", "_") + "_" + project;
    }
    targets.push({ service, engine, ...conn });
  }

  return targets;
}

// Normalizes a service path for the CT layout. New composed estates place the
// ecompose.yml at <estate>/<project>_bootstrap and services under
// <estate>/<project>_bootstrap/<domain>/...; the CT extraction roots the
// estate at /opt/projects/<project>. Legacy estates put ecompose.yml at the
// estate root with service paths like `assessment/backend`, which resolves to
// /opt/projects/<project>/assessment/backend even though the checkout is at
// /opt/projects/<project>/backend. Mirrors up.js's relativeCtServicePath.
function relativeServiceDir(servicePath, project) {
  const segments = String(servicePath || "").split("/").filter(Boolean);
  if (segments.length > 0 && segments[0] === project) {
    segments.shift();
  }
  return segments.join("/");
}

// Reads a service's .env from a CT on the Proxmox host (used so database
// names come from the source CT, matching what its services actually connect
// to). Returns null when the file is absent.
function makeRemoteEnvResolver({ sshHost, ctid, ctProjectRoot, project }) {
  return async function remoteEnvResolver(servicePath) {
    const rel = relativeServiceDir(servicePath, project);
    const envPath = `${ctProjectRoot}/${rel}/.env`;
    try {
      await runCommand("ssh", [sshHost, `pct exec ${ctid} -- test -f ${envPath}`], { stdout: "ignore", stderr: "ignore" });
    } catch {
      return null;
    }
    return new Promise((resolve) => {
      const child = spawn("ssh", [sshHost, `pct exec ${ctid} -- cat ${envPath}`], { stdio: ["ignore", "pipe", "ignore"] });
      let out = "";
      child.stdout.on("data", (d) => { out += d.toString(); });
      child.on("error", () => resolve(null));
      child.on("exit", (code) => resolve(code === 0 ? out : null));
    });
  };
}

async function runCommand(command, args, { stdin = "ignore", stdout = "inherit", stderr = "inherit" } = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { stdio: [stdin, stdout, stderr] });
    child.on("error", (error) => reject(new Error(`Unable to run ${command}: ${error.message}`)));
    child.on("exit", (code, signal) => {
      if (signal) reject(new Error(`${command} terminated by signal ${signal}`));
      else if (code !== 0) reject(new Error(`${command} exited with code ${code}`));
      else resolve();
    });
  });
}

async function commandOnPath(command) {
  return new Promise((resolve) => {
    const child = spawn("which", [command], { stdio: ["ignore", "ignore", "ignore"] });
    child.on("error", () => resolve(false));
    child.on("exit", (code) => resolve(code === 0));
  });
}

// Postgres client tools often live in libpq's bin dir (brew) without being on
// PATH. Return that directory when it holds the binary, else "".
function libpqBinDir(command) {
  for (const dir of ["/usr/local/opt/libpq/bin", "/opt/homebrew/opt/libpq/bin"]) {
    try {
      accessSync(`${dir}/${command}`);
      return dir;
    } catch {}
  }
  return "";
}

// Shell-escapes a value for embedding inside a double-quoted ssh command.
function shellQuote(value) {
  return String(value).replace(/\\/g, "\\\\").replace(/"/g, '\\"').replace(/\$/g, "\\$").replace(/`/g, "\\`");
}

function syncHelp() {
  process.stdout.write(`eco sync

Usage:
  eco sync [options]

Sync production database data from the estate's application CT to the local
development machine. Reads ecompose.yml to discover every MongoDB- and
PostgreSQL-backed service, then for each one:

  MongoDB:
    ssh <host> "pct exec <ctid> -- mongodump --db=<database> --archive" | \\
      mongorestore --archive --drop

  PostgreSQL:
    ssh <host> "pct exec <ctid> -- pg_dump ... -d <database>" | \\
      pg_restore --clean --if-exists

With --staging the data is synced prod-CT -> staging-CT instead (both on the
Proxmox host, streaming CT-to-CT through pct, so no local restore tool is
needed). The destination is the staging.ct declared in ecompose.yml.

Options:
  --host <hostname>   SSH host for the Proxmox host (default: prox)
  --ct <ctid>         CT ID to sync from (reads ecompose.yml ct.id by default)
  --staging           Sync prod CT -> staging CT (staging.ct from ecompose.yml)
  --service <name>    Sync only this service (default: all DB-backed services)
  --skip-ssh-check    Skip the SSH reachability pre-flight check
  --dry-run           Print commands without executing them

Examples:
  eco sync
  eco sync --staging
  eco sync --host prox-eko --service marketplace-backend
  eco sync --service assessment-backend   # a PostgreSQL-backed service
  eco sync --dry-run
`);
}

export async function runSync(args) {
  if (args[0] === "help" || args[0] === "--help" || args[0] === "-h") {
    syncHelp();
    return;
  }

  const options = {};
  const positionals = [];

  for (let i = 0; i < args.length; i += 1) {
    const arg = args[i];
    if (!arg.startsWith("--")) {
      positionals.push(arg);
      continue;
    }
    const key = arg.slice(2);
    if (key === "dry-run" || key === "skip-ssh-check" || key === "staging") {
      options[key] = true;
      continue;
    }
    const value = args[i + 1];
    if (!value || value.startsWith("--")) throw new Error(`Missing value for option --${key}`);
    options[key] = value;
    i += 1;
  }

  if (positionals.length > 0) throw new Error(`Unexpected positional argument(s): ${positionals.join(" ")}`);

  const deployment = await readEcompose(".", process.cwd());
  const ctMeta = parseCtMetadata(deployment.content);
  const ctid = options.ct || ctMeta.id || "101";
  const sshHost = options.host || "prox";
  const dryRun = options["dry-run"] || false;
  const skipSshCheck = options["skip-ssh-check"] || false;
  const toStaging = options.staging || false;
  const ctProjectRoot = `/opt/projects/${parseProjectName(deployment.content)}`;

  let targets = await databaseTargets(deployment, {
    // The source CT's real .env is authoritative for postgres in both local
    // and --staging syncs (DB names/roles match prod); mongo uses the remote
    // env only under --staging, keeping local .env.example dev names otherwise.
    remoteEnvResolver: makeRemoteEnvResolver({ sshHost, ctid, ctProjectRoot, project: parseProjectName(deployment.content) }),
    useRemoteForMongo: toStaging
  });
  if (options.service) {
    targets = targets.filter((t) => t.service.name === options.service);
    if (targets.length === 0) throw new Error(`Service "${options.service}" has no database in this estate. Run eco db to list available services.`);
  }
  if (targets.length === 0) {
    process.stdout.write("No DB-backed services are declared in this estate.\n");
    return;
  }

  // Resolve the staging destination CT when requested. It must be declared in
  // ecompose.yml and differ from the prod ct.id.
  let stagingCtid = "";
  if (toStaging) {
    const staging = parseStaging(deployment.content);
    stagingCtid = staging.ct ? String(staging.ct) : "";
    if (!stagingCtid) {
      throw new Error("--staging requested but ecompose.yml has no staging.ct declared. Add a staging: block (staging.ct: 1000).");
    }
    if (stagingCtid === String(ctid)) {
      throw new Error(`--staging destination CT ${stagingCtid} must differ from the prod ct.id.`);
    }
  }

  if (toStaging) {
    // CT-to-CT stream stays entirely on the Proxmox host (fast private
    // bridge), so restore tools are only needed inside the CTs.
    if (!(await commandOnPath("ssh"))) {
      throw new Error("ssh is not available locally.");
    }
  } else if (!dryRun) {
    const needsMongoRestore = targets.some((t) => t.engine === "mongo");
    const needsPgRestore = targets.some((t) => t.engine === "postgres");
    if (needsMongoRestore && !(await commandOnPath("mongorestore"))) {
      throw new Error("mongorestore is not installed locally. Install MongoDB Database Tools (brew install mongodb-database-tools).");
    }
    if (needsPgRestore && !(await commandOnPath("pg_restore")) && !libpqBinDir("pg_restore")) {
      throw new Error("pg_restore is not installed locally. Install PostgreSQL client tools (brew install libpq).");
    }
  }

  if (!skipSshCheck) {
    process.stdout.write(`Checking SSH to ${sshHost}… `);
    try {
      await runCommand("ssh", [sshHost, "echo", "ok"], { stdout: "ignore", stderr: "ignore" });
    } catch {
      throw new Error(`Cannot reach ${sshHost} via SSH. Check the hostname or use --skip-ssh-check to skip.`);
    }
    process.stdout.write("ok\n");
  }

  // Pre-flight: verify the dump tool exists on the source CT (and the restore
  // tool on the staging CT when syncing CT-to-CT).
  if (!dryRun) {
    const dumpTools = [...new Set(targets.map((t) => (t.engine === "mongo" ? "mongodump" : "pg_dump")))];
    for (const tool of dumpTools) {
      try {
        await runCommand("ssh", [sshHost, `pct exec ${ctid} -- which ${tool}`], { stdout: "ignore", stderr: "ignore" });
      } catch {
        throw new Error(`${tool} is not installed in CT ${ctid}. Run "apt-get install -y ${tool === "mongodump" ? "mongodb-database-tools" : "postgresql-client"}" in the CT first.`);
      }
    }
    if (toStaging) {
      const restoreTools = [...new Set(targets.map((t) => (t.engine === "mongo" ? "mongorestore" : "pg_restore")))];
      for (const tool of restoreTools) {
        try {
          await runCommand("ssh", [sshHost, `pct exec ${stagingCtid} -- which ${tool}`], { stdout: "ignore", stderr: "ignore" });
        } catch {
          throw new Error(`${tool} is not installed in staging CT ${stagingCtid}. Run "apt-get install -y ${tool === "mongorestore" ? "mongodb-database-tools" : "postgresql-client"}" in the CT first.`);
        }
      }
    }
  }

  const project = parseProjectName(deployment.content);
  const mongoCount = targets.filter((t) => t.engine === "mongo").length;
  const pgCount = targets.filter((t) => t.engine === "postgres").length;
  const kindLabel = [`${mongoCount} MongoDB`, pgCount ? `${pgCount} PostgreSQL` : ""].filter(Boolean).join(" + ");
  if (toStaging) {
    process.stdout.write(`\nSyncing ${kindLabel} database(s) from "${project}" CT ${ctid} to staging CT ${stagingCtid} (${sshHost}):\n\n`);
  } else {
    process.stdout.write(`\nSyncing ${kindLabel} database(s) from "${project}" CT ${ctid} (${sshHost}):\n\n`);
  }

  let failed = 0;

  for (const target of targets) {
    let fullPipeline;
    if (target.engine === "mongo") {
      if (toStaging) {
        fullPipeline = `ssh ${sshHost} "pct exec ${ctid} -- mongodump --db=${shellQuote(target.database)} --archive | pct exec ${stagingCtid} -- mongorestore --archive --drop"`;
      } else {
        fullPipeline = `ssh ${sshHost} "pct exec ${ctid} -- mongodump --db=${shellQuote(target.database)} --archive" | mongorestore --archive --drop`;
      }
    } else {
      const dumpCmd = `PGPASSWORD="${shellQuote(target.password)}" pg_dump -h 127.0.0.1 -U ${shellQuote(target.username)} -d ${shellQuote(target.database)} --format=custom --no-owner`;
      if (toStaging) {
        fullPipeline = `ssh ${sshHost} "pct exec ${ctid} -- bash -lc ${JSON.stringify(dumpCmd)} | pct exec ${stagingCtid} -- bash -lc ${JSON.stringify(`PGPASSWORD="${shellQuote(target.password)}" pg_restore -h 127.0.0.1 -U ${shellQuote(target.username)} -d ${shellQuote(target.database)} --clean --if-exists --no-owner --no-acl`)}"`;
      } else {
        fullPipeline = `ssh ${sshHost} "pct exec ${ctid} -- bash -lc ${JSON.stringify(dumpCmd)}" | PGPASSWORD="" pg_restore --clean --if-exists --no-owner --no-acl -d ${shellQuote(target.database)}`;
      }
    }

    process.stdout.write(`  ${target.service.name} (${target.database}) [${target.engine}]`);

    if (dryRun) {
      // Redact passwords from the printed plan.
      const redacted = target.password
        ? fullPipeline.replaceAll(target.password, "********")
        : fullPipeline;
      process.stdout.write(`\n    ${redacted}\n`);
      continue;
    }

    try {
      if (toStaging) {
        // One SSH hop, pipeline assembled remotely: dump in prod CT pipes
        // straight into restore in the staging CT.
        await new Promise((resolve, reject) => {
          const sshProc = spawn("ssh", [sshHost, `pct exec ${ctid} -- bash -lc ${JSON.stringify(target.engine === "mongo"
            ? `mongodump --db=${shellQuote(target.database)} --archive`
            : `PGPASSWORD="${shellQuote(target.password)}" pg_dump -h 127.0.0.1 -U ${shellQuote(target.username)} -d ${shellQuote(target.database)} --format=custom --no-owner`)} | pct exec ${stagingCtid} -- bash -lc ${JSON.stringify(target.engine === "mongo"
            ? `mongorestore --archive --drop`
            : `PGPASSWORD="${shellQuote(target.password)}" pg_restore -h 127.0.0.1 -U ${shellQuote(target.username)} -d ${shellQuote(target.database)} --clean --if-exists --no-owner --no-acl`)}`], {
            stdio: ["ignore", "inherit", "inherit"],
          });
          sshProc.on("error", (err) => reject(new Error(`ssh error: ${err.message}`)));
          sshProc.on("exit", (code) => {
            if (code !== 0) reject(new Error(`ssh exited with code ${code}`));
            else resolve();
          });
        });
      } else if (target.engine === "mongo") {
        // Local mongorestore: ssh -> mongodump pipes to mongorestore on this machine.
        await new Promise((resolve, reject) => {
          const sshProc = spawn("ssh", [sshHost, `pct exec ${ctid} -- mongodump --db=${shellQuote(target.database)} --archive`], {
            stdio: ["ignore", "pipe", "inherit"],
          });
          const restoreProc = spawn("mongorestore", ["--archive", "--drop"], {
            stdio: ["pipe", "inherit", "inherit"],
          });

          sshProc.stdout.pipe(restoreProc.stdin);

          let sshDone = false;
          let restoreDone = false;
          let sshError = null;
          let restoreError = null;

          const maybeResolve = () => {
            if (sshDone && restoreDone) {
              if (sshError) reject(sshError);
              else if (restoreError) reject(restoreError);
              else resolve();
            }
          };

          sshProc.on("error", (err) => { sshError = err; });
          sshProc.on("exit", (code) => {
            sshDone = true;
            if (code !== 0 && !sshError) sshError = new Error(`ssh exited with code ${code}`);
            if (restoreDone) maybeResolve();
          });

          restoreProc.on("error", (err) => { restoreError = err; });
          restoreProc.on("exit", (code) => {
            restoreDone = true;
            if (code !== 0 && !restoreError) restoreError = new Error(`mongorestore exited with code ${code}`);
            if (sshDone) maybeResolve();
          });
        });
      } else {
        // Local pg_restore: ssh -> pg_dump pipes to pg_restore on this machine.
        // libpq's bin dir may not be on PATH (brew); use it when present.
        const pgBinDir = (await commandOnPath("pg_restore")) ? "" : libpqBinDir("pg_restore");
        await new Promise((resolve, reject) => {
          const dumpCmd = `PGPASSWORD="${shellQuote(target.password)}" pg_dump -h 127.0.0.1 -U ${shellQuote(target.username)} -d ${shellQuote(target.database)} --format=custom --no-owner`;
          const sshProc = spawn("ssh", [sshHost, `pct exec ${ctid} -- bash -lc ${JSON.stringify(dumpCmd)}`], {
            stdio: ["ignore", "pipe", "inherit"],
          });
          const restoreEnv = pgBinDir ? { ...process.env, PATH: `${pgBinDir}:${process.env.PATH || ""}` } : process.env;
          const restoreProc = spawn("pg_restore", ["--clean", "--if-exists", "--no-owner", "--no-acl", "-d", target.database], {
            stdio: ["pipe", "inherit", "inherit"],
            env: restoreEnv,
          });

          sshProc.stdout.pipe(restoreProc.stdin);

          let sshDone = false;
          let restoreDone = false;
          let sshError = null;
          let restoreError = null;

          const maybeResolve = () => {
            if (sshDone && restoreDone) {
              if (sshError) reject(sshError);
              else if (restoreError) reject(restoreError);
              else resolve();
            }
          };

          sshProc.on("error", (err) => { sshError = err; });
          sshProc.on("exit", (code) => {
            sshDone = true;
            if (code !== 0 && !sshError) sshError = new Error(`ssh exited with code ${code}`);
            if (restoreDone) maybeResolve();
          });

          restoreProc.on("error", (err) => { restoreError = err; });
          restoreProc.on("exit", (code) => {
            restoreDone = true;
            if (code !== 0 && !restoreError) restoreError = new Error(`pg_restore exited with code ${code}`);
            if (sshDone) maybeResolve();
          });
        });
      }
      process.stdout.write(" ✓\n");
    } catch (err) {
      process.stdout.write(` ✗ (${err.message})\n`);
      failed += 1;
    }
  }

  process.stdout.write(`\nDone. ${targets.length - failed} of ${targets.length} databases synced.\n`);
  if (failed > 0) {
    process.exitCode = 1;
  }
}

// `eco sync-staging` is the prod->staging variant of `eco sync`. It delegates
// to the same runner with the --staging flag forced on.
export async function runSyncStaging(args) {
  await runSync(["--staging", ...args]);
}
