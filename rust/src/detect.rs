use crate::util;
use std::path::Path;

pub const IGNORED_DIR_NAMES: &[&str] = &[
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

#[derive(Debug, Clone)]
pub struct DetectedService {
    pub name: String,
    pub path: String,
    pub runtimes: Vec<String>,
}

fn path_exists(p: &Path) -> bool {
    p.exists()
}

fn read_text_file(p: &Path) -> Option<String> {
    std::fs::read_to_string(p).ok()
}

/// Detect the service type + runtimes for a single project directory.
/// Types mirror configure.sh's discover_services:
///   spring-boot | rust | static (Leptos) | go | nextjs | astro | vite | nuxt | node
pub fn detect_service_type(dir: &Path) -> Option<(String, Vec<String>)> {
    if dir.join("pom.xml").is_file() {
        return Some(("spring-boot".to_string(), vec!["java@17".to_string(), "maven".to_string()]));
    }
    if dir.join("Cargo.toml").is_file() {
        if dir.join("index.html").is_file() {
            // Leptos/Rust frontend: built static dist is served as a static site.
            return Some(("static".to_string(), vec!["static".to_string()]));
        }
        return Some(("rust".to_string(), vec!["rust".to_string()]));
    }
    if dir.join("go.mod").is_file() {
        return Some(("go".to_string(), vec!["go".to_string()]));
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
        let node_runtime = vec!["node@20".to_string(), "npm".to_string(), "pm2".to_string()];
        if deps.contains_key("next") {
            return Some(("nextjs".to_string(), node_runtime));
        }
        if deps.contains_key("astro") {
            return Some(("astro".to_string(), node_runtime));
        }
        if deps.contains_key("vite") {
            return Some(("vite".to_string(), node_runtime));
        }
        if deps.contains_key("nuxt") {
            return Some(("nuxt".to_string(), node_runtime));
        }
        return Some(("node".to_string(), node_runtime));
    }
    None
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

/// Discover every service under an estate root (each top-level dir is scanned
/// with its dir name as the label).
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

/// Scan a single directory (and its children) as one labeled service tree.
pub fn discover_services_at(label: &str, dir_path: &Path) -> Vec<DetectedService> {
    scan_for_services(dir_path, label, "")
}

/// The first service (if any) directly inside `dir` — used by `eco init` so
/// a freshly created app in a folder is picked up as a single service.
pub fn detect_first_service(dir: &Path, label: &str) -> Option<DetectedService> {
    if detect_service_type(dir).is_some() {
        // The project root itself is a service: path "." (relative to the
        // estate root = this dir), named after the project.
        let mut runtimes = detect_service_type(dir).map(|(_, r)| r).unwrap_or_default();
        runtimes.extend(detect_db_runtimes(dir));
        return Some(DetectedService {
            name: label.to_string(),
            path: ".".to_string(),
            runtimes,
        });
    }
    let entries = util::sorted_dir_entries(dir);
    for entry in entries {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if IGNORED_DIR_NAMES.contains(&name.as_str()) {
            continue;
        }
        let found = scan_for_services(&path, label, "");
        if let Some(first) = found.into_iter().next() {
            return Some(first);
        }
    }
    None
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
        "  template: local:vztmpl/eco-npm-rust-mongo_1_amd64.tar.zst".to_string(),
        "  storage: local-lvm".to_string(),
        "  disk: 16".to_string(),
        "  bridge: vmbr0".to_string(),
        "  ip: dhcp".to_string(),
        "  cores: 2".to_string(),
        "  memory: 4096".to_string(),
        "  swap: 1024".to_string(),
        "  unprivileged: 1".to_string(),
        String::new(),
        "services:".to_string(),
    ];
    for service in services {
        lines.push(render_service_block(service));
        lines.push(String::new());
    }
    format!("{}\n", lines.join("\n"))
}

// Keep the signature referenced by callers that import `path_exists` from here.
#[allow(dead_code)]
fn _path_exists(p: &Path) -> bool {
    path_exists(p)
}
