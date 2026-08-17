use crate::registry;
use crate::util;
use std::path::PathBuf;

fn parse_args(argv: &[String]) -> std::collections::HashMap<String, String> {
    let mut args = std::collections::HashMap::new();
    let mut i = 0;
    while i < argv.len() {
        let token = &argv[i];
        if let Some(rest) = token.strip_prefix("--") {
            let key = rest.to_string();
            let next = argv.get(i + 1).cloned();
            if let Some(n) = next {
                if !n.starts_with("--") {
                    args.insert(key, n);
                    i += 1;
                } else {
                    args.insert(key, "true".to_string());
                }
            } else {
                args.insert(key, "true".to_string());
            }
        }
        i += 1;
    }
    args
}

pub fn run_registry_cli(argv: &[String]) -> Result<(), String> {
    let (op, rest) = match argv.first() {
        Some(o) => (o.clone(), &argv[1..]),
        None => (String::new(), &argv[0..0]),
    };
    let args = parse_args(rest);
    let path: PathBuf = args
        .get("path")
        .map(|p| PathBuf::from(p))
        .unwrap_or_else(registry::default_registry_path);
    let scope = args
        .get("scope")
        .cloned()
        .unwrap_or_else(registry::default_scope);

    match op.as_str() {
        "get-or-allocate" => {
            let project = args.get("project").cloned().ok_or("missing --project")?;
            let service = args.get("service").cloned().ok_or("missing --service")?;
            let ty = args.get("type").cloned().unwrap_or_else(|| "service".to_string());
            let env_var = args.get("env-var").cloned().unwrap_or_else(|| "PORT".to_string());
            let preferred = args.get("preferred").cloned();
            let result = registry::get_or_allocate_port(&path, &scope, &project, &service, &ty, &env_var, preferred.as_deref())?;
            util::print_stdout(&result.port.to_string());
            Ok(())
        }
        "pin" => {
            let project = args.get("project").cloned().ok_or("missing --project")?;
            let service = args.get("service").cloned().ok_or("missing --service")?;
            let ty = args.get("type").cloned().unwrap_or_else(|| "service".to_string());
            let env_var = args.get("env-var").cloned().unwrap_or_else(|| "PORT".to_string());
            let port = args.get("port").and_then(|p| p.parse::<u32>().ok()).ok_or("missing --port")?;
            let result = registry::pin_port(&path, &scope, &project, &service, &ty, &env_var, port)?;
            util::print_stdout(&result.port.to_string());
            Ok(())
        }
        "seed" => {
            let project = args.get("project").cloned().ok_or("missing --project")?;
            let service = args.get("service").cloned().ok_or("missing --service")?;
            let ty = args.get("type").cloned().unwrap_or_else(|| "service".to_string());
            let env_var = args.get("env-var").cloned().unwrap_or_else(|| "PORT".to_string());
            let port = args.get("port").and_then(|p| p.parse::<u32>().ok()).ok_or("missing --port")?;
            let result = registry::seed_port(&path, &scope, &project, &service, &ty, &env_var, port)?;
            util::print_stdout(&result.port.to_string());
            Ok(())
        }
        "lookup" => {
            let project = args.get("project").cloned().ok_or("missing --project")?;
            let service = args.get("service").cloned().ok_or("missing --service")?;
            let ty = args.get("type").cloned().unwrap_or_else(|| "service".to_string());
            if let Some(port) = registry::lookup_port(&path, &scope, &project, &service, &ty)? {
                util::print_stdout(&port.to_string());
            }
            Ok(())
        }
        "has-project" => {
            let project = args.get("project").cloned().ok_or("missing --project")?;
            let present = registry::project_has_registry_rows(&path, &scope, &project)?;
            util::print_stdout(if present { "1" } else { "0" });
            Ok(())
        }
        "release" => {
            let project = args.get("project").cloned().ok_or("missing --project")?;
            let service = args.get("service").cloned().ok_or("missing --service")?;
            registry::release_port(&path, &scope, &project, &service)
        }
        "reset" => {
            let project = args.get("project").cloned().ok_or("missing --project")?;
            registry::reset_project(&path, &scope, &project)
        }
        "rename-project" => {
            let from = args.get("from").cloned().ok_or("missing --from")?;
            let to = args.get("to").cloned().ok_or("missing --to")?;
            registry::rename_project(&path, &scope, &from, &to)
        }
        "list" => {
            let rows = registry::list_ports(&path, &scope, args.get("project").map(|s| s.as_str()))?;
            util::println_stdout(&serde_json::to_string_pretty(&rows).unwrap_or_else(|_| "[]".to_string()));
            Ok(())
        }
        "reserved" => {
            let rows = registry::list_reserved(&path, &scope)?;
            util::println_stdout(&serde_json::to_string_pretty(&rows).unwrap_or_else(|_| "[]".to_string()));
            Ok(())
        }
        "record-db" => {
            let project = args.get("project").cloned().ok_or("missing --project")?;
            let service = args.get("service").cloned().ok_or("missing --service")?;
            let db_type = args.get("db-type").cloned().ok_or("missing --db-type")?;
            let port = args.get("port").and_then(|p| p.parse::<u32>().ok()).ok_or("missing --port")?;
            let db_name = args.get("db-name").cloned();
            let username = args.get("username").cloned();
            let password = args.get("password").cloned();
            registry::record_db(&path, &scope, &project, &service, &db_type, port, db_name.as_deref(), username.as_deref(), password.as_deref())
        }
        "list-dbs" => {
            let with_secret = matches!(args.get("secret").map(|s| s.as_str()), Some("1") | Some("true"));
            let rows = registry::list_dbs(&path, &scope, args.get("project").map(|s| s.as_str()), with_secret)?;
            util::println_stdout(&serde_json::to_string_pretty(&rows).unwrap_or_else(|_| "[]".to_string()));
            Ok(())
        }
        other => Err(format!(
            "Unknown registry op: {other}\nAvailable: get-or-allocate, seed, lookup, has-project, pin, release, reset, rename-project, list, reserved, record-db, list-dbs"
        )),
    }
}
