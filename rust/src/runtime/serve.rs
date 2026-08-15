// Serve leases — persistent record of temporary *.getecosphere.com subdomain
// assignments. Stored alongside eco platform accounts (accounts.db) so the
// host-side `eco serve` agent can check conflicts and hand out run tokens.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const DEFAULT_DB: &str = "/etc/eco/accounts.db";

fn db_path() -> PathBuf {
    let p = std::env::var("ECO_ACCOUNTS_DB").unwrap_or_else(|_| DEFAULT_DB.to_string());
    PathBuf::from(p)
}

fn util_now() -> String {
    crate::commands::lxs::now_rfc3339()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServeLease {
    pub subdomain: String,
    pub hostname: String,
    pub owner_email: String,
    pub tunnel_id: String,
    pub tunnel_token: String,
    pub origin: String,
    pub port: u16,
    pub created_at: String,
}

fn open_db() -> Result<rusqlite::Connection, String> {
    let path = db_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = rusqlite::Connection::open(&path).map_err(|e| format!("open accounts db {}: {e}", path.display()))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS serve_leases (
            subdomain TEXT PRIMARY KEY,
            hostname TEXT NOT NULL,
            owner_email TEXT NOT NULL,
            tunnel_id TEXT NOT NULL,
            tunnel_token TEXT NOT NULL,
            origin TEXT NOT NULL,
            port INTEGER NOT NULL,
            created_at TEXT NOT NULL
        );",
    )
    .map_err(|e| format!("init serve_leases table: {e}"))?;
    Ok(conn)
}

/// Subdomain rules: lowercase a-z0-9 and hyphens, 2..=63 chars, must not start
/// or end with a hyphen. Reserved names that would collide with the public
/// estate or control-plane hostnames are rejected up front.
pub fn validate_subdomain(subdomain: &str) -> Result<String, String> {
    let s = subdomain.trim().to_lowercase();
    if s.len() < 2 || s.len() > 63 {
        return Err("subdomain must be 2-63 characters".to_string());
    }
    if !s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
        return Err("subdomain may only contain lowercase letters, digits, and hyphens".to_string());
    }
    if s.starts_with('-') || s.ends_with('-') {
        return Err("subdomain must not start or end with a hyphen".to_string());
    }
    let reserved = [
        "www", "api", "app", "install", "docs", "dashboard", "admin", "auth", "staging",
        "stage", "dev", "test", "mail", "webhook", "registry", "status", "support",
    ];
    if reserved.contains(&s.as_str()) {
        return Err(format!("subdomain \"{s}\" is reserved"));
    }
    Ok(s)
}

pub fn get_lease(subdomain: &str) -> Option<ServeLease> {
    let conn = open_db().ok()?;
    let subdomain = subdomain.to_lowercase();
    conn.query_row(
        "SELECT subdomain, hostname, owner_email, tunnel_id, tunnel_token, origin, port, created_at FROM serve_leases WHERE subdomain = ?1",
        rusqlite::params![subdomain],
        |row| {
            Ok(ServeLease {
                subdomain: row.get(0)?,
                hostname: row.get(1)?,
                owner_email: row.get(2)?,
                tunnel_id: row.get(3)?,
                tunnel_token: row.get(4)?,
                origin: row.get(5)?,
                port: row.get::<_, i64>(6)? as u16,
                created_at: row.get(7)?,
            })
        },
    )
    .ok()
}

pub fn list_leases() -> Vec<ServeLease> {
    let Ok(conn) = open_db() else {
        return Vec::new();
    };
    let Ok(mut stmt) = conn.prepare("SELECT subdomain, hostname, owner_email, tunnel_id, tunnel_token, origin, port, created_at FROM serve_leases ORDER BY created_at DESC") else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map([], |row| {
        Ok(ServeLease {
            subdomain: row.get(0)?,
            hostname: row.get(1)?,
            owner_email: row.get(2)?,
            tunnel_id: row.get(3)?,
            tunnel_token: row.get(4)?,
            origin: row.get(5)?,
            port: row.get::<_, i64>(6)? as u16,
            created_at: row.get(7)?,
        })
    }) else {
        return Vec::new();
    };
    rows.filter_map(|r| r.ok()).collect()
}

pub fn insert_lease(lease: &ServeLease) -> Result<(), String> {
    let conn = open_db()?;
    conn.execute(
        "INSERT OR REPLACE INTO serve_leases (subdomain, hostname, owner_email, tunnel_id, tunnel_token, origin, port, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        rusqlite::params![
            lease.subdomain,
            lease.hostname,
            lease.owner_email,
            lease.tunnel_id,
            lease.tunnel_token,
            lease.origin,
            lease.port as i64,
            lease.created_at,
        ],
    )
    .map_err(|e| format!("insert serve lease: {e}"))?;
    Ok(())
}

pub fn delete_lease(subdomain: &str) -> Result<bool, String> {
    let conn = open_db()?;
    let n = conn
        .execute("DELETE FROM serve_leases WHERE subdomain = ?1", rusqlite::params![subdomain.to_lowercase()])
        .map_err(|e| format!("delete serve lease: {e}"))?;
    Ok(n > 0)
}

pub fn now_lease() -> String {
    util_now()
}

/// Cloudflare account + zone used for temporary serve hostnames. The zone is
/// configurable (default getecosphere.com) so a host can serve on its own
/// domain without code changes; the account follows the standard named-account
/// convention (empty = unsuffixed CF_* env).
pub fn default_account() -> String {
    std::env::var("ECO_SERVE_CF_ACCOUNT").unwrap_or_else(|_| "getecosphere".to_string())
}

pub fn default_zone() -> String {
    std::env::var("ECO_SERVE_ZONE").unwrap_or_else(|_| "getecosphere.com".to_string())
}

/// Reserve a temporary public hostname for a local dev app.
///
/// Flow (all Cloudflare work happens host-side where the API creds live):
///   1. validate + conflict-check the subdomain (lease table + authoritative
///      DNS lookup);
///   2. create/reuse a dedicated remote tunnel for this subdomain;
///   3. upsert the CNAME `<subdomain>.getecosphere.com` → tunnel;
///   4. record the lease so later requests conflict with a clear error.
///
/// Returns (hostname, tunnel_token). `account` selects the Cloudflare account
/// (default: the unsuffixed CF_* env); the public zone is `<zone>` if given.
pub fn reserve_subdomain(
    subdomain: &str,
    owner_email: &str,
    origin: &str,
    port: u16,
    account: &str,
    zone: &str,
) -> Result<(String, String), String> {
    let sub = validate_subdomain(subdomain)?;
    let hostname = format!("{sub}.{zone}");

    if let Some(existing) = get_lease(&sub) {
        return Err(format!(
            "subdomain \"{sub}\" is already reserved by {}{}. Pick another name or release it first.",
            existing.owner_email,
            if existing.owner_email == owner_email { " (this account)" } else { "" }
        ));
    }

    // Authoritative DNS check: an existing CNAME/record for this hostname in
    // the zone means something else already claims it.
    let env = crate::cloudflare::get_cloudflare_env(account);
    if !env.zone_id.is_empty() {
        let encoded = url_encode(&hostname);
        if let Ok(result) = crate::cloudflare::cloudflare_api(
            &format!("/zones/{}/dns_records?name={encoded}&per_page=100", env.zone_id),
            account,
            "GET",
            None,
        ) {
            let taken = result.as_array().map(|arr| {
                arr.iter().any(|record| {
                    record.get("name").and_then(|n| n.as_str()) == Some(hostname.as_str())
                        && ["A", "AAAA", "CNAME"].contains(&record.get("type").and_then(|t| t.as_str()).unwrap_or(""))
                })
            }).unwrap_or(false);
            if taken {
                return Err(format!(
                    "subdomain \"{sub}\" is already in use at {hostname} (DNS record exists). Pick another name."
                ));
            }
        }
    }

    let tunnel_name = format!("serve-{sub}");
    let remote = crate::cloudflare::ensure_remote_tunnel(&tunnel_name, account)?;
    let tunnel_id = remote.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
    let tunnel_token = remote.get("tunnelToken").and_then(|t| t.as_str()).unwrap_or("").to_string();
    if tunnel_id.is_empty() || tunnel_token.is_empty() {
        return Err(format!("Cloudflare did not provision a tunnel for \"{sub}\"."));
    }

    crate::cloudflare::overwrite_dns_record_for_tunnel(&hostname, &tunnel_id, account)?;

    insert_lease(&ServeLease {
        subdomain: sub.clone(),
        hostname: hostname.clone(),
        owner_email: owner_email.to_string(),
        tunnel_id: tunnel_id.clone(),
        tunnel_token: tunnel_token.clone(),
        origin: origin.to_string(),
        port,
        created_at: now_lease(),
    })?;

    Ok((hostname, tunnel_token))
}

/// Release a reserved subdomain: remove the CNAME + tunnel ingress, optionally
/// delete the dedicated tunnel, and clear the lease. Returns the released
/// hostname if anything was removed.
pub fn release_subdomain(
    subdomain: &str,
    account: &str,
    zone: &str,
) -> Result<Option<String>, String> {
    let sub = subdomain.trim().to_lowercase();
    let Some(lease) = get_lease(&sub) else {
        return Ok(None);
    };

    let hostname = format!("{sub}.{zone}");
    let removed_dns = crate::cloudflare::remove_dns_record_for_tunnel(&hostname, &lease.tunnel_id, account).unwrap_or(false);

    // Only delete the tunnel if this hostname was its only public route.
    let mut delete_tunnel = removed_dns;
    if let Ok(config) = crate::cloudflare::get_remote_tunnel_config(&lease.tunnel_id, account) {
        let rules = config.get("ingress").and_then(|i| i.as_array()).cloned().unwrap_or_default();
        let hostname_rules = rules.iter().filter(|r| r.get("hostname").and_then(|h| h.as_str()) == Some(hostname.as_str())).count();
        let other_rules = rules.iter().filter(|r| {
            r.get("hostname").and_then(|h| h.as_str()).is_some() && r.get("hostname").and_then(|h| h.as_str()) != Some(hostname.as_str())
        }).count();
        if hostname_rules > 0 && other_rules == 0 {
            let _ = crate::cloudflare::remove_remote_tunnel(&lease.tunnel_id, account);
        } else {
            let _ = crate::cloudflare::remove_remote_tunnel_hostname(&lease.tunnel_id, &hostname, account);
        }
    }
    let _ = delete_tunnel;
    delete_lease(&sub)?;
    Ok(Some(lease.hostname))
}

fn url_encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}
