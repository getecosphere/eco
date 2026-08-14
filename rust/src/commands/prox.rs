use crate::cloudflare;
use crate::ecompose;
use crate::util;
use std::path::Path;

fn help_text() {
    let text = r#"eco prox

Usage:
  eco prox prepare rust-builder <ctid-or-hostname> [--dry-run]
  eco prox createct rust-builder <name> [options]
  eco prox clear-rust <builder-ctid-or-name> [--yes] [--dry-run]
  eco prox remove-tunnel [domain|*.domain] [--target <ctid-or-hostname>] [--account <name>] [--dry-run]
  eco prox clearenv [--dry-run]
  eco prox showports
  eco prox rename-pct <ctid> <new-hostname>
  eco prox shrink-pct <ctid> <target-gb> [temp-ctid]
  eco prox size-pct
  eco prox set-ct <ctid> --cores <n> --memory <mb> [--swap <mb>] [--dry-run]
  eco prox archive <vm-or-ct> --output <external-directory> [--format qcow2]
  eco prox unarchive <archive-directory-or-vzdump-archive> --id <new-id> [--storage <storage>]

Rust builder preparation installs Eco's shared Rust toolchain and sccache in
an existing CT. That CT may also run applications; use its name or ID in
ECO_RUST_DEDICATED_BUILDER when running production eco up.

VM archive default: compressed QCOW2 images written directly to external storage.
Eco stops a running VM temporarily for a consistent archive, then starts it again.
CT archives remain native vzdump archives. Use --format vzdump when a native VM
snapshot/suspend backup is explicitly required.

Examples:
  eco prox prepare rust-builder deveko
  eco prox createct rust-builder rust-builder --id 1000
  eco prox clear-rust deveko
  eco prox remove-tunnel
  eco prox remove-tunnel app.example.com --target proxy
  eco prox remove-tunnel '*.example.com' --target proxy
  eco prox remove-tunnel app.example.com --account customer-a
  eco prox clearenv
  eco prox clearenv --dry-run
  eco prox showports
  eco prox rename-pct 100 proxy-edge
  eco prox shrink-pct 101 8
  eco prox shrink-pct 101 8 900
  eco prox size-pct
  eco prox set-ct 101 --cores 10 --memory 6144 --swap 2048
  eco prox set-ct 101 --memory 6144 --dry-run
  eco prox archive Win11 --output /mnt/usb/VM
  eco prox unarchive /mnt/usb/VM/eco-qemu-999-... --id 220 --storage local-lvm

After QCOW2 restore, inspect before starting:
  qm config 220
  qm start 220
"#;
    print!("{text}");
}

fn parse_args(args: &[String]) -> (std::collections::HashMap<String, String>, Vec<String>) {
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
        if key == "dry-run" || key == "help" || key == "yes-reinstall" || key == "keep-on-failure" || key == "yes" || key == "allow-local" {
            options.insert(key, "true".to_string());
            i += 1;
            continue;
        }
        if let Some(value) = args.get(i + 1) {
            if !value.starts_with("--") {
                options.insert(key.clone(), value.clone());
                i += 2;
                continue;
            }
        }
        // Missing value: record flag as true
        options.insert(key, "true".to_string());
        i += 1;
    }
    (options, positionals)
}

fn run(command: &str, args: &[String], capture: bool) -> Result<util::Captured, String> {
    let cwd = util::current_dir();
    if capture {
        util::run_capture(command, args, &cwd)
    } else {
        util::run_command(command, args, &cwd)?;
        Ok(util::Captured { code: 0, stdout: String::new(), stderr: String::new() })
    }
}

fn next_id() -> Result<String, String> {
    let r = run("pvesh", &["get".to_string(), "/cluster/nextid".to_string()], true)?;
    Ok(r.stdout.trim().to_string())
}

fn resolve_installed_template(requested: Option<&str>) -> Result<String, String> {
    let listing = run("pveam", &["list".to_string(), "local".to_string()], true)?;
    let installed: Vec<String> = listing
        .stdout
        .lines()
        .filter_map(|l| {
            // match \S+:vztmpl/\S+\.tar\.(zst|gz|xz)
            let idx = l.find(":vztmpl/")?;
            let start = l[..idx].rfind(char::is_whitespace).map(|p| p + 1).unwrap_or(0);
            let token = l[start..].trim().to_string();
            if token.ends_with(".tar.zst") || token.ends_with(".tar.gz") || token.ends_with(".tar.xz") {
                Some(token)
            } else {
                None
            }
        })
        .collect();
    if let Some(requested) = requested {
        if !installed.contains(&requested.to_string()) {
            return Err(format!(
                "Requested template \"{requested}\" is not installed. Available templates:\n{}",
                installed.join("\n")
            ));
        }
        return Ok(requested.to_string());
    }
    let mut candidates: Vec<String> = installed
        .iter()
        .filter(|t| {
            let name = t.rsplit('/').next().unwrap_or("");
            name.starts_with("debian-12-standard_") || name.starts_with("debian-13-standard_")
        })
        .cloned()
        .collect();
    candidates.sort();
    candidates.reverse();
    candidates.pop().ok_or_else(|| {
        "No Debian 12/13 LXC template is installed on local storage. Run `pveam update`, then download one with `pveam download local <template-name>`, and retry.".to_string()
    })
}

fn pct_create_args(id: &str, options: &std::collections::HashMap<String, String>, template: &str, hostname: &str) -> Vec<String> {
    let mut network = vec![
        "name=eth0".to_string(),
        format!("bridge={}", options.get("bridge").cloned().unwrap_or_else(|| "vmbr0".to_string())),
        format!("ip={}", options.get("ip").cloned().unwrap_or_else(|| "dhcp".to_string())),
    ];
    if let Some(gateway) = options.get("gateway") {
        network.push(format!("gw={gateway}"));
    }
    vec![
        "create".to_string(),
        id.to_string(),
        template.to_string(),
        "--hostname".to_string(),
        hostname.to_string(),
        "--cores".to_string(),
        options.get("cores").cloned().unwrap_or_else(|| "2".to_string()),
        "--memory".to_string(),
        options.get("memory").cloned().unwrap_or_else(|| "1024".to_string()),
        "--swap".to_string(),
        options.get("swap").cloned().unwrap_or_else(|| "512".to_string()),
        "--rootfs".to_string(),
        format!("{}:{}", options.get("storage").cloned().unwrap_or_else(|| "local-lvm".to_string()), options.get("disk").cloned().unwrap_or_else(|| "30".to_string())),
        "--net0".to_string(),
        network.join(","),
        "--features".to_string(),
        "nesting=1".to_string(),
        "--unprivileged".to_string(),
        "1".to_string(),
    ]
}

fn existing_ct_hostname(ctid: &str) -> Option<String> {
    let r = run("pct", &["config".to_string(), ctid.to_string()], true).ok()?;
    r.stdout
        .lines()
        .find_map(|l| l.strip_prefix("hostname:"))
        .map(|h| h.trim().to_string())
}

fn find_ct_by_hostname(hostname: &str) -> Option<String> {
    let listing = run("pct", &["list".to_string()], true).ok()?;
    for line in listing.stdout.split('\n') {
        let id = line.trim().split_whitespace().next().unwrap_or("");
        if !id.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if existing_ct_hostname(id).as_deref() == Some(hostname) {
            return Some(id.to_string());
        }
    }
    None
}

fn ensure_ct_running(ctid: &str) -> Result<(), String> {
    let status = run("pct", &["status".to_string(), ctid.to_string()], true)?;
    if status.stdout.contains("status: running") {
        return Ok(());
    }
    run("pct", &["start".to_string(), ctid.to_string()], false)?;
    Ok(())
}

fn wait_for_ct_exec(ctid: &str, attempts: u32, delay_ms: u64) -> Result<(), String> {
    for attempt in 1..=attempts {
        let r = run("pct", &["exec".to_string(), ctid.to_string(), "--".to_string(), "true".to_string()], true);
        if r.is_ok() {
            return Ok(());
        }
        if attempt < attempts {
            util::sleep_ms(delay_ms);
        }
    }
    Err(format!("CT {ctid} did not become exec-ready within {attempts} seconds."))
}

fn resolve_ct_by_reference(reference: &str) -> Result<(String, String), String> {
    if reference.chars().all(|c| c.is_ascii_digit()) {
        let hostname = existing_ct_hostname(reference)
            .ok_or_else(|| format!("CT {reference} does not exist."))?;
        return Ok((reference.to_string(), hostname));
    }
    let ctid = find_ct_by_hostname(reference)
        .ok_or_else(|| format!("No CT with hostname \"{reference}\" exists."))?;
    Ok((ctid, reference.to_string()))
}

fn install_rust_builder(ctid: &str) -> Result<(), String> {
    util::println_stdout(&format!("[CT {ctid}] Installing Rust build toolchain and shared compiler cache..."));
    let command = [
        "export DEBIAN_FRONTEND=noninteractive",
        "apt-get update",
        "apt-get install -y curl ca-certificates build-essential pkg-config libssl-dev",
        "mkdir -p /usr/local/rustup /usr/local/cargo /opt/eco-rust-builds",
        "RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo bash -c 'curl --proto =https --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --no-modify-path'",
        "RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo /usr/local/cargo/bin/rustup toolchain install stable --profile minimal",
        "RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo /usr/local/cargo/bin/rustup default stable",
        "if [ ! -x /usr/local/bin/sccache ]; then arch=$(dpkg --print-architecture); [ \"$arch\" = amd64 ] || { echo \"Eco rust-builder requires an amd64 sccache binary (found: $arch)\" >&2; exit 1; }; version=v0.16.0; tmpdir=$(mktemp -d); trap 'rm -rf \"$tmpdir\"' EXIT; curl --proto '=https' --tlsv1.2 -sSfL \"https://github.com/mozilla/sccache/releases/download/$version/sccache-$version-x86_64-unknown-linux-musl.tar.gz\" -o \"$tmpdir/sccache.tar.gz\"; tar xzf \"$tmpdir/sccache.tar.gz\" -C \"$tmpdir\"; install -m 0755 \"$tmpdir/sccache-$version-x86_64-unknown-linux-musl/sccache\" /usr/local/bin/sccache; rm -rf \"$tmpdir\"; trap - EXIT; fi",
        "install -d -m 0777 /usr/local/sccache-cache",
        "rm -f /usr/local/bin/cargo /usr/local/bin/rustc /usr/local/bin/rustup",
        "printf '%s\\n' '#!/bin/sh' 'export RUSTUP_HOME=/usr/local/rustup' 'export CARGO_HOME=/usr/local/cargo' 'exec /usr/local/cargo/bin/cargo \"$@\"' > /usr/local/bin/cargo",
        "printf '%s\\n' '#!/bin/sh' 'export RUSTUP_HOME=/usr/local/rustup' 'export CARGO_HOME=/usr/local/cargo' 'exec /usr/local/cargo/bin/rustc \"$@\"' > /usr/local/bin/rustc",
        "printf '%s\\n' '#!/bin/sh' 'export RUSTUP_HOME=/usr/local/rustup' 'export CARGO_HOME=/usr/local/cargo' 'exec /usr/local/cargo/bin/rustup \"$@\"' > /usr/local/bin/rustup",
        "chmod 755 /usr/local/bin/cargo /usr/local/bin/rustc /usr/local/bin/rustup",
        "printf '%s\\n' 'export RUSTUP_HOME=/usr/local/rustup' 'export CARGO_HOME=/usr/local/cargo' 'export PATH=/usr/local/cargo/bin:$PATH' 'export RUSTC_WRAPPER=/usr/local/bin/sccache' 'export SCCACHE_DIR=/usr/local/sccache-cache' > /etc/profile.d/eco-rust.sh",
        "chmod 644 /etc/profile.d/eco-rust.sh",
        "install -d -m 755 /etc/eco",
        "printf '%s\\n' 'role=rust-builder' 'rustup_home=/usr/local/rustup' 'cargo_home=/usr/local/cargo' 'build_root=/opt/eco-rust-builds' > /etc/eco/rust-builder.env",
        "/usr/local/bin/cargo --version",
        "/usr/local/bin/sccache --version",
    ]
    .join(" && ");
    run("pct", &["exec".to_string(), ctid.to_string(), "--".to_string(), "bash".to_string(), "-lc".to_string(), command], false)?;
    Ok(())
}

fn prepare_rust_builder(positionals: &[String], options: &std::collections::HashMap<String, String>) -> Result<(), String> {
    let reference = positionals.get(2).cloned().ok_or("Usage: eco prox prepare rust-builder <ctid-or-hostname> [--dry-run]")?;
    let (ctid, hostname) = resolve_ct_by_reference(&reference)?;

    if options.get("dry-run").map(|v| v == "true").unwrap_or(false) {
        let out = format!(
            "eco prox prepare rust-builder plan\n  CT: {ctid} ({hostname})\n  Ensure running\n  Install/refresh: curl, CA certificates, build-essential, pkg-config, libssl-dev\n  Install/refresh: Rust stable minimal toolchain in /usr/local/rustup and /usr/local/cargo\n  Install/refresh: sccache in /usr/local/bin and its shared cache directory\n  Mark role: /etc/eco/rust-builder.env\n\nUse after success:\n  export ECO_RUST_DEDICATED_BUILDER={hostname}\n"
        );
        print!("{out}");
        return Ok(());
    }
    ensure_ct_running(&ctid)?;
    wait_for_ct_exec(&ctid, 30, 1000)?;
    install_rust_builder(&ctid)?;
    util::println_stdout(&format!(
        "Rust builder CT {ctid} ({hostname}) is ready.\n\nFor this shell:\n  export ECO_RUST_DEDICATED_BUILDER={hostname}\n\nPersist it in the Proxmox host environment before running eco up. The builder may also be an application CT; Eco builds in place when it is the destination CT.\n"
    ));
    Ok(())
}

const RUST_CLEANUP_REPORT: &str = "set -euo pipefail\npaths=(/usr/local/rustup /usr/local/cargo /usr/local/sccache-cache)\nbefore_managed=0\nfor path in \"${paths[@]}\"; do [ -e \"$path\" ] || continue; size=$(du -sk \"$path\" 2>/dev/null | awk '{print $1}'); before_managed=$((before_managed + ${size:-0})); done\nbefore_root=$(df -Pk / | awk 'NR == 2 {print $3}')\nrm -rf /usr/local/rustup /usr/local/cargo /usr/local/sccache-cache\nrm -f /usr/local/bin/cargo /usr/local/bin/rustc /usr/local/bin/rustup /usr/local/bin/sccache /etc/profile.d/cargo.sh /etc/profile.d/eco-rust.sh /etc/eco/rust-builder.env\nafter_managed=0\nfor path in \"${paths[@]}\"; do [ -e \"$path\" ] || continue; size=$(du -sk \"$path\" 2>/dev/null | awk '{print $1}'); after_managed=$((after_managed + ${size:-0})); done\nafter_root=$(df -Pk / | awk 'NR == 2 {print $3}')\nprintf 'ECO_RUST_CLEANUP before_managed_kb=%s after_managed_kb=%s before_root_kb=%s after_root_kb=%s\\n' \"$before_managed\" \"$after_managed\" \"$before_root\" \"$after_root\"";

fn human_kib(kb: i64) -> String {
    if kb >= 1024 * 1024 {
        format!("{:.2} GiB", kb as f64 / (1024.0 * 1024.0))
    } else if kb >= 1024 {
        format!("{:.1} MiB", kb as f64 / 1024.0)
    } else {
        format!("{kb} KiB")
    }
}

fn cleanup_metrics(text: &str) -> Result<(i64, i64, i64, i64), String> {
    let marker = "ECO_RUST_CLEANUP";
    let Some(idx) = text.find(marker) else {
        return Err(format!("Rust cleanup completed but did not return a size report.\n{text}"));
    };
    let rest = &text[idx + marker.len()..];
    let mut vals = [0i64; 4];
    let mut found = 0;
    for key in ["before_managed_kb", "after_managed_kb", "before_root_kb", "after_root_kb"] {
        let Some(k) = rest.find(key) else { break };
        let after = &rest[k + key.len()..].trim_start();
        let num = after
            .split(|c: char| !c.is_ascii_digit() && c != '-' && c != '+')
            .next()
            .unwrap_or("0");
        if let Ok(n) = num.parse::<i64>() {
            vals[found] = n;
        }
        found += 1;
    }
    if found != 4 {
        return Err(format!("Rust cleanup completed but did not return a size report.\n{text}"));
    }
    Ok((vals[0], vals[1], vals[2], vals[3]))
}

fn parse_pct_list(output: &str) -> Vec<(String, String, String)> {
    output
        .split('\n')
        .filter_map(|line| {
            let line = line.trim();
            let mut parts = line.split_whitespace();
            let id = parts.next()?.to_string();
            if !id.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let status = parts.next().unwrap_or("").to_string();
            let hostname = parts.collect::<Vec<_>>().join(" ");
            Some((id, status, hostname))
        })
        .collect()
}

fn clear_rust(args: &[String], options: &std::collections::HashMap<String, String>) -> Result<(), String> {
    let builder_reference = args
        .get(1)
        .cloned()
        .or_else(|| std::env::var("ECO_RUST_DEDICATED_BUILDER").ok())
        .ok_or("Usage: eco prox clear-rust <builder-ctid-or-name> [--yes] [--dry-run]")?;
    let (builder_ctid, builder_hostname) = resolve_ct_by_reference(&builder_reference)?;
    let listed = parse_pct_list(&run("pct", &["list".to_string()], true)?.stdout);
    let targets: Vec<(String, String)> = listed
        .iter()
        .filter(|(id, status, _)| id != &builder_ctid && status == "running")
        .map(|(id, _, name)| (id.clone(), name.clone()))
        .collect();
    let skipped: Vec<String> = listed
        .iter()
        .filter(|(id, status, _)| id != &builder_ctid && status != "running")
        .map(|(id, _, _)| id.clone())
        .collect();

    if targets.is_empty() {
        util::println_stdout(&format!(
            "No running CT needs cleanup; builder CT {builder_ctid} was preserved.{}",
            if skipped.is_empty() {
                String::new()
            } else {
                format!(" Stopped CTs were not started: {}.", skipped.join(", "))
            }
        ));
        return Ok(());
    }

    if options.get("dry-run").map(|v| v == "true").unwrap_or(false) {
        let out = format!(
            "eco prox clear-rust plan\n  Preserve builder: CT {builder_ctid} ({builder_hostname})\n  Clean managed Rust toolchains/caches: {}\n  Preserve application binaries and target/ directories.{}",
            targets.iter().map(|(id, name)| format!("CT {id} ({name})")).collect::<Vec<_>>().join(", "),
            if skipped.is_empty() {
                String::new()
            } else {
                format!("\n  Skip stopped CTs (never start them implicitly): {}", skipped.iter().map(|id| format!("CT {id}")).collect::<Vec<_>>().join(", "))
            }
        );
        util::println_stdout(&out);
        return Ok(());
    }

    if options.get("yes").map(|v| v == "true").unwrap_or(false) {
        // proceed
    } else {
        let answer = crate::checklist::prompt_line(&format!(
            "Remove Eco-managed Rust toolchains and caches from CT {}? Type CLEAR-RUST to continue: ",
            targets.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>().join(", ")
        ))?;
        if answer.trim() != "CLEAR-RUST" {
            util::println_stdout("Rust cleanup cancelled.");
            return Ok(());
        }
    }

    let mut totals = (0i64, 0i64, 0i64, 0i64);
    for (id, _) in &targets {
        util::println_stdout(&format!("[CT {id}] Removing Eco-managed Rust toolchain and cache..."));
        let result = run("pct", &["exec".to_string(), id.clone(), "--".to_string(), "bash".to_string(), "-lc".to_string(), RUST_CLEANUP_REPORT.to_string()], true)?;
        let metrics = cleanup_metrics(&format!("{}\n{}", result.stdout, result.stderr))?;
        totals.0 += metrics.0;
        totals.1 += metrics.1;
        totals.2 += metrics.2;
        totals.3 += metrics.3;
        let reclaimed = metrics.0 - metrics.1;
        let managed_pct = if metrics.0 > 0 { reclaimed as f64 / metrics.0 as f64 * 100.0 } else { 0.0 };
        let root_saved = (metrics.2 - metrics.3).max(0);
        let root_pct = if metrics.2 > 0 { root_saved as f64 / metrics.2 as f64 * 100.0 } else { 0.0 };
        util::println_stdout(&format!(
            "  managed Rust: {} → {}; reclaimed {} ({:.1}%)\n  root filesystem: {} used → {} used; reduced {} ({:.1}%)",
            human_kib(metrics.0),
            human_kib(metrics.1),
            human_kib(reclaimed),
            managed_pct,
            human_kib(metrics.2),
            human_kib(metrics.3),
            human_kib(root_saved),
            root_pct
        ));
    }
    let reclaimed = totals.0 - totals.1;
    let managed_pct = if totals.0 > 0 { reclaimed as f64 / totals.0 as f64 * 100.0 } else { 0.0 };
    let root_saved = (totals.2 - totals.3).max(0);
    let root_pct = if totals.2 > 0 { root_saved as f64 / totals.2 as f64 * 100.0 } else { 0.0 };
    util::println_stdout(&format!(
        "\nRust cleanup total (excluding builder CT {builder_ctid}):\n  managed Rust: {} → {}; reclaimed {} ({:.1}%)\n  root filesystem used: {} → {}; reduced {} ({:.1}%)",
        human_kib(totals.0),
        human_kib(totals.1),
        human_kib(reclaimed),
        managed_pct,
        human_kib(totals.2),
        human_kib(totals.3),
        human_kib(root_saved),
        root_pct
    ));
    Ok(())
}

fn parse_tunnel_configs(text: &str) -> Vec<(String, String, String, Vec<String>)> {
    text.split('\n')
        .filter_map(|line| {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() < 2 {
                return None;
            }
            let config_path = parts[0].to_string();
            let tunnel = parts[1].to_string();
            let tunnel_id = parts.get(2).map(|s| s.to_string()).unwrap_or_default();
            let hostnames: Vec<String> = parts
                .get(3)
                .map(|s| s.split(',').map(|h| h.trim().to_string()).filter(|h| !h.is_empty()).collect())
                .unwrap_or_default();
            Some((config_path, tunnel, tunnel_id, hostnames))
        })
        .collect()
}

fn list_tunnel_configs(ctid: &str) -> Result<Vec<(String, String, String, Vec<String>)>, String> {
    let command = [
        "shopt -s nullglob",
        "for file in /etc/cloudflared/config.yml /etc/cloudflared-*/config.yml /root/.cloudflared/config.yml; do",
        "  [ -f \"$file\" ] || continue",
        "  tunnel=$(sed -n 's/^tunnel:[[:space:]]*//p' \"$file\" | head -n 1)",
        "  [ -n \"$tunnel\" ] || continue",
        "  tunnel_id=$(sed -n 's/^# eco-tunnel-id:[[:space:]]*//p' \"$file\" | head -n 1)",
        "  hostnames=$(sed -n 's/^[[:space:]]*-[[:space:]]*hostname:[[:space:]]*//p' \"$file\" | paste -sd, -)",
        "  printf '%s\\t%s\\t%s\\t%s\\n' \"$file\" \"$tunnel\" \"$tunnel_id\" \"$hostnames\"",
        "done",
    ]
    .join("\n");
    let result = run("pct", &["exec".to_string(), ctid.to_string(), "--".to_string(), "bash".to_string(), "-lc".to_string(), command], true)?;
    Ok(parse_tunnel_configs(&result.stdout))
}

fn tunnel_service_for_config(config_path: &str) -> String {
    // /etc/cloudflared-<name>/config.yml -> cloudflared-<name>
    let rest = config_path.strip_prefix("/etc/cloudflared-").unwrap_or("");
    if let Some(dir) = rest.strip_suffix("/config.yml") {
        format!("cloudflared-{dir}")
    } else {
        "cloudflared".to_string()
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn hostname_matches_removal(hostname: &str, requested_domain: &str) -> bool {
    if !requested_domain.starts_with("*.") {
        return hostname == requested_domain;
    }
    let suffix = &requested_domain[1..];
    hostname.ends_with(suffix) && hostname.len() > suffix.len()
}

fn resolve_tunnel_target(reference: &str) -> Result<(String, String), String> {
    if reference.chars().all(|c| c.is_ascii_digit()) {
        let hostname = existing_ct_hostname(reference)
            .ok_or_else(|| format!("No CT with ID {reference} exists."))?;
        return Ok((reference.to_string(), hostname));
    }
    let ctid = find_ct_by_hostname(reference)
        .ok_or_else(|| format!("No CT named \"{reference}\" exists."))?;
    Ok((ctid, reference.to_string()))
}

fn remove_tunnel(positionals: &[String], options: &std::collections::HashMap<String, String>) -> Result<(), String> {
    if positionals.len() > 2 {
        return Err("Usage: eco prox remove-tunnel [domain] [--target <ctid-or-hostname>] [--account <name>] [--dry-run]".to_string());
    }
    let domain = positionals.get(1).cloned();
    let target_ref = options.get("target").cloned().unwrap_or_else(|| "proxy".to_string());
    let account = options.get("account").cloned().unwrap_or_default();
    let (ctid, hostname) = resolve_tunnel_target(&target_ref)?;
    ensure_ct_running(&ctid)?;
    let tunnels = list_tunnel_configs(&ctid)?;

    let Some(domain) = domain else {
        if tunnels.is_empty() {
            util::println_stdout(&format!("No cloudflared tunnel configuration found in CT {ctid} ({hostname})."));
            return Ok(());
        }
        util::println_stdout(&format!("Cloudflared tunnel configuration in CT {ctid} ({hostname}):"));
        for (tunnel, _, config_path, hostnames) in &tunnels {
            util::println_stdout(&format!(
                "  {tunnel}\n    config: {config_path}\n    hostnames: {}",
                if hostnames.is_empty() { "(none)".to_string() } else { hostnames.join(", ") }
            ));
        }
        return Ok(());
    };

    let matches: Vec<(String, String, String, Vec<String>, Vec<String>)> = tunnels
        .iter()
        .filter_map(|(config_path, tunnel, tunnel_id, hostnames)| {
            let selected: Vec<String> = hostnames
                .iter()
                .filter(|h| hostname_matches_removal(h, &domain))
                .cloned()
                .collect();
            if selected.is_empty() {
                None
            } else {
                Some((config_path.clone(), tunnel.clone(), tunnel_id.clone(), selected, hostnames.clone()))
            }
        })
        .collect();

    if matches.is_empty() {
        return Err(format!(
            "No configured tunnel for {domain} was found in CT {ctid} ({hostname}). Run `eco prox remove-tunnel --target {hostname}` to list configured tunnels."
        ));
    }

    for (config_path, _, tunnel_id, _, _) in &matches {
        if tunnel_id.is_empty() {
            return Err(format!(
                "Tunnel {} at {config_path} is not Eco-managed (missing # eco-tunnel-id). Refusing to delete a remote tunnel without its exact ID.",
                config_path.rsplit('/').next().unwrap_or("")
            ));
        }
        if config_path.starts_with("/etc/cloudflared-") && account.is_empty() {
            return Err(format!(
                "Tunnel at {config_path} uses a named Cloudflare account. Pass --account <name> so Eco uses that account's CF_API_TOKEN_<NAME>, CF_ACCOUNT_ID_<NAME>, and CF_ZONE_ID_<NAME>."
            ));
        }
    }

    let plan: Vec<String> = matches
        .iter()
        .map(|(config_path, _, tunnel_id, selected, hostnames)| {
            let remaining: Vec<&String> = hostnames.iter().filter(|h| !selected.contains(h)).collect();
            if remaining.is_empty() {
                format!(
                    "Stop/disable {}, delete remote tunnel {tunnel_id} and DNS records, then remove {config_path}",
                    tunnel_service_for_config(config_path)
                )
            } else {
                format!(
                    "Remove {} from remote tunnel {tunnel_id}, their DNS records, and {config_path}; preserve {}",
                    selected.join(", "),
                    remaining.iter().map(|h| h.as_str()).collect::<Vec<_>>().join(", ")
                )
            }
        })
        .collect();

    if options.get("dry-run").map(|v| v == "true").unwrap_or(false) {
        let out = format!(
            "eco prox remove-tunnel plan\n  CT: {ctid} ({hostname})\n  Domain: {domain}\n  Cloudflare account: {}\n{}\n",
            if account.is_empty() { "default" } else { &account },
            plan.iter().map(|p| format!("  {p}")).collect::<Vec<_>>().join("\n")
        );
        print!("{out}");
        return Ok(());
    }

    for (config_path, tunnel, tunnel_id, selected, hostnames) in matches {
        let remaining: Vec<&String> = hostnames.iter().filter(|h| !selected.contains(h)).collect();
        if remaining.is_empty() {
            // deleteRemoteTunnelAfterStopping
            let service = tunnel_service_for_config(&config_path);
            let _ = run(
                "pct",
                &["exec".to_string(), ctid.clone(), "--".to_string(), "bash".to_string(), "-lc".to_string(), format!("systemctl disable --now {} >/dev/null 2>&1 || true", shell_quote(&service))],
                false,
            );
            util::println_stdout(&format!("[CT {ctid}] Stopped and disabled {service}; waiting for Cloudflare to drop its tunnel connections."));
            for attempt in 1..=6 {
                match cloudflare::remove_remote_tunnel(&tunnel_id, &account) {
                    Ok(()) => break,
                    Err(e) => {
                        if e.to_lowercase().contains("tunnel has active connections") && attempt < 6 {
                            util::println_stdout(&format!(
                                "Cloudflare still reports active connections for tunnel {tunnel_id}. Retrying in 10 seconds ({attempt}/5)..."
                            ));
                            util::sleep_ms(10_000);
                        } else {
                            return Err(e);
                        }
                    }
                }
            }
        } else {
            for hostname in &selected {
                cloudflare::remove_remote_tunnel_hostname(&tunnel_id, hostname, &account)?;
                util::println_stdout(&format!("Removed {hostname} from remote Cloudflare tunnel {tunnel_id}."));
            }
        }

        for hostname in &selected {
            if cloudflare::remove_dns_record_for_tunnel(hostname, &tunnel_id, &account)? {
                util::println_stdout(&format!("Removed Cloudflare DNS record for {hostname}."));
            }
        }

        if remaining.is_empty() {
            let service = tunnel_service_for_config(&config_path);
            util::println_stdout(&format!("Deleted remote Cloudflare tunnel {tunnel_id}."));
            let _ = run(
                "pct",
                &["exec".to_string(), ctid.clone(), "--".to_string(), "bash".to_string(), "-lc".to_string(), format!("rm -f {} {}.bak.*", shell_quote(&config_path), shell_quote(&config_path))],
                false,
            );
            util::println_stdout(&format!("[CT {ctid}] Removed local {service} configuration for tunnel {tunnel}."));
        } else {
            for hostname in &selected {
                remove_local_tunnel_hostname(&ctid, &config_path, hostname)?;
            }
            util::println_stdout(&format!(
                "[CT {ctid}] Preserved shared {} tunnel configuration for {}.",
                tunnel_service_for_config(&config_path),
                remaining.iter().map(|h| h.as_str()).collect::<Vec<_>>().join(", ")
            ));
        }
    }
    util::println_stdout("Tunnel removal complete. Rerun eco up to bootstrap a replacement only if its tunnel was fully removed.");
    Ok(())
}

fn remove_local_tunnel_hostname(ctid: &str, config_path: &str, hostname: &str) -> Result<(), String> {
    let command = format!(
        "tmp=$(mktemp)\nawk -v remove_hostname={} '\n  /^  - hostname:[[:space:]]*/ {{ value = $0; sub(/^  - hostname:[[:space:]]*/, \"\", value); skip = (value == remove_hostname); if (!skip) print; next }}\n  /^  - / {{ skip = 0 }}\n  !skip {{ print }}\n' {} > \"$tmp\"\ninstall -m 0644 \"$tmp\" {}\nrm -f \"$tmp\"",
        shell_quote(hostname),
        shell_quote(config_path),
        shell_quote(config_path)
    );
    run("pct", &["exec".to_string(), ctid.to_string(), "--".to_string(), "bash".to_string(), "-lc".to_string(), command], false)?;
    Ok(())
}

fn clear_env(positionals: &[String], options: &std::collections::HashMap<String, String>) -> Result<(), String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let env_files = find_files(&cwd, ".env");
    let state_files = find_files(&cwd, ".configure-state");

    if env_files.is_empty() && state_files.is_empty() {
        util::println_stdout(&format!("No .env or .configure-state files found in {}", cwd.display()));
        return Ok(());
    }
    let _ = positionals;
    if !env_files.is_empty() {
        util::println_stdout(&format!("Found {} .env file(s):", env_files.len()));
        for f in &env_files {
            util::println_stdout(&format!("  {f}"));
        }
    }
    if !state_files.is_empty() {
        util::println_stdout(&format!("Found {} .configure-state file(s) — will clear ECO_PORTS_CONFIGURED:", state_files.len()));
        for f in &state_files {
            util::println_stdout(&format!("  {f}"));
        }
    }

    if options.get("dry-run").map(|v| v == "true").unwrap_or(false) {
        util::println_stdout("\n--dry-run enabled, no files modified.");
        return Ok(());
    }

    for f in &env_files {
        let _ = std::fs::remove_file(f);
    }
    if !env_files.is_empty() {
        util::println_stdout(&format!("\nRemoved {} .env file(s).", env_files.len()));
    }
    for f in &state_files {
        let content = std::fs::read_to_string(f).unwrap_or_default();
        let filtered: String = content
            .split('\n')
            .filter(|l| !l.starts_with("ECO_PORTS_CONFIGURED"))
            .collect::<Vec<_>>()
            .join("\n");
        let _ = std::fs::write(f, filtered);
    }
    if !state_files.is_empty() {
        util::println_stdout(&format!(
            "Cleared ECO_PORTS_CONFIGURED from {} .configure-state file(s) — ports will be reallocated on next eco up.",
            state_files.len()
        ));
    }
    Ok(())
}

fn find_files(root: &Path, name: &str) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(dir: &Path, name: &str, out: &mut Vec<String>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let fname = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if fname == "node_modules" || fname == ".git" {
                    continue;
                }
                walk(&path, name, out);
            } else if fname == name {
                out.push(path.display().to_string());
            }
        }
    }
    walk(root, name, &mut out);
    out
}

fn show_ports() -> Result<(), String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let env_files = find_files(&cwd, ".env");
    if env_files.is_empty() {
        util::println_stdout(&format!("No .env files found under {}", cwd.display()));
        return Ok(());
    }

    let mut found = false;
    let mut files = env_files.clone();
    files.sort();
    for file in files {
        let rel = file.strip_prefix(&format!("{}/", cwd.display())).map(|s| s.to_string()).unwrap_or_else(|| file.clone());
        let parts: Vec<&str> = rel.split('/').collect();
        let project = parts[..parts.len()].iter().take(2.min(parts.len() - 1)).cloned().collect::<Vec<_>>().join("/");
        let content = std::fs::read_to_string(&file).unwrap_or_default();
        let port_lines: Vec<String> = content
            .split('\n')
            .filter(|l| {
                let t = l.trim();
                !t.is_empty()
                    && !t.starts_with('#')
                    && (t.starts_with("PORT=")
                        || t.starts_with("SERVER_PORT=")
                        || t.starts_with("APP_PORT=")
                        || t.starts_with("SERVICE_PORT=")
                        || t.starts_with("HTTP_PORT=")
                        || t.starts_with("GRPC_PORT=")
                        || t.starts_with("WS_PORT="))
            })
            .map(|l| l.trim().to_string())
            .collect();
        if port_lines.is_empty() {
            continue;
        }
        found = true;
        util::println_stdout(&format!("\n[{project}]  {file}"));
        for line in port_lines {
            util::println_stdout(&format!("  {line}"));
        }
    }
    let _ = &mut found;
    if !found {
        util::println_stdout(&format!("No port variables found in any .env under {}", cwd.display()));
    }
    Ok(())
}

fn rename_pct(positionals: &[String]) -> Result<(), String> {
    let ctid = positionals.get(1).cloned().ok_or("Usage: eco prox rename-pct <ctid> <new-hostname>")?;
    let new_hostname = positionals.get(2).cloned().ok_or("Usage: eco prox rename-pct <ctid> <new-hostname>")?;
    if !ctid.chars().all(|c| c.is_ascii_digit()) {
        return Err("CTID must be a number.".to_string());
    }
    let lower = new_hostname.to_lowercase();
    if !is_valid_hostname(&lower) {
        return Err("Invalid hostname.".to_string());
    }

    let config_result = run("pct", &["config".to_string(), ctid.clone()], true)?;
    let old_hostname = config_result
        .stdout
        .split('\n')
        .find(|l| l.starts_with("hostname:"))
        .and_then(|l| l.split(": ").nth(1))
        .map(|s| s.trim().to_string())
        .ok_or_else(|| format!("CT {ctid} not found or has no hostname."))?;
    if old_hostname == lower {
        util::println_stdout(&format!("CT {ctid} already uses hostname {new_hostname}."));
        return Ok(());
    }

    let backup_dir = "/root/pct-config-backups".to_string();
    let timestamp = chrono::Utc::now().format("%Y%m%d%H%M%S").to_string();
    let backup_file = format!("{backup_dir}/{ctid}-{old_hostname}-{timestamp}.conf");
    let _ = std::fs::create_dir_all(&backup_dir);
    let conf = run("pct", &["config".to_string(), ctid.clone()], true)?;
    let _ = std::fs::write(&backup_file, &conf.stdout);

    util::println_stdout(&format!(
        "CTID          : {ctid}\nHostname lama : {old_hostname}\nHostname baru : {lower}\nBackup        : {backup_file}\n"
    ));
    run("pct", &["set".to_string(), ctid.clone(), "--hostname".to_string(), lower.clone()], false)?;

    let status_result = run("pct", &["status".to_string(), ctid.clone()], true)?;
    let status = status_result.stdout.trim().split_whitespace().nth(1).unwrap_or("").to_string();

    if status == "running" {
        util::println_stdout("Memperbarui /etc/hostname dan /etc/hosts di dalam CT...");
        let inner_script = format!(
            "set -eu\nprintf '%s\\n' '{lower}' > /etc/hostname\n[ -f /etc/hosts ] && cp -a /etc/hosts \"/etc/hosts.rename-backup.$(date +%Y%m%d-%H%M%S)\"\nawk -v old='{old_hostname}' -v new='{lower}' '{{for(i=1;i<=NF;i++){{if($i==old)$i=new}};print}}' /etc/hosts > /etc/hosts.new && mv /etc/hosts.new /etc/hosts"
        );
        run("pct", &["exec".to_string(), ctid.clone(), "--".to_string(), "sh".to_string(), "-c".to_string(), inner_script], false)?;
        util::println_stdout("Reboot CT agar hostname kernel ikut berubah...");
        run("pct", &["reboot".to_string(), ctid.clone()], false)?;
        util::println_stdout("Menunggu CT kembali aktif...");
        for _ in 0..30 {
            if run("pct", &["exec".to_string(), ctid.clone(), "--".to_string(), "true".to_string()], true).is_ok() {
                break;
            }
            util::sleep_ms(1000);
        }
    } else {
        util::println_stdout("CT sedang mati. Perubahan Proxmox diterapkan saat CT dinyalakan.");
    }

    util::println_stdout("\n=== Verifikasi ===");
    let final_config = run("pct", &["config".to_string(), ctid.clone()], true)?;
    let hostname_line = final_config.stdout.split('\n').find(|l| l.starts_with("hostname:")).unwrap_or("");
    util::println_stdout(hostname_line);
    if status == "running" {
        let hn = run("pct", &["exec".to_string(), ctid.clone(), "--".to_string(), "hostname".to_string()], true)?;
        util::println_stdout(&format!("Kernel hostname : {}", hn.stdout.trim()));
    }
    util::println_stdout("\nRename selesai.");
    Ok(())
}

fn is_valid_hostname(hostname: &str) -> bool {
    !hostname.is_empty()
        && hostname.len() <= 63
        && hostname
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
        && hostname.starts_with(|c: char| c.is_ascii_alphanumeric())
        && hostname.ends_with(|c: char| c.is_ascii_alphanumeric())
}

fn shrink_pct(positionals: &[String]) -> Result<(), String> {
    let ctid = positionals.get(1).cloned().ok_or("Usage: eco prox shrink-pct <ctid> <target-gb> [temp-ctid]")?;
    let target_gb = positionals.get(2).cloned().ok_or("Usage: eco prox shrink-pct <ctid> <target-gb> [temp-ctid]")?;
    let temp_id = positionals.get(3).cloned().unwrap_or_else(|| "900".to_string());

    let backup_dir = "/var/lib/vz/dump".to_string();
    let max_usage_pct = 70i64;

    util::println_stdout(&format!(
        "============================================================\nShrink Proxmox LXC\n============================================================\nCT asli      : {ctid}\nTarget disk  : {target_gb} GB\nCT sementara : {temp_id}\n"
    ));

    let temp_status = run("pct", &["status".to_string(), temp_id.clone()], true);
    if temp_status.is_ok() {
        return Err(format!("CT sementara {temp_id} sudah ada. Pilih ID lain."));
    }

    let config_result = run("pct", &["config".to_string(), ctid.clone()], true)?;
    let rootfs_line = config_result
        .stdout
        .split('\n')
        .find(|l| l.starts_with("rootfs:"))
        .and_then(|l| l.split(": ").nth(1))
        .map(|s| s.trim().to_string())
        .ok_or_else(|| format!("Konfigurasi rootfs CT {ctid} tidak ditemukan."))?;
    let storage = rootfs_line.split(':').next().unwrap_or("").to_string();
    util::println_stdout(&format!("Storage : {storage}\n"));

    let status_result = run("pct", &["status".to_string(), ctid.clone()], true)?;
    let ct_status = status_result.stdout.trim().split_whitespace().nth(1).unwrap_or("").to_string();
    let mut started_by_script = false;
    if ct_status != "running" {
        util::println_stdout("Menyalakan CT untuk memeriksa penggunaan disk...");
        run("pct", &["start".to_string(), ctid.clone()], false)?;
        started_by_script = true;
        util::sleep_ms(3000);
    }

    let df_result = run("pct", &["exec".to_string(), ctid.clone(), "--".to_string(), "df".to_string(), "-B1".to_string(), "--output=used,size".to_string(), "/".to_string()], true)?;
    let df_line = df_result.stdout.trim().split('\n').nth(1).unwrap_or("").trim().to_string();
    let mut nums = df_line.split_whitespace().filter_map(|s| s.parse::<u64>().ok());
    let used_bytes = nums.next().unwrap_or(0);
    let total_bytes = nums.next().unwrap_or(0);
    let target_bytes = target_gb.parse::<u64>().unwrap_or(0) * 1024 * 1024 * 1024;
    let usage_pct = if target_bytes > 0 { (used_bytes * 100 / target_bytes) as i64 } else { 0 };

    util::println_stdout(&format!(
        "Pemakaian saat ini: {} MB / Target: {target_gb} GB ({usage_pct}% terpakai)\n",
        used_bytes / 1024 / 1024
    ));

    if used_bytes >= target_bytes {
        return Err("Data lebih besar daripada target disk.".to_string());
    }
    if usage_pct > max_usage_pct {
        return Err(format!("Target terlalu sempit: akan terpakai {usage_pct}%. Maksimum {max_usage_pct}%."));
    }
    let _ = total_bytes;
    let _ = started_by_script;

    util::println_stdout("Menghentikan CT untuk backup konsisten...");
    let current_status = run("pct", &["status".to_string(), ctid.clone()], true)?.stdout.trim().split_whitespace().nth(1).unwrap_or("").to_string();
    if current_status == "running" {
        if run("pct", &["shutdown".to_string(), ctid.clone(), "--timeout".to_string(), "60".to_string()], false).is_err() {
            let _ = run("pct", &["stop".to_string(), ctid.clone()], false);
        }
    }

    util::println_stdout("Membuat backup...");
    run("vzdump", &[ctid.clone(), "--dumpdir".to_string(), backup_dir.clone(), "--mode".to_string(), "stop".to_string(), "--compress".to_string(), "zstd".to_string()], false)?;

    let find_result = run(
        "find",
        &[
            backup_dir.clone(),
            "-maxdepth".to_string(),
            "1".to_string(),
            "-type".to_string(),
            "f".to_string(),
            "-name".to_string(),
            format!("vzdump-lxc-{ctid}-*.tar.zst"),
        ],
        true,
    )?;
    let mut backup_files: Vec<String> = find_result
        .stdout
        .trim()
        .split('\n')
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .collect();
    backup_files.sort();
    let backup_file = backup_files.pop().ok_or("File backup tidak ditemukan.")?;
    util::println_stdout(&format!("Backup: {backup_file}\n"));

    util::println_stdout(&format!("Restore ke CT sementara {temp_id} dengan disk {target_gb} GB..."));
    run("pct", &["restore".to_string(), temp_id.clone(), backup_file.clone(), "--storage".to_string(), storage.clone(), "--rootfs".to_string(), format!("{storage}:{target_gb}")], false)?;
    run("pct", &["start".to_string(), temp_id.clone()], false)?;
    util::sleep_ms(5000);

    util::println_stdout("\n=== Hasil CT Sementara ===");
    let df_new = run("pct", &["exec".to_string(), temp_id.clone(), "--".to_string(), "df".to_string(), "-h".to_string(), "/".to_string()], true)?;
    util::println_stdout(&df_new.stdout);
    let ip_result = run("pct", &["exec".to_string(), temp_id.clone(), "--".to_string(), "hostname".to_string(), "-I".to_string()], true)?;
    util::println_stdout(&format!("IP: {}", ip_result.stdout.trim()));

    util::println_stdout(&format!(
        "\nCT asli {ctid} dalam keadaan mati. CT sementara {temp_id} berjalan.\nUji CT sementara, lalu jalankan:\n  pct stop {temp_id} && pct destroy {temp_id} --purge\n  pct restore {ctid} {backup_file} --storage {storage} --rootfs {storage}:{target_gb}\n  pct start {ctid}\n\nBackup rollback tersedia di:\n  {backup_file}\n"
    ));
    Ok(())
}

fn size_pct() -> Result<(), String> {
    let list_result = run("pct", &["list".to_string()], true)?;
    let ctids: Vec<String> = list_result
        .stdout
        .trim()
        .split('\n')
        .skip(1)
        .filter_map(|l| l.trim().split_whitespace().next().map(|s| s.to_string()))
        .collect();

    util::println_stdout(&format!(
        "{:<6} {:<20} {:<10} {:<10} {:<10} FREE%",
        "CTID", "NAME", "SIZE", "USED", "FREE"
    ));

    for ctid in ctids {
        let config_result = run("pct", &["config".to_string(), ctid.clone()], true)?;
        let name = config_result
            .stdout
            .split('\n')
            .find(|l| l.starts_with("hostname:"))
            .and_then(|l| l.split(": ").nth(1))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| ctid.clone());
        let df_result = run("pct", &["exec".to_string(), ctid.clone(), "--".to_string(), "df".to_string(), "-h".to_string(), "/".to_string()], true);
        match df_result {
            Ok(r) if r.code == 0 => {
                let parts: Vec<&str> = r.stdout.trim().split('\n').nth(1).unwrap_or("").trim().split_whitespace().collect();
                if parts.len() >= 5 {
                    let size = parts[0];
                    let used = parts[1];
                    let avail = parts[2];
                    let use_pct = parts[4].trim_end_matches('%').parse::<i64>().unwrap_or(0);
                    let free_pct = 100 - use_pct;
                    util::println_stdout(&format!(
                        "{:<6} {:<20} {:<10} {:<10} {:<10} {}%",
                        ctid, name, size, used, avail, free_pct
                    ));
                }
            }
            _ => {
                util::println_stdout(&format!("{:<6} {:<20} {:<10}", ctid, name, "(stopped)"));
            }
        }
    }
    Ok(())
}

fn set_ct_resources(positionals: &[String], options: &std::collections::HashMap<String, String>) -> Result<(), String> {
    let reference = positionals.get(1).cloned().ok_or("Usage: eco prox set-ct <ctid-or-hostname> --cores <n> --memory <mb> [--swap <mb>] [--dry-run]")?;
    let (ctid, hostname) = resolve_ct_by_reference(&reference)?;

    let mut settings: Vec<String> = Vec::new();
    let mut desc: Vec<String> = Vec::new();
    if let Some(cores) = options.get("cores") {
        settings.push("--cores".to_string());
        settings.push(cores.clone());
        desc.push(format!("cores: {cores}"));
    }
    if let Some(memory) = options.get("memory") {
        settings.push("--memory".to_string());
        settings.push(memory.clone());
        desc.push(format!("memory: {memory} MB"));
    }
    if let Some(swap) = options.get("swap") {
        settings.push("--swap".to_string());
        settings.push(swap.clone());
        desc.push(format!("swap: {swap} MB"));
    }
    if settings.is_empty() {
        return Err("At least one of --cores, --memory, or --swap is required.".to_string());
    }

    if options.get("dry-run").map(|v| v == "true").unwrap_or(false) {
        let out = format!(
            "eco prox set-ct plan\n  CT: {ctid} ({hostname})\n  Set: {}\n  Command: pct set {ctid} {}\n",
            desc.join(", "),
            settings.join(" ")
        );
        print!("{out}");
        return Ok(());
    }

    util::println_stdout(&format!("[CT {ctid}] Setting {}...", desc.join(", ")));
    let mut args = vec!["set".to_string(), ctid.clone()];
    args.extend(settings);
    run("pct", &args, false)?;
    util::println_stdout(&format!("CT {ctid} ({hostname}) updated."));
    Ok(())
}

fn install_minio(ctid: &str, reset: bool) -> Result<(), String> {
    let installer = crate::embedded::INSTALL_MINIO_SH;
    let temp_dir = std::env::temp_dir().join(format!("eco-minio-installer-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&temp_dir);
    let source = temp_dir.join("install-minio.sh");
    std::fs::write(&source, installer).map_err(|e| e.to_string())?;
    crate::util::make_executable(&source);
    let result = (|| -> Result<(), String> {
        util::println_stdout(&format!("[CT {ctid}] Uploading managed MinIO installer..."));
        run("pct", &["push".to_string(), ctid.to_string(), source.display().to_string(), "/tmp/eco-install-minio.sh".to_string()], false)?;
        let reset_flag = if reset { " --reset" } else { "" };
        util::println_stdout(&format!("[CT {ctid}] Installing and starting MinIO..."));
        run("pct", &["exec".to_string(), ctid.to_string(), "--".to_string(), "bash".to_string(), "-lc".to_string(), format!("chmod 700 /tmp/eco-install-minio.sh && ECO_DEPLOY_MODE=prod bash /tmp/eco-install-minio.sh --ensure{reset_flag} && rm -f /tmp/eco-install-minio.sh")], false)?;
        util::println_stdout(&format!("[CT {ctid}] Checking MinIO health..."));
        let health = run("pct", &["exec".to_string(), ctid.to_string(), "--".to_string(), "bash".to_string(), "-lc".to_string(), "curl -fsS http://127.0.0.1:9000/minio/health/live >/dev/null".to_string()], true);
        if let Err(e) = health {
            let mut diagnostics = String::new();
            if let Ok(result) = run("pct", &["exec".to_string(), ctid.to_string(), "--".to_string(), "bash".to_string(), "-lc".to_string(), "systemctl status eco-minio --no-pager -l; journalctl -u eco-minio --no-pager -n 80".to_string()], true) {
                diagnostics = format!("{}\n{}", result.stdout, result.stderr);
            }
            return Err(format!(
                "{e}\n\nMinIO service diagnostics from CT {ctid}:\n{diagnostics}"
            ));
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&temp_dir);
    result
}

fn archive_workload(positionals: &[String], options: &std::collections::HashMap<String, String>) -> Result<(), String> {
    let reference = positionals.get(1).cloned();
    let output = options.get("output").cloned();
    let (Some(reference), Some(output)) = (reference, output) else {
        return Err("Usage: eco prox archive <vm-or-ct> --output <external-directory>".to_string());
    };
    let output_directory = util::current_dir().join(&output);
    if !output_directory.is_dir() {
        return Err(format!("Archive output directory does not exist: {}", output_directory.display()));
    }
    // Determine workload kind
    let vm_name = existing_vm_name(&reference);
    let ct_name = existing_ct_hostname(&reference);
    let (kind, id, name) = if reference.chars().all(|c| c.is_ascii_digit()) {
        if let Some(vm) = vm_name {
            ("qemu".to_string(), reference.clone(), vm)
        } else if let Some(ct) = ct_name {
            ("lxc".to_string(), reference.clone(), ct)
        } else {
            return Err(format!("No VM or CT with ID {reference} exists."));
        }
    } else if let (Some(vm), Some(ct)) = (&vm_name, &ct_name) {
        return Err(format!("\"{reference}\" matches both VM {vm} and CT {ct}; use the numeric ID."));
    } else if let Some(vm) = vm_name {
        ("qemu".to_string(), reference.clone(), vm)
    } else if let Some(ct) = ct_name {
        ("lxc".to_string(), reference.clone(), ct)
    } else {
        return Err(format!("No VM or CT named \"{reference}\" exists."));
    };

    let format = options.get("format").cloned().unwrap_or_else(|| {
        if kind == "qemu" { "qcow2".to_string() } else { "vzdump".to_string() }
    });
    if format != "qcow2" && format != "vzdump" {
        return Err("--format must be qcow2 or vzdump.".to_string());
    }
    if kind == "lxc" && format != "vzdump" {
        return Err("CT archives support only --format vzdump.".to_string());
    }
    let mode = options.get("mode").cloned().unwrap_or_else(|| {
        if format == "qcow2" { "stop".to_string() } else { "snapshot".to_string() }
    });
    if mode != "snapshot" && mode != "suspend" && mode != "stop" {
        return Err("--mode must be snapshot, suspend, or stop.".to_string());
    }
    let prefix = format!("vzdump-{kind}-{id}-");

    if options.get("dry-run").map(|v| v == "true").unwrap_or(false) {
        let command = if format == "qcow2" {
            "qemu-img convert -O qcow2 -c <source-disk> <external-output>".to_string()
        } else {
            format!("vzdump {id} --dumpdir {} --mode {mode} --compress zstd", output_directory.display())
        };
        let out = format!(
            "Archive plan\n  Workload: {} {id} ({name})\n  Format:   {format}\n  Output:   {}\n  Mode:     {mode}\n  Command:  {command}\n",
            if kind == "qemu" { "VM" } else { "CT" },
            output_directory.display()
        );
        print!("{out}");
        return Ok(());
    }

    if format == "qcow2" {
        return archive_qcow2_vm(&id, &name, &output_directory);
    }

    util::println_stdout(&format!(
        "Archiving {} {id} ({name}) to {}...",
        if kind == "qemu" { "VM" } else { "CT" },
        output_directory.display()
    ));
    let started_at = chrono::Utc::now().timestamp_millis();
    run("vzdump", &[id.clone(), "--dumpdir".to_string(), output_directory.display().to_string(), "--mode".to_string(), mode.clone(), "--compress".to_string(), "zstd".to_string()], false)?;
    let archive = latest_archive(&output_directory, &prefix, started_at)?;
    let manifest_path = format!("{}.eco.json", archive);
    let manifest = serde_json::json!({
        "version": 1,
        "createdAt": chrono::Utc::now().to_rfc3339(),
        "kind": kind,
        "sourceId": id,
        "sourceName": name,
        "archive": std::path::Path::new(&archive).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
    });
    let _ = std::fs::write(&manifest_path, format!("{}\n", serde_json::to_string_pretty(&manifest).unwrap_or_default()));
    util::println_stdout(&format!("Archive complete:\n  {archive}\n  {manifest_path}\n\nCopy both files offsite before deleting the original. Restore with:\n  eco prox unarchive {archive} --id <new-id>"));
    Ok(())
}

fn existing_vm_name(vmid: &str) -> Option<String> {
    let r = run("qm", &["config".to_string(), vmid.to_string()], true).ok()?;
    r.stdout
        .lines()
        .find_map(|l| l.strip_prefix("name:"))
        .map(|n| n.trim().to_string())
}

fn latest_archive(directory: &Path, prefix: &str, started_at: i64) -> Result<String, String> {
    let entries = std::fs::read_dir(directory).map_err(|e| e.to_string())?;
    let mut candidates: Vec<(String, i64, u64)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with(prefix) {
            continue;
        }
        let path = entry.path();
        if let Ok(meta) = std::fs::metadata(&path) {
            if meta.is_file() && meta.modified().map(|m| m.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)).unwrap_or(0) >= started_at - 2000 {
                candidates.push((name, meta.modified().map(|m| m.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)).unwrap_or(0), meta.len()));
            }
        }
    }
    candidates.sort_by(|a, b| b.1.cmp(&a.1));
    candidates.first().map(|c| directory.join(&c.0).display().to_string()).ok_or_else(|| {
        format!("vzdump succeeded but no {prefix} archive was found in {}.", directory.display())
    })
}

fn archive_qcow2_vm(id: &str, name: &str, output_directory: &Path) -> Result<(), String> {
    let config = run("qm", &["config".to_string(), id.to_string()], true)?.stdout;
    let status = run("qm", &["status".to_string(), id.to_string()], true)?;
    let was_running = status.stdout.contains("status: running");

    let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H-%M-%S").to_string();
    let archive_directory = output_directory.join(format!("eco-qemu-{id}-{timestamp}"));
    std::fs::create_dir_all(&archive_directory).map_err(|e| e.to_string())?;

    let result = (|| -> Result<(), String> {
        if was_running {
            util::println_stdout(&format!("Stopping VM {id} for a consistent QCOW2 archive..."));
            run("qm", &["shutdown".to_string(), id.to_string(), "--timeout".to_string(), "180".to_string()], false)?;
            wait_for_vm_state(id, "stopped")?;
        }
        let disks = parse_qm_disks(&config);
        let mut disk_list = Vec::new();
        for disk in disks {
            let source = run("pvesm", &["path".to_string(), disk.1.clone()], true)?.stdout.trim().to_string();
            if source.is_empty() {
                return Err(format!("Could not resolve storage path for {}.", disk.1));
            }
            let filename = format!("{}.qcow2", disk.0);
            util::println_stdout(&format!("Compressing {} directly to external storage...", disk.0));
            run("qemu-img", &["convert".to_string(), "-p".to_string(), "-O".to_string(), "qcow2".to_string(), "-c".to_string(), source.clone(), archive_directory.join(&filename).display().to_string()], false)?;
            disk_list.push(serde_json::json!({
                "slot": disk.0,
                "volume": disk.1,
                "options": disk.2,
                "filename": filename
            }));
        }
        let _ = std::fs::write(archive_directory.join("vm.conf"), &config);
        let manifest = serde_json::json!({
            "version": 2,
            "format": "qcow2",
            "createdAt": chrono::Utc::now().to_rfc3339(),
            "kind": "qemu",
            "sourceId": id,
            "sourceName": name,
            "configFile": "vm.conf",
            "disks": disk_list
        });
        let _ = std::fs::write(archive_directory.join("eco-archive.json"), format!("{}\n", serde_json::to_string_pretty(&manifest).unwrap_or_default()));
        util::println_stdout(&format!(
            "Archive complete:\n  {}\n\nRestore with:\n  eco prox unarchive {} --id <new-id> --storage local-lvm\n",
            archive_directory.display(),
            archive_directory.display()
        ));
        Ok(())
    })();

    if was_running {
        if run("qm", &["start".to_string(), id.to_string()], false).is_err() {
            util::eprintln_stderr(&format!("Could not restart VM {id}: start failed"));
        }
    }
    if let Err(e) = &result {
        util::eprintln_stderr(&format!(
            "QCOW2 archive did not complete. Partial files are in {}; remove that directory from the external drive before retrying.",
            archive_directory.display()
        ));
        return Err(e.clone());
    }
    result
}

fn wait_for_vm_state(vmid: &str, state: &str) -> Result<(), String> {
    for _ in 0..90 {
        let status = run("qm", &["status".to_string(), vmid.to_string()], true)?;
        if status.stdout.contains(&format!("status: {state}")) {
            return Ok(());
        }
        util::sleep_ms(2000);
    }
    Err(format!("VM {vmid} did not become {state} in time."))
}

fn parse_qm_disks(config: &str) -> Vec<(String, String, String)> {
    config
        .split('\n')
        .filter_map(|line| {
            let mut parts = line.splitn(2, ':');
            let slot = parts.next()?.trim().to_string();
            let rest = parts.next()?.trim().to_string();
            let mut tokens = rest.split(',');
            let volume = tokens.next()?.to_string();
            if volume == "none" || volume == "cdrom" {
                return None;
            }
            let options = tokens.collect::<Vec<_>>().join(",");
            if !["scsi", "sata", "ide", "virtio", "efidisk0", "tpmstate0"]
                .iter()
                .any(|p| slot == *p || slot.starts_with(&format!("{p}")))
            {
                return None;
            }
            Some((slot, volume, options))
        })
        .collect()
}

fn unarchive_workload(positionals: &[String], options: &std::collections::HashMap<String, String>) -> Result<(), String> {
    let archive = positionals.get(1).cloned().ok_or("Usage: eco prox unarchive <archive-directory-or-vzdump-archive> --id <new-id> [--storage <storage>]")?;
    let target_id = options.get("id").cloned().ok_or("Usage: eco prox unarchive <archive-directory-or-vzdump-archive> --id <new-id> [--storage <storage>]")?;
    if target_id.parse::<i64>().is_err() || target_id.parse::<i64>().unwrap() <= 0 {
        return Err("--id must be a positive numeric VM/CT ID.".to_string());
    }
    let archive_path = util::current_dir().join(&archive);
    if archive_path.is_dir() {
        return unarchive_qcow2_vm(&archive_path, &target_id, options);
    }
    if !archive_path.is_file() {
        return Err(format!("Archive is not a regular file or QCOW2 archive directory: {}", archive_path.display()));
    }
    let filename = archive_path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let kind = if filename.contains("vzdump-qemu-") && filename.ends_with(".vma.zst") {
        "qemu"
    } else if filename.contains("vzdump-lxc-") && filename.ends_with(".tar.zst") {
        "lxc"
    } else {
        return Err("Unsupported archive. Use a native Proxmox vzdump-qemu *.vma.zst or vzdump-lxc *.tar.zst file.".to_string());
    };
    let exists = if kind == "qemu" {
        existing_vm_name(&target_id).is_some()
    } else {
        existing_ct_hostname(&target_id).is_some()
    };
    if exists {
        return Err(format!("Target {} ID {target_id} already exists. Refusing to overwrite it.", if kind == "qemu" { "VM" } else { "CT" }));
    }
    let (command, mut command_args) = if kind == "qemu" {
        (
            "qmrestore".to_string(),
            vec![archive_path.display().to_string(), target_id.clone(), "--unique".to_string(), "1".to_string()],
        )
    } else {
        (
            "pct".to_string(),
            vec!["restore".to_string(), target_id.clone(), archive_path.display().to_string()],
        )
    };
    if let Some(storage) = options.get("storage") {
        command_args.push("--storage".to_string());
        command_args.push(storage.clone());
    }
    if options.get("dry-run").map(|v| v == "true").unwrap_or(false) {
        util::println_stdout(&format!("Restore plan\n  {command} {}\n", command_args.join(" ")));
        return Ok(());
    }
    util::println_stdout(&format!("Restoring {} {target_id} from {}...", if kind == "qemu" { "VM" } else { "CT" }, archive_path.display()));
    run(&command, &command_args, false)?;
    util::println_stdout(&format!(
        "Restore complete. {} {target_id} remains stopped; inspect its configuration before starting it.",
        if kind == "qemu" { "VM" } else { "CT" }
    ));
    Ok(())
}

fn unarchive_qcow2_vm(archive_path: &Path, target_id: &str, options: &std::collections::HashMap<String, String>) -> Result<(), String> {
    let manifest_path = archive_path.join("eco-archive.json");
    let manifest_text = std::fs::read_to_string(&manifest_path)
        .map_err(|_| format!("QCOW2 archive is missing a readable {}.", manifest_path.display()))?;
    let manifest: serde_json::Value = serde_json::from_str(&manifest_text)
        .map_err(|_| format!("QCOW2 archive is missing a readable {}.", manifest_path.display()))?;
    if manifest.get("format").and_then(|f| f.as_str()) != Some("qcow2")
        || manifest.get("kind").and_then(|k| k.as_str()) != Some("qemu")
        || !manifest.get("disks").map(|d| d.is_array()).unwrap_or(false)
    {
        return Err("Unsupported QCOW2 archive manifest.".to_string());
    }
    if existing_vm_name(target_id).is_some() {
        return Err(format!("Target VM ID {target_id} already exists. Refusing to overwrite it."));
    }
    let storage = options.get("storage").cloned().unwrap_or_else(|| "local-lvm".to_string());
    let config = std::fs::read_to_string(archive_path.join(manifest.get("configFile").and_then(|c| c.as_str()).unwrap_or("vm.conf")))
        .map_err(|e| e.to_string())?;
    let source_name = manifest.get("sourceName").and_then(|s| s.as_str()).unwrap_or("restored-vm");

    let mut create_args = vec!["create".to_string(), target_id.to_string(), "--name".to_string(), source_name.to_string()];
    for key in ["memory", "cores", "sockets", "cpu", "machine", "bios", "ostype", "scsihw", "agent", "vga", "tablet", "numa", "balloon", "net0", "net1", "net2", "net3"] {
        if let Some(value) = config_value(&config, key) {
            create_args.push(format!("--{key}"));
            create_args.push(value);
        }
    }

    if options.get("dry-run").map(|v| v == "true").unwrap_or(false) {
        let disks = manifest.get("disks").and_then(|d| d.as_array()).map(|a| a.len()).unwrap_or(0);
        let out = format!(
            "Restore plan\n  qm {}\n  Import {disks} QCOW2 disk image(s) into {storage}, then attach them to their original slots.\n\nManual follow-up after restore:\n  1. Verify firmware, boot order, TPM, and network: qm config {target_id}\n  2. Start only after review: qm start {target_id}\n",
            create_args.join(" ")
        );
        print!("{out}");
        return Ok(());
    }

    util::println_stdout(&format!("Creating stopped VM {target_id}..."));
    run("qm", &create_args, false)?;
    let result = (|| -> Result<(), String> {
        let disks = manifest.get("disks").and_then(|d| d.as_array().cloned()).unwrap_or_default();
        for disk in &disks {
            let filename = disk.get("filename").and_then(|f| f.as_str()).unwrap_or("");
            let image = archive_path.join(filename);
            if !image.is_file() {
                return Err(format!("Missing disk image {}.", image.display()));
            }
            util::println_stdout(&format!("Importing {filename} into {storage}..."));
            run("qm", &["importdisk".to_string(), target_id.to_string(), image.display().to_string(), storage.clone()], false)?;
            let imported = newest_unused_disk(target_id)?;
            if imported.is_empty() {
                return Err(format!("Could not identify imported disk for {filename}."));
            }
            let slot = disk.get("slot").and_then(|s| s.as_str()).unwrap_or("");
            let options_part = disk.get("options").and_then(|o| o.as_str()).unwrap_or("");
            let suffix = without_size(options_part);
            let set_val = if suffix.is_empty() { imported.clone() } else { format!("{imported},{suffix}") };
            run("qm", &["set".to_string(), target_id.to_string(), format!("--{slot}"), set_val], false)?;
        }
        if let Some(boot) = config_value(&config, "boot") {
            run("qm", &["set".to_string(), target_id.to_string(), "--boot".to_string(), boot], false)?;
        }
        Ok(())
    })();
    if let Err(e) = &result {
        util::eprintln_stderr(&format!(
            "Restore did not complete. VM {target_id} was intentionally kept for diagnosis; inspect qm config {target_id} before removing it."
        ));
        return Err(e.clone());
    }
    util::println_stdout(&format!(
        "Restore complete. VM {target_id} remains stopped.\n\nManual follow-up:\n  1. Inspect disk slots, boot order, UEFI/OVMF, TPM, and network: qm config {target_id}\n  2. If Windows asks for recovery, confirm the restored TPM state and boot disk first.\n  3. Start after review: qm start {target_id}\n"
    ));
    Ok(())
}

fn config_value(config: &str, key: &str) -> Option<String> {
    config
        .split('\n')
        .find_map(|l| {
            let l = l.trim_end_matches('\r');
            l.strip_prefix(&format!("{key}: ")).map(|v| v.trim().to_string())
        })
}

fn newest_unused_disk(vmid: &str) -> Result<String, String> {
    let config = run("qm", &["config".to_string(), vmid.to_string()], true)?.stdout;
    let mut last = String::new();
    for line in config.split('\n') {
        if let Some(rest) = line.strip_prefix("unused") {
            if let Some(idx) = rest.find(":") {
                if rest[..idx].chars().all(|c| c.is_ascii_digit()) {
                    last = rest[idx + 1..].trim().to_string();
                }
            }
        }
    }
    Ok(last)
}

fn without_size(options: &str) -> String {
    options
        .split(',')
        .filter(|e| !e.is_empty() && !e.starts_with("size="))
        .collect::<Vec<_>>()
        .join(",")
}

fn attach_minio(args: &[String], options: &std::collections::HashMap<String, String>) -> Result<(), String> {
    let reference = args.get(2).cloned().ok_or("Usage: eco prox attach minio <name-or-id> --project <bootstrap-dir>")?;
    let project = options.get("project").cloned().ok_or("Missing --project. Give the estate bootstrap directory or ecompose.yml path.")?;
    let (ctid, hostname) = resolve_ct_by_reference(&reference)?;
    let manifest_path = ecompose::resolve_ecompose_file(&project, &util::current_dir())?;
    let content = std::fs::read_to_string(&manifest_path).map_err(|e| e.to_string())?;
    let storage = ecompose::parse_storage(&content);
    let region = options
        .get("region")
        .cloned()
        .or_else(|| storage.get("minio").and_then(|m| m.get("region")).cloned())
        .unwrap_or_else(|| "us-east-1".to_string());

    let next = set_minio_storage(&content, &hostname, &region);
    if next == content {
        util::println_stdout(&format!("{} already uses MinIO CT {hostname}. Nothing changed.", manifest_path.display()));
        return Ok(());
    }
    util::println_stdout(&format!(
        "Will configure storage.minio for {}:\n  ct: {hostname} (CT {ctid})\n  region: {region}\n",
        manifest_path.display()
    ));
    if options.get("yes").map(|v| v == "true").unwrap_or(false) {
        // proceed
    } else {
        let answer = crate::checklist::prompt_line(&format!("Attach MinIO CT {ctid} ({hostname}) to {}? [y/N]: ", manifest_path.display()))?;
        if answer.to_lowercase() != "y" && answer.to_lowercase() != "yes" {
            util::println_stdout("Estate storage attachment cancelled.");
            return Ok(());
        }
    }
    std::fs::write(&manifest_path, &next).map_err(|e| e.to_string())?;
    util::println_stdout(&format!(
        "Attached MinIO CT {hostname} to {}. Commit that bootstrap-repository change, then run eco up from the estate.",
        manifest_path.display()
    ));
    Ok(())
}

fn set_minio_storage(content: &str, ct: &str, region: &str) -> String {
    let block = format!(
        "# Eco manages MinIO credentials and resolves this CT's private bridge\n# address at `eco up`; never commit endpoint or credentials here.\nstorage:\n  minio:\n    ct: {ct}\n    region: {region}\n"
    );
    let storage_start = content.lines().position(|l| l == "storage:");
    match storage_start {
        None => {
            let suffix = if content.ends_with('\n') { "" } else { "\n" };
            format!("{content}{suffix}\n{block}")
        }
        Some(start) => {
            let lines: Vec<&str> = content.split('\n').collect();
            let mut storage_end = lines.len();
            for i in (start + 1)..lines.len() {
                let l = lines[i];
                if !l.is_empty() && !l.starts_with(' ') && l.ends_with(':') && !l.starts_with('#') {
                    storage_end = i;
                    break;
                }
            }
            let minio_start = (start + 1..storage_end).find(|i| lines[*i] == "  minio:");
            let rendered: Vec<String> = vec![
                "  minio:".to_string(),
                format!("    ct: {ct}"),
                format!("    region: {region}"),
            ];
            let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
            match minio_start {
                Some(ms) => {
                    let mut minio_end = storage_end;
                    for i in (ms + 1)..storage_end {
                        if lines[i].starts_with("  ") && !lines[i].starts_with("    ") && lines[i].ends_with(':') {
                            minio_end = i;
                            break;
                        }
                    }
                    out.splice(ms..minio_end, rendered);
                }
                None => {
                    out.splice(storage_end..storage_end, rendered);
                }
            }
            out.join("\n")
        }
    }
}

pub fn run_prox(args: &[String]) -> Result<(), String> {
    let (options, positionals) = parse_args(args);
    let first = positionals.first().cloned();
    if first.is_none() || matches!(first.as_deref(), Some("help") | Some("--help") | Some("-h")) || options.get("help").is_some() {
        help_text();
        return Ok(());
    }
    let first = first.unwrap();
    let second = positionals.get(1).cloned();
    match (first.as_str(), second.as_deref()) {
        ("attach", Some("minio")) => attach_minio(&positionals, &options),
        ("archive", _) => archive_workload(&positionals, &options),
        ("unarchive", _) => unarchive_workload(&positionals, &options),
        ("clear-rust", _) => clear_rust(&positionals, &options),
        ("remove-tunnel", _) => remove_tunnel(&positionals, &options),
        ("clearenv", _) => clear_env(&positionals, &options),
        ("showports", _) => show_ports(),
        ("tunnel-replicas", _) => {
            let mut proxy_args = vec!["tunnel-replicas".to_string()];
            proxy_args.extend_from_slice(&args[1..]);
            crate::commands::proxy::run_proxy(&proxy_args)
        }
        ("rename-pct", _) => rename_pct(&positionals),
        ("shrink-pct", _) => shrink_pct(&positionals),
        ("size-pct", _) => size_pct(),
        ("set-ct", _) => set_ct_resources(&positionals, &options),
        ("prepare", Some("rust-builder")) => prepare_rust_builder(&positionals, &options),
        ("createct", Some("rust-builder")) => {
            let requested_name = positionals.get(2).cloned().or_else(|| options.get("hostname").cloned()).unwrap_or_else(|| "rust-builder".to_string());
            let hostname = options.get("hostname").cloned().unwrap_or_else(|| requested_name.clone());
            let template = resolve_installed_template(options.get("template").map(|s| s.as_str()))?;
            let known_id = options.get("id").cloned().or_else(|| {
                if options.get("dry-run").map(|v| v == "true").unwrap_or(false) {
                    None
                } else {
                    find_ct_by_hostname(&hostname)
                }
            });
            let id = known_id.or_else(|| {
                if options.get("dry-run").map(|v| v == "true").unwrap_or(false) {
                    Some("<next-available-id>".to_string())
                } else {
                    next_id().ok()
                }
            }).ok_or("cannot determine CT id")?;
            let mut merged = options.clone();
            merged.insert("disk".to_string(), options.get("disk").cloned().unwrap_or_else(|| "60".to_string()));
            merged.insert("cores".to_string(), options.get("cores").cloned().unwrap_or_else(|| "4".to_string()));
            merged.insert("memory".to_string(), options.get("memory").cloned().unwrap_or_else(|| "8192".to_string()));
            let create = pct_create_args(&id, &merged, &template, &hostname);
            if options.get("dry-run").map(|v| v == "true").unwrap_or(false) {
                let out = format!(
                    "eco prox createct rust-builder plan\n  Template: {template}\n  pct {}\n  pct start {id}\n  pct exec {id} -- install managed Rust toolchain\n",
                    create.join(" ")
                );
                print!("{out}");
                return Ok(());
            }
            let existing_hostname = existing_ct_hostname(&id);
            if let Some(eh) = existing_hostname.as_ref() {
                if eh != &hostname {
                    return Err(format!("CT {id} already belongs to hostname \"{eh}\". Refusing to modify it; choose another --id."));
                }
            }
            if existing_hostname.is_none() {
                util::println_stdout(&format!("[CT {id}] Creating {hostname}..."));
                run("pct", &create, false)?;
            }
            ensure_ct_running(&id)?;
            wait_for_ct_exec(&id, 30, 1000)?;
            install_rust_builder(&id)?;
            util::println_stdout(&format!(
                "Rust builder CT {id} is ready ({}). Set ECO_RUST_DEDICATED_BUILDER={hostname} before running eco up.",
                if existing_hostname.is_some() { "reused" } else { "created" }
            ));
            Ok(())
        }
        ("createct", Some("minio")) => {
            let requested_name = positionals.get(2).cloned().or_else(|| options.get("hostname").cloned()).unwrap_or_else(|| "minio".to_string());
            let hostname = options.get("hostname").cloned().unwrap_or_else(|| requested_name.clone());
            let template = resolve_installed_template(options.get("template").map(|s| s.as_str()))?;
            let known_id = options.get("id").cloned().or_else(|| {
                if options.get("dry-run").map(|v| v == "true").unwrap_or(false) {
                    None
                } else {
                    find_ct_by_hostname(&hostname)
                }
            });
            let id = known_id.or_else(|| {
                if options.get("dry-run").map(|v| v == "true").unwrap_or(false) {
                    Some("<next-available-id>".to_string())
                } else {
                    next_id().ok()
                }
            }).ok_or("cannot determine CT id")?;
            let create = pct_create_args(&id, &options, &template, &hostname);
            if options.get("dry-run").map(|v| v == "true").unwrap_or(false) {
                let out = format!(
                    "eco prox createct minio plan\n  Template: {template}\n  pct {}\n  pct start {id}\n  pct push {id} <eco install-minio.sh> /tmp/eco-install-minio.sh\n  pct exec {id} -- ECO_DEPLOY_MODE=prod eco install minio --ensure\n",
                    create.join(" ")
                );
                print!("{out}");
                return Ok(());
            }
            let existing_hostname = existing_ct_hostname(&id);
            if let Some(eh) = existing_hostname.as_ref() {
                if eh != &hostname {
                    return Err(format!("CT {id} already belongs to hostname \"{eh}\". Refusing to modify it; choose another --id."));
                }
            }
            let created_now = existing_hostname.is_none();
            if !created_now {
                let approved = options.get("yes-reinstall").map(|v| v == "true").unwrap_or(false)
                    || crate::checklist::prompt_line(&format!("CT {id} ({hostname}) already exists. Reinstalling MinIO WILL DELETE all objects and credentials. Type RESET to continue: "))?
                        .trim()
                        == "RESET";
                if !approved {
                    util::println_stdout(&format!("Existing MinIO CT {id} was left unchanged."));
                    return Ok(());
                }
            }
            if created_now {
                util::println_stdout(&format!("[CT {id}] Creating {hostname}..."));
                run("pct", &create, false)?;
            }
            let setup_result = (|| -> Result<(), String> {
                util::println_stdout(&format!("[CT {id}] Starting and waiting for first boot..."));
                ensure_ct_running(&id)?;
                wait_for_ct_exec(&id, 30, 1000)?;
                install_minio(&id, !created_now)
            })();
            if let Err(e) = setup_result {
                if created_now {
                    let keep = options.get("keep-on-failure").map(|v| v == "true").unwrap_or(false);
                    if keep {
                        return Err(format!(
                            "MinIO setup failed; CT {id} was intentionally kept for diagnosis.\nCause: {e}"
                        ));
                    }
                    let _ = run("pct", &["stop".to_string(), id.clone()], false);
                    let _ = run("pct", &["destroy".to_string(), id.clone(), "--purge".to_string(), "1".to_string()], false);
                    return Err(format!("MinIO setup failed; newly created CT {id} and its volumes were removed.\nCause: {e}"));
                }
                return Err(format!("MinIO setup failed on existing CT {id}; it was preserved. {e}"));
            }
            util::println_stdout(&format!(
                "MinIO CT {id} is healthy ({}). Attach it to an estate through Eco before running `eco up`.",
                if created_now { "created" } else { "reused" }
            ));
            Ok(())
        }
        _ => Err("Usage: eco prox createct minio <name> [options]".to_string()),
    }
}
