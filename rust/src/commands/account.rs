// eco account — `eco signup` / `eco login` / `eco logout` / `eco whoami`.
// Stores the API key + URL in ~/.eco/auth.json (chmod 600) so `eco up --remote`
// works with no manual env vars. Env overrides (ECO_API_URL / ECO_API_KEY) win.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const DEFAULT_API_URL: &str = "https://api.getecosphere.com";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StoredAuth {
    pub api_url: String,
    pub api_key: String,
    pub email: String,
}

fn auth_path() -> PathBuf {
    let home = util_home();
    PathBuf::from(home).join(".eco").join("auth.json")
}

fn util_home() -> String {
    std::env::var("HOME").unwrap_or_else(|_| ".".to_string())
}

pub fn read_stored_auth() -> Option<StoredAuth> {
    let path = auth_path();
    if !path.is_file() {
        return None;
    }
    std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok())
}

fn write_stored_auth(auth: &StoredAuth) -> Result<(), String> {
    let path = auth_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(auth).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| format!("write {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn resolve_api_url() -> String {
    crate::util::env_var_or("ECO_API_URL", "").trim().trim_end_matches('/').to_string()
}

fn resolve_api_key() -> String {
    crate::util::env_var_or("ECO_API_KEY", "").trim().to_string()
}

/// Returns (api_url, api_key): explicit env wins, else the stored login.
pub fn resolve_api_credentials() -> Result<(String, String), String> {
    let url = resolve_api_url();
    let key = resolve_api_key();
    if !url.is_empty() && !key.is_empty() {
        return Ok((url, key));
    }
    if let Some(auth) = read_stored_auth() {
        let url = if url.is_empty() { auth.api_url } else { url };
        let key = if key.is_empty() { auth.api_key } else { key };
        if !url.is_empty() && !key.is_empty() {
            return Ok((url, key));
        }
    }
    Err(
        "not logged in. Run `eco login` (or set ECO_API_URL + ECO_API_KEY).".to_string(),
    )
}

fn post_json(url: &str, body: &serde_json::Value) -> Result<serde_json::Value, String> {
    let response = match ureq::post(url).set("User-Agent", "eco-cli").set("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(30))
        .send_string(&serde_json::to_string(body).unwrap())
    {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            let text = r.into_string().unwrap_or_default();
            return Err(format!("HTTP {code}: {}", text.chars().take(200).collect::<String>()));
        }
        Err(ureq::Error::Transport(t)) => return Err(format!("network error: {t}")),
    };
    let status = response.status();
    let text = response.into_string().unwrap_or_default();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({"raw": text}));
    if (200..300).contains(&status) {
        Ok(value)
    } else {
        let msg = value.get("error").and_then(|e| e.as_str()).unwrap_or(&text).to_string();
        Err(msg)
    }
}

fn do_signup(api_url: &str, email: &str, password: &str) -> Result<(), String> {
    let url = format!("{api_url}/v1/account/signup");
    let result = post_json(&url, &serde_json::json!({"email": email, "password": password}))?;
    let api_key = result.get("api_key").and_then(|k| k.as_str()).unwrap_or("").to_string();
    if api_key.is_empty() {
        return Err("signup did not return an API key".to_string());
    }
    write_stored_auth(&StoredAuth { api_url: api_url.to_string(), api_key: api_key.clone(), email: email.to_string() })?;
    println!("Account created for {email} (free tier).");
    println!("API key saved to ~/.eco/auth.json. Run `eco up --remote` in your estate to deploy.");
    Ok(())
}

fn do_login(api_url: &str, email: &str, password: &str) -> Result<(), String> {
    let url = format!("{api_url}/v1/account/login");
    let result = post_json(&url, &serde_json::json!({"email": email, "password": password}))?;
    let api_key = result.get("api_key").and_then(|k| k.as_str()).unwrap_or("").to_string();
    if api_key.is_empty() {
        return Err("login did not return an API key".to_string());
    }
    write_stored_auth(&StoredAuth { api_url: api_url.to_string(), api_key, email: email.to_string() })?;
    println!("Logged in as {email}. API key saved — `eco up --remote` will now deploy.");
    Ok(())
}

fn readline(prompt: &str) -> Result<String, String> {
    use std::io::Write;
    print!("{prompt}");
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).map_err(|e| e.to_string())?;
    Ok(line.trim().to_string())
}

pub fn run_account(args: &[String]) -> Result<(), String> {
    let subcommand = args.first().map(|s| s.as_str()).unwrap_or("help");
    let rest = &args[1..];
    match subcommand {
        "signup" => {
            let email = rest.first().cloned().unwrap_or_else(|| readline("Email: ").unwrap_or_default());
            if email.is_empty() {
                return Err("usage: eco signup <email>".to_string());
            }
            let password = readline("Password (min 8 chars): ").unwrap_or_default();
            let api_url = resolve_api_url();
            let api_url = if api_url.is_empty() { DEFAULT_API_URL.to_string() } else { api_url };
            do_signup(&api_url, &email, &password)
        }
        "login" => {
            let email = rest.first().cloned().unwrap_or_else(|| readline("Email: ").unwrap_or_default());
            if email.is_empty() {
                return Err("usage: eco login <email>".to_string());
            }
            let password = readline("Password: ").unwrap_or_default();
            let api_url = resolve_api_url();
            let api_url = if api_url.is_empty() { DEFAULT_API_URL.to_string() } else { api_url };
            do_login(&api_url, &email, &password)
        }
        "logout" => {
            let path = auth_path();
            if path.is_file() {
                let _ = std::fs::remove_file(&path);
            }
            println!("Logged out.");
            Ok(())
        }
        "whoami" => match read_stored_auth() {
            Some(auth) => {
                println!("email: {}", auth.email);
                println!("api_url: {}", auth.api_url);
                println!("tier: free");
                Ok(())
            }
            None => {
                println!("not logged in (run `eco login` or `eco signup`).");
                Ok(())
            }
        },
        _ => {
            println!("eco account\n\nUsage:\n  eco signup <email>        create a free account + API key\n  eco login <email>         log in and save the API key\n  eco logout                remove the saved key\n  eco whoami                show the current account");
            Ok(())
        }
    }
}
