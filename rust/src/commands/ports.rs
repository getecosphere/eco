use crate::registry;
use crate::util;

fn ports_help() {
    let text = r#"eco ports

Usage:
  eco ports list [--project X] [--scope S] [--path P]
  eco ports pin <service> <port> [--type service|gateway|index] [--env-var PORT]
  eco ports release <service>
  eco ports reset [--project X]
  eco ports reserved
  eco ports dbs [--project X] [--secret]

Options:
  --scope S   registry scope (default: hostname)
  --path P    registry database path (default: ~/.eco/registry.db or /etc/eco/registry.db)

Ports are assigned once and never change. The registry is the durable
record; .env and ecosystem.config.js are renders of it.
"#;
    print!("{text}");
}

pub fn run_ports(args: &[String]) -> Result<(), String> {
    let (positionals, options) = util::parse_args_flagged(args);
    let (action, rest) = match positionals.first() {
        Some(a) => (a.clone(), &positionals[1..]),
        None => (String::new(), &positionals[0..0]),
    };

    if action.is_empty() || action == "help" || action == "--help" || action == "-h" {
        ports_help();
        return Ok(());
    }

    let path = options
        .get("path")
        .map(|p| std::path::PathBuf::from(p))
        .unwrap_or_else(registry::default_registry_path);
    let scope = options
        .get("scope")
        .cloned()
        .or_else(|| std::env::var("ECO_REGISTRY_SCOPE").ok())
        .unwrap_or_else(registry::default_scope);

    match action.as_str() {
        "list" => {
            let rows = registry::list_ports(&path, &scope, options.get("project").map(|s| s.as_str()))?;
            let reserved = registry::list_reserved(&path, &scope)?;
            let used: std::collections::HashSet<u64> = rows
                .iter()
                .filter_map(|r| r.get("port").and_then(|v| v.as_i64()).map(|v| v as u64))
                .collect();
            let reserved_rows: Vec<_> = reserved
                .iter()
                .filter(|r| {
                    !r.get("port")
                        .and_then(|v| v.as_i64())
                        .map(|v| used.contains(&(v as u64)))
                        .unwrap_or(false)
                })
                .collect();

            let mut out = String::new();
            out.push_str(&format!("\n{}  {}\n", util::bold("Registry"), util::dim(&path.display().to_string())));
            out.push_str(&format!("  {}  {scope}\n\n", util::cyan("scope")));

            if !reserved_rows.is_empty() {
                out.push_str(&format!("{}\n", util::bold("Reserved")));
                for r in &reserved_rows {
                    let port = r.get("port").and_then(|v| v.as_i64()).unwrap_or(0);
                    let label = r.get("label").and_then(|v| v.as_str()).unwrap_or("");
                    out.push_str(&format!("  {}  {label}\n", util::yellow(&format!("{port:<6}"))));
                }
                out.push('\n');
            }

            if rows.is_empty() {
                out.push_str("No port allocations yet.\n");
                print!("{out}");
                return Ok(());
            }

            out.push_str(&format!("{}\n", util::bold("Allocations")));
            let mut sorted = rows.clone();
            sorted.sort_by_key(|r| r.get("port").and_then(|v| v.as_i64()).unwrap_or(0));
            for row in &sorted {
                let port = row.get("port").and_then(|v| v.as_i64()).unwrap_or(0);
                let project = row.get("project").and_then(|v| v.as_str()).unwrap_or("");
                let service = row.get("service").and_then(|v| v.as_str()).unwrap_or("");
                let ty = row.get("type").and_then(|v| v.as_str()).unwrap_or("");
                let env_var = row.get("env_var").and_then(|v| v.as_str()).unwrap_or("");
                let label = if ty == "service" {
                    format!("{project}/{service}")
                } else {
                    format!("{project}/{service} ({ty})")
                };
                out.push_str(&format!(
                    "  {}  {}  {}\n",
                    util::cyan(&format!("{port:<6}")),
                    util::dim(&format!("{label:<32}")),
                    util::dim(env_var)
                ));
            }
            print!("{out}");
            Ok(())
        }
        "reserved" => {
            let rows = registry::list_reserved(&path, &scope)?;
            let mut out = String::new();
            out.push_str(&format!("\n{}  {} (scope: {scope})\n", util::bold("Reserved ports"), util::dim(&path.display().to_string())));
            for r in &rows {
                let port = r.get("port").and_then(|v| v.as_i64()).unwrap_or(0);
                let label = r.get("label").and_then(|v| v.as_str()).unwrap_or("");
                out.push_str(&format!("  {}  {label}\n", util::yellow(&format!("{port:<6}"))));
            }
            print!("{out}");
            Ok(())
        }
        "pin" => {
            let service = rest.first().cloned();
            let port_input = rest.get(1).cloned();
            let (Some(service), Some(port_input)) = (service, port_input) else {
                return Err("Usage: eco ports pin <service> <port> [--type service] [--env-var PORT]".to_string());
            };
            let port: i64 = port_input
                .parse()
                .map_err(|_| format!("Invalid port: {port_input}"))?;
            if port < 1 || port > 65535 {
                return Err(format!("Invalid port: {port_input}"));
            }
            let project = options.get("project").cloned().ok_or("eco ports pin requires --project <name>".to_string())?;
            let ty = options.get("type").cloned().unwrap_or_else(|| "service".to_string());
            let env_var = options.get("env-var").cloned().unwrap_or_else(|| "PORT".to_string());
            registry::pin_port(&path, &scope, &project, &service, &ty, &env_var, port as u32)?;
            util::println_stdout(&format!("Pinned {project}/{service} to port {port} ({}).", util::dim(&path.display().to_string())));
            Ok(())
        }
        "seed" => {
            let service = rest.first().cloned();
            let port_input = rest.get(1).cloned();
            let (Some(service), Some(port_input)) = (service, port_input) else {
                return Err("Usage: eco ports seed <service> <port> [--type service] [--env-var PORT]".to_string());
            };
            let port: i64 = port_input.parse().map_err(|_| format!("Invalid port: {port_input}"))?;
            let project = options.get("project").cloned().ok_or("eco ports seed requires --project <name>".to_string())?;
            let ty = options.get("type").cloned().unwrap_or_else(|| "service".to_string());
            let env_var = options.get("env-var").cloned().unwrap_or_else(|| "PORT".to_string());
            registry::seed_port(&path, &scope, &project, &service, &ty, &env_var, port as u32)?;
            util::println_stdout(&format!("Seeded {project}/{service} to port {port} ({}).", util::dim(&path.display().to_string())));
            Ok(())
        }
        "release" => {
            let service = rest.first().cloned().ok_or("Usage: eco ports release <service> [--project X]".to_string())?;
            let project = options.get("project").cloned().ok_or("eco ports release requires --project <name>".to_string())?;
            registry::release_port(&path, &scope, &project, &service)?;
            util::println_stdout(&format!(
                "Released {project}/{service} ({}). Next eco up will allocate a new one-time port.",
                util::dim(&path.display().to_string())
            ));
            Ok(())
        }
        "reset" => {
            let project = options.get("project").cloned().ok_or("eco ports reset requires --project <name>".to_string())?;
            registry::reset_project(&path, &scope, &project)?;
            util::println_stdout(&format!(
                "Reset all allocations for {project} ({}). Next eco up will reallocate.",
                util::dim(&path.display().to_string())
            ));
            Ok(())
        }
        "dbs" => {
            let rows = registry::list_dbs(
                &path,
                &scope,
                options.get("project").map(|s| s.as_str()),
                matches!(options.get("secret").map(|s| s.as_str()), Some("1") | Some("true")),
            )?;
            let mut out = String::new();
            out.push_str(&format!("\n{}  {} (scope: {scope})\n", util::bold("Databases"), util::dim(&path.display().to_string())));
            if rows.is_empty() {
                out.push_str("No managed databases recorded.\n");
                print!("{out}");
                return Ok(());
            }
            for row in &rows {
                let port = row.get("port").and_then(|v| v.as_i64()).unwrap_or(0);
                let db_type = row.get("db_type").and_then(|v| v.as_str()).unwrap_or("");
                let project = row.get("project").and_then(|v| v.as_str()).unwrap_or("");
                let service = row.get("service").and_then(|v| v.as_str()).unwrap_or("");
                let db_name = row.get("db_name").and_then(|v| v.as_str()).unwrap_or("");
                let username = row.get("username").and_then(|v| v.as_str()).unwrap_or("");
                let password = row.get("password").and_then(|v| v.as_str()).unwrap_or("");
                let db_part = if db_name.is_empty() { String::new() } else { format!(" db={db_name}") };
                let user_part = if username.is_empty() { String::new() } else { format!(" user={username}") };
                let secret_part = if password.is_empty() { String::new() } else { format!(" password={password}") };
                let ident = format!("{project}/{service}");
                out.push_str(&format!(
                    "  {}  {}  {ident}{db_part}{user_part}{secret_part}\n",
                    util::cyan(&format!("{port:<6}")),
                    format!("{db_type:<8}")
                ));
            }
            print!("{out}");
            Ok(())
        }
        other => Err(format!("Unknown action: {other}\n\nRun \"eco ports\" for usage.")),
    }
}
