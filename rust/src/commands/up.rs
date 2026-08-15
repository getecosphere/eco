use crate::cloudflare;
use crate::ecompose;
use crate::embedded;
use crate::repos;
use crate::util;
use sha2::Digest;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const FALLBACK_CT_TEMPLATE: &str = "local:vztmpl/debian-12-standard_12.12-1_amd64.tar.zst";

fn parse_options(args: &[String]) -> (HashMap<String, String>, Vec<String>) {
    let mut options = HashMap::new();
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
        if key == "dry-run" || key == "staging" || key == "prod-only" {
            options.insert(key, "true".to_string());
            i += 1;
            continue;
        }
        if let Some(value) = args.get(i + 1) {
            if !value.starts_with("--") {
                options.insert(key, value.clone());
                i += 2;
                continue;
            }
        }
        options.insert(key, "true".to_string());
        i += 1;
    }
    (options, positionals)
}

fn run_command_env(command: &str, args: &[String], cwd: &Path, extra: &[(String, String)]) -> Result<(), String> {
    let mut env_map: HashMap<String, String> = std::env::vars().collect();
    for (k, v) in extra {
        env_map.insert(k.clone(), v.clone());
    }
    util::run_command_env(command, args, cwd, &env_map)
}

fn run_command(command: &str, args: &[String], cwd: &Path) -> Result<(), String> {
    util::run_command(command, args, cwd)
}

fn run_capture(command: &str, args: &[String], cwd: &Path) -> Result<util::Captured, String> {
    util::run_capture(command, args, cwd)
}

fn build_net0(ct: &HashMap<String, String>) -> String {
    let mut parts = vec![
        "name=eth0".to_string(),
        format!("bridge={}", ct.get("bridge").cloned().unwrap_or_default()),
        format!("ip={}", ct.get("ip").cloned().unwrap_or_else(|| "dhcp".to_string())),
    ];
    if let Some(gateway) = ct.get("gateway") {
        parts.push(format!("gw={gateway}"));
    }
    parts.join(",")
}

fn domain_branch_overrides_from_ecompose(content: &str) -> HashMap<String, String> {
    let mut overrides = HashMap::new();
    let mut in_domains = false;
    let mut block_domain = String::new();

    for raw in content.split('\n') {
        let line = raw.trim_end_matches('\r').trim_end();
        if line.is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if line == "domains:" {
            in_domains = true;
            continue;
        }
        if in_domains && line.starts_with(|c: char| !c.is_whitespace()) {
            in_domains = false;
            continue;
        }
        if !in_domains {
            continue;
        }
        if let Some(rest) = line.trim_start().strip_prefix("- ") {
            let raw_value = util::strip_quotes(rest.trim());
            let colon = raw_value.find(':');
            match colon {
                Some(idx) => {
                    let name = raw_value[..idx].trim().to_string();
                    let after = raw_value[idx + 1..].trim().to_string();
                    if after.is_empty() {
                        block_domain = name;
                    } else {
                        block_domain = name.clone();
                        overrides.insert(name, util::strip_quotes(&after));
                    }
                }
                None => {
                    block_domain = raw_value;
                }
            }
            continue;
        }
        if !block_domain.is_empty() {
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_prefix("branch:") {
                overrides.insert(block_domain.clone(), util::strip_quotes(rest.trim()));
            }
        }
    }
    overrides
}

fn domain_dev_flags_from_ecompose(content: &str) -> HashMap<String, String> {
    let mut flags = HashMap::new();
    let mut in_domains = false;
    let mut block_domain = String::new();

    for raw in content.split('\n') {
        let line = raw.trim_end_matches('\r').trim_end();
        if line.is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if line == "domains:" {
            in_domains = true;
            continue;
        }
        if in_domains && line.starts_with(|c: char| !c.is_whitespace()) {
            in_domains = false;
            continue;
        }
        if !in_domains {
            continue;
        }
        if let Some(rest) = line.trim_start().strip_prefix("- ") {
            let raw_value = util::strip_quotes(rest.trim());
            let colon = raw_value.find(':');
            match colon {
                Some(idx) => {
                    let name = raw_value[..idx].trim().to_string();
                    let after = raw_value[idx + 1..].trim().to_string();
                    if after.is_empty() {
                        block_domain = name;
                    } else {
                        block_domain.clear();
                    }
                }
                None => {
                    block_domain = raw_value;
                }
            }
            continue;
        }
        if !block_domain.is_empty() {
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_prefix("dev:") {
                let value = util::strip_quotes(rest.trim());
                if value == "optional" || value == "disabled" {
                    flags.insert(block_domain.clone(), value);
                }
            }
        }
    }
    flags
}

fn domain_runtimes_from_services(domain: &str, services: &[ecompose::Service]) -> Vec<String> {
    let mut runtimes = Vec::new();
    for service in services {
        let first = service.path.split('/').next().unwrap_or("");
        if first == domain {
            for token in &service.runtimes {
                if !runtimes.contains(token) {
                    runtimes.push(token.clone());
                }
            }
        }
    }
    runtimes
}

fn repo_name_from_git_url(url: &str) -> String {
    let trimmed = url.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or("").trim_end_matches(".git").to_string()
}

fn stat_exists(path: &Path) -> bool {
    path.exists()
}

fn runtime_satisfiable(token: &str) -> bool {
    if token != "onnxruntime" && token != "onnxruntime@1.28" {
        return true;
    }
    if util::platform() == "darwin" {
        let result = run_capture("brew", &["list".to_string(), "onnxruntime".to_string()], &util::current_dir());
        return result.map(|r| r.code == 0).unwrap_or(false);
    }
    Path::new("/opt/eco-tools/libonnxruntime.so").exists()
}

fn sql_database_name_for_service(service: &ecompose::Service, project: &str) -> String {
    if service.name == format!("{project}-backend") {
        project.to_string()
    } else {
        format!("{}_{project}", service.name.replace('-', "_"))
    }
}

fn uses_java_database_configuration(service: &ecompose::Service) -> bool {
    service.runtimes.iter().any(|r| r == "java@17") || service.runtimes.iter().any(|r| r == "maven")
}

fn shell_single_quote(value: &str) -> String {
    util::shell_single_quote(value)
}

fn env_upsert_command(file_path: &str, key: &str, value: &str) -> String {
    let quoted_file = shell_single_quote(file_path);
    let quoted_key = shell_single_quote(key);
    let quoted_value = shell_single_quote(value);
    format!(
        "touch {quoted_file}\nif grep -qE \"^{key}=\" {quoted_file}; then\n  sed -i \"s|^{key}=.*|{key}={value}|\" {quoted_file}\nelse\n  printf \"%s\\n\" \"{key}={value}\" >> {quoted_file}\nfi"
    )
}

fn env_set_if_missing_command(file_path: &str, key: &str, value: &str) -> String {
    let quoted_file = shell_single_quote(file_path);
    format!(
        "touch {quoted_file}\nif ! grep -qE \"^{key}=\" {quoted_file}; then\n  printf \"%s\\n\" \"{key}={value}\" >> {quoted_file}\nfi"
    )
}

fn build_npm_install_command(service_dir: &str) -> String {
    [
        format!("cd \"{service_dir}\""),
        "if [ -f package-lock.json ]; then".to_string(),
        "  if ! npm ci; then".to_string(),
        "    echo '[eco up] npm ci failed, retrying with npm install --legacy-peer-deps' >&2".to_string(),
        "    npm install --legacy-peer-deps".to_string(),
        "  fi".to_string(),
        "elif [ -f package.json ]; then".to_string(),
        "  if ! npm install; then".to_string(),
        "    echo '[eco up] npm install failed, retrying with --legacy-peer-deps' >&2".to_string(),
        "    npm install --legacy-peer-deps".to_string(),
        "  fi".to_string(),
        "fi".to_string(),
    ]
    .join("\n")
}


fn relative_ct_service_path(service_path: &str, project: &str, project_dir: &str, estate_core: &str) -> String {
    let mut segments: Vec<String> = service_path.split('/').filter(|s| !s.is_empty()).map(|s| s.to_string()).collect();
    let project_base = Path::new(project_dir)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    // The first path segment may be the project name, the project dir name, or
    // the estate core repo name (e.g. `stuff8_core/frontend` where the estate
    // core repo is `stuff8_core` but the project is `stuff8`). All three are
    // flattened to the CT project root, so strip whichever matched.
    if !segments.is_empty() && (segments[0] == project || segments[0] == project_base || (!estate_core.is_empty() && segments[0] == estate_core)) {
        segments.remove(0);
    }
    segments.join("/")
}

// The estate core repo name, derived from the ecompose composition.git (e.g.
// `stuff8_core` for git@github.com:kelastanpatembok/stuff8_core.git). On the
// host the staged source is flattened to /opt/projects/<project>, so service
// paths that begin with the estate core name must strip it.
fn estate_core_name(content: &str) -> String {
    let composition = ecompose::parse_composition(content);
    composition
        .get("git")
        .map(|g| repo_name_from_git_url(g))
        .unwrap_or_default()
}

fn resolve_ct_service_dir(service: &ecompose::Service, ct_project_root: &str, project_dir: &str, estate_core: &str) -> String {
    let relative_path = relative_ct_service_path(&service.path, &Path::new(ct_project_root).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(), project_dir, estate_core);
    if relative_path.is_empty() {
        return ct_project_root.to_string();
    }
    format!("{ct_project_root}/{relative_path}")
}

fn is_peer_dependency_resolution_error(result: &util::Captured) -> bool {
    let text = format!("{}\n{}", result.stdout, result.stderr);
    text.contains("ERESOLVE")
        || text.to_lowercase().contains("peer dependency")
        || text.to_lowercase().contains("legacy-peer-deps")
}

fn derive_staging_hostname(app_hostname: &str) -> String {
    if app_hostname.is_empty() {
        return String::new();
    }
    let labels: Vec<&str> = app_hostname.trim_matches('.').split('.').collect();
    if labels.len() <= 2 {
        return format!("staging.{app_hostname}");
    }
    let (head, rest) = labels.split_first().unwrap();
    format!("staging-{head}.{}", rest.join("."))
}

fn resolve_domain_git(domain: &str, project: &str, content: &str) -> Result<Option<(String, String)>, String> {
    if let Ok(Some(repo)) = repos::find_repo_by_name(domain) {
        if !repo.git.is_empty() {
            let branch = if repo.branch.is_empty() { "main".to_string() } else { repo.branch.clone() };
            return Ok(Some((repo.git, branch)));
        }
    }
    if domain == format!("{project}_composition") {
        let composition = ecompose::parse_composition(content);
        if let Some(git) = composition.get("git") {
            if !git.is_empty() {
                let branch = composition.get("branch").cloned().unwrap_or_else(|| "main".to_string());
                return Ok(Some((git.clone(), branch)));
            }
        }
    }
    // Estate-core model: the estate repo (named freely, e.g. {project}_core) is
    // the repo that owns ecompose.yml, self-declared via composition.git. Match
    // it by repo name so it resolves even when the manifest is a bare copy on a
    // host (no .git, project_dir is not the estate repo).
    let composition = ecompose::parse_composition(content);
    if let Some(git) = composition.get("git") {
        if !git.is_empty() && domain == repo_name_from_git_url(git) {
            let branch = composition.get("branch").cloned().unwrap_or_else(|| "main".to_string());
            return Ok(Some((git.clone(), branch)));
        }
    }
    Ok(None)
}

 fn pct_exec(ctid: &str, command: &str) -> Result<(), String> {
     run_command(
         "pct",
         &["exec".to_string(), ctid.to_string(), "--".to_string(), "bash".to_string(), "-lc".to_string(), command.to_string()],
         &util::current_dir(),
     )
 }

pub fn pct_exec_capture(ctid: &str, command: &str) -> Result<String, String> {
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

fn cargo_package_name(cargo_toml: &str) -> Option<String> {
    let mut in_package = false;
    for raw in cargo_toml.split('\n') {
        let line = raw.trim_end_matches('\r');
        if line == "[package]" {
            in_package = true;
            continue;
        }
        if in_package && line.starts_with('[') {
            break;
        }
        if in_package {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("name") {
                if let Some(eq) = rest.find('=') {
                    let val = rest[eq + 1..].trim().trim_matches('"').trim_matches('\'').to_string();
                    if !val.is_empty() {
                        return Some(val);
                    }
                }
            }
        }
    }
    None
}

fn build_data_bootstrap_plan(services: &[ecompose::Service], _ct_workspace_root: &str, ct_project_root: &str, project_dir: &str, project: &str, estate_core: &str) -> Vec<String> {
    let mut commands = Vec::new();
    let has_mongo = services.iter().any(|s| s.runtimes.iter().any(|r| r == "mongodb@7"));
    let sql_services: Vec<&ecompose::Service> = services.iter().filter(|s| s.runtimes.iter().any(|r| r == "postgresql@15")).collect();

    if has_mongo {
        commands.push(
            "if command -v systemctl >/dev/null 2>&1; then\n  systemctl enable mongod >/dev/null 2>&1 || true;\n  systemctl restart mongod;\nelif command -v service >/dev/null 2>&1; then\n  service mongod restart || true;\nfi"
                .to_string(),
        );
    }
    if !sql_services.is_empty() {
        commands.push(
            "if command -v systemctl >/dev/null 2>&1; then\n  systemctl enable postgresql >/dev/null 2>&1 || true;\n  systemctl restart postgresql;\nelif command -v service >/dev/null 2>&1; then\n  service postgresql restart || true;\nfi"
                .to_string(),
        );
    }

    for service in &sql_services {
        let db_name = sql_database_name_for_service(service, project);
        let db_role = format!("{project}_user");
        let env_file = format!("{}/.env", resolve_ct_service_dir(service, ct_project_root, project_dir, estate_core));
        let quoted_env_file = shell_single_quote(&env_file);
        let is_java = uses_java_database_configuration(service);
        let mut commands_for_service = vec![
            format!("touch {quoted_env_file}"),
            "if [[ -z \"${DATABASE_PASSWORD:-}\" ]]; then".to_string(),
            format!("  db_password=\"$(grep -E '^DATABASE_PASSWORD=' {quoted_env_file} 2>/dev/null | cut -d'=' -f2- | tr -d '\\r' || true)\";"),
            "  if [[ -z \"$db_password\" ]]; then".to_string(),
            "    echo 'ERROR: DATABASE_PASSWORD is not exported in the shell environment and no value is persisted in the service .env. Add it to ~/.bashrc on the CT.' >&2;".to_string(),
            "    exit 1;".to_string(),
            "  fi".to_string(),
            format!("  echo \"[eco up] DATABASE_PASSWORD not in shell env -- reusing the value already persisted in {env_file} (no rotation)\";"),
            "else".to_string(),
            "  db_password=\"${DATABASE_PASSWORD}\";".to_string(),
            "fi".to_string(),
            format!("sed -i '/^DATABASE_PASSWORD=/d' {quoted_env_file};"),
            format!("printf 'DATABASE_PASSWORD=%s\\n' \"$db_password\" >> {quoted_env_file};"),
            format!("runuser -u postgres -- psql -v ON_ERROR_STOP=1 -c \"DO \\$\\$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{db_role}') THEN CREATE ROLE {db_role} WITH LOGIN; END IF; END \\$\\$;\""),
            format!("runuser -u postgres -- psql -v ON_ERROR_STOP=1 -c \"ALTER ROLE {db_role} WITH LOGIN PASSWORD '$db_password';\""),
            format!("PGPASSWORD=\"$db_password\" psql -h 127.0.0.1 -U {db_role} -d postgres -v ON_ERROR_STOP=1 -c 'SELECT 1' >/dev/null"),
            format!("runuser -u postgres -- psql -tAc \"SELECT 1 FROM pg_database WHERE datname = '{db_name}'\" | grep -q 1 || runuser -u postgres -- createdb -O {db_role} {}", shell_single_quote(&db_name)),
            format!("runuser -u postgres -- psql -v ON_ERROR_STOP=1 -d {} -c \"GRANT ALL PRIVILEGES ON DATABASE {db_name} TO {db_role};\"", shell_single_quote(&db_name)),
            format!("runuser -u postgres -- psql -v ON_ERROR_STOP=1 -d {} -c \"GRANT ALL ON SCHEMA public TO {db_role};\"", shell_single_quote(&db_name)),
            format!("runuser -u postgres -- psql -v ON_ERROR_STOP=1 -d {} -c \"GRANT ALL PRIVILEGES ON ALL TABLES IN SCHEMA public TO {db_role};\"", shell_single_quote(&db_name)),
            format!("runuser -u postgres -- psql -v ON_ERROR_STOP=1 -d {} -c \"GRANT ALL PRIVILEGES ON ALL SEQUENCES IN SCHEMA public TO {db_role};\"", shell_single_quote(&db_name)),
            format!("runuser -u postgres -- psql -v ON_ERROR_STOP=1 -d {} -c \"ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT ALL ON TABLES TO {db_role};\"", shell_single_quote(&db_name)),
            format!("runuser -u postgres -- psql -v ON_ERROR_STOP=1 -d {} -c \"ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT ALL ON SEQUENCES TO {db_role};\"", shell_single_quote(&db_name)),
            format!("sed -i '/^DATABASE_USERNAME=/d' {quoted_env_file};"),
            format!("printf 'DATABASE_USERNAME=%s\\n' \"{db_role}\" >> {quoted_env_file};"),
        ];
        if is_java {
            commands_for_service.push(env_set_if_missing_command(&env_file, "DATABASE_URL", &format!("jdbc:postgresql://localhost:5432/{db_name}")));
        } else {
            commands_for_service.push(format!(
                "sed -i '/^DATABASE_URL=/d' {quoted_env_file}\nprintf 'DATABASE_URL=postgresql://{db_role}:%s@127.0.0.1:5432/{db_name}\\n' \"$db_password\" >> {quoted_env_file}"
            ));
        }
        commands.push(commands_for_service.join("\n"));
    }

    // DEEPSEEK_API_KEY passthrough
    let deepseek = util::env_var_or("DEEPSEEK_API_KEY", "");
    if !deepseek.is_empty() {
        for service in services {
            let key = "DEEPSEEK_API_KEY";
            let env_file = format!("{}/.env", resolve_ct_service_dir(service, ct_project_root, project_dir, estate_core));
            let quoted_env_file = shell_single_quote(&env_file);
            let quoted_example = shell_single_quote(&format!("{env_file}.example"));
            commands.push(format!(
                "if [ -f {quoted_example} ] && grep -qE \"^{key}=\" {quoted_example}; then\n  touch {quoted_env_file}\n  sed -i '/^{key}=/d' {quoted_env_file}\n  printf '{key}=%s\\n' {} >> {quoted_env_file}\nfi",
                shell_single_quote(&deepseek)
            ));
        }
    }

    commands
}

fn build_rust_migration_plan(services: &[ecompose::Service], _ct_workspace_root: &str, ct_project_root: &str, project_dir: &str, estate_core: &str) -> Vec<String> {
    services
        .iter()
        .filter(|s| s.runtimes.iter().any(|r| r == "rust") && s.runtimes.iter().any(|r| r == "postgresql@15"))
        .map(|service| {
            let service_dir = resolve_ct_service_dir(service, ct_project_root, project_dir, estate_core);
            format!(
                "if [ -d {} ]; then\n  cd {}\n  find migrations -maxdepth 1 -name '._*' -delete 2>/dev/null || true\n  set -a; . ./.env; set +a\n  sqlx_bin=\"${{ECO_SQLX_BIN:-}}\"\n  if [[ -z \"$sqlx_bin\" ]]; then sqlx_bin=\"$(command -v sqlx 2>/dev/null || true)\"; fi\n  if [[ -z \"$sqlx_bin\" ]]; then\n    if command -v cargo >/dev/null 2>&1; then\n      cargo install sqlx-cli --no-default-features --features postgres,rustls\n      sqlx_bin=\"$(command -v sqlx)\"\n    else\n      echo \"[eco up] sqlx is unavailable and this CT has no Cargo — assuming the database is already migrated (re-deploy of a live estate); skipping.\" >&2\n      exit 0\n    fi\n  fi\n  \"$sqlx_bin\" migrate run --source migrations\nfi",
                shell_single_quote(&format!("{service_dir}/migrations")),
                shell_single_quote(&service_dir)
            )
        })
        .collect()
}


fn create_pct_args(project: &str, ct: &HashMap<String, String>, options: &HashMap<String, String>) -> Vec<String> {
    let mut merged = ct.clone();
    for (k, v) in options {
        merged.insert(k.clone(), v.clone());
    }
    vec![
        "create".to_string(),
        merged.get("id").cloned().unwrap_or_default(),
        merged.get("template").cloned().unwrap_or_default(),
        "--hostname".to_string(),
        merged.get("hostname").cloned().unwrap_or_else(|| project.to_string()),
        "--cores".to_string(),
        merged.get("cores").cloned().unwrap_or_else(|| "2".to_string()),
        "--memory".to_string(),
        merged.get("memory").cloned().unwrap_or_else(|| "4096".to_string()),
        "--swap".to_string(),
        merged.get("swap").cloned().unwrap_or_else(|| "1024".to_string()),
        "--rootfs".to_string(),
        format!("{}:{}", merged.get("storage").cloned().unwrap_or_default(), merged.get("disk").cloned().unwrap_or_default()),
        "--net0".to_string(),
        build_net0(&merged),
        "--unprivileged".to_string(),
        merged.get("unprivileged").cloned().unwrap_or_else(|| "1".to_string()),
    ]
}

fn resolve_available_template(requested_template: &str) -> Result<String, String> {
    // <storage>:vztmpl/<archive>
    let Some((storage, archive)) = requested_template.split_once(":vztmpl/") else {
        return Ok(requested_template.to_string());
    };
    let list_result = run_capture("pvesm", &["list".to_string(), storage.to_string(), "--content".to_string(), "vztmpl".to_string()], &util::current_dir())?;
    if list_result.code == 0 && list_result.stdout.contains(archive) {
        return Ok(requested_template.to_string());
    }
    if requested_template == FALLBACK_CT_TEMPLATE {
        return Ok(requested_template.to_string());
    }
    util::eprintln_stderr(&format!(
        "\n[eco up] WARNING: template \"{requested_template}\" not found on this Proxmox host's storage -- falling back to {FALLBACK_CT_TEMPLATE} for this CT. Build the custom template with 'eco ct template <source-ctid> --name <name> --clone-id <id>' to speed up provisioning here.\n"
    ));
    Ok(FALLBACK_CT_TEMPLATE.to_string())
}

fn print_step(message: &str) {
    util::println_stdout(&format!("\n[eco up] {message}"));
}

pub struct ProjectDeployment {
    pub file_path: String,
    pub content: String,
    pub project_dir: PathBuf,
    pub project: String,
    pub ct: HashMap<String, String>,
    pub expose: ecompose::Expose,
    pub services: Vec<ecompose::Service>,
    pub storage: HashMap<String, HashMap<String, String>>,
    pub ctid: String,
    pub ct_workspace_root: String,
    pub ct_project_root: String,
    pub ct_project_parent: String,
    pub ct_eco_root: String,
    pub pm2_config_filename: String,
    pub ct_config_path: String,
}

pub fn load_project_deployment(input: &str, start_dir: &Path) -> Result<ProjectDeployment, String> {
    let deployment = ecompose::read_ecompose(input, start_dir)?;
    let project_dir = deployment
        .file_path
        .parent()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| "cannot resolve project dir".to_string())?;
    let project = {
        let name = ecompose::parse_project_name(&deployment.content);
        if name.is_empty() {
            project_dir
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default()
        } else {
            name
        }
    };
    let mut ct = ecompose::parse_ct_metadata(&deployment.content);
    // Customer model: a user estate doesn't declare its own CT — the platform
    // ships everything to the shared app CT (101). Fill platform defaults when
    // the ct: block is absent so ecompose.yml stays customer-focused.
    if ct.get("id").map(|s| s.is_empty()).unwrap_or(true) {
        ct.insert("id".to_string(), "101".to_string());
    }
    if ct.get("template").map(|s| s.is_empty()).unwrap_or(true) {
        ct.insert("template".to_string(), FALLBACK_CT_TEMPLATE.to_string());
    }
    for (k, v) in [("storage", "local-lvm"), ("disk", "16"), ("bridge", "vmbr0"), ("ip", "dhcp"), ("cores", "1"), ("memory", "1024"), ("swap", "512"), ("unprivileged", "1")] {
        if ct.get(k).map(|s| s.is_empty()).unwrap_or(true) {
            ct.insert(k.to_string(), v.to_string());
        }
    }
    let expose = ecompose::parse_expose(&deployment.content);
    let services = ecompose::parse_services(&deployment.content);
    let storage = ecompose::parse_storage(&deployment.content);

    if ct.get("id").map(|s| s.is_empty()).unwrap_or(true)
        || ct.get("template").map(|s| s.is_empty()).unwrap_or(true)
        || ct.get("storage").map(|s| s.is_empty()).unwrap_or(true)
        || ct.get("disk").map(|s| s.is_empty()).unwrap_or(true)
        || ct.get("bridge").map(|s| s.is_empty()).unwrap_or(true)
    {
        return Err(format!("Missing required ct metadata in {}", deployment.file_path.display()));
    }

    let ctid = ct.get("id").cloned().unwrap_or_default();
    let ct_workspace_root = "/opt/projects".to_string();
    let ct_project_root = format!("{ct_workspace_root}/{project}");
    let ct_project_parent = Path::new(&ct_project_root)
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| ct_project_root.clone());
    let ct_eco_root = format!("{ct_workspace_root}/eco");
    let is_esm = project_is_esm(&project_dir);
    let pm2_config_filename = if is_esm { "ecosystem.config.cjs".to_string() } else { "ecosystem.config.js".to_string() };
    let ct_config_path = format!("{ct_project_root}/{pm2_config_filename}");

    Ok(ProjectDeployment {
        file_path: deployment.file_path.display().to_string(),
        content: deployment.content,
        project_dir,
        project,
        ct,
        expose,
        services,
        storage,
        ctid,
        ct_workspace_root,
        ct_project_root,
        ct_project_parent,
        ct_eco_root,
        pm2_config_filename,
        ct_config_path,
    })
}

fn project_is_esm(project_dir: &Path) -> bool {
    let raw = std::fs::read_to_string(project_dir.join("package.json")).unwrap_or_default();
    serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(|s| s == "module"))
        .unwrap_or(false)
}

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
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }
        let ctid = parts[0];
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
        return Err("Missing CT identifier.".to_string());
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

fn push_text_file_to_ct(ctid: &str, target_path: &str, content: &str, label: &str) -> Result<(), String> {
    let temp_dir = std::env::temp_dir().join(format!("eco-up-file-{}-{}", label, std::process::id()));
    let _ = std::fs::create_dir_all(&temp_dir);
    let source_path = temp_dir.join(format!("{label}.tmp"));
    std::fs::write(&source_path, content).map_err(|e| e.to_string())?;
    let result = run_command(
        "pct",
        &["push".to_string(), ctid.to_string(), source_path.display().to_string(), target_path.to_string()],
        &util::current_dir(),
    );
    let _ = std::fs::remove_dir_all(&temp_dir);
    result
}

fn escape_single_quotes(value: &str) -> String {
    value.replace('\'', "'\\''")
}

fn minio_ct_reference(storage: &HashMap<String, HashMap<String, String>>) -> Result<String, String> {
    let ref_val = storage
        .get("minio")
        .and_then(|m| m.get("ct"))
        .cloned()
        .unwrap_or_default();
    if ref_val.is_empty() {
        return Err(
            "storage.minio requires `ct: <MinIO CT hostname or ID>` for production. \
Eco keeps S3 traffic on Proxmox's private CT bridge and never routes it through public ingress."
                .to_string(),
        );
    }
    Ok(ref_val)
}

fn minio_client_config(endpoint: &str, region: &str, access_key: &str, secret_key: &str) -> Result<String, String> {
    for (name, value) in [("endpoint", endpoint), ("region", region), ("accessKey", access_key), ("secretKey", secret_key)] {
        if value.is_empty() || value.contains('\n') || value.contains('\r') {
            return Err(format!("Invalid managed MinIO {name} value."));
        }
    }
    Ok(format!(
        "S3_ENDPOINT={endpoint}\nS3_REGION={region}\nS3_ACCESS_KEY={access_key}\nS3_SECRET_KEY={secret_key}\n"
    ))
}

fn resolve_ct_primary_ip(ctid: &str) -> Result<String, String> {
    let output = pct_exec_capture(
        ctid,
        "ip=$(ip -4 -o addr show scope global | awk '{ split($4, address, \"/\"); print address[1]; exit }'); if [ -n \"$ip\" ]; then printf '%s\\n' \"$ip\"; else hostname -I | awk '{ for (i = 1; i <= NF; i++) { if (split($i, octets, \".\") == 4) { print $i; exit } } }'; fi",
    )?;
    let ip = output.trim().to_string();
    if ip.is_empty() {
        return Err(format!("Cannot resolve primary IP for CT {ctid}."));
    }
    Ok(ip)
}

fn provision_dedicated_minio(storage: &HashMap<String, HashMap<String, String>>, app_ctid: &str) -> Result<Option<MinioInfo>, String> {
    if storage.get("minio").is_none() {
        return Ok(None);
    }
    let minio_ctid = resolve_ct_input(&minio_ct_reference(storage)?)?;
    if minio_ctid == app_ctid {
        return Err("storage.minio.ct must name a dedicated MinIO CT, not the application CT.".to_string());
    }
    ensure_ct_running(&minio_ctid)?;

    let installer = embedded::INSTALL_MINIO_SH;
    let installer_path = "/tmp/eco-install-minio.sh";
    push_text_file_to_ct(&minio_ctid, installer_path, installer, "eco-install-minio.sh")?;
    pct_exec(&minio_ctid, &format!("chmod 700 {installer_path} && ECO_DEPLOY_MODE=prod bash {installer_path} --ensure && rm -f {installer_path}"))?;

    let credentials = pct_exec_capture(
        &minio_ctid,
        "awk -F= '$1 == \"S3_ACCESS_KEY\" || $1 == \"S3_SECRET_KEY\" { print }' /etc/eco/minio-client.env",
    )?;
    let mut values = HashMap::new();
    for line in credentials.split('\n') {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(eq) = line.find('=') {
            values.insert(line[..eq].to_string(), line[eq + 1..].to_string());
        }
    }
    let ip = resolve_ct_primary_ip(&minio_ctid)?;
    let region = storage
        .get("minio")
        .and_then(|m| m.get("region"))
        .cloned()
        .unwrap_or_else(|| "us-east-1".to_string());
    let endpoint = format!("http://{ip}:9000");
    let client_config = minio_client_config(
        &endpoint,
        &region,
        values.get("S3_ACCESS_KEY").map(|s| s.as_str()).unwrap_or(""),
        values.get("S3_SECRET_KEY").map(|s| s.as_str()).unwrap_or(""),
    )?;
    Ok(Some(MinioInfo {
        ctid: minio_ctid,
        endpoint,
        client_config,
    }))
}

pub struct MinioInfo {
    pub ctid: String,
    pub endpoint: String,
    pub client_config: String,
}

fn install_minio_client_config(ctid: &str, minio: &MinioInfo) -> Result<(), String> {
    pct_exec(ctid, "mkdir -p /etc/eco")?;
    push_text_file_to_ct(ctid, "/etc/eco/minio-client.env", &minio.client_config, "minio-client.env")?;
    pct_exec(ctid, "chmod 600 /etc/eco/minio-client.env")
}

fn is_token_based_tunnel_config(content: &str) -> bool {
    let has_tunnel = content
        .lines()
        .any(|l| l.starts_with("tunnel:") && l.split(':').nth(1).map(|s| !s.trim().is_empty()).unwrap_or(false));
    let has_credentials = content
        .lines()
        .any(|l| l.starts_with("credentials-file:") && l.split(':').nth(1).map(|s| !s.trim().is_empty()).unwrap_or(false));
    has_tunnel && !has_credentials
}

fn is_eco_managed_token_tunnel(content: &str) -> bool {
    let has_id = content
        .lines()
        .any(|l| l.starts_with("# eco-tunnel-id:") && l.split(':').nth(1).map(|s| !s.trim().is_empty()).unwrap_or(false));
    is_token_based_tunnel_config(content) && has_id
}

fn parse_cloudflared_config(content: &str) -> (Vec<String>, Vec<(String, String)>) {
    let mut top_lines = Vec::new();
    let mut rules: Vec<(String, String)> = Vec::new();
    let mut in_ingress = false;
    let mut current_hostname = String::new();

    for line in content.split('\n') {
        let l = line.trim_end_matches('\r');
        if !in_ingress && l.trim() == "ingress:" {
            in_ingress = true;
            continue;
        }
        if !in_ingress {
            top_lines.push(l.to_string());
            continue;
        }
        let trimmed = l.trim_start();
        if let Some(rest) = trimmed.strip_prefix("- hostname:") {
            current_hostname = rest.trim().to_string();
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("service:") {
            let service = rest.trim().to_string();
            if current_hostname.is_empty() {
                rules.push((String::new(), service));
            } else {
                rules.push((current_hostname.clone(), service));
                current_hostname.clear();
            }
        }
    }
    (top_lines, rules)
}

fn serialize_cloudflared_config(top_lines: &[String], rules: &[(String, String)]) -> String {
    let mut lines: Vec<String> = top_lines.to_vec();
    if lines.last().map(|l| !l.is_empty()).unwrap_or(false) {
        lines.push(String::new());
    }
    lines.push("ingress:".to_string());
    for (hostname, service) in rules {
        if hostname.is_empty() {
            lines.push(format!("  - service: {service}"));
        } else {
            lines.push(format!("  - hostname: {hostname}"));
            lines.push(format!("    service: {service}"));
        }
    }
    let joined = lines.join("\n");
    let trimmed = joined.trim_end();
    format!("{trimmed}\n")
}

fn upsert_cloudflared_hostname(content: &str, hostname: &str, service_url: &str) -> String {
    let (top_lines, rules) = parse_cloudflared_config(content);
    let mut non_fallback: Vec<(String, String)> = rules
        .iter()
        .filter(|(h, s)| !h.is_empty() || s != "http_status:404")
        .cloned()
        .collect();
    let fallback = rules
        .iter()
        .find(|(h, s)| h.is_empty() && s == "http_status:404")
        .cloned()
        .unwrap_or((String::new(), "http_status:404".to_string()));

    let mut replaced = false;
    let next_rules: Vec<(String, String)> = non_fallback
        .iter()
        .map(|(h, s)| {
            if h == hostname {
                replaced = true;
                (hostname.to_string(), service_url.to_string())
            } else {
                (h.clone(), s.clone())
            }
        })
        .collect();
    let mut final_rules = next_rules;
    if !replaced {
        final_rules.push((hostname.to_string(), service_url.to_string()));
    }
    final_rules.push(fallback);
    serialize_cloudflared_config(&top_lines, &final_rules)
}

fn require_pm2_config_snippet(config_path: &str) -> String {
    let cjs_path = config_path.trim_end_matches(".js").to_string() + ".cjs";
    format!(
        "const config = (() => {{ try {{ return require({}); }} catch (e) {{ return require({}); }} }})();",
        util::json_string(&cjs_path),
        util::json_string(config_path)
    )
}

fn build_prune_conflicting_ports_command(config_path: &str) -> String {
    let js = format!(
        "{}\nconst {{ execSync }} = require('child_process');\nconst ports = new Set();\nfor (const app of (config.apps || [])) {{ for (const val of Object.values(app.env || {{}})) {{ const p = parseInt(val, 10); if (p > 0) ports.add(p); }} }}\nif (ports.size > 0) {{\n  let procs = [];\n  try {{ procs = JSON.parse(execSync('pm2 jlist').toString()); }} catch (e) {{}}\n  for (const proc of procs) {{\n    const env = proc.pm2_env || {{}};\n    let hit = false;\n    for (const val of Object.values(env)) {{ const p = parseInt(val, 10); if (ports.has(p)) {{ hit = true; break; }} }}\n    if (hit) {{ try {{ execSync('pm2 delete ' + JSON.stringify(proc.name)); }} catch (e) {{}} }}\n  }}\n}}",
        require_pm2_config_snippet(config_path)
    );
    format!("node -e {}", util::shell_single_quote(&js))
}

fn delete_declared_pm2_apps_js(config_path: &str) -> String {
    format!(
        "{}\nconst {{ execSync }} = require('child_process');\nfor (const app of (config.apps || [])) {{\n  if (!app.name) continue;\n  try {{ execSync('pm2 delete ' + JSON.stringify(app.name), {{ stdio: 'ignore' }}); }} catch (e) {{}}\n}}",
        require_pm2_config_snippet(config_path)
    )
}

fn build_delete_declared_pm2_apps_command(config_path: &str) -> String {
    format!("node -e {}", util::shell_single_quote(&delete_declared_pm2_apps_js(config_path)))
}

fn systemd_mode() -> bool {
    // We are on systemd now — PM2 is legacy. Default to systemd; opt out with
    // ECO_SYSTEMD=0 only for a legacy PM2-only CT.
    util::env_var_or("ECO_SYSTEMD", "1") != "0"
}

// Env prefix passed into configure.sh on the CT so it emits systemd units
// (ECO_SYSTEMD=1) alongside the PM2 config during the migration.
fn systemd_env() -> &'static str {
    if systemd_mode() { "ECO_SYSTEMD=1" } else { "" }
}

// Phase 3: when ECO_SYSTEMD=1, restart the estate's apps via systemd units
// (generated by configure.sh from the same ecosystem.config.js) instead of
// `pm2 startOrReload`. Emits: daemon-reload, then enable/reset-failed/restart
// for each app, then VERIFIES each unit is active. A unit that fails to
// enable or does not come up reports its name and makes the deploy exit
// non-zero, so a "Completed" deploy guarantees the services are actually
// running (previously `|| true` swallowed every failure).
fn build_systemd_start_command(config_path: &str) -> String {
    let js = format!(
        r#"{}
const apps = config.apps || [];
const units = apps.map((a) => a.name).filter(Boolean).map((n) => 'eco-' + n + '.service');
const lines = ['systemctl daemon-reload 2>/dev/null || true'];
for (const unit of units) {{
  lines.push('systemctl enable ' + unit + ' 2>/dev/null || true');
  lines.push('systemctl reset-failed ' + unit + ' 2>/dev/null || true');
  lines.push('systemctl restart ' + unit + ' 2>/dev/null || true');
}}
lines.push('sleep 2');
lines.push('failed=""');
for (const unit of units) {{
  lines.push('if ! systemctl is-active --quiet ' + unit + '; then failed="$failed ' + unit + '"; fi');
}}
lines.push('if [ -n "$failed" ]; then echo "FAILED_TO_START:$failed" >&2; exit 1; fi');
process.stdout.write(lines.join('; '));"#,
        require_pm2_config_snippet(config_path)
    );
    // The JS builds the systemctl chain and prints it to stdout; pipe it into
    // `bash` so it actually runs on the CT (previously the chain was only
    // echoed and never executed — services that were already running stayed
    // up by luck, but a stopped service was never started).
    format!("node -e {} | bash", util::shell_single_quote(&js))
}

fn resolve_service_port_from_ct(ctid: &str, config_path: &str, service_name: &str) -> Result<String, String> {
    let js = format!(
        "{}\nconst target = {};\nconst apps = config.apps || [];\nconst match = apps.find((app) => app.name === target || app.name.endsWith(\"-\" + target));\nif (!match) {{ process.stderr.write(\"Missing service \" + target); process.exit(1); }}\nconst env = match.env || {{}};\nconst port = env.PORT || env.SERVER_PORT || \"\";\nprocess.stdout.write(String(port));",
        require_pm2_config_snippet(config_path),
        util::json_string(service_name)
    );
    let output = pct_exec_capture(ctid, &format!("node -e {}", util::shell_single_quote(&js)))?;
    let port = output.trim().to_string();
    if port.is_empty() || !port.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!("Cannot resolve port for exposed service \"{service_name}\" from {config_path}."));
    }
    Ok(port)
}

fn ct_config_has_service(ctid: &str, config_path: &str, service_name: &str) -> bool {
    let js = format!(
        "{}\nconst target = {};\nconst apps = config.apps || [];\nconst match = apps.find((app) => app.name === target || app.name.endsWith(\"-\" + target));\nprocess.stdout.write(match ? \"yes\" : \"no\");",
        require_pm2_config_snippet(config_path),
        util::json_string(service_name)
    );
    match pct_exec_capture(ctid, &format!("node -e {}", util::shell_single_quote(&js))) {
        Ok(output) => output.trim() == "yes",
        Err(_) => false,
    }
}

fn ensure_ct_caddy(ctid: &str) -> Result<(), String> {
    pct_exec(
        ctid,
        "if ! command -v caddy >/dev/null 2>&1; then\n  apt-get update;\n  apt-get install -y caddy;\nfi",
    )
}

fn ensure_proxy_cloudflared(proxy_ctid: &str) -> Result<(), String> {
    pct_exec(
        proxy_ctid,
        "if ! command -v cloudflared >/dev/null 2>&1; then\n  apt-get update;\n  apt-get install -y curl ca-certificates;\n  arch=$(dpkg --print-architecture);\n  case \"$arch\" in\n    amd64) pkg=cloudflared-linux-amd64.deb ;;\n    arm64) pkg=cloudflared-linux-arm64.deb ;;\n    *) echo \"Unsupported architecture for cloudflared: $arch\" >&2; exit 1 ;;\n  esac;\n  curl -fsSL \"https://github.com/cloudflare/cloudflared/releases/latest/download/${pkg}\" -o /tmp/cloudflared.deb;\n  apt-get install -y /tmp/cloudflared.deb;\n  rm -f /tmp/cloudflared.deb;\nfi",
    )
}

fn resolve_proxy_cloudflared_config_path(proxy_ctid: &str, preferred_path: &str, cloudflare_account: &str) -> Result<String, String> {
    let mut candidates: Vec<String> = Vec::new();
    if !preferred_path.is_empty() {
        candidates.push(preferred_path.to_string());
    }
    if !cloudflare_account.is_empty() {
        candidates.push(cloudflare::cloudflared_config_path_for_account(cloudflare_account));
    } else {
        candidates.push("/etc/cloudflared/config.yml".to_string());
        candidates.push("/root/.cloudflared/config.yml".to_string());
    }
    for candidate in candidates {
        let result = run_capture(
            "pct",
            &["exec".to_string(), proxy_ctid.to_string(), "--".to_string(), "bash".to_string(), "-lc".to_string(), format!("test -f {candidate}")],
            &util::current_dir(),
        )?;
        if result.code == 0 {
            return Ok(candidate);
        }
    }
    if !cloudflare_account.is_empty() {
        return Err(format!(
            "Cannot locate cloudflared config for account \"{cloudflare_account}\" in proxy CT {proxy_ctid} at {}.",
            cloudflare::cloudflared_config_path_for_account(cloudflare_account)
        ));
    }
    let systemctl_result = run_capture(
        "pct",
        &["exec".to_string(), proxy_ctid.to_string(), "--".to_string(), "bash".to_string(), "-lc".to_string(), "systemctl cat cloudflared 2>/dev/null || true".to_string()],
        &util::current_dir(),
    )?;
    let unit_text = format!("{}\n{}", systemctl_result.stdout, systemctl_result.stderr);
    // find --config <path> or --config=<path>
    if let Some(idx) = unit_text.find("--config") {
        let rest = &unit_text[idx + 8..];
        let rest = rest.trim_start();
        let path = if let Some(rest) = rest.strip_prefix('=') {
            rest.split_whitespace().next().unwrap_or("").trim_matches('"').trim_matches('\'').to_string()
        } else {
            rest.split_whitespace().next().unwrap_or("").trim_matches('"').trim_matches('\'').to_string()
        };
        if !path.is_empty() {
            let exists = run_capture(
                "pct",
                &["exec".to_string(), proxy_ctid.to_string(), "--".to_string(), "bash".to_string(), "-lc".to_string(), format!("test -f {path}")],
                &util::current_dir(),
            )?;
            if exists.code == 0 {
                return Ok(path);
            }
        }
    }
    let find_result = run_capture(
        "pct",
        &["exec".to_string(), proxy_ctid.to_string(), "--".to_string(), "bash".to_string(), "-lc".to_string(), "find /etc/cloudflared /root/.cloudflared /home -maxdepth 3 -type f -name 'config.yml' 2>/dev/null | head -n 1".to_string()],
        &util::current_dir(),
    )?;
    let found = find_result.stdout.trim().to_string();
    if !found.is_empty() {
        return Ok(found);
    }
    Err(format!("Cannot locate cloudflared config in proxy CT {proxy_ctid}. Checked common paths and systemd unit config."))
}

fn prompt_for_replicas(default_value: &str) -> String {
    match crate::checklist::prompt_line(&format!("Number of cloudflared tunnel replicas? [{default_value}]: ")) {
        Ok(answer) => {
            let parsed: i64 = answer.trim().parse().unwrap_or(0);
            if parsed < 1 {
                default_value.to_string()
            } else {
                parsed.to_string()
            }
        }
        Err(_) => default_value.to_string(),
    }
}

fn ensure_tunnel_replicas(proxy_ctid: &str, account: &str, count: i64, cloudflared_config_path: &str, _dry_run: bool) -> Result<(), String> {
    let service_name = cloudflare::cloudflared_service_name_for_account(if account == "default" { "" } else { account });
    let template_unit_name = format!("{service_name}@.service");
    let config_content = pct_exec_capture(proxy_ctid, &format!("cat {cloudflared_config_path}"))?;
    let token = config_content
        .lines()
        .find_map(|l| l.strip_prefix("tunnel:"))
        .map(|t| t.trim().to_string())
        .unwrap_or_default();
    if token.is_empty() {
        print_step(&format!("Cannot read tunnel token from {cloudflared_config_path} in proxy CT {proxy_ctid}; skipping replicas setup"));
        return Ok(());
    }
    let unit_content = format!(
        "[Unit]\nDescription=cloudflared {account} replica %i\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nTimeoutStartSec=15\nType=notify\nExecStart=/usr/bin/cloudflared --no-autoupdate --config {cloudflared_config_path} tunnel run --token {token}\nRestart=on-failure\nRestartSec=5s\n\n[Install]\nWantedBy=multi-user.target\n"
    );
    push_text_file_to_ct(proxy_ctid, &format!("/etc/systemd/system/{template_unit_name}"), &unit_content, "cloudflared-template")?;
    pct_exec(proxy_ctid, "systemctl daemon-reload")?;
    let current_raw = pct_exec_capture(
        proxy_ctid,
        &format!("systemctl list-units '{service_name}@*' --no-legend --state=active 2>/dev/null | wc -l"),
    )?;
    let current: i64 = current_raw.trim().parse().unwrap_or(0);
    if count > current {
        print_step(&format!("Scaling cloudflared {account} from {current} to {count} replica(s)"));
        for i in (current + 1)..=count {
            pct_exec(proxy_ctid, &format!("systemctl enable --now {service_name}@{i}"))?;
        }
    } else if count < current {
        print_step(&format!("Scaling cloudflared {account} from {current} to {count} replica(s)"));
        for i in (count + 1)..=current {
            pct_exec(proxy_ctid, &format!("systemctl disable --now {service_name}@{i}"))?;
        }
    }
    Ok(())
}

fn ensure_proxy_hostname(
    dry_run: bool,
    proxy_ct_input: &str,
    hostname: &str,
    service_url: &str,
    _cloudflared_config: &str,
    tunnel_name: &str,
    cloudflare_account: &str,
    tunnel_replicas: i64,
    non_interactive: bool,
) -> Result<Vec<String>, String> {
    let default_config_path = cloudflare::cloudflared_config_path_for_account(cloudflare_account);
    let service_name = cloudflare::cloudflared_service_name_for_account(cloudflare_account);

    if dry_run {
        let lines = vec![
            format!(
                "# expose: {hostname} -> {service_url} via proxy CT {proxy_ct_input}{}",
                if cloudflare_account.is_empty() { String::new() } else { format!(" (Cloudflare account \"{cloudflare_account}\")") }
            ),
            format!("pct exec {proxy_ct_input} -- bash -lc 'mkdir -p {}'", std::path::Path::new(&default_config_path).parent().map(|p| p.display().to_string()).unwrap_or_default()),
            format!("pct exec {proxy_ct_input} -- bash -lc '# auto-detect cloudflared config path, update ingress for {hostname}'"),
            format!("pct exec {proxy_ct_input} -- bash -lc '# create DNS route only when tunnel auth supports cloudflared tunnel route dns'"),
            format!("pct exec {proxy_ct_input} -- bash -lc 'systemctl restart {service_name} || service {service_name} restart'"),
        ];
        return Ok(lines);
    }

    let proxy_ctid = resolve_ct_input(proxy_ct_input)?;
    ensure_ct_running(&proxy_ctid)?;
    print_step(&format!(
        "Exposing {hostname} via proxy CT {proxy_ctid}{}",
        if cloudflare_account.is_empty() { String::new() } else { format!(" (Cloudflare account \"{cloudflare_account}\")") }
    ));
    ensure_proxy_cloudflared(&proxy_ctid)?;
    pct_exec(&proxy_ctid, &format!("mkdir -p {}", std::path::Path::new(&default_config_path).parent().map(|p| p.display().to_string()).unwrap_or_default()))?;

    let mut cloudflared_config_path = String::new();
    match resolve_proxy_cloudflared_config_path(&proxy_ctid, "", cloudflare_account) {
        Ok(p) => cloudflared_config_path = p,
        Err(_) => {
            print_step(&format!(
                "Proxy CT {proxy_ctid} has no cloudflared config{}. Bootstrapping dedicated tunnel for {hostname}",
                if cloudflare_account.is_empty() { String::new() } else { format!(" for account \"{cloudflare_account}\"") }
            ));
            let bootstrap = crate::commands::proxy::ensure_proxy_tunnel(
                &proxy_ctid,
                hostname,
                tunnel_name,
                service_url,
                non_interactive,
                cloudflare_account,
            )?;
            cloudflared_config_path = if bootstrap.config_path.is_empty() { default_config_path.clone() } else { bootstrap.config_path };
        }
    }

    let existing_config = match pct_exec_capture(&proxy_ctid, &format!("cat {cloudflared_config_path}")) {
        Ok(c) => c,
        Err(_) => {
            print_step(&format!(
                "Proxy CT {proxy_ctid} cloudflared config is missing at {cloudflared_config_path}. Rebuilding tunnel automation for {hostname}"
            ));
            let bootstrap = crate::commands::proxy::ensure_proxy_tunnel(
                &proxy_ctid,
                hostname,
                tunnel_name,
                service_url,
                non_interactive,
                cloudflare_account,
            )?;
            cloudflared_config_path = if bootstrap.config_path.is_empty() { default_config_path.clone() } else { bootstrap.config_path };
            pct_exec_capture(&proxy_ctid, &format!("cat {cloudflared_config_path}"))?
        }
    };

    if is_token_based_tunnel_config(&existing_config) && !is_eco_managed_token_tunnel(&existing_config) {
        print_step(&format!(
            "Proxy CT {proxy_ctid} is using a legacy token-based cloudflared config. Replacing it with a dedicated eco-managed tunnel for {hostname}"
        ));
        let bootstrap = crate::commands::proxy::ensure_proxy_tunnel(
            &proxy_ctid,
            hostname,
            tunnel_name,
            service_url,
            non_interactive,
            cloudflare_account,
        )?;
        cloudflared_config_path = if bootstrap.config_path.is_empty() { default_config_path.clone() } else { bootstrap.config_path };
        let _ = pct_exec_capture(&proxy_ctid, &format!("cat {cloudflared_config_path}"))?;
    }

    let has_tunnel_line = existing_config
        .lines()
        .any(|l| l.starts_with("tunnel:") && l.split(':').nth(1).map(|s| !s.trim().is_empty()).unwrap_or(false));
    if !has_tunnel_line {
        return Err(format!("cloudflared config in proxy CT {proxy_ctid} is missing a tunnel: entry."));
    }

    if is_token_based_tunnel_config(&existing_config) && !is_eco_managed_token_tunnel(&existing_config) {
        print_step(&format!(
            "Proxy CT {proxy_ctid} already has a token tunnel configured; skipping tunnel configuration for {hostname}"
        ));
        return Ok(Vec::new());
    }

    if is_eco_managed_token_tunnel(&existing_config) {
        let (_, rules) = parse_cloudflared_config(&existing_config);
        let existing_rule = rules.iter().find(|(h, _)| h == hostname);
        if let Some((_, svc)) = existing_rule {
            if svc == service_url {
                print_step(&format!(
                    "Proxy CT {proxy_ctid} already has {hostname} -> {service_url}; reconciling DNS and remote tunnel config"
                ));
                let tunnel_id = existing_config
                    .lines()
                    .find_map(|l| l.strip_prefix("# eco-tunnel-id:").map(|s| s.trim().to_string()))
                    .unwrap_or_default();
                if !tunnel_id.is_empty() && cloudflare::has_cloudflare_api_env(cloudflare_account) {
                    cloudflare::overwrite_dns_record_for_tunnel(hostname, &tunnel_id, cloudflare_account)?;
                    cloudflare::put_remote_tunnel_config(&tunnel_id, hostname, service_url, cloudflare_account)?;
                } else {
                    let resolved_tunnel_name = existing_config
                        .lines()
                        .find_map(|l| l.strip_prefix("tunnel:").map(|s| s.trim().to_string()))
                        .unwrap_or_default();
                    if !resolved_tunnel_name.is_empty() {
                        let _ = pct_exec(
                            &proxy_ctid,
                            &format!("cloudflared tunnel route dns {} {} || true", escape_single_quotes(&resolved_tunnel_name), escape_single_quotes(hostname)),
                        );
                    }
                }
                if tunnel_replicas > 1 {
                    let final_replicas = if !non_interactive {
                        prompt_for_replicas(&tunnel_replicas.to_string()).parse::<i64>().unwrap_or(tunnel_replicas)
                    } else {
                        tunnel_replicas
                    };
                    if final_replicas > 1 {
                        ensure_tunnel_replicas(&proxy_ctid, if cloudflare_account.is_empty() { "default" } else { cloudflare_account }, final_replicas, &cloudflared_config_path, dry_run)?;
                    }
                }
                return Ok(Vec::new());
            }
        }

        print_step(&format!("Adding {hostname} to existing eco-managed tunnel in proxy CT {proxy_ctid}"));
        let tunnel_id = existing_config
            .lines()
            .find_map(|l| l.strip_prefix("# eco-tunnel-id:").map(|s| s.trim().to_string()))
            .unwrap_or_default();
        let next_config = upsert_cloudflared_hostname(&existing_config, hostname, service_url);
        push_text_file_to_ct(&proxy_ctid, "/tmp/eco-cloudflared-config.yml", &next_config, "cloudflared-config")?;
        pct_exec(&proxy_ctid, &format!("install -D -m 0644 /tmp/eco-cloudflared-config.yml {cloudflared_config_path} && rm -f /tmp/eco-cloudflared-config.yml"))?;

        if !tunnel_id.is_empty() && cloudflare::has_cloudflare_api_env(cloudflare_account) {
            cloudflare::overwrite_dns_record_for_tunnel(hostname, &tunnel_id, cloudflare_account)?;
            cloudflare::put_remote_tunnel_config(&tunnel_id, hostname, service_url, cloudflare_account)?;
        } else {
            let resolved_tunnel_name = existing_config
                .lines()
                .find_map(|l| l.strip_prefix("tunnel:").map(|s| s.trim().to_string()))
                .unwrap_or_default();
            if !resolved_tunnel_name.is_empty() {
                let _ = pct_exec(
                    &proxy_ctid,
                    &format!("cloudflared tunnel route dns {} {} || true", escape_single_quotes(&resolved_tunnel_name), escape_single_quotes(hostname)),
                );
            }
        }
        let _ = pct_exec(&proxy_ctid, &format!("systemctl restart {service_name} || service {service_name} restart"));
        print_step(&format!("Expose complete for {hostname}"));
        if tunnel_replicas > 1 {
            let final_replicas = if !non_interactive {
                prompt_for_replicas(&tunnel_replicas.to_string()).parse::<i64>().unwrap_or(tunnel_replicas)
            } else {
                tunnel_replicas
            };
            if final_replicas > 1 {
                ensure_tunnel_replicas(&proxy_ctid, if cloudflare_account.is_empty() { "default" } else { cloudflare_account }, final_replicas, &cloudflared_config_path, dry_run)?;
            }
        }
        return Ok(Vec::new());
    }

    let next_config = upsert_cloudflared_hostname(&existing_config, hostname, service_url);
    push_text_file_to_ct(&proxy_ctid, "/tmp/eco-cloudflared-config.yml", &next_config, "cloudflared-config")?;
    pct_exec(&proxy_ctid, &format!("install -D -m 0644 /tmp/eco-cloudflared-config.yml {cloudflared_config_path} && rm -f /tmp/eco-cloudflared-config.yml"))?;

    let resolved_tunnel_name = existing_config
        .lines()
        .find_map(|l| l.strip_prefix("tunnel:").map(|s| s.trim().to_string()))
        .unwrap_or_default();
    if resolved_tunnel_name.is_empty() {
        return Err(format!("Cannot resolve tunnel name/id from {cloudflared_config_path} in proxy CT {proxy_ctid}."));
    }
    let _ = pct_exec(
        &proxy_ctid,
        &format!("cloudflared tunnel route dns {} {} || true", escape_single_quotes(&resolved_tunnel_name), escape_single_quotes(hostname)),
    );

    if let Err(e) = pct_exec(&proxy_ctid, &format!("systemctl restart {service_name} || service {service_name} restart")) {
        print_step(&format!(
            "{service_name} restart failed in proxy CT {proxy_ctid}. Recreating the dedicated tunnel for {hostname}: {e}"
        ));
        crate::commands::proxy::ensure_proxy_tunnel(&proxy_ctid, hostname, tunnel_name, service_url, true, cloudflare_account)?;
    }

    print_step(&format!("Expose complete for {hostname}"));
    if tunnel_replicas > 1 {
        let final_replicas = if !non_interactive {
            prompt_for_replicas(&tunnel_replicas.to_string()).parse::<i64>().unwrap_or(tunnel_replicas)
        } else {
            tunnel_replicas
        };
        if final_replicas > 1 {
            ensure_tunnel_replicas(&proxy_ctid, if cloudflare_account.is_empty() { "default" } else { cloudflare_account }, final_replicas, &cloudflared_config_path, dry_run)?;
        }
    }
    Ok(Vec::new())
}

fn expose_via_proxy_ct(
    dry_run: bool,
    expose: &ecompose::Expose,
    project: &str,
    app_ctid: &str,
    app_config_path: &str,
) -> Result<Vec<String>, String> {
    if !expose.enabled() {
        return Ok(Vec::new());
    }
    let hostname = {
        let h = expose.hostname();
        if h.is_empty() { format!("{project}.ktt.my.id") } else { h }
    };
    let mut service_name = expose.service();
    if service_name.is_empty() {
        service_name = format!("{project}-frontend");
    }
    let proxy_ct_input = expose.proxy_ct_input();
    if proxy_ct_input.is_empty() {
        return Err(format!("Expose is enabled for {project}, but expose.proxy_ct is missing."));
    }

    let mut results = Vec::new();

    if dry_run {
        let service_port = if !expose.target_port().is_empty() && expose.target_port().chars().all(|c| c.is_ascii_digit()) {
            expose.target_port()
        } else {
            format!("<service-port:{service_name}>")
        };
        results.extend(ensure_proxy_hostname(
            dry_run,
            &proxy_ct_input,
            &hostname,
            &format!("http://<app-ct-ip>:{service_port}"),
            &expose.cloudflared_config(),
            &expose.tunnel_name(),
            &expose.cloudflare_account(),
            expose.tunnel_replicas.unwrap_or(0),
            false,
        )?);
    } else {
        let gateway_service_name = format!("{project}-gateway");
        let has_gateway_service = ct_config_has_service(app_ctid, app_config_path, &gateway_service_name);
        if has_gateway_service {
            service_name = gateway_service_name;
        }
        let port = if !has_gateway_service && !expose.target_port().is_empty() && expose.target_port().chars().all(|c| c.is_ascii_digit()) {
            expose.target_port()
        } else {
            resolve_service_port_from_ct(app_ctid, app_config_path, &service_name)?
        };
        let app_ip = resolve_ct_primary_ip(app_ctid)?;
        let service_url = format!("http://{app_ip}:{port}");
        ensure_proxy_hostname(
            dry_run,
            &proxy_ct_input,
            &hostname,
            &service_url,
            &expose.cloudflared_config(),
            &expose.tunnel_name(),
            &expose.cloudflare_account(),
            expose.tunnel_replicas.unwrap_or(0),
            false,
        )?;
    }

    for entry in &expose.additional {
        let entry_hostname = entry.get("hostname").cloned().unwrap_or_default();
        let entry_service_name = entry.get("service").cloned().unwrap_or_default();
        if entry_hostname.is_empty() || entry_service_name.is_empty() {
            continue;
        }
        if dry_run {
            let service_port = entry
                .get("target_port")
                .filter(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
                .cloned()
                .unwrap_or_else(|| format!("<service-port:{entry_service_name}>"));
            results.extend(ensure_proxy_hostname(
                dry_run,
                &proxy_ct_input,
                &entry_hostname,
                &format!("http://<app-ct-ip>:{service_port}"),
                &entry.get("cloudflared_config").cloned().unwrap_or_else(|| expose.cloudflared_config()),
                &entry.get("tunnel_name").cloned().unwrap_or_else(|| expose.tunnel_name()),
                &entry.get("cloudflare_account").cloned().unwrap_or_else(|| expose.cloudflare_account()),
                0,
                false,
            )?);
            continue;
        }
        let entry_port = entry
            .get("target_port")
            .filter(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
            .cloned()
            .unwrap_or(resolve_service_port_from_ct(app_ctid, app_config_path, &entry_service_name)?);
        let entry_app_ip = resolve_ct_primary_ip(app_ctid)?;
        let entry_service_url = format!("http://{entry_app_ip}:{entry_port}");
        ensure_proxy_hostname(
            dry_run,
            &proxy_ct_input,
            &entry_hostname,
            &entry_service_url,
            &entry.get("cloudflared_config").cloned().unwrap_or_else(|| expose.cloudflared_config()),
            &entry.get("tunnel_name").cloned().unwrap_or_else(|| expose.tunnel_name()),
            &entry.get("cloudflare_account").cloned().unwrap_or_else(|| expose.cloudflare_account()),
            0,
            false,
        )?;
    }
    Ok(results)
}

fn tar_and_push_dir(ctid: &str, source_dir: &str, target_tar_name: &str) -> Result<(), String> {
    let temp_dir = std::env::temp_dir().join(format!("eco-up-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&temp_dir);
    let tar_path = temp_dir.join(format!("{target_tar_name}.tar"));
    let source = Path::new(source_dir);
    let parent_dir = source.parent().unwrap_or(Path::new("/")).display().to_string();
    let base_name = source.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();

    let mut tar_args: Vec<String> = vec!["-C".to_string(), parent_dir];
    if base_name != target_tar_name {
        tar_args.push("--transform".to_string());
        tar_args.push(format!("s|^{base_name}|{target_tar_name}|"));
    }
    for exclude in [
        ".env", "*/.env", ".env.local", "*/.env.local", ".env.*.local", "*/.env.*.local",
        ".configure-state", "*/.configure-state",
        "ecosystem.config.js", "*/ecosystem.config.js",
        "ecosystem.config.cjs", "*/ecosystem.config.cjs",
        "Caddyfile", "*/Caddyfile",
        "node_modules", "*/node_modules",
        "target", "*/target",
        ".git", "*/.git",
    ] {
        tar_args.push("--exclude".to_string());
        tar_args.push(exclude.to_string());
    }
    tar_args.push("-cf".to_string());
    tar_args.push(tar_path.display().to_string());
    tar_args.push(base_name);

    let result = (|| -> Result<(), String> {
        run_command("tar", &tar_args, &util::current_dir())?;
        run_command(
            "pct",
            &["push".to_string(), ctid.to_string(), tar_path.display().to_string(), format!("/tmp/{target_tar_name}.tar")],
            &util::current_dir(),
        )
    })();
    let _ = std::fs::remove_dir_all(&temp_dir);
    result
}

fn is_on_proxmox_host() -> bool {
    util::command_on_path("pct")
}

fn is_ct_estate_context(input: &str) -> bool {
    if !Path::new("/opt/projects/eco").exists() {
        return false;
    }
    Path::new(input).is_absolute() && input.starts_with("/opt/projects")
}

fn resolve_host_ssh_dir() -> Result<PathBuf, String> {
    let home = util::home_dir();
    let ssh_dir = PathBuf::from(&home).join(".ssh");
    if !ssh_dir.is_dir() {
        return Err(format!("Host SSH directory not found: {}", ssh_dir.display()));
    }
    Ok(ssh_dir)
}

fn is_esm_project(project_dir: &Path) -> bool {
    project_is_esm(project_dir)
}

fn delete_local_declared_pm2_apps(config_path: &str, cwd: &Path) -> Result<(), String> {
    let js = delete_declared_pm2_apps_js(config_path);
    run_command("node", &["-e".to_string(), js], cwd)
}

fn resolve_local_pm2_config_path(dir: &Path) -> PathBuf {
    let cjs = dir.join("ecosystem.config.cjs");
    if cjs.is_file() {
        cjs
    } else {
        dir.join("ecosystem.config.js")
    }
}

fn assert_local_pm2_apps_present(config_path: &Path, cwd: &Path) -> Result<(), String> {
    let content = match std::fs::read_to_string(config_path) {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };
    let expected: Vec<String> = extract_pm2_app_names(&content);
    if expected.is_empty() {
        return Ok(());
    }
    let result = run_capture("pm2", &["jlist".to_string()], cwd)?;
    if result.code != 0 {
        return Err(format!("Unable to verify PM2 services after startup: {}", result.stderr.trim()));
    }
    let processes: serde_json::Value = serde_json::from_str(&result.stdout)
        .map_err(|_| "Unable to parse PM2 service list after startup.".to_string())?;
    let actual: Vec<String> = processes
        .as_array()
        .map(|arr| arr.iter().filter_map(|p| p.get("name").and_then(|n| n.as_str()).map(|s| s.to_string())).collect())
        .unwrap_or_default();
    let missing: Vec<&String> = expected.iter().filter(|name| !actual.contains(name)).collect();
    if !missing.is_empty() {
        return Err(format!(
            "PM2 did not register declared service(s): {}. Check the generated ecosystem config and service logs.",
            missing.iter().map(|m| m.as_str()).collect::<Vec<_>>().join(", ")
        ));
    }
    Ok(())
}

fn extract_pm2_app_names(config_text: &str) -> Vec<String> {
    // naive extraction of name: "..." entries within apps array
    let mut names = Vec::new();
    for line in config_text.split('\n') {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("name:") {
            let val = rest.trim();
            let val = val.trim_matches('"').trim_matches('\'');
            if !val.is_empty() && !val.contains("module") && !val.contains('}') {
                names.push(val.to_string());
            }
        }
    }
    names
}

fn run_up_dev(args: &[String]) -> Result<(), String> {
    let (options, positionals) = parse_options(args);
    let input = positionals.first().cloned().unwrap_or_else(|| ".".to_string());
    let cwd = util::current_dir();
    let deployment = load_project_deployment(&input, &cwd)?;
    let estate_root = deployment
        .project_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| deployment.project_dir.clone());
    let domains = ecompose::unique_domains_from_ecompose(&deployment.content, &deployment.project);
    let domain_branch_overrides = domain_branch_overrides_from_ecompose(&deployment.content);
    let domain_dev_flags = domain_dev_flags_from_ecompose(&deployment.content);

    let mut skipped_domains = std::collections::HashSet::new();
    let mut skipped_runtimes = std::collections::HashSet::new();
    for domain in &domains {
        if domain == &deployment.project {
            continue;
        }
        let flag = domain_dev_flags.get(domain).cloned().unwrap_or_default();
        if flag == "disabled" {
            skipped_domains.insert(domain.clone());
            continue;
        }
        if flag == "optional" {
            let mut unsatisfied = Vec::new();
            for token in domain_runtimes_from_services(domain, &deployment.services) {
                if !runtime_satisfiable(&token) {
                    unsatisfied.push(token);
                }
            }
            if !unsatisfied.is_empty() {
                skipped_domains.insert(domain.clone());
                for token in unsatisfied {
                    skipped_runtimes.insert(token);
                }
            }
        }
    }
    let dev_domains: Vec<String> = domains.iter().filter(|d| !skipped_domains.contains(*d)).cloned().collect();
    let dev_services: Vec<ecompose::Service> = deployment
        .services
        .iter()
        .filter(|s| {
            let first = s.path.split('/').next().unwrap_or("");
            !skipped_domains.contains(first)
        })
        .cloned()
        .collect();
    let skip_notice = if skipped_domains.is_empty() {
        String::new()
    } else {
        format!(
            "\nSkipped optional domain(s) in local dev (still deployed in prod): {}{}\n",
            skipped_domains.iter().cloned().collect::<Vec<_>>().join(", "),
            if skipped_runtimes.is_empty() {
                String::new()
            } else {
                format!(
                    " -- runtime(s) not available on this machine: {}",
                    skipped_runtimes.iter().cloned().collect::<Vec<_>>().join(", ")
                )
            }
        )
    };

    let services = discover_local_services(&estate_root, Some(&dev_services), &deployment.project_dir, &deployment.project);

    let dev_plan: Vec<String> = vec![
        format!("estate root: {}", estate_root.display()),
        dev_domains
            .iter()
            .filter(|d| d.as_str() != deployment.project)
            .map(|d| format!("clone repo if missing: {d}"))
            .collect::<Vec<_>>()
            .join("\n"),
        format!("provision local runtimes from manifest: {}", deployment.project_dir.display()),
        dev_services
            .iter()
            .filter(|s| s.runtimes.iter().any(|r| r == "postgresql@15"))
            .map(|s| format!("ensure local PostgreSQL database: {}", sql_database_name_for_service(s, &deployment.project)))
            .collect::<Vec<_>>()
            .join("\n"),
        dev_services
            .iter()
            .filter(|s| s.runtimes.iter().any(|r| r == "rust") && s.runtimes.iter().any(|r| r == "postgresql@15"))
            .map(|s| format!("run Rust migrations if present: {}", s.name))
            .collect::<Vec<_>>()
            .join("\n"),
        services
            .iter()
            .filter(|s| s.r#type == "rust")
            .map(|s| format!("build Rust service: {}", s.name))
            .collect::<Vec<_>>()
            .join("\n"),
        services
            .iter()
            .filter(|s| ["nextjs", "vite", "node"].contains(&s.r#type.as_str()))
            .map(|s| format!("npm install: {} ({})", s.name, s.dir))
            .collect::<Vec<_>>()
            .join("\n"),
        format!("configure locally in estate scope: {}", deployment.project_dir.display()),
        format!(
            "delete existing PM2 services declared by {}",
            resolve_local_pm2_config_path(&deployment.project_dir).display()
        ),
        format!(
            "pm2 startOrReload {} --update-env",
            resolve_local_pm2_config_path(&deployment.project_dir).display()
        ),
    ]
    .into_iter()
    .flat_map(|s| s.split('\n').map(|x| x.to_string()).collect::<Vec<_>>())
    .filter(|s| !s.is_empty())
    .collect();

    if options.get("dry-run").map(|v| v == "true").unwrap_or(false) {
        util::println_stdout("eco up dev plan");
        util::println_stdout(&format!("Manifest: {}", deployment.file_path));
        util::println_stdout(&format!("Project root: {}\n", deployment.project_dir.display()));
        if !skip_notice.is_empty() {
            util::println_stdout(&skip_notice);
        }
        for line in &dev_plan {
            util::println_stdout(line);
        }
        return Ok(());
    }

    if !skip_notice.is_empty() {
        util::println_stdout(&skip_notice);
    }
    ensure_local_domain_repos(&estate_root, &dev_domains, &deployment.project, &domain_branch_overrides, &deployment.content)?;
    print_step(&format!("Provisioning local runtimes for {}", deployment.project));
    let mut extra_env: Vec<(String, String)> = Vec::new();
    if !skipped_runtimes.is_empty() {
        extra_env.push(("ECO_DEV_SKIP_RUNTIMES".to_string(), skipped_runtimes.iter().cloned().collect::<Vec<_>>().join(",")));
    }
    embedded::run_bundled_script("provision.sh", &[deployment.project_dir.display().to_string()], "estate", &extra_env)?;
    bootstrap_local_postgres(&dev_services, &estate_root, &deployment.project)?;
    run_local_rust_migrations(&dev_services, &estate_root)?;

    print_step(&format!("Generating local ecosystem config for {}", deployment.project));
    let mut configure_env: Vec<(String, String)> = vec![
        ("ECO_NON_INTERACTIVE".to_string(), "1".to_string()),
        ("ECO_INIT".to_string(), "1".to_string()),
        ("PROJECT_DIR".to_string(), estate_root.display().to_string()),
        ("PROJECT_NAME".to_string(), deployment.project.clone()),
        ("PM2_DIR".to_string(), deployment.project_dir.display().to_string()),
    ];
    if !skipped_domains.is_empty() {
        configure_env.push(("ECO_DEV_SKIP_DOMAINS".to_string(), skipped_domains.iter().cloned().collect::<Vec<_>>().join(",")));
    }
    embedded::run_bundled_script("configure.sh", &[], "estate", &configure_env)?;

    let refreshed_services = discover_local_services(&estate_root, Some(&dev_services), &deployment.project_dir, &deployment.project);
    build_local_rust_services(&refreshed_services)?;
    install_local_dependencies(&refreshed_services)?;

    print_step(&format!("Starting PM2 services for {}", deployment.project));
    let ecosystem_config = resolve_local_pm2_config_path(&deployment.project_dir);
    print_step(&format!("Removing existing PM2 services for {}", deployment.project));
    delete_local_declared_pm2_apps(&ecosystem_config.display().to_string(), &deployment.project_dir)?;

    clear_local_next_development_caches(&refreshed_services)?;
    run_command("pm2", &["start".to_string(), ecosystem_config.display().to_string(), "--update-env".to_string()], &deployment.project_dir)?;
    assert_local_pm2_apps_present(&ecosystem_config, &deployment.project_dir)?;
    print_step(&format!("Completed local dev bootstrap for {}", deployment.project));

    util::println_stdout("\n[eco up] Following PM2 logs — press Ctrl+C to stop\n");
    let status = std::process::Command::new("pm2")
        .args(["logs".to_string(), "--lines".to_string(), "50".to_string()])
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status();
    let _ = status;
    Ok(())
}

fn clear_local_next_development_caches(services: &[LocalService]) -> Result<(), String> {
    for service in services {
        if service.r#type != "nextjs" {
            continue;
        }
        let cache_dir = Path::new(&service.dir).join(".next");
        if !cache_dir.exists() {
            continue;
        }
        print_step(&format!("Clearing Next.js development cache: {}", service.name));
        let _ = std::fs::remove_dir_all(&cache_dir);
    }
    Ok(())
}

#[derive(Clone)]
struct LocalService {
    name: String,
    r#type: String,
    dir: String,
}

fn detect_service_type_local(package_json_text: &str) -> String {
    if package_json_text.contains("\"next\"") {
        "nextjs".to_string()
    } else if package_json_text.contains("\"astro\"") {
        "astro".to_string()
    } else if package_json_text.contains("\"vite\"") {
        "vite".to_string()
    } else {
        "node".to_string()
    }
}

fn discover_local_services(
    estate_root: &Path,
    declared: Option<&[ecompose::Service]>,
    project_dir: &Path,
    project: &str,
) -> Vec<LocalService> {
    let mut services = Vec::new();
    let top_level = util::sorted_dir_entries(estate_root);
    for entry in top_level {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let label = entry.file_name().to_string_lossy().to_string();
        scan_local_dir(&path, &label, "", &mut services);
    }

    let Some(declared) = declared else {
        return services;
    };
    if declared.is_empty() {
        return services;
    }
    let allowed = estate_service_dirs(declared, estate_root, project_dir, project);
    services
        .into_iter()
        .filter(|s| {
            let canonical = Path::new(&s.dir).canonicalize().unwrap_or_else(|_| PathBuf::from(&s.dir));
            allowed.contains(&canonical)
        })
        .collect()
}

fn scan_local_dir(scan_path: &Path, label: &str, rel_path: &str, services: &mut Vec<LocalService>) {
    let pom = scan_path.join("pom.xml");
    let cargo = scan_path.join("Cargo.toml");
    let go_mod = scan_path.join("go.mod");
    let pkg = scan_path.join("package.json");

    let name = if rel_path.is_empty() {
        label.to_string()
    } else {
        format!("{label}-{}", rel_path.replace('/', "-"))
    };

    if pom.is_file() {
        services.push(LocalService { name, r#type: "spring-boot".to_string(), dir: scan_path.display().to_string() });
        return;
    }
    if cargo.is_file() {
        services.push(LocalService { name, r#type: "rust".to_string(), dir: scan_path.display().to_string() });
        return;
    }
    if go_mod.is_file() {
        services.push(LocalService { name, r#type: "go".to_string(), dir: scan_path.display().to_string() });
        return;
    }
    if pkg.is_file() {
        if let Ok(package_json) = std::fs::read_to_string(&pkg) {
            services.push(LocalService {
                name,
                r#type: detect_service_type_local(&package_json),
                dir: scan_path.display().to_string(),
            });
        }
        return;
    }

    let entries = util::sorted_dir_entries(scan_path);
    for entry in entries {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let child_name = entry.file_name().to_string_lossy().to_string();
        if ["node_modules", "target", ".next", ".git"].contains(&child_name.as_str()) {
            continue;
        }
        let child_rel = if rel_path.is_empty() { child_name.clone() } else { format!("{rel_path}/{child_name}") };
        scan_local_dir(&path, label, &child_rel, services);
    }
}

fn estate_service_dirs(
    declared_services: &[ecompose::Service],
    estate_root: &Path,
    project_dir: &Path,
    project: &str,
) -> std::collections::HashSet<PathBuf> {
    let mut dirs = std::collections::HashSet::new();
    dirs.insert(std::fs::canonicalize(project_dir).unwrap_or_else(|_| project_dir.to_path_buf()));
    for service in declared_services {
        if service.path.is_empty() {
            continue;
        }
        let relative = relative_ct_service_path(&service.path, project, &project_dir.display().to_string(), "");
        let relative = if relative.is_empty() { service.path.clone() } else { relative };
        dirs.insert(std::fs::canonicalize(estate_root.join(&relative)).unwrap_or_else(|_| estate_root.join(&relative)));
        dirs.insert(std::fs::canonicalize(project_dir.join(&relative)).unwrap_or_else(|_| project_dir.join(&relative)));
    }
    dirs
}

fn ensure_local_domain_repos(
    estate_root: &Path,
    domains: &[String],
    project: &str,
    domain_branch_overrides: &HashMap<String, String>,
    content: &str,
) -> Result<(), String> {
    for domain in domains {
        if domain == project {
            continue;
        }
        let target_path = estate_root.join(domain);
        if target_path.join(".git").exists() {
            continue;
        }
        let (git, repo_branch) = resolve_domain_git(domain, project, content)?
            .ok_or_else(|| {
                format!(
                    "No git remote found for domain \"{domain}\" (no composition: block in ecompose.yml, and no git source)"
                )
            })?;
        if target_path.exists() {
            return Err(format!("Refusing to clone {domain} into existing non-git path: {}", target_path.display()));
        }
        let branch = domain_branch_overrides
            .get(domain)
            .cloned()
            .unwrap_or_else(|| repo_branch.clone());
        let branch_note = if branch != repo_branch {
            format!(" (branch override: {branch})")
        } else {
            String::new()
        };
        print_step(&format!("Cloning repo: {domain}{branch_note}"));
        run_command("git", &["clone".to_string(), "--branch".to_string(), branch, git, target_path.display().to_string()], estate_root)?;
    }
    Ok(())
}

fn install_local_dependencies(services: &[LocalService]) -> Result<(), String> {
    for service in services {
        if !["nextjs", "vite", "node"].contains(&service.r#type.as_str()) {
            continue;
        }
        let service_dir = Path::new(&service.dir);
        let package_json_path = service_dir.join("package.json");
        if !package_json_path.exists() {
            continue;
        }
        print_step(&format!("Installing npm dependencies: {}", service.name));
        let install_result = run_capture("npm", &["install".to_string()], service_dir)?;
        if install_result.code != 0 {
            if is_peer_dependency_resolution_error(&install_result) {
                print_step(&format!("Retrying npm install with --legacy-peer-deps: {}", service.name));
                run_command("npm", &["install".to_string(), "--legacy-peer-deps".to_string()], service_dir)?;
            } else {
                return Err(format!("npm install failed for {}", service.name));
            }
        }
    }
    Ok(())
}

fn local_postgres_client() -> Result<String, String> {
    let on_path = run_capture("which", &["psql".to_string()], &util::current_dir())?;
    if on_path.code == 0 && !on_path.stdout.trim().is_empty() {
        return Ok(on_path.stdout.trim().to_string());
    }
    for candidate in [
        "/Applications/Postgres.app/Contents/Versions/15/bin/psql",
        "/Applications/Postgres.app/Contents/Versions/latest/bin/psql",
    ] {
        if Path::new(candidate).exists() {
            return Ok(candidate.to_string());
        }
    }
    Err("PostgreSQL is declared in ecompose.yml but psql was not found. Run `eco provision` first.".to_string())
}

fn is_placeholder_database_url(value: &str) -> bool {
    value.is_empty()
        || value.contains('<')
        || value.contains('>')
        || value.to_lowercase().contains("todo")
        || value.to_lowercase().contains("example")
        || value.to_lowercase().contains("your")
        || value.to_lowercase().contains("password")
}

fn write_local_database_url(env_file: &Path, database_url: &str) -> Result<bool, String> {
    let content = std::fs::read_to_string(env_file).unwrap_or_default();
    let existing = content
        .split('\n')
        .find_map(|l| l.strip_prefix("DATABASE_URL=").map(|v| v.trim().to_string()));
    if let Some(existing) = existing {
        if !is_placeholder_database_url(&existing) {
            return Ok(false);
        }
    }
    let next_line = format!("DATABASE_URL={database_url}");
    let next_content = if let Some(_) = content.split('\n').find(|l| l.starts_with("DATABASE_URL=")) {
        let mut lines: Vec<String> = content.split('\n').map(|s| s.to_string()).collect();
        for l in lines.iter_mut() {
            if l.starts_with("DATABASE_URL=") {
                *l = next_line.clone();
            }
        }
        lines.join("\n")
    } else {
        let suffix = if content.is_empty() || content.ends_with('\n') { "" } else { "\n" };
        format!("{content}{suffix}{next_line}\n")
    };
    std::fs::write(env_file, next_content).map_err(|e| e.to_string())?;
    Ok(true)
}

fn bootstrap_local_postgres(services: &[ecompose::Service], estate_root: &Path, project: &str) -> Result<(), String> {
    let sql_services: Vec<&ecompose::Service> = services.iter().filter(|s| s.runtimes.iter().any(|r| r == "postgresql@15")).collect();
    if sql_services.is_empty() {
        return Ok(());
    }
    let psql = local_postgres_client()?;
    let auth_args: Vec<String> = ["-h".to_string(), "localhost".to_string(), "-d".to_string(), "postgres".to_string(), "-Atqc".to_string()].to_vec();
    let current_user = run_capture(&psql, &auth_args.iter().cloned().chain(vec!["SELECT current_user".to_string()]).collect::<Vec<_>>(), &util::current_dir())?;
    if current_user.code != 0 || current_user.stdout.trim().is_empty() {
        return Err("Could not connect to local PostgreSQL. Start PostgreSQL and configure a local role, then rerun `eco up`.".to_string());
    }
    let username = current_user.stdout.trim().to_string();

    for service in sql_services {
        let db_name = sql_database_name_for_service(service, project);
        if !db_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(format!("Unsafe generated PostgreSQL database name: {db_name}"));
        }
        let exists = run_capture(&psql, &auth_args.iter().cloned().chain(vec![format!("SELECT 1 FROM pg_database WHERE datname = '{db_name}'")]).collect::<Vec<_>>(), &util::current_dir())?;
        if exists.code != 0 {
            return Err(format!("Could not inspect local PostgreSQL database {db_name}."));
        }
        if exists.stdout.trim().is_empty() {
            run_command(&psql, &["-h".to_string(), "localhost".to_string(), "-d".to_string(), "postgres".to_string(), "-v".to_string(), "ON_ERROR_STOP=1".to_string(), "-c".to_string(), format!("CREATE DATABASE \"{db_name}\"")], &util::current_dir())?;
            print_step(&format!("Created local PostgreSQL database {db_name}"));
        }
        let env_file = estate_root.join(&service.path).join(".env");
        let database_url = format!("postgresql://{}@localhost:5432/{db_name}", url::form_urlencoded::byte_serialize(username.as_bytes()).collect::<String>());
        if write_local_database_url(&env_file, &database_url)? {
            print_step(&format!("Configured DATABASE_URL for {}", service.name));
        }
    }
    Ok(())
}

fn cargo_run_env() -> HashMap<String, String> {
    let mut env_map: HashMap<String, String> = std::env::vars().collect();
    let mut path_entries: Vec<String> = env_map
        .get("PATH")
        .map(|p| p.split(':').filter(|s| !s.is_empty()).map(|s| s.to_string()).collect())
        .unwrap_or_default();
    let home = util::home_dir();
    for candidate in [format!("{home}/.cargo/bin"), "/usr/local/cargo/bin".to_string()] {
        if !path_entries.contains(&candidate) {
            path_entries.push(candidate);
        }
    }
    env_map.insert("PATH".to_string(), path_entries.join(":"));
    env_map
}

fn run_local_rust_migrations(services: &[ecompose::Service], estate_root: &Path) -> Result<(), String> {
    let rust_sql: Vec<&ecompose::Service> = services
        .iter()
        .filter(|s| s.runtimes.iter().any(|r| r == "rust") && s.runtimes.iter().any(|r| r == "postgresql@15"))
        .collect();
    for service in rust_sql {
        let service_dir = estate_root.join(&service.path);
        let migrations_dir = service_dir.join("migrations");
        if !migrations_dir.exists() {
            continue;
        }
        let env_content = std::fs::read_to_string(service_dir.join(".env")).unwrap_or_default();
        let database_url = env_content
            .split('\n')
            .find_map(|l| l.strip_prefix("DATABASE_URL=").map(|v| v.trim().to_string()))
            .ok_or_else(|| format!("DATABASE_URL is required to run migrations for {}.", service.name))?;

        let sqlx = run_capture("which", &["sqlx".to_string()], &util::current_dir())?;
        if sqlx.code != 0 {
            print_step("Installing sqlx-cli for Rust migrations");
            run_command_env(
                "cargo",
                &["install".to_string(), "sqlx-cli".to_string(), "--no-default-features".to_string(), "--features".to_string(), "postgres,rustls".to_string()],
                &util::current_dir(),
                &cargo_run_env().into_iter().collect::<Vec<_>>(),
            )?;
        }
        print_step(&format!("Running Rust migrations: {}", service.name));
        let env_map: HashMap<String, String> = std::env::vars().collect();
        let mut run_env = env_map.clone();
        run_env.insert("DATABASE_URL".to_string(), database_url);
        run_command_env("sqlx", &["migrate".to_string(), "run".to_string(), "--source".to_string(), "migrations".to_string()], &service_dir, &run_env.into_iter().collect::<Vec<_>>())?;
    }
    Ok(())
}

fn find_rust_artifact(service_dir: &Path, package_name: &str) -> Result<Option<(String, i64, String)>, String> {
    let metadata_result = run_capture("cargo", &["metadata".to_string(), "--no-deps".to_string(), "--format-version".to_string(), "1".to_string()], service_dir)?;
    if metadata_result.code == 0 {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&metadata_result.stdout) {
            if let Some(target_directory) = parsed.get("target_directory").and_then(|t| t.as_str()) {
                for profile in ["release", "debug"] {
                    let candidate = Path::new(target_directory).join(profile).join(package_name);
                    if candidate.is_file() {
                        let mtime = std::fs::metadata(&candidate).and_then(|m| m.modified()).map(|m| m.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)).unwrap_or(0);
                        let file_desc = run_capture("file", &[candidate.display().to_string()], service_dir).map(|r| r.stdout).unwrap_or_default();
                        return Ok(Some((candidate.display().to_string(), mtime, file_desc)));
                    }
                }
                return Ok(None);
            }
        }
    }
    let candidates = [
        service_dir.join("target").join("release").join(package_name),
        service_dir.parent().map(|p| p.join("target").join("release").join(package_name)).unwrap_or_default(),
        service_dir.parent().and_then(|p| p.parent()).map(|p| p.join("target").join("release").join(package_name)).unwrap_or_default(),
        service_dir.join("target").join("debug").join(package_name),
        service_dir.parent().map(|p| p.join("target").join("debug").join(package_name)).unwrap_or_default(),
        service_dir.parent().and_then(|p| p.parent()).map(|p| p.join("target").join("debug").join(package_name)).unwrap_or_default(),
    ];
    for candidate in candidates {
        if candidate.is_file() {
            let mtime = std::fs::metadata(&candidate).and_then(|m| m.modified()).map(|m| m.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)).unwrap_or(0);
            let file_desc = run_capture("file", &[candidate.display().to_string()], service_dir).map(|r| r.stdout).unwrap_or_default();
            return Ok(Some((candidate.display().to_string(), mtime, file_desc)));
        }
    }
    Ok(None)
}

fn newest_rust_input_mtime(directory: &Path) -> i64 {
    let mut newest = 0i64;
    fn scan(scan_dir: &Path, newest: &mut i64) {
        let entries: Vec<std::fs::DirEntry> = std::fs::read_dir(scan_dir)
            .map(|e| e.flatten().collect())
            .unwrap_or_default();
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !["target", "node_modules", ".git"].contains(&name.as_str()) {
                    scan(&path, newest);
                }
            } else if path.is_file() {
                if let Ok(meta) = std::fs::metadata(&path) {
                    if let Ok(modified) = meta.modified() {
                        if let Ok(d) = modified.duration_since(std::time::UNIX_EPOCH) {
                            let ms = d.as_millis() as i64;
                            if ms > *newest {
                                *newest = ms;
                            }
                        }
                    }
                }
            }
        }
    }
    scan(directory, &mut newest);
    newest
}

fn rust_build_state(service_dir: &Path) -> Result<(bool, String, bool), String> {
    let cargo_toml = std::fs::read_to_string(service_dir.join("Cargo.toml")).unwrap_or_default();
    if cargo_toml.is_empty() {
        return Ok((false, "no Cargo.toml".to_string(), false));
    }
    let package_name = cargo_package_name(&cargo_toml);
    let Some(package_name) = package_name else {
        return Ok((true, "unknown package binary".to_string(), false));
    };
    let artifact = find_rust_artifact(service_dir, &package_name)?;
    let Some((artifact_path, artifact_mtime, file_desc)) = artifact else {
        return Ok((true, "binary is missing".to_string(), false));
    };
    if util::platform() == "darwin" && file_desc.contains("Mach-O") {
        let expected_arch = if util::arch() == "x64" { "x86_64".to_string() } else { util::arch() };
        if !file_desc.contains(&expected_arch) {
            return Ok((true, "binary has a non-native architecture".to_string(), true));
        }
    }
    let mut newest_input = newest_rust_input_mtime(service_dir);
    for directory in [service_dir.parent().map(|p| p.to_path_buf()).unwrap_or_default(), service_dir.parent().and_then(|p| p.parent()).map(|p| p.to_path_buf()).unwrap_or_default()] {
        for filename in ["Cargo.toml", "Cargo.lock"] {
            let input_path = directory.join(filename);
            if input_path.is_file() {
                if let Ok(meta) = std::fs::metadata(&input_path) {
                    if let Ok(modified) = meta.modified() {
                        if let Ok(d) = modified.duration_since(std::time::UNIX_EPOCH) {
                            let ms = d.as_millis() as i64;
                            if ms > newest_input {
                                newest_input = ms;
                            }
                        }
                    }
                }
            }
        }
    }
    if newest_input > artifact_mtime {
        return Ok((true, "source is newer than the binary".to_string(), false));
    }
    let _ = artifact_path;
    Ok((false, "native binary is current".to_string(), false))
}

fn build_local_rust_services(services: &[LocalService]) -> Result<(), String> {
    for service in services.iter().filter(|s| s.r#type == "rust") {
        let service_dir = Path::new(&service.dir);
        if !service_dir.join("Cargo.toml").exists() {
            continue;
        }
        let (needs_build, reason, clean) = rust_build_state(service_dir)?;
        if !needs_build {
            print_step(&format!("Rust service is current; skipping build: {}", service.name));
            continue;
        }
        if reason == "unknown package binary" {
            print_step(&format!("Skipping Rust build for {}: no [package] binary (virtual workspace or malformed manifest)", service.name));
            continue;
        }
        if clean {
            print_step(&format!("Removing non-native Rust build cache: {}", service.name));
            run_command_env("cargo", &["clean".to_string()], service_dir, &cargo_run_env().into_iter().collect::<Vec<_>>())?;
        }
        print_step(&format!("Building Rust service ({reason}): {}", service.name));
        run_command_env("cargo", &["build".to_string()], service_dir, &cargo_run_env().into_iter().collect::<Vec<_>>())?;
    }
    Ok(())
}

fn derive_staging_ecompose_content(content: &str, staging_config: &HashMap<String, String>, staging_hostname: &str) -> String {
    let mut rewritten: Vec<String> = Vec::new();
    let mut in_ct = false;
    let mut in_expose = false;
    for raw in content.split('\n') {
        let line = raw.trim_end_matches('\r').trim_end().to_string();
        if line == "ct:" {
            in_ct = true;
            rewritten.push(line);
            continue;
        }
        if line == "expose:" {
            in_expose = true;
            rewritten.push(line);
            continue;
        }
        if in_ct && !line.starts_with("  ") {
            in_ct = false;
        }
        if in_expose && !line.starts_with("  ") {
            in_expose = false;
        }
        if in_ct {
            if let Some(rest) = line.strip_prefix("  id:") {
                let ct_val = staging_config.get("ct").cloned().unwrap_or_default();
                rewritten.push(format!("  id: {ct_val}"));
                continue;
            }
            rewritten.push(line);
            continue;
        }
        if in_expose {
            if let Some(_) = line.strip_prefix("  hostname:") {
                rewritten.push(format!("  hostname: {staging_hostname}"));
                continue;
            }
            rewritten.push(line);
            continue;
        }
        rewritten.push(line);
    }
    let base = rewritten.join("\n");
    format!(
        "# staging footprint: deployed to CT {} at {staging_hostname}\n# All other settings mirror the prod manifest from which this was derived.\n{base}\n",
        staging_config.get("ct").cloned().unwrap_or_default()
    )
}

fn provision_estate(
    deployment: &ProjectDeployment,
    options: &HashMap<String, String>,
    staging: bool,
    staging_config: &HashMap<String, String>,
) -> Result<(), String> {
    let ctid = if staging {
        staging_config.get("ct").cloned().unwrap_or_default()
    } else {
        deployment.ctid.clone()
    };
    let mut ct = deployment.ct.clone();
    if staging {
        ct.insert("id".to_string(), ctid.clone());
        ct.insert("hostname".to_string(), format!("{}-staging", deployment.project));
    }
    let expose = if staging {
        let mut e = deployment.expose.clone();
        e.map.insert("hostname".to_string(), {
            staging_config
                .get("hostname")
                .cloned()
                .unwrap_or_else(|| derive_staging_hostname(&deployment.expose.hostname()))
        });
        e.additional.clear();
        e
    } else {
        deployment.expose.clone()
    };
    let create_args = create_pct_args(&deployment.project, &ct, &HashMap::new());
    let staging_ecompose_content = if staging {
        derive_staging_ecompose_content(&deployment.content, staging_config, &expose.hostname())
    } else {
        String::new()
    };
    let staging_ecompose_path = format!("{}/ecompose.yml", deployment.ct_project_root);
    let domains = ecompose::unique_domains_from_ecompose(&deployment.content, &deployment.project);
    let host_ssh_dir = resolve_host_ssh_dir()?;

    // Services whose dist was built on the local builder and shipped in the
    // payload (dirs under remote-artifacts) — the CT must NOT npm ci/build
    // them; it serves the shipped artifact.
    let shipped_frontends: std::collections::HashSet<String> = std::collections::HashSet::new();
    // The estate core repo name (e.g. `stuff8_core`), used to strip service
    // path prefixes that reference the estate core on flattened host layouts.
    let estate_core = estate_core_name(&deployment.content);

    let mut dependency_install_steps: Vec<(String, String)> = deployment
        .services
        .iter()
        .filter(|s| !s.path.is_empty())
        .filter(|s| s.runtimes.iter().any(|r| r == "npm" || r.starts_with("node@")))
        .map(|s| {
            let service_dir = resolve_ct_service_dir(s, &deployment.ct_project_root, &deployment.project_dir.display().to_string(), &estate_core);
            (s.name.clone(), build_npm_install_command(&service_dir))
        })
        .collect();

    let mut build_steps: Vec<(String, String)> = Vec::new();
    for service in deployment.services.iter().filter(|s| !s.path.is_empty()) {
        let service_dir = resolve_ct_service_dir(service, &deployment.ct_project_root, &deployment.project_dir.display().to_string(), &estate_core);
        if service.runtimes.iter().any(|r| r == "npm" || r.starts_with("node@")) {
            if shipped_frontends.contains(&service.name) {
                continue;
            }
            build_steps.push((
                service.name.clone(),
                format!(
                    "if [ -f \"{service_dir}/package.json\" ]; then cd \"{service_dir}\" && if [ -f \".env\" ]; then set -a && . ./.env && set +a; fi && ECO_DEPLOY_MODE=prod npm run build --if-present; fi"
                ),
            ));
        }
        if service.runtimes.iter().any(|r| r == "maven") {
            build_steps.push((
                service.name.clone(),
                format!("if [ -f \"{service_dir}/pom.xml\" ]; then cd \"{service_dir}\" && mvn -DskipTests package; fi"),
            ));
        }
    }

    let data_bootstrap_steps = build_data_bootstrap_plan(&deployment.services, &deployment.ct_workspace_root, &deployment.ct_project_root, &deployment.project_dir.display().to_string(), &deployment.project, &estate_core);
    let migration_steps = build_rust_migration_plan(&deployment.services, &deployment.ct_workspace_root, &deployment.ct_project_root, &deployment.project_dir.display().to_string(), &estate_core);
    let rust_services: Vec<&ecompose::Service> = deployment
        .services
        .iter()
        .filter(|s| !s.path.is_empty() && s.runtimes.iter().any(|r| r == "rust"))
        .collect();

    // Remote mode (`eco up --remote`): the Rust binaries were cross-compiled on
    // the developer machine and shipped here inside the deploy payload. The dev
    // source is authoritative, so git force-sync / domain clones are skipped
    // and the CT never compiles Rust; the shipped binaries are installed into
    // target/release and the source hash is recorded so later builds skip.
    let remote_mode = options.get("remote").map(|v| v == "true").unwrap_or(false);
    let remote_artifacts = options.get("remote-artifacts").cloned().unwrap_or_default();
    let remote_hashes = options.get("remote-hashes").cloned().unwrap_or_default();
    let remote_frontend_hashes = options.get("remote-frontend-hashes").cloned().unwrap_or_default();
    // Frontends shipped as prebuilt dist (dirs under remote-artifacts) skip the
    // CT-side `npm run build`; the dist was built on the local builder.
    let shipped_frontends: std::collections::HashSet<String> = if remote_mode && !remote_artifacts.is_empty() {
        std::fs::read_dir(&remote_artifacts)
            .map(|rd| rd.flatten().filter(|e| e.path().is_dir()).map(|e| e.file_name().to_string_lossy().to_string()).collect())
            .unwrap_or_default()
    } else {
        std::collections::HashSet::new()
    };
    if !shipped_frontends.is_empty() {
        build_steps.retain(|(name, _)| !shipped_frontends.contains(name));
        print_step(&format!("[{}] skipping CT-side frontend builds for shipped dists: {:?}", deployment.project, shipped_frontends));
    }
    // Bun-compiled node backends are self-contained single binaries — no npm
    // install (runtime node_modules) on the CT at all.
    let bun_frontends: std::collections::HashSet<String> = if remote_mode && !remote_artifacts.is_empty() {
        std::fs::read_dir(&remote_artifacts)
            .map(|rd| {
                rd.flatten()
                    .filter(|e| e.path().is_dir() && e.path().join(".eco-bun").is_file())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default()
    } else {
        std::collections::HashSet::new()
    };
    if !bun_frontends.is_empty() {
        dependency_install_steps.retain(|(name, _)| !bun_frontends.contains(name));
        print_step(&format!("[{}] Bun-compiled services skip CT npm ci: {:?}", deployment.project, bun_frontends));
    }

    // Rust is built off-CT only: binaries are supplied by `eco up --remote`
    // (cross-compiled on the dev machine) or an LXS package. A plain in-CT
    // `cargo build` is no longer supported -- fail fast instead of silently
    // compiling from source.
    if !rust_services.is_empty() && !remote_mode {
        return Err(format!(
            "{} declares Rust service(s) [{}] but no Rust binary source is configured. Rust must be supplied as prebuilt binaries: run `eco up --remote` or use an `lxs:` service. In-CT `cargo build` is not supported.",
            deployment.project,
            rust_services.iter().map(|s| s.name.clone()).collect::<Vec<_>>().join(", ")
        ));
    }

    let mut commands: Vec<String> = vec![
        format!("pct status {ctid} || pct create {}", create_args[1..].join(" ")),
        format!("pct start {ctid}"),
        format!("pct exec {ctid} -- bash -lc 'mkdir -p {}'", deployment.ct_workspace_root),
        format!("pct push {ctid} <temp-tar:project:{}> /tmp/{}.tar", deployment.project, deployment.project),
        format!("pct push {ctid} <temp-tar:eco> /tmp/eco.tar"),
        format!("pct push {ctid} <temp-tar:.ssh:{}> /tmp/.ssh.tar", host_ssh_dir.display()),
        format!(
            "pct exec {ctid} -- bash -lc 'cd {} && tar -xf /tmp/{}.tar && tar -xf /tmp/eco.tar && rm -f /tmp/{}.tar /tmp/eco.tar && mkdir -p /root && tar -xf /tmp/.ssh.tar -C /root && rm -f /tmp/.ssh.tar && chmod 700 /root/.ssh && find /root/.ssh -type d -exec chmod 700 {} \\; && find /root/.ssh -type f -exec chmod 600 {} \\;'",
            deployment.ct_workspace_root,
            deployment.project,
            deployment.project,
            "{}",
            "{}"
        ),
    ];
    if staging {
        commands.push(format!(
            "# write staging ecompose.yml (staging hostname + ct) after bootstrap sync restores the prod manifest\npct push {ctid} <staging-ecompose> {staging_ecompose_path}"
        ));
    }
    commands.push(format!(
        "pct exec {ctid} -- bash -lc 'cd {} && bash {}/provision.sh {}'",
        deployment.ct_workspace_root,
        deployment.ct_eco_root,
        deployment.project
    ));
    if expose.enabled() {
        commands.push(format!(
            "pct exec {ctid} -- bash -lc 'if ! command -v caddy >/dev/null 2>&1; then apt-get update && apt-get install -y caddy; fi'"
        ));
    }
    for (name, command) in &dependency_install_steps {
        commands.push(format!("# install deps: {name}\npct exec {ctid} -- bash -lc '{}'", command));
    }
    for (index, command) in data_bootstrap_steps.iter().enumerate() {
        commands.push(format!("# bootstrap data service #{}\npct exec {ctid} -- bash -lc '{}'", index + 1, command));
    }
    for (index, command) in migration_steps.iter().enumerate() {
        commands.push(format!("# apply Rust migration set #{}\npct exec {ctid} -- bash -lc '{}'", index + 1, command));
    }
    commands.push(format!("pct exec {ctid} -- bash -lc 'mkdir -p /usr/local/bin && install -m 0755 {}/bin/eco /usr/local/bin/eco && export ECO_BIN=/usr/local/bin/eco'", shell_single_quote(&deployment.ct_eco_root)));
    // Refresh the CT's configure.sh from the shipped binary before running it
    // (in remote mode the workspace copy can be stale).
    commands.push(format!(
        "pct exec {ctid} -- /usr/local/bin/eco __bundle-configure-sh {}",
        shell_single_quote(&format!("{}/configure.sh", deployment.ct_eco_root))
    ));
    commands.push(format!(
        "pct exec {ctid} -- bash -lc 'cd {} && ECO_BIN=/usr/local/bin/eco ECO_DEPLOY_MODE=prod ECO_NON_INTERACTIVE=1 {} PROJECT_DIR={} PROJECT_NAME={} PM2_DIR={} bash {}/configure.sh'",
        deployment.ct_workspace_root,
        systemd_env(),
        deployment.ct_project_root,
        deployment.project,
        deployment.ct_project_root,
        deployment.ct_eco_root
    ));
    for (name, command) in &build_steps {
        commands.push(format!("# build artifact: {name}\npct exec {ctid} -- bash -lc '{}'", command));
    }
    commands.push(format!(
        "# remove the estate's current PM2 services, then prune any old process that still holds a configured port\npct exec {ctid} -- bash -lc '{}'",
        build_delete_declared_pm2_apps_command(&deployment.ct_config_path)
    ));
    let start_services_cmd = if systemd_mode() {
        format!("pct exec {ctid} -- bash -lc '{}'", build_systemd_start_command(&deployment.ct_config_path))
    } else {
        format!("pct exec {ctid} -- bash -lc 'pm2 startOrReload {} --update-env'", deployment.ct_config_path)
    };
    commands.push(start_services_cmd);

    let exposure_plan = expose_via_proxy_ct(true, &expose, &deployment.project, &ctid, &deployment.ct_config_path)
        .unwrap_or_else(|e| {
            if expose.enabled() {
                vec![format!("# expose error: {e}")]
            } else {
                Vec::new()
            }
        });

    if options.get("dry-run").map(|v| v == "true").unwrap_or(false) {
        let mut out = String::new();
        out.push_str(&format!("eco up plan{}\n", if staging { " (staging)" } else { "" }));
        out.push_str(&format!("Manifest: {}\n", deployment.file_path));
        out.push_str(&format!("Project root: {}\n", deployment.project_dir.display()));
        out.push_str(&format!("CT workspace root: {}\n", deployment.ct_workspace_root));
        out.push_str(&format!("CT ID: {ctid}\n"));
        out.push_str(&format!("Hostname: {}\n", if expose.hostname().is_empty() { "(none)".to_string() } else { expose.hostname() }));
        out.push_str(&format!("Domains: {}\n\n", domains.join(", ")));
        for c in &commands {
            out.push_str(c);
            out.push('\n');
        }
        for c in &exposure_plan {
            out.push_str(c);
            out.push('\n');
        }
        print!("{out}");
        return Ok(());
    }

    let status = run_capture("pct", &["status".to_string(), ctid.clone()], &util::current_dir())?;
    if status.code != 0 {
        let available_template = resolve_available_template(ct.get("template").map(|s| s.as_str()).unwrap_or(""))?;
        let resolved_create_args = if available_template == ct.get("template").map(|s| s.to_string()).unwrap_or_default() {
            create_args.clone()
        } else {
            let mut opts = HashMap::new();
            opts.insert("template".to_string(), available_template);
            create_pct_args(&deployment.project, &ct, &opts)
        };
        run_command("pct", &resolved_create_args, &util::current_dir())?;
    }

    ensure_ct_running(&ctid)?;
    print_step(&format!("CT {ctid} is running"));
    let minio = provision_dedicated_minio(&deployment.storage, &ctid)?;
    if let Some(minio) = &minio {
        print_step(&format!("[CT {}] MinIO is ready at its private bridge endpoint", minio.ctid));
        install_minio_client_config(&ctid, minio)?;
    }
    pct_exec(&ctid, &format!("mkdir -p {}", deployment.ct_workspace_root))?;
    print_step(&format!("[CT {ctid}] Pushing project repo: {}", deployment.project));
    tar_and_push_dir(&ctid, &deployment.project_dir.display().to_string(), &deployment.project)?;
    print_step(&format!("[CT {ctid}] Pushing eco repo"));
    tar_and_push_dir(&ctid, &embedded::package_root().display().to_string(), "eco")?;
    print_step(&format!("[CT {ctid}] Copying host SSH credentials"));
    tar_and_push_dir(&ctid, &host_ssh_dir.display().to_string(), ".ssh")?;
    pct_exec(
        &ctid,
        &format!(
            "cd {} && tar -xf /tmp/{}.tar && tar -xf /tmp/eco.tar && rm -f /tmp/{}.tar /tmp/eco.tar && mkdir -p /root && tar -xf /tmp/.ssh.tar -C /root && rm -f /tmp/.ssh.tar && chmod 700 /root/.ssh && find /root/.ssh -type d -exec chmod 700 {{}} \\; && find /root/.ssh -type f -exec chmod 600 {{}} \\;",
            deployment.ct_workspace_root,
            deployment.project,
            deployment.project
        ),
    )?;
    if remote_mode {
        // Remote deploys skip git force-sync (the shipped source is
        // authoritative), so stale top-level files from an earlier deploy
        // layout survive the tar merge. Remove them the way `git clean` would:
        // anything on the CT that is not in the shipped source, preserving only
        // eco-generated state (.eco, target/, the PM2 config, Caddyfile).
        print_step(&format!("[CT {ctid}] Removing stale source files not present in the shipped source"));
        remove_stale_ct_source(&ctid, deployment)?;
    }
    if staging {
        print_step(&format!("[CT {ctid}] Writing staging ecompose.yml ({})", expose.hostname()));
        push_text_file_to_ct(&ctid, &staging_ecompose_path, &staging_ecompose_content, "staging-ecompose")?;
    }
    print_step(&format!("[CT {ctid}] Provisioning runtimes for {}", deployment.project));
    // Refresh the CT's bundled scripts from the shipped binary first, so
    // provision.sh (and configure.sh) are the current versions. The CT's own
    // /usr/local/bin/eco may be stale (no __bundle-scripts), so use the
    // agent's binary pushed in.
    pct_exec(&ctid, "mkdir -p /usr/local/bin")?;
    run_command(
        "pct",
        &["push".to_string(), ctid.to_string(), "/usr/local/bin/eco".to_string(), "/tmp/eco-bundle-bin".to_string()],
        &util::current_dir(),
    )?;
    pct_exec(&ctid, &format!("chmod +x /tmp/eco-bundle-bin && /tmp/eco-bundle-bin __bundle-scripts {} && rm -f /tmp/eco-bundle-bin", shell_single_quote(&deployment.ct_eco_root)))?;
    pct_exec(
        &ctid,
        &format!(
            "cd {} && bash {}/provision.sh {}",
            deployment.ct_workspace_root,
            deployment.ct_eco_root,
            deployment.project
        ),
    )?;
    if expose.enabled() {
        print_step(&format!("[CT {ctid}] Ensuring caddy is installed for gateway"));
        ensure_ct_caddy(&ctid)?;
    }
    for (name, command) in &dependency_install_steps {
        print_step(&format!("[CT {ctid}] Installing npm dependencies: {name}"));
        pct_exec(&ctid, &format!("cd {} && {}", deployment.ct_workspace_root, command))?;
    }
    print_step(&format!("[CT {ctid}] Installing eco CLI"));
    // Install the CURRENT eco binary (the agent's own) so configure.sh and
    // __bundle-configure-sh are the shipped versions, not a stale CT copy.
    pct_exec(&ctid, "mkdir -p /usr/local/bin")?;
    run_command(
        "pct",
        &["push".to_string(), ctid.to_string(), "/usr/local/bin/eco".to_string(), "/tmp/eco-agent-bin".to_string()],
        &util::current_dir(),
    )?;
    pct_exec(&ctid, "install -m 0755 /tmp/eco-agent-bin /usr/local/bin/eco && rm -f /tmp/eco-agent-bin")?;
    for (index, command) in data_bootstrap_steps.iter().enumerate() {
        print_step(&format!("[CT {ctid}] Bootstrapping data service {}", index + 1));
        pct_exec(&ctid, &format!("export LANG=C.UTF-8 LC_ALL=C.UTF-8 PERL_BADLANG=0\n{}", command))?;
    }
    for (index, command) in migration_steps.iter().enumerate() {
        print_step(&format!("[CT {ctid}] Applying Rust migration set {}", index + 1));
        pct_exec(&ctid, &format!("{command}"))?;
    }
    // Build Rust services. The binaries were cross-compiled on the developer
    // machine and shipped in the deploy payload, so they are installed
    // directly. Runs BEFORE configure.sh so it detects the release binaries
    // and points PM2 at them instead of `cargo run`.
    let mut rust_build_failures: Vec<String> = Vec::new();
    if !rust_services.is_empty() {
        if remote_mode {
            print_step(&format!("[CT {ctid}] Installing remotely-built Rust binaries for {}", deployment.project));
            match install_remote_rust_binaries(&ctid, deployment, &remote_artifacts, &remote_hashes, &estate_core) {
                Ok(()) => print_step(&format!("[CT {ctid}] Remote Rust binaries installed for {}", deployment.project)),
                Err(e) => {
                    util::eprintln_stderr(&format!("\n[eco up] WARNING: Installing remote Rust binaries failed, continuing: {e}\n"));
                    rust_build_failures.push(e);
                }
            }
        }
    }
    // Frontend dists are shipped in the payload regardless of whether the
    // estate has any Rust services.
    if remote_mode {
        if let Err(e) = install_remote_frontend_artifacts(&ctid, deployment, &remote_artifacts, &remote_frontend_hashes, &estate_core) {
            util::eprintln_stderr(&format!("\n[eco up] WARNING: Installing shipped frontend dists failed, continuing: {e}\n"));
            rust_build_failures.push(e);
        }
    }
    if let Some(installed) = (|| -> Result<Option<Vec<String>>, String> {
        if deployment.services.iter().any(|s| !s.lxs.is_empty()) {
            print_step(&format!("[CT {ctid}] Installing LXS services for {}", deployment.project));
            return Ok(Some(install_lxs_services(&ctid, deployment)?));
        }
        Ok(None)
    })()? {
        if installed.is_empty() {
            util::eprintln_stderr("[eco up] WARNING: no LXS installed for declared lxs: services");
        }
    }
    print_step(&format!("[CT {ctid}] Generating ecosystem config for {}", deployment.project));
    // Refresh configure.sh from the shipped binary first (remote mode ships a
    // stale workspace copy otherwise).
    pct_exec(&ctid, &format!("/usr/local/bin/eco __bundle-configure-sh {}", shell_single_quote(&format!("{}/configure.sh", deployment.ct_eco_root))))?;
    pct_exec(
        &ctid,
        &format!(
            "cd {} && ECO_BIN=/usr/local/bin/eco ECO_DEPLOY_MODE=prod ECO_NON_INTERACTIVE=1 {} PROJECT_DIR={} PROJECT_NAME={} PM2_DIR={} bash {}/configure.sh",
            deployment.ct_workspace_root,
            systemd_env(),
            deployment.ct_project_root,
            deployment.project,
            deployment.ct_project_root,
            deployment.ct_eco_root
        ),
    )?;

    let mut failures: Vec<(String, String)> = Vec::new();
    for failure in rust_build_failures {
        failures.push(("Building Rust".to_string(), failure));
    }
    for (name, command) in &build_steps {
        match pct_exec(&ctid, &format!("cd {} && {}", deployment.ct_workspace_root, command)) {
            Ok(()) => print_step(&format!("[CT {ctid}] Building artifact: {name}")),
            Err(e) => {
                util::eprintln_stderr(&format!("\n[eco up] [CT {ctid}] WARNING: Building artifact {name} failed, continuing: {e}\n"));
                failures.push((format!("Building artifact: {name}"), e));
            }
        }
    }
    match (|| -> Result<(), String> {
        print_step(&format!("[CT {ctid}] Starting services for {} ({})", deployment.project, if systemd_mode() { "systemd" } else { "pm2" }));
        pct_exec(&ctid, &build_delete_declared_pm2_apps_command(&deployment.ct_config_path))?;
        pct_exec(&ctid, &build_prune_conflicting_ports_command(&deployment.ct_config_path))?;
        if systemd_mode() {
            pct_exec(&ctid, &build_systemd_start_command(&deployment.ct_config_path))
        } else {
            pct_exec(&ctid, &format!("pm2 startOrReload {} --update-env", deployment.ct_config_path))
        }
    })() {
        Ok(()) => {}
        Err(e) => {
            util::eprintln_stderr(&format!("\n[eco up] [CT {ctid}] WARNING: Starting services failed, continuing: {e}\n"));
            failures.push((format!("Starting services for {}", deployment.project), e));
        }
    }
    match expose_via_proxy_ct(false, &expose, &deployment.project, &ctid, &deployment.ct_config_path) {
        Ok(_) => {}
        Err(e) => {
            util::eprintln_stderr(&format!("\n[eco up] [CT {ctid}] WARNING: Exposing via proxy CT failed, continuing: {e}\n"));
            failures.push((format!("Exposing {} via proxy CT", deployment.project), e));
        }
    }

    if !failures.is_empty() {
        let mut message = format!("\n[eco up] Completed {} with {} failed step(s):", deployment.project, failures.len());
        for (label, error) in &failures {
            message.push_str(&format!("\n  - {label}: {error}"));
        }
        if remote_mode {
            // Running inside the `eco serve` agent: never kill the server
            // process. Return the failures as an error instead.
            message.push_str("\nEverything else succeeded, including exposing the estate if enabled.");
            return Err(message);
        }
        util::println_stdout(&message);
        util::println_stdout("\nEverything else succeeded, including exposing the estate if enabled. Fix the failed step(s) above and re-run 'eco up' (or the specific step manually) to clear them.");
        std::process::exit(1);
    } else {
        print_step(&format!("Completed {}", deployment.project));
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Remote deploy: the developer machine keeps the estate source locally,
// cross-compiles the Rust services for Linux (x86_64-unknown-linux-musl) with
// the correct env, and ships the binaries over HTTP to the `eco serve` agent
// on the Proxmox host. The agent installs them into the target CT and runs the
// estate deploy without compiling Rust there — removing the shared builder-CT
// contention that a single CT (e.g. CT 1000) creates when several estates build
// at once.
// ─────────────────────────────────────────────────────────────────────────────

const REMOTE_SOURCE_SKIP: [&str; 13] = [
    ".env", ".env.local", ".git", "node_modules", "target", ".next", ".cache", ".eco", "dist",
    "docs", "build", ".svelte-kit", "data-snapshots",
];

fn skip_none(_: &str) -> bool {
    false
}

// Only secret/local env files are excluded from the shipped source. `.env.example`
// is a committed contract file configure.sh reads on the CT to normalize env
// (JWT_SECRET, CORS, feature flags) and must be shipped.
fn should_skip_remote_source(name: &str) -> bool {
    REMOTE_SOURCE_SKIP.contains(&name)
        || name.starts_with("._")
        || (name.starts_with(".env.") && name.ends_with(".local"))
        || name.ends_with(".log")
}

// Best-effort gitignore awareness: read a dir's `.gitignore` and return the
// top-level entry names it ignores (comments, blanks and nested paths skipped).
// The remote payload ship skips gitignored content — "if it's gitignored, we
// don't ship it" — so a user who accidentally keeps large non-runtime files in
// the estate can gitignore them and they stop going to the CT.
fn load_gitignore_names(dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    if let Ok(content) = std::fs::read_to_string(dir.join(".gitignore")) {
        for line in content.lines() {
            let t = line.trim();
            if t.is_empty() || t.starts_with('#') || t.contains('/') {
                continue;
            }
            names.push(t.trim_end_matches('/').to_string());
        }
    }
    names
}

fn copy_tree_excluding(src: &Path, dst: &Path, skip: &dyn Fn(&str) -> bool) -> Result<(), String> {
    if !src.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().to_string();
        if skip(&name) {
            continue;
        }
        let source = entry.path();
        let destination = dst.join(&name);
        let file_type = entry.file_type().map_err(|e| e.to_string())?;
        if file_type.is_dir() {
            copy_tree_excluding(&source, &destination, skip)?;
        } else if file_type.is_symlink() {
            // Keep payloads self-contained: skip symlinks.
            continue;
        } else {
            std::fs::copy(&source, &destination).map_err(|e| format!("copy {}: {e}", source.display()))?;
        }
    }
    Ok(())
}

fn collect_rust_inputs(dir: &Path, out: &mut Vec<String>) -> Result<(), String> {
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();
        if path.is_dir() {
            if ["target", "node_modules", ".git"].contains(&name.as_str()) {
                continue;
            }
            collect_rust_inputs(&path, out)?;
        } else if name.ends_with(".rs") || name == "Cargo.toml" || name == "Cargo.lock" {
            out.push(path.display().to_string());
        }
    }
    Ok(())
}

// In remote mode the shipped source is authoritative and git force-sync is
// skipped, so a tar extract only merges/overwrites: stale top-level entries
// from an earlier deploy layout survive. Remove any top-level entry on the CT
// that is not in the shipped source, preserving eco-generated state.
fn remove_stale_ct_source(ctid: &str, deployment: &ProjectDeployment) -> Result<(), String> {
    let shipped: std::collections::HashSet<String> = std::fs::read_dir(&deployment.project_dir)
        .map_err(|e| e.to_string())?
        .filter_map(|entry| entry.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .collect();
    let current_output = pct_exec_capture(ctid, &format!("ls -1A {}", shell_single_quote(&deployment.ct_project_root)))?;
    let preserved = [".eco", "target", "node_modules", ".eco-rust-hash", &deployment.pm2_config_filename, "Caddyfile"];
    for entry in current_output.lines().map(|l| l.trim()).filter(|l| !l.is_empty()) {
        if shipped.contains(entry) || preserved.contains(&entry) || entry == "ecompose.yml" || entry == "ecompose.yaml" || entry.ends_with("-ecompose.yml") {
            continue;
        }
        print_step(&format!("[CT {ctid}] Removing stale top-level source entry: {entry}"));
        pct_exec(ctid, &format!("rm -rf {}", shell_single_quote(&format!("{}/{}", deployment.ct_project_root, entry))))?;
    }
    Ok(())
}

// Mirrors the hash produced by the CT-side `buildConditionalRustCommand` so the
// recorded `.eco-rust-hash` lets later deploys skip a rebuild when the source
// shipped to the CT is unchanged.
fn compute_rust_input_hash(service_dir: &Path) -> Result<String, String> {
    let mut inputs: Vec<String> = Vec::new();
    collect_rust_inputs(service_dir, &mut inputs)?;
    let ancestors = [
        service_dir.to_path_buf(),
        service_dir.parent().map(|p| p.to_path_buf()).unwrap_or_default(),
        service_dir.parent().and_then(|p| p.parent()).map(|p| p.to_path_buf()).unwrap_or_default(),
    ];
    for dir in ancestors {
        for manifest in ["Cargo.toml", "Cargo.lock"] {
            let path = dir.join(manifest);
            if path.is_file() {
                inputs.push(path.display().to_string());
            }
        }
    }
    inputs.sort();
    let mut combined = String::new();
    for path in &inputs {
        let bytes = std::fs::read(path).map_err(|e| format!("read {path}: {e}"))?;
        let digest = sha2::Sha256::digest(&bytes);
        combined.push_str(&format!("{}  {path}\n", crate::registry::hex_encode(&digest)));
    }
    let final_digest = sha2::Sha256::digest(combined.as_bytes());
    Ok(crate::registry::hex_encode(&final_digest))
}

// True when a crate uses the compile-time sqlx macros (which need a live DB or
// committed .sqlx/ offline metadata); runtime sqlx::query(&str) does not.
fn crate_uses_sqlx_query_macros(dir: &Path) -> bool {
    const MACRO_TOKENS: [&str; 7] = ["query!", "query_as!", "query_scalar!", "query_with!", "query_file!", "query_as_file!", "query_scalar_file!"];
    let src = dir.join("src");
    if !src.is_dir() {
        return false;
    }
    fn scan(dir: &Path, found: &mut bool) {
        if *found {
            return;
        }
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                if *found {
                    return;
                }
                let path = entry.path();
                if path.is_dir() {
                    scan(&path, found);
                } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        for token in MACRO_TOKENS {
                            if text.contains(token) {
                                *found = true;
                                return;
                            }
                        }
                    }
                }
            }
        }
    }
    let mut found = false;
    scan(&src, &mut found);
    found
}

fn resolve_cargo_target_dir(dir: &Path) -> Option<String> {
    let result = run_capture("cargo", &["metadata".to_string(), "--no-deps".to_string(), "--format-version".to_string(), "1".to_string()], dir).ok()?;
    if result.code != 0 {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(&result.stdout).ok()?;
    value.get("target_directory").and_then(|v| v.as_str()).map(|s| s.to_string())
}

// The workspace root that owns this crate, if any. Workspace members must be
// built with `cargo ... -p <package>` from this root, not from the member dir.
fn cargo_workspace_root(dir: &Path) -> Option<String> {
    let result = run_capture("cargo", &["metadata".to_string(), "--no-deps".to_string(), "--format-version".to_string(), "1".to_string()], dir).ok()?;
    if result.code != 0 {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(&result.stdout).ok()?;
    value.get("workspace_root").and_then(|v| v.as_str()).map(|s| s.to_string())
}

fn is_shell_ident(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn parse_env_line(line: &str) -> Option<(String, String)> {    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (key, value) = line.split_once('=')?;
    let key = key.trim().to_string();
    if key.is_empty() {
        return None;
    }
    let mut value = value.trim().to_string();
    if let Some(stripped) = value.strip_prefix('"') {
        value = stripped.strip_suffix('"').map(|s| s.to_string()).unwrap_or_else(|| stripped.to_string());
    } else if let Some(stripped) = value.strip_prefix('\'') {
        value = stripped.strip_suffix('\'').map(|s| s.to_string()).unwrap_or_else(|| stripped.to_string());
    }
    Some((key, value))
}

fn project_path_segment(value: &str) -> String {
    value.replace(' ', "%20")
}

fn agent_client_get(url: &str, api_key: &str) -> Result<String, String> {
    let response = match ureq::get(url).set("Authorization", &format!("Bearer {api_key}")).call() {
        Ok(response) => response,
        Err(ureq::Error::Status(_, response)) => response,
        Err(e) => return Err(format!("agent request failed: {e}")),
    };
    let status = response.status();
    let text = response.into_string().map_err(|e| e.to_string())?;
    if (200..300).contains(&status) {
        Ok(text)
    } else {
        Err(format!("agent {status}: {text}"))
    }
}

fn agent_client_post(url: &str, api_key: &str, body: &[u8]) -> Result<String, String> {
    // Retry the payload upload — large payloads over lossy links (tailnet,
    // flaky dev machines) can drop mid-stream. Up to 6 attempts with backoff.
    let mut last_err = String::new();
    for attempt in 1..=6 {
        let agent = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(3600)).build();
        let response = match agent
            .post(url)
            .set("Authorization", &format!("Bearer {api_key}"))
            .set("Content-Type", "application/gzip")
            .send_bytes(body)
        {
            Ok(response) => response,
            Err(ureq::Error::Status(_, response)) => response,
            Err(e) => {
                last_err = format!("agent request failed: {e}");
                if attempt < 6 {
                    print_step(&format!("payload upload attempt {attempt} failed ({e}); retrying..."));
                    std::thread::sleep(std::time::Duration::from_secs(attempt as u64 * 3));
                }
                continue;
            }
        };
        let status = response.status();
        let text = response.into_string().map_err(|e| e.to_string())?;
        if (200..300).contains(&status) {
            return Ok(text);
        }
        last_err = format!("agent {status}: {text}");
        break;
    }
    Err(last_err)
}

fn rustup_target_installed(target: &str) -> bool {
    match run_capture("rustup", &["target".to_string(), "list".to_string(), "--installed".to_string()], &util::current_dir()) {
        Ok(result) => result.stdout.lines().any(|line| line.trim() == target),
        Err(_) => false,
    }
}

fn ensure_pinned_zig() -> Result<PathBuf, String> {
    const ZIG_VERSION: &str = "0.13.0";
    let triple = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "x86_64") => "macos-x86_64".to_string(),
        ("macos", "aarch64") => "macos-aarch64".to_string(),
        ("linux", "x86_64") => "linux-x86_64".to_string(),
        ("linux", "aarch64") => "linux-aarch64".to_string(),
        (os, arch) => return Err(format!("zig cross toolchain unsupported on {os}/{arch}")),
    };
    let cache = Path::new(&util::home_dir()).join(".cache").join("eco").join("zig");
    let install_dir = cache.join(format!("zig-{triple}-{ZIG_VERSION}"));
    if install_dir.join("zig").is_file() {
        return Ok(install_dir);
    }
    let tarball = format!("zig-{triple}-{ZIG_VERSION}.tar.xz");
    print_step(&format!("Downloading pinned zig {ZIG_VERSION} ({tarball})"));
    let _ = std::fs::create_dir_all(&cache);
    let target_path = cache.join(&tarball);
    let url = format!("https://ziglang.org/download/{ZIG_VERSION}/{tarball}");
    run_command("curl", &["-fsSL".to_string(), url, "-o".to_string(), target_path.display().to_string()], &util::current_dir())?;
    run_command("tar", &["xf".to_string(), target_path.display().to_string(), "-C".to_string(), cache.display().to_string()], &util::current_dir())?;
    let _ = std::fs::remove_file(&target_path);
    if !install_dir.join("zig").is_file() {
        return Err(format!("zig extraction did not produce {}", install_dir.display()));
    }
    Ok(install_dir)
}

// Provisions the local cross-compile toolchain the same way build-release.sh
// does for the eco binary itself: rustup target + pinned zig + cargo-zigbuild.
// Returns the zig bin dir when a pinned copy was installed (else None when a
// system `zig` is already on PATH).
fn ensure_cross_toolchain() -> Result<Option<PathBuf>, String> {
    if !rustup_target_installed("x86_64-unknown-linux-musl") {
        print_step("Installing rustup target x86_64-unknown-linux-musl");
        run_command("rustup", &["target".to_string(), "add".to_string(), "x86_64-unknown-linux-musl".to_string()], &util::current_dir())?;
    }
    if !util::command_on_path("cargo-zigbuild") {
        print_step("Installing cargo-zigbuild (pinned)");
        run_command("cargo", &["install".to_string(), "cargo-zigbuild".to_string(), "--locked".to_string()], &util::current_dir())?;
    }
    if util::command_on_path("zig") {
        return Ok(None);
    }
    let zig_dir = ensure_pinned_zig()?;
    Ok(Some(zig_dir))
}

// ─────────────────────────────────────────────────────────────────────────────
// Local Linux builder (eco-builder VM) — where Node/Rust artifacts are built so
// production CTs never compile anything. Default driver is Multipass
// (`multipass exec <ECO_BUILDER>`); override with ECO_BUILDER_CMD on machines
// where Multipass is unavailable (M1: `limactl shell <name> --`, etc.).
// ─────────────────────────────────────────────────────────────────────────────

fn builder_name() -> String {
    util::env_var_or("ECO_BUILDER", "eco-builder")
}

// Build node artifacts directly on the dev machine (no VM) when the user sets
// ECO_BUILDER=host, or when no VM driver is available. Correct for node builds
// because the artifact (dist / JS bundle / Bun binary) is platform-agnostic —
// the CT installs its own linux-x64 runtime deps (or runs the Bun binary).
fn builder_is_host() -> bool {
    let mode = util::env_var_or("ECO_BUILDER", "");
    if mode == "host" {
        return true;
    }
    // A VM is only used when one is explicitly requested (ECO_BUILDER=<name>)
    // or reachable via Multipass. With no VM driver, build on the host — a
    // dev machine always has the toolchain, and the artifact is platform-
    // agnostic anyway. This is what makes `eco up --remote` seamless for a
    // fresh laptop with no ECO_BUILDER set.
    if !mode.is_empty() {
        return false;
    }
    !multipass_available()
}

fn multipass_available() -> bool {
    run_capture("multipass", &["version".to_string()], &util::current_dir())
        .map(|c| c.code == 0)
        .unwrap_or(false)
}

// Build a PATH that includes the usual dev-toolchain locations so the host
// builder can find node/npm/bun even when the caller's shell never sourced
// nvm/homebrew (e.g. eco invoked from a bare cron/CI PATH). Appends only
// existing directories; the original PATH stays authoritative first.
fn host_builder_path() -> String {
    let home = util::home_dir();
    let mut candidates: Vec<String> = Vec::new();
    // nvm install dirs: ~/.nvm/versions/node/<ver>/bin (pick the highest)
    if let Ok(entries) = std::fs::read_dir(format!("{home}/.nvm/versions/node")) {
        let mut vers: Vec<String> = entries
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
            .collect();
        vers.sort();
        if let Some(v) = vers.last() {
            candidates.push(format!("{home}/.nvm/versions/node/{v}/bin"));
        }
    }
    for candidate in [
        format!("{home}/.bun/bin"),
        "/opt/homebrew/bin".to_string(),
        "/usr/local/bin".to_string(),
        format!("{home}/.cargo/bin"),
    ] {
        if Path::new(&candidate).is_dir() {
            candidates.push(candidate);
        }
    }
    let existing: Vec<String> = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    let mut path = existing;
    for c in candidates {
        if !path.contains(&c) {
            path.push(c);
        }
    }
    path.join(":")
}

// Where node builds land: the VM (/home/ubuntu/build) or the host cache.
fn builder_build_root() -> String {
    if builder_is_host() {
        format!("{}/.cache/eco/build", util::home_dir())
    } else {
        "/home/ubuntu/build".to_string()
    }
}

fn builder_cmd() -> Vec<String> {
    let cmd = util::env_var_or("ECO_BUILDER_CMD", "");
    if !cmd.is_empty() {
        return cmd.split_whitespace().map(|s| s.to_string()).collect();
    }
    vec!["multipass".to_string(), "exec".to_string(), builder_name(), "--".to_string()]
}

fn builder_exec(script: &str) -> Result<util::Captured, String> {
    if builder_is_host() {
        // Inherit the parent shell env (no env_clear) and prepend the dev
        // toolchain dirs inside the script itself, so nvm's npm (a symlink
        // with `#!/usr/bin/env node`) resolves node exactly like an interactive
        // shell would. Passing the augmented PATH via the env map alone proved
        // fragile on macOS with nvm-managed node.
        let path = host_builder_path();
        let wrapped = format!(
            "export PATH={}; export SQLX_OFFLINE=true; {}",
            shell_single_quote(&path),
            script
        );
        return run_capture("bash", &["-c".to_string(), wrapped], &util::current_dir());
    }
    let mut args = builder_cmd();
    // Non-login shell: `bash -lc` (login) sources ~/.profile/~.bashrc, which
    // makes `set -e; exit 0` report exit 1 on this Ubuntu image — corrupting
    // every builder result. `bash -c` avoids the profile interference.
    args.extend(["bash".to_string(), "-c".to_string(), script.to_string()]);
    run_capture(&args[0], &args[1..], &util::current_dir())
}

fn builder_exec_ok(script: &str) -> Result<(), String> {
    let result = builder_exec(script)?;
    if result.code != 0 {
        return Err(format!("builder command failed ({}): {}", result.code, result.stderr.trim()));
    }
    Ok(())
}

fn builder_available() -> bool {
    if builder_is_host() {
        return true;
    }
    let mut args = vec!["info".to_string(), builder_name()];
    run_capture("multipass", &args, &util::current_dir()).map(|c| c.code == 0).unwrap_or(false)
}

fn skip_build_sync(name: &str) -> bool {
    ["node_modules", "target", ".git", ".env"].contains(&name)
}

// Syncs a local tree into the build location (VM or host cache).
fn sync_dir_to_builder(local_dir: &Path, dest: &str) -> Result<(), String> {
    if builder_is_host() {
        std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
        return copy_tree_excluding(local_dir, Path::new(dest), &(skip_build_sync as fn(&str) -> bool));
    }
    let tar_path = std::env::temp_dir().join(format!("eco-builder-sync-{}.tar.gz", std::process::id()));
    let _ = std::fs::remove_file(&tar_path);
    run_command(
        "tar",
        &[
            "czf".to_string(),
            tar_path.display().to_string(),
            "--exclude".to_string(),
            "node_modules".to_string(),
            "--exclude".to_string(),
            "target".to_string(),
            "--exclude".to_string(),
            ".git".to_string(),
            "--exclude".to_string(),
            ".env".to_string(),
            "-C".to_string(),
            local_dir.display().to_string(),
            ".".to_string(),
        ],
        &util::current_dir(),
    )?;
    let remote_tar = format!("/tmp/eco-builder-sync-{}.tar.gz", std::process::id());
    let mut transfer_args = vec!["transfer".to_string(), tar_path.display().to_string(), format!("{}:{}", builder_name(), remote_tar)];
    run_command("multipass", &transfer_args, &util::current_dir())?;
    builder_exec_ok(&format!("mkdir -p {} && tar xzf {} -C {}", shell_single_quote(dest), remote_tar, shell_single_quote(dest)))?;
    let _ = std::fs::remove_file(&tar_path);
    let _ = transfer_args;
    Ok(())
}

// Copies the built output back from the build location (VM or host cache) into
// a local artifacts/<service> dir, and Bun-compiles SSR node apps into a
// single linux-x64 binary when the build produced a server entry.
// Trim dev/build-cache cruft from a shipped frontend artifact so the payload
// stays small. Next.js dev builds put hundreds of MB under .next/dev (and
// .next/cache/build) that `next start` never needs — only server/static/
// BUILD_ID + the manifests are runtime output. Paths are resolved relative to
// `root` (the artifact dir) so .next/dev is found whether .next sits directly
// under the artifact or under a nested dist/ output dir.
fn trim_dev_artifact(_dir: &Path, root: &Path) {
    for name in [".next/dev", ".next/cache", ".next/build", "node_modules/.cache", ".vite/cache"] {
        let p = root.join(name);
        if p.is_dir() {
            let _ = std::fs::remove_dir_all(&p);
        }
    }
}

fn copy_frontend_artifact_from_builder(build_dir: &str, artifact_dir: &Path, service_name: &str) -> Result<(), String> {
    std::fs::create_dir_all(artifact_dir).map_err(|e| e.to_string())?;
    let subdirs = ["dist", "build", ".next", "output", "app/dist", "app/.next", "app/build"];
    let mut included: Vec<String> = Vec::new();
    for sub in subdirs {
        let path = Path::new(build_dir).join(sub);
        let present = if builder_is_host() {
            path.is_dir()
        } else {
            let c = builder_exec(&format!("test -d {} && echo yes || echo no", shell_single_quote(&path.display().to_string()))).ok();
            c.map(|c| c.stdout.trim() == "yes").unwrap_or(false)
        };
        if present {
            included.push(sub.to_string());
        }
    }
    if included.is_empty() {
        return Err(format!("builder produced no dist/build/.next/output under {build_dir}"));
    }
    if builder_is_host() {
        for sub in &included {
            copy_tree_excluding(&Path::new(build_dir).join(sub), &artifact_dir.join(sub), &(skip_none as fn(&str) -> bool))?;
        }
        // Trim dev/build-cache cruft from the artifact so the shipped payload
        // stays small (Next.js dev builds put ~hundreds of MB under
        // .next/dev, which is never needed at runtime; `next start` only needs
        // server/static/BUILD_ID).
        let artifact = artifact_dir.join("dist");
        trim_dev_artifact(&artifact, &artifact_dir);
        for sub in &included {
            let path = artifact_dir.join(sub);
            if path.is_dir() {
                trim_dev_artifact(&path, &artifact_dir);
            }
        }
    } else {
        let remote_tar = format!("/tmp/eco-builder-artifact-{}.tar.gz", std::process::id());
        builder_exec_ok(&format!(
            "cd {} && tar czf {} {}",
            shell_single_quote(build_dir),
            remote_tar,
            included.join(" ")
        ))?;
        let mut pull_args = vec!["transfer".to_string(), format!("{}:{}", builder_name(), remote_tar), "/tmp/eco-builder-artifact.tar.gz".to_string()];
        run_command("multipass", &pull_args, &util::current_dir())?;
        run_command("tar", &["xzf".to_string(), "/tmp/eco-builder-artifact.tar.gz".to_string(), "-C".to_string(), artifact_dir.display().to_string()], &util::current_dir())?;
        builder_exec(&format!("rm -f {}", remote_tar))?;
        let _ = pull_args;
    }
    // Bun-compile SSR node apps (host builder mode) into a single linux-x64
    // binary so the CT needs no node_modules. The build output's server entry
    // imports runtime deps from node_modules, so the compile runs where npm ci
    // ran (the build dir), not in the copied artifact.
    //
    // SvelteKit adapter-node builds (build/index.js + build/client) are the
    // exception: the node server serves client assets from disk relative to
    // its entry, and `bun build --compile` embeds only the server, so the
    // binary looks for the client in its embedded $bunfs/root/client, never
    // finds it, and 404s every /_app/immutable/* asset. Ship the build/ tree
    // as-is and let the CT run `node build/index.js` instead.
    if builder_is_host() && util::command_on_path("bun") {
        let sveltekit_client = Path::new(build_dir).join("build").join("client");
        if builder_is_host() && sveltekit_client.is_dir() {
            print_step(&format!(
                "SvelteKit adapter-node build ({}), skipping bun-compile — served via `node build/index.js`",
                sveltekit_client.display()
            ));
            return Ok(());
        }
        let server_entry = ["build/index.js", "build/server.js", "build/index.mjs", "build/index.cjs"]
            .iter()
            .map(|p| Path::new(build_dir).join(p))
            .find(|p| p.is_file());
        if let Some(entry) = server_entry {
            let out = artifact_dir.join(service_name);
            print_step(&format!("Bun-compiling {} (SSR node app) -> single linux-x64 binary", service_name));
            // Entry path relative to the build dir so node_modules resolve.
            let rel = entry.strip_prefix(Path::new(build_dir)).map(|p| p.display().to_string()).unwrap_or_else(|_| entry.display().to_string());
            run_command(
                "bun",
                &[
                    "build".to_string(),
                    "--compile".to_string(),
                    "--target=bun-linux-x64".to_string(),
                    rel,
                    "--outfile".to_string(),
                    out.display().to_string(),
                ],
                Path::new(build_dir),
            )?;
            std::fs::write(artifact_dir.join(".eco-bun"), service_name).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn collect_frontend_inputs(dir: &Path, out: &mut Vec<String>) -> Result<(), String> {
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();
        if path.is_dir() {
            if ["target", "node_modules", ".git", ".next", "dist", "build", ".cache", ".eco"].contains(&name.as_str()) {
                continue;
            }
            collect_frontend_inputs(&path, out)?;
        } else {
            if [".env", ".env.local"].contains(&name.as_str()) || name.ends_with(".log") {
                continue;
            }
            out.push(path.display().to_string());
        }
    }
    Ok(())
}

fn compute_frontend_input_hash(service_dir: &Path) -> Result<String, String> {
    let mut inputs: Vec<String> = Vec::new();
    collect_frontend_inputs(service_dir, &mut inputs)?;
    for manifest in ["package.json", "package-lock.json", "pnpm-lock.yaml", "yarn.lock"] {
        let path = service_dir.join(manifest);
        if path.is_file() {
            inputs.push(path.display().to_string());
        }
    }
    inputs.sort();
    let mut combined = String::new();
    for path in &inputs {
        let bytes = std::fs::read(path).map_err(|e| format!("read {path}: {e}"))?;
        let digest = sha2::Sha256::digest(&bytes);
        combined.push_str(&format!("{}  {path}\n", crate::registry::hex_encode(&digest)));
    }
    Ok(if combined.is_empty() {
        String::new()
    } else {
        format!("{:x}", sha2::Sha256::digest(combined.as_bytes()))
    })
}

pub fn run_up_remote(args: &[String]) -> Result<(), String> {
    let (options, positionals) = parse_options(args);
    let input = positionals.first().cloned().unwrap_or_else(|| ".".to_string());
    let cwd = util::current_dir();
    let deployment = load_project_deployment(&input, &cwd)?;
    // API URL + key: explicit env wins, else the `eco login`-stored auth
    // (defaulting the URL to the public api.getecosphere.com).
    let (api_url, api_key) = crate::commands::account::resolve_api_credentials()?;
    let api_url = if api_url.is_empty() { "https://api.getecosphere.com".to_string() } else { api_url };
    if api_url.is_empty() {
        return Err(
            "eco up --remote requires ECO_API_URL pointing at the eco serve agent on the Proxmox host (e.g. http://host:8790).".to_string(),
        );
    }
    if api_key.is_empty() {
        return Err("eco up --remote requires an API key (run `eco login`, or set ECO_API_KEY).".to_string());
    }
    let base = api_url.trim_end_matches('/').to_string();
    let staging = options.get("staging").map(|v| v == "true").unwrap_or(false);
    let staging_config = ecompose::parse_staging(&deployment.content);
    if staging && staging_config.get("ct").map(|s| s.is_empty()).unwrap_or(true) {
        return Err(format!(
            "--staging requested for {}, but ecompose.yml has no staging.ct declared. Add a staging: block (staging.ct: 1000).",
            deployment.project
        ));
    }
    let deploy_query = if staging { "?staging=1" } else { "" };
    let estate_root = deployment
        .project_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| deployment.project_dir.clone());
    let project_dir_str = deployment.project_dir.display().to_string();

    // Pair each declared Rust service with its local crate directory and its
    // CT-relative path (where the binary and .eco-rust-hash live on the CT).
    let mut rust_targets: Vec<(ecompose::Service, String, PathBuf)> = Vec::new();
    for service in deployment
        .services
        .iter()
        .filter(|s| !s.path.is_empty() && s.runtimes.iter().any(|r| r == "rust"))
    {
        let rel = relative_ct_service_path(&service.path, &deployment.project, &project_dir_str, "");
        let rel = if rel.is_empty() { service.path.clone() } else { rel };
        let candidates = [estate_root.join(&rel), deployment.project_dir.join(&rel)];
        let Some(dir) = candidates.iter().find(|c| c.join("Cargo.toml").is_file()) else {
            return Err(format!(
                "Cannot find local crate for Rust service {} (looked in {} and {})",
                service.name,
                candidates[0].display(),
                candidates[1].display()
            ));
        };
        rust_targets.push((service.clone(), rel, dir.clone()));
    }
    if rust_targets.is_empty() {
        // Not necessarily an error any more: an estate may ship only Node
        // frontends (built on the local builder) with no Rust services.
        print_step("no Rust services to cross-compile (continuing with Node/other artifacts)");
    }

    // Node/frontend services: built on the local Linux builder VM (so npm
    // native modules come out linux-x64 and match the production CTs), and
    // the built dist ships to the CT so it never runs `npm run build`.
    let mut frontend_targets: Vec<(ecompose::Service, String, PathBuf)> = Vec::new();
    for service in deployment
        .services
        .iter()
        .filter(|s| !s.path.is_empty() && s.runtimes.iter().any(|r| r == "npm" || r.starts_with("node@") || r == "leptos"))
    {
        let rel = relative_ct_service_path(&service.path, &deployment.project, &project_dir_str, "");
        let rel = if rel.is_empty() { service.path.clone() } else { rel };
        let candidates = [estate_root.join(&rel), deployment.project_dir.join(&rel)];
        // Node frontends have package.json; Leptos/Rust frontends have
        // Cargo.toml + index.html (the trunk CSR entry).
        let is_leptos = service.runtimes.iter().any(|r| r == "leptos");
        let Some(dir) = candidates.iter().find(|c| {
            if is_leptos {
                c.join("Cargo.toml").is_file() && c.join("index.html").is_file()
            } else {
                c.join("package.json").is_file()
            }
        }) else {
            return Err(format!(
                "Cannot find local {} for service {} (looked in {} and {})",
                if is_leptos { "Cargo.toml + index.html (Leptos)" } else { "package.json" },
                service.name,
                candidates[0].display(),
                candidates[1].display()
            ));
        };
        frontend_targets.push((service.clone(), rel, dir.clone()));
    }
    if rust_targets.is_empty() && frontend_targets.is_empty() {
        return Err("eco up --remote found no Rust or Node services in ecompose.yml to build and ship.".to_string());
    }

    if options.get("dry-run").map(|v| v == "true").unwrap_or(false) {
        print_step(&format!("remote deploy plan for {}{} (dry-run)", deployment.project, if staging { " (staging)" } else { "" }));
        if staging {
            print_step(&format!("target CT: {} (staging.getecosphere.com style footprint)", staging_config.get("ct").cloned().unwrap_or_default()));
        }
        print_step(&format!("agent: {base}"));
        print_step("cross-toolchain: x86_64-unknown-linux-musl via cargo-zigbuild");
        for (service, _, dir) in &rust_targets {
            print_step(&format!("cross-compile {} from {} and ship binary", service.name, dir.display()));
        }
        for (service, _, dir) in &frontend_targets {
            print_step(&format!(
                "build {} on local builder ({}): npm ci + npm run build, ship dist",
                service.name,
                builder_name()
            ));
            print_step(&format!("  local source: {}", dir.display()));
        }
        return Ok(());
    }

    let zig_dir = ensure_cross_toolchain()?;
    let mut build_env: Vec<(String, String)> = vec![("SQLX_OFFLINE".to_string(), "true".to_string())];
    if let Some(zig_dir) = &zig_dir {
        let path = std::env::var("PATH").unwrap_or_default();
        build_env.push(("PATH".to_string(), format!("{}:{path}", zig_dir.display())));
    }

    // Only crates that use the compile-time sqlx macros (query!/query_as!/...)
    // need committed .sqlx/ offline metadata; runtime sqlx::query(&str) API
    // builds fine offline. Refuse only when macros are present and metadata is
    // missing, so estates that never use the macros (e.g. the getecosphere
    // platform) are not blocked.
    for (service, _, dir) in &rust_targets {
        if crate_uses_sqlx_query_macros(dir) && !dir.join(".sqlx").is_dir() {
            return Err(format!(
                "{} uses sqlx::query!/query_as! macros but has no committed .sqlx/ offline metadata in {}. Run `cargo sqlx prepare` once against the estate database and commit the .sqlx/ directory so remote builds can use SQLX_OFFLINE=true.",
                service.name,
                dir.display()
            ));
        }
    }

    // Best-effort: pull each service's generated .env from the CT so the local
    // compile sees the same values as production (build.rs inputs, feature
    // flags, PUBLIC_* build-time vars for frontends). The CT .env is generated
    // state on the CT and never leaves the host.
    for (service, _, dir) in rust_targets.iter().chain(frontend_targets.iter()) {
        let url = format!(
            "{base}/v1/estates/{}/services/{}/env{deploy_query}",
            project_path_segment(&deployment.project),
            project_path_segment(&service.name)
        );
        let mut env_text = match agent_client_get(&url, &api_key) {
            Ok(t) => Some(t),
            Err(_) => None,
        };
        // First staging deploy: the staging service has no .env yet (404), so
        // the frontend build would fail on required PUBLIC_* vars. Fall back
        // to the production .env for the same service so the artifact builds;
        // the gateway still serves the staging hostname.
        if env_text.is_none() && staging {
            let prod_url = format!(
                "{base}/v1/estates/{}/services/{}/env",
                project_path_segment(&deployment.project),
                project_path_segment(&service.name)
            );
            if let Ok(t) = agent_client_get(&prod_url, &api_key) {
                env_text = Some(t);
                print_step(&format!("Staging has no .env for {} yet — using production .env for the build", service.name));
            }
        }
        let is_frontend = frontend_targets.iter().any(|(s, _, _)| s.name == service.name);
        if let Some(text) = env_text {
            for line in text.lines() {
                if let Some((key, value)) = parse_env_line(line) {
                    // Security boundary: frontend builds only ever receive the
                    // public build-time subset (PUBLIC_* / VITE_* /
                    // NEXT_PUBLIC_*) — those are sent to the browser anyway and
                    // must be inlined. Secrets in the prod .env (DB URIs, JWT,
                    // API keys) never reach a frontend build. Rust builds get
                    // the full env in memory for compile-time metadata only; the
                    // binary reads its env at runtime from the CT.
                    if is_frontend && !(key.starts_with("PUBLIC_") || key.starts_with("VITE_") || key.starts_with("NEXT_PUBLIC_")) {
                        continue;
                    }
                    if !build_env.iter().any(|(existing, _)| existing == &key) {
                        build_env.push((key, value));
                    }
                }
            }
            print_step(&format!("Using CT .env for {} build environment", service.name));
        } else {
            print_step(&format!("No CT .env for {} yet — building with SQLX_OFFLINE only", service.name));
        }
        // Frontends: also export every key the frontend declares in its own
        // .env.example, so $env/static/public always resolves the exports the
        // source imports (e.g. a freshly renamed PUBLIC_STORAGE_URL) even on a
        // first deploy where the CT .env was generated from an older example.
        // Empty values fall back to the code's defaults; CT-provided values win.
        // Same PUBLIC_*-only filter as above.
        if is_frontend {
            let example_path = dir.join(".env.example");
            if let Ok(text) = std::fs::read_to_string(&example_path) {
                for line in text.lines() {
                    if let Some((key, value)) = parse_env_line(line) {
                        if !(key.starts_with("PUBLIC_") || key.starts_with("VITE_") || key.starts_with("NEXT_PUBLIC_")) {
                            continue;
                        }
                        if !build_env.iter().any(|(existing, _)| existing == &key) {
                            build_env.push((key, value));
                        }
                    }
                }
            }
        }
    }

    // Cross-compile each service and collect artifacts + source hashes.
    let mut artifacts: Vec<(String, PathBuf)> = Vec::new();
    let mut hash_lines: Vec<String> = Vec::new();
    for (service, rel, dir) in &rust_targets {
        let cargo_text = std::fs::read_to_string(dir.join("Cargo.toml")).map_err(|e| format!("read {}: {e}", dir.join("Cargo.toml").display()))?;
        let Some(package) = cargo_package_name(&cargo_text) else {
            print_step(&format!("Skipping {}: no [package] binary name", service.name));
            continue;
        };
        print_step(&format!("Cross-compiling {} ({package}) for x86_64-unknown-linux-musl", service.name));
        // Workspace members cannot be built from the member dir alone ("current
        // package believes it's in a workspace when it's not"). Detect the
        // workspace root via cargo metadata and build with `-p <package>` from
        // there; standalone crates build from their own dir as before.
        let (build_cwd, build_args) = match cargo_workspace_root(dir) {
            Some(root) if PathBuf::from(&root) != *dir => {
                let args: Vec<String> = ["zigbuild", "--release", "-p", &package, "--target", "x86_64-unknown-linux-musl"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                (PathBuf::from(&root), args)
            }
            _ => {
                let args: Vec<String> = ["zigbuild", "--release", "--target", "x86_64-unknown-linux-musl"].iter().map(|s| s.to_string()).collect();
                (dir.clone(), args)
            }
        };
        run_command_env("cargo", &build_args, &build_cwd, &build_env)?;
        // Cargo workspaces write artifacts to the workspace target directory
        // (not the member's own target/), so resolve the real target dir the
        // same way configure.sh does, then fall back to the member-local path.
        let mut binary = dir
            .join("target")
            .join("x86_64-unknown-linux-musl")
            .join("release")
            .join(&package);
        if let Some(target_dir) = resolve_cargo_target_dir(dir) {
            let workspace_binary = Path::new(&target_dir).join("x86_64-unknown-linux-musl").join("release").join(&package);
            if workspace_binary.is_file() {
                binary = workspace_binary;
            }
        }
        if !binary.is_file() {
            return Err(format!("cross-compiled binary not found: {}", binary.display()));
        }
        artifacts.push((package.clone(), binary));
        let hash = compute_rust_input_hash(dir)?;
        hash_lines.push(format!("{rel} {hash}"));
    }
    if artifacts.is_empty() && frontend_targets.is_empty() {
        return Err("no Rust binaries or frontend dist were produced; aborting remote deploy.".to_string());
    }

    // Build each Node/frontend service on the local Linux builder VM and
    // collect the built dist (plus a frontend source hash for the CT-side
    // skip). npm native modules are linux-x64 because the builder is x86_64.
    let mut frontend_artifacts: Vec<(String, PathBuf)> = Vec::new();
    let mut frontend_hash_lines: Vec<String> = Vec::new();
    for (service, rel, dir) in &frontend_targets {
        if !builder_available() {
            return Err(format!(
                "{} is a Node service but no local builder is reachable. Provision the eco-builder VM (see docs/guide/dev-toolchain-free-cts.md) or set ECO_BUILDER.",
                service.name
            ));
        }
        let hash = compute_frontend_input_hash(dir)?;
        frontend_hash_lines.push(format!("{rel} {hash}"));
        let build_dir = format!("{}/{}", builder_build_root(), service.name);
        let build_loc = if builder_is_host() { "on this machine (host builder)".to_string() } else { format!("on local builder ({})", builder_name()) };
        let is_leptos = dir.join("index.html").is_file() && !dir.join("package.json").is_file();
        print_step(&format!(
            "Building {} {}: {}",
            service.name,
            build_loc,
            if is_leptos { "trunk build --release (Leptos wasm)" } else { "npm ci + npm run build" }
        ));
        sync_dir_to_builder(dir, &build_dir)?;
        let mut exports = String::from("set -euo pipefail\n");
        for (key, value) in &build_env {
            // Only shell-valid identifier keys can be exported; keys like
            // `cors.allowed-origins` (dots) are not usable in bash.
            if is_shell_ident(key) {
                if key == "PATH" {
                    // Prepend instead of replacing: build_env pins a zig/cargo
                    // PATH for the Rust cross-compile, but the same export must
                    // not wipe the node/npm dirs a frontend build needs (nvm,
                    // bun, homebrew). Keep the existing PATH after the prepend.
                    exports.push_str(&format!(
                        "export PATH={}:$PATH\n",
                        shell_single_quote(value)
                    ));
                } else {
                    exports.push_str(&format!("export {}={}\n", key, shell_single_quote(value)));
                }
            }
        }
        // Hash-skip: if this frontend's source hash matches the last built
        // hash, reuse the existing output instead of building again.
        let script = if is_leptos {
            format!(
                "cd {} && {exports}if [ -f .eco-frontend-hash ] && [ \"$(cat .eco-frontend-hash)\" = \"{hash}\" ]; then echo \"frontend unchanged, skipping rebuild\"; exit 0; fi\nif ! command -v trunk >/dev/null 2>&1; then cargo install trunk --locked; fi && rustup target add wasm32-unknown-unknown 2>/dev/null || true; trunk build --release && printf '{hash}' > .eco-frontend-hash",
                shell_single_quote(&build_dir)
            )
        } else {
            format!(
                "cd {} && {exports}if [ -f .eco-frontend-hash ] && [ \"$(cat .eco-frontend-hash)\" = \"{hash}\" ]; then echo \"frontend unchanged, skipping rebuild\"; exit 0; fi\nif [ -f package-lock.json ]; then npm ci --no-audit --no-fund || npm install --no-audit --no-fund; elif [ -f pnpm-lock.yaml ]; then corepack enable && pnpm install --frozen-lockfile; else npm install --no-audit --no-fund; fi && ECO_DEPLOY_MODE=prod npm run build --if-present && printf '{hash}' > .eco-frontend-hash",
                shell_single_quote(&build_dir)
            )
        };
        builder_exec_ok(&script)?;
        let artifact_dir = std::env::temp_dir().join(format!("eco-frontend-artifact-{}-{}", service.name, std::process::id()));
        let _ = std::fs::remove_dir_all(&artifact_dir);
        copy_frontend_artifact_from_builder(&build_dir, &artifact_dir, &service.name)?;
        frontend_artifacts.push((service.name.clone(), artifact_dir));
        print_step(&format!("Built {} artifact -> ship", service.name));
    }

    // Build the deploy payload: source (project + domains), artifacts, hashes.
    let payload_dir = std::env::temp_dir().join(format!("eco-remote-payload-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&payload_dir);
    std::fs::create_dir_all(&payload_dir).map_err(|e| e.to_string())?;
    let result = (|| -> Result<(), String> {
        let source_dir = payload_dir.join("source");
        // Ship only the source the CT needs (git-tracked content minus the
        // fixed skip list) — gitignored files never ship. The built binaries
        // and frontend dist travel as the artifacts below.
        let project_ignored = load_gitignore_names(&deployment.project_dir);
        let source_skip = |name: &str| should_skip_remote_source(name) || project_ignored.iter().any(|g| g == name);
        copy_tree_excluding(&deployment.project_dir, &source_dir, &source_skip)?;
        let domains: Vec<String> = deployment
            .services
            .iter()
            .filter(|s| !s.path.is_empty())
            .filter_map(|s| s.path.split('/').next().map(|p| p.to_string()))
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        let project_base = deployment.project_dir.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        for domain in domains {
            if domain == "." || domain.is_empty() {
                continue;
            }
            if domain == project_base {
                continue;
            }
            let domain_dir = estate_root.join(&domain);
            if domain_dir.is_dir() {
                let domain_ignored = load_gitignore_names(&domain_dir);
                let domain_skip = |name: &str| {
                    should_skip_remote_source(name) || project_ignored.iter().any(|g| g == name) || domain_ignored.iter().any(|g| g == name)
                };
                copy_tree_excluding(&domain_dir, &source_dir.join(&domain), &domain_skip)?;
            }
        }
        let artifacts_dir = payload_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts_dir).map_err(|e| e.to_string())?;
        for (package, binary) in &artifacts {
            std::fs::copy(binary, artifacts_dir.join(package)).map_err(|e| format!("copy {}: {e}", binary.display()))?;
        }
        for (service_name, artifact_dir) in &frontend_artifacts {
            let dest = artifacts_dir.join(service_name);
            std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
            copy_tree_excluding(artifact_dir, &dest, &(skip_none as fn(&str) -> bool))?;
        }
        std::fs::write(payload_dir.join("rust-hashes"), format!("{}\n", hash_lines.join("\n"))).map_err(|e| e.to_string())?;
        std::fs::write(payload_dir.join("frontend-hashes"), format!("{}\n", frontend_hash_lines.join("\n"))).map_err(|e| e.to_string())?;
        let tar_path = payload_dir.join("payload.tar.gz");
        run_command(
            "tar",
            &[
                "czf".to_string(),
                tar_path.display().to_string(),
                "-C".to_string(),
                payload_dir.display().to_string(),
                "source".to_string(),
                "artifacts".to_string(),
                "rust-hashes".to_string(),
                "frontend-hashes".to_string(),
            ],
            &util::current_dir(),
        )?;
        // Payload size cap — the pricing hook. The shipped source must be the
        // git-tracked estate source + build artifacts only; large non-runtime
        // content (docs, data, uploads) that isn't gitignored blows past this
        // and is rejected rather than silently shipped to the CT.
        const MAX_PAYLOAD_MB: u64 = 300;
        let tar_meta = std::fs::metadata(&tar_path).map_err(|e| format!("read payload size: {e}"))?;
        let mb = tar_meta.len() / (1024 * 1024);
        if tar_meta.len() > MAX_PAYLOAD_MB * 1024 * 1024 {
            return Err(format!(
                "remote deploy payload is {mb} MB — over the {MAX_PAYLOAD_MB} MB cap. \
The shipped source includes large files that don't belong in a deploy; add them to \
.gitignore (gitignored files are never shipped) or move them out of the estate. \
Larger payloads are a paid-plan limit."
            ));
        }
        let bytes = std::fs::read(&tar_path).map_err(|e| format!("read payload: {e}"))?;
        let project_segment = project_path_segment(&deployment.project);
        // When ECO_SSH is set (e.g. root@100.85.173.92), ship the payload over
        // scp — SSH sustains large transfers on lossy links where the HTTP POST
        // drops — then tell the agent to deploy the uploaded file.
        let ssh = util::env_var_or("ECO_SSH", "");
        if !ssh.is_empty() {
            print_step(&format!(
                "remote payload is {} MB — shipping via scp to {ssh}",
                bytes.len() / (1024 * 1024)
            ));
            let remote_path = format!("/tmp/eco-remote-{project_segment}.tar.gz");
            let remote_path = format!("/tmp/eco-remote-{project_segment}.tar.gz");
            run_command("scp", &["-o".to_string(), "StrictHostKeyChecking=no".to_string(), tar_path.display().to_string(), format!("{ssh}:{remote_path}")], &util::current_dir())?;
            let deploy_file_url = format!("{base}/v1/estates/{project_segment}/deploy-file{deploy_query}");
            let summary = agent_client_post(&deploy_file_url, &api_key, b"")?;
            util::println_stdout(&summary);
        } else {
            print_step(&format!(
                "Shipping remote deploy payload for {} to {base} ({} MB)",
                deployment.project,
                bytes.len() / (1024 * 1024)
            ));
            // Large payloads exceed Cloudflare's free-tier request limit
            // (100MB), so chunk the upload (<90MB per chunk) to the
            // deploy-upload endpoint, which reassembles + deploys.
            const CHUNK_MB: usize = 90;
            const CHUNK_BYTES: usize = CHUNK_MB * 1024 * 1024;
            if bytes.len() > CHUNK_BYTES {
                let total = bytes.len().div_ceil(CHUNK_BYTES);
                print_step(&format!(
                    "payload is {} MB — uploading in {total} chunks (≤{CHUNK_MB} MB each)",
                    bytes.len() / (1024 * 1024)
                ));
                let mut summary = String::new();
                for (i, chunk) in bytes.chunks(CHUNK_BYTES).enumerate() {
                    let url = format!(
                        "{base}/v1/estates/{project_segment}/deploy-upload?part={i}&total={total}{}",
                        if deploy_query.is_empty() { String::new() } else { format!("&{}", &deploy_query[1..]) }
                    );
                    let resp = agent_client_post(&url, &api_key, chunk)?;
                    summary = resp;
                }
                // The agent deploys asynchronously after the last chunk; poll
                // the specific deploy by id until it reaches a final state
                // (the tunnel would time out if we waited synchronously).
                let mut deploy_id: Option<String> = None;
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&summary) {
                    deploy_id = v.get("deploy_id").and_then(|d| match d {
                        serde_json::Value::String(s) => Some(s.clone()),
                        serde_json::Value::Number(n) => Some(n.to_string()),
                        _ => None,
                    });
                }
                print_step("deploy started — waiting for completion…");
                let id_query = match &deploy_id {
                    Some(id) => format!("{}id={id}", if deploy_query.is_empty() { "?" } else { "&" }),
                    None => deploy_query.to_string(),
                };
                let status_url = format!("{base}/v1/estates/{project_segment}/deploy-status{id_query}");
                let mut final_status = "pending".to_string();
                for _ in 0..200 {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    if let Ok(text) = agent_client_get(&status_url, &api_key) {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                            if let Some(st) = v.get("status").and_then(|s| s.as_str()) {
                                final_status = st.to_string();
                                if st == "success" || st == "failed" {
                                    break;
                                }
                            }
                        }
                    }
                }
                if final_status == "success" {
                    util::println_stdout(&format!("Remote deploy of {} completed on CT {}.", deployment.project, deployment.ctid));
                } else if final_status == "failed" {
                    return Err(format!("Remote deploy of {} failed on CT {}.", deployment.project, deployment.ctid));
                } else {
                    return Err(format!("Timed out waiting for the deploy of {} to finish.", deployment.project));
                }
            } else {
                let summary = agent_client_post(
                    &format!("{base}/v1/estates/{project_segment}/deploy{deploy_query}"),
                    &api_key,
                    &bytes,
                )?;
                util::println_stdout(&summary);
            }
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&payload_dir);
    result
}

fn install_remote_rust_binaries(ctid: &str, deployment: &ProjectDeployment, artifacts_dir: &str, hashes_file: &str, estate_core: &str) -> Result<(), String> {
    let mut hashes: HashMap<String, String> = HashMap::new();
    if !hashes_file.is_empty() {
        if let Ok(content) = std::fs::read_to_string(hashes_file) {
            for line in content.lines() {
                let mut parts = line.split_whitespace();
                if let (Some(path), Some(hash)) = (parts.next(), parts.next()) {
                    hashes.insert(path.to_string(), hash.to_string());
                }
            }
        }
    }
    let rust_services: Vec<&ecompose::Service> = deployment
        .services
        .iter()
        .filter(|s| !s.path.is_empty() && s.runtimes.iter().any(|r| r == "rust"))
        .collect();
    let project_dir = deployment.project_dir.display().to_string();
    for service in rust_services {
        let rel = relative_ct_service_path(&service.path, &deployment.project, &project_dir, estate_core);
        let rel = if rel.is_empty() { service.path.clone() } else { rel };
        let host_manifest = Path::new(&deployment.project_dir).join(&rel).join("Cargo.toml");
        let Some(cargo_text) = std::fs::read_to_string(&host_manifest).ok() else {
            continue;
        };
        let Some(package) = cargo_package_name(&cargo_text) else {
            continue;
        };
        let artifact = Path::new(artifacts_dir).join(&package);
        if !artifact.is_file() {
            return Err(format!("Remote artifact missing for {} (expected {})", service.name, artifact.display()));
        }
        let service_dir = resolve_ct_service_dir(service, &deployment.ct_project_root, &project_dir, estate_core);
        // configure.sh resolves the binary from the cargo target dir, the
        // project-root target/release, or the service target/release. Install
        // to the last two so every layout finds the shipped artifact.
        for target_dir in [format!("{service_dir}/target/release"), format!("{}/target/release", deployment.ct_project_root)] {
            pct_exec(ctid, &format!("mkdir -p {}", shell_single_quote(&target_dir)))?;
            run_command(
                "pct",
                &["push".to_string(), ctid.to_string(), artifact.display().to_string(), format!("{target_dir}/{package}.new")],
                &util::current_dir(),
            )?;
            pct_exec(
                ctid,
                &format!(
                    "mv -f {} {} && chmod 755 {}",
                    shell_single_quote(&format!("{target_dir}/{package}.new")),
                    shell_single_quote(&format!("{target_dir}/{package}")),
                    shell_single_quote(&format!("{target_dir}/{package}"))
                ),
            )?;
        }
        if let Some(hash) = hashes.get(&rel) {
            push_text_file_to_ct(ctid, &format!("{service_dir}/.eco-rust-hash"), hash, "eco-rust-hash")?;
        }
        print_step(&format!("[CT {ctid}] Installed remote Rust binary: {}", service.name));
    }
    Ok(())
}

// Installs the frontend dists that `eco up --remote` built on the local
// builder into the CT, and marks each service `.eco-frontend-built` so
// configure.sh skips `npm ci` + `npm run build` on the CT and serves the
// shipped artifact. Returns the service names that got a shipped dist.
fn install_remote_frontend_artifacts(ctid: &str, deployment: &ProjectDeployment, artifacts_dir: &str, hashes_file: &str, estate_core: &str) -> Result<Vec<String>, String> {
    let mut bun_compiled = Vec::new();
    if artifacts_dir.is_empty() || !Path::new(artifacts_dir).is_dir() {
        return Ok(bun_compiled);
    }
    let mut hashes: HashMap<String, String> = HashMap::new();
    if !hashes_file.is_empty() {
        if let Ok(content) = std::fs::read_to_string(hashes_file) {
            for line in content.lines() {
                let mut parts = line.split_whitespace();
                if let (Some(path), Some(hash)) = (parts.next(), parts.next()) {
                    hashes.insert(path.to_string(), hash.to_string());
                }
            }
        }
    }
    let project_dir = deployment.project_dir.display().to_string();
    for entry in std::fs::read_dir(artifacts_dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let fname = entry.file_name().to_string_lossy().to_string();
        // A directory under artifacts/ is a shipped frontend (service name);
        // flat files are the Rust binaries handled by install_remote_rust_binaries.
        if !entry.path().is_dir() {
            continue;
        }
        let Some(service) = deployment.services.iter().find(|s| s.name == fname) else {
            continue;
        };
        let service_dir = resolve_ct_service_dir(service, &deployment.ct_project_root, &project_dir, estate_core);
        pct_exec(ctid, &format!("mkdir -p {}", shell_single_quote(&service_dir)))?;
        // Ship the built dist tree into the CT service dir.
        let tar_path = std::env::temp_dir().join(format!("eco-frontend-{}-{}.tar.gz", service.name, std::process::id()));
        let _ = std::fs::remove_file(&tar_path);
        run_command(
            "tar",
            &[
                "czf".to_string(),
                tar_path.display().to_string(),
                "-C".to_string(),
                entry.path().display().to_string(),
                ".".to_string(),
            ],
            &util::current_dir(),
        )?;
        let remote_tar = format!("/tmp/eco-frontend-{}.tar.gz", service.name);
        run_command(
            "pct",
            &["push".to_string(), ctid.to_string(), tar_path.display().to_string(), remote_tar.clone()],
            &util::current_dir(),
        )?;
        pct_exec(ctid, &format!("tar xzf {} -C {}", shell_single_quote(&remote_tar), shell_single_quote(&service_dir)))?;
        pct_exec(ctid, &format!("rm -f {}", shell_single_quote(&remote_tar)))?;
        let _ = std::fs::remove_file(&tar_path);
        // Marker + hash so configure.sh skips npm install/build and serves dist.
        let is_bun = entry.path().join(".eco-bun").is_file();
        if is_bun {
            // Bun-compiled node backend: the artifact holds <service> (the
            // linux-x64 single binary) + its assets. Install a marker so
            // configure.sh generates a unit that runs the binary directly.
            pct_exec(ctid, &format!("chmod 755 {}", shell_single_quote(&format!("{service_dir}/{}", service.name))))?;
            pct_exec(ctid, &format!("touch {}", shell_single_quote(&format!("{service_dir}/.eco-bun"))))?;
            push_text_file_to_ct(ctid, &format!("{service_dir}/.eco-bun-name"), &service.name, "eco-bun-name")?;
            bun_compiled.push(service.name.clone());
            print_step(&format!("[CT {ctid}] Installed Bun-compiled single binary: {}", service.name));
        } else {
            pct_exec(ctid, &format!("touch {}", shell_single_quote(&format!("{service_dir}/.eco-frontend-built"))))?;
            print_step(&format!("[CT {ctid}] Installed shipped frontend dist: {}", service.name));
        }
        let rel = relative_ct_service_path(&service.path, &deployment.project, &project_dir, estate_core);
        let rel = if rel.is_empty() { service.path.clone() } else { rel };
        if let Some(hash) = hashes.get(&rel) {
            push_text_file_to_ct(ctid, &format!("{service_dir}/.eco-frontend-hash"), hash, "eco-frontend-hash")?;
        }
    }
    Ok(bun_compiled)
}

// ─────────────────────────────────────────────────────────────────────────────
// eco serve agent handlers (run on the Proxmox host).
// ─────────────────────────────────────────────────────────────────────────────

pub fn agent_list_estates() -> Vec<serde_json::Value> {
    let root = Path::new("/opt/projects");
    let mut estates = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        let mut projects: Vec<String> = entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        projects.sort();
        for project in projects {
            let manifest = root.join(&project).join("ecompose.yml");
            if !manifest.is_file() {
                continue;
            }
            let mut entry = serde_json::Map::new();
            entry.insert("project".to_string(), serde_json::Value::String(project.clone()));
            if let Ok(content) = std::fs::read_to_string(&manifest) {
                let ct = ecompose::parse_ct_metadata(&content);
                if let Some(id) = ct.get("id") {
                    entry.insert("ctid".to_string(), serde_json::Value::String(id.clone()));
                }
                let expose = ecompose::parse_expose(&content);
                let main = expose.hostname();
                let mut hostnames: Vec<serde_json::Value> = Vec::new();
                if !main.is_empty() {
                    hostnames.push(serde_json::Value::String(main.clone()));
                }
                for extra in &expose.additional {
                    if let Some(h) = extra.get("hostname") {
                        if !h.is_empty() && !hostnames.iter().any(|v| v.as_str() == Some(h.as_str())) {
                            hostnames.push(serde_json::Value::String(h.clone()));
                        }
                    }
                }
                if !hostnames.is_empty() {
                    entry.insert("hostname".to_string(), serde_json::Value::String(main.clone()));
                    entry.insert("hostnames".to_string(), serde_json::Value::Array(hostnames));
                }
                let staging = ecompose::parse_staging(&content);
                if let Some(h) = staging.get("hostname") {
                    if !h.is_empty() {
                        entry.insert("staging_hostname".to_string(), serde_json::Value::String(h.clone()));
                    }
                }
                let serve = ecompose::parse_indented_block(&content, "serve:");
                if let Some(sub) = serve.get("subdomain") {
                    if !sub.is_empty() {
                        entry.insert("serve_hostname".to_string(), serde_json::Value::String(format!("{sub}.getecosphere.com")));
                    }
                }
            }
            estates.push(serde_json::Value::Object(entry));
        }
    }
    estates
}

pub fn agent_read_service_env(project: &str, service_name: &str, staging: bool) -> Result<String, String> {
    let cwd = util::current_dir();
    let project_path = format!("/opt/projects/{project}");
    let deployment = load_project_deployment(&project_path, &cwd)?;
    let ctid = if staging {
        let staging_config = ecompose::parse_staging(&deployment.content);
        let ct = staging_config.get("ct").cloned().unwrap_or_default();
        if ct.is_empty() {
            return Err(format!("no staging.ct declared for {project}"));
        }
        ct
    } else {
        deployment.ctid.clone()
    };
    ensure_ct_running(&ctid)?;
    let service = deployment
        .services
        .iter()
        .find(|s| s.name == service_name)
        .ok_or_else(|| format!("service not found: {service_name}"))?;
    // The staged estate source is flattened to /opt/projects/<project>, so a
    // service path beginning with the estate core repo name (e.g.
    // `assessment_core/frontend`) must strip it — otherwise the CT-side .env
    // is never found on flattened estates and frontend builds bake empty
    // PUBLIC_* values.
    let estate_core = estate_core_name(&deployment.content);
    let service_dir = resolve_ct_service_dir(service, &deployment.ct_project_root, &deployment.project_dir.display().to_string(), &estate_core);
    let env_path = format!("{service_dir}/.env");
    let output = pct_exec_capture(
        &ctid,
        &format!(
            "if [ -f {} ]; then cat {}; else printf '%%ECO_MISSING_ENV%%'; fi",
            shell_single_quote(&env_path),
            shell_single_quote(&env_path)
        ),
    )?;
    if output.contains("%ECO_MISSING_ENV%") {
        return Err(format!("no .env generated yet for {service_name} on CT {ctid}"));
    }
    Ok(output)
}

pub fn agent_handle_deploy(project: &str, tar_gz: &[u8], staging: bool) -> Result<String, String> {
    let tmp = std::env::temp_dir().join(format!("eco-agent-deploy-{}-{}", project, std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| format!("create temp dir: {e}"))?;
    let tar_path = tmp.join("payload.tar.gz");
    std::fs::write(&tar_path, tar_gz).map_err(|e| format!("write payload: {e}"))?;
    let result = (|| -> Result<String, String> {
        run_command(
            "tar",
            &["xzf".to_string(), tar_path.display().to_string(), "-C".to_string(), tmp.display().to_string()],
            &util::current_dir(),
        )?;
        let source_dir = tmp.join("source");
        if !source_dir.is_dir() {
            return Err("payload is missing source/ — build the tarball with `eco up --remote`".to_string());
        }
        let artifacts_dir = tmp.join("artifacts");
        let hashes_file = tmp.join("rust-hashes");
        let frontend_hashes_file = tmp.join("frontend-hashes");
        let host_project = Path::new("/opt/projects").join(project);
        let _ = std::fs::remove_dir_all(&host_project);
        std::fs::create_dir_all(&host_project).map_err(|e| format!("stage /opt/projects: {e}"))?;
        copy_tree_excluding(&source_dir, &host_project, &(skip_none as fn(&str) -> bool))?;
        let cwd = util::current_dir();
        let project_path = host_project.display().to_string();
        let deployment = load_project_deployment(&project_path, &cwd)?;
        let mut options = HashMap::new();
        options.insert("remote".to_string(), "true".to_string());
        if artifacts_dir.is_dir() {
            options.insert("remote-artifacts".to_string(), artifacts_dir.display().to_string());
        }
        if hashes_file.is_file() {
            options.insert("remote-hashes".to_string(), hashes_file.display().to_string());
        }
        if frontend_hashes_file.is_file() {
            options.insert("remote-frontend-hashes".to_string(), frontend_hashes_file.display().to_string());
        }
        let staging_config = ecompose::parse_staging(&deployment.content);
        let deploy_ctid = if staging {
            staging_config.get("ct").cloned().unwrap_or_else(|| deployment.ctid.clone())
        } else {
            deployment.ctid.clone()
        };
        provision_estate(&deployment, &options, staging, &staging_config)?;
        Ok(format!("Remote deploy of {project} completed on CT {deploy_ctid}."))
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

// Installs `lxs:` services for an estate: pulls each versioned LXS binary from
// the registry, installs it into the CT's target/release, and writes a service
// marker dir (start.sh + .env.example from the LXS contract) so configure.sh
// discovers it as a normal service — port allocation, PM2, and gateway routing
// then work unchanged.
fn install_lxs_services(ctid: &str, deployment: &ProjectDeployment) -> Result<Vec<String>, String> {
    let lxs_services: Vec<&ecompose::Service> = deployment.services.iter().filter(|s| !s.lxs.is_empty()).collect();
    if lxs_services.is_empty() {
        return Ok(Vec::new());
    }
    // LXS is resolved from the registry ADDRESS (no local clone): `eco lxs add`
    // wrote the `lxs:` refs, and `eco up` fetches each binary straight from the
    // registry (the estate's `.eco/state.json` registry, official by default)
    // into the local cache, then ships it.
    let state_registry = crate::commands::lxs::read_estate_state(&deployment.project_dir).map(|s| s.registry).filter(|r| !r.is_empty());
    let mut installed = Vec::new();
    for service in lxs_services {
        let (manifest, version, local_bin) = crate::commands::lxs::fetch_lxs_to_cache(&service.lxs, "linux/amd64", state_registry.as_deref())?;
        let name = manifest.name.clone();
        if name.is_empty() {
            return Err(format!("LXS {} has no name in its manifest", service.lxs));
        }

        let target_dir = format!("{}/target/release", deployment.ct_project_root);
        pct_exec(ctid, &format!("mkdir -p {}", shell_single_quote(&target_dir)))?;
        run_command(
            "pct",
            &["push".to_string(), ctid.to_string(), local_bin.display().to_string(), format!("{target_dir}/{name}.new")],
            &util::current_dir(),
        )?;
        pct_exec(
            ctid,
            &format!(
                "mv -f {} {} && chmod 755 {}",
                shell_single_quote(&format!("{target_dir}/{name}.new")),
                shell_single_quote(&format!("{target_dir}/{name}")),
                shell_single_quote(&format!("{target_dir}/{name}"))
            ),
        )?;

        let service_dir = format!("{}/{}", deployment.ct_project_root, service.name);
        pct_exec(ctid, &format!("mkdir -p {}", shell_single_quote(&service_dir)))?;
        let binary_path = format!("{target_dir}/{name}");
        let start_sh = format!(
            "#!/bin/bash\nset -a\n. \"$(dirname \"$0\")/.env\" 2>/dev/null || true\nset +a\nif [ -z \"${{SERVER_PORT:-}}\" ] && [ -n \"${{PORT:-}}\" ]; then export SERVER_PORT=\"$PORT\"; fi\nexec {}\n",
            shell_single_quote(&binary_path)
        );
        push_text_file_to_ct(ctid, &format!("{service_dir}/start.sh"), &start_sh, "lxs-start")?;
        pct_exec(ctid, &format!("chmod 755 {}", shell_single_quote(&format!("{service_dir}/start.sh"))))?;

        let mut env_example = String::new();
        for key in manifest.contract.env.required.iter().chain(manifest.contract.env.optional.iter()) {
            let value = manifest.contract.env.defaults.get(key).cloned().unwrap_or_default();
            env_example.push_str(&format!("{key}={value}\n"));
        }
        push_text_file_to_ct(ctid, &format!("{service_dir}/.env.example"), &env_example, "lxs-env-example")?;
        // Seed .env with contract defaults (values configure.sh must preserve,
        // e.g. S3_BUCKET) and granted-secret placeholders. Runs before
        // configure.sh, which then adds the rest (ports, DB URIs, shared
        // secrets) via sync_env_from_example / set_env.
        let mut env_seed = String::new();
        for (key, value) in &manifest.contract.env.defaults {
            env_seed.push_str(&format!("{key}={value}\n"));
        }
        for key in &service.grants_secrets {
            if !manifest.contract.env.defaults.contains_key(key) {
                env_seed.push_str(&format!("{key}=\n"));
            }
        }
        if !env_seed.is_empty() {
            push_text_file_to_ct(ctid, &format!("{service_dir}/.env"), &env_seed, "lxs-env")?;
        }

        // The LXS contract declares the database it needs. configure.sh's data
        // bootstrap is driven by runtimes, which `lxs:` services no longer
        // declare, so eco provisions the DB itself from the contract.
        match manifest.contract.db.as_str() {
            "mongodb@7" => provision_lxs_mongodb(ctid, &service_dir)?,
            "postgresql@15" => provision_lxs_postgres(ctid, &service_dir, &service.name, &deployment.project)?,
            _ => {}
        }
        if manifest.contract.env.required.iter().any(|k| k == "REDIS_URL") || manifest.runtime.dependencies.iter().any(|d| d == "redis") {
            provision_lxs_redis(ctid, &service_dir)?;
        }

        print_step(&format!("[CT {ctid}] Installed LXS {}@{} as service {}", name, version, service.name));
        installed.push(service.name.clone());
    }
    Ok(installed)
}

// Ensures mongod runs and that configure.sh treats MONGODB_URI as managed
// (ECO_MANAGED_MONGODB_URI=true forces it to write the estate-scoped URI).
fn provision_lxs_mongodb(ctid: &str, service_dir: &str) -> Result<(), String> {
    pct_exec(
        ctid,
        "if command -v systemctl >/dev/null 2>&1; then systemctl restart mongod 2>/dev/null || true; elif command -v service >/dev/null 2>&1; then service mongod restart 2>/dev/null || true; fi",
    )?;
    let example = format!("{service_dir}/.env.example");
    pct_exec(
        ctid,
        &format!(
            "sed -i '/^ECO_MANAGED_MONGODB_URI=/d' {} && echo 'ECO_MANAGED_MONGODB_URI=true' >> {}",
            shell_single_quote(&example),
            shell_single_quote(&example)
        ),
    )?;
    Ok(())
}

// Replicates the postgres data-bootstrap for an LXS service: create the role +
// database, grant permissions, and write DATABASE_* into the service .env.
fn provision_lxs_postgres(ctid: &str, service_dir: &str, service_name: &str, project: &str) -> Result<(), String> {
    let db_name = format!("{}_{}", service_name.replace('-', "_"), project);
    let db_role = format!("{project}_user");
    let env_file = format!("{service_dir}/.env");
    let env_q = shell_single_quote(&env_file);
    let db_q = shell_single_quote(&db_name);
    let script = format!(
        r#"set -e
touch {env_q}
if [[ -z "${{DATABASE_PASSWORD:-}}" ]]; then
  db_password="$(grep -E '^DATABASE_PASSWORD=' {env_q} 2>/dev/null | cut -d'=' -f2- | tr -d '\r' || true)"
  if [[ -z "$db_password" ]]; then
    echo "ERROR: DATABASE_PASSWORD not available for {service_name}; set it in the CT's ~/.bashrc" >&2
    exit 1
  fi
else
  db_password="${{DATABASE_PASSWORD}}"
fi
sed -i '/^DATABASE_PASSWORD=/d' {env_q}
printf 'DATABASE_PASSWORD=%s\n' "$db_password" >> {env_q}
runuser -u postgres -- psql -v ON_ERROR_STOP=1 -c "DO \$\$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{db_role}') THEN CREATE ROLE {db_role} WITH LOGIN; END IF; END \$\$;"
runuser -u postgres -- psql -v ON_ERROR_STOP=1 -c "ALTER ROLE {db_role} WITH LOGIN PASSWORD '$db_password';"
runuser -u postgres -- psql -tAc "SELECT 1 FROM pg_database WHERE datname = '{db_name}'" | grep -q 1 || runuser -u postgres -- createdb -O {db_role} {db_q}
runuser -u postgres -- psql -v ON_ERROR_STOP=1 -d {db_q} -c "GRANT ALL PRIVILEGES ON DATABASE {db_name} TO {db_role};"
runuser -u postgres -- psql -v ON_ERROR_STOP=1 -d {db_q} -c "GRANT ALL ON SCHEMA public TO {db_role};"
sed -i '/^DATABASE_USERNAME=/d' {env_q}
printf 'DATABASE_USERNAME=%s\n' "{db_role}" >> {env_q}
sed -i '/^DATABASE_URL=/d' {env_q}
printf 'DATABASE_URL=postgresql://{db_role}:%s@127.0.0.1:5432/{db_name}\n' "$db_password" >> {env_q}
"#
    );
    pct_exec(ctid, &format!("export LANG=C.UTF-8 LC_ALL=C.UTF-8 PERL_BADLANG=0\n{script}"))?;
    Ok(())
}

// Ensures redis runs and writes the estate-local REDIS_URL for an LXS whose
// contract requires it (chat, notifications fan-out, etc).
fn provision_lxs_redis(ctid: &str, service_dir: &str) -> Result<(), String> {
    pct_exec(
        ctid,
        "if command -v systemctl >/dev/null 2>&1; then systemctl enable redis-server >/dev/null 2>&1 || true; systemctl restart redis-server 2>/dev/null || systemctl restart redis 2>/dev/null || true; elif command -v service >/dev/null 2>&1; then service redis-server restart 2>/dev/null || true; fi",
    )?;
    let env_file = format!("{service_dir}/.env");
    pct_exec(
        ctid,
        &format!(
            "sed -i '/^REDIS_URL=/d' {} && echo 'REDIS_URL=redis://127.0.0.1:6379' >> {}",
            shell_single_quote(&env_file),
            shell_single_quote(&env_file)
        ),
    )?;
    Ok(())
}

pub fn run_up(args: &[String]) -> Result<(), String> {
    if args.first().map(|s| s.as_str()) == Some("dev") {
        return run_up_dev(&args[1..]);
    }
    if args.iter().any(|a| a == "--remote") {
        // Cross-compile the Rust services on this (developer) machine and ship
        // the Linux binaries to the Proxmox host via the eco serve agent.
        return run_up_remote(args);
    }

    if !is_on_proxmox_host() {
        let input = args.iter().find(|a| !a.starts_with("--")).cloned().unwrap_or_else(|| ".".to_string());
        if is_ct_estate_context(&input) {
            return Err(
                "This looks like a deployed estate inside a container (no 'pct' here), so 'eco up' would fall back to local dev mode and rebuild/restart the production estate as if it were a dev machine.\nDeploy from the developer machine with 'eco up --remote' instead."
                    .to_string(),
            );
        }
        util::println_stdout("Not on a Proxmox host (pct not found) — running in dev mode.");
        return run_up_dev(args);
    }

    let (options, positionals) = parse_options(args);
    let input = positionals.first().cloned().unwrap_or_else(|| ".".to_string());
    let cwd = util::current_dir();
    let deployment = load_project_deployment(&input, &cwd)?;
    let staging_config = ecompose::parse_staging(&deployment.content);

    if options.get("staging").map(|v| v == "true").unwrap_or(false) {
        if staging_config.get("ct").map(|s| s.is_empty()).unwrap_or(true) {
            return Err(format!(
                "--staging requested for {}, but ecompose.yml has no staging.ct declared. Add a staging: block (staging.ct: 1000).",
                deployment.project
            ));
        }
        return provision_estate(&deployment, &options, true, &staging_config);
    }

    provision_estate(&deployment, &options, false, &staging_config)?;
    if staging_config.contains_key("ct") && !options.get("prod-only").map(|v| v == "true").unwrap_or(false) {
        util::println_stdout(&format!(
            "\n[eco up] staging block declared (ct {}) — provisioning the staging footprint.",
            staging_config.get("ct").cloned().unwrap_or_default()
        ));
        provision_estate(&deployment, &options, true, &staging_config)?;
    }
    Ok(())
}

pub fn run_expose(args: &[String]) -> Result<(), String> {
    let (options, positionals) = parse_options(args);
    let input = positionals.first().cloned().unwrap_or_else(|| ".".to_string());
    let cwd = util::current_dir();
    let deployment = load_project_deployment(&input, &cwd)?;

    if options.get("dry-run").map(|v| v == "true").unwrap_or(false) {
        let exposure_plan = expose_via_proxy_ct(true, &deployment.expose, &deployment.project, &deployment.ctid, &deployment.ct_config_path)?;
        util::println_stdout("eco expose plan");
        util::println_stdout(&format!("Manifest: {}", deployment.file_path));
        util::println_stdout(&format!("Project root: {}\n", deployment.project_dir.display()));
        for c in &exposure_plan {
            util::println_stdout(c);
        }
        return Ok(());
    }

    ensure_ct_running(&deployment.ctid)?;
    expose_via_proxy_ct(false, &deployment.expose, &deployment.project, &deployment.ctid, &deployment.ct_config_path)?;
    Ok(())
}

