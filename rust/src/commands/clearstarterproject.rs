use crate::util;
use std::path::{Path, PathBuf};

const STARTER_PATHS: &[(&str, &str, bool)] = &[
    ("frontend/package.json", "frontend package.json", false),
    ("frontend/index.js", "frontend starter server", false),
    ("frontend/index.html", "frontend starter page", false),
    ("frontend/images", "frontend starter images", true),
    ("backend/Cargo.toml", "backend Cargo.toml", false),
    ("backend/src", "backend starter source", true),
];

fn path_exists(p: &Path) -> bool {
    p.exists()
}

fn find_composition_arg(args: &[String]) -> Option<String> {
    args.iter().find(|a| !a.starts_with("--")).cloned()
}

fn find_composition_dir(cwd: &Path) -> Option<PathBuf> {
    let mut dir = cwd.to_path_buf();
    loop {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    if let Some(name) = entry.file_name().to_str() {
                        if name.ends_with("_composition") {
                            return Some(entry.path());
                        }
                    }
                }
            }
        }
        let parent = dir.parent()?.to_path_buf();
        if parent == dir {
            break;
        }
        dir = parent;
    }
    None
}

fn git_status_clean(cwd: &Path) -> Option<bool> {
    let result = util::run_capture(
        "git",
        &["status".to_string(), "--porcelain".to_string()],
        cwd,
    )
    .ok()?;
    if result.code != 0 {
        return None;
    }
    let lines: Vec<String> = result
        .stdout
        .split('\n')
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty())
        .collect();
    let tracked = lines
        .iter()
        .filter(|l| {
            l.starts_with(" M ") || l.starts_with("M ") || l.starts_with(" D ") || l.starts_with("D ")
        })
        .count();
    Some(tracked == 0)
}

fn help_text() {
    let text = r#"eco clearstarterproject [path]

Remove the placeholder starter runtime files from a <project>_composition
repository, leaving the service contract (.gitignore, .env.example) and the
repository itself intact so a real frontend/backend can replace the starter.

Arguments:
  path   path to the composition repository (default: nearest *_composition dir)

Options:
  --commit   commit the removal in the composition repository
  --dry-run  list what would be removed without deleting
"#;
    print!("{text}");
}

pub fn run_clear_starter_project(args: &[String]) -> Result<(), String> {
    if args.iter().any(|a| a == "help" || a == "--help" || a == "-h") {
        help_text();
        return Ok(());
    }
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let commit = args.iter().any(|a| a == "--commit");
    let explicit_path = find_composition_arg(args);

    let cwd = util::current_dir();
    let composition_dir = match explicit_path {
        Some(p) => cwd.join(p),
        None => find_composition_dir(&cwd).ok_or(
            "No <project>_composition directory found. Pass the path or run inside the estate.",
        )?,
    };
    if !path_exists(&composition_dir) {
        return Err(format!(
            "Composition directory not found: {}",
            composition_dir.display()
        ));
    }

    let is_repo = composition_dir.join(".git").exists();
    if is_repo && !dry_run {
        match git_status_clean(&composition_dir) {
            Some(false) => {
                return Err(format!(
                    "{} has uncommitted changes to tracked files. \
Commit or stash them before clearing the starter project.",
                    composition_dir.display()
                ));
            }
            Some(true) => {}
            None => {}
        }
    }

    let mut present: Vec<&(&str, &str, bool)> = Vec::new();
    for entry in STARTER_PATHS {
        if path_exists(&composition_dir.join(entry.0)) {
            present.push(entry);
        }
    }

    let mut out = String::new();
    out.push_str(&format!(
        "\n{}  {}\n",
        util::bold("Eco starter project"),
        util::dim(&composition_dir.display().to_string())
    ));
    if present.is_empty() {
        out.push_str(&format!("  {} — no starter files present.\n\n", util::green("Nothing to clear")));
        print!("{out}");
        return Ok(());
    }

    for entry in &present {
        let verb = if dry_run { "would remove" } else { "removing" };
        out.push_str(&format!("  {verb}  {}\n", util::cyan(entry.0)));
    }
    if dry_run {
        out.push('\n');
        print!("{out}");
        return Ok(());
    }

    for entry in &present {
        let full = composition_dir.join(entry.0);
        let _ = std::fs::remove_dir_all(&full);
        let _ = std::fs::remove_file(&full);
    }

    for service_dir in ["frontend", "backend"] {
        let full = composition_dir.join(service_dir);
        if full.is_dir() {
            let remaining: usize = std::fs::read_dir(&full).map(|e| e.count()).unwrap_or(0);
            if remaining == 0 {
                let _ = std::fs::remove_dir_all(&full);
                out.push_str(&format!("  removing empty   {}\n", util::cyan(&format!("{service_dir}/"))));
            }
        }
    }

    if commit && is_repo {
        let _cwd = util::current_dir();
        let result = util::run_capture(
            "git",
            &["add".to_string(), "-A".to_string()],
            &composition_dir,
        )
        .map_err(|e| format!("git add: {e}"))?;
        if result.code != 0 {
            return Err("git add exited with non-zero".to_string());
        }
        let result = util::run_capture(
            "git",
            &["commit".to_string(), "-m".to_string(), "clear: remove starter project scaffold".to_string()],
            &composition_dir,
        )
        .map_err(|e| format!("git commit: {e}"))?;
        if result.code != 0 {
            return Err(format!("git commit failed: {}", result.stderr.trim()));
        }
        out.push_str(&format!("\n  {} removal in {}\n", util::green("Committed"), composition_dir.display()));
    } else if is_repo && !commit {
        out.push_str(&format!(
            "\n  {}\n",
            util::dim("Commit the removal with: git -C <composition> commit -m \"clear: remove starter scaffold\"")
        ));
    }
    let _ = cwd;
    out.push('\n');
    print!("{out}");
    Ok(())
}
