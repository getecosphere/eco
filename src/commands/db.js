import { readFile, stat } from "node:fs/promises";
import { spawn } from "node:child_process";
import path from "node:path";
import { createInterface } from "node:readline/promises";
import { stdin as input, stdout as output } from "node:process";

import { parseCtMetadata, parseProjectName, parseServices, readEcompose } from "../lib/ecompose.js";

function readEnvValue(content, key) {
  const line = content.split(/\r?\n/).find((entry) => entry.startsWith(`${key}=`));
  return line ? line.slice(key.length + 1).trim() : "";
}

function mongoDatabaseName(uri) {
  const withoutQuery = uri.split("?")[0].split("#")[0];
  const database = withoutQuery.slice(withoutQuery.lastIndexOf("/") + 1).trim();
  if (!database || database.includes("/")) {
    throw new Error("MONGODB_URI must include an explicit database name to clear it safely.");
  }
  return decodeURIComponent(database);
}

function postgresDatabaseName(uri) {
  let parsed;
  try {
    parsed = new URL(uri);
  } catch {
    throw new Error("DATABASE_URL must be a valid PostgreSQL connection URL to clear it safely.");
  }
  if (parsed.protocol !== "postgres:" && parsed.protocol !== "postgresql:") {
    throw new Error("DATABASE_URL must use postgres:// or postgresql:// to clear it safely.");
  }
  const database = decodeURIComponent(parsed.pathname.replace(/^\/+/, ""));
  if (!database || database.includes("/")) {
    throw new Error("DATABASE_URL must include exactly one explicit database name to clear it safely.");
  }
  return database;
}

function declaredEngine(service) {
  if (service.runtimes.some((runtime) => runtime.startsWith("mongodb@"))) return "mongo";
  if (service.runtimes.some((runtime) => runtime.startsWith("postgresql@"))) return "postgres";
  return "";
}

async function databaseTarget(service, estateRoot, project) {
  const engine = declaredEngine(service);
  if (!engine) return null;

  const envPath = path.join(estateRoot, service.path, ".env");
  const envContent = await readFile(envPath, "utf8").catch(() => "");
  if (!envContent && engine !== "mongo") {
    return { service, engine, error: `No .env file found at ${envPath}` };
  }

  if (engine === "mongo") {
    const configuredUri = readEnvValue(envContent, "MONGODB_URI") || readEnvValue(envContent, "MONGO_URI");
    // mongodb@ is an Eco-provisioned local runtime. Keep an explicit URI
    // authoritative, but derive the same estate-scoped default that
    // configure.sh writes for legacy domains whose env example lacks a key.
    const uri = configuredUri || `mongodb://localhost:27017/${service.name.replaceAll("-", "_")}_${project}`;
    if (!uri) return { service, engine, error: `MONGODB_URI is not configured in ${envPath}` };
    return { service, engine, uri, database: mongoDatabaseName(uri) };
  }

  const uri = readEnvValue(envContent, "DATABASE_URL") || readEnvValue(envContent, "DB_URL");
  if (!uri) return { service, engine, error: `DATABASE_URL is not configured in ${envPath}` };
  return { service, engine, uri, database: postgresDatabaseName(uri) };
}

async function databaseTargets(deployment) {
  const estateRoot = path.dirname(path.dirname(deployment.filePath));
  const project = parseProjectName(deployment.content);
  return Promise.all(parseServices(deployment.content).map((service) => databaseTarget(service, estateRoot, project)))
    .then((targets) => targets.filter(Boolean));
}

async function confirmSingle(target) {
  const prompt = createInterface({ input, output });
  const databaseType = target.engine === "mongo" ? "MongoDB database" : "PostgreSQL public schema";
  try {
    output.write(`\nWARNING: This permanently clears the ${databaseType} "${target.database}".\n`);
    output.write(`Service: ${target.service.name}\n`);
    if (target.engine === "postgres") output.write("The next eco up will rerun its migrations.\n");
    const confirmation = (await prompt.question(`Type ${target.database} to confirm: `)).trim();
    return confirmation === target.database;
  } finally {
    prompt.close();
  }
}

async function confirmAll(project, targets) {
  const prompt = createInterface({ input, output });
  try {
    output.write(`\nWARNING: This permanently clears every listed database in the ${project} estate:\n`);
    for (const target of targets) output.write(`- ${target.service.name}: ${target.engine === "mongo" ? "MongoDB" : "PostgreSQL"} ${target.database}\n`);
    output.write("PostgreSQL migration history will be removed and recreated on the next eco up.\n");
    const confirmation = (await prompt.question(`Type ${project} to clear every database above: `)).trim();
    return confirmation === project;
  } finally {
    prompt.close();
  }
}

async function runDatabaseCommand(command, args, failureMessage) {
  await new Promise((resolve, reject) => {
    const child = spawn(command, args, { stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => { stdout += chunk; });
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    child.on("error", (error) => reject(new Error(`Unable to run ${command}: ${error.message}`)));
    child.on("exit", (code, signal) => {
      if (signal) reject(new Error(`${command} terminated by signal ${signal}`));
      else if (code !== 0) reject(new Error(`${failureMessage}: ${stderr || stdout}`.trim()));
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

// In production, MongoDB/PostgreSQL are provisioned inside the estate CT.
// The host must never interpret mongodb://localhost as its own database.
async function databaseExecutionContext(deployment) {
  const ct = parseCtMetadata(deployment.content);
  if (!ct.id || !(await commandOnPath("pct"))) return null;
  return { ctid: String(ct.id) };
}

function scopedCommand(context, command, args) {
  return context ? ["pct", ["exec", context.ctid, "--", command, ...args]] : [command, args];
}

async function runScopedDatabaseCommand(context, command, args, failureMessage) {
  const [executable, scopedArgs] = scopedCommand(context, command, args);
  await runDatabaseCommand(executable, scopedArgs, failureMessage);
}

async function postgresClient(context) {
  if (context) {
    await runScopedDatabaseCommand(context, "psql", ["--version"], "psql could not start inside the application CT");
    return "psql";
  }
  const onPath = await new Promise((resolve) => {
    const child = spawn("which", ["psql"], { stdio: ["ignore", "pipe", "ignore"] });
    let stdout = "";
    child.stdout.on("data", (chunk) => { stdout += chunk; });
    child.on("error", () => resolve(""));
    child.on("exit", (code) => resolve(code === 0 ? stdout.trim() : ""));
  });
  if (onPath) return onPath;

  // Postgres.app on macOS does not necessarily add psql to PATH. Keep this
  // identical to Eco's local `up` provisioning detection.
  for (const candidate of [
    "/Applications/Postgres.app/Contents/Versions/15/bin/psql",
    "/Applications/Postgres.app/Contents/Versions/latest/bin/psql"
  ]) {
    try {
      await stat(candidate);
      return candidate;
    } catch {}
  }
  throw new Error("PostgreSQL is declared in ecompose.yml but psql was not found. Run `eco provision` first.");
}

async function mongoShell(context) {
  try {
    await runScopedDatabaseCommand(context, "mongosh", ["--version"], context ? "mongosh could not start inside the application CT" : "mongosh could not start");
    return "mongosh";
  } catch (error) {
    if (context) {
      throw new Error(`${error.message}\nRun eco up so MongoDB and mongosh are provisioned in CT ${context.ctid}, then rerun the command.`);
    }
    throw new Error(
      `${error.message}\nRepair the local MongoDB shell, then rerun the command. On Homebrew: brew reinstall mongosh`
    );
  }
}

async function clearTarget(target, context) {
  if (target.engine === "mongo") {
    const mongosh = await mongoShell(context);
    await runScopedDatabaseCommand(context, mongosh, [target.uri, "--quiet", "--eval", "db.dropDatabase()"], "mongosh failed to drop the database");
    output.write(`Dropped MongoDB database "${target.database}" for ${target.service.name}${context ? ` via CT ${context.ctid}` : ""}.\n`);
    return;
  }
  const psql = await postgresClient(context);
  await runScopedDatabaseCommand(context, psql, [target.uri, "-v", "ON_ERROR_STOP=1", "-c", "DROP SCHEMA public CASCADE; CREATE SCHEMA public;"], "psql failed to reset the public schema");
  output.write(`Cleared PostgreSQL public schema in database "${target.database}" for ${target.service.name}${context ? ` via CT ${context.ctid}` : ""}.\n`);
}

function dbHelp() {
  output.write(`eco db\n\nUsage:\n  eco db\n  eco db clear all\n  eco db clear <service>\n\nExamples:\n  eco db\n  eco db clear auth-backend\n  eco db clear backend\n  eco db clear all\n\nThe command detects MongoDB or PostgreSQL from ecompose.yml. PostgreSQL clears\nthe public schema so the next eco up can rerun migrations. On a Proxmox host,\nclear commands run through the estate's ct.id with pct exec; they never use the\nhost's localhost database. It never edits .env files.\n`);
}

export async function runDb(args) {
  const [action, targetName, ...extra] = args;
  if (action === "help" || action === "--help" || action === "-h") {
    dbHelp();
    return;
  }

  const deployment = await readEcompose(".", process.cwd());
  const targets = await databaseTargets(deployment);
  const context = await databaseExecutionContext(deployment);
  if (!action) {
    if (targets.length === 0) {
      output.write("No database-backed services are declared in this estate.\n");
      return;
    }
    output.write("Clearable databases:\n");
    if (context) output.write(`Database commands will run inside application CT ${context.ctid}.\n`);
    for (const target of targets) {
      const type = target.engine === "mongo" ? "MongoDB" : "PostgreSQL";
      output.write(target.error
        ? `- ${target.service.name} (${type}): unavailable — ${target.error}\n`
        : `- ${target.service.name} (${type}): ${target.database}\n`);
    }
    output.write("\nUse: eco db clear <service>  or  eco db clear all\n");
    return;
  }

  if (action !== "clear" || !targetName || extra.length > 0) {
    throw new Error("Usage: eco db clear <service|all>");
  }

  if (targetName === "all") {
    const clearable = targets.filter((target) => !target.error);
    if (clearable.length === 0) throw new Error("No configured databases are available to clear. Run eco up first.");
    // Check the local client before accepting confirmation, so an estate with
    // multiple databases cannot be partially cleared merely because psql is
    // unavailable later in the sequence.
    if (clearable.some((target) => target.engine === "mongo")) await mongoShell(context);
    if (clearable.some((target) => target.engine === "postgres")) await postgresClient(context);
    const project = parseProjectName(deployment.content);
    if (!(await confirmAll(project, clearable))) {
      output.write("Cancelled. No database was changed.\n");
      return;
    }
    for (const target of clearable) await clearTarget(target, context);
    return;
  }

  const target = targets.find((entry) => entry.service.name === targetName);
  if (!target) throw new Error(`Service "${targetName}" has no declared MongoDB or PostgreSQL database in ${deployment.filePath}. Run eco db to list available services.`);
  if (target.error) throw new Error(`${target.service.name} cannot be cleared: ${target.error}`);
  if (target.engine === "mongo") await mongoShell(context);
  if (target.engine === "postgres") await postgresClient(context);
  if (!(await confirmSingle(target))) {
    output.write("Cancelled. No database was changed.\n");
    return;
  }
  await clearTarget(target, context);
}
