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

