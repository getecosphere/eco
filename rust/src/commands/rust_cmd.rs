use crate::util;
use std::path::Path;

const IGNORED_DIRS_TARGET: &[&str] = &[".git", "node_modules", "eco"];
const IGNORED_DIRS_HASH: &[&str] = &[".git", "node_modules", "target"];

fn find_target_dirs(root_dir: &Path) -> Vec<String> {
    let mut targets = Vec::new();
    scan_target(root_dir, &mut targets);
    targets
}

fn scan_target(dir: &Path, targets: &mut Vec<String>) {
    let entries = crate::util::sorted_dir_entries(dir);
    for entry in entries {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if IGNORED_DIRS_TARGET.contains(&name.as_str()) {
            continue;
        }
        if name == "target" {
            // Only treat as Rust target if a sibling Cargo.toml/Cargo.lock exists
            // in the same directory (the parent of target/, not inside it).
            let has_cargo = std::fs::read_dir(dir)
                .map(|mut e| e.any(|x| x.as_ref().is_ok_and(|x| x.file_name() == "Cargo.toml" || x.file_name() == "Cargo.lock")))
                .unwrap_or(false);
            if has_cargo {
                targets.push(path.display().to_string());
                continue;
            }
        }
        scan_target(&path, targets);
    }
}

fn find_hash_files(root_dir: &Path) -> Vec<String> {
    let mut hash_files = Vec::new();
    scan_hash(root_dir, &mut hash_files);
    hash_files
}

fn scan_hash(dir: &Path, hash_files: &mut Vec<String>) {
    let entries = crate::util::sorted_dir_entries(dir);
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !IGNORED_DIRS_HASH.contains(&name.as_str()) {
                scan_hash(&path, hash_files);
            }
        } else if entry.file_name().to_string_lossy().as_ref() == ".eco-rust-hash" {
            hash_files.push(path.display().to_string());
        }
    }
}

fn find_estate_root_rust(cwd: &Path) -> Result<String, String> {
    let mut dir = cwd.to_path_buf();
    let mut found = false;
    let mut estate_root = cwd.to_path_buf();
    loop {
        let entries: Vec<std::fs::DirEntry> = std::fs::read_dir(&dir)
            .map(|e| e.flatten().collect())
            .unwrap_or_default();
        let names: Vec<String> = entries
            .iter()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        if names.iter().any(|n| n == "ecompose.yml") {
            let parent_entries = std::fs::read_dir(dir.parent().unwrap_or(&dir))
                .map(|e| e.flatten().map(|x| x.file_name().to_string_lossy().to_string()).collect::<Vec<_>>())
                .unwrap_or_default();
            let parent_has_multiple_repos = parent_entries.iter().filter(|e| !e.starts_with('.')).count() > 3;
            estate_root = if parent_has_multiple_repos {
                dir.parent().unwrap_or(&dir).to_path_buf()
            } else {
                dir.clone()
            };
            found = true;
            break;
        }
        // check one level of subdirectories
        let mut sub_found = false;
        for name in &names {
            let sub = dir.join(name);
            if sub.is_dir() {
                if let Ok(sub_entries) = std::fs::read_dir(&sub) {
                    if sub_entries.flatten().any(|e| e.file_name().to_string_lossy().as_ref() == "ecompose.yml") {
                        estate_root = dir.clone();
                        found = true;
                        sub_found = true;
                        break;
                    }
                }
            }
        }
        if sub_found {
            break;
        }
        let parent = dir.parent().map(|p| p.to_path_buf());
        let Some(parent) = parent else { break };
        if parent == dir {
            break;
        }
        dir = parent;
    }
    if !found {
        return Err("Could not find ecompose.yml. Run from inside an eco project directory.".to_string());
    }
    Ok(estate_root.display().to_string())
}

pub fn run_rust(args: &[String]) -> Result<(), String> {
    let (subcommand, rest) = match args.first() {
        Some(s) => (s.as_str(), &args[1..]),
        None => ("", &args[0..0]),
    };
    match subcommand {
        "" | "help" | "--help" => {
            util::print_stdout(
                "eco rust\n\nUsage:\n  eco rust cleartarget [--dry-run]  Remove all Rust target/ directories and .eco-rust-hash files to force a full recompile on next eco up.\n",
            );
            Ok(())
        }
        "cleartarget" => {
            let dry_run = rest.iter().any(|a| a == "--dry-run");
            let cwd = util::current_dir();
            let estate_root = find_estate_root_rust(&cwd)?;
            util::println_stdout(&format!(
                "{}Clearing Rust build artifacts in: {estate_root}\n",
                if dry_run { "[dry-run] " } else { "" }
            ));

            let target_dirs = find_target_dirs(Path::new(&estate_root));
            let hash_files = find_hash_files(Path::new(&estate_root));

            if target_dirs.is_empty() && hash_files.is_empty() {
                util::println_stdout("Nothing to clean.");
                return Ok(());
            }

            for dir in &target_dirs {
                let rel = dir.trim_start_matches(&format!("{estate_root}/"));
                util::println_stdout(&format!("  {}rm -rf {rel}", if dry_run { "[dry-run] " } else { "" }));
                if !dry_run {
                    let _ = std::fs::remove_dir_all(dir);
                }
            }
            for file in &hash_files {
                let rel = file.trim_start_matches(&format!("{estate_root}/"));
                util::println_stdout(&format!("  {}rm {rel}", if dry_run { "[dry-run] " } else { "" }));
                if !dry_run {
                    let _ = std::fs::remove_file(file);
                }
            }
            util::println_stdout(&format!(
                "\n{}Done. Run eco up to trigger a full recompile.",
                if dry_run { "[dry-run] " } else { "" }
            ));
            Ok(())
        }
        other => Err(format!("Unknown rust subcommand: {other}\n\nRun \"eco rust help\" for usage.")),
    }
}
