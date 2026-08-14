import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { copyFile, mkdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { homedir, hostname } from "node:os";
import { dirname, join } from "node:path";
import crypto from "node:crypto";

import initSqlJs from "sql.js";

const require = createRequire(import.meta.url);
const SQL_WASM_PATH = require.resolve("sql.js/dist/sql-wasm.wasm");

const DEFAULT_RANGES = {
  service: [20000, 27999],
  gateway: [20000, 27999],
  index: [20000, 27999]
};

const DEFAULT_RESERVED = [
  [27017, "mongod (application databases)"],
  [5432, "postgres (application databases)"],
  [6379, "redis (application runtime)"]
];

let sqlPromise = null;
async function getSql() {
  if (!sqlPromise) {
    sqlPromise = initSqlJs({ locateFile: () => SQL_WASM_PATH });
  }
  return sqlPromise;
}

export function defaultRegistryPath() {
  if (process.env.ECO_REGISTRY_PATH) {
    return process.env.ECO_REGISTRY_PATH;
  }
  const base = process.platform === "linux" && process.getuid?.() === 0
    ? "/etc/eco"
    : join(homedir(), ".eco");
  return join(base, "registry.db");
}

function keyPathFor(registryPath) {
  return `${registryPath}.key`;
}

async function loadKey(registryPath) {
  const keyPath = keyPathFor(registryPath);
  if (existsSync(keyPath)) {
    return Buffer.from((await readFile(keyPath, "utf8")).trim(), "hex");
  }
  const key = crypto.randomBytes(32);
  await mkdir(dirname(keyPath), { recursive: true });
  await writeFile(keyPath, key.toString("hex") + "\n", { mode: 0o600 });
  return key;
}

export function encryptSecret(key, plaintext) {
  const iv = crypto.randomBytes(12);
  const cipher = crypto.createCipheriv("aes-256-gcm", key, iv);
  const ciphertext = Buffer.concat([cipher.update(plaintext, "utf8"), cipher.final()]);
  const tag = cipher.getAuthTag();
  return Buffer.concat([iv, tag, ciphertext]).toString("base64");
}

export function decryptSecret(key, encoded) {
  const buffer = Buffer.from(encoded, "base64");
  const iv = buffer.subarray(0, 12);
  const tag = buffer.subarray(12, 28);
  const ciphertext = buffer.subarray(28);
  const decipher = crypto.createDecipheriv("aes-256-gcm", key, iv);
  decipher.setAuthTag(tag);
  return Buffer.concat([decipher.update(ciphertext), decipher.final()]).toString("utf8");
}

function portInUse(port) {
  try {
    if (process.platform === "linux") {
      const out = execFileSync("ss", ["-ltnH", `sport = :${port}`], { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] });
      return out.trim().length > 0;
    }
    execFileSync("lsof", ["-nP", "-iTCP:" + port, "-sTCP:LISTEN"], { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] });
    return true;
  } catch {
    return false;
  }
}

function migrate(db) {
  db.exec(`
    CREATE TABLE IF NOT EXISTS reserved_ports (
      scope TEXT NOT NULL,
      port INTEGER NOT NULL,
      label TEXT NOT NULL,
      PRIMARY KEY (scope, port)
    );

    CREATE TABLE IF NOT EXISTS ranges (
      scope TEXT NOT NULL,
      type TEXT NOT NULL,
      min_port INTEGER NOT NULL,
      max_port INTEGER NOT NULL,
      PRIMARY KEY (scope, type)
    );

    CREATE TABLE IF NOT EXISTS ports (
      scope TEXT NOT NULL,
      project TEXT NOT NULL,
      service TEXT NOT NULL,
      type TEXT NOT NULL,
      port INTEGER NOT NULL,
      env_var TEXT NOT NULL,
      created_at TEXT NOT NULL,
      PRIMARY KEY (scope, project, service, type),
      UNIQUE (scope, port)
    );

    CREATE TABLE IF NOT EXISTS dbs (
      scope TEXT NOT NULL,
      project TEXT NOT NULL,
      service TEXT NOT NULL,
      db_type TEXT NOT NULL,
      port INTEGER NOT NULL,
      db_name TEXT,
      username TEXT,
      secret_cipher BLOB,
      created_at TEXT NOT NULL,
      PRIMARY KEY (scope, project, service, db_type)
    );
  `);
}

function ensureReserved(db, scope) {
  const stmt = db.prepare("INSERT OR IGNORE INTO reserved_ports (scope, port, label) VALUES (?, ?, ?)");
  try {
    for (const [port, label] of DEFAULT_RESERVED) {
      stmt.run([scope, port, label]);
    }
  } finally {
    stmt.free();
  }
}

function ensureRanges(db, scope) {
  const stmt = db.prepare("INSERT OR IGNORE INTO ranges (scope, type, min_port, max_port) VALUES (?, ?, ?, ?)");
  try {
    for (const [type, [minPort, maxPort]] of Object.entries(DEFAULT_RANGES)) {
      stmt.run([scope, type, minPort, maxPort]);
    }
  } finally {
    stmt.free();
  }
}

export async function openRegistry(registryPath = defaultRegistryPath()) {
  const SQL = await getSql();
  const data = existsSync(registryPath) ? await readFile(registryPath) : null;
  const db = data ? new SQL.Database(data) : new SQL.Database();
  migrate(db);
  return { db, path: registryPath };
}

export async function persistRegistry({ db, path }) {
  const bytes = db.export();
  await mkdir(dirname(path), { recursive: true });
  // Layer 1 recovery: keep a shadow copy of the last good database so an
  // accidental deletion of registry.db can be reverted with `mv
  // registry.db.prev registry.db`. Written before the live file is replaced
  // so the shadow always lags by exactly one persist and never shares the
  // half-written tmp state. Failure to copy (e.g. first persist, or the
  // previous DB was already deleted) must not block the write itself.
  if (existsSync(path)) {
    await copyFile(path, `${path}.prev`).catch(() => {});
  }
  const tmp = `${path}.tmp`;
  await writeFile(tmp, bytes, { mode: 0o600 });
  await rename(tmp, path);
  db.close();
}

// A healthy registry op (read-modify-write of a small sqlite file) takes
// milliseconds. Any holder alive past this is hung, and any holder that is
// dead (crashed / SIGKILL'd mid-op) never finishes at all -- reclaim both.
const LOCK_STALE_MS = 15000;
const LOCK_WAIT_TIMEOUT_MS = 30000;

function lockOwnerPath(lockPath) {
  return join(lockPath, "owner");
}

async function tryAcquireLock(lockPath) {
  try {
    await mkdir(dirname(lockPath), { recursive: true });
    await mkdir(lockPath);
  } catch (error) {
    if (error.code === "EEXIST") {
      return false;
    }
    throw error;
  }
  try {
    await writeFile(lockOwnerPath(lockPath), `${hostname()}\n${process.pid}\n${Date.now()}\n`);
    return true;
  } catch (error) {
    await rm(lockPath, { recursive: true, force: true });
    throw error;
  }
}

async function readLockOwner(lockPath) {
  try {
    const text = (await readFile(lockOwnerPath(lockPath), "utf8")).trim();
    const [host, pid, ts] = text.split(/\n/);
    return { host, pid: Number(pid) || null, ts: Number(ts) || 0 };
  } catch {
    return null;
  }
}

function pidAlive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

async function lockIsStale(lockPath) {
  const owner = await readLockOwner(lockPath);
  if (!owner || !owner.pid) {
    // No/unparseable owner file: leftover from a crash that predates the
    // owner file, or a partial write. Treat as stale and reclaim.
    return true;
  }
  const age = Date.now() - owner.ts;
  if (owner.host === hostname()) {
    if (!pidAlive(owner.pid)) {
      return true;
    }
    return age > LOCK_STALE_MS;
  }
  // Lock left by another host (shared filesystem): can't probe its PID,
  // fall back to the age threshold.
  return age > LOCK_STALE_MS;
}

async function withLock(registryPath, fn) {
  const lockPath = `${registryPath}.lock`;
  let waited = 0;
  for (;;) {
    if (await tryAcquireLock(lockPath)) {
      break;
    }
    if (await lockIsStale(lockPath)) {
      await rm(lockPath, { recursive: true, force: true });
      if (await tryAcquireLock(lockPath)) {
        break;
      }
    }
    if (waited > LOCK_WAIT_TIMEOUT_MS) {
      throw new Error(`Registry is locked by another eco process (${lockPath}); gave up after ${LOCK_WAIT_TIMEOUT_MS / 1000}s.`);
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
    waited += 50;
  }
  try {
    return await fn();
  } finally {
    // Only release if we still own it -- a stale-reclaim race may have
    // re-assigned the lock to another process while we ran.
    const owner = await readLockOwner(lockPath);
    if (owner && owner.host === hostname() && owner.pid === process.pid) {
      await rm(lockPath, { recursive: true, force: true });
    }
  }
}

function rows(db, sql, params = []) {
  const stmt = db.prepare(sql);
  try {
    stmt.bind(params);
    const result = [];
    while (stmt.step()) {
      result.push(stmt.getAsObject());
    }
    return result;
  } finally {
    stmt.free();
  }
}

function run(db, sql, params = []) {
  const stmt = db.prepare(sql);
  try {
    stmt.run(params);
  } finally {
    stmt.free();
  }
}

function rangeFor(db, scope, type) {
  const found = rows(db, "SELECT min_port, max_port FROM ranges WHERE scope = ? AND type = ?", [scope, type]);
  if (found.length === 0) {
    const [minPort, maxPort] = DEFAULT_RANGES[type] || DEFAULT_RANGES.service;
    return [minPort, maxPort];
  }
  return [found[0].min_port, found[0].max_port];
}

function usedPorts(db, scope) {
  const reserved = rows(db, "SELECT port FROM reserved_ports WHERE scope = ?", [scope]).map((r) => r.port);
  const allocated = rows(db, "SELECT port FROM ports WHERE scope = ?", [scope]).map((r) => r.port);
  return new Set([...reserved, ...allocated]);
}

export async function getOrAllocatePort({ registryPath, scope, project, service, type, envVar, preferred }) {
  return withLock(registryPath || defaultRegistryPath(), async () => {
    const reg = await openRegistry(registryPath);
    const { db } = reg;
    ensureReserved(db, scope);
    ensureRanges(db, scope);

    const existing = rows(db, "SELECT port FROM ports WHERE scope = ? AND project = ? AND service = ? AND type = ?", [scope, project, service, type]);
    if (existing.length > 0) {
      await persistRegistry(reg);
      return { port: existing[0].port, created: false };
    }

    const used = usedPorts(db, scope);
    const [minPort, maxPort] = rangeFor(db, scope, type);
    let port = null;

    if (preferred && /^\d+$/.test(String(preferred))) {
      const wanted = Number(preferred);
      if (wanted >= minPort && wanted <= maxPort && !used.has(wanted) && !portInUse(wanted)) {
        port = wanted;
      }
    }

    if (port === null) {
      const candidates = maxPort - minPort + 1;
      const startOffset = crypto.randomInt(candidates);
      for (let offset = 0; offset < candidates; offset++) {
        const candidate = minPort + ((startOffset + offset) % candidates);
        if (used.has(candidate) || portInUse(candidate)) {
          continue;
        }
        port = candidate;
        break;
      }
    }

    if (port === null) {
      throw new Error(`No free port available for ${project}/${service} in range ${minPort}-${maxPort}. Release a port with 'eco ports release' or adjust the range.`);
    }

    run(db, "INSERT INTO ports (scope, project, service, type, port, env_var, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)", [scope, project, service, type, port, envVar, new Date().toISOString()]);
    await persistRegistry(reg);
    return { port, created: true };
  });
}

export async function lookupPort({ registryPath, scope, project, service, type }) {
  const reg = await openRegistry(registryPath);
  const { db } = reg;
  const found = rows(db, "SELECT port FROM ports WHERE scope = ? AND project = ? AND service = ? AND type = ?", [scope, project, service, type]);
  return found.length > 0 ? found[0].port : null;
}

export async function projectHasRegistryRows({ registryPath, scope, project }) {
  const reg = await openRegistry(registryPath);
  const { db } = reg;
  const ports = rows(db, "SELECT 1 FROM ports WHERE scope = ? AND project = ? LIMIT 1", [scope, project]);
  if (ports.length > 0) {
    return true;
  }
  const dbs = rows(db, "SELECT 1 FROM dbs WHERE scope = ? AND project = ? LIMIT 1", [scope, project]);
  return dbs.length > 0;
}

export async function seedPort({ registryPath, scope, project, service, type, envVar, port }) {
  return withLock(registryPath || defaultRegistryPath(), async () => {
    const reg = await openRegistry(registryPath);
    const { db } = reg;
    ensureReserved(db, scope);
    ensureRanges(db, scope);

    const reserved = rows(db, "SELECT label FROM reserved_ports WHERE scope = ? AND port = ?", [scope, port]);
    if (reserved.length > 0) {
      throw new Error(`Port ${port} is reserved (${reserved[0].label}) and cannot be adopted.`);
    }

    const conflict = rows(db, "SELECT project, service FROM ports WHERE scope = ? AND port = ?", [scope, port]);
    if (conflict.length > 0) {
      if (conflict[0].project === project && conflict[0].service === service) {
        await persistRegistry(reg);
        return { port, created: false };
      }
      throw new Error(`Port ${port} is already allocated to ${conflict[0].project}/${conflict[0].service}.`);
    }

    const existing = rows(db, "SELECT port FROM ports WHERE scope = ? AND project = ? AND service = ? AND type = ?", [scope, project, service, type]);
    if (existing.length > 0) {
      await persistRegistry(reg);
      return { port: existing[0].port, created: false };
    }

    run(db, "INSERT INTO ports (scope, project, service, type, port, env_var, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)", [scope, project, service, type, port, envVar, new Date().toISOString()]);
    await persistRegistry(reg);
    return { port, created: true };
  });
}

export async function pinPort({ registryPath, scope, project, service, type, envVar, port }) {
  return withLock(registryPath || defaultRegistryPath(), async () => {
    const reg = await openRegistry(registryPath);
    const { db } = reg;
    ensureReserved(db, scope);
    ensureRanges(db, scope);

    const reserved = rows(db, "SELECT label FROM reserved_ports WHERE scope = ? AND port = ?", [scope, port]);
    if (reserved.length > 0) {
      throw new Error(`Port ${port} is reserved (${reserved[0].label}) and cannot be assigned.`);
    }

    if (port < 1 || port > 65535) {
      throw new Error(`Port ${port} is not a valid TCP port.`);
    }

    if (portInUse(port)) {
      throw new Error(`Port ${port} is already in use on this machine.`);
    }

    const conflict = rows(db, "SELECT project, service FROM ports WHERE scope = ? AND port = ?", [scope, port]);
    if (conflict.length > 0) {
      throw new Error(`Port ${port} is already allocated to ${conflict[0].project}/${conflict[0].service}.`);
    }

    const existing = rows(db, "SELECT port FROM ports WHERE scope = ? AND project = ? AND service = ? AND type = ?", [scope, project, service, type]);
    if (existing.length > 0) {
      if (existing[0].port !== port) {
        throw new Error(`${project}/${service} already holds port ${existing[0].port}; release it first to change it.`);
      }
      await persistRegistry(reg);
      return { port, created: false };
    }

    run(db, "INSERT INTO ports (scope, project, service, type, port, env_var, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)", [scope, project, service, type, port, envVar, new Date().toISOString()]);
    await persistRegistry(reg);
    return { port, created: true };
  });
}

export async function releasePort({ registryPath, scope, project, service }) {
  return withLock(registryPath || defaultRegistryPath(), async () => {
    const reg = await openRegistry(registryPath);
    const { db } = reg;
    run(db, "DELETE FROM ports WHERE scope = ? AND project = ? AND service = ?", [scope, project, service]);
    run(db, "DELETE FROM dbs WHERE scope = ? AND project = ? AND service = ?", [scope, project, service]);
    await persistRegistry(reg);
  });
}

export async function resetProject({ registryPath, scope, project }) {
  return withLock(registryPath || defaultRegistryPath(), async () => {
    const reg = await openRegistry(registryPath);
    const { db } = reg;
    run(db, "DELETE FROM ports WHERE scope = ? AND project = ?", [scope, project]);
    run(db, "DELETE FROM dbs WHERE scope = ? AND project = ?", [scope, project]);
    await persistRegistry(reg);
  });
}

// Renames every row keyed to a project, e.g. after the estate's canonical
// project name is corrected (manifest `project:` vs the directory basename).
// Ports are immutable, so the new name must not collide with rows that the
// target project already owns; otherwise the (scope, port) unique index
// would reject the rename mid-way.
export async function renameProject({ registryPath, scope, from, to }) {
  return withLock(registryPath || defaultRegistryPath(), async () => {
    const reg = await openRegistry(registryPath);
    const { db } = reg;

    if (from === to) return;

    const existing = rows(db, "SELECT port FROM ports WHERE scope = ? AND project = ?", [scope, to]);
    if (existing.length > 0) {
      throw new Error(`Project ${to} already owns registry rows; refuse to merge ${from} into it.`);
    }

    run(db, "UPDATE ports SET project = ? WHERE scope = ? AND project = ?", [to, scope, from]);
    run(db, "UPDATE dbs SET project = ? WHERE scope = ? AND project = ?", [to, scope, from]);
    await persistRegistry(reg);
  });
}

export async function listPorts({ registryPath, scope, project }) {
  const reg = await openRegistry(registryPath);
  const { db } = reg;
  ensureReserved(db, scope);
  ensureRanges(db, scope);
  const params = [scope];
  let sql = "SELECT * FROM ports WHERE scope = ?";
  if (project) {
    sql += " AND project = ?";
    params.push(project);
  }
  sql += " ORDER BY port";
  return rows(db, sql, params);
}

export async function listReserved({ registryPath, scope }) {
  const reg = await openRegistry(registryPath);
  const { db } = reg;
  ensureReserved(db, scope);
  return rows(db, "SELECT port, label FROM reserved_ports WHERE scope = ? ORDER BY port", [scope]);
}

// Reads the whole registry regardless of scope -- used by `eco dashboard`
// to render every estate recorded on a machine. Returns ports grouped by
// (scope, project), plus every db and reserved row.
export async function readRegistryAll({ registryPath }) {
  const reg = await openRegistry(registryPath);
  const { db } = reg;
  const ports = rows(db, "SELECT * FROM ports ORDER BY scope, project, port");
  const dbs = rows(db, "SELECT * FROM dbs ORDER BY scope, project, service");
  const reserved = rows(db, "SELECT * FROM reserved_ports ORDER BY scope, port");
  return { ports, dbs, reserved };
}

export async function listDbs({ registryPath, scope, project, withSecret = false }) {
  const reg = await openRegistry(registryPath);
  const { db } = reg;
  const params = [scope];
  let sql = "SELECT * FROM dbs WHERE scope = ?";
  if (project) {
    sql += " AND project = ?";
    params.push(project);
  }
  sql += " ORDER BY service";
  const results = rows(db, sql, params);
  if (!withSecret) {
    return results.map(({ secret_cipher, ...rest }) => rest);
  }
  const key = await loadKey(reg.path);
  return results.map((row) => {
    const { secret_cipher, ...rest } = row;
    if (!secret_cipher) {
      return rest;
    }
    return { ...rest, password: decryptSecret(key, secret_cipher) };
  });
}

export async function recordDb({ registryPath, scope, project, service, dbType, port, dbName, username, password }) {
  return withLock(registryPath || defaultRegistryPath(), async () => {
    const reg = await openRegistry(registryPath);
    const { db } = reg;
    const key = await loadKey(reg.path);
    const secretCipher = password ? encryptSecret(key, password) : null;
    const existing = rows(db, "SELECT port FROM dbs WHERE scope = ? AND project = ? AND service = ? AND db_type = ?", [scope, project, service, dbType]);
    if (existing.length > 0) {
      run(db, "UPDATE dbs SET port = ?, db_name = ?, username = ?, secret_cipher = COALESCE(?, secret_cipher) WHERE scope = ? AND project = ? AND service = ? AND db_type = ?", [port, dbName, username, secretCipher, scope, project, service, dbType]);
    } else {
      run(db, "INSERT INTO dbs (scope, project, service, db_type, port, db_name, username, secret_cipher, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)", [scope, project, service, dbType, port, dbName, username, secretCipher, new Date().toISOString()]);
    }
    await persistRegistry(reg);
  });
}
