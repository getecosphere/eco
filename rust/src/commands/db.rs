use crate::ecompose;
use crate::util;
use std::path::Path;

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = hex_nibble(bytes[i + 1]);
            let lo = hex_nibble(bytes[i + 2]);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn mongo_database_name(uri: &str) -> Result<String, String> {
    let without_query = uri.split('?').next().unwrap_or(uri).split('#').next().unwrap_or(uri);
    let database = without_query
        .rsplit('/')
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if database.is_empty() || database.contains('/') {
        return Err("MONGODB_URI must include an explicit database name to clear it safely.".to_string());
    }
    Ok(percent_decode(&database))
}

fn postgres_database_name(uri: &str) -> Result<String, String> {
    let parsed = url::Url::parse(uri)
        .map_err(|_| "DATABASE_URL must be a valid PostgreSQL connection URL to clear it safely.".to_string())?;
    if parsed.scheme() != "postgres" && parsed.scheme() != "postgresql" {
        return Err("DATABASE_URL must use postgres:// or postgresql:// to clear it safely.".to_string());
    }
    let database = parsed.path().trim_start_matches('/').to_string();
    let database = percent_decode(&database);
    if database.is_empty() || database.contains('/') {
        return Err("DATABASE_URL must include exactly one explicit database name to clear it safely.".to_string());
    }
    Ok(database)
}

fn declared_engine(service: &ecompose::Service) -> String {
    if service.runtimes.iter().any(|r| r.starts_with("mongodb@")) {
        return "mongo".to_string();
    }
    if service.runtimes.iter().any(|r| r.starts_with("postgresql@")) {
        return "postgres".to_string();
    }
    String::new()
}

struct DbTarget {
    service: ecompose::Service,
    engine: String,
    uri: String,
    database: String,
    error: Option<String>,
}

fn database_target(service: &ecompose::Service, estate_root: &Path, project: &str) -> Option<DbTarget> {
    let engine = declared_engine(service);
    if engine.is_empty() {
        return None;
    }

    let env_path = estate_root.join(&service.path).join(".env");
    let env_content = std::fs::read_to_string(&env_path).unwrap_or_default();
    if env_content.is_empty() && engine != "mongo" {
        return Some(DbTarget {
            service: service.clone(),
            engine,
            uri: String::new(),
            database: String::new(),
            error: Some(format!("No .env file found at {}", env_path.display())),
        });
    }

    if engine == "mongo" {
        let configured_uri = util::read_env_value_opt(&env_content, "MONGODB_URI")
            .or_else(|| util::read_env_value_opt(&env_content, "MONGO_URI"));
        let uri = match configured_uri {
            Some(u) => u,
            None => {
                let db_name = format!("{}_{project}", service.name.replace('-', "_"));
                format!("mongodb://localhost:27017/{db_name}")
            }
        };
        if uri.is_empty() {
            return Some(DbTarget {
                service: service.clone(),
                engine,
                uri: String::new(),
                database: String::new(),
                error: Some(format!("MONGODB_URI is not configured in {}", env_path.display())),
            });
        }
        return match mongo_database_name(&uri) {
            Ok(database) => Some(DbTarget {
                service: service.clone(),
                engine,
                uri,
                database,
                error: None,
            }),
            Err(e) => Some(DbTarget {
                service: service.clone(),
                engine,
                uri: String::new(),
                database: String::new(),
                error: Some(e),
            }),
        };
    }

    let uri = util::read_env_value_opt(&env_content, "DATABASE_URL")
        .or_else(|| util::read_env_value_opt(&env_content, "DB_URL"))
        .unwrap_or_default();
    if uri.is_empty() {
        return Some(DbTarget {
            service: service.clone(),
            engine,
            uri: String::new(),
            database: String::new(),
            error: Some(format!("DATABASE_URL is not configured in {}", env_path.display())),
        });
    }
    match postgres_database_name(&uri) {
        Ok(database) => Some(DbTarget {
            service: service.clone(),
            engine,
            uri,
            database,
            error: None,
        }),
        Err(e) => Some(DbTarget {
            service: service.clone(),
            engine,
            uri: String::new(),
            database: String::new(),
            error: Some(e),
        }),
    }
}

fn database_targets(deployment: &ecompose::EcomposeRead) -> Vec<DbTarget> {
    let estate_root = deployment
        .file_path
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| util::current_dir());
    let project = ecompose::parse_project_name(&deployment.content);
    ecompose::parse_services(&deployment.content)
        .iter()
        .filter_map(|service| database_target(service, &estate_root, &project))
        .collect()
}

fn command_on_path(command: &str) -> bool {
    util::command_on_path(command)
}

fn database_execution_context(deployment: &ecompose::EcomposeRead) -> Option<String> {
    let ct = ecompose::parse_ct_metadata(&deployment.content);
    if let Some(id) = ct.get("id") {
        if !id.is_empty() && command_on_path("pct") {
            return Some(id.clone());
        }
    }
    None
}

fn run_database_command(command: &str, args: &[String], failure_message: &str) -> Result<(), String> {
    let cwd = util::current_dir();
    let result = util::run_capture(command, args, &cwd)?;
    if result.code != 0 {
        let detail = if !result.stderr.trim().is_empty() {
            result.stderr.trim()
        } else {
            result.stdout.trim()
        };
        Err(format!("{failure_message}: {detail}"))
    } else {
        Ok(())
    }
}

fn run_scoped_database_command(context: Option<&str>, command: &str, args: &[String], failure_message: &str) -> Result<(), String> {
    match context {
        Some(ctid) => {
            let mut scoped = vec!["exec".to_string(), ctid.to_string(), "--".to_string(), command.to_string()];
            scoped.extend_from_slice(args);
            run_database_command("pct", &scoped, failure_message)
        }
        None => run_database_command(command, args, failure_message),
    }
}

fn postgres_client(context: Option<&str>) -> Result<String, String> {
    if context.is_some() {
        run_scoped_database_command(context, "psql", &["--version".to_string()], "psql could not start inside the application CT")?;
        return Ok("psql".to_string());
    }
    if let Ok(r) = util::run_capture("which", &["psql".to_string()], &util::current_dir()) {
        if r.code == 0 && !r.stdout.trim().is_empty() {
            return Ok(r.stdout.trim().to_string());
        }
    }
    for candidate in [
        "/Applications/Postgres.app/Contents/Versions/15/bin/psql",
        "/Applications/Postgres.app/Contents/Versions/latest/bin/psql",
    ] {
        if std::path::Path::new(candidate).exists() {
            return Ok(candidate.to_string());
        }
    }
    Err("PostgreSQL is declared in ecompose.yml but psql was not found. Run `eco provision` first.".to_string())
}

fn mongo_shell(context: Option<&str>) -> Result<String, String> {
    let msg = match context {
        Some(_) => "mongosh could not start inside the application CT",
        None => "mongosh could not start",
    };
    run_scoped_database_command(context, "mongosh", &["--version".to_string()], msg)?;
    Ok("mongosh".to_string())
}

fn clear_target(target: &DbTarget, context: Option<&str>) -> Result<(), String> {
    if target.engine == "mongo" {
        let mongosh = mongo_shell(context)?;
        run_scoped_database_command(
            context,
            &mongosh,
            &[target.uri.clone(), "--quiet".to_string(), "--eval".to_string(), "db.dropDatabase()".to_string()],
            "mongosh failed to drop the database",
        )?;
        util::println_stdout(&format!(
            "Dropped MongoDB database \"{}\" for {}{}.",
            target.database,
            target.service.name,
            context.map(|c| format!(" via CT {c}")).unwrap_or_default()
        ));
        return Ok(());
    }
    let psql = postgres_client(context)?;
    run_scoped_database_command(
        context,
        &psql,
        &[
            target.uri.clone(),
            "-v".to_string(),
            "ON_ERROR_STOP=1".to_string(),
            "-c".to_string(),
            "DROP SCHEMA public CASCADE; CREATE SCHEMA public;".to_string(),
        ],
        "psql failed to reset the public schema",
    )?;
    util::println_stdout(&format!(
        "Cleared PostgreSQL public schema in database \"{}\" for {}{}.",
        target.database,
        target.service.name,
        context.map(|c| format!(" via CT {c}")).unwrap_or_default()
    ));
    Ok(())
}

fn db_help() {
    let text = r#"eco db

Usage:
  eco db
  eco db clear all
  eco db clear <service>

Examples:
  eco db
  eco db clear auth-backend
  eco db clear backend
  eco db clear all

The command detects MongoDB or PostgreSQL from ecompose.yml. PostgreSQL clears
the public schema so the next eco up can rerun migrations. On a Proxmox host,
clear commands run through the estate's ct.id with pct exec; they never use the
host's localhost database. It never edits .env files.
"#;
    print!("{text}");
}

pub fn run_db(args: &[String]) -> Result<(), String> {
    let action = args.first().cloned();
    let target_name = args.get(1).cloned();
    let extra: Vec<String> = args.iter().skip(2).cloned().collect();

    if matches!(action.as_deref(), Some("help") | Some("--help") | Some("-h")) {
        db_help();
        return Ok(());
    }

    let cwd = util::current_dir();
    let deployment = ecompose::read_ecompose(".", &cwd)?;
    let targets = database_targets(&deployment);
    let context = database_execution_context(&deployment);

    match action.as_deref() {
        None => {
            if targets.is_empty() {
                util::println_stdout("No database-backed services are declared in this estate.");
                return Ok(());
            }
            let mut out = String::new();
            out.push_str("Clearable databases:\n");
            if let Some(ctid) = &context {
                out.push_str(&format!("Database commands will run inside application CT {ctid}.\n"));
            }
            for target in &targets {
                let ty = if target.engine == "mongo" { "MongoDB" } else { "PostgreSQL" };
                match &target.error {
                    Some(err) => {
                        out.push_str(&format!("- {} ({}): unavailable — {err}\n", target.service.name, ty));
                    }
                    None => {
                        out.push_str(&format!("- {} ({}): {}\n", target.service.name, ty, target.database));
                    }
                }
            }
            out.push_str("\nUse: eco db clear <service>  or  eco db clear all\n");
            print!("{out}");
            Ok(())
        }
        Some(action) => {
            if action != "clear" || target_name.is_none() || !extra.is_empty() {
                return Err("Usage: eco db clear <service|all>".to_string());
            }
            let target_name = target_name.unwrap();
            if target_name == "all" {
                let clearable: Vec<&DbTarget> = targets.iter().filter(|t| t.error.is_none()).collect();
                if clearable.is_empty() {
                    return Err("No configured databases are available to clear. Run eco up first.".to_string());
                }
                if clearable.iter().any(|t| t.engine == "mongo") {
                    mongo_shell(context.as_deref())?;
                }
                if clearable.iter().any(|t| t.engine == "postgres") {
                    postgres_client(context.as_deref())?;
                }
                let project = ecompose::parse_project_name(&deployment.content);
                let mut out = String::new();
                out.push_str(&format!(
                    "\nWARNING: This permanently clears every listed database in the {project} estate:\n"
                ));
                for target in &clearable {
                    let ty = if target.engine == "mongo" { "MongoDB" } else { "PostgreSQL" };
                    out.push_str(&format!("- {}: {ty} {}\n", target.service.name, target.database));
                }
                out.push_str("PostgreSQL migration history will be removed and recreated on the next eco up.\n");
                print!("{out}");
                let confirmation = crate::checklist::prompt_line(&format!("Type {project} to clear every database above: "))?;
                if confirmation.trim() != project {
                    util::println_stdout("Cancelled. No database was changed.");
                    return Ok(());
                }
                for target in clearable {
                    clear_target(target, context.as_deref())?;
                }
                Ok(())
            } else {
                let target = targets.iter().find(|t| t.service.name == target_name).ok_or(format!(
                    "Service \"{target_name}\" has no declared MongoDB or PostgreSQL database in {}. Run eco db to list available services.",
                    deployment.file_path.display()
                ))?;
                if let Some(err) = &target.error {
                    return Err(format!("{} cannot be cleared: {err}", target.service.name));
                }
                if target.engine == "mongo" {
                    mongo_shell(context.as_deref())?;
                }
                if target.engine == "postgres" {
                    postgres_client(context.as_deref())?;
                }
                let database_type = if target.engine == "mongo" { "MongoDB database" } else { "PostgreSQL public schema" };
                let mut out = String::new();
                out.push_str(&format!("\nWARNING: This permanently clears the {database_type} \"{}\".\n", target.database));
                out.push_str(&format!("Service: {}\n", target.service.name));
                if target.engine == "postgres" {
                    out.push_str("The next eco up will rerun its migrations.\n");
                }
                print!("{out}");
                let confirmation = crate::checklist::prompt_line(&format!("Type {} to confirm: ", target.database))?;
                if confirmation.trim() != target.database {
                    util::println_stdout("Cancelled. No database was changed.");
                    return Ok(());
                }
                clear_target(target, context.as_deref())
            }
        }
    }
}
