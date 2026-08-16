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
    /// The env var name this service reads its port from (SERVER_PORT / PORT).
    pub port_var: String,
    /// Database kind this service needs (postgres | mongo | redis | "").
    pub database: String,
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
///
/// `read_dir` is where shipped artifacts actually live (the HOST release dir,
/// which exists on this machine); `release_dir` is the CT release dir that
/// returned ExecStart/workdir paths must point at (may not exist locally).
/// Existence checks (dist vs .next vs bun) run against read_dir; returned
/// paths are anchored at release_dir.
pub fn resolve_service_exec(
    service: &ecompose::Service,
    read_dir: &str,
    release_dir: &str,
    binary_override: &str,
) -> Result<(String, String, bool), String> {
    let read_artifacts = format!("{read_dir}/artifacts");
    let artifacts = format!("{release_dir}/artifacts");
    // Static (Leptos-style / plain static) site: Cargo.toml + index.html,
    // served from the shipped dist/ by python http.server.
    if service.runtimes.iter().any(|r| r == "static") || (Path::new(&read_dir).join("artifacts").join(&service.name).join("dist").join("index.html").is_file()) {
        let service_dir = format!("{artifacts}/{}", service.name);
        let dist = format!("{service_dir}/dist");
        let port_var = format!("${{{}}}", env_var_for(service, "PORT"));
        let exec = format!("python3 -m http.server {port_var} --directory {dist} --bind 0.0.0.0");
        return Ok((exec, service_dir.clone(), true));
    }
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
        let bin_path = format!("{artifacts}/{}/bin/{bin}", service.name);
        // A source Rust frontend that ships static assets (static/, images/,
        // public/) serves them relative to its working dir. Point the unit's
        // WorkingDirectory at artifacts/<service> so ServeDir::new("static")
        // (and the like) resolves on the CT.
        let workdir = if Path::new(&read_artifacts).join(&service.name).join("static").is_dir()
            || Path::new(&read_artifacts).join(&service.name).join("public").is_dir()
        {
            format!("{artifacts}/{}", service.name)
        } else {
            release_dir.to_string()
        };
        return Ok((bin_path, workdir, true));
    }
    // Source Go service: prebuilt static binary shipped as
    // artifacts/<service>/bin/<name> (built on the dev machine for linux/amd64).
    if service.runtimes.iter().any(|r| r == "go") {
        let bin = if !service.binary.is_empty() {
            service.binary.clone()
        } else {
            service.name.clone()
        };
        let bin_path = format!("{artifacts}/{}/bin/{bin}", service.name);
        return Ok((bin_path, release_dir.to_string(), true));
    }
    // Spring Boot service: built jar shipped as artifacts/<service>/<name>.jar.
    if service.runtimes.iter().any(|r| r == "java@17" || r == "maven") {
        let jar = if !service.binary.is_empty() {
            service.binary.clone()
        } else {
            service.name.clone()
        };
        let jar_path = format!("{artifacts}/{}/{jar}.jar", service.name);
        let exec = format!("java -jar {}", jar_path);
        return Ok((exec, release_dir.to_string(), true));
    }
    // LXS-only service (no runtimes declared): a registry binary shipped into
    // artifacts/<service.name>/bin/<name>.
    if !service.lxs.is_empty() {
        let bin = service.lxs.split('@').next().unwrap_or("").to_string();
        let bin_path = format!("{artifacts}/{}/bin/{bin}", service.name);
        return Ok((bin_path, format!("{artifacts}/{}", service.name), true));
    }
    if service.runtimes.iter().any(|r| r == "npm" || r.starts_with("node@")) {
        // Frontend: served as a static dist via python http.server, or a
        // Bun-compiled single binary when a `.eco-bun` marker exists.
        let read_service_dir = format!("{read_artifacts}/{}", service.name);
        let service_dir = format!("{artifacts}/{}", service.name);
        let bun_marker = format!("{read_service_dir}/.eco-bun");
        if Path::new(&bun_marker).exists() {
            let bun_name = std::fs::read_to_string(format!("{read_service_dir}/.eco-bun-name"))
                .unwrap_or_else(|_| service.name.clone())
                .trim()
                .to_string();
            return Ok((format!("{service_dir}/{bun_name}"), service_dir.clone(), true));
        }
        // Next.js build output: serve via next start (needs node + node_modules
        // shipped to the CT). Static dist / Bun single-binary handled below.
        let next_dir = format!("{read_service_dir}/.next");
        if Path::new(&next_dir).is_dir() {
            let port_var = format!("${{{}}}", env_var_for(service, "PORT"));
            let exec = format!("{service_dir}/node_modules/.bin/next start -p {port_var}");
            return Ok((exec, service_dir.clone(), true));
        }
        // SvelteKit adapter-node build (build/index.js + build/client): served
        // via `node build/index.js`. The builder ships the build/ tree as-is
        // (bun-compiling embeds the server only and loses the client assets).
        let sveltekit_entry = format!("{read_service_dir}/build/index.js");
        if Path::new(&sveltekit_entry).is_file() {
            let exec = format!("node {service_dir}/build/index.js");
            return Ok((exec, service_dir.clone(), true));
        }
        // Astro SSR with @astrojs/node (standalone): dist/server/entry.mjs
        // (or app/dist/server/entry.mjs in a workspace layout).
        for astro_rel in ["dist/server/entry.mjs", "app/dist/server/entry.mjs"] {
            let astro_entry = format!("{read_service_dir}/{astro_rel}");
            if Path::new(&astro_entry).is_file() {
                let exec = format!("node {service_dir}/{astro_rel}");
                return Ok((exec, service_dir.clone(), true));
            }
        }
        // Nuxt (nitro) SSR: .output/server/index.mjs.
        let nuxt_entry = format!("{read_service_dir}/.output/server/index.mjs");
        if Path::new(&nuxt_entry).is_file() {
            let exec = format!("node {service_dir}/.output/server/index.mjs");
            return Ok((exec, service_dir.clone(), true));
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
    } else if service.runtimes.iter().any(|r| r == "go") {
        "PORT".to_string()
    } else if service.runtimes.iter().any(|r| r == "npm" || r.starts_with("node@")) {
        "PORT".to_string()
    } else if !service.lxs.is_empty() {
        // LXS from the registry: most are Rust/Go binaries that read SERVER_PORT.
        "SERVER_PORT".to_string()
    } else {
        default.to_string()
    }
}

/// Build systemd unit files for the estate. Returns Vec<(unit_filename, unit_text)>.
/// `env_paths` maps service name -> absolute .env file path on the CT.
pub fn build_systemd_units(
    project: &str,
    services: &[ServiceDeploy],
    env_paths: &HashMap<String, String>,
) -> Vec<(String, String)> {
    let mut units = Vec::new();
    for s in services {
        let unit_name = format!("eco-{project}-{}.service", s.name);
        let env_file = env_paths.get(&s.name).cloned().unwrap_or_default();
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
/// Every key from the shipped `.env.example` contract is written; the port var
/// (SERVER_PORT for rust, PORT for node/frontend) and JWT_SECRET are filled
/// when the contract does not provide them.
pub fn build_env_files(
    deploys: &[ServiceDeploy],
    project: &str,
    ports: &HashMap<String, u16>,
    registry_values: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let scope = std::env::var("ECO_REGISTRY_SCOPE").unwrap_or_else(|_| hostname());
    for s in deploys {
        let port = ports.get(&s.name).copied().unwrap_or(0);
        let mut env = String::new();
        for line in s.env_example.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                let k = k.trim();
                let v = v.trim().trim_matches('"').to_string();
                if k == s.port_var {
                    env.push_str(&format!("{k}={port}\n"));
                } else if let Some(rv) = registry_values.get(k) {
                    env.push_str(&format!("{k}={rv}\n"));
                } else if let Some(mv) = managed_env_value(&k, &s.name, project) {
                    env.push_str(&format!("{k}={mv}\n"));
                } else if v.is_empty() && k.starts_with("S3_") {
                    // Grant-provided S3 secrets: leave a resolvable placeholder
                    // so the service can start; real values come from grants.
                    env.push_str(&format!("{k}=\n"));
                } else {
                    env.push_str(&format!("{k}={v}\n"));
                }
            }
        }
        // Guarantee the port var is present.
        if !env.contains(&format!("{}=", s.port_var)) {
            env.push_str(&format!("{}={port}\n", s.port_var));
        }
        // Shared JWT. The contract may ship `JWT_SECRET=` empty (the service
        // only validates, the value comes from the estate) — treat an empty
        // value as missing so the shared secret is injected, not a blank.
        let jwt_present = env
            .lines()
            .any(|l| l.strip_prefix("JWT_SECRET=").map(|v| !v.trim().is_empty()).unwrap_or(false));
        if !jwt_present {
            let jwt = configgen_shared_jwt(&scope, project);
            let mut lines: Vec<String> = env
                .lines()
                .filter(|l| !l.trim().starts_with("JWT_SECRET="))
                .map(|l| l.to_string())
                .collect();
            lines.push(format!("JWT_SECRET={jwt}\n"));
            env = lines.join("\n");
        }
        // Guarantee the managed DB URL the service's runtime requires is
        // present even when the shipped contract omits it.
        if s.database == "postgres" && !env.contains("DATABASE_URL=") {
            if let Some(mv) = managed_env_value("DATABASE_URL", &s.name, project) {
                env.push_str(&format!("DATABASE_URL={mv}\n"));
            }
        }
        if s.database == "mongo" && !env.contains("MONGODB_URI=") {
            if let Some(mv) = managed_env_value("MONGODB_URI", &s.name, project) {
                env.push_str(&format!("MONGODB_URI={mv}\n"));
            }
        }
        if s.database == "redis" && !env.contains("REDIS_URL=") {
            if let Some(mv) = managed_env_value("REDIS_URL", &s.name, project) {
                env.push_str(&format!("REDIS_URL={mv}\n"));
            }
        }
        out.insert(s.name.clone(), env);
    }
    out
}

fn hostname() -> String {
    std::env::var("HOSTNAME").unwrap_or_else(|_| "ecosphere".to_string())
}

/// Managed env values Eco derives itself (mirrors configure.sh's managed
/// providers): the estate-local MongoDB/Postgres/Redis URIs. The DB name for a
/// service follows the historical convention `<service>_<project>`.
fn managed_env_value(key: &str, service: &str, project: &str) -> Option<String> {
    match key {
        "MONGODB_URI" => Some(format!("mongodb://localhost:27017/{service}_{project}")),
        "REDIS_URL" => Some("redis://127.0.0.1:6379".to_string()),
        // Hostname-only default (no password); data bootstrap overwrites this
        // with the real role password. db name convention: <service>_<project>.
        "DATABASE_URL" | "POSTGRES_URL" => Some(format!(
            "postgresql://{project}_user@127.0.0.1:5432/{service}_{project}"
        )),
        _ => None,
    }
}

/// Provisional shared-JWT derivation until the registry secret is plumbed.
fn configgen_shared_jwt(_scope: &str, _project: &str) -> String {
    let mut b = [0u8; 32];
    use rand::Rng;
    rand::thread_rng().fill(&mut b);
    crate::registry::hex_encode(&b)
}

/// Build the estate Caddyfile (gateway) for one public hostname.
pub fn build_caddyfile(
    hostname: &str,
    gateway_port: u16,
    frontend_port: u16,
    auth_port: Option<u16>,
    api_service_port: Option<(String, u16)>,
    auth_ui_port: Option<u16>,
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
    if let Some(up) = auth_ui_port {
        caddy.push_str("\t\thandle /signin {\n");
        caddy.push_str(&format!("\t\t\treverse_proxy 127.0.0.1:{up}\n\t\t}}\n"));
        caddy.push_str("\t\thandle /signup {\n");
        caddy.push_str(&format!("\t\t\treverse_proxy 127.0.0.1:{up}\n\t\t}}\n"));
        caddy.push_str("\t\thandle /static/* {\n");
        caddy.push_str(&format!("\t\t\treverse_proxy 127.0.0.1:{up}\n\t\t}}\n"));
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

/// Materialize config files, ready to push into the CT.
/// `read_dir` is where artifacts + .env.example live (the host release dir);
/// `write_dir` is where .env files + systemd ExecStart paths must point (the
/// CT release dir). Returns (files, unit_files) — files as relative path ->
/// content, units as (unit_filename, unit_text).
pub fn generate_all(
    project: &str,
    read_dir: &str,
    write_dir: &str,
    services: &[ecompose::Service],
    expose_hostname: &str,
) -> Result<(Vec<(String, String)>, Vec<(String, String)>), String> {
    generate_all_auth(project, read_dir, write_dir, services, expose_hostname, None)
}

/// `generate_all` with an optional `auth.email_verification.enabled` value from
/// ecompose.yml (mirrors configure.sh's ecompose_nested_value lookup). When the
/// estate explicitly disables email verification, the value is written into the
/// auth service's .env so the LXS binary honors it without operator secrets.
pub fn generate_all_auth(
    project: &str,
    read_dir: &str,
    write_dir: &str,
    services: &[ecompose::Service],
    expose_hostname: &str,
    auth_email_verification_enabled: Option<&str>,
) -> Result<(Vec<(String, String)>, Vec<(String, String)>), String> {
    let mut deploys: Vec<ServiceDeploy> = Vec::new();
    let mut ports: HashMap<String, u16> = HashMap::new();
    for svc in services {
        let (exec, workdir, http) = resolve_service_exec(svc, read_dir, write_dir, "")?;
        if exec.is_empty() {
            continue;
        }
        let env_example = read_env_example(read_dir, svc);
        let port = if http {
            allocate_port(project, &svc.name, "service")?
        } else {
            0
        };
        ports.insert(svc.name.clone(), port);
        let port_var = env_var_for(svc, "PORT");
        let database = if svc.runtimes.iter().any(|r| r == "postgresql@15") {
            "postgres".to_string()
        } else if svc.runtimes.iter().any(|r| r.starts_with("mongodb")) {
            "mongo".to_string()
        } else if svc.runtimes.iter().any(|r| r.starts_with("redis")) {
            "redis".to_string()
        } else {
            String::new()
        };
        deploys.push(ServiceDeploy {
            name: svc.name.clone(),
            exec,
            workdir,
            env_example,
            http,
            port,
            port_var,
            database,
        });
    }
    let mut env_files = build_env_files(&deploys, project, &ports, &HashMap::new());
    // auth.email_verification.enabled=false from ecompose.yml: write it into
    // the auth service's env so the LXS binary skips email verification
    // (the default is required=true). Mirrors configure.sh's behavior.
    if let Some(enabled) = auth_email_verification_enabled {
        if let Some(auth_name) = deploys.iter().find(|d| d.name.contains("auth")).map(|d| d.name.clone()) {
            if let Some(env) = env_files.get_mut(&auth_name) {
                let v = enabled.trim();
                if !v.is_empty() {
                    let mut lines: Vec<String> = env
                        .lines()
                        .filter(|l| !l.trim_start().starts_with("EMAIL_VERIFICATION_REQUIRED="))
                        .map(|l| l.to_string())
                        .collect();
                    lines.push(format!("EMAIL_VERIFICATION_REQUIRED={v}"));
                    env_files.insert(auth_name.clone(), lines.join("\n"));
                }
            }
        }
    }
    // systemd EnvironmentFile must point at the .env file path on the CT.
    let mut env_paths: HashMap<String, String> = HashMap::new();
    for name in env_files.keys() {
        env_paths.insert(name.clone(), format!("{write_dir}/.env/{name}.env"));
    }
    let mut units = build_systemd_units(project, &deploys, &env_paths);
    // .env files: relative path -> content
    let mut files: Vec<(String, String)> = Vec::new();
    for (name, env) in &env_files {
        files.push((format!(".env/{name}.env"), env.clone()));
    }
    // Caddyfile — gateway routes by role, detected flexibly (not just the
    // literal names `frontend`/`auth`/`backend`):
    //   frontend: the HTTP service named `<project>-frontend` or `frontend`;
    //             fall back to the estate's primary source HTTP service (any
    //             service that isn't auth/LXS), so a user estate whose app
    //             service is named after the project (e.g. proof-spring-boot)
    //             still gets a working gateway.
    //   auth:     the service whose name contains `auth`
    //   api:      the source HTTP service (rust/npm) that is not the frontend
    //             and not auth — i.e. the estate's own API/backend.
    let gateway_port = if expose_hostname.is_empty() { 0 } else { allocate_port(project, "gateway", "gateway")? };
    let frontend_port = deploys
        .iter()
        .filter(|d| d.http)
        .find(|d| d.name == "frontend" || d.name == format!("{project}-frontend"))
        .map(|d| d.port)
        .or_else(|| {
            deploys
                .iter()
                .filter(|d| d.http)
                .find(|d| !d.name.contains("auth") && !d.name.contains("auth-ui") && !d.name.contains("auth_ui"))
                .map(|d| d.port)
        })
        .unwrap_or(0);
    if gateway_port > 0 && frontend_port > 0 {
        let auth_port = deploys.iter().filter(|d| d.http).find(|d| d.name.contains("auth")).map(|d| d.port);
        let api_port = deploys
            .iter()
            .filter(|d| d.http)
            .find(|d| {
                d.port != frontend_port
                    && d.name != "frontend"
                    && d.name != format!("{project}-frontend")
                    && !d.name.contains("auth")
            })
            .map(|d| (d.name.clone(), d.port));
        // auth-ui LXS (signin/signup pages) routes /signin, /signup, /static.
        let auth_ui_port = deploys
            .iter()
            .filter(|d| d.http)
            .find(|d| d.name.contains("auth-ui") || d.name.contains("auth_ui"))
            .map(|d| d.port);
        let caddy = build_caddyfile(expose_hostname, gateway_port, frontend_port, auth_port, api_port, auth_ui_port);
        files.push(("Caddyfile".to_string(), caddy));
        // Record the gateway port so host-side exposure can find it.
        files.push((".env/gateway.env".to_string(), format!("PORT={gateway_port}\n")));
        // systemd unit for the estate gateway (Caddy serving the Caddyfile).
        let caddyfile_path = format!("{write_dir}/Caddyfile");
        let unit = format!(
            "[Unit]\nDescription={project} gateway\nAfter=network.target\nStartLimitIntervalSec=0\n\n[Service]\nType=simple\nWorkingDirectory={write_dir}\nExecStart=/usr/bin/caddy run --config {caddyfile_path} --adapter caddyfile\nRestart=always\nRestartSec=2\nKillSignal=SIGTERM\n\n[Install]\nWantedBy=multi-user.target\n"
        );
        units.push((format!("eco-{project}-gateway.service"), unit));
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
