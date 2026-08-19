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
fn is_peer_dependency_resolution_error(result: &util::Captured) -> bool {
    let text = format!("{}\n{}", result.stdout, result.stderr);
    text.contains("ERESOLVE")
        || text.to_lowercase().contains("peer dependency")
        || text.to_lowercase().contains("legacy-peer-deps")
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

fn print_step(message: &str) {
    util::println_stdout(&format!("\n{message}"));
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

fn require_pm2_config_snippet(config_path: &str) -> String {
    let cjs_path = config_path.trim_end_matches(".js").to_string() + ".cjs";
    format!(
        "const config = (() => {{ try {{ return require({}); }} catch (e) {{ return require({}); }} }})();",
        util::json_string(&cjs_path),
        util::json_string(config_path)
    )
}

fn delete_declared_pm2_apps_js(config_path: &str) -> String {
    format!(
        "{}\nconst {{ execSync }} = require('child_process');\nfor (const app of (config.apps || [])) {{\n  if (!app.name) continue;\n  try {{ execSync('pm2 delete ' + JSON.stringify(app.name), {{ stdio: 'ignore' }}); }} catch (e) {{}}\n}}",
        require_pm2_config_snippet(config_path)
    )
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
    // pm2 start returns before the spawned apps are visible in `jlist`; retry
    // briefly so a freshly-started estate doesn't false-negative.
    let mut last_actual: Vec<String> = Vec::new();
    for _ in 0..20 {
        let result = run_capture("pm2", &["jlist".to_string()], cwd)?;
        if result.code != 0 {
            return Err(format!("Unable to verify PM2 services after startup: {}", result.stderr.trim()));
        }
        let processes: serde_json::Value = serde_json::from_str(&result.stdout)
            .map_err(|_| "Unable to parse PM2 service list after startup.".to_string())?;
        last_actual = processes
            .as_array()
            .map(|arr| arr.iter().filter_map(|p| p.get("name").and_then(|n| n.as_str()).map(|s| s.to_string())).collect())
            .unwrap_or_default();
        let missing: Vec<&String> = expected.iter().filter(|name| !last_actual.contains(name)).collect();
        if missing.is_empty() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    let missing: Vec<&String> = expected.iter().filter(|name| !last_actual.contains(name)).collect();
    Err(format!(
        "PM2 did not register declared service(s): {}. Check the generated ecosystem config and service logs.",
        missing.iter().map(|m| m.as_str()).collect::<Vec<_>>().join(", ")
    ))
}

fn extract_pm2_app_names(config_text: &str) -> Vec<String> {
    // naive extraction of name: "..." entries within apps array
    let mut names = Vec::new();
    for line in config_text.split('\n') {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("name:") {
            let val = rest.trim();
            // strip trailing `,` (and any inline comment) after the quoted value
            let val = val.split(',').next().unwrap_or("").trim();
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
    // The eco-init model: a `.eco/state.json` marker makes the project dir the
    // ONLY scanned root (no sibling-domain discovery, no parent estate). Legacy
    // estates without the marker keep the parent-as-estate-root layout.
    let is_init_project = deployment.project_dir.join(".eco").join("state.json").is_file();
    print_lxs_update_notice(&deployment.content, &deployment.project_dir, lxs_check_disabled(&options));
    let estate_root = if is_init_project {
        deployment.project_dir.clone()
    } else {
        deployment
            .project_dir
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| deployment.project_dir.clone())
    };
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
    print_step(&format!("Bootstrapping local PostgreSQL for {}", deployment.project));
    bootstrap_local_postgres(&dev_services, &estate_root, &deployment.project_dir, &deployment.project)?;
    print_step(&format!("Running local Rust migrations for {}", deployment.project));
    run_local_rust_migrations(&dev_services, &estate_root, &deployment.project_dir)?;

    // Install `lxs:` services as local binaries so configure.sh/PM2 can run
    // them in dev, mirroring the CT release path (native arch binary).
    print_step(&format!("Installing local LXS binaries for {}", deployment.project));
    install_lxs_services_local(&deployment, &estate_root)?;

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

    util::println_stdout("\nFollowing PM2 logs — press Ctrl+C to stop\n");
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
        if domain == project || domain == "." {
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
        // Homebrew kegs: postgresql@15 is keg-only, so psql is not on PATH
        // when another postgres version shadows it. Probe the common prefixes.
        "/opt/homebrew/opt/postgresql@15/bin/psql",
        "/usr/local/opt/postgresql@15/bin/psql",
        "/opt/homebrew/opt/postgresql/bin/psql",
        "/usr/local/opt/postgresql/bin/psql",
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

/// Resolve a source service's local directory: service `path:` is relative to
/// the estate repo (project_dir); it may also live directly under the estate
/// root in legacy flat layouts. Prefer project_dir (v2), fall back to root.
fn resolve_local_service_dir(service: &ecompose::Service, estate_root: &Path, project_dir: &Path) -> PathBuf {
    let p = project_dir.join(&service.path);
    if p.is_dir() {
        return p;
    }
    let e = estate_root.join(&service.path);
    if e.is_dir() {
        return e;
    }
    p
}

fn bootstrap_local_postgres(services: &[ecompose::Service], estate_root: &Path, project_dir: &Path, project: &str) -> Result<(), String> {
    let sql_services: Vec<&ecompose::Service> = services.iter().filter(|s| s.runtimes.iter().any(|r| r == "postgresql@15")).collect();
    if sql_services.is_empty() {
        return Ok(());
    }
    let psql = local_postgres_client()?;
    print_step(&format!("Using local psql client: {psql}"));
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
        let env_file = resolve_local_service_dir(service, estate_root, project_dir).join(".env");
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

fn run_local_rust_migrations(services: &[ecompose::Service], estate_root: &Path, project_dir: &Path) -> Result<(), String> {
    let rust_sql: Vec<&ecompose::Service> = services
        .iter()
        .filter(|s| s.runtimes.iter().any(|r| r == "rust") && s.runtimes.iter().any(|r| r == "postgresql@15"))
        .collect();
    for service in rust_sql {
        let service_dir = resolve_local_service_dir(service, estate_root, project_dir);
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

// Remote deploy: the developer machine keeps the estate source locally,
// cross-compiles the Rust services for Linux (x86_64-unknown-linux-musl) with
// the correct env, and ships the binaries over HTTP to the `eco serve` agent
// on the remote host. The agent installs them into the target CT and runs the
// estate deploy without compiling Rust there — removing the shared builder-CT
// contention that a single build CT creates when several estates build
// at once.
// ─────────────────────────────────────────────────────────────────────────────

fn skip_none(_: &str) -> bool {
    false
}

/// Files that can contain developer credentials or workspace-only state must
/// never enter a deploy artifact. This filter is intentionally shared by
/// source-backed runtime packages (Python and plain static sites).
fn skip_sensitive_artifact_entry(name: &str) -> bool {
    name == ".env"
        || name.starts_with(".env.")
        || matches!(
            name,
            ".git"
                | ".eco"
                | ".ssh"
                | ".DS_Store"
                | "node_modules"
                | "target"
                | "__pycache__"
                | ".venv"
                | "venv"
                | ".next"
                | ".vite"
                | ".cache"
        )
        || name.ends_with(".log")
}

fn collect_payload_files(root: &Path, dir: &Path, out: &mut Vec<serde_json::Value>) -> Result<(), String> {
    for entry in std::fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|e| e.to_string())?;
        if file_type.is_symlink() {
            return Err(format!("deploy payload must not contain symlinks: {}", path.display()));
        }
        if file_type.is_dir() {
            collect_payload_files(root, &path, out)?;
            continue;
        }
        if !file_type.is_file() {
            return Err(format!("deploy payload contains unsupported file type: {}", path.display()));
        }
        let rel = path
            .strip_prefix(root)
            .map_err(|_| format!("payload path escaped root: {}", path.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let digest = sha2::Sha256::digest(&bytes);
        out.push(serde_json::json!({
            "path": rel,
            "size": bytes.len(),
            "sha256": crate::registry::hex_encode(&digest),
        }));
    }
    Ok(())
}

fn write_payload_manifest(payload_dir: &Path, project: &str) -> Result<(), String> {
    let mut files = Vec::new();
    collect_payload_files(payload_dir, payload_dir, &mut files)?;
    files.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    files.dedup_by(|a, b| a["path"] == b["path"]);
    let manifest = serde_json::json!({
        "schema_version": 1,
        "project": project,
        "cli_version": env!("CARGO_PKG_VERSION"),
        "target": { "os": "linux", "arch": "amd64" },
        "files": files,
    });
    let text = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
    std::fs::write(payload_dir.join("artifact-manifest.json"), format!("{text}\n"))
        .map_err(|e| format!("write artifact manifest: {e}"))
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
    let response = match ureq::get(url)
        .set("Authorization", &format!("Bearer {api_key}"))
        .set(crate::util::PROTOCOL_HEADER, &crate::util::PROTOCOL_VERSION.to_string())
        .set(crate::util::CLIENT_VERSION_HEADER, env!("CARGO_PKG_VERSION"))
        .call()
    {
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
            .set(crate::util::PROTOCOL_HEADER, &crate::util::PROTOCOL_VERSION.to_string())
            .set(crate::util::CLIENT_VERSION_HEADER, env!("CARGO_PKG_VERSION"))
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
// production CTs never compile anything and production binaries are never built
// on the raw client OS (SOC2/ISO27001). Default driver is Lima
// (`limactl shell <name> --`); falls back to Multipass (`multipass exec`)
// when Lima is absent. Override the exec command with ECO_BUILDER_CMD.
// ─────────────────────────────────────────────────────────────────────────────

fn builder_name() -> String {
    util::env_var_or("ECO_BUILDER", "eco-builder")
}

// The compliance stance: production binaries are built inside an isolated
// Linux builder VM (Lima preferred, Multipass fallback), never on the raw
// client OS. Host mode (`ECO_BUILDER=host`) is an explicit opt-out for
// prototyping only — never the default for a production `eco up --remote`.
fn builder_is_host() -> bool {
    util::env_var_or("ECO_BUILDER", "") == "host"
}

fn limactl_available() -> bool {
    run_capture("limactl", &["--version".to_string()], &util::current_dir())
        .map(|c| c.code == 0)
        .unwrap_or(false)
}

fn multipass_available() -> bool {
    run_capture("multipass", &["version".to_string()], &util::current_dir())
        .map(|c| c.code == 0)
        .unwrap_or(false)
}

// Prefer the Lima driver over Multipass. `ECO_BUILDER_CMD` overrides the whole
// exec command, so no driver-specific dispatch applies when it is set.
fn builder_driver_is_lima() -> bool {
    if !util::env_var_or("ECO_BUILDER_CMD", "").is_empty() {
        return false;
    }
    limactl_available()
}

fn builder_driver_is_available() -> bool {
    if builder_is_host() {
        return true;
    }
    if !util::env_var_or("ECO_BUILDER_CMD", "").is_empty() {
        return true;
    }
    builder_driver_is_lima() || multipass_available()
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

// Resolve the builder VM's home directory. Lima names it after the host user
// (e.g. /home/eco.guest, /home/anggaadiwibowo.linux), so it is queried live
// rather than hardcoded — a freshly provisioned VM on any host user resolves
// correctly. Falls back to the reference builder home when the query fails.
fn builder_home() -> String {
    if builder_is_host() {
        return util::home_dir();
    }
    if builder_driver_is_lima() {
        // `limactl shell <name> -- bash -c 'echo $HOME'`: limactl execs args
        // directly (no login shell), so $HOME must expand inside `bash -c`.
        let args = vec![
            "shell".to_string(),
            builder_name(),
            "--".to_string(),
            "bash".to_string(),
            "-c".to_string(),
            "echo $HOME".to_string(),
        ];
        if let Ok(c) = run_capture("limactl", &args, &util::current_dir()) {
            let home = c.stdout.trim().to_string();
            if !home.is_empty() && home.starts_with('/') {
                return home;
            }
        }
        return "/home/eco.guest".to_string();
    }
    // Multipass builder: reference user home.
    "/home/ubuntu".to_string()
}

// Where node builds land: the VM (queried home) or the host cache.
fn builder_build_root() -> String {
    if builder_is_host() {
        format!("{}/.cache/eco/build", util::home_dir())
    } else if builder_driver_is_lima() {
        format!("{}/build", builder_home())
    } else {
        "/home/ubuntu/build".to_string()
    }
}

fn builder_cmd() -> Vec<String> {
    let cmd = util::env_var_or("ECO_BUILDER_CMD", "");
    if !cmd.is_empty() {
        return cmd.split_whitespace().map(|s| s.to_string()).collect();
    }
    if builder_driver_is_lima() {
        vec!["limactl".to_string(), "shell".to_string(), builder_name(), "--".to_string()]
    } else {
        vec!["multipass".to_string(), "exec".to_string(), builder_name(), "--".to_string()]
    }
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

// Push a local file into the builder VM. Multipass: `multipass transfer`; Lima:
// `limactl copy <local> <name>:<remote>`.
fn builder_transfer_push(local: &Path, remote: &str) -> Result<(), String> {
    let local = local.display().to_string();
    if builder_driver_is_lima() {
        return run_command("limactl", &["copy".to_string(), local, format!("{}:{remote}", builder_name())], &util::current_dir());
    }
    let args = vec!["transfer".to_string(), local, format!("{}:{remote}", builder_name())];
    run_command("multipass", &args, &util::current_dir())
}

// Pull a file from the builder VM back to the host. Multipass:
// `multipass transfer <name>:<remote> <local>`; Lima: `limactl copy`.
fn builder_transfer_pull(remote: &str, local: &Path) -> Result<(), String> {
    let local = local.display().to_string();
    if builder_driver_is_lima() {
        return run_command("limactl", &["copy".to_string(), format!("{}:{remote}", builder_name()), local], &util::current_dir());
    }
    let args = vec!["transfer".to_string(), format!("{}:{remote}", builder_name()), local];
    run_command("multipass", &args, &util::current_dir())
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
    if !util::env_var_or("ECO_BUILDER_CMD", "").is_empty() {
        return true;
    }
    if builder_driver_is_lima() {
        // `limactl list` shows Running only for a started instance.
        let args = vec!["list".to_string(), "-f".to_string(), "{{.Name}} {{.Status}}".to_string()];
        return run_capture("limactl", &args, &util::current_dir())
            .map(|c| c.code == 0 && c.stdout.contains(&format!("{} Running", builder_name())))
            .unwrap_or(false);
    }
    let args = vec!["info".to_string(), builder_name()];
    run_capture("multipass", &args, &util::current_dir()).map(|c| c.code == 0).unwrap_or(false)
}

// ── Builder auto-provision ──────────────────────────────────────────────────
// `eco up --remote` used to fail with "provision it with Lima" instructions
// when no builder VM was reachable. Now it provisions itself: installs Lima
// (Homebrew when present, otherwise a sudo-less limactl binary download),
// starts the eco-builder VM from the bundled template, and bootstraps the
// pinned toolchain inside it. Idempotent — no-ops when everything is up.

// Make dev-tool dirs resolvable for the rest of this eco run: Homebrew on
// Apple Silicon/Intel and ~/.local/bin (where a sudo-less limactl may land).
// Mirrors provision.sh's ensure_brew() exporting PATH to the shell.
fn prepend_tool_paths() {
    let path = std::env::var("PATH").unwrap_or_default();
    let mut candidates = vec![
        "/opt/homebrew/bin".to_string(),
        "/usr/local/bin".to_string(),
        format!("{}/.local/bin", util::home_dir()),
    ];
    candidates.retain(|d| Path::new(d).is_dir());
    let mut new_path: Vec<String> = Vec::new();
    for d in candidates {
        if !path.split(':').any(|p| p == d) {
            new_path.push(d);
        }
    }
    if !new_path.is_empty() {
        let mut joined = new_path.join(":");
        if !path.is_empty() {
            joined.push(':');
            joined.push_str(&path);
        }
        std::env::set_var("PATH", joined);
    }
}

// Ensure a local Linux builder is reachable, provisioning it when missing.
fn ensure_builder() -> Result<(), String> {
    if builder_is_host() {
        return Ok(()); // explicit ECO_BUILDER=host (dev-only) — nothing to provision
    }
    if !util::env_var_or("ECO_BUILDER_CMD", "").is_empty() {
        return Ok(()); // custom builder exec — provisioning is the caller's job
    }

    prepend_tool_paths();

    if !limactl_available() {
        if multipass_available() {
            // Multipass fallback: start an existing VM, but never create one.
            if !builder_available() {
                return Err(format!(
                    "Multipass builder `{}` is not running. Start it (`multipass start {}`), or install Lima (`brew install lima`) so eco can provision it automatically.",
                    builder_name(),
                    builder_name()
                ));
            }
            return Ok(());
        }
        print_step("No local Linux builder VM is reachable — provisioning Lima (one-time)");
        ensure_lima_installed()?;
    }

    ensure_eco_builder_vm()
}

fn ensure_lima_installed() -> Result<(), String> {
    if limactl_available() {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        if Path::new("/opt/homebrew/bin/brew").is_file() || util::command_on_path("brew") {
            print_step("Installing Lima via Homebrew (brew install lima)…");
            run_host_command("brew", &["install".to_string(), "lima".to_string()])?;
        } else {
            // No Homebrew (and maybe no sudo for one) — download the limactl
            // binary directly into ~/.local/bin. Sudo-free.
            print_step("Homebrew not found — downloading the limactl binary directly (no sudo needed)…");
            install_limactl_binary()?;
        }
        if !limactl_available() {
            return Err("limactl still not found after installing Lima. Open a new terminal and re-run `eco up --remote`.".to_string());
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        print_step("Installing Lima via apt (apt-get install lima)…");
        run_command("sudo", &["apt-get".to_string(), "install".to_string(), "-y".to_string(), "lima".to_string()], &util::current_dir())?;
        if !limactl_available() {
            return Err("Lima not available after apt install. Install it from https://github.com/lima-vm/lima/releases, or set ECO_BUILDER=host (dev only).".to_string());
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err("Lima is supported on macOS and Linux only. Install a builder VM manually or set ECO_BUILDER=host (dev only).".to_string())
    }
}

// Run a host command with the usual dev-tool dirs (Homebrew, ~/.local/bin)
// prepended to PATH, so brew/limactl resolve regardless of the caller's shell.
fn run_host_command(command: &str, args: &[String]) -> Result<(), String> {
    let mut env = std::env::vars().collect::<HashMap<String, String>>();
    let path = env.get("PATH").cloned().unwrap_or_default();
    let extra = ["/opt/homebrew/bin", "/usr/local/bin", &format!("{}/.local/bin", util::home_dir())]
        .iter()
        .filter(|d| Path::new(d).is_dir())
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    let mut new_path = extra;
    if !path.is_empty() {
        new_path.push(path);
    }
    env.insert("PATH".to_string(), new_path.join(":"));
    util::run_command_env(command, args, &util::current_dir(), &env)
}

// Sudo-free fallback: download the pinned limactl binary from the GitHub
// release into ~/.local/bin so eco can provision the builder VM even on a
// machine with no Homebrew and no sudo.
fn install_limactl_binary() -> Result<(), String> {
    const LIMA_VERSION: &str = "2.2.0";
    #[cfg(target_os = "macos")]
    let os_arch = ("Darwin".to_string(), lima_arch());
    #[cfg(target_os = "linux")]
    let os_arch = ("Linux".to_string(), lima_arch());
    let (os, arch) = os_arch;
    let url = format!("https://github.com/lima-vm/lima/releases/download/v{LIMA_VERSION}/lima-{LIMA_VERSION}-{os}-{arch}.tar.gz");
    let tarball = std::env::temp_dir().join(format!("lima-{LIMA_VERSION}-{os}-{arch}.tar.gz"));
    let extract = std::env::temp_dir().join("lima-download");
    let bin_dir = PathBuf::from(format!("{}/.local/bin", util::home_dir()));

    print_step(&format!("Downloading limactl {LIMA_VERSION} ({os}/{arch})…"));
    download_to(&url, &tarball)?;
    std::fs::create_dir_all(&extract).map_err(|e| format!("create {}: {e}", extract.display()))?;
    run_command("tar", &["xzf".to_string(), tarball.display().to_string(), "-C".to_string(), extract.display().to_string()], &util::current_dir())?;
    let limactl = extract.join("lima").join("bin").join("limactl");
    if !limactl.is_file() {
        return Err(format!("limactl binary not found after extracting {}", tarball.display()));
    }
    std::fs::create_dir_all(&bin_dir).map_err(|e| format!("create {}: {e}", bin_dir.display()))?;
    std::fs::copy(&limactl, bin_dir.join("limactl")).map_err(|e| format!("copy limactl: {e}"))?;
    std::fs::remove_dir_all(&extract).ok();
    std::fs::remove_file(&tarball).ok();
    print_step(&format!("limactl installed at {}", bin_dir.join("limactl").display()));
    Ok(())
}

// Lima release assets name arch arm64/x86_64 (util::arch() is arm64/x64).
fn lima_arch() -> String {
    match util::arch().as_str() {
        "x64" => "x86_64".to_string(),
        other => other.to_string(),
    }
}

fn download_to(url: &str, dest: &Path) -> Result<(), String> {
    let args = vec!["-fsSL".to_string(), url.to_string(), "-o".to_string(), dest.display().to_string()];
    run_command("curl", &args, &util::current_dir())
}

// Create + start the eco-builder VM from the bundled template, then bootstrap
// the pinned toolchain inside it (limactl start on a running VM is a no-op;
// the bootstrap script self-checks every tool, so both are re-runnable).
fn ensure_eco_builder_vm() -> Result<(), String> {
    if builder_available() {
        return Ok(());
    }
    let yml = embedded::bundled_script_path("eco-builder.lima.yml")?;
    print_step(&format!(
        "Starting builder VM `{}` (first run downloads the Ubuntu image — a few minutes)…",
        builder_name()
    ));
    run_command(
        "limactl",
        &[
            "start".to_string(),
            "--name".to_string(),
            builder_name(),
            "--tty=false".to_string(),
            yml.display().to_string(),
        ],
        &util::current_dir(),
    )?;
    if !builder_available() {
        return Err(format!("Builder VM `{}` is not Running after `limactl start`.", builder_name()));
    }

    // Bootstrap the pinned toolchain inside the VM (idempotent).
    let bootstrap = embedded::bundled_script_path("eco-builder-bootstrap.sh")?;
    run_command(
        "limactl",
        &[
            "copy".to_string(),
            bootstrap.display().to_string(),
            format!("{}:/tmp/eco-builder-bootstrap.sh", builder_name()),
        ],
        &util::current_dir(),
    )?;
    print_step(&format!(
        "Bootstrapping pinned toolchain in `{}` (rust, zig, node, bun — first run takes a while)…",
        builder_name()
    ));
    run_command(
        "limactl",
        &[
            "shell".to_string(),
            builder_name(),
            "--".to_string(),
            "bash".to_string(),
            "/tmp/eco-builder-bootstrap.sh".to_string(),
        ],
        &util::current_dir(),
    )?;

    print_step(&format!("Builder VM `{}` ready (toolchain bootstrapped).", builder_name()));
    Ok(())
}

fn skip_build_sync(name: &str) -> bool {
    // Build/dev artifacts must never be synced into the remote build dir: a
    // stale local `.next`/`.vite` can carry dev-baked public URLs (localhost)
    // into a production frontend build. Regenerable — the build recreates them.
    ["node_modules", "target", ".git", ".env", ".env.local", ".next", ".vite", ".cache", ".eco"].contains(&name)
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
    builder_transfer_push(&tar_path, &remote_tar)?;
    builder_exec_ok(&format!("mkdir -p {} && tar xzf {} -C {}", shell_single_quote(dest), remote_tar, shell_single_quote(dest)))?;
    let _ = std::fs::remove_file(&tar_path);
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
    // SvelteKit adapter-node that will be bun-compiled ships only the compiled
    // binary + a static `client/` dir (the recipe copies build/client next to
    // the binary); the raw `build/` tree would duplicate the client and blow
    // the payload past the cap. `included` still lists `build` so the SSR/bun
    // path is taken; the copy below skips it.
    let sveltekit_bun = builder_is_host()
        && util::command_on_path("bun")
        && Path::new(build_dir).join("build").join("client").is_dir();
    let subdirs = ["dist", "build", ".next", ".output", "output", "app/dist", "app/.next", "app/build", "app/.output", "public", "app/public"];
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
        // Plain Node service (no framework build): Bun-compile the entry into
        // a single linux-x64 binary so the CT still gets an executable-only
        // artifact (no node_modules, no source). Entry is the `start`/`main`
        // script target or the conventional index/server file.
        let entry_candidates = ["index.js", "server.js", "main.js", "app.js", "src/index.js", "src/server.js", "src/main.js"];
        let entry = entry_candidates
            .iter()
            .map(|p| Path::new(build_dir).join(p))
            .find(|p| p.is_file());
        if let Some(entry) = entry {
            if builder_is_host() && util::command_on_path("bun") {
                let out = artifact_dir.join(service_name);
                print_step(&format!("Bun-compiling plain Node {} -> single linux-x64 binary", service_name));
                let rel = entry
                    .strip_prefix(Path::new(build_dir))
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| entry.display().to_string());
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
                return Ok(());
            }
        }
        return Err(format!("builder produced no dist/build/.next/output under {build_dir}"));
    }
    if builder_is_host() {
        for sub in &included {
            // SvelteKit bun builds ship the compiled binary + a static
            // `client/` dir; the raw build/ tree (with its own client copy)
            // would double the payload.
            if sveltekit_bun && sub == "build" {
                continue;
            }
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
        // Next.js (`.next`), Astro (`dist/server/entry.mjs`) and SvelteKit
        // (`build/index.js`) SSR frontends need their node_modules on the CT at
        // runtime. Ship the manifest files so the CT can `npm ci` (native
        // modules are linux).
        if included.iter().any(|s| s == ".next" || s == "app/.next")
            || Path::new(build_dir).join("dist/server/entry.mjs").is_file()
            || Path::new(build_dir).join("app/dist/server/entry.mjs").is_file()
            || Path::new(build_dir).join("build/index.js").is_file()
        {
            for f in ["package.json", "package-lock.json", "npm-shrinkwrap.json"] {
                let src = Path::new(build_dir).join(f);
                if src.is_file() {
                    std::fs::copy(&src, artifact_dir.join(f)).map_err(|e| e.to_string())?;
                }
            }
            // Workspace layouts (Astro/SvelteKit under `app/`): ship the
            // workspace member manifest too, or npm ci can't resolve deps.
            for f in ["package.json", "package-lock.json"] {
                let src = Path::new(build_dir).join("app").join(f);
                if src.is_file() {
                    std::fs::copy(&src, artifact_dir.join("app").join(f)).map_err(|e| e.to_string())?;
                }
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
        builder_transfer_pull(&remote_tar, Path::new("/tmp/eco-builder-artifact.tar.gz"))?;
        run_command("tar", &["xzf".to_string(), "/tmp/eco-builder-artifact.tar.gz".to_string(), "-C".to_string(), artifact_dir.display().to_string()], &util::current_dir())?;
        builder_exec(&format!("rm -f {}", remote_tar))?;
    }
    // Bun-compile SSR node apps (host builder mode) into a single linux-x64
    // binary so the CT needs no node_modules. The build output's server entry
    // imports runtime deps from node_modules, so the compile runs where npm ci
    // ran (the build dir), not in the copied artifact.
    //
    // SvelteKit adapter-node builds (build/index.js + build/client) are handled
    // specially: `bun build --compile` embeds only the server, so the client
    // assets (served from disk by adapter-node) must be embedded separately and
    // materialized at startup. prepare_sveltekit_bun_recipe patches the built
    // handler to read the asset dir from env and emits build-eco/ (base64
    // client chunks + a wrapper entry). The wrapper is the compile entry.
    if builder_is_host() && util::command_on_path("bun") {
        let sveltekit_client = Path::new(build_dir).join("build").join("client");
        let is_sveltekit = builder_is_host() && sveltekit_client.is_dir();
        if is_sveltekit {
            print_step(&format!(
                "SvelteKit adapter-node build ({}): preparing self-contained bun recipe",
                sveltekit_client.display()
            ));
            prepare_sveltekit_bun_recipe(Path::new(build_dir))?;
        }
        let server_entry = [
            "build-eco/eco-entry.js",
            "build/index.js",
            "build/server.js",
            "build/index.mjs",
            "build/index.cjs",
        ]
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
            // SvelteKit adapter-node: ship the client assets as static files
            // next to the binary. The wrapper points the server at
            // dirname(execPath)/client, so no node_modules ever reach the CT.
            if is_sveltekit {
                copy_tree_excluding(&sveltekit_client, &artifact_dir.join("client"), &|_| false)?;
                print_step(&format!(
                    "SvelteKit client assets shipped next to binary ({} files)",
                    count_tree_files(&sveltekit_client)
                ));
            }
        }
    }
    Ok(())
}

fn count_tree_files(dir: &Path) -> usize {
    let mut n = 0;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.path().is_dir() {
                n += count_tree_files(&e.path());
            } else {
                n += 1;
            }
        }
    }
    n
}

// Cross-compile a Rust service to x86_64-unknown-linux-musl inside the local
// Linux builder VM, then pull the binary back to a temp dir. Mirrors the
// host-side zigbuild path (workspace `-p` handling, target-dir resolution) but
// keeps production binaries out of the raw client OS — the SOC2/ISO27001 fix.
fn cross_compile_rust_on_builder(service: &str, dir: &Path, package: &str, build_env: &[(String, String)]) -> Result<PathBuf, String> {
    if !builder_driver_is_available() {
        return Err(format!(
            "{} is a Rust service but no local Linux builder VM is reachable. Provision it with Lima (`brew install lima && limactl start --name eco-builder scripts/eco-builder.lima.yml`), or set ECO_BUILDER=host to build on this machine (dev only).",
            service
        ));
    }
    if !builder_available() {
        return Err(format!(
            "{} is a Rust service but the local builder VM `{}` is not running. Start it (`limactl start {}`).",
            service,
            builder_name(),
            builder_name()
        ));
    }
    // Sync the source into the VM build root (never a live mount — copy-in so
    // no host folder can be swapped mid-build / TOCTOU).
    let build_dir = format!("{}/{}", builder_build_root(), service);
    print_step(&format!("Syncing {} source into builder VM ({})", service, build_dir));
    sync_dir_to_builder(dir, &build_dir)?;
    // Determine the workspace root from INSIDE the VM so `-p` builds resolve
    // against the synced layout, not the host paths.
    let workspace_root = builder_exec(&format!(
        "cd {} && cargo metadata --no-deps --format-version 1 2>/dev/null | grep -o '\"workspace_root\":\"[^\"]*\"' | head -1 | cut -d'\"' -f4 || true",
        shell_single_quote(&build_dir)
    ))
    .ok()
    .map(|c| c.stdout.trim().to_string())
    .filter(|s| !s.is_empty() && Path::new(s).is_absolute())
    .unwrap_or_default();
    let workspace_build = !workspace_root.is_empty() && workspace_root != build_dir;
    // Export the same build env the host path used (SQLX_OFFLINE, PUBLIC_*),
    // plus a PATH that finds zig/cargo-zigbuild inside the VM.
    let mut exports = String::from("set -euo pipefail\n");
    for (key, value) in build_env {
        if is_shell_ident(key) {
            exports.push_str(&format!("export {}={}\n", key, shell_single_quote(value)));
        }
    }
    // The VM's zig lives in /usr/local/bin and rustup in ~/.cargo/bin; prepend
    // them to PATH before running cargo so non-login `bash -c` finds them.
    exports.push_str("export PATH=\"$HOME/.cargo/bin:/usr/local/bin:$PATH\"\n");
    let build_args = if workspace_build {
        format!("cargo zigbuild --release -p {} --target x86_64-unknown-linux-musl", shell_single_quote(package))
    } else {
        format!("cargo zigbuild --release --target x86_64-unknown-linux-musl")
    };
    let script = format!("cd {} && {exports}{build_args}", shell_single_quote(&build_dir));
    builder_exec_ok(&script)?;
    // Resolve the real target dir inside the VM (workspace builds land in the
    // workspace root's target/), then pull the binary back.
    let target_dir = if workspace_build {
        builder_exec(&format!("cd {} && cargo metadata --no-deps --format-version 1 2>/dev/null | grep -o '\"target_directory\":\"[^\"]*\"' | head -1 | cut -d'\"' -f4 || true", shell_single_quote(&workspace_root)))
            .ok()
            .map(|c| c.stdout.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("{workspace_root}/target"))
    } else {
        format!("{build_dir}/target")
    };
    let remote_bin = format!("{target_dir}/x86_64-unknown-linux-musl/release/{package}");
    let local_dir = std::env::temp_dir().join(format!("eco-rust-builder-{}-{}", service, std::process::id()));
    let _ = std::fs::remove_dir_all(&local_dir);
    std::fs::create_dir_all(&local_dir).map_err(|e| format!("create {}: {e}", local_dir.display()))?;
    let local_bin = local_dir.join(package);
    let exists = builder_exec(&format!("test -f {} && echo yes || echo no", shell_single_quote(&remote_bin)))
        .map(|c| c.stdout.trim() == "yes")
        .unwrap_or(false);
    if !exists {
        return Err(format!("cross-compiled binary not found in builder VM: {remote_bin}"));
    }
    builder_transfer_pull(&remote_bin, &local_bin)?;
    if !local_bin.is_file() {
        return Err(format!("failed to pull cross-compiled binary from builder VM: {remote_bin}"));
    }
    print_step(&format!("Pulled {} binary from builder VM ({} bytes)", package, std::fs::metadata(&local_bin).map(|m| m.len()).unwrap_or(0)));
    Ok(local_bin)
}

/// Prepare a SvelteKit adapter-node build for bun-compilation by running the
/// bundled recipe script: patch build/handler.js to serve client assets from
/// an env-configurable dir, and emit build-eco/ (embedded base64 client chunks
/// + a wrapper entry that materializes them and starts the server).
fn prepare_sveltekit_bun_recipe(build_dir: &Path) -> Result<(), String> {
    let recipe = build_dir.join("sveltekit-bun-recipe.mjs");
    std::fs::write(&recipe, crate::embedded::SVELTEKIT_BUN_RECIPE_MJS)
        .map_err(|e| format!("write sveltekit recipe: {e}"))?;
    let result = run_capture(
        "node",
        &[
            recipe.display().to_string(),
            build_dir.display().to_string(),
        ],
        &util::current_dir(),
    )?;
    if result.code != 0 {
        let detail = if result.stderr.trim().is_empty() {
            result.stdout.trim().to_string()
        } else {
            result.stderr.trim().to_string()
        };
        return Err(format!("SvelteKit bun recipe failed: {detail}"));
    }
    if result.stdout.trim().contains("sveltekit bun recipe ready") {
        print_step(result.stdout.trim());
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

fn compute_frontend_input_hash(service_dir: &Path, build_env: &[(String, String)]) -> Result<String, String> {
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
    // Build-time public env vars (NEXT_PUBLIC_*/VITE_*/PUBLIC_*) are baked
    // into the output, so a change to them must invalidate the cached build —
    // otherwise the hash-skip reuses a stale artifact (e.g. an old localhost
    // API URL) even though the CT .env now resolves the real public address.
    for (key, value) in build_env {
        if key.starts_with("NEXT_PUBLIC_") || key.starts_with("VITE_") || key.starts_with("PUBLIC_") {
            combined.push_str(&format!("{key}={value}\n"));
        }
    }
    Ok(if combined.is_empty() {
        String::new()
    } else {
        format!("{:x}", sha2::Sha256::digest(combined.as_bytes()))
    })
}

// Hash of ONLY the build-time public env vars. Frontend compilers inline these
// into the output AND keep their own incremental cache (Next.js .next/cache,
// Vite .vite/cache) that is keyed on source, not on env — so after an env
// change a rebuild reuses stale chunks that still contain the old value (e.g.
// an old localhost API URL). The CLI tracks this hash in the build dir and
// clears the compiler cache whenever it changes, forcing a full recompile.
fn compute_frontend_env_hash(build_env: &[(String, String)]) -> String {
    let mut combined = String::new();
    for (key, value) in build_env {
        if key.starts_with("NEXT_PUBLIC_") || key.starts_with("VITE_") || key.starts_with("PUBLIC_") {
            combined.push_str(&format!("{key}={value}\n"));
        }
    }
    if combined.is_empty() {
        String::new()
    } else {
        format!("{:x}", sha2::Sha256::digest(combined.as_bytes()))
    }
}

pub fn run_up_remote(args: &[String]) -> Result<(), String> {
    let (options, positionals) = parse_options(args);
    let input = positionals.first().cloned().unwrap_or_else(|| ".".to_string());
    let cwd = util::current_dir();
    let deployment = load_project_deployment(&input, &cwd)?;
    print_lxs_update_notice(&deployment.content, &deployment.project_dir, lxs_check_disabled(&options));
    // API URL + key: explicit env wins, else the `eco login`-stored auth
    // (defaulting the URL to the public api.getecosphere.com).
    let (api_url, api_key) = crate::commands::account::resolve_api_credentials()?;
    let api_url = if api_url.is_empty() { "https://api.getecosphere.com".to_string() } else { api_url };
    if api_url.is_empty() {
        return Err(
            "eco up --remote requires ECO_API_URL pointing at the eco serve agent on the remote host (e.g. http://host:8790).".to_string(),
        );
    }
    if api_key.is_empty() {
        return Err("eco up --remote requires an API key (run `eco login`, or set ECO_API_KEY).".to_string());
    }
    let base = api_url.trim_end_matches('/').to_string();
    let staging = options.get("staging").map(|v| v == "true").unwrap_or(false);

    // Fail fast on protocol mismatch BEFORE building the payload: ask the
    // agent its protocol, and require an exact match (a stale client could
    // otherwise ship a payload the agent mis-reads).
    let health_url = format!("{base}/v1/health");
    if let Ok(health_text) = agent_client_get(&health_url, &api_key) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&health_text) {
            let agent_protocol = v.get("protocol").and_then(|p| p.as_u64()).unwrap_or(0) as u32;
            if agent_protocol != 0 && agent_protocol != crate::util::PROTOCOL_VERSION {
                let agent_semver = v.get("version").and_then(|s| s.as_str()).unwrap_or("(unknown)");
                return Err(crate::util::protocol_mismatch_msg(
                    &crate::util::PROTOCOL_VERSION.to_string(),
                    env!("CARGO_PKG_VERSION"),
                    agent_semver,
                ));
            }
        }
    }
    let staging_config = ecompose::parse_staging(&deployment.content);
    if staging && staging_config.get("ct").map(|s| s.is_empty()).unwrap_or(true) {
        return Err(format!(
            "--staging requested for {}, but ecompose.yml has no staging.ct declared. Add a staging: block (staging.ct: 1000).",
            deployment.project
        ));
    }
    let deploy_query = if staging { "?staging=1" } else { "" };
    let project_dir_str = deployment.project_dir.display().to_string();

    // Pair each declared Rust service with its local crate directory and its
    // CT-relative path (where the binary and .eco-rust-hash live on the CT).
    // `path:` is relative to the repo root (project_dir) and may point outside
    // the repo (`../bidding`) — the developer arranges their own folders.
    let mut rust_targets: Vec<(ecompose::Service, String, PathBuf)> = Vec::new();
    for service in deployment
        .services
        .iter()
        .filter(|s| !s.path.is_empty() && s.runtimes.iter().any(|r| r == "rust"))
    {
        let rel = relative_ct_service_path(&service.path, &deployment.project, &project_dir_str, "");
        let rel = if rel.is_empty() { service.path.clone() } else { rel };
        let candidate = deployment.project_dir.join(&rel);
        if !candidate.join("Cargo.toml").is_file() {
            return Err(format!(
                "Cannot find local crate for Rust service {} (looked at {})",
                service.name,
                candidate.display()
            ));
        }
        rust_targets.push((service.clone(), rel, candidate));
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
        .filter(|s| !s.path.is_empty() && s.runtimes.iter().any(|r| r == "npm" || r.starts_with("node@") || r == "leptos" || r == "static"))
    {
        let rel = relative_ct_service_path(&service.path, &deployment.project, &project_dir_str, "");
        let rel = if rel.is_empty() { service.path.clone() } else { rel };
        let candidate = deployment.project_dir.join(&rel);
        // Node frontends have package.json; Leptos/Rust frontends have
        // Cargo.toml + index.html (the trunk CSR entry); static sites have
        // Cargo.toml + index.html (served as plain dist) or start.sh.
        let is_leptos = service.runtimes.iter().any(|r| r == "leptos");
        let is_static = service.runtimes.iter().any(|r| r == "static");
        let ok = if is_leptos {
            candidate.join("Cargo.toml").is_file() && candidate.join("index.html").is_file()
        } else if is_static {
            candidate.join("index.html").is_file()
        } else {
            candidate.join("package.json").is_file()
        };
        if !ok {
            return Err(format!(
                "Cannot find local {} for service {} (looked at {})",
                if is_leptos { "Cargo.toml + index.html (Leptos)" } else if is_static { "index.html (static)" } else { "package.json" },
                service.name,
                candidate.display()
            ));
        }
        frontend_targets.push((service.clone(), rel, candidate));
    }
    if rust_targets.is_empty() && frontend_targets.is_empty() {
        // Not necessarily an error: an estate may ship only Go/Spring/LXS
        // services with no Rust or Node. Continue so those still deploy.
        print_step("no Rust or Node services to build (continuing with Go/Spring/LXS artifacts)");
    }

    // Go services: prebuilt static binaries. `go build` targets linux/amd64
    // directly (Go cross-compiles out of the box with CGO_ENABLED=0).
    let mut go_targets: Vec<(ecompose::Service, String, PathBuf)> = Vec::new();
    for service in deployment
        .services
        .iter()
        .filter(|s| !s.path.is_empty() && s.runtimes.iter().any(|r| r == "go"))
    {
        let rel = relative_ct_service_path(&service.path, &deployment.project, &project_dir_str, "");
        let rel = if rel.is_empty() { service.path.clone() } else { rel };
        let candidate = deployment.project_dir.join(&rel);
        if !candidate.join("go.mod").is_file() {
            return Err(format!(
                "Cannot find local Go module for service {} (looked at {})",
                service.name,
                candidate.display()
            ));
        }
        go_targets.push((service.clone(), rel, candidate));
    }

    // Spring Boot services: built fat jar shipped to the CT.
    let mut spring_targets: Vec<(ecompose::Service, String, PathBuf)> = Vec::new();
    for service in deployment
        .services
        .iter()
        .filter(|s| !s.path.is_empty() && s.runtimes.iter().any(|r| r == "java@17" || r == "maven"))
    {
        let rel = relative_ct_service_path(&service.path, &deployment.project, &project_dir_str, "");
        let rel = if rel.is_empty() { service.path.clone() } else { rel };
        let candidate = deployment.project_dir.join(&rel);
        if !candidate.join("pom.xml").is_file() {
            return Err(format!(
                "Cannot find local Maven project for service {} (looked at {})",
                service.name,
                candidate.display()
            ));
        }
        spring_targets.push((service.clone(), rel, candidate));
    }

    // Python services: source + vendored manylinux wheels shipped to the CT
    // (pip download --only-binary => prebuilt linux binaries, never built on
    // the server). The CT's python3 runs the app with PYTHONPATH=vendored.
    let mut python_targets: Vec<(ecompose::Service, String, PathBuf)> = Vec::new();
    for service in deployment
        .services
        .iter()
        .filter(|s| !s.path.is_empty() && s.runtimes.iter().any(|r| r == "python" || r.starts_with("python@")))
    {
        let rel = relative_ct_service_path(&service.path, &deployment.project, &project_dir_str, "");
        let rel = if rel.is_empty() { service.path.clone() } else { rel };
        let candidate = deployment.project_dir.join(&rel);
        if !candidate.join("manage.py").is_file() && !candidate.join("app.py").is_file() {
            return Err(format!(
                "Cannot find local Python entry (manage.py or app.py) for service {} (looked at {})",
                service.name,
                candidate.display()
            ));
        }
        python_targets.push((service.clone(), rel, candidate));
    }

    // dotnet services: `dotnet publish` a self-contained linux-x64 single-file
    // executable on the dev machine (the build farm) and ship that binary.
    let mut dotnet_targets: Vec<(ecompose::Service, String, PathBuf)> = Vec::new();
    for service in deployment
        .services
        .iter()
        .filter(|s| !s.path.is_empty() && s.runtimes.iter().any(|r| r == "dotnet" || r.starts_with("dotnet@")))
    {
        let rel = relative_ct_service_path(&service.path, &deployment.project, &project_dir_str, "");
        let rel = if rel.is_empty() { service.path.clone() } else { rel };
        let candidate = deployment.project_dir.join(&rel);
        let has_csproj = std::fs::read_dir(&candidate)
            .map(|rd| rd.flatten().any(|e| e.path().extension().map(|x| x == "csproj").unwrap_or(false)))
            .unwrap_or(false);
        if !has_csproj {
            return Err(format!(
                "Cannot find a *.csproj for dotnet service {} (looked at {})",
                service.name,
                candidate.display()
            ));
        }
        dotnet_targets.push((service.clone(), rel, candidate));
    }

    if options.get("dry-run").map(|v| v == "true").unwrap_or(false) {
        print_step(&format!("remote deploy plan for {}{} (dry-run)", deployment.project, if staging { " (staging)" } else { "" }));
        if staging {
            print_step(&format!("target CT: {} (staging.getecosphere.com style footprint)", staging_config.get("ct").cloned().unwrap_or_default()));
        }
        print_step(&format!("agent: {base}"));
        print_step("cross-toolchain: x86_64-unknown-linux-musl via cargo-zigbuild");
        for (service, _, dir) in &rust_targets {
            if builder_is_host() {
                print_step(&format!("cross-compile {} from {} and ship binary", service.name, dir.display()));
            } else {
                print_step(&format!("cross-compile {} in builder VM ({}): zigbuild x86_64-unknown-linux-musl", service.name, builder_name()));
            }
        }
        for (service, _, dir) in &frontend_targets {
            print_step(&format!(
                "build {} on local builder ({}): npm ci + npm run build, ship dist",
                service.name,
                builder_name()
            ));
            print_step(&format!("  local source: {}", dir.display()));
        }
        for (service, _, dir) in &go_targets {
            print_step(&format!("cross-compile Go {} from {} and ship binary", service.name, dir.display()));
        }
        for (service, _, dir) in &spring_targets {
            print_step(&format!("build Spring Boot {} from {} and ship jar", service.name, dir.display()));
        }
        return Ok(());
    }

    let zig_dir: Option<PathBuf> = if builder_is_host() {
        ensure_cross_toolchain()?
    } else {
        // Rust cross-compiles run inside the builder VM (pinned toolchain) —
        // no zig/cargo-zigbuild needs to live on the client machine.
        None
    };
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

    // Fetch only public build-time values for frontends. Native builds never
    // receive a production service environment: runtime secrets stay on the
    // server and are injected into the activated release there.
    for (service, _, dir) in &frontend_targets {
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
        if let Some(text) = env_text {
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

    // Provision the local Linux builder (Lima eco-builder VM) when a remote
    // build needs one and none is present — installs Lima + starts the VM +
    // bootstraps the toolchain. No-op when already up.
    let needs_builder = !rust_targets.is_empty() || !frontend_targets.is_empty();
    if needs_builder {
        ensure_builder()?;
    }

    // Cross-compile each service and collect artifacts + source hashes.
    let mut artifacts: Vec<(String, String, PathBuf)> = Vec::new();
    let mut hash_lines: Vec<String> = Vec::new();
    for (service, rel, dir) in &rust_targets {
        let cargo_text = std::fs::read_to_string(dir.join("Cargo.toml")).map_err(|e| format!("read {}: {e}", dir.join("Cargo.toml").display()))?;
        let Some(package) = cargo_package_name(&cargo_text) else {
            print_step(&format!("Skipping {}: no [package] binary name", service.name));
            continue;
        };
        print_step(&format!("Cross-compiling {} ({package}) for x86_64-unknown-linux-musl", service.name));
        // Compliance default: production binaries are cross-compiled inside the
        // isolated Linux builder VM. Host zigbuild is the explicit ECO_BUILDER=host
        // (dev-only) path.
        if !builder_is_host() {
            let binary = cross_compile_rust_on_builder(&service.name, dir, &package, &build_env)?;
            artifacts.push((service.name.clone(), package.clone(), binary));
            let hash = compute_rust_input_hash(dir)?;
            hash_lines.push(format!("{rel} {hash}"));
            continue;
        }
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
        artifacts.push((service.name.clone(), package.clone(), binary));
        let hash = compute_rust_input_hash(dir)?;
        hash_lines.push(format!("{rel} {hash}"));
    }
    if artifacts.is_empty() && frontend_targets.is_empty() && go_targets.is_empty() && spring_targets.is_empty() && python_targets.is_empty() && dotnet_targets.is_empty() {
        return Err("no Rust/Go/Spring/Python/dotnet binaries or frontend dist were produced; aborting remote deploy.".to_string());
    }

    // Cross-compile Go services to a static linux/amd64 binary (Go builds
    // cross-platform out of the box with CGO_ENABLED=0).
    for (service, rel, dir) in &go_targets {
        let bin_name = if !service.binary.is_empty() { service.binary.clone() } else { service.name.clone() };
        print_step(&format!("Cross-compiling Go {} for linux/amd64", service.name));
        let out_bin = std::env::temp_dir()
            .join(format!("eco-go-{}-{}", service.name, std::process::id()))
            .join(&bin_name);
        std::fs::create_dir_all(out_bin.parent().unwrap()).map_err(|e| e.to_string())?;
        let mut go_env: Vec<(String, String)> = vec![
            ("GOOS".to_string(), "linux".to_string()),
            ("GOARCH".to_string(), "amd64".to_string()),
            ("CGO_ENABLED".to_string(), "0".to_string()),
        ];
        // Merge any build_env that makes sense (e.g. injected .env is not needed
        // for Go builds — the binary reads env at runtime).
        for (k, v) in build_env.iter() {
            if k == "PATH" {
                go_env.push((k.clone(), v.clone()));
            }
        }
        run_command_env("go", &["build".to_string(), "-o".to_string(), out_bin.display().to_string(), ".".to_string()], dir, &go_env)?;
        if !out_bin.is_file() {
            return Err(format!("Go build for {} did not produce {}", service.name, out_bin.display()));
        }
        artifacts.push((service.name.clone(), bin_name, out_bin));
        let hash = compute_rust_input_hash(dir).unwrap_or_else(|_| "go".to_string());
        hash_lines.push(format!("{rel} {hash}"));
    }

    // Python services: vendor manylinux wheels locally (pip download
    // --only-binary) and ship source + vendored/ to the CT. Wheels are
    // prebuilt linux binaries — nothing is installed or built on the server.
    let mut python_artifacts: Vec<(String, PathBuf)> = Vec::new();
    for (service, rel, dir) in &python_targets {
        print_step(&format!("Vendoring Python deps for {} (manylinux wheels)", service.name));
        let artifact_dir = std::env::temp_dir().join(format!("eco-python-{}-{}", service.name, std::process::id()));
        let _ = std::fs::remove_dir_all(&artifact_dir);
        std::fs::create_dir_all(&artifact_dir).map_err(|e| e.to_string())?;
        copy_tree_excluding(
            dir,
            &artifact_dir,
            &(skip_sensitive_artifact_entry as fn(&str) -> bool),
        )?;
        let requirements = dir.join("requirements.txt");
        if requirements.is_file() {
            let vendored = artifact_dir.join("vendored");
            std::fs::create_dir_all(&vendored).map_err(|e| e.to_string())?;
            let pip_args = [
                "-m".to_string(),
                "pip".to_string(),
                "install".to_string(),
                "--target".to_string(),
                vendored.display().to_string(),
                "--only-binary=:all:".to_string(),
                "--platform".to_string(),
                "manylinux_2_17_x86_64".to_string(),
                "--python-version".to_string(),
                "311".to_string(),
                "--implementation".to_string(),
                "cp".to_string(),
                "-r".to_string(),
                requirements.display().to_string(),
            ];
            run_command_env("python3", &pip_args, dir, &build_env)?;
        }
        python_artifacts.push((service.name.clone(), artifact_dir));
        let hash = compute_rust_input_hash(dir).unwrap_or_else(|_| "python".to_string());
        hash_lines.push(format!("{rel} {hash}"));
    }

    // dotnet services: `dotnet publish` a self-contained linux-x64 single-file
    // executable on the dev machine and ship that binary (framework deps are
    // bundled into it — no .NET runtime on the server).
    for (service, rel, dir) in &dotnet_targets {
        let bin_name = if !service.binary.is_empty() { service.binary.clone() } else { service.name.clone() };
        print_step(&format!("Publishing dotnet {} (self-contained linux-x64)", service.name));
        let out_dir = std::env::temp_dir().join(format!("eco-dotnet-{}-{}", service.name, std::process::id()));
        let _ = std::fs::remove_dir_all(&out_dir);
        std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
        run_command_env(
            "dotnet",
            &[
                "publish".to_string(),
                "-c".to_string(),
                "Release".to_string(),
                "-r".to_string(),
                "linux-x64".to_string(),
                "--self-contained".to_string(),
                "true".to_string(),
                "-p:PublishSingleFile=true".to_string(),
                "-o".to_string(),
                out_dir.display().to_string(),
            ],
            dir,
            &build_env,
        )?;
        // The publish output's native executable is the file with no known
        // extension (exclude .dll/.pdb/.json/.xml and the shared framework).
        let exe = std::fs::read_dir(&out_dir)
            .map_err(|e| format!("read {}: {e}", out_dir.display()))?
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                let ext = p.extension().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
                ext.is_empty() && p.is_file()
            })
            .find(|p| {
                let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                !name.is_empty()
            })
            .ok_or_else(|| format!("dotnet publish for {} did not produce a native executable", service.name))?;
        artifacts.push((service.name.clone(), bin_name, exe));
        let hash = compute_rust_input_hash(dir).unwrap_or_else(|_| "dotnet".to_string());
        hash_lines.push(format!("{rel} {hash}"));
    }

    // Build Spring Boot services: `mvn package -DskipTests` produces the fat
    // jar, shipped to the CT where `java -jar` runs it.
    let mut spring_artifacts: Vec<(String, PathBuf)> = Vec::new();
    for (service, rel, dir) in &spring_targets {
        let jar_name = if !service.binary.is_empty() { service.binary.clone() } else { service.name.clone() };
        print_step(&format!("Building Spring Boot {} (mvn package)", service.name));
        run_command_env("mvn", &["package".to_string(), "-DskipTests".to_string(), "-q".to_string()], dir, &build_env)?;
        // Find the fat jar (exclude *-original and *-plain).
        let target_dir = dir.join("target");
        let jar = std::fs::read_dir(&target_dir)
            .map_err(|e| format!("read {}: {e}", target_dir.display()))?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "jar").unwrap_or(false))
            .filter(|p| {
                let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                !name.contains("original") && !name.contains("plain") && name != ".jar"
            })
            .max_by_key(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
            .ok_or_else(|| format!("No fat jar produced by mvn package for {}", service.name))?;
        let jar_artifacts_dir = std::env::temp_dir().join(format!("eco-spring-artifacts-{}-{}", service.name, std::process::id()));
        let _ = std::fs::remove_dir_all(&jar_artifacts_dir);
        std::fs::create_dir_all(&jar_artifacts_dir).map_err(|e| e.to_string())?;
        std::fs::copy(&jar, jar_artifacts_dir.join(format!("{jar_name}.jar"))).map_err(|e| format!("copy jar: {e}"))?;
        spring_artifacts.push((service.name.clone(), jar_artifacts_dir));
        let hash = compute_rust_input_hash(dir).unwrap_or_else(|_| "spring".to_string());
        hash_lines.push(format!("{rel} {hash}"));
    }

    // Build each Node/frontend service on the local Linux builder VM and
    // collect the built dist (plus a frontend source hash for the CT-side
    // skip). npm native modules are linux-x64 because the builder is x86_64.
    let mut frontend_artifacts: Vec<(String, PathBuf)> = Vec::new();
    let mut frontend_hash_lines: Vec<String> = Vec::new();
    // Spring Boot fat jars ship through the same per-service artifact dir
    // mechanism (tarballed into artifacts/<service>/).
    frontend_artifacts.extend(spring_artifacts);
    frontend_artifacts.extend(python_artifacts);
    for (service, rel, dir) in &frontend_targets {
        if !builder_driver_is_available() {
            return Err(format!(
                "{} is a Node service but no local Linux builder VM is reachable. Provision it with Lima (`brew install lima && limactl start --name eco-builder scripts/eco-builder.lima.yml`), or set ECO_BUILDER=host to build on this machine (dev only).",
                service.name
            ));
        }
        if !builder_available() {
            return Err(format!(
                "{} is a Node service but the local builder VM `{}` is not running. Start it (`limactl start {}` or `multipass start {}`).",
                service.name,
                builder_name(),
                builder_name(),
                builder_name()
            ));
        }
        let hash = compute_frontend_input_hash(dir, &build_env)?;
        let env_hash = compute_frontend_env_hash(&build_env);
        frontend_hash_lines.push(format!("{rel} {hash}"));
        let build_dir = format!("{}/{}", builder_build_root(), service.name);
        let build_loc = if builder_is_host() { "on this machine (host builder)".to_string() } else { format!("on local builder ({})", builder_name()) };
        let is_leptos = dir.join("index.html").is_file() && !dir.join("package.json").is_file();
        let is_static = service.runtimes.iter().any(|r| r == "static") || (dir.join("index.html").is_file() && !dir.join("package.json").is_file() && !is_leptos);
        print_step(&format!(
            "Building {} {}: {}",
            service.name,
            build_loc,
            if is_leptos { "trunk build --release (Leptos wasm)" } else if is_static { "shipping static dist" } else { "npm ci + npm run build" }
        ));
        sync_dir_to_builder(dir, &build_dir)?;
        if is_static {
            // Static site: ship the source dir (index.html + assets) as the
            // dist/ so the CT serves it via python http.server.
            let artifact_dir = std::env::temp_dir().join(format!("eco-frontend-artifact-{}-{}", service.name, std::process::id()));
            let _ = std::fs::remove_dir_all(&artifact_dir);
            std::fs::create_dir_all(&artifact_dir).map_err(|e| e.to_string())?;
            copy_tree_excluding(dir, &artifact_dir.join("dist"), &(skip_sensitive_artifact_entry as fn(&str) -> bool))?;
            frontend_artifacts.push((service.name.clone(), artifact_dir));
            print_step(&format!("Built {} static artifact -> ship", service.name));
            continue;
        }
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
        // hash, reuse the existing output instead of building again. Whenever
        // a rebuild IS needed, always clear the compiler's own incremental
        // cache first: Next.js/Vite reuse cached chunks for unchanged modules,
        // so a rebuild after an env change could otherwise ship stale chunks
        // that still contain the old value (e.g. an old localhost API URL).
        let env_wipe = "echo \"rebuilding frontend: clearing stale compiler output\"; rm -rf .next .vite .cache";
        let script = if is_leptos {
            format!(
                "cd {} && {exports}if [ -f .eco-frontend-hash ] && [ \"$(cat .eco-frontend-hash)\" = \"{hash}\" ]; then echo \"frontend unchanged, skipping rebuild\"; exit 0; fi\n{env_wipe}\nif ! command -v trunk >/dev/null 2>&1; then cargo install trunk --locked; fi && rustup target add wasm32-unknown-unknown 2>/dev/null || true; trunk build --release && printf '{hash}' > .eco-frontend-hash && printf '{env_hash}' > .eco-frontend-envhash",
                shell_single_quote(&build_dir)
            )
        } else {
            format!(
                "cd {} && {exports}if [ -f .eco-frontend-hash ] && [ \"$(cat .eco-frontend-hash)\" = \"{hash}\" ]; then echo \"frontend unchanged, skipping rebuild\"; exit 0; fi\n{env_wipe}\nif [ -f package-lock.json ]; then npm ci --no-audit --no-fund || npm install --no-audit --no-fund --legacy-peer-deps; elif [ -f pnpm-lock.yaml ]; then corepack enable && pnpm install --frozen-lockfile; else npm install --no-audit --no-fund --legacy-peer-deps; fi && ECO_DEPLOY_MODE=prod npm run build --if-present && printf '{hash}' > .eco-frontend-hash && printf '{env_hash}' > .eco-frontend-envhash",
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

    // Build the deploy payload: ecompose.yml + artifacts + hashes. Executable-only:
    // the server never sees source code. The manifest travels so configure.sh
    // can derive service topology without scanning a source tree.
    let payload_dir = std::env::temp_dir().join(format!("eco-remote-payload-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&payload_dir);
    std::fs::create_dir_all(&payload_dir).map_err(|e| e.to_string())?;
    let result = (|| -> Result<(), String> {
        let artifacts_dir = payload_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts_dir).map_err(|e| e.to_string())?;
        for (service_name, package, binary) in &artifacts {
            // Binary lives at artifacts/<service.name>/bin/<package> so the
            // service dir (.env.example, migrations) never collides with a
            // flat binary file when service name == package name.
            let service_bin_dir = artifacts_dir.join(service_name).join("bin");
            std::fs::create_dir_all(&service_bin_dir).map_err(|e| e.to_string())?;
            std::fs::copy(binary, service_bin_dir.join(package)).map_err(|e| format!("copy {}: {e}", binary.display()))?;
        }
        for (service_name, artifact_dir) in &frontend_artifacts {
            let dest = artifacts_dir.join(service_name);
            std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
            copy_tree_excluding(artifact_dir, &dest, &(skip_none as fn(&str) -> bool))?;
        }
        // Ship the env contract and migrations with each source service's
        // artifact so the server can generate `.env` and run migrations without
        // ever seeing source. Artifacts are keyed by service name for
        // frontends and by package name for rust binaries; we key the
        // contract under the SERVICE name dir.
        for (service, _rel, dir) in rust_targets
            .iter()
            .chain(frontend_targets.iter())
            .chain(python_targets.iter())
            .chain(dotnet_targets.iter())
        {
            let service_artifact = artifacts_dir.join(&service.name);
            std::fs::create_dir_all(&service_artifact).map_err(|e| e.to_string())?;
            let example = dir.join(".env.example");
            if example.is_file() {
                let _ = std::fs::copy(&example, service_artifact.join(".env.example"));
            }
            let migrations = dir.join("migrations");
            if migrations.is_dir() {
                copy_tree_excluding(&migrations, &service_artifact.join("migrations"), &(skip_none as fn(&str) -> bool))?;
            }
            // Ship static web assets (static/, images/, public/, downloads/) so
            // a source frontend binary that serves them (Leptos ServeDir,
            // etc.) finds them at artifacts/<service>/<dir> on the CT.
            for asset_dir in ["static", "images", "public", "assets", "downloads"] {
                let src = dir.join(asset_dir);
                if src.is_dir() {
                    let dst = service_artifact.join(asset_dir);
                    std::fs::create_dir_all(&dst).map_err(|e| e.to_string())?;
                    copy_tree_excluding(&src, &dst, &(skip_none as fn(&str) -> bool))?;
                }
            }
        }
        std::fs::write(payload_dir.join("rust-hashes"), format!("{}\n", hash_lines.join("\n"))).map_err(|e| e.to_string())?;
        std::fs::write(payload_dir.join("frontend-hashes"), format!("{}\n", frontend_hash_lines.join("\n"))).map_err(|e| e.to_string())?;
        std::fs::write(payload_dir.join("ecompose.yml"), &deployment.content).map_err(|e| e.to_string())?;
        write_payload_manifest(&payload_dir, &deployment.project)?;
        let tar_path = payload_dir.join("payload.tar.gz");
        run_command_env(
            "tar",
            &[
                "czf".to_string(),
                tar_path.display().to_string(),
                "--no-xattrs".to_string(),
                "-C".to_string(),
                payload_dir.display().to_string(),
                "artifacts".to_string(),
                "rust-hashes".to_string(),
                "frontend-hashes".to_string(),
                "ecompose.yml".to_string(),
                "artifact-manifest.json".to_string(),
            ],
            &util::current_dir(),
            &[("COPYFILE_DISABLE".to_string(), "1".to_string())],
        )?;
        // Payload size cap — the pricing hook. The shipped payload is the
        // built artifacts + the manifest only (no source).
        const MAX_PAYLOAD_MB: u64 = 300;
        let tar_meta = std::fs::metadata(&tar_path).map_err(|e| format!("read payload size: {e}"))?;
        let mb = tar_meta.len() / (1024 * 1024);
        if tar_meta.len() > MAX_PAYLOAD_MB * 1024 * 1024 {
            return Err(format!(
                "remote deploy payload is {mb} MB — over the {MAX_PAYLOAD_MB} MB cap. \
The shipped artifacts exceed the limit; reduce what is being built/shipped."
            ));
        }
        let bytes = std::fs::read(&tar_path).map_err(|e| format!("read payload: {e}"))?;
        let project_segment = project_path_segment(&deployment.project);
        // When ECO_SSH is set (e.g. root@<host>), ship the payload over
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

// shipped artifact. Returns the service names that got a shipped dist.

/// Print a highlight when a composed LXS has a newer version available in the
/// registry. Called by `eco up` (dev + remote) so the user learns about
/// updates on every deploy; never blocks on an offline registry.
///
/// Can be turned off to focus on the current LXS versions or to speed up
/// `eco up`:
///   eco up --no-lxs-check        # per-command flag
///   ECO_NO_LXS_CHECK=1 eco up    # or via env
fn print_lxs_update_notice(content: &str, project_dir: &Path, skip: bool) {
    if skip {
        return;
    }
    if util::env_var_or("ECO_NO_LXS_CHECK", "").eq_ignore_ascii_case("1")
        || util::env_var_or("ECO_NO_LXS_CHECK", "").eq_ignore_ascii_case("true")
    {
        return;
    }
    let state_registry = crate::commands::lxs::read_estate_state(project_dir)
        .map(|s| s.registry)
        .filter(|r| !r.is_empty());
    let updates = crate::commands::lxs::lxs_updates_available(content, state_registry.as_deref());
    if updates.is_empty() {
        return;
    }
    util::println_stdout("\nLXS updates available:");
    for (service, pinned, latest) in &updates {
        let (name, _) = crate::commands::lxs::parse_pinned_ref(pinned);
        let from_v = pinned.split('@').nth(1).unwrap_or("");
        let to_v = latest.split('@').nth(1).unwrap_or("");
        util::println_stdout(&format!(
            "  {}  \x1b[1;33m{} -> {}\x1b[0m   run `eco lxs update {}`",
            service, pinned, latest, name
        ));
        let note = crate::commands::lxs::changelog_note(&name, to_v, from_v, state_registry.as_deref());
        if !note.is_empty() {
            for line in note.lines() {
                util::println_stdout(&format!("     {}", line));
            }
        }
    }
    util::println_stdout("  (latest from the registry; `eco lxs update` bumps ecompose.yml)\n");
}

/// Whether the caller asked to skip the LXS update check (`--no-lxs-check`).
fn lxs_check_disabled(options: &HashMap<String, String>) -> bool {
    options.get("no-lxs-check").map(|v| v == "true").unwrap_or(false)
}

pub fn run_up(args: &[String]) -> Result<(), String> {    if args.first().map(|s| s.as_str()) == Some("dev") {
        return run_up_dev(&args[1..]);
    }
    if args.iter().any(|a| a == "--remote") {
        // Cross-compile the Rust services on this (developer) machine and ship
        // the Linux binaries to the Proxmox host via the eco serve agent.
        return run_up_remote(args);
    }

    // Client-only build: direct on-host provisioning lives in the private
    // eco-agent binary. A plain `eco up` on a dev machine runs local dev mode.
    util::println_stdout("Not on a Proxmox host \u{2014} running in dev mode.");
    run_up_dev(args)
}
/// Install `lxs:` services into the local dev workspace, mirroring the CT
/// release path: fetch the native binary for this platform, install it under
/// <project>/<service-name>/bin/<name> plus a start.sh + .env.example from the
/// contract, so configure.sh's static-service discovery (start.sh) picks it up
/// and PM2 runs it exactly like a source service.
fn install_lxs_services_local(deployment: &ProjectDeployment, estate_root: &Path) -> Result<Vec<String>, String> {
    let lxs_services: Vec<&ecompose::Service> = deployment.services.iter().filter(|s| !s.lxs.is_empty()).collect();
    if lxs_services.is_empty() {
        return Ok(Vec::new());
    }
    let state_registry = crate::commands::lxs::read_estate_state(&deployment.project_dir).map(|s| s.registry).filter(|r| !r.is_empty());
    // Native arch for the dev machine (darwin/aarch64 on Apple Silicon,
    // linux/amd64 on x86_64 Linux, linux/arm64 on aarch64 Linux).
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    let local_arch = match (os, arch) {
        ("macos", "aarch64") => "darwin/aarch64".to_string(),
        ("macos", "x86_64") => "darwin/x86_64".to_string(),
        ("linux", "x86_64") => "linux/amd64".to_string(),
        ("linux", "aarch64") => "linux/arm64".to_string(),
        other => return Err(format!(
            "eco up dev: no LXS artifact for local platform {other:?} — use eco up --remote or run on linux/darwin"
        )),
    };
    let mut installed = Vec::new();
    for service in lxs_services {
        let (manifest, version, local_bin) = crate::commands::lxs::fetch_lxs_to_cache(&service.lxs, &local_arch, state_registry.as_deref())?;
        let name = manifest.name.clone();
        if name.is_empty() {
            return Err(format!("LXS {} has no name in its manifest", service.lxs));
        }
        let service_dir = estate_root.join(&service.name);
        let bin_dir = service_dir.join("bin");
        std::fs::create_dir_all(&bin_dir).map_err(|e| format!("create {}: {e}", bin_dir.display()))?;
        // The manifest's artifact may carry the short name (e.g. `auth`); we
        // install it under the artifact name so configgen/configure resolve it.
        let installed_name = service
            .lxs
            .split('@')
            .next()
            .map(|s| s.to_string())
            .unwrap_or_else(|| name.clone());
        let dest_bin = bin_dir.join(&installed_name);
        std::fs::copy(&local_bin, &dest_bin).map_err(|e| format!("copy {} -> {}: {e}", local_bin.display(), dest_bin.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dest_bin, std::fs::Permissions::from_mode(0o755));
        }
        // start.sh runs the binary; configure.sh discovers `<dir>/start.sh`
        // as a `static` service and PM2 runs `bash start.sh`. The wrapper
        // sources the generated .env (filled by configure.sh) and resolves an
        // empty MONGODB_URI/REDIS_URL/DATABASE_URL to the estate-local
        // managed DB so the binary never sees a blank connection string.
        let db_name = service.name.replace('-', "_");
        let start_sh = format!(
            "#!/bin/bash\nset -euo pipefail\ncd \"$(dirname \"$0\")\"\nif [[ -f .env ]]; then set -a; source .env; set +a; fi\n: \"${{SERVER_PORT:?SERVER_PORT not set}}\"\n[[ -n \"${{MONGODB_URI:-}}\" ]] || export MONGODB_URI=\"mongodb://localhost:27017/{db_name}_{project}\"\n[[ -n \"${{REDIS_URL:-}}\" ]] || export REDIS_URL=\"redis://127.0.0.1:6379\"\n[[ -n \"${{DATABASE_URL:-}}\" ]] || export DATABASE_URL=\"\"\nexec ./bin/{installed_name}\n",
            db_name = db_name,
            project = deployment.project,
            installed_name = installed_name,
        );
        std::fs::write(service_dir.join("start.sh"), start_sh).map_err(|e| format!("write start.sh: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&service_dir.join("start.sh"), std::fs::Permissions::from_mode(0o755));
        }
        // .env.example from the contract so configgen can fill secrets.
        let mut env_example = String::new();
        for key in manifest.contract.env.required.iter().chain(manifest.contract.env.optional.iter()) {
            let value = manifest.contract.env.defaults.get(key).cloned().unwrap_or_default();
            env_example.push_str(&format!("{key}={value}\n"));
        }
        // The LXS contract's db requirement owns a managed estate-local DB
        // URI; leave the key blank so configure.sh fills it (same convention
        // as a declared `mongodb@`/`postgresql@15`/`redis@7` runtime).
        match manifest.contract.db.as_str() {
            "mongodb@7" | "mongo" | "mongodb" => {
                if !env_example.contains("MONGODB_URI=") {
                    env_example.push_str("MONGODB_URI=\n");
                }
            }
            "postgresql@15" | "postgres" | "postgresql" => {
                if !env_example.contains("DATABASE_URL=") {
                    env_example.push_str("DATABASE_URL=\n");
                }
            }
            "redis@7" | "redis" => {
                if !env_example.contains("REDIS_URL=") {
                    env_example.push_str("REDIS_URL=\n");
                }
            }
            _ => {}
        }
        std::fs::write(service_dir.join(".env.example"), env_example).map_err(|e| format!("write .env.example: {e}"))?;
        print_step(&format!("Installed local LXS {}@{} as {}", name, version, service.name));
        installed.push(service.name.clone());
    }
    Ok(installed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_artifact_filter_blocks_secrets_and_workspace_state() {
        for name in [".env", ".env.production", ".git", ".eco", ".ssh", "debug.log", "node_modules"] {
            assert!(skip_sensitive_artifact_entry(name), "expected {name} to be excluded");
        }
        for name in ["index.html", "app.py", "requirements.txt", "public"] {
            assert!(!skip_sensitive_artifact_entry(name), "expected {name} to be shippable");
        }
    }

    #[test]
    fn payload_manifest_records_sha256_and_size() {
        let root = std::env::temp_dir().join(format!("eco-payload-manifest-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("artifacts/web")).unwrap();
        std::fs::write(root.join("artifacts/web/index.html"), b"hello").unwrap();
        std::fs::write(root.join("ecompose.yml"), b"project: sample\n").unwrap();
        std::fs::write(root.join("rust-hashes"), b"\n").unwrap();
        std::fs::write(root.join("frontend-hashes"), b"\n").unwrap();

        write_payload_manifest(&root, "sample").unwrap();
        let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(root.join("artifact-manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["schema_version"], 1);
        assert_eq!(manifest["project"], "sample");
        let entry = manifest["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["path"] == "artifacts/web/index.html")
            .unwrap();
        assert_eq!(entry["size"], 5);
        assert_eq!(entry["sha256"].as_str().unwrap().len(), 64);
        let _ = std::fs::remove_dir_all(&root);
    }
}
