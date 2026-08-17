use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::util;

pub fn find_workspace_root(start_dir: &Path) -> Result<PathBuf, String> {
    let mut current = std::fs::canonicalize(start_dir).unwrap_or_else(|_| start_dir.to_path_buf());
    loop {
        let eco_dir = current.join("eco");
        let core_dir = current.join("core");
        if eco_dir.exists() || core_dir.exists() {
            return Ok(current);
        }
        let parent = match current.parent() {
            Some(p) => p.to_path_buf(),
            None => break,
        };
        if parent == current {
            break;
        }
        current = parent;
    }
    Err("Could not find a workspace root containing eco/ or core/. Run this command from inside a SuperApp workspace.".to_string())
}

pub fn find_estate_root(start_dir: &Path) -> Result<PathBuf, String> {
    let workspace_root = find_workspace_root(start_dir)?;
    let resolved_start = std::fs::canonicalize(start_dir).unwrap_or_else(|_| start_dir.to_path_buf());
    let relative = resolved_start.strip_prefix(&workspace_root).unwrap_or(&resolved_start);
    if relative.as_os_str().is_empty() {
        return Ok(workspace_root);
    }
    let first_segment = relative.components().next();
    if let Some(segment) = first_segment {
        let segment_str = segment.as_os_str().to_string_lossy().to_string();
        if segment_str == "." || segment_str == "eco" || segment_str == "core" {
            return Ok(workspace_root);
        }
        return Ok(workspace_root.join(segment_str));
    }
    Ok(workspace_root)
}

pub fn path_exists(p: &Path) -> bool {
    p.exists()
}

pub fn read_text_file(p: &Path) -> Option<String> {
    std::fs::read_to_string(p).ok()
}

/// Resolve a project/ecompose input to the absolute path of its ecompose.yml.
/// Mirrors lib/ecompose.js resolveEcomposeFile.
fn is_manifest_filename(name: &str) -> bool {
    name.ends_with("ecompose.yml") || name.ends_with("ecompose.yaml") || name.ends_with("-ecompose.yml")
}

pub fn resolve_ecompose_file(input: &str, start_dir: &Path) -> Result<PathBuf, String> {
    if input.is_empty() {
        return Err("Missing project or ecompose.yml path.".to_string());
    }
    let absolute_input = std::fs::canonicalize(start_dir.join(input)).unwrap_or_else(|_| start_dir.join(input));
    // Direct manifest file path (e.g. `eco up /path/to/stuff8-<unique>-ecompose.yml`).
    if is_manifest_filename(&absolute_input.to_string_lossy()) {
        if absolute_input.is_file() {
            return Ok(absolute_input);
        }
        // PaaS layout: a bare unique-named manifest copied into the host's
        // manifest directory (ECO_PROJECTS_ROOT), so `eco up <name>-ecompose.yml`
        // works without the full repo being cloned on the host.
        let projects_root = util::env_var_or("ECO_PROJECTS_ROOT", &format!("{}/projects", util::home_dir()));
        let flat_manifest = Path::new(&projects_root).join(input);
        if flat_manifest.is_file() {
            return Ok(flat_manifest);
        }
        return Err(format!(
            "Manifest not found: {} (also looked in {})",
            absolute_input.display(),
            flat_manifest.display()
        ));
    }
    if absolute_input.is_dir() {
        let direct = absolute_input.join("ecompose.yml");
        if direct.is_file() {
            return Ok(direct);
        }
        let mut nested_matches: Vec<PathBuf> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&absolute_input) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    let candidate = entry.path().join("ecompose.yml");
                    if candidate.is_file() {
                        nested_matches.push(candidate);
                    }
                }
            }
        }
        if nested_matches.len() == 1 {
            return Ok(nested_matches.remove(0));
        }
        return Ok(direct);
    }
    if absolute_input.is_absolute() {
        return Ok(absolute_input.join("ecompose.yml"));
    }
    let projects_root = util::env_var_or("ECO_PROJECTS_ROOT", &format!("{}/projects", util::home_dir()));
    let host_project = Path::new(&projects_root).join(input).join("ecompose.yml");
    if host_project.is_file() {
        return Ok(host_project);
    }
    let workspace_root = find_workspace_root(start_dir)?;
    Ok(workspace_root.join(input).join("ecompose.yml"))
}

pub struct EcomposeRead {
    pub file_path: PathBuf,
    pub content: String,
}

pub fn read_ecompose(input: &str, start_dir: &Path) -> Result<EcomposeRead, String> {
    let file_path = resolve_ecompose_file(input, start_dir)?;
    let content = std::fs::read_to_string(&file_path)
        .map_err(|e| format!("Failed to read {}: {e}", file_path.display()))?;
    Ok(EcomposeRead { file_path, content })
}

fn clean_line(raw: &str) -> String {
    let line = raw.trim_end();
    if line.is_empty() || line.trim_start().starts_with('#') {
        return String::new();
    }
    line.to_string()
}

pub fn parse_project_name(content: &str) -> String {
    for raw in content.split('\n') {
        let line = raw.trim_end_matches('\r').trim_end();
        if line.is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("project:") {
            return util::strip_quotes(rest.trim());
        }
    }
    String::new()
}

/// The `main:` key — the default estate name, like `main()` in C++.
/// Falls back to the project name (or the first estate) when absent.
pub fn parse_main(content: &str) -> String {
    for raw in content.split('\n') {
        let line = raw.trim_end_matches('\r').trim_end();
        if line.is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("main:") {
            return util::strip_quotes(rest.trim());
        }
    }
    String::new()
}

/// One estate: a deployable composition of services selected by name.
#[derive(Debug, Clone, Default)]
pub struct Estate {
    pub name: String,
    pub hostname: String,
    pub ingress: String,
    pub cloudflare_account: String,
    pub ct_id: String,
    pub services: Vec<String>,
    /// Optional path (relative to the estate root, where ecompose.yml lives) to
    /// a custom HTML error-page template. configgen ships it next to
    /// gateway.json and points the gateway LXS at it; gateway-origin errors
    /// (404/401/403/502/503) then render through it instead of the built-in
    /// Ecosphere default page.
    pub error_page: String,
}

/// Parse `estates:` — a map of estate name → estate config. An estate selects
/// which services it runs from the shared `services:` pool. Top-level keys
/// that appear before/after are ignored.
pub fn parse_estates(content: &str) -> Vec<Estate> {
    let mut estates: Vec<Estate> = Vec::new();
    let mut in_estates = false;
    let mut current: Option<Estate> = None;
    let mut in_services = false;

    for raw in content.split('\n') {
        let line = raw.trim_end_matches('\r').trim_end();
        if line.is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if line == "estates:" {
            in_estates = true;
            continue;
        }
        if !in_estates {
            continue;
        }
        if starts_top_level_key(line) {
            break;
        }
        if let Some(name) = match_indented_key(line, 2) {
            if let Some(e) = current.take() {
                estates.push(e);
            }
            current = Some(Estate {
                name,
                ..Default::default()
            });
            in_services = false;
            continue;
        }
        if let Some(e) = current.as_mut() {
            if let Some((key, value)) = match_indented_key_value(line, 4) {
                match key.as_str() {
                    "hostname" => e.hostname = util::strip_quotes(value.trim()),
                    "ingress" => e.ingress = util::strip_quotes(value.trim()),
                    "cloudflare_account" => e.cloudflare_account = util::strip_quotes(value.trim()),
                    "ct" => e.ct_id = util::strip_quotes(value.trim()),
                    "error_page" => e.error_page = util::strip_quotes(value.trim()),
                    _ => {}
                }
                continue;
            }
            if line.trim_start() == "services:" {
                in_services = true;
                continue;
            }
            if in_services {
                let trimmed = line.trim_start();
                if let Some(rest) = trimmed.strip_prefix('-') {
                    let name = util::strip_quotes(rest.trim());
                    if !name.is_empty() && !e.services.contains(&name) {
                        e.services.push(name);
                    }
                    continue;
                }
            }
        }
    }
    if let Some(e) = current.take() {
        estates.push(e);
    }
    estates
}

#[derive(Debug, Clone, Default)]
pub struct Service {
    pub name: String,
    pub path: String,
    pub lxs: String,
    pub runtimes: Vec<String>,
    pub r#type: String,
    pub dir: String,
    pub binary: String,
    pub grants_secrets: Vec<String>,
    pub grants_network: Vec<String>,
    /// Route-protection declarations (`access.routes`): path + access level.
    /// Empty means the service declares nothing — the gateway denies its
    /// routes unless they are declared here (default-deny).
    pub access_routes: Vec<AccessRoute>,
}

#[derive(Debug, Clone, Default)]
pub struct AccessRoute {
    pub path: String,
    pub level: String,
    /// Optional path-prefix rewrite, emitted into the gateway route. When the
    /// request path starts with `strip`, it is replaced with `rewrite` before
    /// forwarding (mirrors Caddy's `uri replace` — e.g. `/auth-api/*` -> `/api/*`).
    pub strip: String,
    pub rewrite: String,
    /// Optional session-cookie name the gateway may fall back to for this route
    /// when no Bearer token is sent (estates that keep the JWT in an httpOnly
    /// cookie for page loads).
    pub cookie: String,
}

#[derive(Debug, Clone, Default)]
pub struct RoleDecl {
    pub name: String,
    /// access level: `public` | `auth` | `role:<name>`
    pub level: String,
    /// True for the role assigned to new accounts at registration.
    pub default: bool,
}

#[derive(Debug, Clone, Default)]
pub struct AuthConfig {
    pub email_verification_enabled: Option<String>,
    pub roles: Vec<RoleDecl>,
}

/// Parse the `auth:` block — the estate's identity model:
///
/// ```yaml
/// auth:
///   email_verification:
///     enabled: false
///   roles:
///     - name: public
///       level: public
///     - name: authenticated
///       level: auth
///       default: true
/// ```
///
/// Roles are declared here, never hardcoded in the auth binary. Eco writes
/// them into the auth LXS's .env (`ECO_AUTH_DEFAULT_ROLE`, `ECO_AUTH_ROLES`)
/// and into the gateway's ACL.
pub fn parse_auth(content: &str) -> AuthConfig {
    let mut cfg = AuthConfig::default();
    let mut in_auth = false;
    let mut in_email_verification = false;
    let mut in_roles = false;
    let mut current_role: Option<RoleDecl> = None;

    for raw in content.split('\n') {
        let line = raw.trim_end_matches('\r').trim_end();
        if line.is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if line == "auth:" {
            in_auth = true;
            continue;
        }
        if !in_auth {
            continue;
        }
        if starts_top_level_key(line) {
            break;
        }
        if line.trim_start() == "email_verification:" {
            in_email_verification = true;
            in_roles = false;
            continue;
        }
        if line.trim_start() == "roles:" {
            in_roles = true;
            in_email_verification = false;
            continue;
        }
        if in_email_verification {
            if let Some((key, value)) = match_indented_key_value(line, 4) {
                if key == "enabled" {
                    cfg.email_verification_enabled = Some(util::strip_quotes(value.trim()));
                }
            }
            continue;
        }
        if in_roles {
            // A role list item is `- name: public` (a mapping whose `name`
            // key names the role); a bare `- public` is also accepted.
            if let Some((key, value)) = match_dash_item(line, 4) {
                if key == "name" {
                    if let Some(r) = current_role.take() {
                        cfg.roles.push(r);
                    }
                    current_role = Some(RoleDecl {
                        name: util::strip_quotes(value.trim()),
                        level: String::new(),
                        default: false,
                    });
                }
                continue;
            }
            // Bare `- public` form.
            if let Some(name) = match_dash_bare(line, 4) {
                if let Some(r) = current_role.take() {
                    cfg.roles.push(r);
                }
                current_role = Some(RoleDecl { name, level: String::new(), default: false });
                continue;
            }
            if let Some(r) = current_role.as_mut() {
                if let Some((key, value)) = match_indented_key_value(line, 6) {
                    match key.as_str() {
                        "level" => r.level = util::strip_quotes(value.trim()),
                        "default" => r.default = util::to_bool(value.trim()),
                        _ => {}
                    }
                }
            }
            continue;
        }
    }
    if let Some(r) = current_role.take() {
        cfg.roles.push(r);
    }
    cfg
}

/// Matches a bare list item `- name` at the given indent.
fn match_dash_bare(line: &str, indent: usize) -> Option<String> {
    let prefix = " ".repeat(indent);
    let rest = line.strip_prefix(&prefix)?;
    let rest = rest.strip_prefix("- ")?;
    let key = rest.trim();
    if key.is_empty() || key.contains(' ') || key.contains(':') {
        return None;
    }
    Some(key.to_string())
}

pub fn parse_services(content: &str) -> Vec<Service> {
    let mut services: Vec<Service> = Vec::new();
    let mut in_services = false;
    let mut current: Option<Service> = None;
    let mut in_runtimes = false;
    let mut in_access = false;
    let mut in_access_routes = false;

    for raw in content.split('\n') {
        let line = raw.trim_end_matches('\r').trim_end();
        if line.is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if line == "services:" {
            in_services = true;
            continue;
        }
        if !in_services {
            continue;
        }
        if starts_top_level_key(line) {
            break;
        }
        if let Some(name) = match_indented_key(line, 2) {
            if let Some(c) = current.take() {
                services.push(c);
            }
            current = Some(Service {
                name,
                path: String::new(),
                lxs: String::new(),
                runtimes: Vec::new(),
                r#type: String::new(),
                dir: String::new(),
                binary: String::new(),
                grants_secrets: Vec::new(),
                grants_network: Vec::new(),
                access_routes: Vec::new(),
            });
            in_runtimes = false;
            in_access = false;
            in_access_routes = false;
            continue;
        }
        if let Some(c) = current.as_mut() {
            if line.trim_start() == "access:" {
                in_access = true;
                in_runtimes = false;
                in_access_routes = false;
                continue;
            }
            if in_access {
                if line.trim_start() == "routes:" {
                    in_access_routes = true;
                    continue;
                }
                if in_access_routes {
                    let trimmed = line.trim_start();
                    if let Some(rest) = trimmed.strip_prefix('-') {
                        // `- path: /x` then `  level: auth` (block form)
                        let item = rest.trim();
                        if let Some((key, value)) = item.split_once(':') {
                            if key.trim() == "path" {
                                c.access_routes.push(AccessRoute {
                                    path: util::strip_quotes(value.trim()),
                                    level: String::new(),
                                    strip: String::new(),
                                    rewrite: String::new(),
                                    cookie: String::new(),
                                });
                                continue;
                            }
                        }
                        continue;
                    }
                    // `path: /x` (indent 8) then `level: auth` / `strip:` / `rewrite:` (indent 8)
                    if let Some((key, value)) = match_indented_key_value(line, 8) {
                        match key.as_str() {
                            "path" => {
                                c.access_routes.push(AccessRoute {
                                    path: util::strip_quotes(value.trim()),
                                    level: String::new(),
                                    strip: String::new(),
                                    rewrite: String::new(),
                                    cookie: String::new(),
                                });
                            }
                            "level" => {
                                if let Some(last) = c.access_routes.last_mut() {
                                    last.level = util::strip_quotes(value.trim());
                                }
                            }
                            "strip" => {
                                if let Some(last) = c.access_routes.last_mut() {
                                    last.strip = util::strip_quotes(value.trim());
                                }
                            }
                            "rewrite" => {
                                if let Some(last) = c.access_routes.last_mut() {
                                    last.rewrite = util::strip_quotes(value.trim());
                                }
                            }
                            "cookie" => {
                                if let Some(last) = c.access_routes.last_mut() {
                                    last.cookie = util::strip_quotes(value.trim());
                                }
                            }
                            _ => {}
                        }
                        continue;
                    }
                    // A deeper non-route line exits the routes list.
                    in_access_routes = false;
                }
                // exit the access block when we hit a service-level key
                if line.trim_start().chars().next().is_none() || (line.starts_with(' ') && !line.starts_with("    ")) {
                    // keep scanning; handled below
                }
            }
            if let Some(path_val) = match_indented_value(line, 4, "path") {
                c.path = util::strip_quotes(path_val.trim());
                in_runtimes = false;
                continue;
            }
            if let Some(lxs_val) = match_indented_value(line, 4, "lxs") {
                c.lxs = util::strip_quotes(lxs_val.trim());
                in_runtimes = false;
                continue;
            }
            if let Some(bin_val) = match_indented_value(line, 4, "binary") {
                c.binary = util::strip_quotes(bin_val.trim());
                in_runtimes = false;
                continue;
            }
            if line.trim_start().starts_with("grants:") {
                in_runtimes = false;
                continue;
            }
            if let Some(list) = match_indented_list(line, 6, "secrets") {
                c.grants_secrets.extend(list);
                in_runtimes = false;
                continue;
            }
            if let Some(list) = match_indented_list(line, 6, "network") {
                c.grants_network.extend(list);
                in_runtimes = false;
                continue;
            }
            if line.trim_start().starts_with("runtimes:") {
                // Support both block form (`runtimes:\n  - rust`) and inline
                // flow form (`runtimes: [rust, mongodb@7]`).
                let after = line.trim_start().trim_start_matches("runtimes:").trim();
                if after.starts_with('[') {
                    let inner = after.trim_start_matches('[').trim_end_matches(']');
                    for item in inner.split(',') {
                        let item = util::strip_quotes(item.trim());
                        if !item.is_empty() {
                            c.runtimes.push(item);
                        }
                    }
                    in_runtimes = false;
                } else {
                    in_runtimes = true;
                }
                continue;
            }
            if in_runtimes {
                let trimmed = line.trim_start();
                if let Some(rest) = trimmed.strip_prefix('-') {
                    c.runtimes.push(util::strip_quotes(rest.trim()));
                    continue;
                }
            }
            if line.trim_start().chars().next().is_some_and(|ch| !ch.is_whitespace()) {
                // not handled; fine
            }
        }
    }
    if let Some(c) = current.take() {
        services.push(c);
    }
    services
}

pub fn parse_ct_metadata(content: &str) -> HashMap<String, String> {
    parse_indented_block(content, "ct:")
}

pub fn parse_storage(content: &str) -> HashMap<String, HashMap<String, String>> {
    let mut storage: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut in_storage = false;
    let mut current_provider: Option<String> = None;

    for raw in content.split('\n') {
        let line = raw.trim_end_matches('\r').trim_end();
        if line.is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if line == "storage:" {
            in_storage = true;
            continue;
        }
        if in_storage && starts_top_level_key(line) {
            break;
        }
        if !in_storage {
            continue;
        }
        if let Some(provider) = match_indented_key(line, 2) {
            current_provider = Some(provider.clone());
            storage.entry(provider).or_default();
            continue;
        }
        if let Some(provider) = current_provider.as_ref() {
            if let Some((key, value)) = match_indented_key_value(line, 4) {
                storage.get_mut(provider).unwrap().insert(key, util::strip_quotes(value.trim()));
            }
        }
    }
    storage
}

pub fn parse_expose(content: &str) -> Expose {
    let mut expose = Expose::default();
    let mut in_expose = false;
    let mut in_additional = false;
    let mut current_additional: Option<HashMap<String, String>> = None;

    for raw in content.split('\n') {
        let line = raw.trim_end_matches('\r').trim_end();
        if line.is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if line == "expose:" {
            in_expose = true;
            continue;
        }
        if !in_expose {
            continue;
        }
        if starts_top_level_key(line) {
            break;
        }
        if line.trim_start() == "additional:" {
            in_additional = true;
            continue;
        }
        if in_additional {
            // item: "    - hostname: value"
            if let Some((key, value)) = match_dash_item(line, 4) {
                flush_additional(&mut current_additional, &mut expose);
                let mut entry = HashMap::new();
                entry.insert(key, util::strip_quotes(value.trim()));
                current_additional = Some(entry);
                continue;
            }
            // field: "      key: value"
            if let Some((key, value)) = match_indented_key_value(line, 6) {
                if let Some(ca) = current_additional.as_mut() {
                    ca.insert(key, util::strip_quotes(value.trim()));
                    continue;
                }
            }
            in_additional = false;
            flush_additional(&mut current_additional, &mut expose);
        }
        if let Some((key, value)) = match_indented_key_value(line, 2) {
            expose.map.insert(key, util::strip_quotes(value.trim()));
        }
    }
    flush_additional(&mut current_additional, &mut expose);
    if let Some(count) = expose.map.get("tunnel_replicas") {
        if let Ok(parsed) = count.parse::<i64>() {
            if parsed > 0 {
                expose.tunnel_replicas = Some(parsed);
            }
        }
    }
    expose
}

fn flush_additional(current: &mut Option<HashMap<String, String>>, expose: &mut Expose) {
    if let Some(entry) = current.take() {
        expose.additional.push(entry);
    }
}

#[derive(Debug, Clone, Default)]
pub struct Expose {
    pub map: HashMap<String, String>,
    pub additional: Vec<HashMap<String, String>>,
    pub tunnel_replicas: Option<i64>,
}

impl Expose {
    pub fn get(&self, key: &str) -> Option<&String> {
        self.map.get(key)
    }

    pub fn enabled(&self) -> bool {
        util::to_bool(self.get("enabled").map(|s| s.as_str()).unwrap_or(""))
    }

    pub fn hostname(&self) -> String {
        self.get("hostname").cloned().unwrap_or_default()
    }

    pub fn service(&self) -> String {
        self.get("service").cloned().unwrap_or_default()
    }

    pub fn cloudflare_account(&self) -> String {
        self.get("cloudflare_account").cloned().unwrap_or_default()
    }

    pub fn proxy_ct_input(&self) -> String {
        self.get("proxy_ct")
            .or_else(|| self.get("proxy_ctid"))
            .or_else(|| self.get("via"))
            .cloned()
            .unwrap_or_default()
    }

    pub fn target_port(&self) -> String {
        self.get("target_port").cloned().unwrap_or_default()
    }

    pub fn cloudflared_config(&self) -> String {
        self.get("cloudflared_config").cloned().unwrap_or_default()
    }

    pub fn tunnel_name(&self) -> String {
        self.get("tunnel_name").cloned().unwrap_or_default()
    }
}

#[derive(Debug, Clone, Default)]
pub struct DeployGithub {
    pub map: HashMap<String, String>,
}

impl DeployGithub {
    pub fn get(&self, key: &str) -> Option<&String> {
        self.map.get(key)
    }

    pub fn enabled(&self) -> bool {
        util::to_bool(self.get("enabled").map(|s| s.as_str()).unwrap_or(""))
    }
}

pub fn parse_deploy(content: &str) -> HashMap<String, HashMap<String, String>> {
    let mut deploy: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut in_deploy = false;
    let mut current_section: Option<String> = None;

    for raw in content.split('\n') {
        let line = raw.trim_end_matches('\r').trim_end();
        if line.is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if line == "deploy:" {
            in_deploy = true;
            continue;
        }
        if !in_deploy {
            continue;
        }
        if starts_top_level_key(line) {
            break;
        }
        if let Some(section) = match_indented_key(line, 2) {
            current_section = Some(section.clone());
            deploy.entry(section).or_default();
            continue;
        }
        if let Some(section) = current_section.as_ref() {
            if let Some((key, value)) = match_indented_key_value(line, 4) {
                deploy.get_mut(section).unwrap().insert(key, util::strip_quotes(value.trim()));
            }
        }
    }
    deploy
}

pub fn parse_staging(content: &str) -> HashMap<String, String> {
    let mut staging: HashMap<String, String> = HashMap::new();
    let mut in_staging = false;

    for raw in content.split('\n') {
        let line = raw.trim_end_matches('\r').trim_end();
        if line.is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if line == "staging:" {
            in_staging = true;
            continue;
        }
        if !in_staging {
            continue;
        }
        if starts_top_level_key(line) {
            break;
        }
        if let Some((key, value)) = match_indented_key_value(line, 2) {
            staging.insert(key, util::strip_quotes(value.trim()));
        }
    }
    staging
}

/// Parse a top-level indented key/value block into a map (e.g. `ct:`).
pub fn parse_indented_block(content: &str, block_header: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut in_block = false;

    for raw in content.split('\n') {
        let line = raw.trim_end_matches('\r').trim_end();
        if line.is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if line == block_header {
            in_block = true;
            continue;
        }
        if !in_block {
            continue;
        }
        if starts_top_level_key(line) {
            break;
        }
        if let Some((key, value)) = match_indented_key_value(line, 2) {
            map.insert(key, util::strip_quotes(value.trim()));
        }
    }
    map
}

fn starts_top_level_key(line: &str) -> bool {
    line.starts_with(|c: char| !c.is_whitespace()) && line.contains(':') && !line.starts_with('#')
}

fn match_indented_key(line: &str, indent: usize) -> Option<String> {
    let prefix = " ".repeat(indent);
    let rest = line.strip_prefix(&prefix)?;
    if rest.ends_with(':') && !rest.starts_with('-') && rest.trim().len() == rest.trim_end_matches(':').len() + 1 {
        let key = rest.trim_end_matches(':').trim();
        if !key.is_empty() && !key.contains(' ') {
            return Some(key.to_string());
        }
    }
    None
}

fn match_indented_key_value(line: &str, indent: usize) -> Option<(String, String)> {
    let prefix = " ".repeat(indent);
    let rest = line.strip_prefix(&prefix)?;
    if rest.starts_with('-') || rest.starts_with('#') {
        return None;
    }
    let colon = rest.find(':')?;
    let key = rest[..colon].trim();
    if key.is_empty() || key.contains(' ') {
        return None;
    }
    let value = rest[colon + 1..].trim();
    Some((key.to_string(), value.to_string()))
}

/// Matches a list item of the form "<indent spaces>- key: value".
fn match_dash_item(line: &str, indent: usize) -> Option<(String, String)> {
    let prefix = " ".repeat(indent);
    let rest = line.strip_prefix(&prefix)?;
    let rest = rest.strip_prefix("- ")?;
    let colon = rest.find(':')?;
    let key = rest[..colon].trim();
    if key.is_empty() {
        return None;
    }
    let value = rest[colon + 1..].trim();
    Some((key.to_string(), value.to_string()))
}

/// Parse the `composition:` block (git/branch for the composition repo).
pub fn parse_composition(content: &str) -> HashMap<String, String> {
    parse_indented_block(content, "composition:")
}

/// Parse `domains:` list entries. Returns entries in declaration order.
/// A plain `- name` yields ("name", None); `- name: branch` yields
/// ("name", Some("branch")); a block `- name:` (empty value) yields the name
/// with no branch override.
pub fn parse_domains(content: &str) -> Vec<(String, Option<String>)> {
    let mut domains: Vec<(String, Option<String>)> = Vec::new();
    let mut in_domains = false;

    for raw in content.split('\n') {
        let line = raw.trim_end_matches('\r').trim_end();
        if line.is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if line == "domains:" {
            in_domains = true;
            continue;
        }
        if in_domains && starts_top_level_key(line) {
            in_domains = false;
            continue;
        }
        if !in_domains {
            continue;
        }
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("- ") {
            let raw_value = util::strip_quotes(rest.trim());
            let colon = raw_value.find(':');
            match colon {
                Some(idx) => {
                    let name = raw_value[..idx].trim().to_string();
                    let after = raw_value[idx + 1..].trim().to_string();
                    if after.is_empty() {
                        domains.push((name, None));
                    } else {
                        domains.push((name, Some(util::strip_quotes(&after))));
                    }
                }
                None => {
                    domains.push((raw_value, None));
                }
            }
        }
    }
    domains
}

/// Return unique domain names from the domains list plus service path roots.
pub fn unique_domains_from_ecompose(content: &str, project: &str) -> Vec<String> {
    let mut domains: Vec<String> = vec![project.to_string()];
    let mut in_domains = false;

    // In ecompose v2 a `path:` service points INSIDE the estate repo (relative
    // to the repo root) — it is NOT an external repo to clone. Only a legacy
    // `domains:` block names sibling repos. Scanning path segments as domains
    // was the v1 mental model and broke v2 manifests (e.g. `path: frontend`
    // was treated as a missing git remote named "frontend").
    for raw in content.split('\n') {
        let line = raw.trim_end_matches('\r').trim_end();
        if line.is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if line == "domains:" {
            in_domains = true;
            continue;
        }
        if starts_top_level_key(line) {
            in_domains = false;
        }
        if in_domains {
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_prefix("- ") {
                let raw_value = util::strip_quotes(rest.trim());
                let value = raw_value.split(':').next().unwrap_or("").trim().to_string();
                if !value.is_empty() && !domains.contains(&value) {
                    domains.push(value);
                }
            }
        }
    }
    domains
}

fn match_indented_value(line: &str, indent: usize, key: &str) -> Option<String> {
    let prefix = " ".repeat(indent);
    let rest = line.strip_prefix(&prefix)?;
    let expected = format!("{key}:");
    let value = rest.strip_prefix(&expected)?;
    Some(value.to_string())
}

// Parses `key: [a, b]` flow-list values (e.g. `secrets: [JWT_SECRET, MONGODB_URI]`).
fn match_indented_list(line: &str, indent: usize, key: &str) -> Option<Vec<String>> {
    let value = match_indented_value(line, indent, key)?;
    let trimmed = value.trim();
    let inner = trimmed.strip_prefix('[')?.strip_suffix(']')?;
    let items = inner
        .split(',')
        .map(|s| util::strip_quotes(s.trim()))
        .filter(|s| !s.is_empty())
        .collect::<Vec<String>>();
    Some(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_service_access_routes() {
        let content = r#"
project: proof-rust
services:
  auth-backend:
    lxs: auth@1.1.0
    access:
      routes:
        - path: /api/auth/login
          level: public
        - path: /api/auth/*
          level: auth
  frontend:
    path: .
    runtimes: [rust]
    access:
      routes:
        - path: /
          level: public
"#;
        let svcs = parse_services(content);
        assert_eq!(svcs.len(), 2);
        let auth = svcs.iter().find(|s| s.name == "auth-backend").unwrap();
        assert_eq!(auth.access_routes.len(), 2);
        assert_eq!(auth.access_routes[0].path, "/api/auth/login");
        assert_eq!(auth.access_routes[0].level, "public");
        assert_eq!(auth.access_routes[1].path, "/api/auth/*");
        assert_eq!(auth.access_routes[1].level, "auth");
        let fe = svcs.iter().find(|s| s.name == "frontend").unwrap();
        assert_eq!(fe.access_routes.len(), 1);
        assert_eq!(fe.access_routes[0].path, "/");
        assert_eq!(fe.access_routes[0].level, "public");
    }

    #[test]
    fn parses_auth_roles() {
        let content = r#"
project: proof-rust
auth:
  email_verification:
    enabled: false
  roles:
    - name: public
      level: public
    - name: authenticated
      level: auth
      default: true
services:
  auth-backend:
    lxs: auth@1.1.0
"#;
        let cfg = parse_auth(content);
        assert_eq!(cfg.email_verification_enabled.as_deref(), Some("false"));
        assert_eq!(cfg.roles.len(), 2);
        assert_eq!(cfg.roles[0].name, "public");
        assert_eq!(cfg.roles[0].level, "public");
        assert!(!cfg.roles[0].default);
        assert_eq!(cfg.roles[1].name, "authenticated");
        assert_eq!(cfg.roles[1].level, "auth");
        assert!(cfg.roles[1].default);
    }

    #[test]
    fn parse_services_ignores_access_after_service_ends() {
        let content = r#"
project: x
services:
  a:
    path: a
    runtimes: [rust]
    access:
      routes:
        - path: /
          level: public
  b:
    lxs: b@1.0.0
"#;
        let svcs = parse_services(content);
        let a = svcs.iter().find(|s| s.name == "a").unwrap();
        assert_eq!(a.access_routes.len(), 1);
        let b = svcs.iter().find(|s| s.name == "b").unwrap();
        assert!(b.access_routes.is_empty());
    }
}

#[cfg(test)]
mod proof_rust_tests {
    use super::*;

    #[test]
    fn parses_proof_rust_manifest() {
        let content = std::fs::read_to_string("/Users/eco/ar-rahman/proofs/proof-rust/ecompose.yml").unwrap();
        let svcs = parse_services(&content);
        let gw = svcs.iter().find(|s| s.name == "gateway").unwrap();
        assert_eq!(gw.path, "../../lxs/gateway/backend");
        assert_eq!(gw.binary, "gateway");
        let auth = svcs.iter().find(|s| s.name == "auth-backend").unwrap();
        assert_eq!(auth.path, "../../lxs/auth/backend");
        assert_eq!(auth.binary, "rwid-auth-service");
        assert_eq!(auth.access_routes.len(), 7);
        let fe = svcs.iter().find(|s| s.name == "proof-rust").unwrap();
        assert_eq!(fe.access_routes.len(), 3);
        let auth_cfg = parse_auth(&content);
        assert_eq!(auth_cfg.roles.len(), 2);
        assert_eq!(auth_cfg.roles[1].name, "authenticated");
        assert!(auth_cfg.roles[1].default);
        assert_eq!(auth_cfg.email_verification_enabled.as_deref(), Some("false"));
        let ct = parse_ct_metadata(&content);
        assert_eq!(ct.get("id").map(|s| s.as_str()), Some("1000"));
    }
}

#[cfg(test)]
mod assessment_tests {
    use super::*;

    #[test]
    fn parses_assessment_manifest() {
        let content = std::fs::read_to_string("/Users/eco/ar-rahman/estates/assessment/ecompose.yml").unwrap();
        let svcs = parse_services(&content);
        let gw = svcs.iter().find(|s| s.name == "gateway").unwrap();
        assert_eq!(gw.binary, "gateway");
        let fe = svcs.iter().find(|s| s.name == "assessment-frontend").unwrap();
        let admin = fe.access_routes.iter().find(|r| r.path == "/admin*").unwrap();
        assert_eq!(admin.level, "role:SUPERADMIN");
        assert_eq!(admin.cookie, "assessment_token");
        let auth = svcs.iter().find(|s| s.name == "auth-backend").unwrap();
        assert_eq!(auth.access_routes.len(), 12);
        let authapi = auth.access_routes.iter().find(|r| r.path == "/auth-api/*").unwrap();
        assert_eq!(authapi.strip, "/auth-api");
        assert_eq!(authapi.rewrite, "/api");
        let cfg = parse_auth(&content);
        assert_eq!(cfg.roles.len(), 6);
        assert!(cfg.roles.iter().any(|r| r.name == "SUPERADMIN"));
        assert_eq!(cfg.roles.iter().find(|r| r.name == "SISWA").unwrap().default, true);
        assert_eq!(cfg.email_verification_enabled.as_deref(), Some("false"));
    }
}
