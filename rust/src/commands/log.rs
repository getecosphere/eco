//! `eco log` — local observability stack (scope: host).
//!
//! `eco log dev` starts the local Grafana + Loki stack via the `logging` LXS
//! (`server` mode provisions Loki+Grafana; `agent` tails a FIFO into Loki) and
//! an optional demo generator, so you can watch estate logs in a browser at
//! http://127.0.0.1:3000 (anonymous, "Live Logs" dashboard) without any setup.
//!
//! `eco log stop` / `eco log status` manage the stack.

use crate::util;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const LOGGING_LXS: &str = "logging";
const GRAFANA_PORT: &str = "3000";
const LOKI_PORT: &str = "3100";

fn log_dir() -> PathBuf {
    util::env_var_or("LOG_DATA_DIR", &format!("{}/.eco/logging", util::home_dir())).into()
}

fn pid_dir() -> PathBuf {
    log_dir().join("run")
}

fn fifo_path() -> PathBuf {
    PathBuf::from(util::env_var_or("ECO_LOG_FIFO", "/tmp/eco-log.fifo"))
}

/// The FIFO path `eco up dev` should point PM2 `out_file` at.
pub fn default_fifo() -> String {
    fifo_path().display().to_string()
}

fn pidfile(name: &str) -> PathBuf {
    pid_dir().join(format!("{name}.pid"))
}

fn read_pid(name: &str) -> Option<i32> {
    std::fs::read_to_string(pidfile(name))
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
}

fn is_alive(pid: i32) -> bool {
    // Unix: kill(pid, 0) via `kill -0`.
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Resolve the `logging` LXS binary (darwin/arm64) from the registry mirror.
fn resolve_logging_bin() -> Result<String, String> {
    let candidates = [
        format!(
            "{}/registry/{LOGGING_LXS}/*/darwin-arm64/{LOGGING_LXS}",
            util::home_dir()
        ),
        format!(
            "{}/{LOGGING_LXS}/*/darwin-arm64/{LOGGING_LXS}",
            util::env_var_or("ECO_LXS_REGISTRY", "")
        ),
    ];
    let mut best: Option<(u64, String)> = None;
    for pattern in candidates {
        if pattern.contains("//") {
            continue; // unset ECO_LXS_REGISTRY produces a malformed pattern
        }
        if let Ok(paths) = glob_simple(&pattern) {
            for p in paths {
                if let Some(ver) = version_of(&p) {
                    if best.as_ref().map(|(v, _)| ver > *v).unwrap_or(true) {
                        best = Some((ver, p));
                    }
                }
            }
        }
    }
    match best {
        Some((_, p)) => Ok(p),
        None => Err(format!(
            "logging LXS not found locally. Build/publish it first: \
             `eco lxs build --arch linux/amd64,darwin/arm64` in the logging domain, \
             or set ECO_LXS_REGISTRY to the registry mirror."
        )),
    }
}

/// Minimal glob expansion for `*` components in a path.
fn glob_simple(pattern: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let parts: Vec<&str> = pattern.split('/').collect();
    // Find the first `*`; expand one level only (version dirs are flat).
    if let Some(idx) = parts.iter().position(|p| *p == "*") {
        let head = parts[..idx].join("/");
        let tail: Vec<&str> = parts[idx + 1..].to_vec();
        let entries = std::fs::read_dir(&head).map_err(|e| format!("read {head}: {e}"))?;
        for e in entries.flatten() {
            let p = e.path();
            let mut full = p;
            for t in &tail {
                full = full.join(t);
            }
            if full.exists() {
                out.push(full.to_string_lossy().into_owned());
            }
        }
    } else if Path::new(pattern).exists() {
        out.push(pattern.to_string());
    }
    Ok(out)
}

fn version_of(path: &str) -> Option<u64> {
    // e.g. .../logging/1.0.5/darwin-arm64/logging → parse "1.0.5" into a sortable number
    let p = Path::new(path);
    let parent = p.parent()?; // darwin-arm64
    let ver = parent.parent()?.file_name()?.to_string_lossy().into_owned();
    let nums: Vec<u64> = ver.split('.').filter_map(|s| s.parse().ok()).collect();
    match nums.as_slice() {
        [a, b, c] => Some(a * 1_000_000 + b * 1_000 + c),
        [a, b] => Some(a * 1_000 + b),
        [a] => Some(*a),
        _ => None,
    }
}

fn ensure_dir(p: &Path) -> Result<(), String> {
    std::fs::create_dir_all(p).map_err(|e| format!("mkdir {}: {e}", p.display()))
}

/// Spawn a self-restarting wrapper that runs `cmd` forever.
fn start_loop(name: &str, cmd: &[String], stdout: Option<&Path>) -> Result<(), String> {
    ensure_dir(&pid_dir())?;
    if let Some(pid) = read_pid(name) {
        if is_alive(pid) {
            println!("[eco log] {name} already running (pid {pid})");
            return Ok(());
        }
    }
    let logfile = log_dir().join(format!("{name}.log"));
    let mut child = Command::new("sh");
    child.arg("-c").arg(loop_command(cmd, stdout.map(|p| p.to_string_lossy().into_owned())));
    if let Some(f) = stdout {
        let target = if f.exists() {
            f.to_string_lossy().into_owned()
        } else {
            format!("{}/{}", log_dir().display(), name)
        };
        // generator writes into the FIFO (stdout) and logs stderr separately.
        child.arg(stdout_arg(&target, &logfile));
    } else {
        child.arg(stdout_arg("", &logfile));
    }
    let c = child
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn {name}: {e}"))?;
    std::fs::write(pidfile(name), c.id().to_string()).map_err(|e| format!("pidfile: {e}"))?;
    println!("[eco log] started {name} (pid {})", c.id());
    Ok(())
}

fn loop_command(cmd: &[String], fifo: Option<String>) -> String {
    let joined = cmd.join(" ");
    match fifo {
        Some(f) => format!("while true; do {joined} 1>>{f} 2>>{} ; sleep 2; done", log_dir().join("generator.log").display()),
        None => format!("while true; do {joined} ; sleep 2; done"),
    }
}

fn stdout_arg(_target: &str, logfile: &std::path::Path) -> String {
    format!(">> {}", logfile.display())
}

fn stop_one(name: &str) {
    if let Some(pid) = read_pid(name) {
        if is_alive(pid) {
            let _ = Command::new("kill").arg(pid.to_string()).status();
            println!("[eco log] stopped {name}");
        }
        let _ = std::fs::remove_file(pidfile(name));
    }
}

fn wait_for_grafana() {
    for _ in 0..90 {
        let ok = Command::new("curl")
            .args(["-s", "-o", "/dev/null", "--max-time", "2"])
            .arg(format!("http://127.0.0.1:{GRAFANA_PORT}/api/health"))
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            println!("[eco log] Grafana ready ✓  http://127.0.0.1:{GRAFANA_PORT}");
            return;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    println!("[eco log] Grafana not ready yet — check ~/.eco/logging/server.log");
}

fn run_dev(args: &[String]) -> Result<(), String> {
    ensure_dev_log_stack()?;
    if args.iter().any(|a| a == "--demo") {
        let fifo = fifo_path();
        let gen = "while true; do printf '{\"ts\":\"%s\",\"level\":\"%s\",\"msg\":\"%s\",\"service\":\"assessment\",\"request_id\":\"req-%d\",\"status\":%s,\"latency_ms\":%d}\\n' \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\" \"$([ $((RANDOM%6)) -eq 0 ] && echo error || echo info)\" \"$([ $((RANDOM%2)) -eq 0 ] && echo 'assessment request ok' || echo 'db query slow')\" \"$RANDOM\" \"$([ $((RANDOM%5)) -eq 0 ] && echo 500 || echo 200)\" \"$((RANDOM%90))\"; sleep 1; done";
        let gen_cmd = vec!["sh".to_string(), "-c".to_string(), gen.to_string()];
        start_loop("generator", &gen_cmd, Some(&fifo))?;
    }
    println!("[eco log] Grafana: http://127.0.0.1:{GRAFANA_PORT}  (anonymous; Live Logs dashboard)");
    println!("[eco log] Loki:    http://127.0.0.1:{LOKI_PORT}");
    println!(
        "[eco log] FIFO:    {}  — write dev logs here to enter the pipeline",
        fifo_path().display()
    );
    wait_for_grafana();
    Ok(())
}

/// Start the Loki + Grafana server and the FIFO agent (no demo generator).
/// Used by `eco log dev` and automatically by `eco up dev`. Fail-soft: if the
/// `logging` LXS binary is unavailable, returns Ok(()) so dev still boots.
pub fn ensure_dev_log_stack() -> Result<(), String> {
    if util::env_var_or("ECO_LOG", "1") == "0" {
        return Ok(());
    }
    let bin = match resolve_logging_bin() {
        Ok(b) => b,
        Err(_) => {
            eprintln!("[eco log] logging LXS not found — skipping local log stack");
            return Ok(());
        }
    };
    ensure_dir(&log_dir())?;

    let server_cmd = vec![
        "env".to_string(),
        format!("LOG_DATA_DIR={}", log_dir().display()),
        "BIND=127.0.0.1".to_string(),
        bin.clone(),
        "server".to_string(),
    ];
    start_loop("server", &server_cmd, None)?;

    let fifo = fifo_path();
    // Regular append file (not a FIFO): robust with PM2 `out_file` writers and
    // the agent's `tail -f` reader. Create it if missing (do not truncate live data).
    if !fifo.exists() {
        let _ = std::fs::write(&fifo, "");
    }
    let agent_cmd = vec![
        "env".to_string(),
        "MODE=agent".to_string(),
        format!("STREAM={}", util::env_var_or("ECO_LOG_STREAM", "assessment")),
        format!("LOG_SOURCE=tail:{}", fifo.display()),
        format!("LOKI_URL=http://127.0.0.1:{LOKI_PORT}"),
        bin,
        "agent".to_string(),
    ];
    start_loop("agent", &agent_cmd, None)?;
    Ok(())
}

fn run_status() -> Result<(), String> {
    for name in ["server", "agent", "generator"] {
        match read_pid(name) {
            Some(pid) if is_alive(pid) => println!("[eco log] {name}: running (pid {pid})"),
            _ => println!("[eco log] {name}: stopped"),
        }
    }
    Ok(())
}

pub fn run_log(args: &[String]) -> Result<(), String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("dev");
    match sub {
        "dev" => run_dev(&args[1..]),
        "start" => run_dev(&args[1..]),
        "stop" => {
            stop_one("generator");
            stop_one("agent");
            stop_one("server");
            // Also reap the supervised children (Loki / Grafana / any leftover
            // logging LXS processes) so the ports are freed — killing only the
            // restart wrappers leaves orphaned grandchildren holding :3000/:3100.
            for pat in [
                format!("{}/bin/loki", log_dir().display()),
                format!("{}/bin/victoria-logs", log_dir().display()),
                "loki-v3".to_string(),
                "victoria-logs-v".to_string(),
                format!("{}/bin/grafana", log_dir().display()),
            ] {
                let _ = Command::new("pkill")
                    .args(["-9", "-f"])
                    .arg(&pat)
                    .status();
            }
            println!("[eco log] stack stopped");
            Ok(())
        }
        "status" => run_status(),
        _ => Err("usage: eco log <dev [--demo]|stop|status>".to_string()),
    }
}
