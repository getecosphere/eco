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

Run your app locally and publish it to a temporary public URL — e.g.
https://mychat.getecosphere.app.

Usage:
  eco serve [<subdomain>] [--port <port>] [--release]
  eco serve stop <subdomain>
  eco serve list

  eco serve         asks for a subdomain, runs your app (eco up dev), and
                    publishes it — ready for anyone with the URL.
  <subdomain>       The name before .getecosphere.app (lowercase letters,
                    digits, hyphens). eco checks for conflicts before reserving.
  --port            Local port to expose (default: ecompose.yml, else 3000).
  --release         Only tear down an existing assignment.
  stop <sub>        Release an active subdomain.
  list              Show all active serve assignments on this host.

While the tunnel runs, eco shows live Ingress/Egress transfer in green bold
plus your daily meter (free tier). Free sessions have no time limit; the daily
transfer resets every day at midnight UTC.

The chosen subdomain is recorded in ecompose.yml (serve.subdomain) so a later
`eco serve` can reuse it. Press Ctrl+C to stop the tunnel, release the
subdomain, and stop the local app.

The host-side agent (subdomain reservation, DNS, tunnel token, daily meter)
runs as the private eco-agent binary on the server ("eco serve --port 8790");
this build is client-only and talks to it over HTTP.
"#;
    print!("{text}");
}

fn find_ecompose(start_dir: &PathBuf) -> Result<(PathBuf, String), String> {
    let file = ecompose::resolve_ecompose_file("", start_dir)
        .or_else(|_| ecompose::resolve_ecompose_file(".", start_dir))?;
    let content = ecompose::read_text_file(&file).unwrap_or_default();
    let project = ecompose::parse_project_name(&content);
    Ok((file, project))
}

fn default_port_from_expose(content: &str) -> Option<String> {
    let expose = ecompose::parse_expose(content);
    let port = expose.target_port();
    if !port.is_empty() {
        Some(port)
    } else {
        None
    }
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
            let parsed: serde_json::Value =
                serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({"raw": text}));
            let msg = parsed
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or(&text)
                .to_string();
            return Err(format!("HTTP {code}: {msg}"));
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
            let parsed: serde_json::Value =
                serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({"raw": text}));
            let msg = parsed
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or(&text)
                .to_string();
            return Err(format!("HTTP {code}: {msg}"));
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
            let parsed: serde_json::Value =
                serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({"raw": text}));
            let msg = parsed
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or(&text)
                .to_string();
            return Err(format!("HTTP {code}: {msg}"));
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
        Err(format!("HTTP {status}: {value}"))
    }
}

fn run_list(api_url: &str, api_key: &str) -> Result<(), String> {
    let url = format!("{}/v1/serve", api_url.trim_end_matches('/'));
    let result = api_get_json(&url, api_key)?;
    let serves = result
        .get("serves")
        .and_then(|s| s.as_array())
        .cloned()
        .unwrap_or_default();
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
        util::println_stdout(&format!(
            "  {sub:16} https://{hostname:<32} port={port:<6} owner={owner}"
        ));
    }
    Ok(())
}

fn ensure_cloudflared() -> Result<(), String> {
    if util::run_capture(
        "cloudflared",
        &["--version".to_string()],
        &util::current_dir(),
    )
    .map(|c| c.code == 0)
    .unwrap_or(false)
    {
        return Ok(());
    }
    util::println_stdout("Installing cloudflared (managed by eco)...");
    crate::commands::install::run_install(&["cloudflared".to_string()])
}

fn write_serve_block(ecompose_path: &PathBuf, subdomain: &str) -> Result<(), String> {
    let content = std::fs::read_to_string(ecompose_path)
        .map_err(|e| format!("read {}: {e}", ecompose_path.display()))?;
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
    std::fs::write(ecompose_path, written)
        .map_err(|e| format!("write {}: {e}", ecompose_path.display()))?;
    util::println_stdout(&format!(
        "Recorded serve.subdomain={subdomain} in {}",
        ecompose_path.display()
    ));
    Ok(())
}

pub fn run_serve(args: &[String]) -> Result<(), String> {
    let first = args.first().map(|s| s.as_str()).unwrap_or("");
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

    if first == "help" || first == "--help" || first == "-h" {
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
        // `eco serve` with no subdomain asks for one interactively.
        if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            let sub = prompt_line("Pick a subdomain for your app (e.g. mychat): ")?;
            if sub.is_empty() {
                return Err("no subdomain given".to_string());
            }
            subdomain = sub;
        } else {
            return Err(
                "usage: eco serve <subdomain> [--port <port>]\nRun \"eco serve help\" for details."
                    .to_string(),
            );
        }
    }

    let (api_url, api_key) = resolve_api_credentials()?;
    let api_url = if api_url.is_empty() {
        "https://api.getecosphere.com".to_string()
    } else {
        api_url
    };
    let base = api_url.trim_end_matches('/').to_string();

    if release_only {
        let url = format!("{base}/v1/serve/{subdomain}");
        match api_delete_json(&url, &api_key) {
            Ok(v) => {
                let hostname = v
                    .get("released")
                    .and_then(|r| r.as_str())
                    .unwrap_or(&subdomain);
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
    // Determine the local port: explicit flag > ecompose expose.target_port;
    // otherwise discover the port eco allocated for the running app from PM2.
    let content = if !ecompose_path.as_os_str().is_empty() {
        std::fs::read_to_string(ecompose_path).unwrap_or_default()
    } else {
        String::new()
    };
    let explicit_port = if !port_flag.is_empty() {
        Some(
            port_flag
                .parse::<u16>()
                .map_err(|_| format!("invalid --port: {port_flag}"))?,
        )
    } else if let Some(p) = default_port_from_expose(&content) {
        Some(
            p.parse::<u16>()
                .map_err(|_| format!("invalid expose.target_port: {p}"))?,
        )
    } else {
        None
    };

    // Run the app locally (this is serve's core job): stop any running dev
    // instance for this project, start `eco up dev`, and wait for the target
    // port to accept connections before reserving a public URL.
    if !project.is_empty() {
        util::println_stdout("Preparing your app for the world…");
        util::println_stdout("Running your app locally…");
        stop_dev_apps(project);
        start_dev_app();
        // When the port isn't explicit (no --port / expose.target_port), read
        // the port eco actually allocated for the app from PM2 (e.g. 20000 for
        // a Next.js dev server) instead of assuming 3000. eco up dev starts
        // asynchronously, so poll until the app registers its PORT.
        let port_num = match explicit_port {
            Some(p) => p,
            None => wait_for_app_port(project, 3000, 180),
        };
        if !wait_for_port(port_num, 180) {
            return Err(format!(
                "Your app did not start listening on http://localhost:{port_num} within 3 minutes. Check the `eco up dev` output."
            ));
        }
        util::println_stdout(&format!("App is up on http://localhost:{port_num}"));
        return run_tunnel_after_app(
            api_url,
            api_key,
            ecompose_path,
            project,
            subdomain,
            port_num,
        );
    }

    let port_num = explicit_port.unwrap_or(3000);
    util::println_stdout("Preparing your app for the world…");
    run_tunnel_after_app(
        api_url,
        api_key,
        ecompose_path,
        project,
        subdomain,
        port_num,
    )
}

// Read the actual dev port eco allocated for a project's app from PM2.
fn discover_app_port(project: &str) -> Option<u16> {
    let Ok(captured) = util::run_capture("pm2", &["jlist".to_string()], &util::current_dir())
    else {
        return None;
    };
    let Ok(list) = serde_json::from_str::<serde_json::Value>(&captured.stdout) else {
        return None;
    };
    if let Some(apps) = list.as_array() {
        for app in apps {
            if let Some(name) = app.get("name").and_then(|n| n.as_str()) {
                if name.starts_with(&format!("{project}-")) {
                    if let Some(port) = app
                        .pointer("/pm2_env/env/PORT")
                        .and_then(|p| p.as_str())
                        .and_then(|s| s.parse::<u16>().ok())
                    {
                        return Some(port);
                    }
                    if let Some(port) = app
                        .pointer("/pm2_env/env/PORT")
                        .and_then(|p| p.as_u64())
                        .and_then(|p| u16::try_from(p).ok())
                    {
                        return Some(port);
                    }
                }
            }
        }
    }
    None
}

// Poll PM2 until the project's app registers with a PORT (eco up dev starts
// asynchronously), then return it.
fn wait_for_app_port(project: &str, fallback: u16, timeout_secs: u64) -> u16 {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    while Instant::now() < deadline {
        if let Some(port) = discover_app_port(project) {
            return port;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    fallback
}

fn run_tunnel_after_app(
    api_url: &str,
    api_key: &str,
    ecompose_path: &PathBuf,
    project: &str,
    subdomain: &str,
    port_num: u16,
) -> Result<(), String> {
    // Reserve through the host agent (conflict check + DNS + tunnel token +
    // metered daily quota).
    let origin = format!("http://localhost:{port_num}");
    util::println_stdout(&format!(
        "Reserving {subdomain}.getecosphere.app -> {origin}..."
    ));
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
                .ok_or_else(|| {
                    format!(
                        "subdomain \"{subdomain}\" is reserved by someone else; pick another name"
                    )
                })?;
            existing
        }
        Err(e) => return Err(e),
    };
    let hostname = reserved
        .get("hostname")
        .and_then(|h| h.as_str())
        .unwrap_or("")
        .to_string();
    let tunnel_token = reserved
        .get("tunnel_token")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    let email_relay_token = reserved
        .get("email_relay_token")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    if hostname.is_empty() || tunnel_token.is_empty() {
        return Err("agent did not return a hostname/tunnel token".to_string());
    }
    let quota_mb = reserved
        .get("quota_mb")
        .and_then(|q| q.as_u64())
        .unwrap_or(0);
    let used_mb = reserved
        .get("used_mb")
        .and_then(|q| q.as_u64())
        .unwrap_or(0);

    if !ecompose_path.as_os_str().is_empty() && !project.is_empty() {
        let _ = write_serve_block(ecompose_path, subdomain);
    }

    // The first `eco up dev` discovers the allocated port. Once the agent has
    // reserved the public hostname it can mint a mail capability scoped to
    // that hostname. Restart the local estate with that capability so Auth can
    // send recovery mail without receiving an agent API key or Brevo secret.
    if !project.is_empty() && !email_relay_token.is_empty() {
        let public_url = format!("https://{hostname}");
        restart_dev_app_with_email_relay(
            project,
            &format!("{api_url}/v1/auth-email"),
            &email_relay_token,
            &public_url,
        );
        if !wait_for_port(port_num, 180) {
            return Err(format!("Your app did not restart on http://localhost:{port_num} with its email relay capability."));
        }
    }

    ensure_cloudflared()?;

    // Count every byte the tunnel moves. cloudflared talks to a local counting
    // proxy that pipes to the real app, so Ingress/Egress are exact regardless
    // of protocol (HTTP, WebSocket, ...). Traffic never passes through the
    // agent — the client reports the totals on stop for the daily meter.
    let (proxy_port, meter) = start_counting_proxy(port_num)?;

    let url = format!("https://{hostname}");
    util::println_stdout(&format!("\n  Public URL: {url}\n  Local app:  {origin}\n"));
    if quota_mb > 0 {
        let (h, m) = minutes_until_utc_midnight();
        let pct = (used_mb as f64 / quota_mb as f64 * 100.0).max(0.0);
        if pct >= 90.0 {
            util::println_stdout(&format!(
                "  \x1b[1;33mDaily transfer: {used_mb} MB / {quota_mb} MB used \u{2014} resets in {h}h {m}m. Upgrade for unlimited.\x1b[0m"
            ));
        } else {
            util::println_stdout(&format!(
                "  Daily transfer: {used_mb} MB / {quota_mb} MB used today \u{00b7} resets in {h}h {m}m"
            ));
        }
        util::println_stdout(&format!(
            "  Free serve sessions have no time limit. When you want your estate always-on,\n  Starter is \x1b[1m$2/mo\x1b[0m \u{2014} hosted on our servers with 2 GB/mo transfer and no daily\n  resets. No pressure \u{2014} it will be here when you need it (getecosphere.com)."
        ));
    }
    util::println_stdout("  \n  Press Ctrl+C to stop the tunnel and release the subdomain.\n");

    let meter_handle = start_meter(&meter);

    // Keep the lease alive while the tunnel runs: the agent reclaims any lease
    // that stops heartbeating (hard kill / power loss), so this ping is what
    // makes the subdomain stay reserved.
    {
        let api_url = api_url.to_string();
        let api_key = api_key.to_string();
        let sub = subdomain.to_string();
        let done = meter.done.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(60));
            if done.load(Ordering::Relaxed) {
                break;
            }
            let _ = api_post_json(
                &format!("{api_url}/v1/serve/{sub}/heartbeat"),
                &api_key,
                &serde_json::json!({}),
            );
        });
    }

    // Keep the terminal clean: cloudflared's own logging goes to a temp file;
    // stdin stays inherited so Ctrl+C reaches it.
    let log_path = std::env::temp_dir().join(format!("eco-serve-{}.log", subdomain));
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("open serve log {}: {e}", log_path.display()))?;
    let status = Command::new("cloudflared")
        .args([
            "tunnel",
            "run",
            "--token",
            &tunnel_token,
            "--url",
            &format!("http://127.0.0.1:{proxy_port}"),
        ])
        .stdin(Stdio::inherit())
        .stdout(Stdio::from(
            log_file.try_clone().map_err(|e| e.to_string())?,
        ))
        .stderr(Stdio::from(log_file))
        .status()
        .map_err(|e| format!("cloudflared failed to start: {e} (is it installed?)"))?;

    meter.done.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = meter_handle.join();
    print!("\x1b[2A\r\x1b[2K\x1b[1;32mIngress:\x1b[0m {:.1} MB\n\x1b[2K\x1b[1;32mEgress:\x1b[0m {:.1} MB\n", bytes_to_mb(meter.ingress.load(Ordering::Relaxed)), bytes_to_mb(meter.egress.load(Ordering::Relaxed)));
    use std::io::Write as _;
    let _ = std::io::stdout().flush();

    // Report measured usage to the agent, then release the subdomain.
    util::println_stdout("\nTunnel stopped. Recording usage + releasing subdomain...");
    let _ = api_post_json(
        &format!("{api_url}/v1/serve/{subdomain}/usage"),
        &api_key,
        &serde_json::json!({
            "ingress_bytes": meter.ingress.load(Ordering::Relaxed),
            "egress_bytes": meter.egress.load(Ordering::Relaxed),
        }),
    );
    let _ = api_delete_json(&format!("{api_url}/v1/serve/{subdomain}"), &api_key);

    // Stop the local dev app we started.
    if !project.is_empty() {
        util::println_stdout("Stopping the local app…");
        stop_dev_apps(project);
    }

    if status.success() {
        Ok(())
    } else {
        Err(util::describe_status("cloudflared", &status))
    }
}

fn prompt_line(prompt: &str) -> Result<String, String> {
    use std::io::Write as _;
    print!("{prompt}");
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| format!("read input: {e}"))?;
    Ok(line.trim().to_string())
}

// Stop every PM2 dev app for this project (named `<project>-<service>`), so a
// re-run of `eco serve` starts a clean instance.
fn stop_dev_apps(project: &str) {
    let Ok(captured) = util::run_capture("pm2", &["jlist".to_string()], &util::current_dir())
    else {
        return;
    };
    let Ok(list) = serde_json::from_str::<serde_json::Value>(&captured.stdout) else {
        return;
    };
    if let Some(apps) = list.as_array() {
        for app in apps {
            if let Some(name) = app.get("name").and_then(|n| n.as_str()) {
                if name.starts_with(&format!("{project}-")) {
                    let _ = util::run_capture(
                        "pm2",
                        &["delete".to_string(), name.to_string()],
                        &util::current_dir(),
                    );
                }
            }
        }
    }
}

// Start the local dev app by running `eco up dev` in the background (PM2 keeps
// the services running after the command returns).
fn start_dev_app() {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("eco"));
    let _ = Command::new(exe).args(["up", "dev"]).spawn();
}

fn restart_dev_app_with_email_relay(
    project: &str,
    relay_url: &str,
    relay_token: &str,
    public_url: &str,
) {
    stop_dev_apps(project);
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("eco"));
    let _ = Command::new(exe)
        .args(["up", "dev"])
        .env("ECO_AUTH_EMAIL_RELAY_URL", relay_url)
        .env("ECO_AUTH_EMAIL_RELAY_TOKEN", relay_token)
        .env("ECO_AUTH_EMAIL_PUBLIC_URL", public_url)
        .spawn();
}

// Poll until the app's port accepts TCP connections (or the timeout passes).
fn wait_for_port(port: u16, timeout_secs: u64) -> bool {
    use std::net::TcpStream;
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    false
}

use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

struct TunnelMeter {
    ingress: Arc<AtomicU64>,
    egress: Arc<AtomicU64>,
    done: Arc<AtomicBool>,
}

fn bytes_to_mb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

// Time until the daily meter resets (midnight UTC) — a live countdown on the
// quota line so free users know exactly when their allowance refreshes.
fn minutes_until_utc_midnight() -> (u64, u64) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let remaining = 86400 - (now % 86400);
    (remaining / 3600, (remaining % 3600) / 60)
}

// A protocol-agnostic counting proxy: cloudflared connects here, it pipes to
// the app, and every byte is counted in its direction. Ingress = bytes coming
// in from visitors (cloudflared → app), Egress = bytes the app sends back.
fn start_counting_proxy(app_port: u16) -> Result<(u16, TunnelMeter), String> {
    let listener =
        TcpListener::bind(("127.0.0.1", 0)).map_err(|e| format!("bind counting proxy: {e}"))?;
    let proxy_port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let ingress = Arc::new(AtomicU64::new(0));
    let egress = Arc::new(AtomicU64::new(0));
    let meter = TunnelMeter {
        ingress: ingress.clone(),
        egress: egress.clone(),
        done: Arc::new(AtomicBool::new(false)),
    };
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(visitor) = stream else { break };
            let Ok(upstream) = TcpStream::connect(("127.0.0.1", app_port)) else {
                continue;
            };
            let _ = visitor.set_nodelay(true);
            let _ = upstream.set_nodelay(true);
            let Ok(visitor_w) = visitor.try_clone() else {
                continue;
            };
            let Ok(upstream_w) = upstream.try_clone() else {
                continue;
            };
            let ingress = ingress.clone();
            let egress = egress.clone();
            std::thread::spawn(move || {
                let _ = pipe(visitor, upstream_w, &ingress);
            });
            std::thread::spawn(move || {
                let _ = pipe(upstream, visitor_w, &egress);
            });
        }
    });
    Ok((proxy_port, meter))
}

fn pipe(mut src: TcpStream, mut dst: TcpStream, counter: &Arc<AtomicU64>) -> std::io::Result<()> {
    let mut buf = [0u8; 16384];
    loop {
        let n = src.read(&mut buf)?;
        if n == 0 {
            break;
        }
        dst.write_all(&buf[..n])?;
        counter.fetch_add(n as u64, Ordering::Relaxed);
    }
    Ok(())
}

// Live meter: two lines, updated in place every second, with the labels in
// green bold. Cursor gymnastics keep the terminal free of scroll spam.
fn start_meter(meter: &TunnelMeter) -> std::thread::JoinHandle<()> {
    let ingress = meter.ingress.clone();
    let egress = meter.egress.clone();
    let done = meter.done.clone();
    let mut first = true;
    std::thread::spawn(move || loop {
        if done.load(Ordering::Relaxed) {
            break;
        }
        let i = bytes_to_mb(ingress.load(Ordering::Relaxed));
        let e = bytes_to_mb(egress.load(Ordering::Relaxed));
        if first {
            print!("\x1b[1;32mIngress:\x1b[0m {i:.1} MB\n\x1b[1;32mEgress:\x1b[0m {e:.1} MB");
            first = false;
        } else {
            print!("\x1b[2A\r\x1b[2K\x1b[1;32mIngress:\x1b[0m {i:.1} MB\n\x1b[2K\x1b[1;32mEgress:\x1b[0m {e:.1} MB");
        }
        let _ = std::io::stdout().flush();
        std::thread::sleep(Duration::from_millis(1000));
    })
}
