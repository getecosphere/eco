use crate::cloudflare;
use crate::util;
use std::path::Path;

fn proxy_help() {
    let text = r#"eco proxy

Manage ingress/proxy infrastructure helpers.

Usage:
  eco proxy migrate-cloudflared [proxy|ctid] [--dry-run] [--stop-host]
  eco proxy init-tunnel [proxy|ctid] <hostname> [--name <tunnel-name>] [--account <name>] [--dry-run]
  eco proxy tunnel-replicas <account> [count] [--target <ctid>] [--dry-run]
  eco prox tunnel-replicas <account> [count] [--target <ctid>] [--dry-run]

Options:
  --dry-run    Print the migration plan without executing it
  --stop-host  Stop and disable host-level cloudflared after CT service starts
  --name       Override the tunnel name for init-tunnel
  --account    Named Cloudflare account to use (reads CF_API_TOKEN_<NAME>,
               CF_ACCOUNT_ID_<NAME>, CF_ZONE_ID_<NAME> instead of the
               unsuffixed defaults) -- lets one host manage tunnels/DNS
               across multiple Cloudflare accounts. Matches ecompose.yml's
               expose.cloudflare_account.
  --target     Target proxy CT id or hostname (tunnel-replicas; default "proxy")

Examples:
  eco proxy migrate-cloudflared
  eco proxy migrate-cloudflared 100
  eco proxy migrate-cloudflared proxy --dry-run
  eco proxy migrate-cloudflared proxy --stop-host
  eco proxy init-tunnel proxy assessment.ktt.my.id
  eco proxy init-tunnel proxy training.jogjaitcamp.com --account jogjaitcamp
  eco proxy tunnel-replicas jogjaitcamp 3
  eco proxy tunnel-replicas jogjaitcamp 3 --target proxy --dry-run
  eco proxy tunnel-replicas jogjaitcamp
"#;
    print!("{text}");
}

fn path_exists(p: &Path) -> bool {
    p.exists()
}

fn run_command(command: &str, args: &[String], cwd: &Path) -> Result<(), String> {
    util::run_command(command, args, cwd)
}

fn run_capture(command: &str, args: &[String], cwd: &Path) -> Result<util::Captured, String> {
    util::run_capture(command, args, cwd)
}

async fn _noop() {}

fn resolve_ct_id_by_hostname(hostname: &str) -> Result<String, String> {
    let result = run_capture("pct", &["list".to_string()], &util::current_dir())?;
    if result.code != 0 {
        return Err(format!("pct list failed with code {}: {}", result.code, result.stderr.trim()));
    }
    for raw_line in result.stdout.split('\n') {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with("VMID") {
            continue;
        }
        let ctid = line.split_whitespace().next().unwrap_or("");
        if !ctid.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let config = run_capture("pct", &["config".to_string(), ctid.to_string()], &util::current_dir())?;
        if config.code != 0 {
            continue;
        }
        let found = config.stdout.lines().find_map(|l| {
            let l = l.trim();
            l.strip_prefix("hostname:")
        });
        if let Some(value) = found {
            if value.trim() == hostname {
                return Ok(ctid.to_string());
            }
        }
    }
    Err(format!("Cannot resolve CT by hostname \"{hostname}\" from pct list."))
}

fn resolve_ct_input(input: &str) -> Result<String, String> {
    if input.is_empty() {
        return resolve_ct_id_by_hostname("proxy");
    }
    if input.chars().all(|c| c.is_ascii_digit()) {
        return Ok(input.to_string());
    }
    resolve_ct_id_by_hostname(input)
}

fn ensure_ct_running(ctid: &str) -> Result<(), String> {
    let status = run_capture("pct", &["status".to_string(), ctid.to_string()], &util::current_dir())?;
    if status.code == 0 && status.stdout.contains("status: running") {
        return Ok(());
    }
    run_command("pct", &["start".to_string(), ctid.to_string()], &util::current_dir())
}

fn pct_exec(ctid: &str, command: &str) -> Result<(), String> {
    run_command(
        "pct",
        &["exec".to_string(), ctid.to_string(), "--".to_string(), "bash".to_string(), "-lc".to_string(), command.to_string()],
        &util::current_dir(),
    )
}

fn pct_exec_capture(ctid: &str, command: &str) -> Result<String, String> {
    let result = run_capture(
        "pct",
        &["exec".to_string(), ctid.to_string(), "--".to_string(), "bash".to_string(), "-lc".to_string(), command.to_string()],
        &util::current_dir(),
    )?;
    if result.code != 0 {
        return Err(format!(
            "pct exec {ctid} failed with code {}: {}",
            result.code,
            if result.stderr.trim().is_empty() { result.stdout.trim().to_string() } else { result.stderr.trim().to_string() }
        ));
    }
    Ok(result.stdout)
}

fn push_file_to_ct(ctid: &str, source_path: &str, target_path: &str) -> Result<(), String> {
    run_command("pct", &["push".to_string(), ctid.to_string(), source_path.to_string(), target_path.to_string()], &util::current_dir())
}

fn escape_single_quotes(value: &str) -> String {
    value.replace('\'', "'\\''")
}

fn resolve_tunnel_id_by_name(ctid: &str, tunnel_name: &str) -> Result<String, String> {
    let output = pct_exec_capture(
        ctid,
        &format!("cloudflared tunnel list 2>/dev/null | awk 'NR>1 && $2 == \"{tunnel_name}\" {{ print $1; exit }}'"),
    )?;
    let tunnel_id = output.trim().to_string();
    if tunnel_id.is_empty() {
        return Err(format!("Cannot resolve tunnel ID for \"{tunnel_name}\" inside CT {ctid}."));
    }
    Ok(tunnel_id)
}

async fn _unused(_: &str) {}

fn ensure_host_cloudflared_files() -> Result<(String, String, Vec<String>), String> {
    let config_path = "/etc/cloudflared/config.yml".to_string();
    if !path_exists(&Path::new(&config_path)) {
        return Err(format!("Missing host cloudflared config: {config_path}"));
    }
    let config_text = std::fs::read_to_string(&config_path).map_err(|e| e.to_string())?;
    let credentials_match = config_text.lines().find_map(|l| l.strip_prefix("credentials-file:"));
    match credentials_match {
        Some(raw) => {
            let credentials_path = raw.trim().trim_matches('"').trim_matches('\'').to_string();
            if !path_exists(&Path::new(&credentials_path)) {
                return Err(format!("Missing host cloudflared credentials file: {credentials_path}"));
            }
            Ok((config_path, "file".to_string(), vec![credentials_path]))
        }
        None => Ok((config_path, "token".to_string(), Vec::new())),
    }
}

fn write_temp_service_file(tunnel_token: &str, config_path: &str) -> Result<(String, String), String> {
    let exec_start = if tunnel_token.is_empty() {
        format!("ExecStart=/usr/bin/cloudflared --no-autoupdate --config {config_path} tunnel run")
    } else {
        format!("ExecStart=/usr/bin/cloudflared --no-autoupdate --config {config_path} tunnel run --token {tunnel_token}")
    };
    let service_text = format!(
        "[Unit]\nDescription=cloudflared\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nTimeoutStartSec=15\nType=notify\n{exec_start}\nRestart=on-failure\nRestartSec=5s\n\n[Install]\nWantedBy=multi-user.target\n"
    );
    let temp_dir = std::env::temp_dir().join(format!("eco-cloudflared-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&temp_dir);
    let file_path = temp_dir.join("cloudflared.service");
    std::fs::write(&file_path, service_text).map_err(|e| e.to_string())?;
    Ok((temp_dir.display().to_string(), file_path.display().to_string()))
}

fn install_ct_cloudflared_service(ctid: &str, tunnel_token: &str, service_name: &str, config_path: &str) -> Result<(), String> {
    let (temp_dir, file_path) = write_temp_service_file(tunnel_token, config_path)?;
    let result = push_file_to_ct(ctid, &file_path, &format!("/etc/systemd/system/{service_name}.service"));
    let _ = std::fs::remove_dir_all(&temp_dir);
    result
}

fn verify_cloudflared_service(ctid: &str, service_name: &str) -> Result<(), String> {
    pct_exec(
        ctid,
        &format!(
            "if systemctl is-active --quiet {service_name}; then exit 0; fi;\nsystemctl --no-pager --full status {service_name} || true;\njournalctl -u {service_name} -n 80 --no-pager;\nexit 1"
        ),
    )
}

fn write_tunnel_config(
    ctid: &str,
    tunnel_id: &str,
    tunnel_token: &str,
    tunnel_name: &str,
    hostname: &str,
    service_url: &str,
    config_path: &str,
) -> Result<(), String> {
    let config_text = format!(
        "tunnel: {tunnel_token}\n# eco-tunnel-id: {tunnel_id}\n# eco-tunnel-name: {tunnel_name}\n\noriginRequest:\n  noTLSVerify: true\n\ningress:\n  - hostname: {hostname}\n    service: {service_url}\n  - service: http_status:404\n"
    );
    let config_dir = std::path::Path::new(config_path)
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "/etc/cloudflared".to_string());
    let shell = format!(
        "mkdir -p {config_dir}\ncat >{config_path} <<'EOF'\n{}\nEOF",
        config_text.trim_end()
    );
    pct_exec(ctid, &shell)
}

fn ensure_cloudflared_installed(ctid: &str) -> Result<(), String> {
    pct_exec(
        ctid,
        "if ! command -v cloudflared >/dev/null 2>&1; then\n  apt-get update;\n  apt-get install -y curl ca-certificates;\n  arch=$(dpkg --print-architecture);\n  case \"$arch\" in\n    amd64) pkg=cloudflared-linux-amd64.deb ;;\n    arm64) pkg=cloudflared-linux-arm64.deb ;;\n    *) echo \"Unsupported architecture for cloudflared: $arch\" >&2; exit 1 ;;\n  esac;\n  curl -fsSL \"https://github.com/cloudflare/cloudflared/releases/latest/download/${pkg}\" -o /tmp/cloudflared.deb;\n  apt-get install -y /tmp/cloudflared.deb;\n  rm -f /tmp/cloudflared.deb;\nfi",
    )
}

pub struct ProxyTunnelResult {
    pub ctid: String,
    pub tunnel_name: String,
    pub tunnel_id: String,
    pub config_path: String,
    pub service_name: String,
}

/// Bootstrap a dedicated tunnel for a hostname in a CT, using Cloudflare API
/// env when available (called from up.rs expose flows).
pub fn ensure_proxy_tunnel(
    target: &str,
    hostname: &str,
    tunnel_name: &str,
    service_url: &str,
    _non_interactive: bool,
    cloudflare_account: &str,
) -> Result<ProxyTunnelResult, String> {
    let ctid = resolve_ct_input(target)?;
    let resolved_tunnel_name = if tunnel_name.is_empty() {
        cloudflare::slugify_tunnel_name(hostname)
    } else {
        tunnel_name.to_string()
    };
    let config_path = cloudflare::cloudflared_config_path_for_account(cloudflare_account);
    let service_name = cloudflare::cloudflared_service_name_for_account(cloudflare_account);
    let config_dir = std::path::Path::new(&config_path)
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "/etc/cloudflared".to_string());

    ensure_ct_running(&ctid)?;
    ensure_cloudflared_installed(&ctid)?;
    pct_exec(&ctid, &format!("if [ -f {config_path} ]; then cp {config_path} {config_path}.bak.$(date +%s); fi"))?;
    pct_exec(&ctid, &format!("mkdir -p {config_dir} /root/.cloudflared"))?;

    if cloudflare::has_cloudflare_api_env(cloudflare_account) {
        let remote = cloudflare::ensure_remote_tunnel(&resolved_tunnel_name, cloudflare_account)?;
        let tunnel_id = remote.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
        let tunnel_token = remote.get("tunnelToken").and_then(|t| t.as_str()).unwrap_or("").to_string();
        let remote_name = remote.get("tunnelName").and_then(|n| n.as_str()).unwrap_or(&resolved_tunnel_name).to_string();
        let created = remote.get("created").and_then(|c| c.as_bool()).unwrap_or(false);
        if created {
            util::println_stdout(&format!("[eco proxy] Created remote tunnel {remote_name} ({tunnel_id})"));
        } else {
            util::println_stdout(&format!("[eco proxy] Reusing remote tunnel {remote_name} ({tunnel_id})"));
        }
        cloudflare::overwrite_dns_record_for_tunnel(hostname, &tunnel_id, cloudflare_account)?;
        cloudflare::put_remote_tunnel_config(&tunnel_id, hostname, service_url, cloudflare_account)?;
        write_tunnel_config(&ctid, &tunnel_id, &tunnel_token, &remote_name, hostname, service_url, &config_path)?;
        install_ct_cloudflared_service(&ctid, &tunnel_token, &service_name, &config_path)?;
        pct_exec(&ctid, &format!("systemctl daemon-reload && systemctl enable {service_name} && systemctl restart {service_name}"))?;
        verify_cloudflared_service(&ctid, &service_name)?;
        return Ok(ProxyTunnelResult {
            ctid,
            tunnel_name: remote_name,
            tunnel_id,
            config_path,
            service_name,
        });
    }

    // Non-API fallback: interactive cert.pem flow (rare in up context)
    let tunnel_id = resolve_tunnel_id_by_name(&ctid, &resolved_tunnel_name).unwrap_or_default();
    let tunnel_id = if tunnel_id.is_empty() {
        let tunnel_exists = run_capture(
            "pct",
            &["exec".to_string(), ctid.clone(), "--".to_string(), "bash".to_string(), "-lc".to_string(), format!("cloudflared tunnel list 2>/dev/null | awk 'NR>1 && $2 == \"{resolved_tunnel_name}\" {{ found=1 }} END {{ exit found ? 0 : 1 }}'")],
            &util::current_dir(),
        )?;
        if tunnel_exists.code != 0 {
            pct_exec(&ctid, &format!("cloudflared tunnel create {resolved_tunnel_name}"))?;
        }
        resolve_tunnel_id_by_name(&ctid, &resolved_tunnel_name)?
    } else {
        tunnel_id
    };

    pct_exec(
        &ctid,
        &format!(
            "cat >{config_path} <<'EOF'\ntunnel: {tunnel_id}\ncredentials-file: /root/.cloudflared/{tunnel_id}.json\n\noriginRequest:\n  noTLSVerify: true\n\ningress:\n  - hostname: {hostname}\n    service: {service_url}\n  - service: http_status:404\nEOF"
        ),
    )?;
    install_ct_cloudflared_service(&ctid, "", &service_name, &config_path)?;
    pct_exec(&ctid, &format!("systemctl daemon-reload && systemctl enable {service_name} && systemctl restart {service_name}"))?;
    verify_cloudflared_service(&ctid, &service_name)?;
    Ok(ProxyTunnelResult {
        ctid,
        tunnel_name: resolved_tunnel_name,
        tunnel_id,
        config_path,
        service_name,
    })
}

fn run_tunnel_replicas(positionals: &[String], options: &std::collections::HashMap<String, String>) -> Result<(), String> {
    let account = positionals.first().cloned().ok_or("Missing <account> argument.\n\nUsage: eco proxy tunnel-replicas <account> [count] [--target <ctid>] [--dry-run]")?;
    let target = options.get("target").cloned().unwrap_or_else(|| "proxy".to_string());
    let ctid = resolve_ct_input(&target)?;
    ensure_ct_running(&ctid)?;

    let account_slug = cloudflare::slugify_tunnel_name(&account);
    let template_name = format!("cloudflared-{account_slug}@.service");
    let config_path = cloudflare::cloudflared_config_path_for_account(&account);
    let service_name = cloudflare::cloudflared_service_name_for_account(&account);

    if positionals.len() == 1 {
        let current_raw = pct_exec_capture(
            &ctid,
            &format!("systemctl list-units 'cloudflared-{account_slug}@*' --no-legend --state=active 2>/dev/null | wc -l"),
        )?;
        let current: i64 = current_raw.trim().parse().unwrap_or(0);
        util::println_stdout(&format!("{account}: {current} active replica(s) on CT {ctid}"));
        return Ok(());
    }

    let count_str = positionals.get(1).cloned().ok_or("Missing replica count.")?;
    let desired: i64 = count_str
        .parse()
        .map_err(|_| format!("Invalid replica count: {count_str}. Must be a non-negative integer."))?;
    if desired < 0 {
        return Err(format!("Invalid replica count: {count_str}. Must be a non-negative integer."));
    }

    if options.get("dry-run").map(|v| v == "true").unwrap_or(false) {
        let current_raw = pct_exec_capture(
            &ctid,
            &format!("systemctl list-units 'cloudflared-{account_slug}@*' --no-legend --state=active 2>/dev/null | wc -l"),
        )
        .unwrap_or_else(|_| "0".to_string());
        let current: i64 = current_raw.trim().parse().unwrap_or(0);
        let mut out = String::new();
        out.push_str("eco proxy tunnel-replicas plan\n\n");
        out.push_str(&format!("Account: {account} (service: {template_name})\n"));
        out.push_str(&format!("Target CT: {ctid}\n"));
        out.push_str(&format!("Desired: {desired} replica(s)\n"));
        out.push_str(&format!("Current: {current} active replica(s)\n\n"));
        if desired > current {
            out.push_str("Enable new replicas:\n");
            for i in current + 1..=desired {
                out.push_str(&format!("  pct exec {ctid} -- systemctl enable --now cloudflared-{account_slug}@{i}\n"));
            }
        } else if desired < current {
            out.push_str("Disable removed replicas:\n");
            for i in (desired + 1..=current).rev() {
                out.push_str(&format!("  pct exec {ctid} -- systemctl disable --now cloudflared-{account_slug}@{i}\n"));
            }
        } else {
            out.push_str("Replica count unchanged. Nothing to do.\n");
        }
        print!("{out}");
        return Ok(());
    }

    let config_content = pct_exec_capture(&ctid, &format!("cat {config_path}"))?;
    let token = config_content
        .lines()
        .find_map(|l| l.strip_prefix("tunnel:"))
        .map(|t| t.trim().to_string())
        .unwrap_or_default();
    if token.is_empty() {
        return Err(format!("Cannot read tunnel token from {config_path} in CT {ctid}."));
    }

    let unit_content = format!(
        "[Unit]\nDescription=cloudflared {account} replica %i\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nTimeoutStartSec=15\nType=notify\nExecStart=/usr/bin/cloudflared --no-autoupdate --config {config_path} tunnel run --token {token}\nRestart=on-failure\nRestartSec=5s\n\n[Install]\nWantedBy=multi-user.target\n"
    );
    let temp_dir = std::env::temp_dir().join(format!("eco-cloudflared-replica-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&temp_dir);
    let file_path = temp_dir.join(format!("cloudflared-{account_slug}@.service"));
    std::fs::write(&file_path, unit_content).map_err(|e| e.to_string())?;
    let push = push_file_to_ct(&ctid, &file_path.display().to_string(), &format!("/etc/systemd/system/{service_name}@.service"));
    let _ = std::fs::remove_dir_all(&temp_dir);
    push?;
    pct_exec(&ctid, "systemctl daemon-reload")?;

    let current_raw = pct_exec_capture(
        &ctid,
        &format!("systemctl list-units 'cloudflared-{account_slug}@*' --no-legend --state=active 2>/dev/null | wc -l"),
    )?;
    let current: i64 = current_raw.trim().parse().unwrap_or(0);

    if desired > current {
        util::println_stdout(&format!("[eco proxy] Scaling {account} from {current} to {desired} replica(s)"));
        for i in current + 1..=desired {
            pct_exec(&ctid, &format!("systemctl enable --now cloudflared-{account_slug}@{i}"))?;
        }
    } else if desired < current {
        util::println_stdout(&format!("[eco proxy] Scaling {account} from {current} to {desired} replica(s)"));
        for i in (desired + 1..=current).rev() {
            pct_exec(&ctid, &format!("systemctl disable --now cloudflared-{account_slug}@{i}"))?;
        }
    } else {
        util::println_stdout(&format!("[eco proxy] {account} already at {desired} replica(s)"));
    }
    Ok(())
}

fn run_init_tunnel(positionals: &[String], options: &std::collections::HashMap<String, String>) -> Result<(), String> {
    let target = positionals.first().cloned().unwrap_or_else(|| "proxy".to_string());
    let hostname = positionals.get(1).cloned();
    if hostname.is_none() {
        return Err("Missing hostname.\n\nRun \"eco proxy help\" for usage.".to_string());
    }
    let hostname = hostname.unwrap();
    let dry_run = options.get("dry-run").map(|v| v == "true").unwrap_or(false);
    let service_url = options.get("service").cloned().unwrap_or_else(|| "http://127.0.0.1:80".to_string());

    let ctid = resolve_ct_input(&target)?;
    let tunnel_name = options.get("name").cloned().unwrap_or_else(|| cloudflare::slugify_tunnel_name(&hostname));
    let config_path = cloudflare::cloudflared_config_path_for_account(options.get("account").map(|s| s.as_str()).unwrap_or(""));
    let service_name = cloudflare::cloudflared_service_name_for_account(options.get("account").map(|s| s.as_str()).unwrap_or(""));
    let account = options.get("account").cloned().unwrap_or_default();

    let plan = vec![
        format!("Ensure CT {ctid} is running"),
        format!("Ensure cloudflared is installed inside CT {ctid}"),
        format!("Backup existing {config_path} inside CT {ctid} if present"),
        format!("Ensure {} and /root/.cloudflared exist in CT {ctid}", std::path::Path::new(&config_path).parent().map(|p| p.display().to_string()).unwrap_or_default()),
        if cloudflare::has_cloudflare_api_env(&account) {
            format!("Create or reuse remote tunnel \"{tunnel_name}\" through Cloudflare API{}", if account.is_empty() { String::new() } else { format!(" (account \"{account}\")") })
        } else {
            format!("Run interactive cloudflared tunnel login inside CT {ctid} if cert.pem is missing")
        },
        if cloudflare::has_cloudflare_api_env(&account) {
            format!("Create or update DNS record {hostname} -> <tunnel-id>.cfargotunnel.com through Cloudflare API")
        } else {
            format!("Create DNS route {hostname} -> tunnel {tunnel_name}")
        },
        format!("Write {config_path} in CT {ctid} with ingress {service_url}"),
        format!("Install {service_name}.service in CT {ctid}"),
        format!("Enable and restart {service_name} in CT {ctid}"),
        format!("Verify {service_name} status in CT {ctid}"),
    ];

    if dry_run {
        util::println_stdout("eco proxy init-tunnel plan\n");
        for (index, step) in plan.iter().enumerate() {
            util::println_stdout(&format!("{}. {step}", index + 1));
        }
        return Ok(());
    }

    util::println_stdout(&format!(
        "[eco proxy] Initializing dedicated tunnel in CT {ctid} for {hostname}{}",
        if account.is_empty() { String::new() } else { format!(" (account \"{account}\")") }
    ));
    ensure_ct_running(&ctid)?;
    ensure_cloudflared_installed(&ctid)?;
    pct_exec(&ctid, &format!("if [ -f {config_path} ]; then cp {config_path} {config_path}.bak.$(date +%s); fi"))?;
    let config_dir = std::path::Path::new(&config_path).parent().map(|p| p.display().to_string()).unwrap_or_default();
    pct_exec(&ctid, &format!("mkdir -p {config_dir} /root/.cloudflared"))?;

    if cloudflare::has_cloudflare_api_env(&account) {
        let remote = cloudflare::ensure_remote_tunnel(&tunnel_name, &account)?;
        let tunnel_id = remote.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
        let tunnel_token = remote.get("tunnelToken").and_then(|t| t.as_str()).unwrap_or("").to_string();
        let created = remote.get("created").and_then(|c| c.as_bool()).unwrap_or(false);
        let remote_name = remote.get("tunnelName").and_then(|n| n.as_str()).unwrap_or(&tunnel_name);
        if created {
            util::println_stdout(&format!("[eco proxy] Created remote tunnel {remote_name} ({tunnel_id})"));
        } else {
            util::println_stdout(&format!("[eco proxy] Reusing remote tunnel {remote_name} ({tunnel_id})"));
        }
        cloudflare::overwrite_dns_record_for_tunnel(&hostname, &tunnel_id, &account)?;
        cloudflare::put_remote_tunnel_config(&tunnel_id, &hostname, &service_url, &account)?;
        write_tunnel_config(&ctid, &tunnel_id, &tunnel_token, remote_name, &hostname, &service_url, &config_path)?;
        install_ct_cloudflared_service(&ctid, &tunnel_token, &service_name, &config_path)?;
        pct_exec(&ctid, &format!("systemctl daemon-reload && systemctl enable {service_name} && systemctl restart {service_name}"))?;
        verify_cloudflared_service(&ctid, &service_name)?;
        return Ok(());
    }

    // Interactive (cert.pem) path
    let has_cert = run_capture(
        "pct",
        &["exec".to_string(), ctid.clone(), "--".to_string(), "bash".to_string(), "-lc".to_string(), "test -f /root/.cloudflared/cert.pem".to_string()],
        &util::current_dir(),
    )?;
    if has_cert.code != 0 {
        util::println_stdout("[eco proxy] Browser login required for Cloudflare tunnel authorization");
        pct_exec(&ctid, "cloudflared tunnel login")?;
    }

    let tunnel_exists = run_capture(
        "pct",
        &["exec".to_string(), ctid.clone(), "--".to_string(), "bash".to_string(), "-lc".to_string(), format!("cloudflared tunnel list 2>/dev/null | awk 'NR>1 && $2 == \"{tunnel_name}\" {{ found=1 }} END {{ exit found ? 0 : 1 }}'")],
        &util::current_dir(),
    )?;
    if tunnel_exists.code != 0 {
        pct_exec(&ctid, &format!("cloudflared tunnel create {tunnel_name}"))?;
    }
    let tunnel_id = resolve_tunnel_id_by_name(&ctid, &tunnel_name)?;

    let route = pct_exec(&ctid, &format!("cloudflared tunnel route dns {tunnel_name} {hostname}"));
    if let Err(e) = route {
        if e.to_lowercase().contains("record with that host already exists") {
            util::println_stdout(&format!("[eco proxy] Existing DNS record detected for {hostname}, attempting Cloudflare API overwrite"));
            let result = cloudflare::overwrite_dns_record_for_tunnel(&hostname, &tunnel_id, &account)?;
            util::println_stdout(&format!("[eco proxy] Cloudflare DNS {result} for {hostname}"));
        } else {
            return Err(e);
        }
    }

    pct_exec(
        &ctid,
        &format!(
            "cat >{config_path} <<'EOF'\ntunnel: {tunnel_id}\ncredentials-file: /root/.cloudflared/{tunnel_id}.json\n\noriginRequest:\n  noTLSVerify: true\n\ningress:\n  - hostname: {hostname}\n    service: http://127.0.0.1:80\n  - service: http_status:404\nEOF"
        ),
    )?;
    install_ct_cloudflared_service(&ctid, "", &service_name, &config_path)?;
    pct_exec(&ctid, &format!("systemctl daemon-reload && systemctl enable {service_name} && systemctl restart {service_name}"))?;
    verify_cloudflared_service(&ctid, &service_name)?;
    Ok(())
}

fn run_migrate_cloudflared(positionals: &[String], options: &std::collections::HashMap<String, String>) -> Result<(), String> {
    let target = positionals.first().cloned().unwrap_or_else(|| "proxy".to_string());
    let ctid = resolve_ct_input(&target)?;
    let (config_path, credentials_mode, credential_files) = ensure_host_cloudflared_files()?;

    // resolve host service file (write captured systemctl output to temp)
    let direct = "/etc/systemd/system/cloudflared.service";
    let (host_service_file, cleanup_dir) = if std::path::Path::new(direct).exists() {
        (direct.to_string(), None)
    } else {
        let result = run_capture("systemctl", &["cat".to_string(), "cloudflared".to_string()], &util::current_dir())?;
        if result.code != 0 || result.stdout.trim().is_empty() {
            return Err("Cannot resolve host cloudflared.service definition.".to_string());
        }
        let temp_dir = std::env::temp_dir().join(format!("eco-cloudflared-host-unit-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let file_path = temp_dir.join("cloudflared.service");
        std::fs::write(&file_path, &result.stdout).map_err(|e| e.to_string())?;
        (file_path.display().to_string(), Some(temp_dir))
    };

    let mut plan = vec![
        format!("Validate host cloudflared config at {config_path}"),
        if credentials_mode == "file" {
            format!("Validate host cloudflared credentials file {}", credential_files[0])
        } else {
            "Use token-based cloudflared config (no separate credentials file)".to_string()
        },
        format!("Reuse host cloudflared systemd unit from {host_service_file}"),
        format!("Ensure CT {ctid} is running"),
        format!("Prepare /etc/cloudflared, /root/.cloudflared, /etc/systemd/system in CT {ctid}"),
        format!("Copy host config into CT {ctid}: /etc/cloudflared/config.yml"),
    ];
    for file in &credential_files {
        let basename = std::path::Path::new(file).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        plan.push(format!("Copy credential into CT {ctid}: /root/.cloudflared/{basename}"));
    }
    plan.push(format!("Install cloudflared.service in CT {ctid}"));
    plan.push(format!("Enable and restart cloudflared in CT {ctid}"));
    plan.push(format!("Verify cloudflared status in CT {ctid}"));

    if options.get("stop-host").map(|v| v == "true").unwrap_or(false) {
        plan.push(format!("Stop and disable host-level cloudflared after CT {ctid} starts cleanly"));
    }

    if options.get("dry-run").map(|v| v == "true").unwrap_or(false) {
        util::println_stdout("eco proxy migrate-cloudflared plan\n");
        for (index, step) in plan.iter().enumerate() {
            util::println_stdout(&format!("{}. {step}", index + 1));
        }
        if let Some(dir) = cleanup_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
        return Ok(());
    }

    util::println_stdout(&format!("[eco proxy] Migrating host cloudflared into CT {ctid}"));
    ensure_ct_running(&ctid)?;
    pct_exec(&ctid, "mkdir -p /etc/cloudflared /root/.cloudflared /etc/systemd/system")?;
    push_file_to_ct(&ctid, &config_path, "/etc/cloudflared/config.yml")?;
    for file in &credential_files {
        let basename = std::path::Path::new(file).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        push_file_to_ct(&ctid, file, &format!("/root/.cloudflared/{basename}"))?;
    }
    let push_result = push_file_to_ct(&ctid, &host_service_file, "/etc/systemd/system/cloudflared.service");
    if let Some(dir) = cleanup_dir {
        let _ = std::fs::remove_dir_all(dir);
    }
    push_result?;
    pct_exec(&ctid, "systemctl daemon-reload && systemctl enable cloudflared && systemctl restart cloudflared")?;
    verify_cloudflared_service(&ctid, "cloudflared")?;

    if options.get("stop-host").map(|v| v == "true").unwrap_or(false) {
        util::println_stdout("[eco proxy] Stopping host-level cloudflared");
        run_command("systemctl", &["stop".to_string(), "cloudflared".to_string()], &util::current_dir())?;
        run_command("systemctl", &["disable".to_string(), "cloudflared".to_string()], &util::current_dir())?;
    }
    Ok(())
}

fn parse_proxy_options(args: &[String]) -> Result<(std::collections::HashMap<String, String>, Vec<String>), String> {
    let mut options = std::collections::HashMap::new();
    let mut positionals = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if !arg.starts_with("--") {
            positionals.push(arg.clone());
            i += 1;
            continue;
        }
        let key = arg[2..].to_string();
        if key == "dry-run" || key == "stop-host" {
            options.insert(key, "true".to_string());
            i += 1;
            continue;
        }
        if key == "name" || key == "account" || key == "target" || key == "service" {
            let value = args.get(i + 1).cloned().ok_or_else(|| format!("Missing value for option --{key}"))?;
            if value.starts_with("--") {
                return Err(format!("Missing value for option --{key}"));
            }
            options.insert(key, value);
            i += 2;
            continue;
        }
        return Err(format!("Unknown option --{key}"));
    }
    Ok((options, positionals))
}

pub fn run_proxy(args: &[String]) -> Result<(), String> {
    let (subcommand, rest) = match args.first() {
        Some(s) => (s.as_str(), &args[1..]),
        None => ("", &args[0..0]),
    };
    match subcommand {
        "" | "help" | "--help" | "-h" => {
            proxy_help();
            Ok(())
        }
        "init-tunnel" => {
            let (options, positionals) = parse_proxy_options(rest)?;
            run_init_tunnel(&positionals, &options)
        }
        "tunnel-replicas" => {
            let (options, positionals) = parse_proxy_options(rest)?;
            run_tunnel_replicas(&positionals, &options)
        }
        "migrate-cloudflared" => {
            let (options, positionals) = parse_proxy_options(rest)?;
            run_migrate_cloudflared(&positionals, &options)
        }
        other => Err(format!("Unknown proxy subcommand: {other}\n\nRun \"eco proxy help\" for usage.")),
    }
}
