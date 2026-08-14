use std::path::PathBuf;

use crate::embedded;
use crate::util;

fn args_to_strings(args: &[String]) -> Vec<String> {
    args.to_vec()
}

pub fn run_init(args: &[String]) -> Result<(), String> {
    embedded::run_bundled_script("init.sh", &args_to_strings(args), "workspace", &[])
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
