// eco serve — temporary public URL for a locally-running dev app.
//
// `eco up dev` runs the estate on localhost; `eco serve <subdomain>` opens a
// public tunnel at https://<subdomain>.getecosphere.com that proxies to the
// local app. The heavy lifting (subdomain reservation, conflict check,
// Cloudflare DNS + tunnel) happens host-side in the eco serve agent; this
// client only calls the agent, records the assignment in ecompose.yml, and
// runs `cloudflared tunnel --token <token> --url http://localhost:<port>`.
//
// Because the agent binary (eco serve, port 8790) also lives behind this
// subcommand, dispatch is by the first positional: a bare subdomain (or
// `list`/`stop`) is the dev tunnel; agent flags (`--port`, `--host`,
// `gen-key`) keep the host-side server behavior.
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::commands::account::resolve_api_credentials;
use crate::ecompose;
use crate::util;

fn serve_help() {
    let text = r#"eco serve

Expose a locally-running dev app (started by `eco up dev`) through a temporary
public URL, e.g. https://mychat.getecosphere.com.

Usage:
  eco serve <subdomain> [--port <port>] [--release]
  eco serve stop <subdomain>
  eco serve list

  <subdomain>   The name before .getecosphere.com (lowercase letters, digits,
                hyphens). eco checks for conflicts before reserving it.
  --port        Local port of the app to expose (default: read from ecompose.yml,
                else 3000).
  --release     Reserve nothing — only tear down an existing assignment.
  stop <sub>    Release an active subdomain.
  list          Show all active serve assignments on this host.

The chosen subdomain is recorded in ecompose.yml (serve.subdomain) so a later
`eco up dev && eco serve` can reuse it. Press Ctrl+C to stop the tunnel and
release the subdomain.

Host-side (run on the Proxmox host, not a dev machine):
  eco serve gen-key            Generate an agent API key
  eco serve --port 8790        Run the eco serve agent server
"#;
    print!("{text}");
}

fn find_ecompose(start_dir: &PathBuf) -> Result<(PathBuf, String), String> {
    let file = ecompose::resolve_ecompose_file("", start_dir).or_else(|_| ecompose::resolve_ecompose_file(".", start_dir))?;
    let content = ecompose::read_text_file(&file).unwrap_or_default();
    let project = ecompose::parse_project_name(&content);
    Ok((file, project))
}

fn default_port_from_expose(content: &str) -> Option<String> {
    let expose = ecompose::parse_expose(content);
    let port = expose.target_port();
    if !port.is_empty() { Some(port) } else { None }
}

fn api_post_json(
    url: &str,
    api_key: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let response = match ureq::post(url)
        .set("Authorization", &format!("Bearer {api_key}"))
        .set("Content-Type", "application/json")
        .timeout(Duration::from_secs(30))
        .send_string(&serde_json::to_string(body).unwrap())
    {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            let text = r.into_string().unwrap_or_default();
            let parsed: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({"raw": text}));
            let msg = parsed.get("error").and_then(|e| e.as_str()).unwrap_or(&text).to_string();
            return Err(format!("HTTP {code}: {msg}"));
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
        Err(format!("HTTP {status}: {msg}"))
    }
}

fn api_delete_json(url: &str, api_key: &str) -> Result<serde_json::Value, String> {
    let response = match ureq::delete(url)
        .set("Authorization", &format!("Bearer {api_key}"))
        .timeout(Duration::from_secs(30))
        .call()
    {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            let text = r.into_string().unwrap_or_default();
            let parsed: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({"raw": text}));
            let msg = parsed.get("error").and_then(|e| e.as_str()).unwrap_or(&text).to_string();
            return Err(format!("HTTP {code}: {msg}"));
        }
        Err(ureq::Error::Transport(t)) => return Err(format!("network error: {t}")),
    };
    let status = response.status();
    let text = response.into_string().unwrap_or_default();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({"raw": text}));
    if (200..300).contains(&status) {
        Ok(value)
    } else {
        Err(format!("HTTP {status}: {value}"))
    }
}

fn api_get_json(url: &str, api_key: &str) -> Result<serde_json::Value, String> {
    let response = match ureq::get(url)
        .set("Authorization", &format!("Bearer {api_key}"))
        .timeout(Duration::from_secs(30))
        .call()
    {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            let text = r.into_string().unwrap_or_default();
            let parsed: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({"raw": text}));
            let msg = parsed.get("error").and_then(|e| e.as_str()).unwrap_or(&text).to_string();
            return Err(format!("HTTP {code}: {msg}"));
        }
        Err(ureq::Error::Transport(t)) => return Err(format!("network error: {t}")),
    };
    let status = response.status();
    let text = response.into_string().unwrap_or_default();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({"raw": text}));
    if (200..300).contains(&status) {
        Ok(value)
    } else {
        Err(format!("HTTP {status}: {value}"))
    }
}

fn run_list(api_url: &str, api_key: &str) -> Result<(), String> {
    let url = format!("{}/v1/serve", api_url.trim_end_matches('/'));
    let result = api_get_json(&url, api_key)?;
    let serves = result.get("serves").and_then(|s| s.as_array()).cloned().unwrap_or_default();
    if serves.is_empty() {
        util::println_stdout("No active serve assignments.");
        return Ok(());
    }
    util::println_stdout(&format!("Active serve assignments ({})", serves.len()));
    for s in &serves {
        let sub = s.get("subdomain").and_then(|v| v.as_str()).unwrap_or("");
        let hostname = s.get("hostname").and_then(|v| v.as_str()).unwrap_or("");
        let owner = s.get("owner_email").and_then(|v| v.as_str()).unwrap_or("");
        let port = s.get("port").and_then(|v| v.as_u64()).unwrap_or(0);
        util::println_stdout(&format!("  {sub:16} https://{hostname:<32} port={port:<6} owner={owner}"));
    }
    Ok(())
}

fn ensure_cloudflared() -> Result<(), String> {
    if util::run_capture("cloudflared", &["--version".to_string()], &util::current_dir())
        .map(|c| c.code == 0)
        .unwrap_or(false)
    {
        return Ok(());
    }
    util::println_stdout("Installing cloudflared (managed by eco)...");
    crate::commands::install::run_install(&["cloudflared".to_string()])
}

fn write_serve_block(ecompose_path: &PathBuf, subdomain: &str) -> Result<(), String> {
    let content = std::fs::read_to_string(ecompose_path).map_err(|e| format!("read {}: {e}", ecompose_path.display()))?;
    let lines: Vec<&str> = content.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut replaced = false;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.trim_start().starts_with("serve:") {
            out.push(format!("serve:"));
            out.push(format!("  subdomain: {subdomain}"));
            out.push(format!("  enabled: true"));
            // skip the old block (2-space-indented children)
            i += 1;
            while i < lines.len() && (lines[i].starts_with("  ") || lines[i].trim().is_empty()) {
                i += 1;
            }
            replaced = true;
            continue;
        }
        out.push(line.to_string());
        i += 1;
    }
    if !replaced {
        if !content.ends_with('\n') {
            out.push(String::new());
        }
        out.push(format!("serve:"));
        out.push(format!("  subdomain: {subdomain}"));
        out.push(format!("  enabled: true"));
    }
    let written = format!("{}\n", out.join("\n"));
    std::fs::write(ecompose_path, written).map_err(|e| format!("write {}: {e}", ecompose_path.display()))?;
    util::println_stdout(&format!("Recorded serve.subdomain={subdomain} in {}", ecompose_path.display()));
    Ok(())
}

pub fn run_serve(args: &[String]) -> Result<(), String> {
    // Host-side agent dispatch: eco serve gen-key / eco serve --port / --host.
    let first = args.first().map(|s| s.as_str()).unwrap_or("");
    if first == "gen-key"
        || first == "--port"
        || first == "--host"
        || (first.starts_with("--") && args.iter().any(|a| a == "--port"))
    {
        return crate::runtime::agent::run_serve(args);
    }

    let cwd = util::current_dir();
    let (ecompose_path, project) = match find_ecompose(&cwd) {
        Ok(v) => v,
        Err(_) => (PathBuf::new(), String::new()),
    };

    let mut subdomain = String::new();
    let mut port = String::new();
    let mut release_only = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--port" => {
                port = args.get(i + 1).cloned().unwrap_or_default();
                i += 2;
            }
            "--release" => {
                release_only = true;
                i += 1;
            }
            "stop" | "list" | "help" | "--help" | "-h" => {
                // handled below via first-arg match
                i += 1;
            }
            other => {
                if other.starts_with("--") {
                    return Err(format!("Unknown eco serve option: {other}"));
                }
                if subdomain.is_empty() {
                    subdomain = other.to_string();
                }
                i += 1;
            }
        }
    }

    if first == "help" || first == "--help" || first == "-h" || (first.is_empty() && subdomain.is_empty() && !release_only) {
        serve_help();
        return Ok(());
    }
    if first == "stop" {
        let sub = args.get(1).cloned().unwrap_or_default();
        if sub.is_empty() {
            return Err("usage: eco serve stop <subdomain>".to_string());
        }
        subdomain = sub;
        release_only = true;
    }
    if first == "list" {
        let (api_url, api_key) = resolve_api_credentials()?;
        return run_list(&api_url, &api_key);
    }
    if subdomain.is_empty() && !release_only {
        return Err("usage: eco serve <subdomain> [--port <port>]\nRun \"eco serve help\" for details.".to_string());
    }

    let (api_url, api_key) = resolve_api_credentials()?;
    let api_url = if api_url.is_empty() { "https://api.getecosphere.com".to_string() } else { api_url };
    let base = api_url.trim_end_matches('/').to_string();

    if release_only {
        let url = format!("{base}/v1/serve/{subdomain}");
        match api_delete_json(&url, &api_key) {
            Ok(v) => {
                let hostname = v.get("released").and_then(|r| r.as_str()).unwrap_or(&subdomain);
                util::println_stdout(&format!("Released https://{hostname}"));
                Ok(())
            }
            Err(e) => Err(e),
        }
    } else {
        run_tunnel(&base, &api_key, &ecompose_path, &project, &subdomain, &port)
    }
}

fn run_tunnel(
    api_url: &str,
    api_key: &str,
    ecompose_path: &PathBuf,
    project: &str,
    subdomain: &str,
    port_flag: &str,
) -> Result<(), String> {
    // Determine the local port: explicit flag > ecompose expose.target_port > 3000.
    let content = if !ecompose_path.as_os_str().is_empty() {
        std::fs::read_to_string(ecompose_path).unwrap_or_default()
    } else {
        String::new()
    };
    let port = if !port_flag.is_empty() {
        port_flag.to_string()
    } else if let Some(p) = default_port_from_expose(&content) {
        p
    } else {
        "3000".to_string()
    };
    let port_num: u16 = port.parse().map_err(|_| format!("invalid --port: {port}"))?;

    // Reserve through the host agent (conflict check + DNS + tunnel token).
    let origin = format!("http://localhost:{port_num}");
    util::println_stdout(&format!("Reserving {subdomain}.getecosphere.com -> {origin}..."));
    let body = serde_json::json!({
        "subdomain": subdomain,
        "origin": origin,
        "port": port_num,
    });
    let reserved = match api_post_json(&format!("{api_url}/v1/serve"), &api_key, &body) {
        Ok(v) => v,
        Err(e) if e.contains("already reserved") => {
            // Reuse an existing lease: `eco serve <name>` again after a
            // previous run (or a crash) should pick up the same tunnel token
            // instead of erroring. Only reuse when the hostname matches.
            util::println_stdout("Subdomain already reserved — reusing the existing tunnel.");
            let existing = api_get_json(&format!("{api_url}/v1/serve"), &api_key)?
                .get("serves")
                .and_then(|s| s.as_array())
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .find(|l| l.get("subdomain").and_then(|s| s.as_str()) == Some(subdomain))
                .ok_or_else(|| format!("subdomain \"{subdomain}\" is reserved by someone else; pick another name"))?;
            existing
        }
        Err(e) => return Err(e),
    };
    let hostname = reserved.get("hostname").and_then(|h| h.as_str()).unwrap_or("").to_string();
    let tunnel_token = reserved.get("tunnel_token").and_then(|t| t.as_str()).unwrap_or("").to_string();
    if hostname.is_empty() || tunnel_token.is_empty() {
        return Err("agent did not return a hostname/tunnel token".to_string());
    }

    if !ecompose_path.as_os_str().is_empty() && !project.is_empty() {
        let _ = write_serve_block(ecompose_path, subdomain);
    }

    ensure_cloudflared()?;

    let url = format!("https://{hostname}");
    util::println_stdout(&format!("\n  Public URL: {url}\n  Local app:  {origin}\n\n  Press Ctrl+C to stop the tunnel and release the subdomain.\n"));

    let status = Command::new("cloudflared")
        .args(["tunnel", "run", "--token", &tunnel_token, "--url", &origin])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| format!("cloudflared failed to start: {e} (is it installed?)"))?;

    // cloudflared exited: release the subdomain regardless of exit code.
    util::println_stdout("\nTunnel stopped. Releasing subdomain...");
    let _ = api_delete_json(&format!("{api_url}/v1/serve/{subdomain}"), &api_key);

    if status.success() {
        Ok(())
    } else {
        Err(util::describe_status("cloudflared", &status))
    }
}
