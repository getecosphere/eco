use std::collections::HashMap;

use crate::util;

const GITHUB_API: &str = "https://api.github.com";

fn github_url(path: &str) -> String {
    format!("{GITHUB_API}{path}")
}

fn github_agent() -> &'static str {
    "eco-cli"
}

fn github_request(
    path: &str,
    token: &str,
    method: &str,
    body: Option<&serde_json::Value>,
    allow_not_found: bool,
    user_agent: &str,
) -> Result<Option<serde_json::Value>, String> {
    if token.is_empty() {
        return Err("Missing GITHUB_TOKEN.".to_string());
    }
    let mut req = ureq::request(method, &github_url(path));
    req = req
        .set("Accept", "application/vnd.github+json")
        .set("Authorization", &format!("Bearer {token}"))
        .set("User-Agent", user_agent)
        .set("X-GitHub-Api-Version", "2022-11-28");
    let req = if let Some(b) = body {
        req.set("Content-Type", "application/json").send_string(&serde_json::to_string(b).unwrap())
    } else {
        req.call()
    };

    match req {
        Ok(response) => {
            if response.status() == 204 {
                return Ok(None);
            }
            let text = response.into_string().unwrap_or_default();
            let payload: Option<serde_json::Value> =
                if text.trim().is_empty() { None } else { serde_json::from_str(&text).ok() };
            Ok(payload)
        }
        Err(ureq::Error::Status(code, response)) => {
            if code == 404 && allow_not_found {
                return Ok(None);
            }
            let text = response.into_string().unwrap_or_default();
            let message = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| v.get("message").cloned())
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| {
                    if text.trim().is_empty() {
                        format!("GitHub API request failed: {code}")
                    } else {
                        text
                    }
                });
            Err(message)
        }
        Err(ureq::Error::Transport(t)) => Err(format!("GitHub API transport error: {t}")),
    }
}

pub struct GithubRepoCoords {
    pub owner: String,
    pub repo: String,
    pub full_name: String,
}

pub fn parse_github_repo_coordinates(remote_url: &str) -> Result<GithubRepoCoords, String> {
    let (owner, repo) = util::parse_github_coordinates(remote_url)?;
    let full_name = format!("{owner}/{repo}");
    Ok(GithubRepoCoords {
        owner,
        repo,
        full_name,
    })
}

pub struct WebhookSyncResult {
    pub action: String,
    pub hook_id: String,
    pub removed_stale: usize,
}

/// Port of lib/github.js syncGithubPushWebhook.
pub fn sync_github_push_webhook(
    token: &str,
    owner: &str,
    repo: &str,
    webhook_url: &str,
    secret: &str,
    stale_webhook_hostname: &str,
) -> Result<WebhookSyncResult, String> {
    let path = format!("/repos/{owner}/{repo}/hooks");
    let hooks = github_request(&path, token, "GET", None, false, github_agent())?;
    let list: Vec<serde_json::Value> = hooks
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();

    let existing = list.iter().find(|hook| {
        hook.get("config")
            .and_then(|c| c.get("url"))
            .and_then(|u| u.as_str())
            .map(|url| url == webhook_url)
            .unwrap_or(false)
    });

    let mut removed_stale = 0usize;
    if !stale_webhook_hostname.is_empty() {
        let webhook_path = url::Url::parse(webhook_url)
            .map(|u| u.path().to_string())
            .unwrap_or_default();
        let stale_url = format!("https://{stale_webhook_hostname}{webhook_path}");
        for hook in list.iter() {
            let hook_url = hook
                .get("config")
                .and_then(|c| c.get("url"))
                .and_then(|u| u.as_str())
                .unwrap_or("");
            if hook_url == stale_url {
                if let Some(id) = hook.get("id").and_then(|i| i.as_str()).map(|s| s.to_string()).or_else(|| {
                    hook.get("id").and_then(|i| i.as_i64()).map(|n| n.to_string())
                }) {
                    let _ = github_request(
                        &format!("/repos/{owner}/{repo}/hooks/{id}"),
                        token,
                        "DELETE",
                        None,
                        false,
                        github_agent(),
                    );
                    removed_stale += 1;
                }
            }
        }
    }

    let body = serde_json::json!({
        "active": true,
        "events": ["push"],
        "config": {
            "url": webhook_url,
            "content_type": "json",
            "secret": secret,
            "insecure_ssl": "0"
        }
    });

    if let Some(existing) = existing {
        let hook_id = existing
            .get("id")
            .and_then(|i| i.as_i64().map(|n| n.to_string()))
            .unwrap_or_default();
        github_request(
            &format!("/repos/{owner}/{repo}/hooks/{hook_id}"),
            token,
            "PATCH",
            Some(&body),
            false,
            github_agent(),
        )?;
        return Ok(WebhookSyncResult {
            action: "updated".to_string(),
            hook_id,
            removed_stale,
        });
    }

    let created =
        github_request(&path, token, "POST", Some(&body), false, github_agent())?;
    let hook_id = created
        .as_ref()
        .and_then(|v| v.get("id"))
        .and_then(|i| i.as_i64().map(|n| n.to_string()))
        .unwrap_or_default();
    Ok(WebhookSyncResult {
        action: "created".to_string(),
        hook_id,
        removed_stale,
    })
}

/// Inspect GitHub repos by name using ECO_GITHUB_API_KEY. Returns
/// (login, Vec<(name, exists, clone_url, ssh_url)>).
pub fn inspect_github_repositories(
    names: &[String],
) -> Result<(String, Vec<GithubRepoInfo>), String> {
    let token = util::env_var_or("ECO_GITHUB_API_KEY", "");
    if token.is_empty() {
        return Err("Missing ECO_GITHUB_API_KEY.".to_string());
    }
    let user = github_request("/user", &token, "GET", None, false, github_agent())?;
    let login = user
        .as_ref()
        .and_then(|v| v.get("login"))
        .and_then(|l| l.as_str().map(|s| s.to_string()))
        .ok_or_else(|| "GitHub API did not return a login.".to_string())?;

    let mut infos = Vec::new();
    for name in names {
        let repo =
            github_request(&format!("/repos/{login}/{name}"), &token, "GET", None, true, github_agent())?;
        match repo {
            Some(v) => infos.push(GithubRepoInfo {
                name: name.clone(),
                exists: true,
                clone_url: v.get("clone_url").and_then(|u| u.as_str()).unwrap_or("").to_string(),
                ssh_url: v.get("ssh_url").and_then(|u| u.as_str()).unwrap_or("").to_string(),
                html_url: v.get("html_url").and_then(|u| u.as_str()).unwrap_or("").to_string(),
            }),
            None => infos.push(GithubRepoInfo {
                name: name.clone(),
                exists: false,
                clone_url: String::new(),
                ssh_url: String::new(),
                html_url: String::new(),
            }),
        }
    }
    Ok((login, infos))
}

#[derive(Debug, Clone)]
pub struct GithubRepoInfo {
    pub name: String,
    pub exists: bool,
    pub clone_url: String,
    pub ssh_url: String,
    pub html_url: String,
}

pub fn create_github_repository(name: &str) -> Result<GithubRepoInfo, String> {
    let token = util::env_var_or("ECO_GITHUB_API_KEY", "");
    if token.is_empty() {
        return Err("ECO_GITHUB_API_KEY is required to create and push project repositories.".to_string());
    }
    let body = serde_json::json!({
        "name": name,
        "private": true,
        "auto_init": false,
        "description": format!("eco-managed repository for {name}")
    });
    let created = github_request("/user/repos", &token, "POST", Some(&body), false, github_agent())?;
    let created = created.ok_or_else(|| "GitHub did not return a repository.".to_string())?;
    Ok(GithubRepoInfo {
        name: name.to_string(),
        exists: true,
        clone_url: created.get("clone_url").and_then(|u| u.as_str()).unwrap_or("").to_string(),
        ssh_url: created.get("ssh_url").and_then(|u| u.as_str()).unwrap_or("").to_string(),
        html_url: created.get("html_url").and_then(|u| u.as_str()).unwrap_or("").to_string(),
    })
}

pub fn authenticated_github_url(repository: &GithubRepoInfo) -> String {
    let token = util::env_var_or("ECO_GITHUB_API_KEY", "");
    repository
        .clone_url
        .replacen("https://", &format!("https://x-access-token:{}@", url_encode(&token)), 1)
}

pub fn url_encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

/// A simple JSON HTTP GET used by webhook-clean's Cloudflare DoH lookup.
pub fn dns_resolves_via_doh(hostname: &str) -> bool {
    let url = format!(
        "https://cloudflare-dns.com/dns-query?name={}&type=A",
        url_encode(hostname)
    );
    match ureq::get(&url).set("accept", "application/dns-json").timeout(std::time::Duration::from_secs(8)).call()
    {
        Ok(response) => {
            let text = response.into_string().unwrap_or_default();
            serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| v.get("Answer").cloned())
                .and_then(|a| a.as_array().cloned())
                .map(|answers| answers.iter().any(|a| a.get("type").and_then(|t| t.as_i64()) == Some(1)))
                .unwrap_or(false)
        }
        Err(_) => false,
    }
}

pub fn env_map() -> HashMap<String, String> {
    std::env::vars().collect()
}

/// Generic JSON request against an arbitrary URL (used by webhook-clean for
/// GitHub and Cloudflare API calls with explicit headers).
pub fn github_request_public(
    url: &str,
    token: &str,
    method: &str,
    body: Option<&serde_json::Value>,
    user_agent: &str,
) -> Result<Option<serde_json::Value>, String> {
    let req = ureq::request(method, url)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Accept", "application/vnd.github+json")
        .set("User-Agent", user_agent)
        .set("X-GitHub-Api-Version", "2022-11-28");
    let req = if let Some(b) = body {
        req.set("Content-Type", "application/json")
            .send_string(&serde_json::to_string(b).unwrap_or_default())
    } else {
        req.call()
    };
    match req {
        Ok(response) => {
            let text = response.into_string().unwrap_or_default();
            let payload: Option<serde_json::Value> =
                if text.trim().is_empty() { None } else { serde_json::from_str(&text).ok() };
            Ok(payload)
        }
        Err(ureq::Error::Status(code, response)) => {
            let text = response.into_string().unwrap_or_default();
            let message = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| v.get("message").cloned())
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| {
                    format!("GitHub API {code} {url}: {}", if text.trim().is_empty() { "request failed" } else { text.trim() })
                });
            Err(message)
        }
        Err(ureq::Error::Transport(t)) => Err(format!("HTTP transport error: {t}")),
    }
}

/// Generic Cloudflare JSON request (Bearer token) for webhook-clean.
pub fn cf_request(
    url: &str,
    token: &str,
    method: &str,
    body: Option<&serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let req = ureq::request(method, url).set("Authorization", &format!("Bearer {token}"));
    let req = if let Some(b) = body {
        req.set("Content-Type", "application/json")
            .send_string(&serde_json::to_string(b).unwrap_or_default())
    } else {
        req.call()
    };
    match req {
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
                    return Err(format!("Cloudflare API {url}: {message}"));
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
            Err(format!("Cloudflare API {code} {url}: {message}"))
        }
        Err(ureq::Error::Transport(t)) => Err(format!("Cloudflare transport error: {t}")),
    }
}

fn code_status(_payload: &serde_json::Value) -> String {
    String::new()
}

