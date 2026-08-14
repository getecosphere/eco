// Eco accounts — signup/login issue free per-account API keys used by
// `eco up --remote` / `eco lxs`. Identity is separate from the auth LXS (which
// owns app-level signup/signin for estates); this is the eco platform account
// (tier, payload cap, API key). For now every account is the free tier.
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::path::PathBuf;

use crate::registry::hex_encode;

const DEFAULT_DB: &str = "/etc/eco/accounts.db";
const SALT_LEN: usize = 16;
const KEY_BYTES: usize = 32;
const PBKDF2_ITERATIONS: u32 = 100_000;

pub struct Account {
    pub email: String,
    pub tier: String,
    pub payload_cap_mb: u64,
    pub created_at: String,
}

fn accounts_db_path() -> PathBuf {
    let p = util_env("ECO_ACCOUNTS_DB", DEFAULT_DB);
    PathBuf::from(p)
}

fn util_env(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex_encode(&sha2::Sha256::digest(bytes))
}

fn hash_password(password: &str, salt: &[u8]) -> Vec<u8> {
    let mut out = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<sha2::Sha256>(password.as_bytes(), salt, PBKDF2_ITERATIONS, &mut out);
    out.to_vec()
}

fn gen_salt() -> Vec<u8> {
    let mut salt = vec![0u8; SALT_LEN];
    rand::thread_rng().fill_bytes(&mut salt);
    salt
}

fn gen_api_key() -> String {
    let mut bytes = [0u8; KEY_BYTES];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex_encode(&bytes)
}

fn open_db() -> Result<rusqlite::Connection, String> {
    let path = accounts_db_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = rusqlite::Connection::open(&path).map_err(|e| format!("open accounts db {}: {e}", path.display()))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS accounts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            email TEXT NOT NULL UNIQUE,
            password_salt TEXT NOT NULL,
            password_hash TEXT NOT NULL,
            api_key_hash TEXT NOT NULL UNIQUE,
            tier TEXT NOT NULL DEFAULT 'free',
            payload_cap_mb INTEGER NOT NULL DEFAULT 300,
            created_at TEXT NOT NULL
        );",
    )
    .map_err(|e| format!("init accounts db: {e}"))?;
    Ok(conn)
}

fn row_to_account(row: &rusqlite::Row) -> Result<Account, rusqlite::Error> {
    Ok(Account {
        email: row.get(0)?,
        tier: row.get(1)?,
        payload_cap_mb: row.get::<_, i64>(2)? as u64,
        created_at: row.get(3)?,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Public API (used by runtime/agent.rs)
// ─────────────────────────────────────────────────────────────────────────────

pub fn signup(email: &str, password: &str) -> Result<(String, Account), String> {
    let email = email.trim().to_lowercase();
    if email.is_empty() || !email.contains('@') {
        return Err("invalid email".to_string());
    }
    if password.len() < 8 {
        return Err("password must be at least 8 characters".to_string());
    }
    let api_key = gen_api_key();
    let salt = gen_salt();
    let hash = hash_password(password, &salt);
    let created_at = crate::commands::lxs::now_rfc3339();
    let conn = open_db()?;
    conn.execute(
        "INSERT INTO accounts (email, password_salt, password_hash, api_key_hash, tier, payload_cap_mb, created_at) VALUES (?1,?2,?3,?4,'free',300,?5)",
        rusqlite::params![email, hex_encode(&salt), hex_encode(&hash), sha256_hex(api_key.as_bytes()), created_at],
    )
    .map_err(|e| {
        if e.to_string().contains("UNIQUE") {
            "an account with that email already exists".to_string()
        } else {
            format!("create account: {e}")
        }
    })?;
    Ok((api_key, Account { email, tier: "free".to_string(), payload_cap_mb: 300, created_at }))
}

pub fn login(email: &str, password: &str) -> Result<String, String> {
    let email = email.trim().to_lowercase();
    let conn = open_db()?;
    let row = conn
        .query_row(
            "SELECT password_salt, password_hash FROM accounts WHERE email = ?1",
            rusqlite::params![email],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .map_err(|_| "invalid email or password".to_string())?;
    let salt = hex_to_bytes(&row.0)?;
    let expected = hex_to_bytes(&row.1)?;
    let actual = hash_password(password, &salt);
    if actual != expected {
        return Err("invalid email or password".to_string());
    }
    // Only the key's hash is stored; each successful login issues a fresh key
    // (the previous one is invalidated) and returns it to the caller.
    let api_key = gen_api_key();
    conn.execute(
        "UPDATE accounts SET api_key_hash = ?1 WHERE email = ?2",
        rusqlite::params![sha256_hex(api_key.as_bytes()), email],
    )
    .map_err(|e| format!("issue key: {e}"))?;
    Ok(api_key)
}

pub fn account_for_key(bearer: &str) -> Option<Account> {
    let key_hash = sha256_hex(bearer.trim().as_bytes());
    let conn = open_db().ok()?;
    conn.query_row(
        "SELECT email, tier, payload_cap_mb, created_at FROM accounts WHERE api_key_hash = ?1",
        rusqlite::params![key_hash],
        row_to_account,
    )
    .ok()
}

pub fn rotate_key(bearer: &str) -> Result<String, String> {
    let account = account_for_key(bearer).ok_or_else(|| "invalid API key".to_string())?;
    let api_key = gen_api_key();
    let conn = open_db()?;
    conn.execute(
        "UPDATE accounts SET api_key_hash = ?1 WHERE email = ?2",
        rusqlite::params![sha256_hex(api_key.as_bytes()), account.email],
    )
    .map_err(|e| format!("rotate key: {e}"))?;
    Ok(api_key)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployRecord {
    pub project: String,
    pub status: String,
    pub size_mb: u64,
    pub created_at: String,
}

fn open_db_full() -> Result<rusqlite::Connection, String> {
    let conn = open_db()?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS deploys (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            email TEXT NOT NULL,
            project TEXT NOT NULL,
            status TEXT NOT NULL,
            size_mb INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL
        );",
    )
    .map_err(|e| format!("init deploys db: {e}"))?;
    Ok(conn)
}

pub fn record_deploy(bearer: &str, project: &str, status: &str, size_mb: u64) {
    if let Some(account) = account_for_key(bearer) {
        if let Ok(conn) = open_db_full() {
            let _ = conn.execute(
                "INSERT INTO deploys (email, project, status, size_mb, created_at) VALUES (?1,?2,?3,?4,?5)",
                rusqlite::params![account.email, project, status, size_mb as i64, crate::commands::lxs::now_rfc3339()],
            );
        }
    }
}

pub fn record_deploy_running(bearer: &str, project: &str, size_mb: u64) -> Option<i64> {
    let account = account_for_key(bearer)?;
    let conn = open_db_full().ok()?;
    conn.execute(
        "INSERT INTO deploys (email, project, status, size_mb, created_at) VALUES (?1,?2,'running',?3,?4)",
        rusqlite::params![account.email, project, size_mb as i64, crate::commands::lxs::now_rfc3339()],
    )
    .ok()?;
    conn.last_insert_rowid().into()
}

pub fn update_deploy_status(id: i64, status: &str) {
    if let Ok(conn) = open_db_full() {
        let _ = conn.execute(
            "UPDATE deploys SET status = ?1 WHERE id = ?2",
            rusqlite::params![status, id],
        );
    }
}

pub fn deploy_status_by_id(id: i64) -> String {
    if let Ok(conn) = open_db_full() {
        if let Ok(mut stmt) = conn.prepare("SELECT status FROM deploys WHERE id = ?1") {
            if let Ok(mut rows) = stmt.query_map(rusqlite::params![id], |r| r.get::<_, String>(0)) {
                if let Some(Ok(status)) = rows.next() {
                    return status;
                }
            }
        }
    }
    "pending".to_string()
}

pub fn latest_deploy_status_for_project(project: &str) -> String {
    if let Ok(conn) = open_db_full() {
        if let Ok(mut stmt) = conn.prepare("SELECT status FROM deploys WHERE project = ?1 ORDER BY id DESC LIMIT 1") {
            if let Ok(mut rows) = stmt.query_map(rusqlite::params![project], |r| r.get::<_, String>(0)) {
                if let Some(Ok(status)) = rows.next() {
                    return status;
                }
            }
        }
    }
    "pending".to_string()
}

pub fn list_deploys(bearer: &str, limit: i64) -> Vec<DeployRecord> {    let Some(account) = account_for_key(bearer) else {
        return Vec::new();
    };
    let Ok(conn) = open_db_full() else {
        return Vec::new();
    };
    let Ok(mut stmt) = conn.prepare("SELECT project, status, size_mb, created_at FROM deploys WHERE email = ?1 ORDER BY id DESC LIMIT ?2") else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map(rusqlite::params![account.email, limit], |row| {
        Ok(DeployRecord {
            project: row.get(0)?,
            status: row.get(1)?,
            size_mb: row.get::<_, i64>(2)? as u64,
            created_at: row.get(3)?,
        })
    }) else {
        return Vec::new();
    };
    rows.filter_map(|r| r.ok()).collect()
}

fn hex_to_bytes(hex: &str) -> Result<Vec<u8>, String> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|e| format!("bad hex: {e}")))
        .collect()
}
