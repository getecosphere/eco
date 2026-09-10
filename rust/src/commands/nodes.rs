// eco nodes / eco node / eco estates / eco estate — infra & estate inventory
// and live stats from the eco agent (read-only). See `eco telegram` for the
// same data over Telegram.
use crate::commands::account::resolve_api_credentials;
use std::time::Duration;

fn get_json_auth(api_url: &str, api_key: &str, path: &str) -> Result<serde_json::Value, String> {
    let url = format!("{api_url}{path}");
    let response = match ureq::get(&url)
        .set("User-Agent", "eco-cli")
        .set("Authorization", &format!("Bearer {api_key}"))
        .timeout(Duration::from_secs(60))
        .call()
    {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            let text = r.into_string().unwrap_or_default();
            let msg = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(|s| s.to_string()))
                .unwrap_or_else(|| text.chars().take(200).collect());
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
        Err(value
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or(&text)
            .to_string())
    }
}

fn hb(bytes: u64) -> String {
    const K: f64 = 1024.0;
    let b = bytes as f64;
    if b >= K * K * K * K {
        format!("{:.1}T", b / (K * K * K * K))
    } else if b >= K * K * K {
        format!("{:.1}G", b / (K * K * K))
    } else if b >= K * K {
        format!("{:.0}M", b / (K * K))
    } else if b >= K {
        format!("{:.0}K", b / K)
    } else {
        format!("{bytes}B")
    }
}

fn hdur(secs: u64) -> String {
    let d = secs / 86400;
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    if d > 0 {
        format!("{d}d{h}h")
    } else if h > 0 {
        format!("{h}h{m}m")
    } else {
        format!("{m}m")
    }
}

fn s(v: &serde_json::Value, k: &str) -> String {
    v.get(k)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}
fn u(v: &serde_json::Value, k: &str) -> u64 {
    v.get(k).and_then(|x| x.as_u64()).unwrap_or(0)
}
fn f(v: &serde_json::Value, k: &str) -> f64 {
    v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0)
}

fn mem_str(v: &serde_json::Value, used_key: &str, total_key: &str) -> String {
    let used = u(v, used_key);
    let total = u(v, total_key);
    if total == 0 {
        hb(used)
    } else {
        format!("{}/{}", hb(used), hb(total))
    }
}

fn post_json_auth(
    api_url: &str,
    api_key: &str,
    path: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let url = format!("{api_url}{path}");
    let response = match ureq::post(&url)
        .set("User-Agent", "eco-cli")
        .set("Authorization", &format!("Bearer {api_key}"))
        .set("Content-Type", "application/json")
        .timeout(Duration::from_secs(120))
        .send_string(&serde_json::to_string(body).unwrap())
    {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            let text = r.into_string().unwrap_or_default();
            let msg = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(|s| s.to_string()))
                .unwrap_or_else(|| text.chars().take(300).collect());
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
        Err(value
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or(&text)
            .to_string())
    }
}

fn creds() -> Result<(String, String), String> {
    resolve_api_credentials()
}

fn print_host(h: &serde_json::Value, compact: bool) {
    let load = &h["load"];
    let mem = &h["memory"];
    let mem_used = u(mem, "mem_used");
    let mem_total = u(mem, "mem_total");
    let disks = h["disks"].as_array().cloned().unwrap_or_default();
    let root = disks
        .iter()
        .max_by_key(|d| u(d, "size"))
        .cloned()
        .unwrap_or(serde_json::json!({}));
    let disk = if root.is_null() {
        String::new()
    } else {
        format!(
            "  disk {}/{} ({})",
            hb(u(&root, "used")),
            hb(u(&root, "size")),
            s(&root, "pct")
        )
    };
    if compact {
        println!(
            "HOST {:<14} cpu {:>5.1}%  load {}/{}/{}  mem {}/{} ({:.0}%)  up {}{}",
            s(h, "name"),
            f(h, "cpu_pct"),
            s(load, "1"),
            s(load, "5"),
            s(load, "15"),
            hb(mem_used),
            hb(mem_total),
            f(h, "mem_pct"),
            hdur(u(h, "uptime_secs")),
            disk,
        );
    } else {
        println!("HOST {}", s(h, "name"));
        println!("  kernel: {}   pve: {}", s(h, "kernel"), s(h, "pve_version"));
        println!("  cpu:    {} cores  {}  ({:.1}%)", u(h, "cores"), s(h, "cpu_model"), f(h, "cpu_pct"));
        println!("  load:   {} / {} / {}", s(load, "1"), s(load, "5"), s(load, "15"));
        println!(
            "  mem:    {} used / {} total ({:.0}%)  ({} avail, {} cache)",
            hb(u(mem, "mem_used")),
            hb(u(mem, "mem_total")),
            f(h, "mem_pct"),
            hb(u(mem, "mem_available")),
            hb(u(mem, "mem_cache"))
        );
        println!(
            "  swap:   {} used / {} total",
            hb(u(mem, "swap_used")),
            hb(u(mem, "swap_total"))
        );
        println!("  uptime: {}", hdur(u(h, "uptime_secs")));
        for d in &disks {
            println!(
                "  disk {}  {} / {} ({})",
                s(d, "mount"),
                hb(u(d, "used")),
                hb(u(d, "size")),
                s(d, "pct")
            );
        }
        let net = &h["net"];
        println!(
            "  net:    rx {}  tx {}",
            hb(u(net, "rx_bytes")),
            hb(u(net, "tx_bytes"))
        );
        for p in h["top_cpu"].as_array().cloned().unwrap_or_default() {
            println!(
                "  top cpu: pid {} {}  {}%  mem {}%",
                s(&p, "pid"),
                s(&p, "comm"),
                s(&p, "cpu_pct"),
                s(&p, "mem_pct")
            );
        }
    }
}

fn print_ct(c: &serde_json::Value, compact: bool) {
    let load = &c["load"];
    if compact {
        if s(c, "status") != "running" {
            println!(
                "CT{:<4} {:<14} stopped  cores {}  mem {}MB",
                s(c, "id"),
                s(c, "name"),
                u(c, "cores"),
                u(c, "memory_mb")
            );
            return;
        }
        println!(
            "CT{:<4} {:<14} cpu {:>5.1}%  load {}/{}/{}  mem {}/{} ({:.0}%)  up {}  svc {} (fail {})  ports {}",
            s(c, "id"),
            s(c, "name"),
            f(c, "cpu_pct"),
            s(load, "1"),
            s(load, "5"),
            s(load, "15"),
            hb(u(c, "mem_current")),
            if u(c, "mem_max") > 0 { hb(u(c, "mem_max")) } else { format!("{}MB", u(c, "memory_mb")) },
            f(c, "mem_pct"),
            hdur(u(c, "uptime_secs")),
            u(c, "services_running"),
            u(c, "services_failed"),
            u(c, "ports_listening"),
        );
    } else {
        println!("CT{} {}", s(c, "id"), s(c, "name"));
        println!("  status: {}  onboot {}  ostype {}  unprivileged {}", s(c, "status"), s(c, "onboot"), s(c, "ostype"), s(c, "unprivileged"));
        println!("  alloc:  {} cores  {}MB mem  {}MB swap", u(c, "cores"), u(c, "memory_mb"), u(c, "swap_mb"));
        println!("  rootfs: {}", s(c, "rootfs"));
        println!("  net:    {}", s(c, "net0"));
        if s(c, "status") == "running" {
            println!("  cpu:    {:.1}%   load {} / {} / {}", f(c, "cpu_pct"), s(load, "1"), s(load, "5"), s(load, "15"));
            println!("  mem:    {}/{} ({:.0}%)   procs {}   uptime {}", hb(u(c, "mem_current")), if u(c, "mem_max")>0 {hb(u(c,"mem_max"))} else {format!("{}MB",u(c,"memory_mb"))}, f(c, "mem_pct"), u(c, "procs"), hdur(u(c, "uptime_secs")));
            println!("  svc:    {} running, {} failed, {} restarts", u(c, "services_running"), u(c, "services_failed"), u(c, "service_restarts"));
            let d = &c["disk_inside"];
            println!("  disk:   {} used / {} ({})", hb(u(d, "used")), hb(u(d, "size")), s(d, "pct"));
            let n = &c["net"];
            println!("  net:    rx {}  tx {}", hb(u(n, "rx_bytes")), hb(u(n, "tx_bytes")));
            if let Some(units) = c["services"]["units"].as_array() {
                let names: Vec<String> = units.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect();
                println!("  units:  {}", names.join(", "));
            }
            if let Some(failed) = c["services"]["failed"].as_array() {
                if !failed.is_empty() {
                    let names: Vec<String> = failed.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect();
                    println!("  FAILED: {}", names.join(", "));
                }
            }
            for p in c["top_cpu"].as_array().cloned().unwrap_or_default() {
                println!("  top cpu: pid {} {}  {}%  mem {}%", s(&p, "pid"), s(&p, "comm"), s(&p, "cpu_pct"), s(&p, "mem_pct"));
            }
        }
    }
}

pub fn run_nodes(args: &[String]) -> Result<(), String> {
    let (api_url, api_key) = creds()?;
    let v = get_json_auth(&api_url, &api_key, "/v1/nodes")?;
    if args.iter().any(|a| a == "--json") {
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        return Ok(());
    }
    let t = &v["totals"];
    println!(
        "Eco nodes — {} total ({} running, {} stopped)",
        u(t, "nodes"),
        u(t, "running"),
        u(t, "stopped")
    );
    print_host(&v["host"], true);
    for c in v["cts"].as_array().cloned().unwrap_or_default() {
        print_ct(&c, true);
    }
    let stopped = v["stopped_cts"].as_array().cloned().unwrap_or_default();
    if !stopped.is_empty() {
        println!("— stopped —");
        for c in &stopped {
            print_ct(c, true);
        }
    }
    Ok(())
}

pub fn run_node(args: &[String]) -> Result<(), String> {
    let pos: Vec<String> = args
        .iter()
        .filter(|a| !a.starts_with("--"))
        .cloned()
        .collect();
    let id = pos
        .first()
        .cloned()
        .ok_or_else(|| "usage: eco node <id|name|host> [start|stop|restart|service <name> <action>] [--yes]".to_string())?;
    let (api_url, api_key) = creds()?;

    // Actions (Phase 3): eco node <id> restart --yes
    if pos.len() >= 2 {
        let sub = pos[1].as_str();
        let (action, service): (String, Option<String>) = if sub == "service" {
            let svc = pos
                .get(2)
                .cloned()
                .ok_or("usage: eco node <id> service <name> <start|stop|restart> --yes")?;
            let act = pos
                .get(3)
                .cloned()
                .ok_or("usage: eco node <id> service <name> <start|stop|restart> --yes")?;
            (act, Some(svc))
        } else {
            (sub.to_string(), None)
        };
        if !matches!(action.as_str(), "start" | "stop" | "restart" | "destroy") {
            return Err("usage: eco node <id> [start|stop|restart|destroy] | service <name> <action> --yes".into());
        }
        if !args.iter().any(|a| a == "--yes") {
            return Err(format!(
                "refusing to {action} {id} without confirmation — re-run with --yes"
            ));
        }
        let body = serde_json::json!({ "action": action, "service": service });
        let v = post_json_auth(&api_url, &api_key, &format!("/v1/nodes/{id}/action"), &body)?;
        println!("{}", s(&v, "result"));
        return Ok(());
    }

    let v = get_json_auth(&api_url, &api_key, &format!("/v1/nodes/{id}"))?;
    if args.iter().any(|a| a == "--json") {
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        return Ok(());
    }
    if s(&v, "type") == "host" {
        print_host(&v, false);
    } else {
        print_ct(&v, false);
    }
    Ok(())
}

pub fn run_estates(args: &[String]) -> Result<(), String> {
    let (api_url, api_key) = creds()?;
    let v = get_json_auth(&api_url, &api_key, "/v1/estates/list")?;
    if args.iter().any(|a| a == "--json") {
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        return Ok(());
    }
    let estates = v["estates"].as_array().cloned().unwrap_or_default();
    println!("Eco estates — {}", u(&v, "count"));
    for e in &estates {
        let lxs: Vec<String> = e["lxs"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|x| s(x, "lxs"))
            .collect();
        println!(
            "{:<14} {:<28} CT{}  release {:<12} svc {}/{}  {}",
            s(e, "project"),
            s(e, "hostname"),
            s(e, "ct"),
            s(e, "current_release"),
            u(e, "services_running"),
            u(e, "services_total"),
            lxs.join(" "),
        );
    }
    Ok(())
}

pub fn run_estate(args: &[String]) -> Result<(), String> {
    let pos: Vec<String> = args
        .iter()
        .filter(|a| !a.starts_with("--"))
        .cloned()
        .collect();
    let name = pos
        .first()
        .cloned()
        .ok_or_else(|| "usage: eco estate <name> [restart|stop|start --yes] [--json]".to_string())?;
    let (api_url, api_key) = creds()?;

    if pos.len() >= 2 {
        let action = pos[1].clone();
        if !matches!(action.as_str(), "start" | "stop" | "restart") {
            return Err("usage: eco estate <name> [restart|stop|start --yes]".into());
        }
        if !args.iter().any(|a| a == "--yes") {
            return Err(format!(
                "refusing to {action} estate {name} without confirmation — re-run with --yes"
            ));
        }
        let body = serde_json::json!({ "action": action });
        let v = post_json_auth(&api_url, &api_key, &format!("/v1/estates/{name}/action"), &body)?;
        println!("{}", s(&v, "result"));
        return Ok(());
    }

    let v = get_json_auth(&api_url, &api_key, &format!("/v1/estates/{name}"))?;
    if args.iter().any(|a| a == "--json") {
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        return Ok(());
    }
    println!("Estate {}", s(&v, "project"));
    println!("  hostname: {}   CT {}", s(&v, "hostname"), s(&v, "ct"));
    println!("  release:  {} ({} releases kept)", s(&v, "current_release"), u(&v, "releases"));
    println!("  http:     {}", s(&v, "http_status"));
    println!("  services: {}/{} running", u(&v, "services_running"), u(&v, "services_total"));
    if let Some(svcs) = v["service_details"].as_array() {
        for svc in svcs {
            let kind = if !s(svc, "lxs").is_empty() {
                format!("lxs {}", s(svc, "lxs"))
            } else {
                format!("source {}", s(svc, "path"))
            };
            println!("    {:<24} {:<10} {}", s(svc, "service"), s(svc, "state"), kind);
        }
    }
    Ok(())
}
