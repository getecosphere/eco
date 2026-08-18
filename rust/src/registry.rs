use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use rand::Rng;
use rusqlite::Connection;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::util;

const DEFAULT_RANGES: &[(&str, u32, u32)] = &[
    ("service", 20000, 27999),
    ("gateway", 20000, 27999),
    ("index", 20000, 27999),
];

const DEFAULT_RESERVED: &[(u32, &str)] = &[
    (27017, "mongod (application databases)"),
    (5432, "postgres (application databases)"),
    (6379, "redis (application runtime)"),
];

const LOCK_STALE_MS: u128 = 15000;
const LOCK_WAIT_TIMEOUT_MS: u128 = 30000;

pub fn default_registry_path() -> PathBuf {
    if let Ok(p) = std::env::var("ECO_REGISTRY_PATH") {
        return PathBuf::from(p);
    }
    let base = if platform_is_linux_root() {
        "/etc/eco".to_string()
    } else {
        format!("{}/.eco", util::home_dir())
    };
    PathBuf::from(base).join("registry.db")
}

fn platform_is_linux_root() -> bool {
    util::platform() == "linux" && nix_getuid() == 0
}

fn nix_getuid() -> u32 {
    #[cfg(unix)]
    {
        unsafe { libc::getuid() }
    }
    #[cfg(not(unix))]
    {
        1000
    }
}

pub fn default_scope() -> String {
    util::env_var_or("ECO_REGISTRY_SCOPE", &util::hostname())
}

fn key_path_for(registry_path: &Path) -> PathBuf {
    let mut p = registry_path.as_os_str().to_os_string();
    p.push(".key");
    PathBuf::from(p)
}

fn load_or_create_key(registry_path: &Path) -> Result<Vec<u8>, String> {
    let key_path = key_path_for(registry_path);
    if key_path.is_file() {
        let content = std::fs::read_to_string(&key_path).map_err(|e| format!("read key: {e}"))?;
        return hex_decode(content.trim());
    }
    let mut key = [0u8; 32];
    rand::thread_rng().fill(&mut key);
    if let Some(parent) = key_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir key dir: {e}"))?;
    }
    std::fs::write(&key_path, format!("{}\n", hex_encode(&key)))
        .map_err(|e| format!("write key: {e}"))?;
    restrict_permissions(&key_path);
    Ok(key.to_vec())
}

/// Restrict a file to owner-only (0600). Unix-only; Windows uses default ACLs.
#[cfg(unix)]
fn restrict_permissions(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &std::path::Path) {}

pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn hex_decode(text: &str) -> Result<Vec<u8>, String> {
    let text = text.trim();
    if text.len() % 2 != 0 {
        return Err("invalid hex length".to_string());
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    let bytes = text.as_bytes();
    for i in (0..bytes.len()).step_by(2) {
        let hi = hex_val(bytes[i])?;
        let lo = hex_val(bytes[i + 1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_val(b: u8) -> Result<u8, String> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err("invalid hex character".to_string()),
    }
}

pub fn encrypt_secret(key: &[u8], plaintext: &str) -> Result<String, String> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|e| format!("aes init: {e}"))?;
    let mut iv = [0u8; 12];
    rand::thread_rng().fill(&mut iv);
    let nonce = Nonce::from_slice(&iv);
    // aes-gcm returns ciphertext||tag(16); Node's format is iv||tag||ciphertext.
    let ct_with_tag = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|e| format!("aes encrypt: {e}"))?;
    let split = ct_with_tag.len() - 16;
    let (ciphertext, tag) = ct_with_tag.split_at(split);
    let mut buf = Vec::with_capacity(12 + 16 + ciphertext.len());
    buf.extend_from_slice(&iv);
    buf.extend_from_slice(tag);
    buf.extend_from_slice(ciphertext);
    Ok(base64::engine::general_purpose::STANDARD.encode(&buf))
}

pub fn decrypt_secret(key: &[u8], encoded: &str) -> Result<String, String> {
    let buf = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .map_err(|e| format!("base64 decode: {e}"))?;
    if buf.len() < 28 {
        return Err("ciphertext too short".to_string());
    }
    let iv = &buf[0..12];
    let tag = &buf[12..28];
    let ciphertext = &buf[28..];
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|e| format!("aes init: {e}"))?;
    let mut ct_with_tag = Vec::with_capacity(ciphertext.len() + 16);
    ct_with_tag.extend_from_slice(ciphertext);
    ct_with_tag.extend_from_slice(tag);
    let plaintext = cipher
        .decrypt(Nonce::from_slice(iv), ct_with_tag.as_slice())
        .map_err(|e| format!("aes decrypt: {e}"))?;
    String::from_utf8(plaintext).map_err(|e| format!("utf8: {e}"))
}


fn text_val(s: &str) -> rusqlite::types::Value {
    rusqlite::types::Value::from(s.to_string())
}

fn port_in_use(port: u32) -> bool {
    let cwd = util::current_dir();
    if util::platform() == "linux" {
        if let Ok(r) = util::run_capture("ss", &["-ltnH".to_string(), format!("sport = :{port}")], &cwd) {
            return !r.stdout.trim().is_empty();
        }
        return false;
    }
    // macOS: lsof; only true when a LISTEN socket is actually reported
    if let Ok(r) = util::run_capture(
        "lsof",
        &["-nP".to_string(), format!("-iTCP:{port}"), "-sTCP:LISTEN".to_string()],
        &cwd,
    ) {
        return r.code == 0 && !r.stdout.trim().is_empty();
    }
    false
}

fn migrate(db: &Connection) -> Result<(), String> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS reserved_ports (
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
        );",
    )
    .map_err(|e| format!("registry migrate: {e}"))
}

fn ensure_reserved(db: &Connection, scope: &str) -> Result<(), String> {
    let mut stmt = db
        .prepare("INSERT OR IGNORE INTO reserved_ports (scope, port, label) VALUES (?1, ?2, ?3)")
        .map_err(|e| format!("prepare reserved: {e}"))?;
    for (port, label) in DEFAULT_RESERVED {
        stmt.execute(rusqlite::params![scope, port, label])
            .map_err(|e| format!("insert reserved: {e}"))?;
    }
    Ok(())
}

fn ensure_ranges(db: &Connection, scope: &str) -> Result<(), String> {
    let mut stmt = db
        .prepare("INSERT OR IGNORE INTO ranges (scope, type, min_port, max_port) VALUES (?1, ?2, ?3, ?4)")
        .map_err(|e| format!("prepare ranges: {e}"))?;
    for (ty, min_port, max_port) in DEFAULT_RANGES {
        stmt.execute(rusqlite::params![scope, ty, min_port, max_port])
            .map_err(|e| format!("insert ranges: {e}"))?;
    }
    Ok(())
}

fn open_db(path: &Path) -> Result<Connection, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir registry: {e}"))?;
    }
    let db = if path.is_file() {
        Connection::open(path).map_err(|e| format!("open registry: {e}"))?
    } else {
        Connection::open(path).map_err(|e| format!("create registry: {e}"))?
    };
    migrate(&db)?;
    Ok(db)
}

fn persist(db: &Connection, path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir registry: {e}"))?;
    }
    // Layer 1 recovery: shadow copy
    if path.is_file() {
        let _ = std::fs::copy(path, format!("{}.prev", path.display()));
    }
    let tmp = format!("{}.tmp", path.display());
    // force checkpoint so the file on disk reflects in-memory writes
    db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").ok();
    restrict_permissions(path);
    if let Ok(bytes) = std::fs::read(path) {
        std::fs::write(&tmp, &bytes).ok();
        let _ = std::fs::rename(&tmp, path);
    }
    Ok(())
}

fn rows_as_objects(
    db: &Connection,
    sql: &str,
    params: &[rusqlite::types::Value],
) -> Result<Vec<serde_json::Value>, String> {
    let mut stmt = db.prepare(sql).map_err(|e| format!("prepare: {e}"))?;
    let col_count = stmt.column_count();
    let column_names: Vec<String> = (0..col_count)
        .map(|i| stmt.column_name(i).unwrap_or("").to_string())
        .collect();
    let mut rows = stmt.query(rusqlite::params_from_iter(params.iter().cloned()))
        .map_err(|e| format!("query: {e}"))?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().map_err(|e| format!("step: {e}"))? {
        let mut obj = serde_json::Map::new();
        for i in 0..col_count {
            let name = column_names[i].clone();
            let value = row.get_ref(i).map_err(|e| format!("get: {e}"))?;
            let json_value = match value {
                rusqlite::types::ValueRef::Null => serde_json::Value::Null,
                rusqlite::types::ValueRef::Integer(n) => serde_json::Value::Number(n.into()),
                rusqlite::types::ValueRef::Real(f) => serde_json::Number::from_f64(f).map(serde_json::Value::Number).unwrap_or(serde_json::Value::Null),
                rusqlite::types::ValueRef::Text(t) => serde_json::Value::String(String::from_utf8_lossy(t).to_string()),
                rusqlite::types::ValueRef::Blob(b) => serde_json::Value::String(base64::engine::general_purpose::STANDARD.encode(b)),
            };
            obj.insert(name, json_value);
        }
        out.push(serde_json::Value::Object(obj));
    }
    Ok(out)
}

fn row_count(db: &Connection, sql: &str, params: &[rusqlite::types::Value]) -> Result<i64, String> {
    let mut stmt = db.prepare(sql).map_err(|e| format!("prepare: {e}"))?;
    let mut rows = stmt
        .query(rusqlite::params_from_iter(params.iter().cloned()))
        .map_err(|e| format!("query: {e}"))?;
    if let Some(row) = rows.next().map_err(|e| format!("step: {e}"))? {
        Ok(row.get(0).unwrap_or(0))
    } else {
        Ok(0)
    }
}

fn range_for(db: &Connection, scope: &str, ty: &str) -> Result<(u32, u32), String> {
    let rows = rows_as_objects(
        db,
        "SELECT min_port, max_port FROM ranges WHERE scope = ?1 AND type = ?2",
        &[text_val(scope), text_val(ty)],
    )?;
    if rows.is_empty() {
        let found = DEFAULT_RANGES
            .iter()
            .find(|(t, _, _)| *t == ty)
            .map(|(_, min_p, max_p)| (*min_p, *max_p))
            .unwrap_or((20000, 27999));
        return Ok(found);
    }
    Ok((
        rows[0].get("min_port").and_then(|v| v.as_i64()).unwrap_or(20000) as u32,
        rows[0].get("max_port").and_then(|v| v.as_i64()).unwrap_or(27999) as u32,
    ))
}

fn used_ports(db: &Connection, scope: &str) -> Result<HashSet<u32>, String> {
    let reserved = rows_as_objects(
        db,
        "SELECT port FROM reserved_ports WHERE scope = ?1",
        &[text_val(scope)],
    )?;
    let allocated = rows_as_objects(
        db,
        "SELECT port FROM ports WHERE scope = ?1",
        &[text_val(scope)],
    )?;
    let mut set = HashSet::new();
    for r in reserved {
        if let Some(p) = r.get("port").and_then(|v| v.as_i64()) {
            set.insert(p as u32);
        }
    }
    for r in allocated {
        if let Some(p) = r.get("port").and_then(|v| v.as_i64()) {
            set.insert(p as u32);
        }
    }
    Ok(set)
}

fn with_lock<T>(registry_path: &Path, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let lock_path = format!("{}.lock", registry_path.display());
    let owner_path = format!("{lock_path}/owner");
    let started = Instant::now();

    loop {
        match std::fs::create_dir(&lock_path) {
            Ok(()) => {
                let now = chrono::Utc::now().timestamp_millis();
                let content = format!("{}\n{}\n{}\n", util::hostname(), std::process::id(), now);
                if std::fs::write(&owner_path, content).is_err() {
                    let _ = std::fs::remove_dir_all(&lock_path);
                    return Err("registry lock owner write failed".to_string());
                }
                break;
            }
            Err(_) => {
                // lock exists: check staleness
                if lock_is_stale(&lock_path, &owner_path) {
                    let _ = std::fs::remove_dir_all(&lock_path);
                    continue;
                }
                if started.elapsed().as_millis() > LOCK_WAIT_TIMEOUT_MS {
                    return Err(format!(
                        "Registry is locked by another eco process ({lock_path}); gave up after {}s.",
                        LOCK_WAIT_TIMEOUT_MS / 1000
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }

    let result = f();
    // release only if still ours
    if let Ok(owner_text) = std::fs::read_to_string(&owner_path) {
        let parts: Vec<&str> = owner_text.split('\n').collect();
        let owner_pid: i64 = parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(-1);
        if owner_pid == std::process::id() as i64 {
            let _ = std::fs::remove_dir_all(&lock_path);
        }
    }
    result
}

fn lock_is_stale(_lock_path: &str, owner_path: &str) -> bool {
    let owner_text = std::fs::read_to_string(owner_path).unwrap_or_default();
    let parts: Vec<&str> = owner_text.split('\n').collect();
    if parts.len() < 3 {
        return true;
    }
    let owner_host = parts[0].trim();
    let owner_pid: i64 = parts[1].trim().parse().unwrap_or(0);
    let owner_ts: i128 = parts[2].trim().parse().unwrap_or(0);
    let age = chrono::Utc::now().timestamp_millis() as i128 - owner_ts;
    if owner_pid <= 0 {
        return true;
    }
    if owner_host == util::hostname() {
        if !pid_alive(owner_pid as i32) {
            return true;
        }
        return age > LOCK_STALE_MS as i128;
    }
    // other host (shared fs): age threshold only
    age > LOCK_STALE_MS as i128
}

#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(not(unix))]
fn pid_alive(_pid: i32) -> bool {
    false
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub struct PortResult {
    pub port: u32,
    pub created: bool,
}

pub fn get_or_allocate_port(
    registry_path: &Path,
    scope: &str,
    project: &str,
    service: &str,
    ty: &str,
    env_var: &str,
    preferred: Option<&str>,
) -> Result<PortResult, String> {
    let registry_path = registry_path.to_path_buf();
    with_lock(&registry_path, || {
        let db = open_db(&registry_path)?;
        ensure_reserved(&db, scope)?;
        ensure_ranges(&db, scope)?;

        let existing = rows_as_objects(
            &db,
            "SELECT port FROM ports WHERE scope = ?1 AND project = ?2 AND service = ?3 AND type = ?4",
            &[
                text_val(scope),
                text_val(project),
                text_val(service),
                text_val(ty),
            ],
        )?;
        if !existing.is_empty() {
            let port = existing[0].get("port").and_then(|v| v.as_i64()).unwrap_or(0) as u32;
            persist(&db, &registry_path)?;
            return Ok(PortResult { port, created: false });
        }

        let used = used_ports(&db, scope)?;
        let (min_port, max_port) = range_for(&db, scope, ty)?;
        let mut port: Option<u32> = None;

        if let Some(pref) = preferred {
            if !pref.is_empty() && pref.chars().all(|c| c.is_ascii_digit()) {
                let wanted = pref.parse::<u32>().unwrap_or(0);
                if wanted >= min_port && wanted <= max_port && !used.contains(&wanted) && !port_in_use(wanted) {
                    port = Some(wanted);
                }
            }
        }

        if port.is_none() {
            // Bin-packing: fill the lowest free port first so the band stays
            // dense and released holes are reused before anything higher.
            for candidate in min_port..=max_port {
                if used.contains(&candidate) || port_in_use(candidate) {
                    continue;
                }
                port = Some(candidate);
                break;
            }
        }

        let port = port.ok_or_else(|| {
            format!(
                "No free port available for {project}/{service} in range {min_port}-{max_port}. Release a port with 'eco ports release' or adjust the range."
            )
        })?;

        db.execute(
            "INSERT INTO ports (scope, project, service, type, port, env_var, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![scope, project, service, ty, port, env_var, now_iso()],
        )
        .map_err(|e| format!("insert port: {e}"))?;
        persist(&db, &registry_path)?;
        Ok(PortResult { port, created: true })
    })
}

pub fn lookup_port(
    registry_path: &Path,
    scope: &str,
    project: &str,
    service: &str,
    ty: &str,
) -> Result<Option<u32>, String> {
    let db = open_db(registry_path)?;
    let rows = rows_as_objects(
        &db,
        "SELECT port FROM ports WHERE scope = ?1 AND project = ?2 AND service = ?3 AND type = ?4",
        &[
            text_val(scope),
            text_val(project),
            text_val(service),
            text_val(ty),
        ],
    )?;
    Ok(rows.first().and_then(|r| r.get("port").and_then(|v| v.as_i64()).map(|p| p as u32)))
}

pub fn project_has_registry_rows(
    registry_path: &Path,
    scope: &str,
    project: &str,
) -> Result<bool, String> {
    let db = open_db(registry_path)?;
    let ports = row_count(
        &db,
        "SELECT 1 FROM ports WHERE scope = ?1 AND project = ?2 LIMIT 1",
        &[text_val(scope), text_val(project)],
    )?;
    if ports > 0 {
        return Ok(true);
    }
    let dbs = row_count(
        &db,
        "SELECT 1 FROM dbs WHERE scope = ?1 AND project = ?2 LIMIT 1",
        &[text_val(scope), text_val(project)],
    )?;
    Ok(dbs > 0)
}

pub fn seed_port(
    registry_path: &Path,
    scope: &str,
    project: &str,
    service: &str,
    ty: &str,
    env_var: &str,
    port: u32,
) -> Result<PortResult, String> {
    let registry_path = registry_path.to_path_buf();
    with_lock(&registry_path, || {
        let db = open_db(&registry_path)?;
        ensure_reserved(&db, scope)?;
        ensure_ranges(&db, scope)?;

        let reserved = rows_as_objects(
            &db,
            "SELECT label FROM reserved_ports WHERE scope = ?1 AND port = ?2",
            &[text_val(scope), rusqlite::types::Value::from(port as i64)],
        )?;
        if !reserved.is_empty() {
            let label = reserved[0].get("label").and_then(|v| v.as_str()).unwrap_or("reserved");
            return Err(format!("Port {port} is reserved ({label}) and cannot be adopted."));
        }

        let conflict = rows_as_objects(
            &db,
            "SELECT project, service FROM ports WHERE scope = ?1 AND port = ?2",
            &[text_val(scope), rusqlite::types::Value::from(port as i64)],
        )?;
        if !conflict.is_empty() {
            let c_project = conflict[0].get("project").and_then(|v| v.as_str()).unwrap_or("");
            let c_service = conflict[0].get("service").and_then(|v| v.as_str()).unwrap_or("");
            if c_project == project && c_service == service {
                persist(&db, &registry_path)?;
                return Ok(PortResult { port, created: false });
            }
            return Err(format!("Port {port} is already allocated to {c_project}/{c_service}."));
        }

        let existing = rows_as_objects(
            &db,
            "SELECT port FROM ports WHERE scope = ?1 AND project = ?2 AND service = ?3 AND type = ?4",
            &[
                text_val(scope),
                text_val(project),
                text_val(service),
                text_val(ty),
            ],
        )?;
        if !existing.is_empty() {
            let port = existing[0].get("port").and_then(|v| v.as_i64()).unwrap_or(0) as u32;
            persist(&db, &registry_path)?;
            return Ok(PortResult { port, created: false });
        }

        db.execute(
            "INSERT INTO ports (scope, project, service, type, port, env_var, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![scope, project, service, ty, port, env_var, now_iso()],
        )
        .map_err(|e| format!("insert port: {e}"))?;
        persist(&db, &registry_path)?;
        Ok(PortResult { port, created: true })
    })
}

pub fn pin_port(
    registry_path: &Path,
    scope: &str,
    project: &str,
    service: &str,
    ty: &str,
    env_var: &str,
    port: u32,
) -> Result<PortResult, String> {
    let registry_path = registry_path.to_path_buf();
    with_lock(&registry_path, || {
        let db = open_db(&registry_path)?;
        ensure_reserved(&db, scope)?;
        ensure_ranges(&db, scope)?;

        let reserved = rows_as_objects(
            &db,
            "SELECT label FROM reserved_ports WHERE scope = ?1 AND port = ?2",
            &[text_val(scope), rusqlite::types::Value::from(port as i64)],
        )?;
        if !reserved.is_empty() {
            let label = reserved[0].get("label").and_then(|v| v.as_str()).unwrap_or("reserved");
            return Err(format!("Port {port} is reserved ({label}) and cannot be assigned."));
        }
        if port < 1 || port > 65535 {
            return Err(format!("Port {port} is not a valid TCP port."));
        }
        if port_in_use(port) {
            return Err(format!("Port {port} is already in use on this machine."));
        }

        let conflict = rows_as_objects(
            &db,
            "SELECT project, service FROM ports WHERE scope = ?1 AND port = ?2",
            &[text_val(scope), rusqlite::types::Value::from(port as i64)],
        )?;
        if !conflict.is_empty() {
            let c_project = conflict[0].get("project").and_then(|v| v.as_str()).unwrap_or("");
            let c_service = conflict[0].get("service").and_then(|v| v.as_str()).unwrap_or("");
            return Err(format!("Port {port} is already allocated to {c_project}/{c_service}."));
        }

        let existing = rows_as_objects(
            &db,
            "SELECT port FROM ports WHERE scope = ?1 AND project = ?2 AND service = ?3 AND type = ?4",
            &[
                text_val(scope),
                text_val(project),
                text_val(service),
                text_val(ty),
            ],
        )?;
        if !existing.is_empty() {
            let existing_port = existing[0].get("port").and_then(|v| v.as_i64()).unwrap_or(0) as u32;
            if existing_port != port {
                return Err(format!(
                    "{project}/{service} already holds port {existing_port}; release it first to change it."
                ));
            }
            persist(&db, &registry_path)?;
            return Ok(PortResult { port, created: false });
        }

        db.execute(
            "INSERT INTO ports (scope, project, service, type, port, env_var, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![scope, project, service, ty, port, env_var, now_iso()],
        )
        .map_err(|e| format!("insert port: {e}"))?;
        persist(&db, &registry_path)?;
        Ok(PortResult { port, created: true })
    })
}

pub fn release_port(
    registry_path: &Path,
    scope: &str,
    project: &str,
    service: &str,
) -> Result<(), String> {
    let registry_path = registry_path.to_path_buf();
    with_lock(&registry_path, || {
        let db = open_db(&registry_path)?;
        db.execute(
            "DELETE FROM ports WHERE scope = ?1 AND project = ?2 AND service = ?3",
            rusqlite::params![scope, project, service],
        )
        .map_err(|e| format!("delete ports: {e}"))?;
        db.execute(
            "DELETE FROM dbs WHERE scope = ?1 AND project = ?2 AND service = ?3",
            rusqlite::params![scope, project, service],
        )
        .map_err(|e| format!("delete dbs: {e}"))?;
        persist(&db, &registry_path)
    })
}

pub fn reset_project(registry_path: &Path, scope: &str, project: &str) -> Result<(), String> {
    let registry_path = registry_path.to_path_buf();
    with_lock(&registry_path, || {
        let db = open_db(&registry_path)?;
        db.execute(
            "DELETE FROM ports WHERE scope = ?1 AND project = ?2",
            rusqlite::params![scope, project],
        )
        .map_err(|e| format!("delete ports: {e}"))?;
        db.execute(
            "DELETE FROM dbs WHERE scope = ?1 AND project = ?2",
            rusqlite::params![scope, project],
        )
        .map_err(|e| format!("delete dbs: {e}"))?;
        persist(&db, &registry_path)
    })
}

pub fn rename_project(
    registry_path: &Path,
    scope: &str,
    from: &str,
    to: &str,
) -> Result<(), String> {
    let registry_path = registry_path.to_path_buf();
    with_lock(&registry_path, || {
        if from == to {
            return Ok(());
        }
        let db = open_db(&registry_path)?;
        let existing = rows_as_objects(
            &db,
            "SELECT port FROM ports WHERE scope = ?1 AND project = ?2",
            &[text_val(scope), text_val(to)],
        )?;
        if !existing.is_empty() {
            return Err(format!("Project {to} already owns registry rows; refuse to merge {from} into it."));
        }
        db.execute(
            "UPDATE ports SET project = ?1 WHERE scope = ?2 AND project = ?3",
            rusqlite::params![to, scope, from],
        )
        .map_err(|e| format!("update ports: {e}"))?;
        db.execute(
            "UPDATE dbs SET project = ?1 WHERE scope = ?2 AND project = ?3",
            rusqlite::params![to, scope, from],
        )
        .map_err(|e| format!("update dbs: {e}"))?;
        persist(&db, &registry_path)
    })
}

pub fn list_ports(
    registry_path: &Path,
    scope: &str,
    project: Option<&str>,
) -> Result<Vec<serde_json::Value>, String> {
    let db = open_db(registry_path)?;
    ensure_reserved(&db, scope)?;
    ensure_ranges(&db, scope)?;
    let sql = match project {
        Some(_) => "SELECT * FROM ports WHERE scope = ?1 AND project = ?2 ORDER BY port",
        None => "SELECT * FROM ports WHERE scope = ?1 ORDER BY port",
    };
    let params: Vec<rusqlite::types::Value> = match project {
        Some(p) => vec![text_val(scope), text_val(p)],
        None => vec![text_val(scope)],
    };
    rows_as_objects(&db, sql, &params)
}

pub fn list_reserved(
    registry_path: &Path,
    scope: &str,
) -> Result<Vec<serde_json::Value>, String> {
    let db = open_db(registry_path)?;
    ensure_reserved(&db, scope)?;
    rows_as_objects(
        &db,
        "SELECT port, label FROM reserved_ports WHERE scope = ?1 ORDER BY port",
        &[text_val(scope)],
    )
}

pub fn list_dbs(
    registry_path: &Path,
    scope: &str,
    project: Option<&str>,
    with_secret: bool,
) -> Result<Vec<serde_json::Value>, String> {
    let db = open_db(registry_path)?;
    let sql = match project {
        Some(_) => "SELECT * FROM dbs WHERE scope = ?1 AND project = ?2 ORDER BY service",
        None => "SELECT * FROM dbs WHERE scope = ?1 ORDER BY service",
    };
    let params: Vec<rusqlite::types::Value> = match project {
        Some(p) => vec![text_val(scope), text_val(p)],
        None => vec![text_val(scope)],
    };
    let mut results = rows_as_objects(&db, sql, &params)?;
    if with_secret {
        let key = load_or_create_key(registry_path)?;
        for row in results.iter_mut() {
            if let Some(cipher) = row.get("secret_cipher").and_then(|v| v.as_str()) {
                if !cipher.is_empty() {
                    if let Ok(plain) = decrypt_secret(&key, cipher) {
                        row.as_object_mut().unwrap().insert("password".to_string(), serde_json::Value::String(plain));
                    }
                }
            }
        }
    }
    Ok(results)
}

pub fn record_db(
    registry_path: &Path,
    scope: &str,
    project: &str,
    service: &str,
    db_type: &str,
    port: u32,
    db_name: Option<&str>,
    username: Option<&str>,
    password: Option<&str>,
) -> Result<(), String> {
    let registry_path = registry_path.to_path_buf();
    with_lock(&registry_path, || {
        let db = open_db(&registry_path)?;
        let key = load_or_create_key(&registry_path)?;
        let secret_cipher = match password {
            Some(p) if !p.is_empty() => Some(encrypt_secret(&key, p)?),
            _ => None,
        };
        let existing = rows_as_objects(
            &db,
            "SELECT port FROM dbs WHERE scope = ?1 AND project = ?2 AND service = ?3 AND db_type = ?4",
            &[
                text_val(scope),
                text_val(project),
                text_val(service),
                text_val(db_type),
            ],
        )?;
        if !existing.is_empty() {
            let cipher_sql = match &secret_cipher {
                Some(c) => c.clone(),
                None => String::new(),
            };
            db.execute(
                "UPDATE dbs SET port = ?1, db_name = ?2, username = ?3, secret_cipher = COALESCE(?4, secret_cipher) WHERE scope = ?5 AND project = ?6 AND service = ?7 AND db_type = ?8",
                rusqlite::params![
                    port,
                    db_name,
                    username,
                    if cipher_sql.is_empty() { None } else { Some(cipher_sql) },
                    scope,
                    project,
                    service,
                    db_type
                ],
            )
            .map_err(|e| format!("update db: {e}"))?;
        } else {
            db.execute(
                "INSERT INTO dbs (scope, project, service, db_type, port, db_name, username, secret_cipher, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    scope,
                    project,
                    service,
                    db_type,
                    port,
                    db_name,
                    username,
                    secret_cipher,
                    now_iso()
                ],
            )
            .map_err(|e| format!("insert db: {e}"))?;
        }
        persist(&db, &registry_path)
    })
}

/// Read the whole registry regardless of scope (used by dashboard).
pub fn read_registry_all(
    registry_path: &Path,
) -> Result<(Vec<serde_json::Value>, Vec<serde_json::Value>, Vec<serde_json::Value>), String> {
    let db = open_db(registry_path)?;
    let ports = rows_as_objects(&db, "SELECT * FROM ports ORDER BY scope, project, port", &[])?;
    let dbs = rows_as_objects(&db, "SELECT * FROM dbs ORDER BY scope, project, service", &[])?;
    let reserved = rows_as_objects(&db, "SELECT * FROM reserved_ports ORDER BY scope, port", &[])?;
    Ok((ports, dbs, reserved))
}
