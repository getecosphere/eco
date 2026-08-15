// Host-side estate configuration generator.
//
// The CT is a pure runtime: no source, no node, no configure.sh, no PM2. This
// module reads the manifest + the shipped artifacts + the resource registry on
// the HOST and produces the finished config files (systemd units, .env,
// Caddyfile) that get pushed into the CT's release directory. systemd
// ExecStart always points at `current/...` so rollback is re-pointing the
// `current` symlink and restarting units.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::ecompose;
use crate::registry;
use crate::util;

/// A resolved service deployment: what to run, where, and its env contract.
#[derive(Debug, Clone)]
pub struct ServiceDeploy {
    pub name: String,
    /// Absolute path to the executable (binary, bun binary, or serve command).
    pub exec: String,
    /// Working directory for the unit.
    pub workdir: String,
    /// The service's `.env.example` text (env contract), if shipped.
    pub env_example: String,
    /// Whether this service exposes HTTP (frontend/backend) — used by Caddy.
    pub http: bool,
    /// Primary HTTP port (allocated by the registry host-side).
    pub port: u16,
}

/// Resolve the executable for a service inside a CT release dir.
///
/// Release layout on the CT:
///   /opt/eco/<estate>/releases/r<ts>/
///     artifacts/
///       <binary-name>          # rust binary (flat)
///       <service-name>/dist/   # frontend dist
///       <service-name>/.env.example
///       <service-name>/migrations/
///
/// `binary:` in the manifest names the artifact for source Rust services.
pub fn resolve_service_exec(
    service: &ecompose::Service,
    release_dir: &str,
    binary_override: &str,
) -> Result<(String, String, bool), String> {
    let artifacts = format!("{release_dir}/artifacts");
    // Source Rust service: binary named `binary:` (or service/lxs name).
    if service.runtimes.iter().any(|r| r == "rust") {
        let bin = if !binary_override.is_empty() {
            binary_override.to_string()
        } else if !service.binary.is_empty() {
            service.binary.clone()
        } else if !service.lxs.is_empty() {
            service.lxs.split('@').next().unwrap_or("").to_string()
        } else {
            service.name.clone()
        };
        let bin_path = format!("{artifacts}/{bin}");
        return Ok((bin_path, release_dir.to_string(), true));
    }
    // LXS-only service (no runtimes declared): a registry binary named by the
    // lxs package name, shipped into artifacts/<name>.
    if !service.lxs.is_empty() {
        let bin = service.lxs.split('@').next().unwrap_or("").to_string();
        let bin_path = format!("{artifacts}/{bin}");
        return Ok((bin_path, release_dir.to_string(), true));
    }
    if service.runtimes.iter().any(|r| r == "npm" || r.starts_with("node@")) {
        // Frontend: served as a static dist via python http.server, or a
        // Bun-compiled single binary when a `.eco-bun` marker exists.
        let service_dir = format!("{artifacts}/{}", service.name);
        let bun_marker = format!("{service_dir}/.eco-bun");
        if Path::new(&bun_marker).exists() {
            let bun_name = std::fs::read_to_string(format!("{service_dir}/.eco-bun-name"))
                .unwrap_or_else(|_| service.name.clone())
                .trim()
                .to_string();
            return Ok((format!("{service_dir}/{bun_name}"), service_dir.clone(), true));
        }
        // Static dist served by python3 http.server on the allocated port.
        let dist = format!("{service_dir}/dist");
        let port_var = format!("${{{}}}", env_var_for(service, "PORT"));
        let exec = format!("python3 -m http.server {port_var} --directory {dist} --bind 0.0.0.0");
        return Ok((exec, service_dir.clone(), true));
    }
    Ok((String::new(), release_dir.to_string(), false))
}

/// The conventional env var name a service reads its port from.
pub fn env_var_for(service: &ecompose::Service, default: &str) -> String {
    if service.runtimes.iter().any(|r| r == "rust") {
        "SERVER_PORT".to_string()
    } else if service.runtimes.iter().any(|r| r == "npm" || r.starts_with("node@")) {
        "PORT".to_string()
    } else {
        default.to_string()
    }
}

/// Build systemd unit files for the estate. Returns Vec<(unit_filename, unit_text)>.
/// `units_dir` is the target dir on the CT (/etc/systemd/system) — content is
/// written to the host staging dir and pushed.
pub fn build_systemd_units(
    project: &str,
    services: &[ServiceDeploy],
    env_files: &HashMap<String, String>, // service -> .env path on the CT
) -> Vec<(String, String)> {
    let mut units = Vec::new();
    for s in services {
        let unit_name = format!("eco-{project}-{}.service", s.name);
        let env_file = env_files.get(&s.name).cloned().unwrap_or_default();
        let mut unit = String::from("[Unit]\n");
        unit.push_str(&format!("Description={project} {}\n", s.name));
        unit.push_str("After=network.target\n");
        unit.push_str("StartLimitIntervalSec=0\n\n");
        unit.push_str("[Service]\n");
        unit.push_str("Type=simple\n");
        unit.push_str(&format!("WorkingDirectory={}\n", s.workdir));
        if !env_file.is_empty() {
            unit.push_str(&format!("EnvironmentFile={env_file}\n"));
        }
        unit.push_str(&format!("ExecStart={}\n", s.exec));
        unit.push_str("Restart=always\n");
        unit.push_str("RestartSec=2\n");
        unit.push_str("KillSignal=SIGTERM\n\n");
        unit.push_str("[Install]\nWantedBy=multi-user.target\n");
        units.push((unit_name, unit));
    }
    units
}

/// Build the `.env` for each service from its shipped `.env.example` contract.
/// Returns service name -> env file text (host-generated values merged).
pub fn build_env_files(
    services: &[ServiceDeploy],
    project: &str,
    ports: &HashMap<String, u16>,
    registry_values: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let scope = std::env::var("ECO_REGISTRY_SCOPE").unwrap_or_else(|_| hostname());
    for s in services {
        let port = ports.get(&s.name).copied().unwrap_or(0);
        let mut env = String::new();
        // Start from the shipped contract.
        for line in s.env_example.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                let k = k.trim();
                let v = v.trim().trim_matches('"').to_string();
                // Fill from registry / port where known.
                let port_var = env_var_for_by_name(&s.name, port, "PORT");
                if k == port_var {
                    env.push_str(&format!("{k}={port}\n"));
                } else if let Some(rv) = registry_values.get(k) {
                    env.push_str(&format!("{k}={rv}\n"));
                } else {
                    env.push_str(&format!("{k}={v}\n"));
                }
            }
        }
        // Guarantee the port var is present.
        let port_var = env_var_for_by_name(&s.name, port, "PORT");
        if !env.contains(&format!("{port_var}=")) {
            env.push_str(&format!("{port_var}={port}\n"));
        }
        // Shared JWT + scope marker.
        if !env.contains("JWT_SECRET=") {
            let jwt = configgen_shared_jwt(&scope, project);
            env.push_str(&format!("JWT_SECRET={jwt}\n"));
        }
        out.insert(s.name.clone(), env);
    }
    out
}

fn hostname() -> String {
    std::env::var("HOSTNAME").unwrap_or_else(|_| "ecosphere".to_string())
}

/// Provisional shared-JWT derivation until the registry secret is plumbed.
fn configgen_shared_jwt(_scope: &str, _project: &str) -> String {
    let mut b = [0u8; 32];
    use rand::Rng;
    rand::thread_rng().fill(&mut b);
    crate::registry::hex_encode(&b)
}

fn env_var_for_by_name(_name: &str, _port: u16, default: &str) -> String {
    default.to_string()
}/// Build the estate Caddyfile (gateway) for one public hostname.
pub fn build_caddyfile(
    hostname: &str,
    gateway_port: u16,
    frontend_port: u16,
    auth_port: Option<u16>,
    api_service_port: Option<(String, u16)>,
) -> String {
    let mut caddy = format!(
        "{{\n\tadmin off\n}}\n\n:{gateway_port} {{\n"
    );
    caddy.push_str("\t@plain_http header X-Forwarded-Proto http\n");
    caddy.push_str("\tredir @plain_http https://{host}{uri} 302\n");
    if let Some(ap) = auth_port {
        caddy.push_str("\t\thandle /auth-api/* {\n\t\t\turi replace /auth-api /api\n");
        caddy.push_str(&format!("\t\t\treverse_proxy 127.0.0.1:{ap}\n\t\t}}\n"));
        caddy.push_str("\t\thandle /api/auth/* {\n");
        caddy.push_str(&format!("\t\t\treverse_proxy 127.0.0.1:{ap}\n\t\t}}\n"));
    }
    if let Some((name, port)) = api_service_port {
        caddy.push_str(&format!("\t\thandle /api/{name}/* {{\n"));
        caddy.push_str(&format!("\t\t\treverse_proxy 127.0.0.1:{port}\n\t\t}}\n"));
    }
    caddy.push_str("\t\thandle {\n");
    caddy.push_str(&format!("\t\t\treverse_proxy 127.0.0.1:{frontend_port}\n"));
    caddy.push_str("\t}\n}\n");
    // hostname is used by the reverse-proxy layer / exposure metadata.
    let _ = hostname;
    caddy
}

/// Allocate a port for a service from the host registry (single writer).
pub fn allocate_port(project: &str, service: &str, type_kind: &str) -> Result<u16, String> {
    let scope = std::env::var("ECO_REGISTRY_SCOPE").unwrap_or_else(|_| hostname());
    let path = registry::default_registry_path();
    registry::get_or_allocate_port(&path, &scope, project, service, type_kind, "PORT", None)
        .map(|r| r.port as u16)
}

/// Read an existing allocated port (no allocation).
pub fn lookup_port(project: &str, service: &str, type_kind: &str) -> Result<u16, String> {
    let scope = std::env::var("ECO_REGISTRY_SCOPE").unwrap_or_else(|_| hostname());
    let path = registry::default_registry_path();
    registry::lookup_port(&path, &scope, project, service, type_kind)
        .map(|p| p.unwrap_or(0) as u16)
}

/// Materialize config files into a staging dir on the host, ready to push.
/// Returns (files, unit_files) as relative path -> content.
pub fn generate_all(
    project: &str,
    release_dir: &str,
    services: &[ecompose::Service],
    expose_hostname: &str,
) -> Result<(Vec<(String, String)>, Vec<(String, String)>), String> {
    let mut deploys: Vec<ServiceDeploy> = Vec::new();
    let mut ports: HashMap<String, u16> = HashMap::new();
    for svc in services {
        let (exec, workdir, http) = resolve_service_exec(svc, release_dir, "")?;
        if exec.is_empty() {
            continue;
        }
        let env_example = read_env_example(release_dir, svc);
        let port = if http {
            allocate_port(project, &svc.name, "service")?
        } else {
            0
        };
        ports.insert(svc.name.clone(), port);
        deploys.push(ServiceDeploy {
            name: svc.name.clone(),
            exec,
            workdir,
            env_example,
            http,
            port,
        });
    }
    let env_files = build_env_files(&deploys, project, &ports, &HashMap::new());
    let units = build_systemd_units(project, &deploys, &env_files);
    // .env files: relative path -> content
    let mut files: Vec<(String, String)> = Vec::new();
    for (name, env) in &env_files {
        files.push((format!(".env/{name}.env"), env.clone()));
    }
    // Caddyfile
    let gateway_port = if expose_hostname.is_empty() { 0 } else { allocate_port(project, "gateway", "gateway")? };
    let frontend_port = ports.get("frontend").copied().unwrap_or(0);
    if gateway_port > 0 && frontend_port > 0 {
        let auth_port = ports.get("auth").copied();
        let api_port = ports.iter().find(|(k, _)| k.as_str() == "backend").map(|(k, v)| (k.clone(), *v));
        let caddy = build_caddyfile(expose_hostname, gateway_port, frontend_port, auth_port, api_port);
        files.push(("Caddyfile".to_string(), caddy));
    }
    Ok((files, units))
}

fn read_env_example(release_dir: &str, service: &ecompose::Service) -> String {
    let p = PathBuf::from(release_dir).join("artifacts").join(&service.name).join(".env.example");
    std::fs::read_to_string(&p).unwrap_or_default()
}

// Keep `Path` import used.
#[allow(dead_code)]
fn _path_use(_: &Path) {}
