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
                "user.name=Eco Creator".to_string(),
                "-c".to_string(),
                "user.email=dev@getecosphere.com".to_string(),
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

pub fn run_update(_args: &[String]) -> Result<(), String> {
    let package_root = embedded::package_root();
    let package_root_str = package_root.display().to_string();
    util::println_stdout(&format!("Updating eco from {package_root_str}"));
    util::run_command("git", &["fetch".to_string(), "origin".to_string()], &package_root)?;
    let cwd = util::current_dir();
    util::run_capture("git", &["reset".to_string(), "--hard".to_string(), "origin/main".to_string()], &package_root)
        .map_err(|e| format!("git reset failed: {e}"))?;
    util::run_capture("git", &["clean".to_string(), "-xfd".to_string()], &package_root)
        .map_err(|e| format!("git clean failed: {e}"))?;
    let _ = cwd;
    util::println_stdout("eco is up to date and clean.");
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
