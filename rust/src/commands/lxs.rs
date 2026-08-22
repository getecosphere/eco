use crate::util;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const DEFAULT_REGISTRY: &str = "lxs-registry";
const ARTIFACTS_FILE: &str = ".lxs-artifacts.json";
const ZIG_VERSION: &str = "0.13.0";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LxsArtifact {
    pub path: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LxsEnv {
    #[serde(default)]
    pub required: Vec<String>,
    #[serde(default)]
    pub optional: Vec<String>,
    #[serde(default)]
    pub defaults: HashMap<String, String>,
    /// Contract v2: per-key schema metadata. When non-empty, this is the
    /// authoritative env contract (required/optional/defaults are derived
    /// views). See eco-server/docs/lxs-config-schema-v2.md.
    #[serde(default)]
    pub fields: HashMap<String, LxsField>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LxsField {
    /// Non-empty value must exist before the service starts.
    #[serde(default)]
    pub required: bool,
    /// Value shape: string | bool | int | float | enum | csv | csv-url | url |
    /// uri | email | secret | json.
    #[serde(default)]
    pub r#type: String,
    /// Static default written to `.env` when nothing else sets the key.
    #[serde(default)]
    pub default: String,
    /// Human label/detail for CLI + dashboard.
    #[serde(default)]
    pub description: String,
    /// Dashboard section grouping.
    #[serde(default)]
    pub group: String,
    /// Allowed values; only valid when `type: enum`.
    #[serde(default)]
    pub choices: Vec<String>,
    /// Numeric bounds (int/float) or string/secret length bounds.
    #[serde(default)]
    pub min: f64,
    #[serde(default)]
    pub max: f64,
    /// Regex validation for string/secret/url.
    #[serde(default)]
    pub pattern: String,
    /// Sensitive: never rendered, never stored in the manifest — the value
    /// lives in `.env`/secret store, the key goes in `grants.secrets`.
    #[serde(default)]
    pub secret: bool,
    /// Eco derives/fills this key (port, shared-jwt, mongo-db, ...). Managed
    /// keys are not settable via `services.<name>.config`.
    #[serde(default)]
    pub managed: String,
    /// Sample value (docs/CLI hint).
    #[serde(default)]
    pub example: String,
}

/// Publish-time lint for the contract v2 env schema. Runs only when `fields`
/// is present (v2 contracts). See eco-server/docs/lxs-config-schema-v2.md.
pub fn validate_env_fields(env: &LxsEnv) -> Result<(), String> {
    if env.fields.is_empty() {
        return Ok(());
    }
    const KNOWN_TYPES: &[&str] = &[
        "string", "bool", "int", "float", "enum", "csv", "csv-url", "url", "uri", "email",
        "secret", "json",
    ];
    for (key, field) in &env.fields {
        let where_ = format!("lxs.yml contract.env.fields.{key}");
        if field.description.trim().is_empty() {
            return Err(format!(
                "{where_}: missing description (required by the v2 schema lint)"
            ));
        }
        let ty = field.r#type.as_str();
        if !KNOWN_TYPES.contains(&ty) {
            return Err(format!(
                "{where_}: unknown type '{ty}' (known: {})",
                KNOWN_TYPES.join(", ")
            ));
        }
        if ty == "enum" && field.choices.is_empty() {
            return Err(format!("{where_}: type enum requires non-empty choices"));
        }
        if ty != "enum" && !field.choices.is_empty() {
            return Err(format!("{where_}: type '{ty}' must not declare choices"));
        }
        if field.min != 0.0 && field.max != 0.0 && field.min > field.max {
            return Err(format!(
                "{where_}: min ({}) > max ({})",
                field.min, field.max
            ));
        }
    }
    Ok(())
}

/// Build the `.env.example` text (env contract) for an LXS from its manifest
/// contract plus the estate's per-service `config:` values.
///
/// - v2 (`fields` non-empty): fields drive the key set + defaults; every
///   `config:` value is validated against the schema (unknown key / secret /
///   managed key → error).
/// - v1 (no fields): required + optional + defaults; `config:` values overlay,
///   validated only against the declared key lists.
pub fn build_lxs_env_example(
    env: &LxsEnv,
    service_name: &str,
    config: &HashMap<String, String>,
) -> Result<String, String> {
    let mut lines: Vec<String> = Vec::new();
    if !env.fields.is_empty() {
        let mut keys: Vec<&String> = env.fields.keys().collect();
        keys.sort();
        for key in &keys {
            let field = &env.fields[key.as_str()];
            let value = config
                .get(key.as_str())
                .cloned()
                .unwrap_or_else(|| field.default.clone());
            lines.push(format!("{key}={value}"));
        }
        for (key, value) in config {
            match env.fields.get(key) {
                None => {
                    let allowed = keys
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(format!(
                        "service {service_name}: config key {key} is not declared in the LXS env contract (allowed: {allowed})"
                    ));
                }
                Some(f) if f.secret => {
                    return Err(format!(
                        "service {service_name}: {key} is a secret field — declare it in grants.secrets, never set it in config:"
                    ));
                }
                Some(f) if !f.managed.is_empty() => {
                    return Err(format!(
                        "service {service_name}: {key} is managed by eco ({}) — it cannot be set in config:",
                        f.managed
                    ));
                }
                Some(_) => {
                    let _ = value;
                }
            }
        }
    } else {
        let known: std::collections::HashSet<&String> =
            env.required.iter().chain(env.optional.iter()).collect();
        for key in config.keys() {
            if !known.contains(key) {
                return Err(format!("service {service_name}: config key {key} is not declared in the LXS env contract"));
            }
        }
        for key in env.required.iter().chain(env.optional.iter()) {
            let value = config
                .get(key)
                .cloned()
                .unwrap_or_else(|| env.defaults.get(key).cloned().unwrap_or_default());
            lines.push(format!("{key}={value}"));
        }
    }
    Ok(lines.join("\n") + "\n")
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LxsNetwork {
    #[serde(default)]
    pub inbound: Vec<String>,
    #[serde(default)]
    pub outbound: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LxsResources {
    #[serde(default)]
    pub memory: String,
    #[serde(default)]
    pub disk: String,
    #[serde(default)]
    pub startup_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LxsContract {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub api: String,
    #[serde(default)]
    pub env: LxsEnv,
    #[serde(default)]
    pub db: String,
    #[serde(default)]
    pub network: LxsNetwork,
    #[serde(default)]
    pub resources: LxsResources,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LxsRuntime {
    #[serde(default)]
    pub base: String,
    #[serde(default)]
    pub libc: String,
    #[serde(default)]
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LxsProvenance {
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub commit: String,
    #[serde(default)]
    pub built_by: String,
    #[serde(default)]
    pub built_at: String,
    #[serde(default)]
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LxsManifest {
    pub name: String,
    #[serde(default)]
    pub domain: String,
    pub version: String,
    #[serde(default)]
    pub publisher: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub targets: Vec<String>,
    #[serde(default)]
    pub artifacts: HashMap<String, LxsArtifact>,
    #[serde(default)]
    pub contract: LxsContract,
    #[serde(default)]
    pub runtime: LxsRuntime,
    #[serde(default)]
    pub provenance: LxsProvenance,
    #[serde(default)]
    pub release: Vec<String>,
    #[serde(default)]
    pub docs: Vec<String>,
}

pub(crate) fn registry_root() -> Result<PathBuf, String> {
    if let Some(path) = util::env_var_or("ECO_LXS_REGISTRY", "").strip_prefix("") {
        if !path.is_empty() {
            return Ok(PathBuf::from(path));
        }
    }
    let root = util::env_var_or("ECO_PROJECTS_ROOT", "");
    if !root.is_empty() {
        return Ok(Path::new(&root).join(DEFAULT_REGISTRY));
    }
    let home = util::home_dir();
    Ok(Path::new(&home).join("projects").join(DEFAULT_REGISTRY))
}

// The public LXS registry source of truth. eco does NOT resolve LXS from
// repos.json — every `lxs:` service and `eco lxs pull` is resolved against
// this remote clone of getecosphere/lxs-registry. `ensure_registry_synced`
// clones it on first use and fast-forwards it on every read so estates always
// install the current published versions (a stale local registry is otherwise
// an invisible foot-gun). Failures are tolerated (offline host) — the caller
// then sees whatever the local clone has, as before.
pub(crate) const LXS_REGISTRY_REMOTE: &str = "https://github.com/getecosphere/lxs-registry.git";

pub(crate) fn ensure_registry_synced() -> Result<(), String> {
    let registry = registry_root()?;
    let _ = std::fs::create_dir_all(registry.parent().unwrap_or(Path::new(".")));
    if !registry.join(".git").exists() {
        // Avoid cloning into a non-empty partial dir (e.g. interrupted clone).
        if registry.exists()
            && std::fs::read_dir(&registry)
                .map(|mut d| d.next().is_some())
                .unwrap_or(false)
        {
            let _ = std::fs::remove_dir_all(&registry);
        }
        if let Err(e) = run_command(
            "git",
            &[
                "clone".to_string(),
                "--quiet".to_string(),
                "--depth".to_string(),
                "1".to_string(),
                LXS_REGISTRY_REMOTE.to_string(),
                registry.display().to_string(),
            ],
            &util::current_dir(),
        ) {
            println!("[eco lxs] WARNING: could not clone the LXS registry from {LXS_REGISTRY_REMOTE}: {e}");
        }
    } else {
        if let Err(e) = git(
            &[
                "pull".to_string(),
                "--ff-only".to_string(),
                "origin".to_string(),
                "main".to_string(),
            ],
            &registry,
        ) {
            println!("[eco lxs] WARNING: could not refresh LXS registry from remote ({e}); using the local clone as-is");
        }
    }
    Ok(())
}

fn arch_to_triple(arch: &str) -> Result<&'static str, String> {
    match arch {
        "linux/amd64" | "amd64" | "x86_64" | "linux/x86_64" => Ok("x86_64-unknown-linux-musl"),
        "linux/arm64" | "arm64" | "aarch64" | "linux/aarch64" => Ok("aarch64-unknown-linux-musl"),
        "darwin/arm64" | "macos/arm64" | "darwin/aarch64" => Ok("aarch64-apple-darwin"),
        "darwin/amd64" | "macos/amd64" | "darwin/x86_64" => Ok("x86_64-apple-darwin"),
        "windows/amd64" | "win32/amd64" | "windows/x86_64" => Ok("x86_64-pc-windows-gnu"),
        other => Err(format!(
            "unsupported LXS target: {other} (use linux/amd64, linux/arm64, darwin/arm64, darwin/amd64, windows/amd64)"
        )),
    }
}

fn triple_to_arch(triple: &str) -> String {
    match triple {
        "x86_64-unknown-linux-musl" => "linux/amd64".to_string(),
        "aarch64-unknown-linux-musl" => "linux/arm64".to_string(),
        "aarch64-apple-darwin" => "darwin/arm64".to_string(),
        "x86_64-apple-darwin" => "darwin/amd64".to_string(),
        "x86_64-pc-windows-gnu" => "windows/amd64".to_string(),
        other => other.to_string(),
    }
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let digest = sha2::Sha256::digest(&bytes);
    Ok(crate::registry::hex_encode(&digest))
}

fn run_command(command: &str, args: &[String], cwd: &Path) -> Result<(), String> {
    util::run_command(command, args, cwd)
}

fn run_capture(command: &str, args: &[String], cwd: &Path) -> Result<util::Captured, String> {
    util::run_capture(command, args, cwd)
}

fn git(args: &[String], cwd: &Path) -> Result<String, String> {
    let result = run_capture("git", args, cwd)?;
    if result.code != 0 {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            result.stderr.trim()
        ));
    }
    Ok(result.stdout.trim().to_string())
}

fn git_repo_origin(cwd: &Path) -> String {
    run_capture(
        "git",
        &[
            "config".to_string(),
            "--get".to_string(),
            "remote.origin.url".to_string(),
        ],
        cwd,
    )
    .map(|r| r.stdout.trim().to_string())
    .unwrap_or_default()
}

// ─────────────────────────────────────────────────────────────────────────────
// Cross-compile toolchain (same as build-release.sh / `eco up --remote`)
// ─────────────────────────────────────────────────────────────────────────────

fn rustup_target_installed(target: &str) -> bool {
    match run_capture(
        "rustup",
        &[
            "target".to_string(),
            "list".to_string(),
            "--installed".to_string(),
        ],
        &util::current_dir(),
    ) {
        Ok(result) => result.stdout.lines().any(|line| line.trim() == target),
        Err(_) => false,
    }
}

fn ensure_zig() -> Result<Option<PathBuf>, String> {
    if util::command_on_path("zig") {
        return Ok(None);
    }
    let triple = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "x86_64") => "macos-x86_64".to_string(),
        ("macos", "aarch64") => "macos-aarch64".to_string(),
        ("linux", "x86_64") => "linux-x86_64".to_string(),
        ("linux", "aarch64") => "linux-aarch64".to_string(),
        (os, arch) => return Err(format!("zig cross toolchain unsupported on {os}/{arch}")),
    };
    let cache = Path::new(&util::home_dir())
        .join(".cache")
        .join("eco")
        .join("zig");
    let install_dir = cache.join(format!("zig-{triple}-{ZIG_VERSION}"));
    if install_dir.join("zig").is_file() {
        return Ok(Some(install_dir));
    }
    let tarball = format!("zig-{triple}-{ZIG_VERSION}.tar.xz");
    println!("[eco lxs] Downloading pinned zig {ZIG_VERSION} ({tarball})");
    let _ = std::fs::create_dir_all(&cache);
    let target_path = cache.join(&tarball);
    let url = format!("https://ziglang.org/download/{ZIG_VERSION}/{tarball}");
    run_command(
        "curl",
        &[
            "-fsSL".to_string(),
            url,
            "-o".to_string(),
            target_path.display().to_string(),
        ],
        &util::current_dir(),
    )?;
    run_command(
        "tar",
        &[
            "xf".to_string(),
            target_path.display().to_string(),
            "-C".to_string(),
            cache.display().to_string(),
        ],
        &util::current_dir(),
    )?;
    let _ = std::fs::remove_file(&target_path);
    if !install_dir.join("zig").is_file() {
        return Err(format!(
            "zig extraction did not produce {}",
            install_dir.display()
        ));
    }
    Ok(Some(install_dir))
}

fn ensure_toolchain(target: &str) -> Result<Option<PathBuf>, String> {
    if !rustup_target_installed(target) {
        println!("[eco lxs] Installing rustup target {target}");
        run_command(
            "rustup",
            &["target".to_string(), "add".to_string(), target.to_string()],
            &util::current_dir(),
        )?;
    }
    // Darwin targets build with the platform toolchain (plain cargo); only
    // musl-Linux and Windows-GNU cross-compiles need zig + cargo-zigbuild.
    if target.contains("apple-darwin") {
        return Ok(None);
    }
    if !util::command_on_path("cargo-zigbuild") {
        println!("[eco lxs] Installing cargo-zigbuild (pinned)");
        run_command(
            "cargo",
            &[
                "install".to_string(),
                "cargo-zigbuild".to_string(),
                "--locked".to_string(),
            ],
            &util::current_dir(),
        )?;
    }
    ensure_zig()
}

// ─────────────────────────────────────────────────────────────────────────────
// eco lxs build
// ─────────────────────────────────────────────────────────────────────────────

fn crate_package_name(cargo_toml: &str) -> Option<String> {
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
                    let val = rest[eq + 1..]
                        .trim()
                        .trim_matches('"')
                        .trim_matches('\'')
                        .to_string();
                    if !val.is_empty() {
                        return Some(val);
                    }
                }
            }
        }
    }
    None
}

fn find_crate_dir(source: &Path) -> Option<PathBuf> {
    for candidate in [source.join("backend"), source.to_path_buf()] {
        if candidate.join("Cargo.toml").is_file() {
            return Some(candidate);
        }
    }
    if let Ok(entries) = std::fs::read_dir(source) {
        let mut crates = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && path.join("Cargo.toml").is_file() {
                crates.push(path);
            }
        }
        if crates.len() == 1 {
            return Some(crates.remove(0));
        }
    }
    None
}

/// Find the Node/Astro app directory for a bun-compilable LXS UI: prefers
/// `frontend/`, then the source root, when it has a package.json with an
/// Astro (or generic node SSR) build.
fn find_node_app_dir(source: &Path) -> Option<PathBuf> {
    let mut found: Option<PathBuf> = None;
    for candidate in [source.join("frontend"), source.to_path_buf()] {
        let pkg = candidate.join("package.json");
        if pkg.is_file() {
            found = Some(candidate);
            break;
        }
    }
    let candidate = found?;
    let pkg_text = std::fs::read_to_string(candidate.join("package.json")).ok()?;
    // Astro is the supported bun-compile UI target today.
    if pkg_text.contains("\"astro\"") && candidate.join("astro.config.mjs").is_file() {
        return Some(candidate);
    }
    if pkg_text.contains("\"astro\"") || pkg_text.contains("\"@astrojs/") {
        return Some(candidate);
    }
    None
}

fn build_crate_for_target(
    crate_dir: &Path,
    target: &str,
    zig_dir: &Option<PathBuf>,
) -> Result<(String, PathBuf), String> {
    let cargo_text =
        std::fs::read_to_string(crate_dir.join("Cargo.toml")).map_err(|e| e.to_string())?;
    let package = crate_package_name(&cargo_text)
        .ok_or_else(|| format!("cannot determine package name from {}", crate_dir.display()))?;
    println!("[eco lxs] Cross-compiling {package} for {target}");

    // Build from an isolated, persistent workspace so an LXS build never
    // depends on the surrounding estate's (generated, sometimes stale) Cargo
    // workspace. The domain crate is copied in; the target/ dir persists per
    // domain so incremental builds stay fast; the nearest Cargo.lock is reused
    // for reproducible versions.
    let build_root = Path::new(&util::home_dir())
        .join(".cache")
        .join("eco")
        .join("lxs-build")
        .join(&package);
    let member_dir = build_root.join(&package);
    let _ = std::fs::remove_dir_all(&member_dir);
    std::fs::create_dir_all(&member_dir).map_err(|e| e.to_string())?;
    copy_crate_source(crate_dir, &member_dir)?;
    if let Some(lock) = nearest_cargo_lock(crate_dir) {
        std::fs::copy(&lock, build_root.join("Cargo.lock"))
            .map_err(|e| format!("copy Cargo.lock: {e}"))?;
    }
    let workspace_toml = format!("# Generated by eco lxs build -- isolated LXS workspace\n[workspace]\nresolver = \"2\"\nmembers = [\"{package}\"]\n");
    std::fs::write(build_root.join("Cargo.toml"), workspace_toml).map_err(|e| e.to_string())?;

    let mut build_env: Vec<(String, String)> = Vec::new();
    if let Some(zig_dir) = zig_dir {
        let path = std::env::var("PATH").unwrap_or_default();
        build_env.push(("PATH".to_string(), format!("{}:{path}", zig_dir.display())));
    }
    // Darwin targets build with the platform toolchain (native/cross via
    // rustup target + the OS SDK); musl-Linux and Windows-GNU go through
    // cargo-zigbuild so no C toolchain is needed on the build machine.
    let use_plain_cargo = target.contains("apple-darwin");
    let args: Vec<String> = if use_plain_cargo {
        ["build", "--release", "--target", target]
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        ["zigbuild", "--release", "--target", target]
            .iter()
            .map(|s| s.to_string())
            .collect()
    };
    let env_map: HashMap<String, String> = {
        let mut m: HashMap<String, String> = std::env::vars().collect();
        for (k, v) in &build_env {
            m.insert(k.clone(), v.clone());
        }
        m
    };
    util::run_command_env("cargo", &args, &build_root, &env_map)?;
    let is_windows = target.contains("-windows-");
    let binary = if is_windows {
        build_root
            .join("target")
            .join(target)
            .join("release")
            .join(format!("{package}.exe"))
    } else {
        build_root
            .join("target")
            .join(target)
            .join("release")
            .join(&package)
    };
    if !binary.is_file() {
        return Err(format!(
            "cross-compiled binary not found: {}",
            binary.display()
        ));
    }
    Ok((package, binary))
}

fn copy_crate_source(src: &Path, dst: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().to_string();
        if [".git", "target", "node_modules"].contains(&name.as_str()) {
            continue;
        }
        let source = entry.path();
        let destination = dst.join(&name);
        if source.is_dir() {
            std::fs::create_dir_all(&destination).map_err(|e| e.to_string())?;
            copy_crate_source(&source, &destination)?;
        } else {
            std::fs::copy(&source, &destination)
                .map_err(|e| format!("copy {}: {e}", source.display()))?;
        }
    }
    Ok(())
}

fn nearest_cargo_lock(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let candidate = d.join("Cargo.lock");
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = d.parent();
    }
    None
}

fn run_lxs_build(args: &[String]) -> Result<(), String> {
    let mut archs: Vec<String> = vec!["linux/amd64".to_string()];
    let mut source: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--arch" => {
                let raw = args.get(i + 1).cloned().unwrap_or_default();
                archs = raw
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                i += 2;
            }
            "--source" => {
                source = Some(PathBuf::from(args.get(i + 1).cloned().unwrap_or_default()));
                i += 2;
            }
            other if other.starts_with('-') => {
                return Err(format!("Unknown eco lxs build option: {other}"))
            }
            other => {
                source = Some(PathBuf::from(other));
                i += 1;
            }
        }
    }
    let source = source.unwrap_or_else(util::current_dir);

    // Node/Astro UI LXS: bun-compile a standalone Astro SSR app into a
    // self-contained binary for each declared target (no node_modules on the host).
    if find_crate_dir(&source).is_none() {
        if let Some(node_dir) = find_node_app_dir(&source) {
            println!(
                "[eco lxs] Detected Astro/Node UI project at {}",
                node_dir.display()
            );
            let mut artifacts: HashMap<String, String> = HashMap::new();
            for arch in &archs {
                let binary = build_node_app_for_target(&node_dir, arch)?;
                artifacts.insert(arch.clone(), binary.display().to_string());
            }
            let artifacts_json =
                serde_json::json!({ "arch": archs, "binaries": artifacts }).to_string();
            std::fs::write(source.join(ARTIFACTS_FILE), artifacts_json)
                .map_err(|e| e.to_string())?;
            println!(
                "[eco lxs] Built {} artifact(s) for {}",
                artifacts.len(),
                source.display()
            );
            for (arch, binary) in &artifacts {
                println!("  {arch}: {binary}");
            }
            println!(
                "[eco lxs] Artifact map written to {}/{}",
                source.display(),
                ARTIFACTS_FILE
            );
            return Ok(());
        }
    }

    let crate_dir = find_crate_dir(&source).ok_or_else(|| {
        format!(
            "no Cargo crate or Astro/Node app found under {}",
            source.display()
        )
    })?;

    let mut artifacts: HashMap<String, String> = HashMap::new();
    for arch in &archs {
        let target = arch_to_triple(arch)?;
        let zig = ensure_toolchain(target)?;
        let (_, binary) = build_crate_for_target(&crate_dir, target, &zig)?;
        artifacts.insert(arch.clone(), binary.display().to_string());
    }

    let artifacts_json = serde_json::json!({ "arch": archs, "binaries": artifacts }).to_string();
    std::fs::write(source.join(ARTIFACTS_FILE), artifacts_json).map_err(|e| e.to_string())?;
    println!(
        "[eco lxs] Built {} artifact(s) for {}",
        artifacts.len(),
        source.display()
    );
    for (arch, binary) in &artifacts {
        println!("  {arch}: {binary}");
    }
    println!(
        "[eco lxs] Artifact map written to {}/{}",
        source.display(),
        ARTIFACTS_FILE
    );
    Ok(())
}

/// Build a standalone Astro SSR app into a self-contained platform binary via
/// bun-compile. Runs `npm ci && npm run build` (Astro adapter-node
/// -> dist/server/entry.mjs + dist/client), then bun-compiles the server
/// entry, embedding client assets alongside the binary.
fn build_node_app_for_target(node_dir: &Path, arch: &str) -> Result<PathBuf, String> {
    if !util::command_on_path("bun") {
        return Err("bun is required to build an Astro/Node UI LXS but was not found on PATH. Install bun: `brew install oven-sh/bun/bun`.".to_string());
    }
    // Isolated build root so the source stays pristine (node_modules never
    // lands in the repo); reuse a per-package cache for speed.
    let pkg_name = node_package_name(&node_dir)?;
    let bun_target = match arch {
        "linux/amd64" => "bun-linux-x64",
        "linux/arm64" => "bun-linux-arm64",
        "darwin/arm64" => "bun-darwin-arm64",
        "darwin/amd64" => "bun-darwin-x64",
        other => return Err(format!(
            "unsupported LXS target: {other} (Astro/Node UI supports linux/amd64, linux/arm64, darwin/arm64, darwin/amd64)"
        )),
    };
    let target_label = arch.replace('/', "-");
    let build_root = Path::new(&util::home_dir())
        .join(".cache")
        .join("eco")
        .join("lxs-build")
        .join(&pkg_name)
        .join(target_label);
    let member_dir = build_root.join("app");
    let _ = std::fs::remove_dir_all(&member_dir);
    std::fs::create_dir_all(&member_dir).map_err(|e| e.to_string())?;
    copy_node_source(node_dir, &member_dir)?;

    println!("[eco lxs] npm ci + astro build (bun) for {pkg_name}");
    let npm_ci = run_command_in_dir("npm", &["ci"], &member_dir)?;
    let _ = npm_ci;
    let build = run_command_in_dir("npm", &["run", "build"], &member_dir)?;
    let _ = build;

    let server_entry = [
        "dist/server/entry.mjs",
        "dist/server/index.js",
        "dist/index.js",
    ]
    .iter()
    .map(|p| member_dir.join(p))
    .find(|p| p.is_file())
    .ok_or_else(|| {
        format!(
            "Astro build produced no dist/server entry under {}",
            member_dir.display()
        )
    })?;
    let out = build_root.join(&pkg_name);
    let _ = std::fs::remove_file(&out);
    let rel = server_entry
        .strip_prefix(&member_dir)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| server_entry.display().to_string());
    run_command_in_dir(
        "bun",
        &[
            "build",
            "--compile",
            &format!("--target={bun_target}"),
            &rel,
            "--outfile",
            &out.display().to_string(),
        ],
        &member_dir,
    )?;
    if !out.is_file() {
        return Err(format!("bun-compiled binary not found: {}", out.display()));
    }
    println!("[eco lxs] Bun-compiled {pkg_name} -> {}", out.display());
    Ok(out)
}

fn node_package_name(node_dir: &Path) -> Result<String, String> {
    let pkg_text =
        std::fs::read_to_string(node_dir.join("package.json")).map_err(|e| e.to_string())?;
    let v: serde_json::Value =
        serde_json::from_str(&pkg_text).map_err(|e| format!("parse package.json: {e}"))?;
    let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("node-lxs");
    Ok(name.trim_start_matches('@').replace('/', "-").to_string())
}

fn copy_node_source(src: &Path, dst: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().to_string();
        if [
            ".git",
            "target",
            "node_modules",
            "dist",
            ".astro",
            ".output",
        ]
        .contains(&name.as_str())
        {
            continue;
        }
        let source = entry.path();
        let destination = dst.join(&name);
        if source.is_dir() {
            std::fs::create_dir_all(&destination).map_err(|e| e.to_string())?;
            copy_node_source(&source, &destination)?;
        } else if !source.is_symlink() {
            std::fs::copy(&source, &destination)
                .map_err(|e| format!("copy {}: {e}", source.display()))?;
        }
    }
    Ok(())
}

fn run_command_in_dir(cmd: &str, args: &[&str], dir: &Path) -> Result<(), String> {
    util::run_command_env(
        cmd,
        &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        dir,
        &std::env::vars().collect(),
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// eco lxs publish
// ─────────────────────────────────────────────────────────────────────────────

pub(crate) fn parse_lxs_ref(ref_str: &str) -> Result<(String, Option<String>), String> {
    let mut parts = ref_str.rsplitn(2, '@');
    let tail = parts.next().unwrap_or("").trim().to_string();
    let head = parts.next().map(|s| s.trim().to_string());
    match head {
        // No `@` present: only one part, the whole string is the name.
        None => {
            if tail.is_empty() {
                Err(format!(
                    "invalid LXS reference: {ref_str} (expected name[@version])"
                ))
            } else {
                Ok((tail, None))
            }
        }
        Some(name) => {
            if name.is_empty() || tail.is_empty() {
                Err(format!(
                    "invalid LXS reference: {ref_str} (expected name[@version])"
                ))
            } else {
                Ok((name, Some(tail)))
            }
        }
    }
}

pub(crate) fn load_manifest(path: &Path) -> Result<LxsManifest, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_yaml::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))
}

fn run_lxs_publish(args: &[String]) -> Result<(), String> {
    let mut source: Option<PathBuf> = None;
    let mut reference = String::new();
    let mut bump_major = false;
    let mut bump_minor = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--source" => {
                source = Some(PathBuf::from(args.get(i + 1).cloned().unwrap_or_default()));
                i += 2;
            }
            "--major" => {
                bump_major = true;
                i += 1;
            }
            "--minor" => {
                bump_minor = true;
                i += 1;
            }
            other if other.starts_with("--") => {
                return Err(format!("Unknown eco lxs publish option: {other}"))
            }
            other => {
                reference = other.to_string();
                i += 1;
            }
        }
    }
    if bump_major && bump_minor {
        return Err("--major and --minor are mutually exclusive.".to_string());
    }
    if reference.is_empty() {
        return Err(
            "usage: eco lxs publish <name>[@<version>] [--source <dir>] [--minor|--major]"
                .to_string(),
        );
    }
    // Version is optional: when omitted (just `<name>`), auto-bump the latest
    // published version — patch by default, or minor/major with the flag.
    let (name, explicit_version) = match reference.rsplit_once('@') {
        Some((n, v)) => (n.to_string(), Some(v.to_string())),
        None => (reference.clone(), None),
    };
    if name.is_empty() {
        return Err("usage: eco lxs publish <name>[@<version>]".to_string());
    }
    if explicit_version.is_some() && (bump_major || bump_minor) {
        return Err(
            "--major/--minor auto-bump only when no explicit version is supplied; use `eco lxs publish <name> --minor` or publish an explicit new version without a bump flag."
                .to_string(),
        );
    }
    let source = source.unwrap_or_else(util::current_dir);
    if !source.is_dir() {
        return Err(format!("source dir not found: {}", source.display()));
    }

    let registry = registry_root()?;
    let version = match explicit_version {
        Some(v) => v,
        None => {
            match pick_latest(&list_versions_local(&registry.join(&name))) {
                // First publish: start at 0.1.0 (no bump on an empty registry).
                None => "0.1.0".to_string(),
                Some(latest_v) => {
                    let base = parse_semver(&latest_v).unwrap_or(SemVer {
                        major: 0,
                        minor: 1,
                        patch: 0,
                    });
                    let bumped = if bump_major {
                        SemVer {
                            major: base.major + 1,
                            minor: 0,
                            patch: 0,
                        }
                    } else if bump_minor {
                        SemVer {
                            major: base.major,
                            minor: base.minor + 1,
                            patch: 0,
                        }
                    } else {
                        SemVer {
                            major: base.major,
                            minor: base.minor,
                            patch: base.patch + 1,
                        }
                    };
                    let v = format!("{}.{}.{}", bumped.major, bumped.minor, bumped.patch);
                    println!("[eco lxs] Auto-bumped version: {} -> {}", latest_v, v);
                    v
                }
            }
        }
    };

    let mut manifest = load_manifest(&source.join("lxs.yml"))?;
    validate_env_fields(&manifest.contract.env).map_err(|e| format!("publish lint: {e}"))?;
    if !manifest.name.is_empty() && manifest.name != name {
        return Err(format!(
            "lxs.yml name ({}) does not match reference ({name})",
            manifest.name
        ));
    }
    if manifest.name.is_empty() {
        manifest.name = name.clone();
    }
    manifest.version = version.clone();
    manifest.publisher = if manifest.publisher.is_empty() {
        "stuff8".to_string()
    } else {
        manifest.publisher
    };
    if manifest.status.is_empty() {
        manifest.status = "unverified".to_string();
    }
    if manifest.runtime.base.is_empty() {
        manifest.runtime.base = "self-contained-static".to_string();
    }
    if manifest.runtime.libc.is_empty() {
        manifest.runtime.libc = "musl".to_string();
    }

    let artifacts_json = std::fs::read_to_string(source.join(ARTIFACTS_FILE)).map_err(|_| {
        format!(
            "{}/{} missing — run `eco lxs build` first",
            source.display(),
            ARTIFACTS_FILE
        )
    })?;
    let artifacts_map: serde_json::Value =
        serde_json::from_str(&artifacts_json).map_err(|e| e.to_string())?;
    let binaries = artifacts_map
        .get("binaries")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    if binaries.is_empty() {
        return Err(format!(
            "{}/{} has no binaries — run `eco lxs build` first",
            source.display(),
            ARTIFACTS_FILE
        ));
    }

    // Resolve the crate commit + origin for provenance.
    let commit = git(&["rev-parse".to_string(), "HEAD".to_string()], &source).unwrap_or_default();
    let origin = git_repo_origin(&source);
    manifest.provenance.source = if manifest.provenance.source.is_empty() {
        origin
    } else {
        manifest.provenance.source
    };
    manifest.provenance.commit = commit;
    manifest.provenance.built_by = format!("eco@{}", env!("CARGO_PKG_VERSION"));
    manifest.provenance.built_at = now_rfc3339();

    let version_dir = registry.join(&name).join(&version);
    // Published versions are immutable. Rebuilding a binary is not an excuse
    // to mutate a release that existing estates may already have verified and
    // pinned; publish a new patch/minor/major instead.
    if version_dir.exists() {
        return Err(format!(
            "refusing to overwrite existing published version {name}@{version}; LXS releases are immutable. Publish a new version (for example `eco lxs publish {name}` or `eco lxs publish {name} --minor`)."
        ));
    }
    std::fs::create_dir_all(&version_dir).map_err(|e| e.to_string())?;

    let mut targets = Vec::new();
    for (arch, binary) in &binaries {
        let binary_str = binary
            .as_str()
            .ok_or_else(|| format!("artifact map entry for {arch} is not a string path"))?;
        let binary_path = Path::new(binary_str);
        if !binary_path.is_file() {
            return Err(format!("binary missing for {arch}: {binary_str}"));
        }
        let short = arch.replace('/', "-");
        let dest_dir = version_dir.join(&short);
        std::fs::create_dir_all(&dest_dir).map_err(|e| e.to_string())?;
        let is_windows = arch.starts_with("windows/");
        let artifact_name = if is_windows {
            format!("{name}.exe")
        } else {
            name.clone()
        };
        let dest = dest_dir.join(&artifact_name);
        std::fs::copy(binary_path, &dest)
            .map_err(|e| format!("copy {} -> {}: {e}", binary_path.display(), dest.display()))?;
        let size = std::fs::metadata(&dest).map_err(|e| e.to_string())?.len();
        let digest = sha256_file(&dest)?;
        manifest.artifacts.insert(
            arch.clone(),
            LxsArtifact {
                path: format!("{short}/{artifact_name}"),
                sha256: digest,
                size,
            },
        );
        targets.push(arch.clone());
        println!(
            "[eco lxs] packaged {arch}: {} ({size} bytes)",
            dest.display()
        );
    }
    manifest.targets = targets;

    // LXS docs bundle — copied into the registry version dir so consumers
    // (humans and AI agents) get the interface even though they only receive
    // the binary. Required files enforced here; the domain README.md documents
    // the same contract.
    let required_docs = ["README.md", "api.md", "changelog.md"];
    let docs_dir = source.join("docs");
    let mut missing_docs = Vec::new();
    for f in required_docs {
        if !docs_dir.join(f).is_file() {
            missing_docs.push(f.to_string());
        }
    }
    if !docs_dir.is_dir() || !missing_docs.is_empty() {
        return Err(format!(
            "LXS docs bundle incomplete: missing {} — every LXS must ship docs/README.md, docs/api.md, docs/changelog.md (and ideally docs/examples.sh, docs/openapi.json, docs/gotchas.md) so consumers who only get the binary can still work with it",
            if missing_docs.is_empty() { "docs/ dir".to_string() } else { missing_docs.join(", ") }
        ));
    }
    let docs_dest = version_dir.join("docs");
    std::fs::create_dir_all(&docs_dest)
        .map_err(|e| format!("create {}: {e}", docs_dest.display()))?;
    copy_crate_source(&docs_dir, &docs_dest)?;
    let mut docs_files: Vec<String> = std::fs::read_dir(&docs_dir)
        .map_err(|e| format!("read {}: {e}", docs_dir.display()))?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    docs_files.sort();
    let docs_count = docs_files.len();
    manifest.docs = docs_files;
    println!(
        "[eco lxs] bundled docs ({docs_count}) into {}",
        version_dir.join("docs").display()
    );

    if !manifest.release.contains(&version) {
        manifest.release.push(version.clone());
    }

    let manifest_yaml = serde_yaml::to_string(&manifest).map_err(|e| e.to_string())?;
    std::fs::write(version_dir.join("lxs.yml"), manifest_yaml).map_err(|e| e.to_string())?;

    let tag = format!("{name}-{version}");
    let git_args = [
        vec!["add".to_string(), format!("{name}/{version}")],
        vec![
            "-c".to_string(),
            "user.name=Eko SW".to_string(),
            "-c".to_string(),
            "user.email=576066+swdevbali@users.noreply.github.com".to_string(),
            "commit".to_string(),
            "-m".to_string(),
            format!("publish {name}@{version}"),
        ],
    ];
    for args in git_args {
        git(&args, &registry)?;
    }
    // Tag only if the tag doesn't already exist.
    let tag_exists = git(
        &["tag".to_string(), "-l".to_string(), tag.clone()],
        &registry,
    )
    .ok()
    .map(|out| !out.is_empty())
    .unwrap_or(false);
    if !tag_exists {
        git(&["tag".to_string(), tag.clone()], &registry)?;
    }

    println!(
        "[eco lxs] Published {name}@{version} to {} (tag {tag})",
        registry.display()
    );
    println!(
        "[eco lxs] Push with: git -C {} push --tags",
        registry.display()
    );
    Ok(())
}

pub(crate) fn now_rfc3339() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let days = secs / 86400;
    let (year, month, day) = civil_from_days(days as i64);
    let rem = secs % 86400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ─────────────────────────────────────────────────────────────────────────────
// eco lxs search / list / pull / verify
// ─────────────────────────────────────────────────────────────────────────────

fn collect_lxs(registry: &Path) -> Result<Vec<LxsManifest>, String> {
    let mut out = Vec::new();
    if !registry.is_dir() {
        return Ok(out);
    }
    for name_entry in std::fs::read_dir(registry).map_err(|e| e.to_string())? {
        let name_entry = name_entry.map_err(|e| e.to_string())?;
        let name_dir = name_entry.path();
        if !name_dir.is_dir()
            || ["node_modules", ".git"].contains(&name_entry.file_name().to_string_lossy().as_ref())
        {
            continue;
        }
        for version_entry in std::fs::read_dir(&name_dir).map_err(|e| e.to_string())? {
            let version_entry = version_entry.map_err(|e| e.to_string())?;
            let version_dir = version_entry.path();
            if !version_dir.is_dir() {
                continue;
            }
            let manifest_path = version_dir.join("lxs.yml");
            if manifest_path.is_file() {
                if let Ok(m) = load_manifest(&manifest_path) {
                    out.push(m);
                }
            }
        }
    }
    out.sort_by(|a, b| {
        format!("{}-{}", a.name, a.version).cmp(&format!("{}-{}", b.name, b.version))
    });
    Ok(out)
}

/// Resolve the latest available version of an LXS from the registry, without
/// downloading the artifact. Returns (name, latest_version, Some(current) if
/// an address/pinned check applies). Used by `eco up`'s update check and by
/// `eco lxs update`.
pub fn latest_available_version(
    name: &str,
    address: Option<&str>,
) -> Result<Option<String>, String> {
    let target = resolve_registry_target(address)?;
    let versions = match target {
        RegistryTarget::Local(dir) => list_versions_local(&dir.join(name)),
        RegistryTarget::Github { owner, repo, token } => {
            list_versions_github(&owner, &repo, &token, name).unwrap_or_default()
        }
    };
    Ok(pick_latest(&versions))
}

/// Parse a `name@version` reference into (name, Some(version)).
pub fn parse_pinned_ref(reference: &str) -> (String, Option<String>) {
    match reference.rsplit_once('@') {
        Some((n, v)) => (n.to_string(), Some(v.to_string())),
        None => (reference.to_string(), None),
    }
}

/// Fetch the changelog for a specific name@version from the registry (local
/// path or GitHub). Returns the raw markdown; empty when unavailable.
pub fn changelog_for_version(
    name: &str,
    version: &str,
    address: Option<&str>,
) -> Result<String, String> {
    let target = resolve_registry_target(address)?;
    match target {
        RegistryTarget::Local(dir) => {
            let path = dir
                .join(name)
                .join(version)
                .join("docs")
                .join("changelog.md");
            Ok(std::fs::read_to_string(&path).unwrap_or_default())
        }
        RegistryTarget::Github { owner, repo, token } => {
            let url = github_raw_url(
                &owner,
                &repo,
                &format!("{name}/{version}/docs/changelog.md"),
            );
            Ok(http_get_text(&url, &token).unwrap_or_default())
        }
    }
}

/// Extract the changelog section(s) for versions strictly newer than `from`,
/// as a compact "what changed" note. Sections look like `## 1.1.0 — title`.
pub fn changelog_note(name: &str, latest: &str, from: &str, address: Option<&str>) -> String {
    let raw = changelog_for_version(name, latest, address).unwrap_or_default();

    let from_semver = parse_semver(from);
    let mut note = String::new();
    let mut current_header: Option<String> = None;
    for line in raw.lines() {
        let t = line.trim_end();
        if let Some(header) = t.strip_prefix("## ") {
            let version_part = header.split([' ', '—', '-']).next().unwrap_or("").trim();
            let v = parse_semver(version_part);
            let relevant = match (&v, &from_semver) {
                (Some(nv), Some(fv)) => nv > fv,
                (Some(_), None) => true,
                _ => false,
            };
            if relevant {
                current_header = Some(header.to_string());
                note.push_str(&format!("\n{}", header));
            } else {
                current_header = None;
            }
            continue;
        }
        if current_header.is_some() && !t.is_empty() {
            note.push_str(&format!("\n  {}", t.trim_start()));
        }
    }
    note.trim().to_string()
}

/// List every composed `lxs:` service as (service_name, lxs_name, pinned_ref).
pub fn composed_lxs(content: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let mut in_services = false;
    let mut current_service = String::new();
    for raw in content.lines() {
        let line = raw.trim_end_matches('\r').trim_end();
        if line.trim_start().starts_with('#') {
            continue;
        }
        if line == "services:" {
            in_services = true;
            continue;
        }
        if in_services
            && !line.is_empty()
            && !line.starts_with(' ')
            && !line.starts_with('\t')
            && line.contains(':')
        {
            break;
        }
        if !in_services {
            continue;
        }
        if let Some(name) = match_indented_key_2(line) {
            current_service = name;
            continue;
        }
        if !current_service.is_empty() {
            if let Some(lxs_val) = match_indented_value_4(line, "lxs") {
                let (lxs_name, pinned) = parse_pinned_ref(&lxs_val);
                if let Some(pinned) = pinned {
                    out.push((
                        current_service.clone(),
                        lxs_name.clone(),
                        format!("{lxs_name}@{pinned}"),
                    ));
                }
                current_service.clear();
            }
        }
    }
    out
}

/// Check every `lxs:` service declared in ecompose.yml content against the
/// registry and return Vec<(service_name, pinned, latest)> where latest is a
/// newer version. Fails silently (returns empty) when the registry is
/// unreachable so `eco up` never blocks on an offline check.
pub fn lxs_updates_available(
    content: &str,
    address: Option<&str>,
) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let mut in_services = false;
    let mut current_service = String::new();
    for raw in content.lines() {
        let line = raw.trim_end_matches('\r').trim_end();
        if line.trim_start().starts_with('#') {
            continue;
        }
        if line == "services:" {
            in_services = true;
            continue;
        }
        if in_services
            && !line.is_empty()
            && !line.starts_with(' ')
            && !line.starts_with('\t')
            && line.contains(':')
        {
            break;
        }
        if !in_services {
            continue;
        }
        if let Some(name) = match_indented_key_2(line) {
            current_service = name;
            continue;
        }
        if !current_service.is_empty() {
            if let Some(lxs_val) = match_indented_value_4(line, "lxs") {
                let (lxs_name, pinned) = parse_pinned_ref(&lxs_val);
                if let Some(pinned) = pinned {
                    if let Ok(Some(latest)) = latest_available_version(&lxs_name, address) {
                        if latest != pinned
                            && parse_semver(&latest)
                                .map(|l| {
                                    l > parse_semver(&pinned).unwrap_or(SemVer {
                                        major: 0,
                                        minor: 0,
                                        patch: 0,
                                    })
                                })
                                .unwrap_or(false)
                        {
                            out.push((
                                current_service.clone(),
                                format!("{lxs_name}@{pinned}"),
                                format!("{lxs_name}@{latest}"),
                            ));
                        }
                    }
                }
                current_service.clear();
            }
        }
    }
    out
}

fn match_indented_key_2(line: &str) -> Option<String> {
    if line.len() >= 2 && line.starts_with("  ") && !line.starts_with("    ") {
        if let Some(rest) = line.trim_start().strip_suffix(':') {
            let key = rest.trim();
            if !key.is_empty()
                && key
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.')
            {
                return Some(key.to_string());
            }
        }
    }
    None
}

fn match_indented_value_4(line: &str, key: &str) -> Option<String> {
    let prefix = format!("    {key}:");
    if let Some(rest) = line.trim_end().strip_prefix(prefix.as_str()) {
        let v = rest.trim().trim_matches('"').trim_matches('\'').to_string();
        if !v.is_empty() {
            return Some(v);
        }
    }
    None
}

/// Resolve just an LXS manifest (contract schema), without fetching any
/// binary — used by `eco config` to render configuration forms.
pub fn fetch_lxs_manifest(
    reference: &str,
    address: Option<&str>,
) -> Result<(LxsManifest, String), String> {
    let (name, pinned_version) = parse_lxs_ref(reference)?;
    let target = resolve_registry_target(address)?;
    match &target {
        RegistryTarget::Local(dir) => {
            let versions = list_versions_local(&dir.join(&name));
            let version = match &pinned_version {
                Some(v) => v.clone(),
                None => pick_latest(&versions).ok_or_else(|| {
                    format!(
                        "no versions found for {name} under {}",
                        dir.join(&name).display()
                    )
                })?,
            };
            let manifest = load_manifest(&dir.join(&name).join(&version).join("lxs.yml"))?;
            Ok((manifest, version))
        }
        RegistryTarget::Github { owner, repo, token } => {
            let version = match &pinned_version {
                Some(v) => v.clone(),
                None => {
                    let versions = list_versions_github(owner, repo, token, &name)?;
                    pick_latest(&versions)
                        .ok_or_else(|| format!("no versions found for {name} in {owner}/{repo}"))?
                }
            };
            let text =
                http_get_github_text(owner, repo, &format!("{name}/{version}/lxs.yml"), token)?;
            let manifest: LxsManifest =
                serde_yaml::from_str(&text).map_err(|e| format!("parse lxs.yml: {e}"))?;
            Ok((manifest, version))
        }
    }
}

pub fn fetch_lxs_to_cache(
    reference: &str,
    arch: &str,
    address: Option<&str>,
) -> Result<(LxsManifest, String, PathBuf), String> {
    let (name, pinned_version) = parse_lxs_ref(reference)?;
    let target = resolve_registry_target(address)?;
    let short = arch.replace("linux/", "linux-");

    // Resolve manifest + version first (cheap: a local read, or an
    // authenticated GitHub contents-API fetch that does not share the raw CDN
    // rate bucket).
    let (manifest, version) = match &target {
        RegistryTarget::Local(dir) => {
            let versions = list_versions_local(&dir.join(&name));
            let version = match &pinned_version {
                Some(v) => v.clone(),
                None => pick_latest(&versions).ok_or_else(|| {
                    format!(
                        "no versions found for {name} under {}",
                        dir.join(&name).display()
                    )
                })?,
            };
            let vdir = dir.join(&name).join(&version);
            let manifest = load_manifest(&vdir.join("lxs.yml"))?;
            (manifest, version)
        }
        RegistryTarget::Github { owner, repo, token } => {
            let version = match &pinned_version {
                Some(v) => v.clone(),
                None => {
                    let versions = list_versions_github(owner, repo, token, &name)?;
                    pick_latest(&versions)
                        .ok_or_else(|| format!("no versions found for {name} in {owner}/{repo}"))?
                }
            };
            let manifest_text =
                http_get_github_text(owner, repo, &format!("{name}/{version}/lxs.yml"), token)?;
            let manifest: LxsManifest =
                serde_yaml::from_str(&manifest_text).map_err(|e| format!("parse lxs.yml: {e}"))?;
            (manifest, version)
        }
    };

    let cache = Path::new(&util::home_dir())
        .join(".cache")
        .join("eco")
        .join("lxs")
        .join(&name)
        .join(&version)
        .join(&short);
    let dest = cache.join(&name);
    let want_sha = manifest.artifacts.get(arch).and_then(|a| {
        if a.sha256.is_empty() {
            None
        } else {
            Some(a.sha256.clone())
        }
    });

    // Cache hit: reuse an existing binary whose checksum matches the manifest
    // instead of re-downloading (avoids GitHub raw CDN rate limits on repeated
    // deploys).
    if dest.is_file() {
        let digest = sha256_file(&dest)?;
        if want_sha
            .as_deref()
            .map(|w| w == digest.as_str())
            .unwrap_or(true)
        {
            return Ok((manifest, version, dest));
        }
    }

    let bytes = match &target {
        RegistryTarget::Local(dir) => {
            let vdir = dir.join(&name).join(&version);
            let artifact = manifest.artifacts.get(arch).ok_or_else(|| {
                format!(
                    "{name}@{version} has no {arch} artifact (targets: {:?})",
                    manifest.targets
                )
            })?;
            std::fs::read(vdir.join(&artifact.path))
                .map_err(|e| format!("read {}: {e}", vdir.join(&artifact.path).display()))?
        }
        RegistryTarget::Github { owner, repo, token } => {
            let artifact = manifest.artifacts.get(arch).ok_or_else(|| {
                format!(
                    "{name}@{version} has no {arch} artifact (targets: {:?})",
                    manifest.targets
                )
            })?;
            http_get_bytes(
                &github_raw_url(owner, repo, &format!("{name}/{version}/{}", artifact.path)),
                token,
            )?
        }
    };

    std::fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
    std::fs::write(&dest, &bytes).map_err(|e| format!("write {}: {e}", dest.display()))?;
    let digest = sha256_file(&dest)?;
    if let Some(want) = want_sha {
        if want != digest {
            return Err(format!("checksum mismatch for {name}@{version} ({arch}): manifest {want} != actual {digest}"));
        }
    }
    Ok((manifest, version, dest))
}
fn collect_lxs_github(owner: &str, repo: &str, token: &str) -> Result<Vec<LxsManifest>, String> {
    let mut out = Vec::new();
    let root_url = github_api_contents_url(owner, repo, "");
    let root_text = http_get_text(&root_url, token)?;
    let root: serde_json::Value =
        serde_json::from_str(&root_text).map_err(|e| format!("parse registry root: {e}"))?;
    let arr = root
        .as_array()
        .ok_or_else(|| format!("expected a directory listing for {owner}/{repo}"))?;
    for item in arr {
        let name = match (
            item.get("type").and_then(|t| t.as_str()),
            item.get("name").and_then(|n| n.as_str()),
        ) {
            (Some("dir"), Some(n)) => n.to_string(),
            _ => continue,
        };
        let versions = list_versions_github(owner, repo, token, &name).unwrap_or_default();
        for v in versions {
            if let Ok(text) = http_get_text(
                &github_raw_url(owner, repo, &format!("{name}/{v}/lxs.yml")),
                token,
            ) {
                if let Ok(m) = serde_yaml::from_str::<LxsManifest>(&text) {
                    out.push(m);
                }
            }
        }
    }
    out.sort_by(|a, b| {
        format!("{}-{}", a.name, a.version).cmp(&format!("{}-{}", b.name, b.version))
    });
    Ok(out)
}

fn collect_lxs_from_target(address: Option<&str>) -> Result<Vec<LxsManifest>, String> {
    match resolve_registry_target(address)? {
        RegistryTarget::Local(dir) => collect_lxs(&dir),
        RegistryTarget::Github { owner, repo, token } => collect_lxs_github(&owner, &repo, &token),
    }
}

fn run_lxs_search(args: &[String]) -> Result<(), String> {
    let mut address: Option<String> = None;
    let mut query = String::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--address" => {
                address = Some(args.get(i + 1).cloned().unwrap_or_default());
                i += 2;
            }
            other if other.starts_with('-') => {
                return Err(format!("Unknown eco lxs search option: {other}"));
            }
            other => {
                query = other.to_lowercase();
                i += 1;
            }
        }
    }
    let all = collect_lxs_from_target(address.as_deref())?;
    let matches: Vec<&LxsManifest> = all
        .iter()
        .filter(|m| {
            query.is_empty()
                || m.name.to_lowercase().contains(&query)
                || m.summary.to_lowercase().contains(&query)
        })
        .collect();
    if matches.is_empty() {
        println!(
            "[eco lxs] No LXS found{}",
            if query.is_empty() {
                String::new()
            } else {
                format!(" matching \"{query}\"")
            }
        );
        return Ok(());
    }
    for m in &matches {
        println!(
            "{:<18} {:<10} {:<12} {}",
            m.name, m.version, m.status, m.summary
        );
    }
    Ok(())
}

fn run_lxs_list(args: &[String]) -> Result<(), String> {
    let mut address: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--address" => {
                address = Some(args.get(i + 1).cloned().unwrap_or_default());
                i += 2;
            }
            other if other.starts_with('-') => {
                return Err(format!("Unknown eco lxs list option: {other}"))
            }
            other => {
                let _ = other;
                i += 1;
            }
        }
    }
    let all = collect_lxs_from_target(address.as_deref())?;
    if all.is_empty() {
        println!("[eco lxs] Registry empty or unreachable");
        return Ok(());
    }
    for m in &all {
        println!(
            "{:<18} {:<10} {:<12} artifacts: {}",
            m.name,
            m.version,
            m.status,
            m.artifacts.len()
        );
    }
    Ok(())
}

fn run_lxs_pull(args: &[String]) -> Result<(), String> {
    let mut address: Option<String> = None;
    let mut reference = String::new();
    let mut arch = "linux/amd64".to_string();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--address" => {
                address = Some(args.get(i + 1).cloned().unwrap_or_default());
                i += 2;
            }
            "--arch" => {
                arch = args.get(i + 1).cloned().unwrap_or_default();
                i += 2;
            }
            other if other.starts_with('-') => {
                return Err(format!("Unknown eco lxs pull option: {other}"))
            }
            other => {
                reference = other.to_string();
                i += 1;
            }
        }
    }
    if reference.is_empty() {
        return Err(
            "usage: eco lxs pull <name>@<version> [--arch linux/amd64] [--address <registry>]"
                .to_string(),
        );
    }
    let (_, version, dest) = fetch_lxs_to_cache(&reference, &arch, address.as_deref())?;
    println!(
        "[eco lxs] Pulled {reference} ({arch}) -> {} [verified] (version {version})",
        dest.display()
    );
    Ok(())
}

fn run_lxs_verify(args: &[String]) -> Result<(), String> {
    let mut address: Option<String> = None;
    let mut reference = String::new();
    let mut arch = "linux/amd64".to_string();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--address" => {
                address = Some(args.get(i + 1).cloned().unwrap_or_default());
                i += 2;
            }
            "--arch" => {
                arch = args.get(i + 1).cloned().unwrap_or_default();
                i += 2;
            }
            other if other.starts_with('-') => {
                return Err(format!("Unknown eco lxs verify option: {other}"))
            }
            other => {
                reference = other.to_string();
                i += 1;
            }
        }
    }
    if reference.is_empty() {
        return Err(
            "usage: eco lxs verify <name>@<version> [--arch linux/amd64] [--address <registry>]"
                .to_string(),
        );
    }
    let (manifest, version, dest) = fetch_lxs_to_cache(&reference, &arch, address.as_deref())?;
    println!(
        "[eco lxs] {reference} verified (v{version}, {} artifacts) -> {}",
        manifest.artifacts.len(),
        dest.display()
    );
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// LXS registry address resolution + remote fetch (no local clone for consumers)
// ─────────────────────────────────────────────────────────────────────────────

fn github_token_from_env() -> String {
    // Prefer the org-scoped PATs this workspace already carries (they are the
    // default credential for all GitHub activity against the getecosphere org),
    // then the conventional ECO_GITHUB_TOKEN / GITHUB_TOKEN, then gh auth.
    // An authenticated raw.githubusercontent fetch raises the LXS-binary
    // download rate limit from ~60/hr to 5000/hr — without it, repeated
    // deploys hit HTTP 429.
    for var in [
        "GITHUB_SWDEV_ECOSPHERE_API_KEY",
        "ECO_GITHUB_API_KEY",
        "ECO_GITHUB_TOKEN",
        "GITHUB_TOKEN",
    ] {
        if let Ok(v) = std::env::var(var) {
            if !v.is_empty() {
                return v;
            }
        }
    }
    if let Ok(out) = util::run_capture(
        "gh",
        &["auth".to_string(), "token".to_string()],
        &util::current_dir(),
    ) {
        if out.code == 0 {
            let t = out.stdout.trim().to_string();
            if !t.is_empty() {
                return t;
            }
        }
    }
    String::new()
}

enum RegistryTarget {
    Local(PathBuf),
    Github {
        owner: String,
        repo: String,
        token: String,
    },
}

fn is_local_path(address: &str) -> bool {
    let a = address.trim();
    a.starts_with('.')
        || a.starts_with('/')
        || a.starts_with('~')
        || a.starts_with("file://")
        || Path::new(a).exists()
}

fn parse_github_owner_repo(address: &str) -> Option<(String, String)> {
    let a = address
        .trim()
        .trim_end_matches(".git")
        .replace("git@github.com:", "")
        .replace("ssh://git@github.com/", "")
        .replace("https://github.com/", "")
        .replace("http://github.com/", "");
    let a = a.trim_end_matches('/');
    let mut parts = a.splitn(2, '/');
    let owner = parts.next()?.trim().to_string();
    let repo = parts.next()?.trim().to_string();
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner, repo))
}

fn expand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        format!("{}/{}", util::home_dir(), rest)
    } else {
        path.to_string()
    }
}

fn resolve_registry_target(address: Option<&str>) -> Result<RegistryTarget, String> {
    match address {
        Some(a) if is_local_path(a) => {
            let p = expand_home(a.trim_start_matches("file://"));
            Ok(RegistryTarget::Local(PathBuf::from(p)))
        }
        Some(a) => {
            let (owner, repo) = parse_github_owner_repo(a)
                .ok_or_else(|| format!("cannot parse registry address: {a} (use owner/repo, a git URL, or a local path)"))?;
            Ok(RegistryTarget::Github {
                owner,
                repo,
                token: github_token_from_env(),
            })
        }
        None => {
            let (owner, repo) = ("getecosphere".to_string(), DEFAULT_REGISTRY.to_string());
            Ok(RegistryTarget::Github {
                owner,
                repo,
                token: github_token_from_env(),
            })
        }
    }
}

fn github_raw_url(owner: &str, repo: &str, path: &str) -> String {
    format!("https://raw.githubusercontent.com/{owner}/{repo}/main/{path}")
}

fn github_api_contents_url(owner: &str, repo: &str, path: &str) -> String {
    format!("https://api.github.com/repos/{owner}/{repo}/contents/{path}")
}

/// Fetch a file's text from GitHub via the contents API (base64), which is
/// not cached by the raw.githubusercontent CDN the way raw URLs are — a raw
/// URL can serve a stale version of a just-pushed manifest for minutes.
fn http_get_github_text(
    owner: &str,
    repo: &str,
    path: &str,
    token: &str,
) -> Result<String, String> {
    let url = github_api_contents_url(owner, repo, path);
    let text = http_get_text(&url, token)?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("parse contents response for {path}: {e}"))?;
    let b64 = value
        .get("content")
        .and_then(|c| c.as_str())
        .ok_or_else(|| format!("GitHub contents response for {path} has no content field"))?;
    use base64::Engine as _;
    let cleaned: String = b64.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&cleaned)
        .map_err(|e| format!("decode base64 for {path}: {e}"))?;
    String::from_utf8(bytes).map_err(|e| format!("utf8 for {path}: {e}"))
}

fn http_get_text(url: &str, token: &str) -> Result<String, String> {
    let mut req = ureq::get(url).set("User-Agent", "eco-cli");
    if !token.is_empty() {
        req = req.set("Authorization", &format!("Bearer {token}"));
    }
    match req.timeout(std::time::Duration::from_secs(30)).call() {
        Ok(resp) => resp.into_string().map_err(|e| format!("read {url}: {e}")),
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            let hint = if code == 404 && token.is_empty() {
                " (not found; private registries need a GITHUB_TOKEN / gh auth login)"
            } else {
                ""
            };
            Err(format!(
                "HTTP {code} fetching {url}: {}{}",
                body.chars().take(160).collect::<String>(),
                hint
            ))
        }
        Err(ureq::Error::Transport(t)) => Err(format!("network error fetching {url}: {t}")),
    }
}

fn http_get_bytes(url: &str, token: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut req = ureq::get(url).set("User-Agent", "eco-cli");
    if !token.is_empty() {
        req = req.set("Authorization", &format!("Bearer {token}"));
    }
    match req.timeout(std::time::Duration::from_secs(180)).call() {
        Ok(resp) => {
            let mut buf = Vec::new();
            resp.into_reader()
                .read_to_end(&mut buf)
                .map_err(|e| format!("read {url}: {e}"))?;
            Ok(buf)
        }
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            Err(format!(
                "HTTP {code} downloading {url}: {}",
                body.chars().take(160).collect::<String>()
            ))
        }
        Err(ureq::Error::Transport(t)) => Err(format!("network error downloading {url}: {t}")),
    }
}

fn list_versions_github(
    owner: &str,
    repo: &str,
    token: &str,
    name: &str,
) -> Result<Vec<String>, String> {
    let url = github_api_contents_url(owner, repo, name);
    let text = http_get_text(&url, token)?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("parse registry listing for {name}: {e}"))?;
    let arr = value
        .as_array()
        .ok_or_else(|| format!("expected a directory listing for {name} in {owner}/{repo}"))?;
    let mut versions = Vec::new();
    for item in arr {
        if item.get("type").and_then(|t| t.as_str()) == Some("dir") {
            if let Some(v) = item.get("name").and_then(|n| n.as_str()) {
                versions.push(v.to_string());
            }
        }
    }
    Ok(versions)
}

fn list_versions_local(dir: &Path) -> Vec<String> {
    let mut versions = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                versions.push(entry.file_name().to_string_lossy().to_string());
            }
        }
    }
    versions
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct SemVer {
    major: u32,
    minor: u32,
    patch: u32,
}

fn parse_semver(v: &str) -> Option<SemVer> {
    let v = v.trim_start_matches('v');
    let mut parts = v.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some(SemVer {
        major,
        minor,
        patch,
    })
}

fn pick_latest(versions: &[String]) -> Option<String> {
    versions
        .iter()
        .filter_map(|v| parse_semver(v).map(|s| (s, v.clone())))
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, v)| v)
}

fn fetch_lxs(
    target: &RegistryTarget,
    name: &str,
    version: Option<&str>,
    arch: &str,
) -> Result<(LxsManifest, String, Vec<u8>), String> {
    match target {
        RegistryTarget::Local(dir) => {
            let versions = list_versions_local(&dir.join(name));
            let version = match version {
                Some(v) => v.to_string(),
                None => pick_latest(&versions).ok_or_else(|| {
                    format!(
                        "no versions found for {name} under {}",
                        dir.join(name).display()
                    )
                })?,
            };
            let vdir = dir.join(name).join(&version);
            let manifest = load_manifest(&vdir.join("lxs.yml"))?;
            let artifact = manifest.artifacts.get(arch).ok_or_else(|| {
                format!(
                    "{name}@{version} has no {arch} artifact (targets: {:?})",
                    manifest.targets
                )
            })?;
            let bytes = std::fs::read(vdir.join(&artifact.path))
                .map_err(|e| format!("read {}: {e}", vdir.join(&artifact.path).display()))?;
            Ok((manifest, version, bytes))
        }
        RegistryTarget::Github { owner, repo, token } => {
            let version = match version {
                Some(v) => v.to_string(),
                None => {
                    let versions = list_versions_github(owner, repo, token, name)?;
                    pick_latest(&versions)
                        .ok_or_else(|| format!("no versions found for {name} in {owner}/{repo}"))?
                }
            };
            let manifest_text =
                http_get_github_text(owner, repo, &format!("{name}/{version}/lxs.yml"), token)?;
            let manifest: LxsManifest =
                serde_yaml::from_str(&manifest_text).map_err(|e| format!("parse lxs.yml: {e}"))?;
            let artifact = manifest.artifacts.get(arch).ok_or_else(|| {
                format!(
                    "{name}@{version} has no {arch} artifact (targets: {:?})",
                    manifest.targets
                )
            })?;
            let bytes = http_get_bytes(
                &github_raw_url(owner, repo, &format!("{name}/{version}/{}", artifact.path)),
                token,
            )?;
            Ok((manifest, version, bytes))
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// eco lxs init-registry — create a local registry repo (git init + contract)
// ─────────────────────────────────────────────────────────────────────────────

fn run_lxs_init_registry(args: &[String]) -> Result<(), String> {
    let folder = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .cloned()
        .unwrap_or_else(|| ".".to_string());
    let dir = Path::new(&folder);
    if dir.exists() && !dir.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let readme = r#"# LXS Registry

A registry of LXS (Linux Services) — versioned executable capabilities that
estates compose via `eco lxs add <name>`. This folder follows the registry
contract so `eco lxs add <name> --address <this-repo>` resolves identically to
the official registry.

## Structure

```
<name>/
  <version>/
    lxs.yml          # manifest: name, version, contract, artifacts, docs
    <arch>/<name>    # static binary, e.g. linux-amd64/<name>
    docs/            # the docs bundle (README.md, api.md, changelog.md, ...)
```

## Publish

```bash
eco lxs build
eco lxs publish <name>          # auto-bumps the patch version
eco lxs publish <name> --minor  # or --major for a breaking change
git push origin main --tags
```

## Consume

```bash
eco lxs add <name>                       # latest from the official registry
eco lxs add <name> --address <this-repo> # from this registry (public or private)
eco lxs add <name>@<version>             # pin a specific version
```
"#;
    std::fs::write(dir.join("README.md"), readme).map_err(|e| format!("write README.md: {e}"))?;
    std::fs::write(dir.join(".gitignore"), "*.tmp\n").map_err(|e| e.to_string())?;
    if !dir.join(".git").exists() {
        run_command(
            "git",
            &["init".to_string(), "-b".to_string(), "main".to_string()],
            dir,
        )?;
        run_command(
            "git",
            &[
                "add".to_string(),
                "README.md".to_string(),
                ".gitignore".to_string(),
            ],
            dir,
        )?;
        run_command(
            "git",
            &[
                "-c".to_string(),
                "user.name=Eko SW".to_string(),
                "-c".to_string(),
                "user.email=swdev.bali@gmail.com".to_string(),
                "commit".to_string(),
                "-m".to_string(),
                "chore: init LXS registry".to_string(),
            ],
            dir,
        )?;
    }
    println!("[eco lxs] Initialized LXS registry at {}/", dir.display());
    println!("  Next: publish an LXS from a domain with `eco lxs publish <name>`, then push with");
    println!("  `git -C {} push origin main --tags`.", dir.display());
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// eco lxs add — compose an LXS into the current estate
// ─────────────────────────────────────────────────────────────────────────────

// Per-project eco state lives in a gitignored `.eco/state.json` beside the
// ecompose.yml, so a checkout cloned by another user is untouched (their own
// copy simply has no state file). It binds a folder to its estate and carries
// the registry the estate composes from (defaults to the official registry).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EstateState {
    pub project: String,
    #[serde(default)]
    pub registry: String,
    #[serde(default)]
    pub updated_at: String,
}

fn estate_state_path(estate_root: &Path) -> PathBuf {
    estate_root.join(".eco").join("state.json")
}

pub fn read_estate_state(estate_root: &Path) -> Option<EstateState> {
    let path = estate_state_path(estate_root);
    if !path.is_file() {
        return None;
    }
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

pub fn write_estate_state(estate_root: &Path, project: &str, registry: &str) -> Result<(), String> {
    let dir = estate_root.join(".eco");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let state = EstateState {
        project: project.to_string(),
        registry: if registry.is_empty() {
            "getecosphere/lxs-registry".to_string()
        } else {
            registry.to_string()
        },
        updated_at: now_rfc3339(),
    };
    let json = serde_json::to_string_pretty(&state).map_err(|e| e.to_string())?;
    std::fs::write(estate_state_path(estate_root), json).map_err(|e| format!("write state: {e}"))
}

fn estate_project_name(estate_root: &Path) -> String {
    std::fs::read_to_string(estate_root.join("ecompose.yml"))
        .ok()
        .and_then(|c| {
            let mut p = String::new();
            for raw in c.split('\n') {
                if let Some(rest) = raw.trim().strip_prefix("project:") {
                    p = rest.trim().trim_matches('"').trim_matches('\'').to_string();
                    break;
                }
            }
            (!p.is_empty()).then_some(p)
        })
        .unwrap_or_else(|| {
            estate_root
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default()
        })
}

fn ensure_estate_state(estate_root: &Path, registry: &str) -> Result<(), String> {
    if read_estate_state(estate_root).is_none() {
        let project = estate_project_name(estate_root);
        let _ = write_estate_state(estate_root, &project, registry);
    }
    Ok(())
}

fn run_lxs_estates(args: &[String]) -> Result<(), String> {
    // Filesystem-driven listing: scan for ecompose.yml + .eco/state.json under
    // the workspace (or an explicit path). Zero-maintenance — `eco lxs add` and
    // `eco init` write the state file, and the listing just finds them.
    let mut base: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--path" => {
                base = Some(PathBuf::from(args.get(i + 1).cloned().unwrap_or_default()));
                i += 2;
            }
            other if other.starts_with('-') => {
                return Err(format!("Unknown eco lxs estates option: {other}"))
            }
            other => {
                base = Some(PathBuf::from(other));
                i += 1;
            }
        }
    }
    let base = base.unwrap_or_else(|| Path::new(&util::home_dir()).join("superapp"));
    if !base.is_dir() {
        return Err(format!("workspace not found at {}", base.display()));
    }
    let mut found = 0;
    if let Ok(entries) = std::fs::read_dir(&base) {
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let ecompose = dir.join("ecompose.yml");
            let state = read_estate_state(&dir);
            if ecompose.is_file() || state.is_some() {
                found += 1;
                let project = state
                    .as_ref()
                    .map(|s| s.project.clone())
                    .unwrap_or_else(|| estate_project_name(&dir));
                let registry = state
                    .as_ref()
                    .map(|s| s.registry.clone())
                    .unwrap_or_else(|| "getecosphere/lxs-registry".to_string());
                println!("{:<20} registry: {}", project, registry);
                println!(
                    "  ecompose: {}",
                    if ecompose.is_file() {
                        ecompose.display().to_string()
                    } else {
                        "(not yet adopted)".to_string()
                    }
                );
            }
        }
    }
    println!("\n{} estate(s) found under {}", found, base.display());
    Ok(())
}

fn find_estate_ecompose(cwd: &Path) -> Result<PathBuf, String> {
    let mut dir = Some(cwd);
    for _ in 0..8 {
        if let Some(d) = dir {
            let candidate = d.join("ecompose.yml");
            if candidate.is_file() {
                return Ok(candidate);
            }
            dir = d.parent();
        } else {
            break;
        }
    }
    Err(format!(
        "No ecompose.yml found from {} (walked up 8 levels) — run \"eco init\" first, or cd into an estate/project directory.",
        cwd.display()
    ))
}

fn insert_service_into_ecompose(manifest_path: &Path, block: &str) -> Result<(), String> {
    let content = std::fs::read_to_string(manifest_path)
        .map_err(|e| format!("read {}: {e}", manifest_path.display()))?;
    let lines: Vec<&str> = content.split('\n').collect();
    let mut svc_idx: Option<usize> = None;
    for (idx, line) in lines.iter().enumerate() {
        if line.trim_end_matches('\r').trim_end() == "services:" {
            svc_idx = Some(idx);
            break;
        }
    }
    let new_content = match svc_idx {
        Some(idx) => {
            let mut end = lines.len();
            for j in (idx + 1)..lines.len() {
                let l = lines[j];
                let trimmed = l.trim_end_matches('\r');
                if !trimmed.trim_start().is_empty()
                    && !trimmed.trim_start().starts_with('#')
                    && !trimmed.starts_with(' ')
                    && !trimmed.starts_with('\t')
                    && trimmed.contains(':')
                {
                    end = j;
                    break;
                }
            }
            let mut out = lines[..end].join("\n");
            out.push_str(block);
            if end < lines.len() {
                out.push('\n');
                out.push_str(&lines[end..].join("\n"));
            }
            out
        }
        None => format!("{}\nservices:{block}", content.trim_end()),
    };
    std::fs::write(manifest_path, new_content)
        .map_err(|e| format!("write {}: {e}", manifest_path.display()))?;
    println!("[eco lxs] Updated {}", manifest_path.display());
    Ok(())
}

fn upsert_service_ref(
    manifest_path: &Path,
    service: &str,
    key: &str,
    value: &str,
) -> Result<bool, String> {
    // Ok(true) = service block already existed (its `key:` ref updated);
    // Ok(false) = a new service block was inserted.
    let content = std::fs::read_to_string(manifest_path)
        .map_err(|e| format!("read {}: {e}", manifest_path.display()))?;
    let lines: Vec<String> = content.split('\n').map(|s| s.to_string()).collect();
    let service_header = format!("  {service}:");
    let mut start: Option<usize> = None;
    let mut end: Option<usize> = None;
    for (idx, line) in lines.iter().enumerate() {
        let t = line.trim_end_matches('\r');
        if start.is_none() && t == service_header {
            start = Some(idx);
            continue;
        }
        if let Some(s) = start {
            if end.is_none() {
                let is_sibling = t.len() >= 2
                    && t.starts_with("  ")
                    && !t.starts_with("    ")
                    && t.ends_with(':');
                let is_top =
                    !t.trim_start().is_empty() && !t.starts_with(' ') && !t.starts_with('\t');
                if (is_sibling && idx > s) || is_top {
                    end = Some(idx);
                    break;
                }
            }
        }
    }
    let (s, e) = match start {
        Some(s) => (s, end.unwrap_or(lines.len())),
        None => {
            let block = format!("\n  {service}:\n    {key}: {value}\n");
            insert_service_into_ecompose(manifest_path, &block)?;
            return Ok(false);
        }
    };

    // Service block exists — update the `key:` line inside it, or add one.
    let mut found = false;
    for (idx, line) in lines.iter().enumerate() {
        if idx > s && idx < e && line.trim_start().starts_with(&format!("{key}:")) {
            found = true;
            break;
        }
    }
    let new_content = if found {
        let mut out_lines = Vec::new();
        for (idx, line) in lines.iter().enumerate() {
            if idx > s && idx < e && line.trim_start().starts_with(&format!("{key}:")) {
                out_lines.push(format!("    {key}: {value}"));
            } else {
                out_lines.push(line.clone());
            }
        }
        out_lines.join("\n")
    } else {
        let mut with_key = lines[..=s].join("\n");
        with_key.push_str(&format!("\n    {key}: {value}"));
        for (idx, line) in lines.iter().enumerate() {
            if idx > s && idx < e {
                with_key.push('\n');
                with_key.push_str(line);
            }
        }
        if e < lines.len() {
            with_key.push('\n');
            with_key.push_str(&lines[e..].join("\n"));
        }
        with_key
    };
    std::fs::write(manifest_path, new_content)
        .map_err(|e| format!("write {}: {e}", manifest_path.display()))?;
    Ok(true)
}

fn add_lxs_service_to_ecompose(service: &str, lxs_ref: &str) -> Result<(), String> {
    let cwd = util::current_dir();
    let manifest_path = find_estate_ecompose(&cwd)?;
    let existed = upsert_service_ref(&manifest_path, service, "lxs", lxs_ref)?;
    if existed {
        println!(
            "[eco lxs] Updated {service} ({lxs_ref}) in {}",
            manifest_path.display()
        );
    } else {
        println!(
            "[eco lxs] Added {service} ({lxs_ref}) to {}",
            manifest_path.display()
        );
    }
    Ok(())
}

fn add_source_service_to_ecompose(service: &str, rel_path: &str) -> Result<(), String> {
    let cwd = util::current_dir();
    let manifest_path = find_estate_ecompose(&cwd)?;
    let existed = upsert_service_ref(&manifest_path, service, "path", rel_path)?;
    if existed {
        println!(
            "[eco lxs] Updated {service} (path: {rel_path}) in {}",
            manifest_path.display()
        );
    } else {
        println!(
            "[eco lxs] Added {service} (path: {rel_path}) to {}",
            manifest_path.display()
        );
    }
    Ok(())
}

/// `eco lxs update [name]` — bump one (or all) composed LXS to the latest
/// available version in ecompose.yml.
fn run_lxs_update(args: &[String]) -> Result<(), String> {
    let mut address: Option<String> = None;
    let mut name_filter: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--address" => {
                address = Some(args.get(i + 1).cloned().unwrap_or_default());
                i += 2;
            }
            other if other.starts_with('-') => {
                return Err(format!("Unknown eco lxs update option: {other}"))
            }
            other => {
                name_filter = Some(other.to_string());
                i += 1;
            }
        }
    }

    let cwd = util::current_dir();
    let manifest_path = find_estate_ecompose(&cwd)?;
    let content = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("read {}: {e}", manifest_path.display()))?;
    let updates = lxs_updates_available(&content, address.as_deref());
    if updates.is_empty() {
        println!(
            "[eco lxs] All composed LXS are up to date in {}",
            manifest_path.display()
        );
        return Ok(());
    }

    let mut any = false;
    for (service, pinned, latest) in updates {
        let lxs_name = parse_pinned_ref(&pinned).0;
        if let Some(filter) = &name_filter {
            if &lxs_name != filter {
                continue;
            }
        }
        upsert_service_ref(&manifest_path, &service, "lxs", &latest)?;
        println!("[eco lxs] Updated {service}: {pinned} -> {latest}");
        any = true;
    }
    if !any {
        if let Some(filter) = name_filter {
            return Err(format!(
                "No update available for LXS \"{filter}\" in {}",
                manifest_path.display()
            ));
        }
    }
    println!("\nRun `eco up --remote` to deploy the updated LXS binaries.");
    Ok(())
}

/// `eco lxs outdated [--address <registry>]` — show the composed LXS with
/// their current vs latest registry version and a short changelog note.
/// Independent of `eco up`; reports when everything is already up to date.
fn run_lxs_outdated(args: &[String]) -> Result<(), String> {
    let mut address: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--address" => {
                address = Some(args.get(i + 1).cloned().unwrap_or_default());
                i += 2;
            }
            other if other.starts_with('-') => {
                return Err(format!("Unknown eco lxs outdated option: {other}"))
            }
            _ => {
                i += 1;
            }
        }
    }

    let cwd = util::current_dir();
    let manifest_path = find_estate_ecompose(&cwd)?;
    let content = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("read {}: {e}", manifest_path.display()))?;
    let composed = composed_lxs(&content);
    if composed.is_empty() {
        println!(
            "[eco lxs] No composed LXS found in {}",
            manifest_path.display()
        );
        return Ok(());
    }

    println!("[eco lxs] Composed LXS in {}:\n", manifest_path.display());
    let mut outdated_count = 0;
    for (service, name, pinned) in &composed {
        let from_v = pinned.split('@').nth(1).unwrap_or("");
        match latest_available_version(name, address.as_deref()) {
            Ok(Some(latest))
                if latest != from_v && parse_semver(&latest) > parse_semver(from_v) =>
            {
                outdated_count += 1;
                util::println_stdout(&format!(
                    "  {}  \x1b[1;33m{} -> {}\x1b[0m",
                    service,
                    pinned,
                    format!("{name}@{latest}")
                ));
                let note = changelog_note(name, &latest, from_v, address.as_deref());
                if !note.is_empty() {
                    for line in note.lines() {
                        util::println_stdout(&format!("     {}", line));
                    }
                }
                util::println_stdout(&format!("     update: `eco lxs update {name}`\n"));
            }
            Ok(Some(latest)) => {
                util::println_stdout(&format!(
                    "  {}  {}  (\x1b[0;32mup to date\x1b[0m, latest {name}@{latest})",
                    service, pinned
                ));
            }
            _ => {
                util::println_stdout(&format!(
                    "  {}  {}  (latest unknown — registry unreachable)",
                    service, pinned
                ));
            }
        }
    }

    if outdated_count == 0 {
        util::println_stdout("\n[eco lxs] All composed LXS are up to date.");
    } else {
        util::println_stdout(&format!(
            "\n[eco lxs] {outdated_count} update(s) available. Run `eco lxs update [name]` to bump, then `eco up --remote`."
        ));
    }
    Ok(())
}

/// `eco lxs remove <name>` — remove a composed LXS service block from
/// ecompose.yml.
fn run_lxs_remove(args: &[String]) -> Result<(), String> {
    let name = args.iter().find(|a| !a.starts_with('-')).cloned().ok_or_else(|| {
        "usage: eco lxs remove <name>\nRemoves the composed LXS service (e.g. auth-backend) from ecompose.yml.".to_string()
    })?;

    let cwd = util::current_dir();
    let manifest_path = find_estate_ecompose(&cwd)?;
    let content = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("read {}: {e}", manifest_path.display()))?;
    let lines: Vec<String> = content.split('\n').map(|s| s.to_string()).collect();

    // Find the service block: a 2-space `name:` line; capture through the next
    // sibling (2-space key) or a top-level key or EOF.
    let header = format!("  {name}:");
    let mut start: Option<usize> = None;
    let mut end: Option<usize> = None;
    for (idx, line) in lines.iter().enumerate() {
        let t = line.trim_end_matches('\r');
        if start.is_none() && t == header {
            start = Some(idx);
            continue;
        }
        if let Some(s) = start {
            if end.is_none() && idx > s {
                let is_sibling = t.len() >= 2
                    && t.starts_with("  ")
                    && !t.starts_with("    ")
                    && t.ends_with(':');
                let is_top =
                    !t.trim_start().is_empty() && !t.starts_with(' ') && !t.starts_with('\t');
                if is_sibling || is_top {
                    end = Some(idx);
                    break;
                }
            }
        }
    }
    let Some(s) = start else {
        return Err(format!(
            "No service named \"{name}\" in {}",
            manifest_path.display()
        ));
    };
    let e = end.unwrap_or(lines.len());
    // Trim a trailing blank line that belonged to the removed block.
    let mut new_lines = Vec::new();
    new_lines.extend_from_slice(&lines[..s]);
    let mut tail_start = e;
    while tail_start < lines.len() && lines[tail_start].trim().is_empty() {
        tail_start += 1;
    }
    new_lines.extend_from_slice(&lines[tail_start..]);

    // Also drop this service from every `estates.<name>.services:` list and
    // from a top-level `auth:` / `<name>:` config block (auth-backend removal
    // should not leave a dangling auth: email_verification section).
    let mut filtered = Vec::new();
    let mut in_services_list = false;
    let mut in_config_block: Option<String> = None;
    let mut i = 0;
    while i < new_lines.len() {
        let line = new_lines[i].clone();
        let t = line.trim_end_matches('\r');
        // top-level config block for the removed lxs (e.g. `auth:`)
        if let Some(rest) = t.strip_suffix(':') {
            let key = rest.trim();
            if key == name && !line.starts_with(' ') && !line.starts_with('\t') {
                in_config_block = Some(key.to_string());
                i += 1;
                continue;
            }
        }
        if let Some(_blk) = &in_config_block {
            // skip this block's indented lines until a top-level key
            if t.is_empty() {
                filtered.push(line.clone());
            } else if line.starts_with(' ') || line.starts_with('\t') {
                // skip (part of the config block)
            } else {
                in_config_block = None;
                continue; // re-process this line as a normal line
            }
            i += 1;
            continue;
        }
        // services list entry
        if t.trim() == format!("- {name}") {
            i += 1;
            continue;
        }
        // detect `services:` under an estates block by scanning: we simply
        // remove `- <name>` lines under any indented `services:` that follows
        // a top-level `estates:`. A simple approach: remember when a line is
        // exactly `    services:` (4-space, under estates.<x>) and drop
        // `      - <name>` entries until dedent.
        if t == "    services:" {
            in_services_list = true;
            filtered.push(line.clone());
            i += 1;
            continue;
        }
        if in_services_list {
            if t.starts_with("      ") {
                if t.trim() == format!("- {name}") {
                    i += 1;
                    continue;
                }
                filtered.push(line.clone());
                i += 1;
                continue;
            } else {
                in_services_list = false;
                continue;
            }
        }
        filtered.push(line.clone());
        i += 1;
    }

    let mut new_content = filtered.join("\n");
    // Collapse multiple trailing blank lines at the very end.
    while new_content.ends_with("\n\n") {
        new_content.pop();
    }
    new_content = new_content.trim_end().to_string();
    new_content.push('\n');

    std::fs::write(&manifest_path, new_content)
        .map_err(|e| format!("write {}: {e}", manifest_path.display()))?;
    println!("[eco lxs] Removed {name} from {}", manifest_path.display());
    Ok(())
}

fn run_lxs_add(args: &[String]) -> Result<(), String> {
    let mut address: Option<String> = None;
    let mut arch = "linux/amd64".to_string();
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--address" => {
                address = Some(args.get(i + 1).cloned().unwrap_or_default());
                i += 2;
            }
            "--arch" => {
                arch = args.get(i + 1).cloned().unwrap_or_default();
                i += 2;
            }
            other if other.starts_with('-') => {
                return Err(format!("Unknown eco lxs add option: {other}"))
            }
            other => {
                positional.push(other.to_string());
                i += 1;
            }
        }
    }

    if positional.first().map(|s| s.as_str()) == Some(".") {
        return run_lxs_add_source();
    }

    let reference = positional.first().cloned().ok_or_else(|| {
        "usage: eco lxs add <name>[@<version>] [--address <registry>]  |  eco lxs add .".to_string()
    })?;

    let (name, pinned_version) = match reference.rsplit_once('@') {
        Some((n, v)) => (n.to_string(), Some(v.to_string())),
        None => (reference.clone(), None),
    };
    if name.is_empty() {
        return Err("usage: eco lxs add <name>[@<version>]".to_string());
    }

    // Resolve the estate + its configured registry (state file wins when no
    // --address is given).
    let cwd = util::current_dir();
    let manifest_path = find_estate_ecompose(&cwd)?;
    let estate_root = manifest_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| cwd.clone());
    let state_registry = read_estate_state(&estate_root)
        .map(|s| s.registry)
        .filter(|r| !r.is_empty());
    let effective_address = address.as_deref().or(state_registry.as_deref());

    let effective_ref = match &pinned_version {
        Some(v) => format!("{name}@{v}"),
        None => name.clone(),
    };
    let (_, version, dest) = fetch_lxs_to_cache(&effective_ref, &arch, effective_address)?;
    println!(
        "[eco lxs] Added {name}@{version} ({arch}) -> {} [verified]",
        dest.display()
    );

    // Naming: an LXS is a backend by default (`<name>-backend`). A frontend
    // LXS (name ends in `-ui`/`-frontend`) keeps its own name so the gateway
    // can route /signin /signup to it and the frontend fallback skips it.
    let service = if name.ends_with("-ui") || name.ends_with("-frontend") {
        name.clone()
    } else {
        format!("{name}-backend")
    };
    add_lxs_service_to_ecompose(&service, &format!("{name}@{version}"))?;
    ensure_estate_state(&estate_root, address.as_deref().unwrap_or(""))?;
    Ok(())
}

fn run_lxs_add_source() -> Result<(), String> {
    let cwd = util::current_dir();
    let manifest_path = find_estate_ecompose(&cwd)?;
    let source_dir = cwd
        .canonicalize()
        .map_err(|e| format!("resolve cwd: {e}"))?;
    if !source_dir.join("lxs.yml").is_file() {
        return Err(format!(
            "{} has no lxs.yml — this folder is not an LXS yet. Run `eco lxs new <name>` to scaffold one.",
            source_dir.display()
        ));
    }
    let manifest = load_manifest(&source_dir.join("lxs.yml"))?;
    if manifest.name.is_empty() {
        return Err("lxs.yml has no name:".to_string());
    }
    let name = manifest.name.clone();
    let manifest_dir = manifest_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| cwd.clone());
    let estate_root = manifest_dir;
    let rel = source_dir
        .strip_prefix(&estate_root)
        .map_err(|_| format!("{} is outside the estate root {} — a source LXS must live inside the estate (or be cloned as a sibling).", source_dir.display(), estate_root.display()))?;
    let rel_str = rel.to_string_lossy().replace('\\', "/");
    let service = format!("{name}-backend");
    add_source_service_to_ecompose(&service, &rel_str)?;
    println!("[eco lxs] Registered {name} as a source LXS at {rel_str} for this estate.");
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// eco lxs help
// ─────────────────────────────────────────────────────────────────────────────

pub fn run_lxs(args: &[String]) -> Result<(), String> {
    let subcommand = args.first().map(|s| s.as_str()).unwrap_or("help");
    match subcommand {
        "build" => run_lxs_build(&args[1..]),
        "publish" => run_lxs_publish(&args[1..]),
        "search" => run_lxs_search(&args[1..]),
        "list" | "ls" => run_lxs_list(&args[1..]),
        "pull" => run_lxs_pull(&args[1..]),
        "verify" => run_lxs_verify(&args[1..]),
        "add" => run_lxs_add(&args[1..]),
        "update" => run_lxs_update(&args[1..]),
        "outdated" => run_lxs_outdated(&args[1..]),
        "remove" | "rm" => run_lxs_remove(&args[1..]),
        "estates" => run_lxs_estates(&args[1..]),
        "init-registry" => run_lxs_init_registry(&args[1..]),
        "new" | "init" => run_lxs_new(&args[1..]),
        "help" | "-h" | "--help" => {
            println!("eco lxs\n\nLXS (Linux Service) — versioned executable capabilities.\n\nUsage:\n  eco lxs new <name>                       scaffold a domain repo from a template\n  eco lxs build [path] [--arch linux/amd64,linux/arm64,darwin/arm64,darwin/amd64,windows/amd64]\n  eco lxs publish <name>[@<version>] [--source <dir>] [--minor|--major]  (auto patch bump)\n  eco lxs add <name>[@<version>] [--address <registry>]   compose an LXS binary\n  eco lxs add .                            register the current folder as a source LXS\n  eco lxs update [name] [--address <registry>]   bump composed LXS to the latest\n  eco lxs outdated [--address <registry>]   show composed LXS vs latest (+ changelog)\n  eco lxs remove <name>                 remove a composed LXS service from ecompose.yml\n  eco lxs estates                          list estates on this machine\n  eco lxs init-registry [folder]           create a registry repo (git init + contract)\n  eco lxs search [query]\n  eco lxs list\n  eco lxs pull <name>@<version> [--arch linux/amd64]\n  eco lxs verify <name>@<version>\n");
            Ok(())
        }
        other => Err(format!(
            "Unknown eco lxs subcommand: {other}\n\nRun \"eco lxs help\" for usage."
        )),
    }
}

// Scaffolds a new domain repo from a template (backend crate + lxs.yml contract
// + the tag-triggered CI publish workflow), so contributors start from a
// working capability, not a blank directory.
fn run_lxs_new(args: &[String]) -> Result<(), String> {
    let mut name = String::new();
    let mut skip_git = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-git" => {
                skip_git = true;
                i += 1;
            }
            other if other.starts_with('-') => {
                return Err(format!("Unknown eco lxs new option: {other}"))
            }
            other => {
                name = other.to_string();
                i += 1;
            }
        }
    }
    if name.is_empty() {
        return Err("usage: eco lxs new <name>".to_string());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(
            "domain name must be lowercase alphanumeric with dashes (e.g. notifications)"
                .to_string(),
        );
    }
    let dir = Path::new(&name);
    if dir.exists() {
        return Err(format!("{} already exists", dir.display()));
    }
    std::fs::create_dir_all(dir.join("backend/src")).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(dir.join(".github/workflows")).map_err(|e| e.to_string())?;

    // backend/Cargo.toml — a minimal axum service
    let cargo_toml = format!(
        "[package]\nname = \"{name}-backend\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\naxum = \"0.7\"\ntokio = {{ version = \"1\", features = [\"macros\", \"rt-multi-thread\", \"net\"] }}\nserde = {{ version = \"1\", features = [\"derive\"] }}\n"
    );
    let main_rs = format!(
        "use axum::{{routing::get, Router, Json}};\nuse serde::Serialize;\n\n#[derive(Serialize)]\nstruct Status {{ ok: bool, service: &'static str }}\n\nasync fn health() -> Json<Status> {{ Json(Status {{ ok: true, service: \"{name}\" }}) }}\n\n#[tokio::main]\nasync fn main() {{\n    let port: u16 = std::env::var(\"PORT\").ok().and_then(|p| p.parse().ok()).unwrap_or(8270);\n    let app = Router::new().route(\"/health\", get(health));\n    let listener = tokio::net::TcpListener::bind((\"0.0.0.0\", port)).await.unwrap();\n    println!(\"[{name}] listening on :{{port}}\");\n    axum::serve(listener, app).await.unwrap();\n}}\n"
    );
    std::fs::write(dir.join("backend/Cargo.toml"), cargo_toml).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("backend/src/main.rs"), main_rs).map_err(|e| e.to_string())?;

    // lxs.yml — the contract
    let lxs = format!(
        "name: {name}\ndomain: {name}\nversion: 0.1.0\ncategory: Infrastructure\npublisher: getecosphere\nstatus: unverified\nlicense: mit\nsummary: \"{name} — describe your capability\"\n\ntargets:\n  - linux/amd64\n  - linux/arm64\n  - darwin/arm64\n  - darwin/amd64\n  - windows/amd64\n\nartifacts: {{}}\n\ncontract:\n  version: 1\n  api: \"{name} REST API\"\n  env:\n    required:\n      - SERVER_PORT\n    optional: []\n  db: \"none\"\n  network:\n    inbound: [http]\n    outbound: []\n  resources:\n    memory: \"128m\"\n    disk: \"256m\"\n    startup_seconds: 5\n\nruntime:\n  base: self-contained-static\n  libc: musl\n  dependencies: []\n\nprovenance:\n  source: \"\"\n  commit: \"\"\n  built_by: \"\"\n  built_at: \"\"\n  target: x86_64-unknown-linux-musl\n\nrelease: []\n"
    );
    std::fs::write(dir.join("lxs.yml"), lxs).map_err(|e| e.to_string())?;

    // .github/workflows/lxs-publish.yml — tag-triggered CI publish. Runs on a
    // macOS runner so darwin targets build with the platform toolchain and
    // musl-Linux/Windows-GNU go through zig — one build produces every LXS
    // artifact (mac, windows, linux).
    let workflow = r##"name: publish-lxs
on:
  push:
    tags:
      - 'v*'
permissions:
  contents: write
jobs:
  publish:
    runs-on: macos-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: x86_64-unknown-linux-musl,aarch64-unknown-linux-musl,aarch64-apple-darwin,x86_64-apple-darwin,x86_64-pc-windows-gnu
      - name: Install zig + cargo-zigbuild
        run: |
          curl -fsSL https://ziglang.org/download/0.13.0/zig-macos-aarch64-0.13.0.tar.xz -o /tmp/zig.tar.xz
          tar xf /tmp/zig.tar.xz -C /tmp
          echo "/tmp/zig-macos-aarch64-0.13.0" >> "$GITHUB_PATH"
          cargo install cargo-zigbuild --locked
      - name: Build the eco CLI
        run: |
          git clone --depth 1 https://github.com/getecosphere/eco.git /tmp/eco
          cd /tmp/eco/rust && cargo build --release
      - name: Build + publish the LXS for all OS
        env:
          REGISTRY_TOKEN: ${{ secrets.GETECOSPHERE_TOKEN }}
        run: |
          set -euo pipefail
          REPO="${GITHUB_REPOSITORY##*/}"
          V="${GITHUB_REF_NAME#v}"
          git clone "https://x-access-token:${REGISTRY_TOKEN}@github.com/getecosphere/lxs-registry.git" /tmp/reg
          export ECO_LXS_REGISTRY=/tmp/reg
          /tmp/eco/rust/target/release/eco lxs build --arch linux/amd64,linux/arm64,darwin/arm64,darwin/amd64,windows/amd64
          /tmp/eco/rust/target/release/eco lxs publish "${REPO}@${V}"
          git -C /tmp/reg push origin main --tags
"##;
    std::fs::write(dir.join(".github/workflows/lxs-publish.yml"), workflow)
        .map_err(|e| e.to_string())?;

    let readme = format!(
        "# {name}\n\n## What this LXS owns\n- <list the entities and responsibilities this capability owns>\n\n## What this LXS must NEVER own\n- <list anything that belongs to other domains>\n\n## Contracts (public API)\n- see `docs/api.md` for the full endpoint reference\n\n## Docs\n- `docs/README.md` — agent-facing index + composition example\n- `docs/api.md` — endpoint reference with request/response JSON + errors\n- `docs/changelog.md` — per-version + breaking-change notes\n- `docs/examples.sh` — executable smoke test\n- `docs/openapi.json` — machine-readable API spec\n- `docs/gotchas.md` — production-learned constraints\n\n## Runtime\n- Rust (axum), self-contained static binary (musl)\n\n## Environment variables\n- `SERVER_PORT` — listen port\n\n## Build + publish\n\n```bash\neco lxs build\nVERSION=0.1.0 eco lxs publish {name}@$VERSION\ngit tag v$VERSION && git push --tags\n```\n"
    );
    std::fs::write(dir.join("README.md"), readme).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(".gitignore"), "target/\n").map_err(|e| e.to_string())?;

    // docs/ bundle — required by eco lxs publish; consumers only get the
    // binary + manifest + docs, so the docs ARE the interface (also for AI
    // agents). docs/README.md is the canonical agent-facing index.
    std::fs::create_dir_all(dir.join("docs")).map_err(|e| e.to_string())?;
    let docs_readme = format!(
        "# {name} — LXS docs\n\n## Capability\n\n<one paragraph: what {name} does, what it returns to consumers>\n\n## Compose it\n\n```yaml\n# ecompose.yml\nservices:\n  {name}-backend:\n    lxs: {name}@0.1.0\n    grants:\n      secrets: [SERVER_PORT]   # must cover contract.env.required\n```\n\n## Docs index\n\n- `api.md` — endpoints, request/response JSON, errors\n- `changelog.md` — version history\n- `examples.sh` — executable smoke test\n- `openapi.json` — OpenAPI spec\n- `gotchas.md` — production constraints\n"
    );
    std::fs::write(dir.join("docs/README.md"), docs_readme).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("docs/api.md"), "# API\n\n## Health\n- `GET /health` — liveness\n\n_(fill in every endpoint from the backend code, with JSON request/response examples and error codes)_\n").map_err(|e| e.to_string())?;
    std::fs::write(
        dir.join("docs/changelog.md"),
        "# Changelog\n\n## 0.1.0\n- initial scaffold\n",
    )
    .map_err(|e| e.to_string())?;

    if !skip_git {
        run_command(
            "git",
            &["init".to_string(), "-b".to_string(), "main".to_string()],
            dir,
        )?;
        run_command("git", &["add".to_string(), "-A".to_string()], dir)?;
        run_command(
            "git",
            &[
                "-c".to_string(),
                "user.name=Eko SW".to_string(),
                "-c".to_string(),
                "user.email=swdev.bali@gmail.com".to_string(),
                "commit".to_string(),
                "-m".to_string(),
                format!("chore: scaffold {name} domain from eco lxs new template"),
            ],
            dir,
        )?;
    }

    println!("[eco lxs] Scaffolded LXS domain in {}/", dir.display());
    println!("  backend/    a minimal Rust service (axum)");
    println!("  lxs.yml     the contract — edit it to declare your capability");
    println!("  docs/       the docs bundle (README.md, api.md, changelog.md) — required by eco lxs publish");
    println!("  .github/    tag-triggered CI publish workflow");
    println!("\nNext:\n  cd {name}\n  $EDITOR lxs.yml            # declare the contract\n  eco lxs build              # cross-compile for linux/amd64\n  eco lxs publish {name}@0.1.0\n  git remote add origin <your-repo> && git push -u origin main\n  git tag v0.1.0 && git push --tags   # CI publishes to the LXS Registry");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(r#type: &str, default: &str, secret: bool, managed: &str) -> LxsField {
        LxsField {
            required: false,
            r#type: r#type.to_string(),
            default: default.to_string(),
            description: "".to_string(),
            group: "".to_string(),
            choices: vec![],
            min: 0.0,
            max: 0.0,
            pattern: "".to_string(),
            secret,
            managed: managed.to_string(),
            example: "".to_string(),
        }
    }

    fn auth_env() -> LxsEnv {
        let mut fields = HashMap::new();
        fields.insert(
            "JWT_SECRET".to_string(),
            field("secret", "", true, "shared-jwt"),
        );
        fields.insert(
            "MONGODB_URI".to_string(),
            field("uri", "", false, "mongo-db"),
        );
        fields.insert("SERVER_PORT".to_string(), field("int", "", false, "port"));
        fields.insert(
            "EMAIL_VERIFICATION_REQUIRED".to_string(),
            field("bool", "false", false, ""),
        );
        fields.insert("BREVO_API_KEY".to_string(), field("secret", "", true, ""));
        fields.insert(
            "RATE_LIMIT_AUTH_BURST".to_string(),
            field("int", "5", false, ""),
        );
        LxsEnv {
            fields,
            ..Default::default()
        }
    }

    #[test]
    fn v2_fields_render_defaults_with_config_overlay() {
        let env = auth_env();
        let mut config = HashMap::new();
        config.insert(
            "EMAIL_VERIFICATION_REQUIRED".to_string(),
            "true".to_string(),
        );
        config.insert("RATE_LIMIT_AUTH_BURST".to_string(), "10".to_string());
        let text = build_lxs_env_example(&env, "auth-backend", &config).unwrap();
        assert!(text.contains("EMAIL_VERIFICATION_REQUIRED=true\n"));
        assert!(text.contains("RATE_LIMIT_AUTH_BURST=10\n"));
        assert!(text.contains("EMAIL_VERIFICATION_REQUIRED"));
        // defaults land when config has nothing for the key
        assert!(text.contains("SERVER_PORT=\n"));
        assert!(text.contains("JWT_SECRET=\n"));
    }

    #[test]
    fn v2_unknown_config_key_is_rejected() {
        let env = auth_env();
        let mut config = HashMap::new();
        config.insert("NOT_A_FIELD".to_string(), "x".to_string());
        let err = build_lxs_env_example(&env, "auth-backend", &config).unwrap_err();
        assert!(err.contains("NOT_A_FIELD is not declared"));
    }

    #[test]
    fn v2_secret_config_key_is_rejected() {
        let env = auth_env();
        let mut config = HashMap::new();
        config.insert("BREVO_API_KEY".to_string(), "super-secret".to_string());
        let err = build_lxs_env_example(&env, "auth-backend", &config).unwrap_err();
        assert!(err.contains("secret field"));
    }

    #[test]
    fn v2_managed_config_key_is_rejected() {
        let env = auth_env();
        let mut config = HashMap::new();
        config.insert(
            "MONGODB_URI".to_string(),
            "mongodb://localhost/x".to_string(),
        );
        let err = build_lxs_env_example(&env, "auth-backend", &config).unwrap_err();
        assert!(err.contains("managed by eco (mongo-db)"));
    }

    #[test]
    fn v2_publish_lint_rejects_missing_description_and_bad_enum() {
        let mut fields = HashMap::new();
        fields.insert("SERVER_PORT".to_string(), field("int", "", false, ""));
        let no_desc = LxsEnv {
            fields,
            ..Default::default()
        };
        let err = validate_env_fields(&no_desc).unwrap_err();
        assert!(err.contains("missing description"));

        let mut fields2 = HashMap::new();
        let mut enum_field = field("enum", "", false, "");
        enum_field.description = "login mode".to_string();
        fields2.insert("LOGIN_MODE".to_string(), enum_field);
        let bad_enum = LxsEnv {
            fields: fields2,
            ..Default::default()
        };
        let err = validate_env_fields(&bad_enum).unwrap_err();
        assert!(err.contains("enum requires non-empty choices"));

        let mut ok_env = auth_env();
        for v in ok_env.fields.values_mut() {
            v.description = "d".to_string();
        }
        assert!(validate_env_fields(&ok_env).is_ok());
    }

    #[test]
    fn v1_config_overlay_and_unknown_key_rejected() {
        let env = LxsEnv {
            required: vec!["JWT_SECRET".to_string(), "SERVER_PORT".to_string()],
            optional: vec!["EMAIL_VERIFICATION_REQUIRED".to_string()],
            defaults: {
                let mut m = HashMap::new();
                m.insert(
                    "EMAIL_VERIFICATION_REQUIRED".to_string(),
                    "false".to_string(),
                );
                m
            },
            ..Default::default()
        };
        let mut config = HashMap::new();
        config.insert(
            "EMAIL_VERIFICATION_REQUIRED".to_string(),
            "true".to_string(),
        );
        let text = build_lxs_env_example(&env, "auth-backend", &config).unwrap();
        assert!(text.contains("EMAIL_VERIFICATION_REQUIRED=true\n"));
        assert!(text.contains("JWT_SECRET=\n"));

        let mut bad = HashMap::new();
        bad.insert("NOT_DECLARED".to_string(), "x".to_string());
        let err = build_lxs_env_example(&env, "auth-backend", &bad).unwrap_err();
        assert!(err.contains("NOT_DECLARED is not declared"));
    }
}
