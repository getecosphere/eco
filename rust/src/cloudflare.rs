use std::collections::HashMap;

use crate::util;

const CF_API: &str = "https://api.cloudflare.com/client/v4";

pub struct CloudflareEnv {
    pub token: String,
    pub account_id: String,
    pub zone_id: String,
}

pub fn normalize_account_key(account: &str) -> String {
    let upper = account.trim().to_uppercase();
    let mut out = String::new();
    for c in upper.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    out.trim_matches('_').to_string()
}

pub fn get_cloudflare_env(account: &str) -> CloudflareEnv {
    let env = std::env::vars().collect::<HashMap<_, _>>();
    if account.is_empty() {
        CloudflareEnv {
            token: env.get("CF_API_TOKEN").cloned().unwrap_or_default(),
            account_id: env.get("CF_ACCOUNT_ID").cloned().unwrap_or_default(),
            zone_id: env.get("CF_ZONE_ID").cloned().unwrap_or_default(),
        }
    } else {
        let key = normalize_account_key(account);
        CloudflareEnv {
            token: env.get(&format!("CF_API_TOKEN_{key}")).cloned().unwrap_or_default(),
            account_id: env.get(&format!("CF_ACCOUNT_ID_{key}")).cloned().unwrap_or_default(),
            zone_id: env.get(&format!("CF_ZONE_ID_{key}")).cloned().unwrap_or_default(),
        }
    }
}

pub fn has_cloudflare_api_env(account: &str) -> bool {
    let env = get_cloudflare_env(account);
    !env.token.is_empty() && !env.account_id.is_empty() && !env.zone_id.is_empty()
}

pub fn cloudflare_api(
    pathname: &str,
    account: &str,
    method: &str,
    body: Option<&serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let env = get_cloudflare_env(account);
    if env.token.is_empty() {
        let var = if account.is_empty() {
            "CF_API_TOKEN".to_string()
        } else {
            format!("CF_API_TOKEN_{}", normalize_account_key(account))
        };
        return Err(format!(
            "{var} is required for Cloudflare API automation{}.",
            if account.is_empty() { String::new() } else { format!(" (account \"{account}\")") }
        ));
    }

    let url = format!("{CF_API}{pathname}");
    let mut req = ureq::request(method, &url)
        .set("Authorization", &format!("Bearer {}", env.token))
        .set("Content-Type", "application/json");
    let result = match body {
        Some(b) => req.send_string(&serde_json::to_string(b).unwrap()),
        None => req.call(),
    };

    match result {
        Ok(response) => {
            let text = response.into_string().unwrap_or_default();
            let payload: serde_json::Value =
                serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
            if let Some(success) = payload.get("success").and_then(|s| s.as_bool()) {
                if !success {
                    let message = payload
                        .get("errors")
                        .and_then(|e| e.as_array())
                        .map(|errors| {
                            errors
                                .iter()
                                .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
                                .collect::<Vec<_>>()
                                .join("; ")
                        })
                        .unwrap_or_else(|| "Cloudflare API error".to_string());
                    return Err(format!("Cloudflare API request failed: {message}"));
                }
            }
            Ok(payload.get("result").cloned().unwrap_or(serde_json::Value::Null))
        }
        Err(ureq::Error::Status(code, response)) => {
            let text = response.into_string().unwrap_or_default();
            let message = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| {
                    v.get("errors")
                        .and_then(|e| e.as_array())
                        .map(|errors| {
                            errors
                                .iter()
                                .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
                                .collect::<Vec<_>>()
                                .join("; ")
                        })
                })
                .unwrap_or_else(|| format!("HTTP {code}"));
            Err(format!("Cloudflare API {code} {pathname}: {message}"))
        }
        Err(ureq::Error::Transport(t)) => Err(format!("Cloudflare API transport error: {t}")),
    }
}

pub fn require_env_var(value: &str, base_name: &str, account: &str) -> Result<(), String> {
    if !value.is_empty() {
        return Ok(());
    }
    let var = if account.is_empty() {
        base_name.to_string()
    } else {
        format!("{base_name}_{}", normalize_account_key(account))
    };
    Err(format!(
        "{var} is required for Cloudflare automation{}.",
        if account.is_empty() { String::new() } else { format!(" (account \"{account}\")") }
    ))
}

pub fn slugify_tunnel_name(hostname: &str) -> String {
    let slug = hostname
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>();
    let trimmed = slug.trim_matches('-').to_string();
    let sliced: String = trimmed.chars().take(48).collect();
    if sliced.is_empty() {
        "eco-proxy".to_string()
    } else {
        sliced
    }
}

/// Public-facing tunnel target: <tunnel-id>.cfargotunnel.com
pub fn tunnel_target(tunnel_id: &str) -> String {
    format!("{tunnel_id}.cfargotunnel.com")
}

pub fn overwrite_dns_record_for_tunnel(
    hostname: &str,
    tunnel_id: &str,
    account: &str,
) -> Result<String, String> {
    let env = get_cloudflare_env(account);
    require_env_var(&env.zone_id, "CF_ZONE_ID", account)?;
    let target = tunnel_target(tunnel_id);
    let encoded = github::url_encode(hostname);
    let existing = cloudflare_api(
        &format!("/zones/{}/dns_records?name={encoded}&per_page=100", env.zone_id),
        account,
        "GET",
        None,
    )?;

    let matching = existing
        .as_array()
        .map(|arr| {
            arr.iter().find(|record| {
                record.get("name").and_then(|n| n.as_str()) == Some(hostname)
                    && ["A", "AAAA", "CNAME"]
                        .contains(&record.get("type").and_then(|t| t.as_str()).unwrap_or(""))
            })
        })
        .flatten()
        .cloned();

    let body = serde_json::json!({
        "type": "CNAME",
        "name": hostname,
        "content": target,
        "proxied": true,
        "ttl": 1
    });

    match matching {
        Some(record) => {
            let id = record.get("id").and_then(|i| i.as_str()).unwrap_or("");
            cloudflare_api(
                &format!("/zones/{}/dns_records/{id}", env.zone_id),
                account,
                "PUT",
                Some(&body),
            )?;
        }
        None => {
            cloudflare_api(
                &format!("/zones/{}/dns_records", env.zone_id),
                account,
                "POST",
                Some(&body),
            )?;
        }
    }

    // Verify authoritative zone record
    let verified = cloudflare_api(
        &format!("/zones/{}/dns_records?name={encoded}&per_page=100", env.zone_id),
        account,
        "GET",
        None,
    )?;
    let record = verified
        .as_array()
        .map(|arr| {
            arr.iter().find(|entry| {
                entry.get("name").and_then(|n| n.as_str()) == Some(hostname)
                    && entry.get("type").and_then(|t| t.as_str()) == Some("CNAME")
            })
        })
        .flatten()
        .cloned();
    match record {
        Some(r) => {
            let content = r.get("content").and_then(|c| c.as_str()).unwrap_or("").trim_end_matches('.');
            if content != target {
                return Err(format!(
                    "Cloudflare DNS verification failed for {hostname}; expected CNAME {target}."
                ));
            }
        }
        None => {
            return Err(format!(
                "Cloudflare DNS verification failed for {hostname}; expected CNAME {target}."
            ));
        }
    }
    Ok("updated".to_string())
}

pub fn remove_dns_record_for_tunnel(
    hostname: &str,
    tunnel_id: &str,
    account: &str,
) -> Result<bool, String> {
    let env = get_cloudflare_env(account);
    require_env_var(&env.zone_id, "CF_ZONE_ID", account)?;
    let target = tunnel_target(tunnel_id);
    let encoded = github::url_encode(hostname);
    let existing = cloudflare_api(
        &format!("/zones/{}/dns_records?name={encoded}&per_page=100", env.zone_id),
        account,
        "GET",
        None,
    )?;
    let matching = existing
        .as_array()
        .map(|arr| {
            arr.iter().find(|record| {
                record.get("name").and_then(|n| n.as_str()) == Some(hostname)
                    && record.get("type").and_then(|t| t.as_str()) == Some("CNAME")
                    && record
                        .get("content")
                        .and_then(|c| c.as_str())
                        .map(|c| c.trim_end_matches('.') == target)
                        .unwrap_or(false)
            })
        })
        .flatten()
        .cloned();
    match matching {
        Some(record) => {
            let id = record.get("id").and_then(|i| i.as_str()).unwrap_or("");
            cloudflare_api(
                &format!("/zones/{}/dns_records/{id}", env.zone_id),
                account,
                "DELETE",
                None,
            )?;
            Ok(true)
        }
        None => Ok(false),
    }
}

pub fn remove_remote_tunnel(tunnel_id: &str, account: &str) -> Result<(), String> {
    let env = get_cloudflare_env(account);
    require_env_var(&env.account_id, "CF_ACCOUNT_ID", account)?;
    cloudflare_api(
        &format!(
            "/accounts/{}/cfd_tunnel/{}",
            env.account_id,
            github::url_encode(tunnel_id)
        ),
        account,
        "DELETE",
        None,
    )?;
    Ok(())
}

pub fn get_tunnel_token_by_id(tunnel_id: &str, account: &str) -> Result<String, String> {
    let env = get_cloudflare_env(account);
    require_env_var(&env.account_id, "CF_ACCOUNT_ID", account)?;
    let result = cloudflare_api(
        &format!(
            "/accounts/{}/cfd_tunnel/{}/token",
            env.account_id,
            github::url_encode(tunnel_id)
        ),
        account,
        "GET",
        None,
    )?;
    let token = result
        .as_str()
        .map(|s| s.trim().to_string())
        .ok_or_else(|| format!("Cloudflare did not return a token for tunnel {tunnel_id}."))?;
    if token.is_empty() {
        return Err(format!("Cloudflare did not return a token for tunnel {tunnel_id}."));
    }
    Ok(token)
}

pub fn list_remote_tunnels_by_name(
    tunnel_name: &str,
    account: &str,
) -> Result<Vec<serde_json::Value>, String> {
    let env = get_cloudflare_env(account);
    require_env_var(&env.account_id, "CF_ACCOUNT_ID", account)?;
    let result = cloudflare_api(
        &format!(
            "/accounts/{}/cfd_tunnel?is_deleted=false&per_page=1000",
            env.account_id
        ),
        account,
        "GET",
        None,
    )?;
    Ok(result
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|tunnel| tunnel.get("name").and_then(|n| n.as_str()) == Some(tunnel_name))
        .collect())
}

pub fn ensure_remote_tunnel(
    tunnel_name: &str,
    account: &str,
) -> Result<serde_json::Value, String> {
    let env = get_cloudflare_env(account);
    require_env_var(&env.account_id, "CF_ACCOUNT_ID", account)?;
    let existing = list_remote_tunnels_by_name(tunnel_name, account)?;
    // Cloudflare returns `deleted_at: null` for live tunnels (field present
    // with a null value), so `.is_none()` on the Option is wrong here — a
    // present-null must be treated as "not deleted".
    let active = existing
        .iter()
        .find(|t| {
            t.get("deleted_at")
                .map(|v| v.is_null())
                .unwrap_or(true)
        })
        .cloned();
    if let Some(tunnel) = active {
        let tunnel_id = tunnel.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
        let token = tunnel
            .get("token")
            .and_then(|t| t.as_str())
            .map(|s| s.to_string())
            .or_else(|| get_tunnel_token_by_id(&tunnel_id, account).ok())
            .unwrap_or_default();
        let mut result = tunnel.clone();
        if let Some(obj) = result.as_object_mut() {
            obj.insert("tunnelToken".to_string(), serde_json::Value::String(token));
            obj.insert("created".to_string(), serde_json::Value::Bool(false));
        }
        return Ok(result);
    }

    let created = cloudflare_api(
        &format!("/accounts/{}/cfd_tunnel", env.account_id),
        account,
        "POST",
        Some(&serde_json::json!({
            "name": tunnel_name,
            "config_src": "cloudflare"
        })),
    )?;
    let tunnel_id = created.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
    let token = created
        .get("token")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .or_else(|| get_tunnel_token_by_id(&tunnel_id, account).ok())
        .unwrap_or_default();
    let mut result = created.clone();
    if let Some(obj) = result.as_object_mut() {
        obj.insert("tunnelToken".to_string(), serde_json::Value::String(token));
        obj.insert("created".to_string(), serde_json::Value::Bool(true));
        obj.insert("tunnelName".to_string(), serde_json::Value::String(tunnel_name.to_string()));
    }
    Ok(result)
}

pub fn get_remote_tunnel_config(tunnel_id: &str, account: &str) -> Result<serde_json::Value, String> {
    let env = get_cloudflare_env(account);
    require_env_var(&env.account_id, "CF_ACCOUNT_ID", account)?;
    let result = cloudflare_api(
        &format!(
            "/accounts/{}/cfd_tunnel/{}/configurations",
            env.account_id,
            github::url_encode(tunnel_id)
        ),
        account,
        "GET",
        None,
    )?;
    Ok(result.get("config").cloned().unwrap_or(serde_json::Value::Object(Default::default())))
}

pub fn put_remote_tunnel_config(
    tunnel_id: &str,
    hostname: &str,
    service_url: &str,
    account: &str,
) -> Result<(), String> {
    let env = get_cloudflare_env(account);
    require_env_var(&env.account_id, "CF_ACCOUNT_ID", account)?;
    let existing_config = get_remote_tunnel_config(tunnel_id, account).unwrap_or(serde_json::Value::Object(Default::default()));
    let ingress = existing_config
        .get("ingress")
        .and_then(|i| i.as_array().cloned())
        .unwrap_or_default();
    let non_fallback: Vec<serde_json::Value> = ingress
        .iter()
        .filter(|rule| rule.get("hostname").is_some() || rule.get("service").and_then(|s| s.as_str()) != Some("http_status:404"))
        .cloned()
        .collect();
    let fallback = ingress
        .iter()
        .find(|rule| rule.get("hostname").is_none() && rule.get("service").and_then(|s| s.as_str()) == Some("http_status:404"))
        .cloned()
        .unwrap_or_else(|| serde_json::json!({ "service": "http_status:404" }));

    let mut replaced = false;
    let mut next_ingress: Vec<serde_json::Value> = Vec::new();
    for rule in non_fallback {
        if rule.get("hostname").and_then(|h| h.as_str()) == Some(hostname) {
            replaced = true;
            let origin = rule.get("originRequest").cloned().unwrap_or(serde_json::Value::Object(Default::default()));
            let mut new_rule = serde_json::Map::new();
            new_rule.insert("hostname".to_string(), serde_json::Value::String(hostname.to_string()));
            new_rule.insert("service".to_string(), serde_json::Value::String(service_url.to_string()));
            new_rule.insert("originRequest".to_string(), origin);
            next_ingress.push(serde_json::Value::Object(new_rule));
        } else {
            next_ingress.push(rule);
        }
    }
    if !replaced {
        next_ingress.push(serde_json::json!({
            "hostname": hostname,
            "service": service_url,
            "originRequest": {}
        }));
    }
    next_ingress.push(fallback);

    let body = serde_json::json!({
        "config": {
            "ingress": next_ingress
        }
    });
    cloudflare_api(
        &format!(
            "/accounts/{}/cfd_tunnel/{}/configurations",
            env.account_id,
            github::url_encode(tunnel_id)
        ),
        account,
        "PUT",
        Some(&body),
    )?;

    // verify
    let verified = get_remote_tunnel_config(tunnel_id, account)?;
    let verified_rule = verified
        .get("ingress")
        .and_then(|i| i.as_array())
        .and_then(|arr| arr.iter().find(|rule| rule.get("hostname").and_then(|h| h.as_str()) == Some(hostname)))
        .cloned();
    match verified_rule {
        Some(rule) => {
            if rule.get("service").and_then(|s| s.as_str()) != Some(service_url) {
                return Err(format!(
                    "Cloudflare tunnel route verification failed for {hostname}; expected origin {service_url}."
                ));
            }
        }
        None => {
            return Err(format!(
                "Cloudflare tunnel route verification failed for {hostname}; expected origin {service_url}."
            ));
        }
    }
    Ok(())
}

pub fn remove_remote_tunnel_hostname(
    tunnel_id: &str,
    hostname: &str,
    account: &str,
) -> Result<bool, String> {
    let env = get_cloudflare_env(account);
    require_env_var(&env.account_id, "CF_ACCOUNT_ID", account)?;
    let existing_config = get_remote_tunnel_config(tunnel_id, account)?;
    let ingress = existing_config
        .get("ingress")
        .and_then(|i| i.as_array().cloned())
        .unwrap_or_default();
    let next_ingress: Vec<serde_json::Value> = ingress
        .iter()
        .filter(|rule| rule.get("hostname").and_then(|h| h.as_str()) != Some(hostname))
        .cloned()
        .collect();
    if next_ingress.len() == ingress.len() {
        return Ok(false);
    }
    let body = serde_json::json!({ "config": { "ingress": next_ingress } });
    cloudflare_api(
        &format!(
            "/accounts/{}/cfd_tunnel/{}/configurations",
            env.account_id,
            github::url_encode(tunnel_id)
        ),
        account,
        "PUT",
        Some(&body),
    )?;
    Ok(true)
}

/// deterministic per-account cloudflared config path / service name
pub fn cloudflared_config_path_for_account(account: &str) -> String {
    if account.is_empty() {
        "/etc/cloudflared/config.yml".to_string()
    } else {
        format!("/etc/cloudflared-{}/config.yml", slugify_tunnel_name(account))
    }
}

pub fn cloudflared_service_name_for_account(account: &str) -> String {
    if account.is_empty() {
        "cloudflared".to_string()
    } else {
        format!("cloudflared-{}", slugify_tunnel_name(account))
    }
}

mod github {
    pub fn url_encode(value: &str) -> String {
        url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
    }
}
