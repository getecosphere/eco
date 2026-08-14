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
        if registry.exists() && std::fs::read_dir(&registry).map(|mut d| d.next().is_some()).unwrap_or(false) {
            let _ = std::fs::remove_dir_all(&registry);
        }
        if let Err(e) = run_command(
            "git",
            &["clone".to_string(), "--quiet".to_string(), "--depth".to_string(), "1".to_string(), LXS_REGISTRY_REMOTE.to_string(), registry.display().to_string()],
            &util::current_dir(),
        ) {
            println!("[eco lxs] WARNING: could not clone the LXS registry from {LXS_REGISTRY_REMOTE}: {e}");
        }
    } else {
        if let Err(e) = git(&["pull".to_string(), "--ff-only".to_string(), "origin".to_string(), "main".to_string()], &registry) {
            println!("[eco lxs] WARNING: could not refresh LXS registry from remote ({e}); using the local clone as-is");
        }
    }
    Ok(())
}


fn arch_to_triple(arch: &str) -> Result<&'static str, String> {
    match arch {
        "linux/amd64" | "amd64" | "x86_64" => Ok("x86_64-unknown-linux-musl"),
        "linux/arm64" | "arm64" | "aarch64" => Ok("aarch64-unknown-linux-musl"),
        other => Err(format!("unsupported LXS target architecture: {other} (use linux/amd64 or linux/arm64)")),
    }
}

fn triple_to_arch(triple: &str) -> String {
    match triple {
        "x86_64-unknown-linux-musl" => "linux/amd64".to_string(),
        "aarch64-unknown-linux-musl" => "linux/arm64".to_string(),
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
        return Err(format!("git {} failed: {}", args.join(" "), result.stderr.trim()));
    }
    Ok(result.stdout.trim().to_string())
}

fn git_repo_origin(cwd: &Path) -> String {
    run_capture("git", &["config".to_string(), "--get".to_string(), "remote.origin.url".to_string()], cwd)
        .map(|r| r.stdout.trim().to_string())
        .unwrap_or_default()
}

// ─────────────────────────────────────────────────────────────────────────────
// Cross-compile toolchain (same as build-release.sh / `eco up --remote`)
// ─────────────────────────────────────────────────────────────────────────────

fn rustup_target_installed(target: &str) -> bool {
    match run_capture("rustup", &["target".to_string(), "list".to_string(), "--installed".to_string()], &util::current_dir()) {
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
    let cache = Path::new(&util::home_dir()).join(".cache").join("eco").join("zig");
    let install_dir = cache.join(format!("zig-{triple}-{ZIG_VERSION}"));
    if install_dir.join("zig").is_file() {
        return Ok(Some(install_dir));
    }
    let tarball = format!("zig-{triple}-{ZIG_VERSION}.tar.xz");
    println!("[eco lxs] Downloading pinned zig {ZIG_VERSION} ({tarball})");
    let _ = std::fs::create_dir_all(&cache);
    let target_path = cache.join(&tarball);
    let url = format!("https://ziglang.org/download/{ZIG_VERSION}/{tarball}");
    run_command("curl", &["-fsSL".to_string(), url, "-o".to_string(), target_path.display().to_string()], &util::current_dir())?;
    run_command("tar", &["xf".to_string(), target_path.display().to_string(), "-C".to_string(), cache.display().to_string()], &util::current_dir())?;
    let _ = std::fs::remove_file(&target_path);
    if !install_dir.join("zig").is_file() {
        return Err(format!("zig extraction did not produce {}", install_dir.display()));
    }
    Ok(Some(install_dir))
}

fn ensure_toolchain(target: &str) -> Result<Option<PathBuf>, String> {
    if !rustup_target_installed(target) {
        println!("[eco lxs] Installing rustup target {target}");
        run_command("rustup", &["target".to_string(), "add".to_string(), target.to_string()], &util::current_dir())?;
    }
    if !util::command_on_path("cargo-zigbuild") {
        println!("[eco lxs] Installing cargo-zigbuild (pinned)");
        run_command("cargo", &["install".to_string(), "cargo-zigbuild".to_string(), "--locked".to_string()], &util::current_dir())?;
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

fn build_crate_for_target(crate_dir: &Path, target: &str, zig_dir: &Option<PathBuf>) -> Result<(String, PathBuf), String> {
    let cargo_text = std::fs::read_to_string(crate_dir.join("Cargo.toml")).map_err(|e| e.to_string())?;
    let package = crate_package_name(&cargo_text).ok_or_else(|| format!("cannot determine package name from {}", crate_dir.display()))?;
    println!("[eco lxs] Cross-compiling {package} for {target}");

    // Build from an isolated, persistent workspace so an LXS build never
    // depends on the surrounding estate's (generated, sometimes stale) Cargo
    // workspace. The domain crate is copied in; the target/ dir persists per
    // domain so incremental builds stay fast; the nearest Cargo.lock is reused
    // for reproducible versions.
    let build_root = Path::new(&util::home_dir()).join(".cache").join("eco").join("lxs-build").join(&package);
    let member_dir = build_root.join(&package);
    let _ = std::fs::remove_dir_all(&member_dir);
    std::fs::create_dir_all(&member_dir).map_err(|e| e.to_string())?;
    copy_crate_source(crate_dir, &member_dir)?;
    if let Some(lock) = nearest_cargo_lock(crate_dir) {
        std::fs::copy(&lock, build_root.join("Cargo.lock")).map_err(|e| format!("copy Cargo.lock: {e}"))?;
    }
    let workspace_toml = format!("# Generated by eco lxs build -- isolated LXS workspace\n[workspace]\nresolver = \"2\"\nmembers = [\"{package}\"]\n");
    std::fs::write(build_root.join("Cargo.toml"), workspace_toml).map_err(|e| e.to_string())?;

    let mut build_env: Vec<(String, String)> = Vec::new();
    if let Some(zig_dir) = zig_dir {
        let path = std::env::var("PATH").unwrap_or_default();
        build_env.push(("PATH".to_string(), format!("{}:{path}", zig_dir.display())));
    }
    let args: Vec<String> = ["zigbuild", "--release", "--target", target].iter().map(|s| s.to_string()).collect();
    let env_map: HashMap<String, String> = {
        let mut m: HashMap<String, String> = std::env::vars().collect();
        for (k, v) in &build_env {
            m.insert(k.clone(), v.clone());
        }
        m
    };
    util::run_command_env("cargo", &args, &build_root, &env_map)?;
    let binary = build_root.join("target").join(target).join("release").join(&package);
    if !binary.is_file() {
        return Err(format!("cross-compiled binary not found: {}", binary.display()));
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
            std::fs::copy(&source, &destination).map_err(|e| format!("copy {}: {e}", source.display()))?;
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
                archs = raw.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                i += 2;
            }
            "--source" => {
                source = Some(PathBuf::from(args.get(i + 1).cloned().unwrap_or_default()));
                i += 2;
            }
            other if other.starts_with('-') => return Err(format!("Unknown eco lxs build option: {other}")),
            other => {
                source = Some(PathBuf::from(other));
                i += 1;
            }
        }
    }
    let source = source.unwrap_or_else(util::current_dir);
    let crate_dir = find_crate_dir(&source).ok_or_else(|| format!("no Cargo crate found under {}", source.display()))?;

    let mut artifacts: HashMap<String, String> = HashMap::new();
    for arch in &archs {
        let target = arch_to_triple(arch)?;
        let zig = ensure_toolchain(target)?;
        let (_, binary) = build_crate_for_target(&crate_dir, target, &zig)?;
        artifacts.insert(arch.clone(), binary.display().to_string());
    }

    let artifacts_json = serde_json::json!({ "arch": archs, "binaries": artifacts }).to_string();
    std::fs::write(source.join(ARTIFACTS_FILE), artifacts_json).map_err(|e| e.to_string())?;
    println!("[eco lxs] Built {} artifact(s) for {}", artifacts.len(), source.display());
    for (arch, binary) in &artifacts {
        println!("  {arch}: {binary}");
    }
    println!("[eco lxs] Artifact map written to {}/{}", source.display(), ARTIFACTS_FILE);
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// eco lxs publish
// ─────────────────────────────────────────────────────────────────────────────

pub(crate) fn parse_lxs_ref(ref_str: &str) -> Result<(String, String), String> {
    let mut parts = ref_str.rsplitn(2, '@');
    let version = parts.next().unwrap_or("").trim().to_string();
    let name = parts.next().unwrap_or("").trim().to_string();
    if name.is_empty() || version.is_empty() {
        return Err(format!("invalid LXS reference: {ref_str} (expected name@version)"));
    }
    Ok((name, version))
}

pub(crate) fn load_manifest(path: &Path) -> Result<LxsManifest, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_yaml::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))
}

fn run_lxs_publish(args: &[String]) -> Result<(), String> {
    let mut source: Option<PathBuf> = None;
    let mut reference = String::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--source" => {
                source = Some(PathBuf::from(args.get(i + 1).cloned().unwrap_or_default()));
                i += 2;
            }
            other if other.starts_with("--") => return Err(format!("Unknown eco lxs publish option: {other}")),
            other => {
                reference = other.to_string();
                i += 1;
            }
        }
    }
    if reference.is_empty() {
        return Err("usage: eco lxs publish <name>@<version> [--source <dir>]".to_string());
    }
    let (name, version) = parse_lxs_ref(&reference)?;
    let source = source.unwrap_or_else(util::current_dir);
    if !source.is_dir() {
        return Err(format!("source dir not found: {}", source.display()));
    }

    let mut manifest = load_manifest(&source.join("lxs.yml"))?;
    if !manifest.name.is_empty() && manifest.name != name {
        return Err(format!("lxs.yml name ({}) does not match reference ({name})", manifest.name));
    }
    if manifest.name.is_empty() {
        manifest.name = name.clone();
    }
    manifest.version = version.clone();
    manifest.publisher = if manifest.publisher.is_empty() { "stuff8".to_string() } else { manifest.publisher };
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
        format!("{}/{} missing — run `eco lxs build` first", source.display(), ARTIFACTS_FILE)
    })?;
    let artifacts_map: serde_json::Value = serde_json::from_str(&artifacts_json).map_err(|e| e.to_string())?;
    let binaries = artifacts_map.get("binaries").and_then(|v| v.as_object()).cloned().unwrap_or_default();
    if binaries.is_empty() {
        return Err(format!("{}/{} has no binaries — run `eco lxs build` first", source.display(), ARTIFACTS_FILE));
    }

    // Resolve the crate commit + origin for provenance.
    let commit = git(&["rev-parse".to_string(), "HEAD".to_string()], &source).unwrap_or_default();
    let origin = git_repo_origin(&source);
    manifest.provenance.source = if manifest.provenance.source.is_empty() { origin } else { manifest.provenance.source };
    manifest.provenance.commit = commit;
    manifest.provenance.built_by = format!("eco@{}", env!("CARGO_PKG_VERSION"));
    manifest.provenance.built_at = now_rfc3339();

    let registry = registry_root()?;
    let version_dir = registry.join(&name).join(&version);
    std::fs::create_dir_all(&version_dir).map_err(|e| e.to_string())?;

    let mut targets = Vec::new();
    for (arch, binary) in &binaries {
        let binary_str = binary.as_str().ok_or_else(|| format!("artifact map entry for {arch} is not a string path"))?;
        let binary_path = Path::new(binary_str);
        if !binary_path.is_file() {
            return Err(format!("binary missing for {arch}: {binary_str}"));
        }
        let short = arch.replace("linux/", "linux-");
        let dest_dir = version_dir.join(&short);
        std::fs::create_dir_all(&dest_dir).map_err(|e| e.to_string())?;
        let dest = dest_dir.join(&name);
        std::fs::copy(binary_path, &dest).map_err(|e| format!("copy {} -> {}: {e}", binary_path.display(), dest.display()))?;
        let size = std::fs::metadata(&dest).map_err(|e| e.to_string())?.len();
        let digest = sha256_file(&dest)?;
        manifest.artifacts.insert(arch.clone(), LxsArtifact { path: format!("{short}/{name}"), sha256: digest, size });
        targets.push(arch.clone());
        println!("[eco lxs] packaged {arch}: {} ({size} bytes)", dest.display());
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
    std::fs::create_dir_all(&docs_dest).map_err(|e| format!("create {}: {e}", docs_dest.display()))?;
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
    println!("[eco lxs] bundled docs ({docs_count}) into {}", version_dir.join("docs").display());

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
            "user.name=Eco Creator".to_string(),
            "-c".to_string(),
            "user.email=dev@getecosphere.com".to_string(),
            "commit".to_string(),
            "-m".to_string(),
            format!("publish {name}@{version}"),
        ],
    ];
    for args in git_args {
        git(&args, &registry)?;
    }
    // Tag only if the tag doesn't already exist.
    let tag_exists = git(&["tag".to_string(), "-l".to_string(), tag.clone()], &registry).ok().map(|out| !out.is_empty()).unwrap_or(false);
    if !tag_exists {
        git(&["tag".to_string(), tag.clone()], &registry)?;
    }

    println!("[eco lxs] Published {name}@{version} to {} (tag {tag})", registry.display());
    println!("[eco lxs] Push with: git -C {} push --tags", registry.display());
    Ok(())
}

fn now_rfc3339() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let secs = now.as_secs();
    let days = secs / 86400;
    let (year, month, day) = civil_from_days(days as i64);
    let rem = secs % 86400;
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rem / 3600, (rem % 3600) / 60, rem % 60)
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
        if !name_dir.is_dir() || ["node_modules", ".git"].contains(&name_entry.file_name().to_string_lossy().as_ref()) {
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
    out.sort_by(|a, b| format!("{}-{}", a.name, a.version).cmp(&format!("{}-{}", b.name, b.version)));
    Ok(out)
}

fn run_lxs_search(args: &[String]) -> Result<(), String> {
    ensure_registry_synced()?;
    let query = args.iter().find(|a| !a.starts_with("--")).cloned().unwrap_or_default().to_lowercase();
    let registry = registry_root()?;
    let all = collect_lxs(&registry)?;
    let matches: Vec<&LxsManifest> = all.iter().filter(|m| query.is_empty() || m.name.to_lowercase().contains(&query) || m.summary.to_lowercase().contains(&query)).collect();
    if matches.is_empty() {
        println!("[eco lxs] No LXS found{} in {}", if query.is_empty() { String::new() } else { format!(" matching \"{query}\"") }, registry.display());
        return Ok(());
    }
    for m in &matches {
        println!("{:<18} {:<10} {:<12} {}", m.name, m.version, m.status, m.summary);
    }
    Ok(())
}

fn run_lxs_list(args: &[String]) -> Result<(), String> {
    let _ = args;
    ensure_registry_synced()?;
    let registry = registry_root()?;
    let all = collect_lxs(&registry)?;
    if all.is_empty() {
        println!("[eco lxs] Registry empty or not found at {}", registry.display());
        return Ok(());
    }
    for m in &all {
        println!("{:<18} {:<10} {:<12} artifacts: {}", m.name, m.version, m.status, m.artifacts.len());
    }
    Ok(())
}

fn run_lxs_pull(args: &[String]) -> Result<(), String> {
    let reference = args.iter().find(|a| !a.starts_with("--")).cloned().unwrap_or_default();
    let arch = args
        .iter()
        .position(|a| a == "--arch")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "linux/amd64".to_string());
    if reference.is_empty() {
        return Err("usage: eco lxs pull <name>@<version> [--arch linux/amd64]".to_string());
    }
    let (name, version) = parse_lxs_ref(&reference)?;
    ensure_registry_synced()?;
    let registry = registry_root()?;
    let manifest_path = registry.join(&name).join(&version).join("lxs.yml");
    let manifest = load_manifest(&manifest_path)?;
    let short = arch.replace("linux/", "linux-");
    let artifact = manifest.artifacts.get(&arch).ok_or_else(|| format!("{name}@{version} has no {arch} artifact (targets: {:?})", manifest.targets))?;
    let src = registry.join(&name).join(&version).join(&artifact.path);
    let cache = Path::new(&util::home_dir()).join(".cache").join("eco").join("lxs").join(&name).join(&version).join(&short);
    std::fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
    let dest = cache.join(&name);
    std::fs::copy(&src, &dest).map_err(|e| format!("copy {} -> {}: {e}", src.display(), dest.display()))?;
    let digest = sha256_file(&dest)?;
    if artifact.sha256.is_empty() || artifact.sha256 != digest {
        return Err(format!("checksum mismatch for {name}@{version} ({arch}): manifest {} != actual {digest}", artifact.sha256));
    }
    println!("[eco lxs] Pulled {name}@{version} ({arch}) -> {} [verified]", dest.display());
    Ok(())
}

fn run_lxs_verify(args: &[String]) -> Result<(), String> {
    let reference = args.iter().find(|a| !a.starts_with("--")).cloned().unwrap_or_default();
    if reference.is_empty() {
        return Err("usage: eco lxs verify <name>@<version>".to_string());
    }
    let (name, version) = parse_lxs_ref(&reference)?;
    ensure_registry_synced()?;
    let registry = registry_root()?;
    let manifest = load_manifest(&registry.join(&name).join(&version).join("lxs.yml"))?;
    let mut ok = true;
    for (arch, artifact) in &manifest.artifacts {
        let path = registry.join(&name).join(&version).join(&artifact.path);
        if !path.is_file() {
            println!("  MISSING {arch}: {}", path.display());
            ok = false;
            continue;
        }
        let digest = sha256_file(&path)?;
        let match_ = !artifact.sha256.is_empty() && artifact.sha256 == digest;
        println!("  {} {} (manifest {})", if match_ { "OK   " } else { "FAIL " }, arch, artifact.sha256);
        if !match_ {
            ok = false;
        }
    }
    if ok {
        println!("[eco lxs] {name}@{version} verified");
        Ok(())
    } else {
        Err(format!("{name}@{version} failed verification"))
    }
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
        "new" | "init" => run_lxs_new(&args[1..]),
        "help" | "-h" | "--help" => {
            println!("eco lxs\n\nLXS (Linux Service) — versioned executable capabilities.\n\nUsage:\n  eco lxs new <name>                       scaffold a domain repo from a template\n  eco lxs build [path] [--arch linux/amd64,linux/arm64]\n  eco lxs publish <name>@<version> [--source <dir>]\n  eco lxs search [query]\n  eco lxs list\n  eco lxs pull <name>@<version> [--arch linux/amd64]\n  eco lxs verify <name>@<version>\n");
            Ok(())
        }
        other => Err(format!("Unknown eco lxs subcommand: {other}\n\nRun \"eco lxs help\" for usage.")),
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
            other if other.starts_with('-') => return Err(format!("Unknown eco lxs new option: {other}")),
            other => {
                name = other.to_string();
                i += 1;
            }
        }
    }
    if name.is_empty() {
        return Err("usage: eco lxs new <name>".to_string());
    }
    if !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err("domain name must be lowercase alphanumeric with dashes (e.g. notifications)".to_string());
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
        "name: {name}\ndomain: {name}\nversion: 0.1.0\ncategory: Infrastructure\npublisher: getecosphere\nstatus: unverified\nlicense: mit\nsummary: \"{name} — describe your capability\"\n\ntargets:\n  - linux/amd64\n\nartifacts: {{}}\n\ncontract:\n  version: 1\n  api: \"{name} REST API\"\n  env:\n    required:\n      - SERVER_PORT\n    optional: []\n  db: \"none\"\n  network:\n    inbound: [http]\n    outbound: []\n  resources:\n    memory: \"128m\"\n    disk: \"256m\"\n    startup_seconds: 5\n\nruntime:\n  base: self-contained-static\n  libc: musl\n  dependencies: []\n\nprovenance:\n  source: \"\"\n  commit: \"\"\n  built_by: \"\"\n  built_at: \"\"\n  target: x86_64-unknown-linux-musl\n\nrelease: []\n"
    );
    std::fs::write(dir.join("lxs.yml"), lxs).map_err(|e| e.to_string())?;

    // .github/workflows/lxs-publish.yml — tag-triggered CI publish
    let workflow = r##"name: publish-lxs
on:
  push:
    tags:
      - 'v*'
permissions:
  contents: write
jobs:
  publish:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: x86_64-unknown-linux-musl
      - name: Install zig + cargo-zigbuild
        run: |
          curl -fsSL https://ziglang.org/download/0.13.0/zig-linux-x86_64-0.13.0.tar.xz -o /tmp/zig.tar.xz
          tar xf /tmp/zig.tar.xz -C /tmp
          echo "/tmp/zig-linux-x86_64-0.13.0" >> "$GITHUB_PATH"
          cargo install cargo-zigbuild --locked
      - name: Build the eco CLI
        run: |
          git clone --depth 1 https://github.com/kelastanpatembok/ecology-ddd.git /tmp/eco
          cd /tmp/eco/rust && cargo build --release
      - name: Build + publish the LXS
        env:
          REGISTRY_TOKEN: ${{ secrets.GETECOSPHERE_TOKEN }}
        run: |
          set -euo pipefail
          REPO="${GITHUB_REPOSITORY##*/}"
          V="${GITHUB_REF_NAME#v}"
          git clone "https://x-access-token:${REGISTRY_TOKEN}@github.com/getecosphere/lxs-registry.git" /tmp/reg
          export ECO_LXS_REGISTRY=/tmp/reg
          /tmp/eco/rust/target/release/eco lxs build
          /tmp/eco/rust/target/release/eco lxs publish "${REPO}@${V}"
          git -C /tmp/reg push origin main --tags
"##;
    std::fs::write(dir.join(".github/workflows/lxs-publish.yml"), workflow).map_err(|e| e.to_string())?;

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
    std::fs::write(dir.join("docs/changelog.md"), "# Changelog\n\n## 0.1.0\n- initial scaffold\n").map_err(|e| e.to_string())?;

    if !skip_git {
        run_command("git", &["init".to_string(), "-b".to_string(), "main".to_string()], dir)?;
        run_command("git", &["add".to_string(), "-A".to_string()], dir)?;
        run_command("git", &["-c".to_string(), "user.name=Eco Creator".to_string(), "-c".to_string(), "user.email=dev@getecosphere.com".to_string(), "commit".to_string(), "-m".to_string(), format!("chore: scaffold {name} domain from eco lxs new template")], dir)?;
    }

    println!("[eco lxs] Scaffolded LXS domain in {}/", dir.display());
    println!("  backend/    a minimal Rust service (axum)");
    println!("  lxs.yml     the contract — edit it to declare your capability");
    println!("  docs/       the docs bundle (README.md, api.md, changelog.md) — required by eco lxs publish");
    println!("  .github/    tag-triggered CI publish workflow");
    println!("\nNext:\n  cd {name}\n  $EDITOR lxs.yml            # declare the contract\n  eco lxs build              # cross-compile for linux/amd64\n  eco lxs publish {name}@0.1.0\n  git remote add origin <your-repo> && git push -u origin main\n  git tag v0.1.0 && git push --tags   # CI publishes to the LXS Registry");
    Ok(())
}
