use std::path::{Path, PathBuf};
use std::process::Command;

use crate::util;

// Embedded bundled assets (mirrors the files shipped with the Node package).
pub const CONFIGURE_SH: &str = include_str!("../../configure.sh");
pub const PROVISION_SH: &str = include_str!("../../provision.sh");
pub const GIT_SH: &str = include_str!("../../git.sh");
pub const TREE_SH: &str = include_str!("../../tree.sh");
pub const INSTALL_MINIO_SH: &str = include_str!("../../install-minio.sh");
pub const INSTALL_ONNXRUNTIME_SH: &str = include_str!("../../install-onnxruntime.sh");
pub const INSTALL_REDIS_SH: &str = include_str!("../../install-redis.sh");
pub const INSTALL_CLOUDFLARED_SH: &str = include_str!("../../install-cloudflared.sh");
pub const ECOLOGY_MARK: &[u8] = include_bytes!("../../assets/ecology-mark.webp");
pub const SVELTEKIT_BUN_RECIPE_MJS: &str = include_str!("../../sveltekit-bun-recipe.mjs");
pub const ASTRO_BUN_RECIPE_MJS: &str = include_str!("../../astro-bun-recipe.mjs");
pub const ECO_BUILDER_LIMA_YML: &str = include_str!("../../scripts/eco-builder.lima.yml");
pub const ECO_BUILDER_BOOTSTRAP_SH: &str = include_str!("../../scripts/eco-builder-bootstrap.sh");

const BUNDLED: &[(&str, &str)] = &[
    ("configure.sh", CONFIGURE_SH),
    ("provision.sh", PROVISION_SH),
    ("git.sh", GIT_SH),
    ("tree.sh", TREE_SH),
    ("install-minio.sh", INSTALL_MINIO_SH),
    ("install-onnxruntime.sh", INSTALL_ONNXRUNTIME_SH),
    ("install-redis.sh", INSTALL_REDIS_SH),
    ("install-cloudflared.sh", INSTALL_CLOUDFLARED_SH),
    ("eco-builder.lima.yml", ECO_BUILDER_LIMA_YML),
    ("eco-builder-bootstrap.sh", ECO_BUILDER_BOOTSTRAP_SH),
];

/// The directory where embedded assets are materialized for execution.
pub fn bundled_root() -> PathBuf {
    let base = util::env_var_or("ECO_BUNDLED_ROOT", "");
    if !base.is_empty() {
        return PathBuf::from(base);
    }
    let cache = util::env_var_or("XDG_CACHE_HOME", &format!("{}/.cache", util::home_dir()));
    PathBuf::from(cache).join("eco").join("bundled")
}

/// Extract embedded scripts to the bundle dir, refreshing content that
/// changed between versions. Returns the bundle dir.
pub fn ensure_bundled() -> Result<PathBuf, String> {
    let root = bundled_root();
    let marker = root.join(".eco-version");
    let version = env!("CARGO_PKG_VERSION");
    let needs_write = std::fs::read_to_string(&marker).map(|v| v.trim() != version).unwrap_or(true)
        || BUNDLED
            .iter()
            .any(|(name, content)| std::fs::read(root.join(name)).map(|b| b != content.as_bytes()).unwrap_or(true));

    if !needs_write {
        // Verify every file exists; if any is missing, rewrite the whole set.
        let all_present = BUNDLED.iter().all(|(name, _)| root.join(name).is_file())
            && root.join("assets").join("ecology-mark.webp").is_file()
            && root.join("bin").join("eco").is_file();
        if all_present {
            return Ok(root);
        }
    }

    std::fs::create_dir_all(&root)
        .map_err(|e| format!("Cannot create bundle dir {}: {e}", root.display()))?;
    std::fs::create_dir_all(root.join("assets"))
        .map_err(|e| format!("Cannot create bundle assets dir: {e}"))?;
    std::fs::create_dir_all(root.join("bin"))
        .map_err(|e| format!("Cannot create bundle bin dir: {e}"))?;
    for (name, content) in BUNDLED {
        std::fs::write(root.join(name), content)
            .map_err(|e| format!("Cannot write {}: {e}", root.join(name).display()))?;
    }
    std::fs::write(root.join("assets").join("ecology-mark.webp"), ECOLOGY_MARK)
        .map_err(|e| format!("Cannot write ecology-mark.webp: {e}"))?;
    // Ship the running binary alongside the bundled scripts so a CT can
    // install it onto its PATH without npm/node (see docs/releasing.md).
    // The running binary is the same platform as the CT during `eco up`,
    // because the host runs the Linux build. Cross-arch is not supported.
    let exe_path = current_exe_path();
    if std::fs::copy(&exe_path, root.join("bin").join("eco")).is_err() {
        eprintln!("warn: could not copy {} into bundle bin/", exe_path);
    }
    std::fs::write(&marker, version).ok();
    Ok(root)
}

/// Write the embedded configure.sh to a target path (used to refresh the CT's
/// /opt/projects/eco/configure.sh from the shipped binary before running it).
pub fn materialize_configure_sh(dest: &str) -> Result<(), String> {
    std::fs::write(dest, CONFIGURE_SH).map_err(|e| format!("write {dest}: {e}"))
}

/// Write every bundled script (configure.sh, provision.sh, git.sh,
/// tree.sh, install-*.sh) into a directory — the CT's eco root — so the CT
/// always runs the shipped script versions, even when its bundle predates the
/// deploy (e.g. provision.sh gains a new runtime token).
pub fn materialize_bundled_scripts(dest: &str) -> Result<(), String> {
    let dir = Path::new(dest);
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    for (name, content) in BUNDLED {
        std::fs::write(dir.join(name), content).map_err(|e| format!("write {}: {e}", dir.join(name).display()))?;
    }
    Ok(())
}

/// Resolve the on-disk path for a bundled script by name (already extracted).
pub fn bundled_script_path(name: &str) -> Result<PathBuf, String> {
    let root = ensure_bundled()?;
    let path = root.join(name);
    if !path.is_file() {
        return Err(format!("Cannot find bundled script: {}", path.display()));
    }
    Ok(path)
}

/// Run a bundled script with bash, mirroring run-bundled-script.js.
/// scope: "estate" resolves ECOLOGY_WORKSPACE_ROOT to the estate root;
/// otherwise to the workspace root.
pub fn run_bundled_script(
    script_name: &str,
    args: &[String],
    scope: &str,
    extra_env: &[(String, String)],
) -> Result<(), String> {
    let original_cwd = util::current_dir();
    let script_path = bundled_script_path(script_name)?;

    let workspace_root = crate::workspace::find_workspace_root(&original_cwd)?;
    let estate_root = crate::workspace::find_estate_root(&original_cwd)?;
    let ecology_root = if scope == "estate" { estate_root } else { workspace_root };

    let mut cmd = Command::new("bash");
    cmd.arg(&script_path);
    cmd.args(args);
    cmd.current_dir(&original_cwd);
    cmd.env("ECOLOGY_WORKSPACE_ROOT", &ecology_root);
    // Point embedded scripts at the eco binary so Node-era helpers resolve.
    cmd.env("ECO_BIN", current_exe_path());
    cmd.env("ECO_BUNDLED_ROOT", bundled_root());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.stdin(std::process::Stdio::inherit());
    cmd.stdout(std::process::Stdio::inherit());
    cmd.stderr(std::process::Stdio::inherit());

    let status = cmd.status().map_err(|e| format!("Cannot run {script_name}: {e}"))?;
    if !status.success() {
        return Err(crate::util::describe_status(script_name, &status));
    }
    Ok(())
}

/// Path to the currently running eco binary (for `$ECO_BIN` / `eco registry`).
pub fn current_exe_path() -> String {
    std::env::current_exe()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "eco".to_string())
}

/// The bundle dir, exposed for callers that need to reference assets
/// directly (e.g. `eco install` reading install-minio.sh).
pub fn package_root() -> PathBuf {
    bundled_root()
}

pub fn path_contains_segment(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == "..")
}
