use crate::ecompose;
use crate::util;
use std::path::Path;

fn declared_engine(service: &ecompose::Service) -> String {
    if service.runtimes.iter().any(|r| r.starts_with("mongodb@")) {
        return "mongo".to_string();
    }
    if service.runtimes.iter().any(|r| r == "postgresql@15") {
        return "postgres".to_string();
    }
    String::new()
}

fn mongo_database_name(uri: &str) -> String {
    let without_query = uri.split('?').next().unwrap_or(uri).split('#').next().unwrap_or(uri);
    let database = without_query.rsplit('/').next().unwrap_or("").trim().to_string();
    // minimal percent-decode for common encodings
    database
        .replace("%20", " ")
        .replace("%40", "@")
        .replace("%3A", ":")
        .replace("%2F", "/")
}

fn postgres_database_name(uri: &str) -> String {
    let stripped = String::from(uri)
        .strip_prefix("jdbc:")
        .unwrap_or(uri)
        .split('?')
        .next()
        .unwrap_or("")
        .split('#')
        .next()
        .unwrap_or("")
        .to_string();
    let path_part = stripped.rsplit('/').next().unwrap_or("").to_string();
    path_part
}

struct PgConnection {
    database: String,
    username: String,
    password: String,
    url: String,
}

fn postgres_connection(env_contents: &str) -> PgConnection {
    let url = util::read_env_value_opt(env_contents, "DATABASE_URL")
        .or_else(|| util::read_env_value_opt(env_contents, "DB_URL"))
        .unwrap_or_default();
    let database = postgres_database_name(&url);
    let username = util::read_env_value_opt(env_contents, "DATABASE_USERNAME")
        .unwrap_or_else(|| "postgres".to_string());
    let password = util::read_env_value(env_contents, "DATABASE_PASSWORD");
    PgConnection { database, username, password, url }
}

struct SyncTarget {
    service: ecompose::Service,
    engine: String,
    database: String,
    uri: String,
    username: String,
    password: String,
}

fn database_targets(
    deployment: &ecompose::EcomposeRead,
    remote_env_resolver: Option<&dyn Fn(&str) -> Option<String>>,
    use_remote_for_mongo: bool,
) -> Vec<SyncTarget> {
    let estate_root = deployment
        .file_path
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| util::current_dir());
    let project = ecompose::parse_project_name(&deployment.content);
    let services = ecompose::parse_services(&deployment.content);
    let mut targets = Vec::new();

    for service in services {
        let engine = declared_engine(&service);
        if engine.is_empty() {
            continue;
        }
        let remote_env = if let Some(resolver) = remote_env_resolver {
            if engine != "mongo" || use_remote_for_mongo {
                resolver(&service.path).unwrap_or_default()
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        if engine == "mongo" {
            let mut configured_uri = String::new();
            if !remote_env.is_empty() {
                configured_uri = util::read_env_value_opt(&remote_env, "MONGODB_URI")
                    .or_else(|| util::read_env_value_opt(&remote_env, "MONGO_URI"))
                    .unwrap_or_default();
            }
            if configured_uri.is_empty() {
                let env_example = std::fs::read_to_string(estate_root.join(&service.path).join(".env.example")).unwrap_or_default();
                configured_uri = util::read_env_value_opt(&env_example, "MONGODB_URI")
                    .or_else(|| util::read_env_value_opt(&env_example, "MONGO_URI"))
                    .unwrap_or_default();
            }
            let db_name = format!("{}_{project}", service.name.replace('-', "_"));
            let uri = if configured_uri.is_empty() {
                format!("mongodb://localhost:27017/{db_name}")
            } else {
                configured_uri
            };
            let database = mongo_database_name(&uri);
            targets.push(SyncTarget {
                service,
                engine,
                database,
                uri,
                username: String::new(),
                password: String::new(),
            });
            continue;
        }

        let conn = if !remote_env.is_empty() {
            postgres_connection(&remote_env)
        } else {
            let env_example = std::fs::read_to_string(estate_root.join(&service.path).join(".env.example")).unwrap_or_default();
            let mut conn = postgres_connection(&env_example);
            if conn.database.is_empty() {
                conn.database = format!("{}_{project}", service.name.replace('-', "_"));
            }
            conn
        };
        let mut conn = conn;
        if conn.database.is_empty() {
            conn.database = format!("{}_{project}", service.name.replace('-', "_"));
        }
        targets.push(SyncTarget {
            service,
            engine,
            database: conn.database,
            uri: conn.url,
            username: conn.username,
            password: conn.password,
        });
    }
    targets
}

fn relative_service_dir(service_path: &str, project: &str) -> String {
    let mut segments: Vec<String> = service_path.split('/').filter(|s| !s.is_empty()).map(|s| s.to_string()).collect();
    if !segments.is_empty() && segments[0] == project {
        segments.remove(0);
    }
    segments.join("/")
}

fn libpq_bin_dir(command: &str) -> String {
    for dir in ["/usr/local/opt/libpq/bin", "/opt/homebrew/opt/libpq/bin"] {
        if std::path::Path::new(&format!("{dir}/{command}")).exists() {
            return dir.to_string();
        }
    }
    String::new()
}

fn sync_help() {
    let text = r#"eco sync

Usage:
  eco sync [options]

Sync production database data from the estate's application CT to the local
development machine. Reads ecompose.yml to discover every MongoDB- and
PostgreSQL-backed service, then for each one:

  MongoDB:
    ssh <host> "pct exec <ctid> -- mongodump --db=<database> --archive" | \
      mongorestore --archive --drop

  PostgreSQL:
    ssh <host> "pct exec <ctid> -- pg_dump ... -d <database>" | \
      pg_restore --clean --if-exists

With --staging the data is synced prod-CT -> staging-CT instead (both on the
remote host, streaming CT-to-CT through the host container runtime, so no
local restore tool is needed). The destination is the staging.ct declared in
ecompose.yml.

Options:
  --host <hostname>   SSH host for the remote host (default: prox)
  --ct <ctid>         CT ID to sync from (reads ecompose.yml ct.id by default)
  --staging           Sync prod CT -> staging CT (staging.ct from ecompose.yml)
  --service <name>    Sync only this service (default: all DB-backed services)
  --skip-ssh-check    Skip the SSH reachability pre-flight check
  --dry-run           Print commands without executing them

Examples:
  eco sync
  eco sync --staging
  eco sync --host prox-eko --service marketplace-backend
  eco sync --service assessment-backend   # a PostgreSQL-backed service
  eco sync --dry-run
"#;
    print!("{text}");
}

pub fn run_sync_staging(args: &[String]) -> Result<(), String> {
    let mut full = vec!["--staging".to_string()];
    full.extend_from_slice(args);
    run_sync(&full)
}

pub fn run_sync(args: &[String]) -> Result<(), String> {
    if matches!(args.first().map(|s| s.as_str()), Some("help") | Some("--help") | Some("-h")) {
        sync_help();
        return Ok(());
    }

    let mut options: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut positionals: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if !arg.starts_with("--") {
            positionals.push(arg.clone());
            i += 1;
            continue;
        }
        let key = arg[2..].to_string();
        if key == "dry-run" || key == "skip-ssh-check" || key == "staging" {
            options.insert(key, "true".to_string());
            i += 1;
            continue;
        }
        let value = args.get(i + 1).cloned().ok_or_else(|| format!("Missing value for option --{key}"))?;
        if value.starts_with("--") {
            return Err(format!("Missing value for option --{key}"));
        }
        options.insert(key, value);
        i += 2;
    }
    if !positionals.is_empty() {
        return Err(format!("Unexpected positional argument(s): {}", positionals.join(" ")));
    }

    let deployment = ecompose::read_ecompose(".", &util::current_dir())?;
    let ct_meta = ecompose::parse_ct_metadata(&deployment.content);
    let ctid = options
        .get("ct")
        .cloned()
        .or_else(|| ct_meta.get("id").cloned())
        .unwrap_or_else(|| "101".to_string());
    let ssh_host = options.get("host").cloned().unwrap_or_else(|| "prox".to_string());
    let dry_run = options.get("dry-run").map(|v| v == "true").unwrap_or(false);
    let skip_ssh_check = options.get("skip-ssh-check").map(|v| v == "true").unwrap_or(false);
    let to_staging = options.get("staging").map(|v| v == "true").unwrap_or(false);
    let project = ecompose::parse_project_name(&deployment.content);
    let ct_project_root = format!("/opt/projects/{project}");

    let remote_env_resolver = |service_path: &str| -> Option<String> {
        let rel = relative_service_dir(service_path, &project);
        let env_path = format!("{ct_project_root}/{rel}/.env");
        let test = util::run_capture(
            "ssh",
            &[ssh_host.clone(), format!("pct exec {ctid} -- test -f {env_path}")],
            &util::current_dir(),
        )
        .ok()?;
        if test.code != 0 {
            return None;
        }
        let cat = util::run_capture(
            "ssh",
            &[ssh_host.clone(), format!("pct exec {ctid} -- cat {env_path}")],
            &util::current_dir(),
        )
        .ok()?;
        if cat.code == 0 {
            Some(cat.stdout)
        } else {
            None
        }
    };

    let mut targets = database_targets(&deployment, Some(&remote_env_resolver), to_staging);
    if let Some(service_filter) = options.get("service") {
        targets.retain(|t| t.service.name == *service_filter);
        if targets.is_empty() {
            return Err(format!(
                "Service \"{service_filter}\" has no database in this estate. Run eco db to list available services."
            ));
        }
    }
    if targets.is_empty() {
        util::println_stdout("No DB-backed services are declared in this estate.");
        return Ok(());
    }

    let mut staging_ctid = String::new();
    if to_staging {
        let staging = ecompose::parse_staging(&deployment.content);
        staging_ctid = staging.get("ct").cloned().unwrap_or_default();
        if staging_ctid.is_empty() {
            return Err("--staging requested but ecompose.yml has no staging.ct declared. Add a staging: block (staging.ct: 1000).".to_string());
        }
        if staging_ctid == ctid {
            return Err(format!("--staging destination CT {staging_ctid} must differ from the prod ct.id."));
        }
    }

    if to_staging {
        if !util::command_on_path("ssh") {
            return Err("ssh is not available locally.".to_string());
        }
    } else if !dry_run {
        let needs_mongo_restore = targets.iter().any(|t| t.engine == "mongo");
        let needs_pg_restore = targets.iter().any(|t| t.engine == "postgres");
        if needs_mongo_restore && !util::command_on_path("mongorestore") {
            return Err("mongorestore is not installed locally. Install MongoDB Database Tools (brew install mongodb-database-tools).".to_string());
        }
        if needs_pg_restore && !util::command_on_path("pg_restore") && libpq_bin_dir("pg_restore").is_empty() {
            return Err("pg_restore is not installed locally. Install PostgreSQL client tools (brew install libpq).".to_string());
        }
    }

    if !skip_ssh_check {
        util::print_stdout(&format!("Checking SSH to {ssh_host}… "));
        let result = util::run_capture("ssh", &[ssh_host.clone(), "echo".to_string(), "ok".to_string()], &util::current_dir());
        match result {
            Ok(r) if r.code == 0 => util::println_stdout("ok"),
            _ => return Err(format!("Cannot reach {ssh_host} via SSH. Check the hostname or use --skip-ssh-check to skip.")),
        }
    }

    if !dry_run {
        let mut dump_tools = std::collections::BTreeSet::new();
        for t in &targets {
            dump_tools.insert(if t.engine == "mongo" { "mongodump" } else { "pg_dump" }.to_string());
        }
        for tool in &dump_tools {
            let check = util::run_capture(
                "ssh",
                &[ssh_host.clone(), format!("pct exec {ctid} -- which {tool}")],
                &util::current_dir(),
            );
            if check.map(|r| r.code == 0).unwrap_or(false) {
                continue;
            }
            let pkg = if tool == "mongodump" { "mongodb-database-tools" } else { "postgresql-client" };
            return Err(format!(
                "{tool} is not installed in CT {ctid}. Run \"apt-get install -y {pkg}\" in the CT first."
            ));
        }
        if to_staging {
            let mut restore_tools = std::collections::BTreeSet::new();
            for t in &targets {
                restore_tools.insert(if t.engine == "mongo" { "mongorestore" } else { "pg_restore" }.to_string());
            }
            for tool in &restore_tools {
                let check = util::run_capture(
                    "ssh",
                    &[ssh_host.clone(), format!("pct exec {staging_ctid} -- which {tool}")],
                    &util::current_dir(),
                );
                if check.map(|r| r.code == 0).unwrap_or(false) {
                    continue;
                }
                let pkg = if tool == "mongorestore" { "mongodb-database-tools" } else { "postgresql-client" };
                return Err(format!(
                    "{tool} is not installed in staging CT {staging_ctid}. Run \"apt-get install -y {pkg}\" in the CT first."
                ));
            }
        }
    }

    let mongo_count = targets.iter().filter(|t| t.engine == "mongo").count();
    let pg_count = targets.iter().filter(|t| t.engine == "postgres").count();
    let kind_label = {
        let mut parts = vec![format!("{mongo_count} MongoDB")];
        if pg_count > 0 {
            parts.push(format!("{pg_count} PostgreSQL"));
        }
        parts.join(" + ")
    };
    if to_staging {
        util::println_stdout(&format!(
            "\nSyncing {kind_label} database(s) from \"{project}\" CT {ctid} to staging CT {staging_ctid} ({ssh_host}):\n"
        ));
    } else {
        util::println_stdout(&format!(
            "\nSyncing {kind_label} database(s) from \"{project}\" CT {ctid} ({ssh_host}):\n"
        ));
    }

    let mut failed = 0;
    for target in &targets {
        let full_pipeline = build_pipeline(target, &ssh_host, &ctid, to_staging, &staging_ctid);
        util::print_stdout(&format!(
            "  {} ({}) [{}]",
            target.service.name,
            target.database,
            target.engine
        ));

        if dry_run {
            let redacted = if target.password.is_empty() {
                full_pipeline
            } else {
                full_pipeline.replace(&target.password, "********")
            };
            util::println_stdout(&format!("\n    {redacted}"));
            continue;
        }

        let result = run_pipeline(&full_pipeline);
        match result {
            Ok(()) => util::println_stdout(" ✓"),
            Err(e) => {
                util::println_stdout(&format!(" ✗ ({e})"));
                failed += 1;
            }
        }
    }

    util::println_stdout(&format!(
        "\nDone. {} of {} databases synced.",
        targets.len() - failed,
        targets.len()
    ));
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn shell_quote(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('`', "\\`")
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| format!("\"{}\"", value))
}

fn build_pipeline(target: &SyncTarget, ssh_host: &str, ctid: &str, to_staging: bool, staging_ctid: &str) -> String {
    if target.engine == "mongo" {
        if to_staging {
            format!(
                "ssh {ssh_host} \"pct exec {ctid} -- mongodump --db={} --archive | pct exec {staging_ctid} -- mongorestore --archive --drop\"",
                shell_quote(&target.database)
            )
        } else {
            format!(
                "ssh {ssh_host} \"pct exec {ctid} -- mongodump --db={} --archive\" | mongorestore --archive --drop",
                shell_quote(&target.database)
            )
        }
    } else {
        let dump_cmd = format!(
            "PGPASSWORD=\"{}\" pg_dump -h 127.0.0.1 -U {} -d {} --format=custom --no-owner",
            shell_quote(&target.password),
            shell_quote(&target.username),
            shell_quote(&target.database)
        );
        if to_staging {
            format!(
                "ssh {ssh_host} \"pct exec {ctid} -- bash -lc {} | pct exec {staging_ctid} -- bash -lc {}\"",
                json_string(&dump_cmd),
                json_string(&format!(
                    "PGPASSWORD=\"{}\" pg_restore -h 127.0.0.1 -U {} -d {} --clean --if-exists --no-owner --no-acl",
                    shell_quote(&target.password),
                    shell_quote(&target.username),
                    shell_quote(&target.database)
                ))
            )
        } else {
            format!(
                "ssh {ssh_host} \"pct exec {ctid} -- bash -lc {}\" | PGPASSWORD=\"\" pg_restore --clean --if-exists --no-owner --no-acl -d {}",
                json_string(&dump_cmd),
                shell_quote(&target.database)
            )
        }
    }
}

fn run_pipeline(pipeline: &str) -> Result<(), String> {
    let cwd = util::current_dir();
    let status = std::process::Command::new("bash")
        .arg("-lc")
        .arg(pipeline)
        .current_dir(&cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|e| format!("pipeline error: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("exited with code {}", status.code().unwrap_or(-1)))
    }
}
