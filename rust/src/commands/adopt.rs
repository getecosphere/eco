use crate::ecompose;
use crate::util;
use std::path::Path;

const DEFAULT_CT_TEMPLATE: &str = "local:vztmpl/eco-npm-rust-mongo_1_amd64.tar.zst";
const DEFAULT_SHARED_TOOLS: &[&str] = &["git", "openssh-client", "curl", "jq", "ca-certificates"];
const IGNORED_DIR_NAMES: &[&str] = &[
    "node_modules",
    "target",
    ".next",
    ".git",
    "dist",
    "build",
    "vendor",
    ".venv",
    "__pycache__",
];

fn path_exists(p: &Path) -> bool {
    p.exists()
}

fn read_text_file(p: &Path) -> Option<String> {
    std::fs::read_to_string(p).ok()
}

/// Same runtime detection as configure.sh's discover_services.
pub fn detect_service_type(dir: &Path) -> Option<(String, Vec<String>)> {
    if dir.join("pom.xml").is_file() {
        return Some(("spring-boot".to_string(), vec!["java@17".to_string(), "maven".to_string()]));
    }
    if dir.join("Cargo.toml").is_file() {
        return Some(("rust".to_string(), vec!["rust".to_string()]));
    }
    let package_json_raw = read_text_file(&dir.join("package.json"));
    if let Some(raw) = package_json_raw {
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(&raw);
        let deps = match parsed {
            Ok(v) => {
                let mut combined = std::collections::HashMap::new();
                if let Some(d) = v.get("dependencies").and_then(|x| x.as_object()) {
                    for (k, val) in d {
                        combined.insert(k.clone(), val.clone());
                    }
                }
                if let Some(d) = v.get("devDependencies").and_then(|x| x.as_object()) {
                    for (k, val) in d {
                        combined.insert(k.clone(), val.clone());
                    }
                }
                combined
            }
            Err(_) => std::collections::HashMap::new(),
        };
        if deps.contains_key("next") {
            return Some(("nextjs".to_string(), vec!["node@20".to_string(), "npm".to_string(), "pm2".to_string()]));
        }
        if deps.contains_key("vite") {
            return Some(("vite".to_string(), vec!["node@20".to_string(), "npm".to_string(), "pm2".to_string()]));
        }
        return Some(("node".to_string(), vec!["node@20".to_string(), "npm".to_string(), "pm2".to_string()]));
    }
    None
}

pub fn detect_db_runtimes(dir: &Path) -> Vec<String> {
    let contents = read_text_file(&dir.join(".env.example"))
        .or_else(|| read_text_file(&dir.join(".env")))
        .unwrap_or_default();
    let cargo_toml = read_text_file(&dir.join("Cargo.toml")).unwrap_or_default();
    let mut runtimes = Vec::new();
    if contains_line(&contents, "MONGO(?:DB)?_URI=") || cargo_toml.contains("mongodb") {
        runtimes.push("mongodb@7".to_string());
    }
    if contains_line(&contents, "REDIS_URL=") || cargo_toml.contains("redis") {
        runtimes.push("redis@7".to_string());
    }
    if contains_line_multiline(&contents, "DATABASE_URL|DB_URL") && contents.to_lowercase().contains("postgres") {
        runtimes.push("postgresql@15".to_string());
    }
    runtimes
}

fn contains_line(content: &str, key_pattern: &str) -> bool {
    // simple line prefix match for MONGO(DB)?_URI= / REDIS_URL=
    for line in content.split('\n') {
        let line = line.trim_start();
        if key_pattern.contains("?") {
            let bare = line.split('=').next().unwrap_or("");
            if bare == "MONGO_URI" || bare == "MONGODB_URI" {
                return true;
            }
        } else if line.starts_with(key_pattern.trim_end_matches('=')) && line.contains('=') {
            return true;
        }
    }
    false
}

fn contains_line_multiline(content: &str, _pattern: &str) -> bool {
    for line in content.split('\n') {
        let line = line.trim_start();
        if (line.starts_with("DATABASE_URL=") || line.starts_with("DB_URL=")) && line.contains("postgres") {
            return true;
        }
    }
    false
}

#[derive(Debug, Clone)]
pub struct DetectedService {
    pub name: String,
    pub path: String,
    pub runtimes: Vec<String>,
}

/// Recursively scan a directory for services, stopping at project markers.
pub fn scan_for_services(scan_dir: &Path, label: &str, rel_path: &str) -> Vec<DetectedService> {
    if let Some((_, runtimes)) = detect_service_type(scan_dir) {
        let name = if rel_path.is_empty() {
            label.to_string()
        } else {
            format!("{label}-{}", rel_path.replace('/', "-"))
        };
        let path = if rel_path.is_empty() {
            label.to_string()
        } else {
            format!("{label}/{rel_path}")
        };
        let mut all_runtimes = runtimes;
        all_runtimes.extend(detect_db_runtimes(scan_dir));
        return vec![DetectedService { name, path, runtimes: all_runtimes }];
    }

    let entries = util::sorted_dir_entries(scan_dir);
    let mut services = Vec::new();
    for entry in entries {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if IGNORED_DIR_NAMES.contains(&name.as_str()) {
            continue;
        }
        let next_rel = if rel_path.is_empty() {
            name.clone()
        } else {
            format!("{rel_path}/{name}")
        };
        services.extend(scan_for_services(&path, label, &next_rel));
    }
    services
}

pub fn discover_estate_services(estate_root: &Path) -> Vec<DetectedService> {
    let entries = util::sorted_dir_entries(estate_root);
    let mut services = Vec::new();
    for entry in entries {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if IGNORED_DIR_NAMES.contains(&name.as_str()) {
            continue;
        }
        services.extend(scan_for_services(&path, &name, ""));
    }
    services
}

pub fn discover_services_at(label: &str, dir_path: &Path) -> Vec<DetectedService> {
    scan_for_services(dir_path, label, "")
}

pub fn render_service_block(service: &DetectedService) -> String {
    let mut lines = vec![format!("  {}:", service.name), format!("    path: {}", service.path), "    runtimes:".to_string()];
    for runtime in &service.runtimes {
        lines.push(format!("      - {runtime}"));
    }
    lines.join("\n")
}

pub fn build_ecompose_content(project_name: &str, ct_id: u64, hostname: &str, services: &[DetectedService]) -> String {
    let mut lines = vec![
        format!("project: {project_name}"),
        String::new(),
        "ct:".to_string(),
        format!("  id: {ct_id}"),
        format!("  hostname: {hostname}"),
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
        "shared_tools:".to_string(),
    ];
    for tool in DEFAULT_SHARED_TOOLS {
        lines.push(format!("  - {tool}"));
    }
    lines.push(String::new());
    lines.push("services:".to_string());
    for service in services {
        lines.push(render_service_block(service));
        lines.push(String::new());
    }
    format!("{}\n", lines.join("\n"))
}

pub fn run_adopt(args: &[String]) -> Result<(), String> {
    let cwd = util::current_dir();
    let input = args.first().cloned().unwrap_or_else(|| ".".to_string());
    let target_dir = cwd.join(&input);
    let estate_root = target_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| target_dir.clone());
    let ecompose_path = target_dir.join("ecompose.yml");

    if !path_exists(&target_dir) {
        return Err(format!("No such directory: {}", target_dir.display()));
    }
    if path_exists(&ecompose_path) {
        return Err(format!(
            "ecompose.yml already exists at {} -- edit it directly, or remove it first to regenerate.",
            ecompose_path.display()
        ));
    }

    let services = discover_estate_services(&estate_root);

    util::println_stdout("Adopting project into eco:");
    util::println_stdout(&format!("  manifest dir: {}", target_dir.display()));
    util::println_stdout(&format!("  estate root:  {}", estate_root.display()));
    util::println_stdout("");

    if services.is_empty() {
        util::println_stdout(
            "No services detected (looked for pom.xml/Cargo.toml/package.json in every top-level directory under the estate root).\nYou can still generate a manifest and add services to it by hand.\n",
        );
    } else {
        util::println_stdout("Detected services:");
        for service in &services {
            let runtimes = if service.runtimes.is_empty() {
                "(none)".to_string()
            } else {
                service.runtimes.join(", ")
            };
            util::println_stdout(&format!("  {} -- path: {}, runtimes: {}", service.name, service.path, runtimes));
        }
        util::println_stdout("");
    }

    let default_project_name = estate_root
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "project".to_string());
    let project_name_input = crate::checklist::prompt_line(&format!("Project name [{default_project_name}]: "))?;
    let project_name = if project_name_input.is_empty() {
        default_project_name.clone()
    } else {
        project_name_input
    };

    let ct_id_input = crate::checklist::prompt_line("Proxmox CT id (leave blank to fill in later): ")?;
    let ct_id: u64 = ct_id_input.trim().parse().unwrap_or(0);

    let hostname_input = crate::checklist::prompt_line(&format!("CT hostname [{project_name}]: "))?;
    let hostname = if hostname_input.is_empty() { project_name.clone() } else { hostname_input };

    let content = build_ecompose_content(&project_name, ct_id, &hostname, &services);

    util::println_stdout(&format!("\nProposed {}:\n\n{content}\n", ecompose_path.display()));
    util::println_stdout(
        "Note: expose/deploy blocks are intentionally left out -- add them by hand\n(see assessment/assessment_core/ecompose.yml or training/training_core/ecompose.yml\nfor examples) once this estate has a public hostname set up.\n",
    );

    let confirmation = crate::checklist::prompt_line(&format!(
        "Write this to {}? [y/N]: ",
        ecompose_path.display()
    ))?
    .to_lowercase();
    if confirmation != "y" && confirmation != "yes" {
        return Err("Cancelled.".to_string());
    }

    std::fs::write(&ecompose_path, content)
        .map_err(|e| format!("Cannot write {}: {e}", ecompose_path.display()))?;
    util::println_stdout(&format!("Wrote {}", ecompose_path.display()));
    util::println_stdout(&format!("Next: run \"eco configure\" from {}", target_dir.display()));
    Ok(())
}

#[allow(dead_code)]
fn _assert_parse(services: &[DetectedService]) {
    let _ = ecompose::parse_services;
}
