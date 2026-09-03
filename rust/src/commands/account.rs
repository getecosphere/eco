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
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
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
    crate::util::env_var_or("ECO_API_URL", "")
        .trim()
        .trim_end_matches('/')
        .to_string()
}

fn resolve_api_key() -> String {
    crate::util::env_var_or("ECO_API_KEY", "")
        .trim()
        .to_string()
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
    Err("Not logged in. Run `eco login` to connect your account.".to_string())
}

fn post_json(url: &str, body: &serde_json::Value) -> Result<serde_json::Value, String> {
    let response = match ureq::post(url)
        .set("User-Agent", "eco-cli")
        .set("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(30))
        .send_string(&serde_json::to_string(body).unwrap())
    {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            let text = r.into_string().unwrap_or_default();
            return Err(format!(
                "HTTP {code}: {}",
                text.chars().take(200).collect::<String>()
            ));
        }
        Err(ureq::Error::Transport(t)) => return Err(format!("network error: {t}")),
    };
    let status = response.status();
    let text = response.into_string().unwrap_or_default();
    let value: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({"raw": text}));
    if (200..300).contains(&status) {
        Ok(value)
    } else {
        let msg = value
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or(&text)
            .to_string();
        Err(msg)
    }
}

fn api_get(api_url: &str, api_key: &str, path: &str) -> Result<serde_json::Value, String> {
    let url = format!("{api_url}{path}");
    let response = match ureq::get(&url).set("User-Agent", "eco-cli").set("Authorization", &format!("Bearer {api_key}"))
        .timeout(std::time::Duration::from_secs(30))
        .call()
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

/// POST with an Authorization header (account or agent key) for the
/// account-authenticated endpoints (subscribe / admin plan set).
fn post_json_auth(url: &str, api_key: &str, body: &serde_json::Value) -> Result<serde_json::Value, String> {
    let response = match ureq::post(url).set("User-Agent", "eco-cli").set("Content-Type", "application/json")
        .set("Authorization", &format!("Bearer {api_key}"))
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
    let result = post_json(
        &url,
        &serde_json::json!({"email": email, "password": password}),
    )?;
    let api_key = result
        .get("api_key")
        .and_then(|k| k.as_str())
        .unwrap_or("")
        .to_string();
    if api_key.is_empty() {
        return Err("signup did not return an API key".to_string());
    }
    write_stored_auth(&StoredAuth {
        api_url: api_url.to_string(),
        api_key: api_key.clone(),
        email: email.to_string(),
    })?;
    println!("Account created for {email} (free tier).");
    println!("API key saved to ~/.eco/auth.json. Run `eco up --remote` in your estate to deploy.");
    Ok(())
}

fn readline(prompt: &str) -> Result<String, String> {
    use std::io::Write;
    print!("{prompt}");
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    Ok(line.trim().to_string())
}

// Password prompt with terminal echo suppressed (like `read -s`). Falls back
// to plain input on non-Unix (or when stdin is not a terminal — e.g. piped).
fn read_secret(prompt: &str) -> Result<String, String> {
    use std::io::Write;
    print!("{prompt}");
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("sh")
            .args(["-c", "stty -echo"])
            .status();
        let read_result = std::io::stdin().read_line(&mut line);
        let _ = std::process::Command::new("sh")
            .args(["-c", "stty echo"])
            .status();
        println!();
        read_result.map_err(|e| e.to_string())?;
    }
    #[cfg(not(unix))]
    {
        std::io::stdin()
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
    }
    Ok(line.trim().to_string())
}

// Open the login URL in the platform's default browser. Returns false when no
// browser opener is available (or it failed), so the caller can fall back to
// printing the URL for the user to open manually.
fn command_available(name: &str) -> bool {
    let path = std::env::var("PATH").unwrap_or_default();
    path.split(':')
        .any(|dir| !dir.is_empty() && std::path::Path::new(dir).join(name).is_file())
}

fn open_browser(url: &str) -> bool {
    let (program, args): (&str, Vec<String>) = if cfg!(target_os = "macos") {
        ("open", vec![url.to_string()])
    } else if cfg!(target_os = "linux") {
        for candidate in ["xdg-open", "sensible-browser", "x-www-browser"] {
            if command_available(candidate) {
                return std::process::Command::new(candidate)
                    .arg(url)
                    .spawn()
                    .map(|_| true)
                    .unwrap_or(false);
            }
        }
        return false;
    } else if cfg!(target_os = "windows") {
        (
            "rundll32",
            vec!["url.dll,FileProtocolHandler".to_string(), url.to_string()],
        )
    } else {
        return false;
    };
    std::process::Command::new(program)
        .args(&args)
        .spawn()
        .map(|_| true)
        .unwrap_or(false)
}

// Browser-based device login: create a pending session, open the sign-in page
// (or print the URL when no browser is available), then poll until the user
// signs in and the freshly issued API key is saved.
fn do_device_login(api_url: &str) -> Result<(), String> {
    let create_url = format!("{api_url}/v1/account/device-login");
    let created = post_json(&create_url, &serde_json::json!({}))?;
    let session_id = created
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let code = created
        .get("code")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if session_id.is_empty() || code.is_empty() {
        return Err(
            "could not start a login session — is the account service reachable?".to_string(),
        );
    }

    let login_url = format!("{api_url}/v1/account/device-login/{code}");
    println!("\nOpening your browser to sign in to Ecosphere.");
    println!("If the browser does not open, copy and paste this URL:\n");
    println!("  {login_url}\n");
    if !open_browser(&login_url) {
        println!("(no browser detected on this machine — open the URL above manually)");
    }

    let status_url = format!("{api_url}/v1/account/device-login/status");
    println!("Waiting for you to sign in… (this times out in 5 minutes)");
    for _ in 0..150 {
        std::thread::sleep(std::time::Duration::from_secs(2));
        match post_json(&status_url, &serde_json::json!({"session_id": session_id})) {
            Ok(status) => match status.get("status").and_then(|s| s.as_str()).unwrap_or("") {
                "success" => {
                    let email = status
                        .get("email")
                        .and_then(|e| e.as_str())
                        .unwrap_or("")
                        .to_string();
                    let api_key = status
                        .get("api_key")
                        .and_then(|k| k.as_str())
                        .unwrap_or("")
                        .to_string();
                    write_stored_auth(&StoredAuth {
                        api_url: api_url.to_string(),
                        api_key,
                        email: email.clone(),
                    })?;
                    println!("\nLogged in as {email}. `eco up --remote` will now deploy.");
                    return Ok(());
                }
                "expired" => {
                    return Err(
                        "The login link expired. Run `eco login` to start a new one.".to_string(),
                    );
                }
                _ => {}
            },
            Err(_) => {}
        }
    }
    Err("Timed out waiting for sign-in. Run `eco login` to try again.".to_string())
}

fn do_login(api_url: &str, email: &str, password: &str) -> Result<(), String> {
    let url = format!("{api_url}/v1/account/login");
    let result = post_json(
        &url,
        &serde_json::json!({"email": email, "password": password}),
    )?;
    let api_key = result
        .get("api_key")
        .and_then(|k| k.as_str())
        .unwrap_or("")
        .to_string();
    if api_key.is_empty() {
        return Err("login did not return an API key".to_string());
    }
    write_stored_auth(&StoredAuth {
        api_url: api_url.to_string(),
        api_key,
        email: email.to_string(),
    })?;
    println!("Logged in as {email}. API key saved — `eco up --remote` will now deploy.");
    Ok(())
}

pub fn run_account(args: &[String]) -> Result<(), String> {
    let subcommand = args.first().map(|s| s.as_str()).unwrap_or("help");
    let rest = &args[1..];
    let api_url = {
        let url = resolve_api_url();
        if !url.is_empty() {
            url
        } else if let Some(auth) = read_stored_auth() {
            auth.api_url
        } else {
            DEFAULT_API_URL.to_string()
        }
    };
    match subcommand {
        "signup" => {
            let email = rest
                .first()
                .cloned()
                .unwrap_or_else(|| readline("Email: ").unwrap_or_default());
            if email.is_empty() {
                return Err("usage: eco signup <email>".to_string());
            }
            let password = read_secret("Password (min 8 chars): ").unwrap_or_default();
            do_signup(&api_url, &email, &password)
        }
        "login" => {
            if let Some(email) = rest.first() {
                let password = read_secret("Password: ").unwrap_or_default();
                do_login(&api_url, email, &password)
            } else {
                do_device_login(&api_url)
            }
        }
        "logout" => {
            let path = auth_path();
            if path.is_file() {
                let _ = std::fs::remove_file(&path);
            }
            println!("Logged out.");
            Ok(())
        }
        "plan" => {
            // eco plan — account plan management.
            //   eco plan list                       show the plan catalog
            //   eco plan subscribe <plan>           simulated self-serve payment → plan applied
            //   eco plan set <email> <plan>         admin op (agent key only)
            // Plan ids: free | tidur ($2) | selalu-on ($5) | growth ($19) | dedicated ($630)
            match rest.first().map(|s| s.as_str()) {
                Some("list") => {
                    let (_, api_key) = match resolve_api_credentials() {
                        Ok(k) => k,
                        Err(_) => (String::new(), String::new()),
                    };
                    let value = api_get(&api_url, &api_key, "/v1/account/plans")?;
                    let plans = value.get("plans").and_then(|p| p.as_array());
                    if let Some(plans) = plans {
                        println!("Eco plans (pricing-concept v2):");
                        for p in plans {
                            println!(
                                "  {:<10} {:>10}/mo   apps={}  mongo={}  postgres={}  storage={}GB",
                                p.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                                format!("${}", p.get("price_usd").and_then(|v| v.as_u64()).unwrap_or(0)),
                                p.get("apps").and_then(|v| v.as_u64()).unwrap_or(0),
                                p.get("mongo").and_then(|v| v.as_u64()).unwrap_or(0),
                                p.get("postgres").and_then(|v| v.as_u64()).unwrap_or(0),
                                p.get("storage_gb").and_then(|v| v.as_u64()).unwrap_or(0),
                            );
                        }
                    }
                    Ok(())
                }
                Some("subscribe") => {
                    let plan = rest.get(1).cloned().unwrap_or_default();
                    if plan.is_empty() {
                        return Err("usage: eco plan subscribe <free|tidur|selalu-on|growth|dedicated>".to_string());
                    }
                    let (_, api_key) = resolve_api_credentials()?;
                    let result = post_json_auth(
                        &format!("{api_url}/v1/account/subscribe"),
                        &api_key,
                        &serde_json::json!({ "plan": plan }),
                    )?;
                    println!(
                        "{}",
                        result.get("message").and_then(|m| m.as_str()).unwrap_or("subscribed")
                    );
                    Ok(())
                }
                Some("set") => {
                    if rest.len() == 3 {
                        let email = rest[1].clone();
                        let plan = rest[2].clone();
                        let (_, api_key) = resolve_api_credentials()?;
                        let _ = post_json_auth(
                            &format!("{api_url}/v1/account/plan"),
                            &api_key,
                            &serde_json::json!({ "email": email.clone(), "plan": plan.clone() }),
                        )?;
                        println!("Plan for {email} set to {plan}.");
                        Ok(())
                    } else {
                        Err("usage: eco plan set <email> <plan>".to_string())
                    }
                }
                _ => Err("usage: eco plan list | eco plan subscribe <plan> | eco plan set <email> <plan>".to_string()),
            }
        }
        "whoami" => match read_stored_auth() {
            Some(auth) => {
                println!("email: {}", auth.email);
                println!("api_url: {}", auth.api_url);
                // Ask the agent for the real tier instead of hardcoding "free".
                match api_get(&auth.api_url, &auth.api_key, "/v1/account/me") {
                    Ok(me) => {
                        let tier = me.get("tier").and_then(|t| t.as_str()).unwrap_or("free");
                        println!("tier:  {tier}");
                        let _ = me.get("email");
                    }
                    Err(_) => println!("tier:  (offline — run with the agent reachable)"),
                }
                Ok(())
            }
            None => {
                println!("not logged in (run `eco login` or `eco signup`).");
                Ok(())
            }
        },
        _ => {
            println!("eco account\n\nUsage:\n  eco signup <email>        create a free account + API key\n  eco login                 sign in via your browser\n  eco login <email>         sign in with email + password\n  eco logout                remove the saved key\n  eco whoami                show the current account (real tier)\n  eco plan list             show the plan catalog\n  eco plan subscribe <plan> simulate subscribing to a paid plan");
            Ok(())
        }
    }
}
