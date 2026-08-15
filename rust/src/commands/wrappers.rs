use std::path::{Path, PathBuf};

use crate::embedded;
use crate::util;

fn args_to_strings(args: &[String]) -> Vec<String> {
    args.to_vec()
}

pub fn run_init(args: &[String]) -> Result<(), String> {
    // Modern project model: `eco init <dir>` makes that directory the project
    // root (the only directory eco scans — no `*_core` naming, no sibling
    // discovery). It scaffolds a minimal ecompose.yml + the gitignored
    // .eco/state.json + .gitignore, then git-inits the project.
    let dir_arg = args.iter().find(|a| !a.starts_with('-')).cloned().unwrap_or_else(|| ".".to_string());
    let dir = Path::new(&dir_arg);
    if dir.exists() && !dir.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let project = dir
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "project".to_string());
    let project = if project.is_empty() { "project".to_string() } else { project };

    let ecompose_path = dir.join("ecompose.yml");
    if !ecompose_path.is_file() {
        let ecompose = format!("project: {project}\n\n# services:\n#   <name>-backend:\n#     lxs: <name>@<version>   # a registry LXS\n#     # path: <relative-dir>     # a source LXS in this project\n\n");
        std::fs::write(&ecompose_path, ecompose).map_err(|e| format!("write {}: {e}", ecompose_path.display()))?;
    }

    // .eco/state.json — the gitignored estate binding + registry.
    crate::commands::lxs::write_estate_state(dir, &project, "")?;

    let gitignore = dir.join(".gitignore");
    if !gitignore.is_file() {
        std::fs::write(&gitignore, ".eco/\ntarget/\nnode_modules/\ndist/\n.next/\n.env\n").map_err(|e| format!("write .gitignore: {e}"))?;
    }

    if !dir.join(".git").exists() {
        crate::util::run_command("git", &["init".to_string(), "-b".to_string(), "main".to_string()], dir)?;
        crate::util::run_command("git", &["add".to_string(), "-A".to_string()], dir)?;
        crate::util::run_command(
            "git",
            &[
                "-c".to_string(),
                "user.name=Eko SW".to_string(),
                "-c".to_string(),
                "user.email=swdev.bali@gmail.com".to_string(),
                "commit".to_string(),
                "-m".to_string(),
                "chore: eco init project".to_string(),
            ],
            dir,
        )?;
    }

    println!("[eco] Initialized project {project} in {}/", dir.display());
    println!("  ecompose.yml      the manifest (project root = the only scanned dir)");
    println!("  .eco/state.json   gitignored estate binding + registry");
    println!("\nNext:");
    println!("  cd {dir_arg}");
    println!("  eco lxs add <name>    compose a registry LXS (binary)");
    println!("  cd <your-domain> && eco lxs add .   register a source LXS");
    println!("  eco up --remote       build locally + ship to the target");
    Ok(())
}

pub fn run_configure(args: &[String]) -> Result<(), String> {
    embedded::run_bundled_script("configure.sh", &args_to_strings(args), "estate", &[])
}

pub fn run_provision(args: &[String]) -> Result<(), String> {
    embedded::run_bundled_script("provision.sh", &args_to_strings(args), "workspace", &[])
}

pub fn run_git(args: &[String]) -> Result<(), String> {
    embedded::run_bundled_script("git.sh", &args_to_strings(args), "workspace", &[])
}

pub fn run_tree(args: &[String]) -> Result<(), String> {
    let script_path = embedded::bundled_script_path("tree.sh")?;
    let status = std::process::Command::new("bash")
        .arg(&script_path)
        .args(args)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|e| format!("tree: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("tree exited with code {}", status.code().unwrap_or(-1)))
    }
}

fn update_asset_for_platform() -> Result<String, String> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    match (os, arch) {
        ("macos", "x86_64") => Ok("eco-x86_64-apple-darwin".to_string()),
        ("macos", "aarch64") => Ok("eco-aarch64-apple-darwin".to_string()),
        ("linux", "x86_64") => Ok("eco-x86_64-unknown-linux-musl".to_string()),
        ("windows", "x86_64") => Ok("eco-x86_64-pc-windows-gnu.exe".to_string()),
        _ => Err(format!("eco update has no prebuilt asset for {os}/{arch} yet")),
    }
}

// Extract "v0.3.3" from a GitHub release asset redirect URL, e.g.
//   https://github.com/getecosphere/eco/releases/download/v0.3.3/eco-...
fn tag_from_release_url(url: &str) -> Option<String> {
    let marker = "/releases/download/";
    let idx = url.find(marker)?;
    let rest = &url[idx + marker.len()..];
    let tag = rest.split('/').next().unwrap_or("").to_string();
    if tag.is_empty() { None } else { Some(tag) }
}

// The latest release tag (e.g. "v0.3.2") for getecosphere/eco. Primary source is
// the GitHub releases API; when that is rate-limited or unreachable, falls back
// to following the `releases/latest/download/<asset>` redirect (a 302 whose
// Location embeds the actual tag) so the self-update can still tell "already
// latest" apart from "a newer version is available" without burning API quota.
fn latest_release_tag() -> Result<Option<String>, String> {
    let api_url = "https://api.github.com/repos/getecosphere/eco/releases/latest";
    let api_text = {
        let mut req = ureq::get(api_url).set("User-Agent", "eco-cli");
        match req.timeout(std::time::Duration::from_secs(20)).call() {
            Ok(resp) => resp.into_string().ok(),
            Err(_) => None,
        }
    };
    if let Some(text) = api_text {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(tag) = value.get("tag_name").and_then(|v| v.as_str()) {
                return Ok(Some(tag.to_string()));
            }
        }
    }

    // Fallback: the download redirect for this platform embeds the latest tag.
    let asset = update_asset_for_platform()?;
    let redirect_url = format!("https://github.com/getecosphere/eco/releases/latest/download/{asset}");
    let agent = ureq::AgentBuilder::new().redirects(0).build();
    match agent.get(&redirect_url).set("User-Agent", "eco-cli").call() {
        Ok(resp) => {
            if let Some(location) = resp.header("Location") {
                return Ok(tag_from_release_url(location));
            }
        }
        Err(ureq::Error::Status(302, resp)) => {
            if let Some(location) = resp.header("Location") {
                return Ok(tag_from_release_url(location));
            }
        }
        Err(_) => {}
    }
    Ok(None)
}

// Trivial semver compare of "v0.3.2"-style tags (also accepts the bare
// "0.3.2"). Only numeric dot-separated segments are compared; anything that
// doesn't parse is treated as older, so the update still happens.
fn version_gt(a: &str, b: &str) -> bool {
    fn segments(v: &str) -> Vec<u64> {
        v.trim_start_matches('v')
            .split('.')
            .filter_map(|s| s.parse::<u64>().ok())
            .collect()
    }
    let a = segments(a);
    let b = segments(b);
    let len = a.len().max(b.len());
    for i in 0..len {
        let av = a.get(i).copied().unwrap_or(0);
        let bv = b.get(i).copied().unwrap_or(0);
        if av != bv {
            return av > bv;
        }
    }
    false
}

pub fn run_update(_args: &[String]) -> Result<(), String> {
    let asset = update_asset_for_platform()?;
    let exe = std::env::current_exe().map_err(|e| format!("resolve current executable: {e}"))?;
    let current = env!("CARGO_PKG_VERSION");

    // Report the current version up front, then check whether a newer release
    // exists before downloading anything.
    let latest = latest_release_tag().ok().flatten();
    match &latest {
        Some(tag) if !version_gt(tag, current) => {
            util::println_stdout(&format!("Already in latest version: eco {current}"));
            return Ok(());
        }
        Some(tag) => util::println_stdout(&format!("Updating eco {current} → {} ({asset})", tag.trim_start_matches('v'))),
        None => util::println_stdout(&format!("Updating eco {current} to the latest release ({asset})")),
    }

    let url = format!("https://github.com/getecosphere/eco/releases/latest/download/{asset}");

    // Download the latest release binary for this platform (over HTTP, so the
    // CLI self-updates anywhere — no git clone of the source needed).
    let bytes = {
        let mut req = ureq::get(&url).set("User-Agent", "eco-cli");
        match req.timeout(std::time::Duration::from_secs(180)).call() {
            Ok(resp) => {
                use std::io::Read;
                let mut buf = Vec::new();
                resp.into_reader().read_to_end(&mut buf).map_err(|e| format!("read {url}: {e}"))?;
                buf
            }
            Err(ureq::Error::Status(code, resp)) => {
                let body = resp.into_string().unwrap_or_default();
                return Err(format!("update failed (HTTP {code}): {}", body.chars().take(160).collect::<String>()));
            }
            Err(ureq::Error::Transport(t)) => return Err(format!("update network error: {t}")),
        }
    };

    // Write to a sibling temp + rename over the running binary (rename is safe
    // on Unix/Windows while the process runs — the old inode stays alive).
    let tmp = exe.with_extension(format!("{}.new", exe.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default()));
    std::fs::write(&tmp, &bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        let _ = std::fs::set_permissions(&tmp, perms);
    }
    std::fs::rename(&tmp, &exe).map_err(|e| format!("replace {}: {e}", exe.display()))?;
    match &latest {
        Some(tag) => util::println_stdout(&format!("eco updated to {} — version {}. Restart any running eco to use it.", tag.trim_start_matches('v'), tag.trim_start_matches('v'))),
        None => util::println_stdout("eco updated — restart any running eco to use the new version."),
    }
    Ok(())
}

pub fn run_dev(args: &[String]) -> Result<(), String> {
    let subcommand = args.first().cloned();
    match subcommand.as_deref() {
        Some("flushdns") => {
            if util::platform() != "darwin" {
                return Err("eco dev flushdns: only supported on macOS".to_string());
            }
            util::println_stdout("Flushing DNS cache...");
            util::run_command("sudo", &["dscacheutil".to_string(), "-flushcache".to_string()], &util::current_dir())?;
            util::run_command("sudo", &["killall".to_string(), "-HUP".to_string(), "mDNSResponder".to_string()], &util::current_dir())?;
            util::println_stdout("DNS cache flushed.");
            Ok(())
        }
        None | Some("help") | Some("--help") | Some("-h") => {
            util::print_stdout(
                "eco dev - local development utilities\n\nUsage:\n  eco dev flushdns    Flush the macOS DNS cache (runs dscacheutil + mDNSResponder restart)\n",
            );
            Ok(())
        }
        Some(other) => Err(format!(
            "Unknown dev subcommand: {other}\n\nRun \"eco dev help\" for usage."
        )),
    }
}

pub fn run_install(args: &[String]) -> Result<(), String> {
    crate::commands::install::run_install(args)
}

pub fn run_show(args: &[String]) -> Result<(), String> {
    crate::commands::show::run_show(args)
}

pub fn run_sync_staging(args: &[String]) -> Result<(), String> {
    let mut full = vec!["--staging".to_string()];
    full.extend_from_slice(args);
    crate::commands::sync::run_sync(&full)
}

#[allow(dead_code)]
pub fn current_dir() -> PathBuf {
    util::current_dir()
}
