use crate::checklist;
use crate::ecompose;
use crate::embedded;
use crate::github;
use crate::repos;
use crate::util;
use crate::workspace;
use std::path::{Path, PathBuf};

const DEFAULT_CT_TEMPLATE: &str = "local:vztmpl/eco-npm-rust-mongo_1_amd64.tar.zst";
const DEFAULT_SHARED_TOOLS: &[&str] = &["git", "openssh-client", "curl", "jq", "ca-certificates"];

fn path_exists(p: &Path) -> bool {
    p.exists()
}

fn run_command(command: &str, args: &[String], cwd: &Path) -> Result<(), String> {
    util::run_command(command, args, cwd)
}

fn has_staged_changes(cwd: &Path) -> Result<bool, String> {
    let result = util::run_capture("git", &["diff".to_string(), "--cached".to_string(), "--quiet".to_string()], cwd)?;
    match result.code {
        0 => Ok(false),
        1 => Ok(true),
        other => Err(format!("git diff exited with code {other}")),
    }
}

fn authenticated_github_url(repository: &github::GithubRepoInfo) -> String {
    github::authenticated_github_url(repository)
}

fn composition_git_url(plan: &Option<github::GithubRepoInfo>, login: &str, name: &str) -> String {
    if let Some(repo) = plan {
        if !repo.ssh_url.is_empty() {
            return repo.ssh_url.clone();
        }
        if !repo.clone_url.is_empty() {
            return repo.clone_url.clone();
        }
    }
    format!("git@github.com:{login}/{name}.git")
}

fn clone_and_clear_repository(directory: &Path, repository: &github::GithubRepoInfo) -> Result<(), String> {
    let url = authenticated_github_url(repository);
    run_command("git", &["clone".to_string(), url, directory.display().to_string()], &util::current_dir())?;
    run_command("git", &["rm".to_string(), "-r".to_string(), "--ignore-unmatch".to_string(), ".".to_string()], directory)?;
    run_command("git", &["clean".to_string(), "-fdx".to_string()], directory)
}

fn initialise_and_push_repository(
    directory: &Path,
    repository_name: &str,
    commit_message: &str,
    existing_repository: Option<&github::GithubRepoInfo>,
) -> Result<github::GithubRepoInfo, String> {
    let existing_repository = existing_repository.filter(|repository| repository.exists);
    if existing_repository.is_none() || !directory.join(".git").is_dir() {
        run_command("git", &["init".to_string(), "--initial-branch=main".to_string()], directory)?;
    }
    let repository = match existing_repository {
        Some(repo) => repo.clone(),
        None => github::create_github_repository(repository_name)?,
    };
    let authenticated_push_url = authenticated_github_url(&repository);
    run_command("git", &["add".to_string(), ".".to_string()], directory)?;
    if !has_staged_changes(directory)? {
        util::println_stdout(&format!("  {repository_name} already matches the generated scaffold; no commit needed."));
        if existing_repository.is_some() {
            let origin = if repository.ssh_url.is_empty() { repository.clone_url.clone() } else { repository.ssh_url.clone() };
            run_command("git", &["remote".to_string(), "set-url".to_string(), "origin".to_string(), origin], directory)?;
        }
        return Ok(repository);
    }
    run_command("git", &["commit".to_string(), "-m".to_string(), commit_message.to_string()], directory)?;
    if existing_repository.is_some() {
        run_command("git", &["remote".to_string(), "set-url".to_string(), "origin".to_string(), authenticated_push_url.clone()], directory)?;
    } else {
        run_command("git", &["remote".to_string(), "add".to_string(), "origin".to_string(), authenticated_push_url.clone()], directory)?;
    }
    run_command("git", &["push".to_string(), "-u".to_string(), "origin".to_string(), "main".to_string()], directory)?;
    let origin = if repository.ssh_url.is_empty() { repository.clone_url.clone() } else { repository.ssh_url.clone() };
    run_command("git", &["remote".to_string(), "set-url".to_string(), "origin".to_string(), origin], directory)?;
    Ok(repository)
}

fn parse_flags(args: &[String]) -> StartProjectFlags {
    let mut flags = StartProjectFlags::default();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--yes" | "-y" => {
                flags.yes = true;
                i += 1;
            }
            "--hostname" | "-H" => {
                if let Some(v) = args.get(i + 1) {
                    flags.hostname = Some(v.clone());
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--cloudflare-account" | "-c" => {
                if let Some(v) = args.get(i + 1) {
                    flags.cloudflare_account = Some(v.clone());
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--ct-id" => {
                if let Some(v) = args.get(i + 1) {
                    flags.ct_id = v.parse().ok();
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--staging-ct" => {
                if let Some(v) = args.get(i + 1) {
                    flags.staging_ct = v.parse().ok();
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--no-deploy" => {
                flags.no_deploy = true;
                i += 1;
            }
            "--no-storage" => {
                flags.no_storage = true;
                i += 1;
            }
            "--no-staging" => {
                flags.no_staging = true;
                i += 1;
            }
            "--no-email-verification" => {
                flags.no_email_verification = true;
                i += 1;
            }
            "--repo" => {
                if let Some(v) = args.get(i + 1) {
                    flags.repos.push(v.clone());
                    i += 2;
                } else {
                    i += 1;
                }
            }
            other if other.starts_with("--branch=") => {
                let rest = &other[9..];
                if let Some(eq) = rest.find('=') {
                    let repo = rest[..eq].to_string();
                    let branch = rest[eq + 1..].to_string();
                    if !repo.is_empty() && !branch.is_empty() {
                        flags.branch_overrides.insert(repo, branch);
                    }
                }
                i += 1;
            }
            "--branch" => {
                if let Some(v) = args.get(i + 1) {
                    if let Some(eq) = v.find('=') {
                        let repo = v[..eq].to_string();
                        let branch = v[eq + 1..].to_string();
                        if !repo.is_empty() && !branch.is_empty() {
                            flags.branch_overrides.insert(repo, branch);
                        }
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            }
            _ => {
                flags.remaining.push(arg.clone());
                i += 1;
            }
        }
    }
    flags
}

#[derive(Default)]
struct StartProjectFlags {
    yes: bool,
    hostname: Option<String>,
    cloudflare_account: Option<String>,
    ct_id: Option<i64>,
    staging_ct: Option<i64>,
    no_deploy: bool,
    no_storage: bool,
    no_staging: bool,
    no_email_verification: bool,
    repos: Vec<String>,
    branch_overrides: std::collections::HashMap<String, String>,
    remaining: Vec<String>,
}

fn prompt_project_name(args: &[String], workspace_root: &Path, cwd: &Path) -> Result<(String, PathBuf, bool), String> {
    let project_arg = args.first().cloned();
    if project_arg.as_deref() == Some(".") {
        let name = cwd
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        return Ok((name, cwd.to_path_buf(), true));
    }
    if let Some(name) = &project_arg {
        return Ok((name.clone(), workspace_root.join(name), false));
    }
    let answer = checklist::prompt_line("Project name: ")?;
    if answer.trim().is_empty() {
        return Err("Project name is required.".to_string());
    }
    Ok((answer.trim().to_string(), workspace_root.join(answer.trim()), false))
}

fn compute_dependency_closure(selected_repos: &[String], repo_catalog: &[repos::RepoEntry]) -> Vec<String> {
    let by_name: std::collections::HashMap<String, &repos::RepoEntry> =
        repo_catalog.iter().map(|r| (r.name.clone(), r)).collect();
    let mut resolved: std::collections::HashSet<String> = selected_repos.iter().cloned().collect();
    let mut stack: Vec<String> = selected_repos.to_vec();
    while let Some(current) = stack.pop() {
        let requires = by_name
            .get(&current)
            .map(|r| r.requires.clone())
            .unwrap_or_default();
        for dep in requires {
            if resolved.insert(dep.clone()) {
                stack.push(dep);
            }
        }
    }
    let mut sorted: Vec<String> = resolved.into_iter().collect();
    sorted.sort();
    sorted
}

fn capitalise_project_name(project_name: &str) -> String {
    if project_name.is_empty() {
        "Eco".to_string()
    } else {
        let mut chars = project_name.chars();
        let first = chars.next().unwrap().to_uppercase().collect::<String>();
        format!("{first}{}", chars.as_str())
    }
}

fn run_repo_checklist(
    repo_catalog: &[repos::RepoEntry],
    project_name: &str,
    non_interactive: bool,
    preselected_repos: &[String],
) -> Result<Vec<String>, String> {
    if non_interactive {
        let available: Vec<String> = repo_catalog
            .iter()
            .filter(|r| r.name != "eco")
            .map(|r| r.name.clone())
            .collect();
        let selected: Vec<String> = preselected_repos
            .iter()
            .filter(|name| available.contains(name))
            .cloned()
            .collect();
        if selected.is_empty() {
            util::println_stdout("No repos selected (non-interactive).");
            return Ok(Vec::new());
        }
        let resolved = compute_dependency_closure(&selected, repo_catalog);
        util::println_stdout(&format!("\nSelected repos (non-interactive): {}\n", resolved.join(", ")));
        return Ok(resolved);
    }

    let items: Vec<checklist::ChecklistItem> = repo_catalog
        .iter()
        .filter(|r| r.name != "eco")
        .map(|r| {
            let desc = if r.description.is_empty() { "No description".to_string() } else { r.description.clone() };
            checklist::ChecklistItem {
                id: r.name.clone(),
                label: format!("{} - {desc}", r.name),
            }
        })
        .collect();
    let (requires_by, required_by) = checklist::build_repo_dependency_maps(repo_catalog);
    let hint = [
        "Controls: ↑/↓ move, x or space toggle, Enter confirm".to_string(),
        String::new(),
        "  If a repo is not in this list, it means you are creating a new domain project.".to_string(),
        "  Navigate into that domain repo directory and run: eco repos add".to_string(),
        "  It will then appear here.".to_string(),
    ]
    .join("\n");
    checklist::run_checklist(
        &items,
        &format!("Select repos for {project_name} (at least one required)"),
        &hint,
        Some(&requires_by),
        Some(&required_by),
        1,
        &[],
        &[],
    )
}

fn prompt_composition_services(project_name: &str, non_interactive: bool) -> Result<Vec<String>, String> {
    if non_interactive {
        util::println_stdout("\nComposition starter (non-interactive)");
        util::println_stdout("  frontend: Node.js (always included)");
        util::println_stdout("  backend: Rust (always included)\n");
        return Ok(vec!["frontend".to_string(), "backend".to_string()]);
    }
    checklist::run_checklist(
        &[
            checklist::ChecklistItem { id: "frontend".to_string(), label: "frontend — required Node.js public application".to_string() },
            checklist::ChecklistItem { id: "backend".to_string(), label: "backend — optional Rust project API".to_string() },
        ],
        &format!("Select composition services for {project_name}"),
        "Controls: ↑/↓ move, x or space toggle, Enter confirm\n\n  frontend is required, receives the estate's first port, and starts as a runnable Eco guide.",
        None,
        None,
        1,
        &["frontend".to_string()],
        &["frontend".to_string()],
    )
}

fn prompt_backend_databases(selected_composition_services: &[String], non_interactive: bool) -> Result<Vec<String>, String> {
    if !selected_composition_services.iter().any(|s| s == "backend") {
        return Ok(Vec::new());
    }
    if non_interactive {
        util::println_stdout("Backend databases (non-interactive): MongoDB 7\n");
        return Ok(vec!["mongodb@7".to_string()]);
    }
    checklist::run_checklist(
        &[
            checklist::ChecklistItem { id: "mongodb@7".to_string(), label: "MongoDB 7".to_string() },
            checklist::ChecklistItem { id: "postgresql@15".to_string(), label: "PostgreSQL 15".to_string() },
        ],
        "Select backend databases (optional)",
        "Controls: ↑/↓ move, x or space toggle, Enter confirm\n\n  Select every database this backend needs. Eco provisions the selected runtimes.",
        None,
        None,
        0,
        &[],
        &[],
    )
}

struct CompositionService {
    name: String,
    path: String,
    language: String,
    runtimes: Vec<String>,
}

fn build_composition_services(
    selected_composition_services: &[String],
    backend_databases: &[String],
) -> Vec<CompositionService> {
    selected_composition_services
        .iter()
        .map(|service_name| {
            let (language, base_runtimes) = match service_name.as_str() {
                "frontend" => ("node", vec!["node@20".to_string(), "npm".to_string(), "pm2".to_string()]),
                "backend" => ("rust", vec!["rust".to_string()]),
                _ => (service_name.as_str(), Vec::new()),
            };
            let mut runtimes = base_runtimes;
            if service_name == "backend" {
                runtimes.extend_from_slice(backend_databases);
            }
            CompositionService {
                name: service_name.clone(),
                path: service_name.clone(),
                language: language.to_string(),
                runtimes,
            }
        })
        .collect()
}

fn discover_service_templates(workspace_root: &Path) -> Result<std::collections::HashMap<String, Vec<ecompose::Service>>, String> {
    let mut templates: std::collections::HashMap<String, Vec<ecompose::Service>> = std::collections::HashMap::new();
    let entries = std::fs::read_dir(workspace_root).map_err(|e| e.to_string())?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "eco" || name == "core" {
            continue;
        }
        let project_dirs: Vec<_> = std::fs::read_dir(&path)
            .map(|e| e.flatten().collect())
            .unwrap_or_default();
        for project_dir in project_dirs {
            if !project_dir.path().is_dir() {
                continue;
            }
            let ecompose_path = project_dir.path().join("ecompose.yml");
            if !ecompose_path.exists() {
                continue;
            }
            let content = std::fs::read_to_string(&ecompose_path).unwrap_or_default();
            for service in ecompose::parse_services(&content) {
                let repo_name = service.path.split('/').next().unwrap_or("").to_string();
                if repo_name.is_empty() {
                    continue;
                }
                let current = templates.entry(repo_name).or_default();
                if !current.iter().any(|e| e.name == service.name && e.path == service.path) {
                    current.push(service);
                }
            }
        }
    }
    Ok(templates)
}

fn render_service_block(service: &CompositionService) -> String {
    let mut lines = vec![format!("  {}:", service.name), format!("    path: {}", service.path), "    runtimes:".to_string()];
    for runtime in &service.runtimes {
        lines.push(format!("      - {runtime}"));
    }
    lines.join("\n")
}

fn render_domain_entry(name: &str, branch: Option<&str>, dev: Option<&str>) -> String {
    if branch.is_none() && dev.is_none() {
        return format!("  - {name}");
    }
    let mut lines = vec![format!("  - {name}:")];
    if let Some(b) = branch {
        lines.push(format!("      branch: {b}"));
    }
    if let Some(d) = dev {
        lines.push(format!("      dev: {d}"));
    }
    lines.join("\n")
}

fn build_ecompose_content(
    project_name: &str,
    selected_repos: &[String],
    service_templates: &std::collections::HashMap<String, Vec<ecompose::Service>>,
    composition_services: &[CompositionService],
    details: &EstateDetails,
    auth_email_verification: Option<&AuthEmailVerification>,
    branch_overrides: &std::collections::HashMap<String, String>,
    dev_flags: &std::collections::HashMap<String, String>,
    composition_git: &str,
    superadmin_setup: bool,
) -> String {
    let mut lines: Vec<String> = vec![
        format!("project: {project_name}"),
        String::new(),
        "ct:".to_string(),
        format!("  id: {}", details.ct_id),
        format!("  hostname: {project_name}"),
        format!("  template: {DEFAULT_CT_TEMPLATE}"),
        "  storage: local-lvm".to_string(),
        "  disk: 16".to_string(),
        "  bridge: vmbr0".to_string(),
        "  ip: dhcp".to_string(),
        "  cores: 2".to_string(),
        "  memory: 4096".to_string(),
        "  swap: 1024".to_string(),
        "  unprivileged: 1".to_string(),
        String::new(),
        "expose:".to_string(),
        "  enabled: true".to_string(),
        format!("  hostname: {}", details.hostname),
        format!("  service: {}", details.expose_service),
        "  proxy_ct: proxy".to_string(),
        format!("  cloudflare_account: {}", details.cloudflare_account),
        String::new(),
    ];

    if details.staging_enabled {
        lines.push("# Staging footprint: a second deployment on a separate CT, exposed at".to_string());
        lines.push("# a staging-<hostname> and deployed explicitly with eco up --remote --staging.".to_string());
        lines.push("staging:".to_string());
        lines.push(format!("  ct: {}", details.staging_ct));
        lines.push(String::new());
    }

    if let Some(auth) = auth_email_verification {
        lines.push("# Auth domain settings. Credentials stay in auth/backend/.env and are never committed.".to_string());
        lines.push("auth:".to_string());
        lines.push("  email_verification:".to_string());
        lines.push(format!("    enabled: {}", if auth.required { "true" } else { "false" }));
        if auth.required {
            lines.push(format!("    ttl_hours: {}", auth.ttl_hours));
            lines.push(format!("    mail_from_email: {}", auth.mail_from_email));
            lines.push(format!("    mail_from_name: {}", util::json_string(&auth.mail_from_name)));
        }
        lines.push(String::new());
    }

    if superadmin_setup {
        lines.push("# When enabled, a fresh deployment shows a /setup page that forces the first".to_string());
        lines.push("# visitor to claim the superadmin role. Modeled after apindo's setup flow:".to_string());
        lines.push("# GET  /api/v1/setup/status → check if any admin exists".to_string());
        lines.push("# POST /api/v1/setup/claim  → create the initial superadmin".to_string());
        lines.push("setup:".to_string());
        lines.push("  superadmin: true".to_string());
        lines.push(String::new());
    }

    if let Some(storage) = &details.storage {
        lines.push("# Eco manages MinIO credentials and resolves this CT's private bridge".to_string());
        lines.push("# address at `eco up`; never commit endpoint or credentials here.".to_string());
        lines.push("storage:".to_string());
        lines.push("  minio:".to_string());
        lines.push(format!("    ct: {}", storage.ct));
        lines.push(format!("    region: {}", storage.region));
        lines.push(String::new());
    }

    lines.push("shared_tools:".to_string());
    for tool in DEFAULT_SHARED_TOOLS {
        lines.push(format!("  - {tool}"));
    }

    if !composition_git.is_empty() {
        lines.push(String::new());
        lines.push("# The composition repository is this estate's own project repo, not a".to_string());
        lines.push("# shared catalog domain: its git address lives here so a fresh host can".to_string());
        lines.push("# clone it without a central catalog entry.".to_string());
        lines.push("composition:".to_string());
        lines.push(format!("  git: {composition_git}"));
        lines.push("  branch: main".to_string());
        lines.push(String::new());
    }

    lines.push(String::new());
    lines.push("domains:".to_string());
    lines.push(format!("  - {project_name}_composition"));
    for repo_name in selected_repos {
        let branch = branch_overrides.get(repo_name).map(|s| s.as_str());
        let dev = dev_flags.get(repo_name).map(|s| s.as_str());
        lines.push(render_domain_entry(repo_name, branch, dev));
    }

    lines.push(String::new());
    lines.push("services:".to_string());
    let mut emitted: std::collections::HashSet<String> = std::collections::HashSet::new();

    for service in composition_services {
        let composition_service = CompositionService {
            name: service.name.clone(),
            path: format!("{project_name}_composition/{}", service.path),
            language: service.language.clone(),
            runtimes: service.runtimes.clone(),
        };
        lines.push(render_service_block(&composition_service));
        lines.push(String::new());
        emitted.insert(format!("{}:{}", composition_service.name, composition_service.path));
    }

    for repo_name in selected_repos {
        if let Some(templates) = service_templates.get(repo_name) {
            for service in templates {
                let key = format!("{}:{}", service.name, service.path);
                if emitted.contains(&key) {
                    continue;
                }
                let block = format!(
                    "  {}:\n    path: {}\n    runtimes:\n{}",
                    service.name,
                    service.path,
                    service.runtimes.iter().map(|r| format!("      - {r}")).collect::<Vec<_>>().join("\n")
                );
                lines.push(block);
                lines.push(String::new());
                emitted.insert(key);
            }
        }
    }

    format!("{}\n", lines.join("\n"))
}

fn build_gitignore_content() -> String {
    [
        "# eco-generated runtime files",
        "ecosystem.config.js",
        ".configure-state",
        "",
        "# environment secrets",
        ".env",
        ".env.local",
        ".env.*.local",
        "",
        "# dependencies",
        "node_modules/",
        "",
        "# build output",
        "dist/",
        "build/",
        ".next/",
        "",
        "# logs",
        "*.log",
        "logs/",
        "",
        "# OS",
        ".DS_Store",
        "Thumbs.db",
        "",
        "# editors",
        ".idea/",
        "*.iml",
        "",
        "# AI agent artifacts",
        ".claude/",
        ".codegraph/",
        ".kiro/",
        ".cursor/",
        ".codeium/",
        ".copilot/",
        ".aider*",
        ".continue/",
        "",
    ]
    .join("\n")
}

fn build_readme_content(project_name: &str) -> String {
    format!(
        "# {project_name}_core\n\nEstate manifest for the {project_name} project. Created by `eco startproject`.\n\nThis is the only repo that needs to be cloned on the Proxmox host before running `eco up`.\nAll domain repos are declared in `ecompose.yml` and will be cloned automatically by eco.\n"
    )
}

fn build_composition_readme(project_name: &str, composition_services: &[CompositionService]) -> String {
    let service_list: Vec<String> = composition_services.iter().map(|s| format!("- `{}/`", s.path)).collect();
    format!(
        "# {project_name}_composition\n\nThe composition layer for the {project_name} estate. It owns the project-specific user experience and optional project API.\n\n## Services\n\n{}\n",
        service_list.join("\n")
    )
}

fn build_service_gitignore_content(language: &str) -> String {
    let language_entries: Vec<&str> = match language {
        "node" => vec!["node_modules/", "dist/", "build/", ".next/", ".nuxt/", "coverage/", "*.tsbuildinfo", "npm-debug.log*", "yarn-debug.log*", "pnpm-debug.log*"],
        "rust" => vec!["/target/", "**/*.rs.bk"],
        "java" => vec!["target/", "*.class", ".gradle/", "build/", "out/"],
        "go" => vec!["/bin/", "*.test", "coverage.out"],
        _ => Vec::new(),
    };
    let common: Vec<&str> = vec![
        ".env", ".env.local", ".env.*.local", "*.log", "logs/", ".DS_Store", "Thumbs.db", ".idea/", ".vscode/", ".claude/", ".codegraph/", ".cursor/",
    ];
    let mut lines = vec![format!("# {language} project artifacts")];
    lines.extend(language_entries.iter().map(|s| s.to_string()));
    lines.push(String::new());
    lines.push("# Local configuration and tooling".to_string());
    lines.extend(common.iter().map(|s| s.to_string()));
    lines.push(String::new());
    lines.join("\n")
}

fn build_service_env_example_content(service: &CompositionService) -> Option<String> {
    if service.name == "frontend" {
        return Some(
            [
                "# Assigned by eco configure. Do not hard-code a port in application source.",
                "PORT=",
                "",
                "# Assigned by eco configure to the composed backend API.",
                "# Local: an allocated loopback URL. Production: the public gateway route.",
                "PUBLIC_API_URL=",
                "",
            ]
            .join("\n"),
        );
    }
    if service.language != "rust" {
        return None;
    }
    Some(
        [
            "# Assigned by eco configure. Do not hard-code a port in application source.",
            "SERVER_PORT=",
            "",
            "# Public route prefix implemented by this Rust API.",
            "# Change this only when the application uses a different API prefix.",
            "API_BASE_PATH=/api",
            "",
            "# Eco replaces this with the public application origin in production and",
            "# the allocated frontend URL in local development.",
            "CORS_ALLOWED_ORIGINS=",
            "",
        ]
        .join("\n"),
    )
}

fn build_starter_frontend_package_json(project_name: &str) -> String {
    let value = serde_json::json!({
        "name": format!("{project_name}-frontend"),
        "version": "0.1.0",
        "private": true,
        "scripts": { "start": "node index.js" }
    });
    format!("{}\n", serde_json::to_string_pretty(&value).unwrap_or_default())
}

fn build_starter_frontend_server() -> String {
    r##"const http = require("node:http");
const { readFile } = require("node:fs/promises");
const { readFileSync } = require("node:fs");
const path = require("node:path");

const root = __dirname;
const envFile = (() => { try { return Object.fromEntries(readFileSync(path.join(root, ".env"), "utf8").split(/\r?\n/).filter((line) => line && !line.startsWith("#")).map((line) => { const index = line.indexOf("="); return index < 0 ? [line, ""] : [line.slice(0, index), line.slice(index + 1)]; })); } catch { return {}; } })();
const port = Number(process.env.PORT || envFile.PORT);
const backendUrl = process.env.PUBLIC_API_URL || envFile.PUBLIC_API_URL;

if (!Number.isInteger(port) || port < 1 || port > 65535) {
  throw new Error("PORT is required. Run eco configure so Eco can assign this service port.");
}
if (!backendUrl) {
  throw new Error("PUBLIC_API_URL is required. Run eco configure so Eco can connect this frontend to its backend.");
}

const files = {
  "/": { file: "index.html", type: "text/html; charset=utf-8" },
  "/index.html": { file: "index.html", type: "text/html; charset=utf-8" },
  "/images/ecology-mark.webp": { file: "images/ecology-mark.webp", type: "image/webp" },
  "/runtime-config.js": { type: "application/javascript; charset=utf-8" }
};

http.createServer(async (request, response) => {
  const pathname = new URL(request.url, "http://" + (request.headers.host || "localhost")).pathname;
  const requested = files[pathname];
  if (!requested) {
    response.writeHead(404, { "content-type": "text/plain; charset=utf-8" });
    response.end("Not found");
    return;
  }
  if (pathname === "/runtime-config.js") {
    response.writeHead(200, { "content-type": requested.type, "cache-control": "no-store" });
    response.end("window.__ECO_BACKEND_URL__ = " + JSON.stringify(backendUrl) + ";");
    return;
  }
  try {
    response.writeHead(200, { "content-type": requested.type, "cache-control": "no-cache" });
    response.end(await readFile(path.join(root, requested.file)));
  } catch {
    response.writeHead(500, { "content-type": "text/plain; charset=utf-8" });
    response.end("Starter asset is unavailable.");
  }
}).listen(port, "0.0.0.0", () => {
  console.log("Eco starter frontend listening on http://0.0.0.0:" + port);
});
"##
    .to_string()
}

fn build_starter_backend_cargo_toml(project_name: &str) -> String {
    format!(
        "[package]\nname = \"{project_name}-backend\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\naxum = \"0.7\"\ntokio = {{ version = \"1\", features = [\"macros\", \"rt-multi-thread\", \"net\"] }}\ntower-http = {{ version = \"0.5\", features = [\"cors\"] }}\n"
    )
}

fn build_starter_backend_main() -> String {
    r##"use axum::{routing::get, Router};
use tokio::net::TcpListener;
use tower_http::cors::CorsLayer;

async fn hello_world() -> &'static str {
    "Ecology works! This is coming from Rust Backend!"
}

#[tokio::main]
async fn main() {
    let port = std::env::var("SERVER_PORT")
        .expect("SERVER_PORT is required; run eco configure so Eco can assign this service port");
    let app = Router::new()
        .route("/helloworld", get(hello_world))
        .route("/api/helloworld", get(hello_world))
        .layer(CorsLayer::permissive());
    let listener = TcpListener::bind(format!("0.0.0.0:{port}"))
        .await
        .expect("Eco starter backend could not bind its port");
    println!("Eco starter Rust backend listening on http://0.0.0.0:{port}");
    axum::serve(listener, app).await.expect("Eco starter backend stopped unexpectedly");
}
"##
    .to_string()
}

fn build_starter_frontend_html(project_name: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <meta name="description" content="A practical introduction to Eco and Domain-Driven Design." />
  <title>{project_name} · Eco starter</title>
  <style>
    :root {{ color-scheme: light; --ink:#10203d; --muted:#5f718c; --line:#dce5f0; --paper:#fffdf9; --soft:#f4f7fb; --blue:#3566bf; --blue-soft:#dce9fc; --cream:#fff0d9; --green:#2a8b69; }}
    * {{ box-sizing:border-box; }} body {{ margin:0; background:var(--soft); color:var(--ink); font:16px/1.55 Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }}
    .shell {{ max-width:1120px; margin:0 auto; padding:clamp(24px,5vw,72px) 24px 56px; }}
    .hero {{ display:grid; grid-template-columns:minmax(0,1.2fr) minmax(240px,.8fr); gap:clamp(28px,6vw,80px); align-items:center; padding:clamp(28px,6vw,72px); background:var(--paper); border:1px solid var(--line); border-radius:28px; box-shadow:0 18px 44px rgba(31,57,95,.08); }}
    .eyebrow {{ margin:0 0 14px; color:var(--blue); font-size:.78rem; font-weight:800; letter-spacing:.13em; text-transform:uppercase; }}
    h1 {{ max-width:12ch; margin:0; font-size:clamp(2.5rem,6vw,5.2rem); line-height:.96; letter-spacing:-.065em; }} h2 {{ margin:0 0 12px; font-size:clamp(1.45rem,2.5vw,2.15rem); letter-spacing:-.04em; }}
    .lede {{ max-width:53ch; margin:22px 0 0; color:var(--muted); font-size:clamp(1.03rem,2vw,1.22rem); }} .mark {{ width:min(100%,360px); justify-self:center; }}
    .tabs {{ display:flex; gap:0; margin-top:34px; overflow:auto; border-bottom:1px solid var(--line); }} .tab {{ appearance:none; border:0; border-bottom:3px solid transparent; padding:15px 18px; background:transparent; color:var(--muted); font:inherit; font-weight:750; white-space:nowrap; cursor:pointer; }} .tab[aria-selected="true"] {{ color:var(--ink); border-color:var(--blue); }}
    .chapter {{ display:none; padding:clamp(24px,4vw,52px) 0 0; }} .chapter.active {{ display:block; }} .chapter > p {{ max-width:68ch; color:var(--muted); }}
    .grid {{ display:grid; grid-template-columns:repeat(3,minmax(0,1fr)); gap:16px; margin-top:26px; }} .card {{ min-height:180px; padding:24px; background:var(--paper); border:1px solid var(--line); border-radius:18px; }} .card h3 {{ margin:0 0 8px; font-size:1.08rem; }} .card p {{ margin:0; color:var(--muted); }}
    .map {{ display:grid; grid-template-columns:1fr auto 1fr auto 1fr; gap:12px; align-items:center; margin-top:30px; padding:26px; background:var(--paper); border:1px solid var(--line); border-radius:20px; }} .domain {{ min-height:138px; padding:20px; border:2px solid var(--ink); border-radius:16px; background:var(--cream); }} .domain:nth-of-type(3) {{ background:var(--blue-soft); }} .domain:last-child {{ background:#e3f1eb; }} .domain strong {{ display:block; font-size:1.08rem; }} .domain span {{ display:block; margin-top:8px; color:#405270; font-size:.91rem; }} .arrow {{ color:var(--blue); font-size:1.9rem; font-weight:700; }}
    .path {{ display:grid; grid-template-columns:repeat(4,1fr); gap:12px; margin-top:26px; }} .step {{ position:relative; padding:22px 18px 18px; border-top:3px solid var(--blue); background:var(--paper); }} .step small {{ display:block; margin-bottom:8px; color:var(--blue); font-weight:800; letter-spacing:.08em; text-transform:uppercase; }} .step strong {{ font-size:1.05rem; }}
    .proof {{ margin-top:26px; display:flex; flex-wrap:wrap; gap:14px; align-items:center; padding:20px 22px; background:#ecf7f1; border:1px solid #b9e2cf; border-radius:16px; }} .proof strong {{ color:#17694f; }} .proof code {{ color:#17694f; font-family:ui-monospace, SFMono-Regular, Menlo, monospace; }} .status {{ color:#4c607d; }}
    .replace {{ margin-top:26px; padding:26px; border-left:4px solid var(--blue); background:var(--paper); }} .replace code {{ display:inline-block; padding:2px 6px; background:var(--soft); color:var(--ink); font-family:ui-monospace, SFMono-Regular, Menlo, monospace; }}
    @media (max-width:760px) {{ .hero {{ grid-template-columns:1fr; }} .mark {{ max-width:230px; }} .grid,.path {{ grid-template-columns:1fr; }} .map {{ grid-template-columns:1fr; }} .arrow {{ transform:rotate(90deg); text-align:center; }} }}
  </style>
</head>
<body>
  <main class="shell">
    <section class="hero">
      <div><p class="eyebrow">Eco starter composition</p><h1>Build systems that can grow without losing their shape.</h1><p class="lede">Eco turns Domain-Driven Design into a practical workspace: clear domains, explicit runtime contracts, and one composition layer that makes them feel like one application.</p></div>
      <img class="mark" src="/images/ecology-mark.webp" alt="Three connected domains forming an Ecology system" />
    </section>
    <nav class="tabs" aria-label="Eco guide chapters">
      <button class="tab" type="button" role="tab" aria-selected="true" aria-controls="why" id="why-tab">Why Eco</button>
      <button class="tab" type="button" role="tab" aria-selected="false" aria-controls="ddd" id="ddd-tab">DDD map</button>
      <button class="tab" type="button" role="tab" aria-selected="false" aria-controls="path" id="path-tab">Build path</button>
      <button class="tab" type="button" role="tab" aria-selected="false" aria-controls="next" id="next-tab">Your placeholder</button>
    </nav>
    <section class="chapter active" role="tabpanel" id="why" aria-labelledby="why-tab"><p class="eyebrow">Chapter 01</p><h2>Eco keeps the seams visible.</h2><p>A scalable product is not one enormous codebase. It is a set of bounded contexts that can change at their own pace, composed deliberately at the edge. Eco keeps the operational details—repositories, runtimes, CTs, deployment, and service exposure—in that same deliberate shape.</p><div class="grid"><article class="card"><h3>Domains stay focused</h3><p>Each repository owns one meaningful capability and its data contract.</p></article><article class="card"><h3>Composition stays human</h3><p>The project composition owns the experience that connects those capabilities.</p></article><article class="card"><h3>Operations stay explicit</h3><p>The estate manifest describes where services run and what they need.</p></article></div></section>
    <section class="chapter" role="tabpanel" id="ddd" aria-labelledby="ddd-tab"><p class="eyebrow">Chapter 02</p><h2>DDD is a conversation about boundaries.</h2><p>Domains communicate through contracts. They do not reach into one another's implementation or database. The composition layer speaks to those contracts and turns them into a coherent customer journey.</p><div class="map"><div class="domain"><strong>Identity</strong><span>Who is this person? What may they do?</span></div><div class="arrow" aria-hidden="true">→</div><div class="domain"><strong>Inventory</strong><span>What is owned, valued, and available?</span></div><div class="arrow" aria-hidden="true">→</div><div class="domain"><strong>Marketplace</strong><span>What can be discovered or traded?</span></div></div></section>
    <section class="chapter" role="tabpanel" id="path" aria-labelledby="path-tab"><p class="eyebrow">Chapter 03</p><h2>Eco gives the first vertical slice a home.</h2><p>Start with a visible page and a small API. Then replace the placeholder with real domains as your product becomes clearer.</p><div class="path"><div class="step"><small>01</small><strong>Describe the estate</strong></div><div class="step"><small>02</small><strong>Compose services</strong></div><div class="step"><small>03</small><strong>Run with Eco</strong></div><div class="step"><small>04</small><strong>Grow bounded contexts</strong></div></div><div class="proof"><strong>Runtime proof</strong><span id="backend-status" class="status">Contacting the Rust backend…</span></div></section>
    <section class="chapter" role="tabpanel" id="next" aria-labelledby="next-tab"><p class="eyebrow">Chapter 04</p><h2>This starter is meant to be replaced.</h2><p><code>{project_name}_composition/frontend</code> and, if selected, <code>{project_name}_composition/backend</code> are runnable placeholders. Delete them when you are ready to create the actual project with the vibecoding model you choose.</p><div class="replace"><strong>Keep the composition contract.</strong><br />Your future frontend remains the public entry point; your future backend remains a separately-owned API. Eco will continue to provision and run what the manifest declares.</div></section>
  </main>
  <script src="/runtime-config.js"></script>
  <script>
    const tabs = [...document.querySelectorAll('.tab')];
    const chapters = [...document.querySelectorAll('.chapter')];
    tabs.forEach((tab) => tab.addEventListener('click', () => {{ tabs.forEach((item) => item.setAttribute('aria-selected', String(item === tab))); chapters.forEach((chapter) => chapter.classList.toggle('active', chapter.id === tab.getAttribute('aria-controls'))); }}));
    fetch(window.__ECO_BACKEND_URL__ + '/helloworld').then((response) => response.ok ? response.text() : Promise.reject()).then((message) => {{ document.querySelector('#backend-status').textContent = message; }}).catch(() => {{ document.querySelector('#backend-status').textContent = 'The backend will appear here after the optional Rust service starts.'; }});
  </script>
</body>
</html>
"#
    )
}

fn create_composition_scaffold(
    composition_repo_path: &Path,
    project_name: &str,
    composition_services: &[CompositionService],
) -> Result<(), String> {
    std::fs::create_dir_all(composition_repo_path).map_err(|e| e.to_string())?;
    std::fs::write(
        composition_repo_path.join("README.md"),
        build_composition_readme(project_name, composition_services),
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(composition_repo_path.join(".gitignore"), build_gitignore_content()).map_err(|e| e.to_string())?;

    for service in composition_services {
        let service_dir = composition_repo_path.join(&service.path);
        std::fs::create_dir_all(&service_dir).map_err(|e| e.to_string())?;
        std::fs::write(service_dir.join(".gitignore"), build_service_gitignore_content(&service.language)).map_err(|e| e.to_string())?;
        if let Some(env_example) = build_service_env_example_content(service) {
            std::fs::write(service_dir.join(".env.example"), env_example).map_err(|e| e.to_string())?;
        }
        if service.name == "frontend" {
            let images_dir = service_dir.join("images");
            std::fs::create_dir_all(&images_dir).map_err(|e| e.to_string())?;
            std::fs::write(service_dir.join("package.json"), build_starter_frontend_package_json(project_name)).map_err(|e| e.to_string())?;
            std::fs::write(service_dir.join("index.js"), build_starter_frontend_server()).map_err(|e| e.to_string())?;
            std::fs::write(service_dir.join("index.html"), build_starter_frontend_html(project_name)).map_err(|e| e.to_string())?;
            let mark = embedded::ecology_mark_path();
            if mark.exists() {
                let _ = std::fs::copy(&mark, images_dir.join("ecology-mark.webp"));
            }
        }
        if service.name == "backend" {
            let src_dir = service_dir.join("src");
            std::fs::create_dir_all(&src_dir).map_err(|e| e.to_string())?;
            std::fs::write(service_dir.join("Cargo.toml"), build_starter_backend_cargo_toml(project_name)).map_err(|e| e.to_string())?;
            std::fs::write(src_dir.join("main.rs"), build_starter_backend_main()).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn build_clone_plan(
    target_root: &Path,
    selected_repos: &[String],
    repo_catalog: &[repos::RepoEntry],
    branch_overrides: &std::collections::HashMap<String, String>,
) -> Result<Vec<(String, String, String, PathBuf)>, String> {
    let by_name: std::collections::HashMap<String, &repos::RepoEntry> =
        repo_catalog.iter().map(|r| (r.name.clone(), r)).collect();
    let mut plan = Vec::new();
    for repo_name in selected_repos {
        let repo = by_name.get(repo_name).ok_or_else(|| format!("Unknown repo in catalog: {repo_name}"))?;
        let branch = branch_overrides
            .get(repo_name)
            .cloned()
            .unwrap_or_else(|| repo.branch.clone());
        plan.push((repo.name.clone(), repo.git.clone(), branch, target_root.join(&repo.name)));
    }
    Ok(plan)
}

fn clone_selected_repos(clone_plan: &[(String, String, String, PathBuf)]) -> Result<(), String> {
    for (_, git, branch, target_path) in clone_plan {
        if target_path.join(".git").exists() {
            continue;
        }
        if target_path.exists() {
            return Err(format!("Refusing to clone into existing non-git path: {}", target_path.display()));
        }
        run_command("git", &["clone".to_string(), "--branch".to_string(), branch.clone(), git.clone(), target_path.display().to_string()], &util::current_dir())?;
    }
    Ok(())
}

fn merge_env_values(content: &str, values: &std::collections::HashMap<String, String>) -> String {
    let mut lines: Vec<String> = if content.is_empty() {
        Vec::new()
    } else {
        content.split('\n').map(|s| s.to_string()).collect()
    };
    for (key, value) in values {
        let rendered = format!("{key}={value}");
        let index = lines.iter().position(|l| l.starts_with(&format!("{key}=")));
        match index {
            Some(i) => lines[i] = rendered,
            None => lines.push(rendered),
        }
    }
    while lines.last().map(|l| l.is_empty()).unwrap_or(false) {
        lines.pop();
    }
    let joined = lines.join("\n");
    format!("{}\n", joined.trim_end())
}

fn write_local_auth_email_env(target_root: &Path, auth: &Option<AuthEmailVerification>) -> Result<(), String> {
    let Some(auth) = auth else {
        return Ok(());
    };
    let env_file = target_root.join("auth").join("backend").join(".env");
    let env_example = format!("{}.example", env_file.display());
    let content = std::fs::read_to_string(&env_file)
        .or_else(|_| std::fs::read_to_string(&env_example))
        .unwrap_or_default();
    let mut values = std::collections::HashMap::new();
    values.insert("EMAIL_VERIFICATION_REQUIRED".to_string(), if auth.required { "true".to_string() } else { "false".to_string() });
    if auth.required {
        values.insert("EMAIL_VERIFICATION_TTL_HOURS".to_string(), auth.ttl_hours.to_string());
        values.insert("MAIL_FROM_EMAIL".to_string(), auth.mail_from_email.clone());
        values.insert("MAIL_FROM_NAME".to_string(), auth.mail_from_name.clone());
        values.insert("BREVO_API_KEY".to_string(), auth.brevo_api_key.clone());
    }
    let _ = std::fs::create_dir_all(env_file.parent().unwrap_or(Path::new(".")));
    std::fs::write(&env_file, merge_env_values(&content, &values)).map_err(|e| e.to_string())?;
    Ok(())
}

fn assert_scaffold_targets_available(
    primary_repo_path: &Path,
    composition_repo_path: &Path,
    non_interactive: bool,
) -> Result<(), String> {
    let mut existing_paths: Vec<PathBuf> = Vec::new();
    if primary_repo_path.exists() {
        existing_paths.push(primary_repo_path.to_path_buf());
    }
    if composition_repo_path.exists() {
        existing_paths.push(composition_repo_path.to_path_buf());
    }
    if existing_paths.is_empty() {
        return Ok(());
    }
    util::println_stdout(&format!(
        "\nDirectory already exists:\n{}",
        existing_paths.iter().map(|p| format!("  - {}", p.display())).collect::<Vec<_>>().join("\n")
    ));
    if non_interactive {
        util::println_stdout("Removing existing scaffold directories (--yes).");
        for p in &existing_paths {
            let _ = std::fs::remove_dir_all(p);
        }
        return Ok(());
    }
    let overwrite = checklist::confirm_with_single_key("Remove the existing scaffold directories and recreate from scratch?", false)?;
    if !overwrite {
        return Err("Cancelled.".to_string());
    }
    for p in &existing_paths {
        let _ = std::fs::remove_dir_all(p);
    }
    Ok(())
}

struct StorageDetails {
    ct: String,
    region: String,
}

struct EstateDetails {
    ct_id: i64,
    hostname: String,
    cloudflare_account: String,
    expose_service: String,
    storage: Option<StorageDetails>,
    staging_ct: i64,
    staging_enabled: bool,
}

struct AuthEmailVerification {
    required: bool,
    ttl_hours: i64,
    mail_from_email: String,
    mail_from_name: String,
    brevo_api_key: String,
}

fn prompt_storage_details(non_interactive: bool, no_storage: bool) -> Result<Option<StorageDetails>, String> {
    if non_interactive {
        util::println_stdout("\nObject storage (non-interactive)");
        if no_storage {
            util::println_stdout("  Not configured.\n");
            return Ok(None);
        }
        util::println_stdout("  MinIO: ct=storage, region=us-east-1\n");
        return Ok(Some(StorageDetails { ct: "storage".to_string(), region: "us-east-1".to_string() }));
    }
    util::println_stdout("\nObject storage (MinIO / S3-compatible)");
    util::println_stdout("  Eco uses managed S3-compatible MinIO. Development provisions it\n  locally; production provisions it in one dedicated MinIO CT and\n  keeps application traffic on Proxmox's private bridge.\n");
    let use_minio = checklist::confirm_with_single_key("  Configure object storage for this estate?", true)?;
    if !use_minio {
        return Ok(None);
    }
    let ct = checklist::prompt_line("  storage.minio.ct (MinIO CT hostname or VMID) [storage]: ")?;
    let ct = if ct.trim().is_empty() { "storage".to_string() } else { ct.trim().to_string() };
    let region = checklist::prompt_line("  storage.minio.region [us-east-1]: ")?;
    let region = if region.trim().is_empty() { "us-east-1".to_string() } else { region.trim().to_string() };
    Ok(Some(StorageDetails { ct, region }))
}

fn prompt_ecompose_details(
    project_name: &str,
    frontend_service: &str,
    non_interactive: bool,
    flags: &StartProjectFlags,
) -> Result<EstateDetails, String> {
    if non_interactive {
        let ct_id = flags.ct_id.unwrap_or(101);
        let default_hostname = format!("{project_name}.jogjaitcamp.com");
        let hostname = flags.hostname.clone().unwrap_or(default_hostname);
        let cloudflare_account = flags.cloudflare_account.clone().unwrap_or_else(|| "jogjaitcamp".to_string());
        let staging_ct = flags.staging_ct.unwrap_or(1000);
        let staging_enabled = !flags.no_staging;
        let storage = prompt_storage_details(true, flags.no_storage)?;
        util::println_stdout(&format!(
            "\n  ct.id: {ct_id}\n  expose.hostname: {hostname}\n  expose.cloudflare_account: {cloudflare_account}\n  expose.service: {frontend_service}\n  staging: {}\n  storage: {}\n",
            if staging_enabled { format!("ct {staging_ct}") } else { "disabled".to_string() },
            if storage.is_some() { format!("minio CT ({})", storage.as_ref().unwrap().ct) } else { "not configured".to_string() }
        ));
        return Ok(EstateDetails {
            ct_id,
            hostname,
            cloudflare_account,
            expose_service: frontend_service.to_string(),
            storage,
            staging_ct,
            staging_enabled,
        });
    }

    let ct_id_raw = checklist::prompt_line("  ct.id [101]: ")?;
    let ct_id: i64 = if ct_id_raw.trim().is_empty() { 101 } else { ct_id_raw.trim().parse().map_err(|_| "CT ID must be a positive integer.".to_string())? };
    if ct_id <= 0 {
        return Err("CT ID must be a positive integer.".to_string());
    }
    let default_hostname = format!("{project_name}.jogjaitcamp.com");
    let hostname = checklist::prompt_line(&format!("  expose.hostname [{default_hostname}]: "))?;
    let hostname = if hostname.trim().is_empty() { default_hostname } else { hostname.trim().to_string() };
    if hostname.is_empty() {
        return Err("Hostname is required.".to_string());
    }
    let cloudflare_account = checklist::prompt_line("  expose.cloudflare_account [jogjaitcamp]: ")?;
    let cloudflare_account = if cloudflare_account.trim().is_empty() { "jogjaitcamp".to_string() } else { cloudflare_account.trim().to_string() };

    let storage = prompt_storage_details(false, false)?;
    let staging_enabled = checklist::confirm_with_single_key("  Enable staging?", true)?;
    let staging_ct = if staging_enabled {
        let raw = checklist::prompt_line("  staging.ct [1000]: ")?;
        if raw.trim().is_empty() {
            1000
        } else {
            let parsed: i64 = raw.trim().parse().map_err(|_| "Staging CT ID must be a positive integer.".to_string())?;
            if parsed <= 0 {
                return Err("Staging CT ID must be a positive integer.".to_string());
            }
            parsed
        }
    } else {
        1000
    };
    Ok(EstateDetails {
        ct_id,
        hostname,
        cloudflare_account,
        expose_service: frontend_service.to_string(),
        storage,
        staging_ct,
        staging_enabled,
    })
}

fn prompt_superadmin_setup(non_interactive: bool) -> Result<bool, String> {
    if non_interactive {
        return Ok(false);
    }
    util::println_stdout("\nSuperadmin / Setup flow");
    util::println_stdout("  When enabled, a fresh deployment shows a /setup page before anything");
    util::println_stdout("  else. The first visitor claims the superadmin role, then the app");
    util::println_stdout("  switches to normal login. Modeled after apindo's setup flow:");
    util::println_stdout("  GET /setup/status → POST /setup/claim.\n");
    checklist::confirm_with_single_key("  Enable superadmin setup flow?", false)
}

fn prompt_auth_email_verification(
    selected_repos: &[String],
    project_name: &str,
    non_interactive: bool,
    no_email_verification: bool,
) -> Result<Option<AuthEmailVerification>, String> {
    if !selected_repos.iter().any(|r| r == "auth") {
        return Ok(None);
    }
    if non_interactive {
        util::println_stdout("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        util::println_stdout("  Auth domain — registration email verification (non-interactive)");
        util::println_stdout("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        if no_email_verification {
            util::println_stdout("  Email verification disabled.");
            return Ok(Some(AuthEmailVerification {
                required: false,
                ttl_hours: 24,
                mail_from_email: String::new(),
                mail_from_name: String::new(),
                brevo_api_key: String::new(),
            }));
        }
        let default_mail_from_name = capitalise_project_name(project_name);
        util::println_stdout("  Verification: enabled");
        util::println_stdout("  MAIL_FROM_EMAIL: no-reply@jogjaitcamp.com");
        util::println_stdout(&format!("  MAIL_FROM_NAME: {default_mail_from_name}"));
        util::println_stdout("  EMAIL_VERIFICATION_TTL_HOURS: 24");
        util::println_stdout("  BREVO_API_KEY: (set in .env after scaffold)\n");
        return Ok(Some(AuthEmailVerification {
            required: true,
            ttl_hours: 24,
            mail_from_email: "no-reply@jogjaitcamp.com".to_string(),
            mail_from_name: default_mail_from_name,
            brevo_api_key: String::new(),
        }));
    }

    let verify = checklist::confirm_with_single_key("  Verify registration email", true)?;
    if !verify {
        return Ok(Some(AuthEmailVerification {
            required: false,
            ttl_hours: 24,
            mail_from_email: String::new(),
            mail_from_name: String::new(),
            brevo_api_key: String::new(),
        }));
    }
    let default_mail_from_name = capitalise_project_name(project_name);
    let mail_from_email = checklist::prompt_line("  MAIL_FROM_EMAIL [no-reply@jogjaitcamp.com]: ")?;
    let mail_from_email = if mail_from_email.trim().is_empty() { "no-reply@jogjaitcamp.com".to_string() } else { mail_from_email.trim().to_string() };
    let mail_from_name = checklist::prompt_line(&format!("  MAIL_FROM_NAME [{default_mail_from_name}]: "))?;
    let mail_from_name = if mail_from_name.trim().is_empty() { default_mail_from_name } else { mail_from_name.trim().to_string() };
    let brevo_api_key = checklist::prompt_line("  BREVO_API_KEY (Paste given BREVO API KEY): ")?;
    let brevo_api_key = brevo_api_key.trim().to_string();
    let ttl_raw = checklist::prompt_line("  EMAIL_VERIFICATION_TTL_HOURS [24]: ")?;
    let ttl_hours: i64 = if ttl_raw.trim().is_empty() { 24 } else { ttl_raw.trim().parse().map_err(|_| "EMAIL_VERIFICATION_TTL_HOURS must be a positive whole number.".to_string())? };
    if ttl_hours <= 0 {
        return Err("EMAIL_VERIFICATION_TTL_HOURS must be a positive whole number.".to_string());
    }
    Ok(Some(AuthEmailVerification {
        required: true,
        ttl_hours,
        mail_from_email,
        mail_from_name,
        brevo_api_key,
    }))
}

fn prompt_repo_placements(
    selected_repos: &[String],
    repo_catalog: &[repos::RepoEntry],
    non_interactive: bool,
    branch_overrides_input: &std::collections::HashMap<String, String>,
) -> Result<(std::collections::HashMap<String, String>, std::collections::HashMap<String, String>), String> {
    let by_name: std::collections::HashMap<String, &repos::RepoEntry> =
        repo_catalog.iter().map(|r| (r.name.clone(), r)).collect();
    let mut branch_overrides = std::collections::HashMap::new();
    let mut dev_flags = std::collections::HashMap::new();

    if non_interactive {
        util::println_stdout("\nRepo branches & environments (non-interactive)");
        for repo_name in selected_repos {
            let repo = by_name.get(repo_name);
            let default_branch = repo.map(|r| r.branch.clone()).unwrap_or_else(|| "main".to_string());
            let branch_override = branch_overrides_input.get(repo_name).cloned();
            if let Some(bo) = &branch_override {
                if bo != &default_branch {
                    branch_overrides.insert(repo_name.clone(), bo.clone());
                    util::println_stdout(&format!("  {repo_name}: branch={bo}, dev=optional, prod=enabled"));
                } else {
                    util::println_stdout(&format!("  {repo_name}: branch={default_branch}, dev=optional, prod=enabled"));
                }
            } else {
                util::println_stdout(&format!("  {repo_name}: branch={default_branch}, dev=optional, prod=enabled"));
            }
            dev_flags.insert(repo_name.clone(), "optional".to_string());
        }
        util::println_stdout("");
        return Ok((branch_overrides, dev_flags));
    }

    for repo_name in selected_repos {
        let repo = by_name.get(repo_name);
        let default_branch = repo.map(|r| r.branch.clone()).unwrap_or_else(|| "main".to_string());
        let input = checklist::prompt_line(&format!("  {repo_name} branch [{default_branch}]: "))?;
        if !input.trim().is_empty() && input.trim() != default_branch {
            branch_overrides.insert(repo_name.clone(), input.trim().to_string());
        }
        let selection = checklist::run_checklist(
            &[
                checklist::ChecklistItem { id: "dev".to_string(), label: format!("{repo_name} in local dev (optional)") },
                checklist::ChecklistItem { id: "prod".to_string(), label: format!("{repo_name} in prod (mandatory)") },
            ],
            &format!("  {repo_name} environments"),
            "  Controls: ↑/↓ move, space toggle, Enter confirm",
            None,
            None,
            1,
            &["dev".to_string(), "prod".to_string()],
            &["prod".to_string()],
        )?;
        dev_flags.insert(repo_name.clone(), if selection.iter().any(|s| s == "dev") { "optional".to_string() } else { "disabled".to_string() });
    }
    Ok((branch_overrides, dev_flags))
}

fn confirm_plan(project_name: &str, target_root: &Path, current_dir_mode: bool, selected_repos: &[String], clone_plan: &[(String, String, String, PathBuf)], primary_repo_path: &Path, composition_repo_path: &Path, composition_services: &[CompositionService], github_repositories: &[github::GithubRepoInfo], details: &EstateDetails, auth_email_verification: &Option<AuthEmailVerification>, dev_flags: &std::collections::HashMap<String, String>, non_interactive: bool, superadmin_setup: bool) -> Result<bool, String> {
    let mut out = String::new();
    out.push_str("\nProject scaffold plan:\n");
    out.push_str(&format!("  project:          {project_name}\n"));
    out.push_str(&format!("  estate root:      {}{}\n", target_root.display(), if current_dir_mode { " (current directory)" } else { "" }));
    out.push_str(&format!("  bootstrap repo:   {}\n", primary_repo_path.display()));
    out.push_str(&format!("  composition repo: {}\n", composition_repo_path.display()));
    out.push_str(&format!("  composition:      {}\n", composition_services.iter().map(|s| s.name.clone()).collect::<Vec<_>>().join(", ")));
    out.push_str(&format!("  ct.id:            {}\n", details.ct_id));
    out.push_str(&format!("  hostname:         {}\n", details.hostname));
    out.push_str(&format!("  cloudflare:       {}\n", details.cloudflare_account));
    out.push_str(&format!("  expose.service:   {}\n", details.expose_service));
    out.push_str(&format!("  staging:          {}\n", if details.staging_enabled { format!("ct {}", details.staging_ct) } else { "disabled".to_string() }));
    out.push_str(&format!("  object storage:   {}\n", if details.storage.is_some() { format!("minio CT ({})", details.storage.as_ref().unwrap().ct) } else { "not configured".to_string() }));
    if superadmin_setup {
        out.push_str("  superadmin setup: enabled\n");
    }
    if let Some(auth) = auth_email_verification {
        out.push_str(&format!("  auth email:       {}\n", if auth.required { format!("verification required ({}h)", auth.ttl_hours) } else { "verification disabled".to_string() }));
    }
    out.push_str(&format!("  selected repos:   {}\n", selected_repos.join(", ")));
    if !selected_repos.is_empty() {
        let placements: Vec<String> = selected_repos
            .iter()
            .map(|r| if dev_flags.get(r).map(|s| s.as_str()) == Some("disabled") { format!("{r} (prod-only)") } else { r.clone() })
            .collect();
        out.push_str(&format!("  environments:     {}\n", placements.join(", ")));
    }
    out.push_str("  clone plan:\n");
    for (name, git, branch, target) in clone_plan {
        let branch_note = if branch == "main" { String::new() } else { format!(" [branch: {branch}]") };
        out.push_str(&format!("    - {name}: {git} -> {}{branch_note}\n", target.display()));
    }
    let existing_repos: Vec<&github::GithubRepoInfo> = github_repositories.iter().filter(|r| r.exists).collect();
    if !existing_repos.is_empty() {
        out.push_str("\nWARNING: Existing GitHub repository content will be removed:\n");
        for item in &existing_repos {
            out.push_str(&format!("  - {}\n", item.html_url));
        }
        out.push_str("  Eco will clone each repository, delete its working-tree content, then commit and push this new scaffold.\n");
    }
    print!("{out}");
    if non_interactive {
        util::println_stdout("\nProceeding automatically (--yes).");
        return Ok(true);
    }
    if !existing_repos.is_empty() {
        checklist::confirm_with_single_key("Replace the existing GitHub repository content?", true)
    } else {
        checklist::confirm_with_single_key("Create this project scaffold?", true)
    }
}

pub fn run_startproject(args: &[String]) -> Result<(), String> {
    let flags = parse_flags(args);
    let non_interactive = flags.yes;
    let cwd = util::current_dir();
    let workspace_root = workspace::find_workspace_root(&cwd)?;
    let (project_name, target_root, current_dir_mode) = prompt_project_name(&flags.remaining, &workspace_root, &cwd)?;
    if util::env_var_or("ECO_GITHUB_API_KEY", "").is_empty() {
        return Err("ECO_GITHUB_API_KEY is required to create and push the bootstrap and composition repositories.".to_string());
    }

    let selected_composition_services = prompt_composition_services(&project_name, non_interactive)?;
    util::println_stdout("\nComposition starter\n  frontend: Node.js\n  backend: Rust (when selected)\n");
    let backend_databases = prompt_backend_databases(&selected_composition_services, non_interactive)?;
    let composition_services = build_composition_services(&selected_composition_services, &backend_databases);
    let repo_catalog = repos::read_repo_catalog()?;
    let selected_repos = run_repo_checklist(&repo_catalog, &project_name, non_interactive, &flags.repos)?;
    let selected_repos_with_deps = compute_dependency_closure(&selected_repos, &repo_catalog);
    let auth_email_verification = prompt_auth_email_verification(&selected_repos_with_deps, &project_name, non_interactive, flags.no_email_verification)?;
    let superadmin_setup = prompt_superadmin_setup(non_interactive)?;
    let (branch_overrides, dev_flags) = prompt_repo_placements(&selected_repos_with_deps, &repo_catalog, non_interactive, &flags.branch_overrides)?;

    let clone_plan = build_clone_plan(&target_root, &selected_repos_with_deps, &repo_catalog, &branch_overrides)?;
    let service_templates = discover_service_templates(&workspace_root)?;
    let primary_repo_path = target_root.join(format!("{project_name}_core"));
    let composition_repo_path = target_root.join(format!("{project_name}_composition"));
    let (github_login, github_repositories) = github::inspect_github_repositories(&[
        format!("{project_name}_core"),
        format!("{project_name}_composition"),
    ])?;

    let details = prompt_ecompose_details(&project_name, "frontend", non_interactive, &flags)?;

    let confirmed = confirm_plan(
        &project_name,
        &target_root,
        current_dir_mode,
        &selected_repos_with_deps,
        &clone_plan,
        &primary_repo_path,
        &composition_repo_path,
        &composition_services,
        &github_repositories,
        &details,
        &auth_email_verification,
        &dev_flags,
        non_interactive,
        superadmin_setup,
    )?;
    if !confirmed {
        return Err("Cancelled.".to_string());
    }

    assert_scaffold_targets_available(&primary_repo_path, &composition_repo_path, non_interactive)?;
    if !current_dir_mode {
        std::fs::create_dir_all(&target_root).map_err(|e| e.to_string())?;
    }

    let bootstrap_repository_plan = github_repositories.iter().find(|item| item.name == format!("{project_name}_core"));
    let composition_repository_plan = github_repositories.iter().find(|item| item.name == format!("{project_name}_composition"));

    if let Some(plan) = bootstrap_repository_plan {
        if plan.exists {
            clone_and_clear_repository(&primary_repo_path, plan)?;
        } else {
            std::fs::create_dir_all(&primary_repo_path).map_err(|e| {
                format!(
                    "Cannot create bootstrap directory {}: {e}",
                    primary_repo_path.display()
                )
            })?;
        }
    } else {
        std::fs::create_dir_all(&primary_repo_path).map_err(|e| e.to_string())?;
    }

    let ecompose_path = primary_repo_path.join("ecompose.yml");
    let readme_path = primary_repo_path.join("README.md");
    let gitignore_path = primary_repo_path.join(".gitignore");

    let created_readme = !readme_path.exists();
    if created_readme {
        std::fs::write(&readme_path, build_readme_content(&project_name)).map_err(|e| e.to_string())?;
    }
    let created_gitignore = !gitignore_path.exists();
    if created_gitignore {
        std::fs::write(&gitignore_path, build_gitignore_content()).map_err(|e| e.to_string())?;
    }

    let composition_git = composition_git_url(
        &composition_repository_plan.cloned(),
        &github_login,
        &format!("{project_name}_composition"),
    );

    let ecompose_content = build_ecompose_content(
        &project_name,
        &selected_repos,
        &service_templates,
        &composition_services,
        &details,
        auth_email_verification.as_ref(),
        &branch_overrides,
        &dev_flags,
        &composition_git,
        superadmin_setup,
    );
    std::fs::write(&ecompose_path, ecompose_content).map_err(|e| e.to_string())?;

    let bootstrap_repository = initialise_and_push_repository(
        &primary_repo_path,
        &format!("{project_name}_core"),
        &format!("init: {project_name} estate manifest"),
        bootstrap_repository_plan.filter(|plan| plan.exists),
    )?;

    if let Some(plan) = composition_repository_plan {
        if plan.exists {
            clone_and_clear_repository(&composition_repo_path, plan)?;
        }
    }
    create_composition_scaffold(&composition_repo_path, &project_name, &composition_services)?;
    let composition_repository = initialise_and_push_repository(
        &composition_repo_path,
        &format!("{project_name}_composition"),
        &format!("init: {project_name} composition"),
        composition_repository_plan.filter(|plan| plan.exists),
    )?;

    clone_selected_repos(&clone_plan)?;
    write_local_auth_email_env(&target_root, &auth_email_verification)?;

    let bootstrap_dir_name = format!("{project_name}_core");

    let mut out = String::new();
    out.push_str(&format!("\n{} in {}\n", util::bold("Project scaffold created"), target_root.display()));
    out.push_str(&format!("{}- {}\n", util::dim("-"), ecompose_path.display()));
    if created_readme {
        out.push_str(&format!("{}- {}\n", util::dim("-"), readme_path.display()));
    }
    if created_gitignore {
        out.push_str(&format!("{}- {}\n", util::dim("-"), gitignore_path.display()));
    }
    out.push_str(&format!("{}- {}\n", util::dim("-"), composition_repo_path.display()));
    out.push_str(&format!("{}- GitHub: {}\n", util::dim("-"), bootstrap_repository.html_url));
    out.push_str(&format!("{}- GitHub: {}\n", util::dim("-"), composition_repository.html_url));

    let proxmox_host = util::env_var_or("PROXMOX_HOST", "<your-proxmox-host>");
    out.push_str(&format!(
        "\n{}\n  {}\n{}\n\n  {}0. Start local dev environment{}\n     From the estate root:\n\n       {}\n       {}\n\n     {}\n     {}\n\n  {}1. Repositories created and pushed{}\n     {}\n     {}\n\n  {}2. Deploy on Proxmox{}\n     {}\n\n       {}\n       {}\n       {}\n\n     {}\n     {}\n\n  {}3. Re-deploy after changes{}\n\n       {}\n{}\n\n  {}\n\n       {}\n       {}\n\n  {}\n\n",
        util::sep(56),
        util::bold("Next steps"),
        util::sep(56),
        util::bold(""),
        util::bold(""),
        util::cmd_bold_cyan(&format!("cd {}", target_root.display())),
        util::cmd_bold_cyan("eco up"),
        util::dim("Services start and PM2 logs follow automatically."),
        util::dim("Ctrl+C stops log tailing — services keep running."),
        util::bold(""),
        util::bold(""),
        util::dim("The bootstrap and composition repositories are private GitHub repos."),
        util::dim("The composition includes frontend first, plus the optional backend you selected."),
        util::bold(""),
        util::bold(""),
        util::dim("SSH into your Proxmox host and run:"),
        util::cmd_bold_cyan(&format!("ssh root@{proxmox_host}")),
        util::cmd_bold_cyan(&format!("git clone {} /root/{bootstrap_dir_name}", if bootstrap_repository.ssh_url.is_empty() { bootstrap_repository.clone_url.clone() } else { bootstrap_repository.ssh_url.clone() })),
        util::cmd_bold_cyan("eco up"),
        util::dim(&format!("eco up will create CT {}, clone domain repos, install", details.ct_id)),
        util::dim("runtimes, wire .env files, start PM2 services,"),
        util::bold(""),
        util::bold(""),
        util::cmd_bold_cyan(&format!("cd /root/{bootstrap_dir_name} && git pull && eco up")),
        util::sep(56),
        util::bold(&format!("Start working on your {project_name} project:")),
        util::cmd_bold_cyan(&format!("cd {}", primary_repo_path.display())),
        util::cmd_bold_cyan("eco up"),
        util::bold("Happy Vibe Coding!")
    ));
    print!("{out}");
    Ok(())
}
