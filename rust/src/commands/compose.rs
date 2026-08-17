use crate::detect;
use crate::checklist;
use crate::ecompose;
use crate::repos;
use crate::util;
use crate::workspace;
use std::path::{Path, PathBuf};

fn path_exists(p: &Path) -> bool {
    p.exists()
}

fn split_lines(content: &str) -> Vec<String> {
    content.split('\n').map(|s| s.trim_end_matches('\r').to_string()).collect()
}

fn is_top_level_key_line(line: &str) -> bool {
    // A real top-level key starts at column 0 (no leading whitespace).
    let first = line.chars().next();
    match first {
        None => false,
        Some(c) if c.is_whitespace() => false,
        Some('#') => false,
        Some(_) => line.trim_end().ends_with(':'),
    }
}

fn find_block(lines: &[String], key: &str) -> Option<(usize, usize)> {
    let header_index = lines.iter().position(|l| {
        let t = l.trim_end();
        t == format!("{key}:")
    })?;
    let mut end = lines.len();
    for i in (header_index + 1)..lines.len() {
        if is_top_level_key_line(&lines[i]) {
            end = i;
            break;
        }
    }
    Some((header_index, end))
}

fn insertion_point_for(lines: &[String], block: (usize, usize)) -> usize {
    let (header_index, end) = block;
    for i in (header_index + 1..end).rev() {
        let trimmed = lines[i].trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        return i + 1;
    }
    header_index + 1
}

fn domain_already_declared(lines: &[String], repo_name: &str) -> bool {
    if let Some((start, end)) = find_block(lines, "domains") {
        for line in &lines[start + 1..end] {
            let t = line.trim_start();
            if let Some(rest) = t.strip_prefix("- ") {
                let name = rest.split(':').next().unwrap_or("").trim();
                if name == repo_name {
                    return true;
                }
            }
        }
    }
    false
}

pub fn insert_domain(content: &str, repo_name: &str, branch: Option<&str>, dev: Option<&str>) -> String {
    let mut lines = split_lines(content);
    if domain_already_declared(&lines, repo_name) {
        return content.to_string();
    }

    let entry_line = if branch.is_none() && dev.is_none() {
        format!("  - {repo_name}")
    } else {
        let mut l = vec![format!("  - {repo_name}:")];
        if let Some(b) = branch {
            l.push(format!("      branch: {b}"));
        }
        if let Some(d) = dev {
            l.push(format!("      dev: {d}"));
        }
        l.join("\n")
    };

    if let Some(block) = find_block(&lines, "domains") {
        let at = insertion_point_for(&lines, block);
        lines.insert(at, entry_line);
        return lines.join("\n");
    }

    let services_block = find_block(&lines, "services");
    let shared_tools_block = find_block(&lines, "shared_tools");
    let insert_at = if let Some((h, _)) = services_block {
        h
    } else if let Some(block) = shared_tools_block {
        insertion_point_for(&lines, block)
    } else {
        lines.len()
    };
    let mut spliced: Vec<String> = Vec::new();
    spliced.push("domains:".to_string());
    spliced.push(entry_line);
    spliced.push(String::new());
    let mut out = Vec::new();
    out.extend_from_slice(&lines[..insert_at]);
    out.extend(spliced);
    out.extend_from_slice(&lines[insert_at..]);
    out.join("\n")
}

fn service_already_declared(lines: &[String], service_name: &str) -> bool {
    let target = format!("  {service_name}:");
    lines.iter().any(|l| l == &target)
}

pub fn insert_services(content: &str, services: &[detect::DetectedService]) -> (String, Vec<detect::DetectedService>) {
    let mut lines = split_lines(content);
    let new_services: Vec<detect::DetectedService> = services
        .iter()
        .filter(|s| !service_already_declared(&lines, &s.name))
        .cloned()
        .collect();
    if new_services.is_empty() {
        return (content.to_string(), Vec::new());
    }

    let mut rendered: Vec<String> = Vec::new();
    for service in &new_services {
        rendered.push(detect::render_service_block(service));
        rendered.push(String::new());
    }

    if let Some(block) = find_block(&lines, "services") {
        let at = insertion_point_for(&lines, block);
        let mut out = Vec::new();
        out.extend_from_slice(&lines[..at]);
        out.extend(rendered);
        out.extend_from_slice(&lines[at..]);
        (out.join("\n"), new_services)
    } else {
        if !lines.is_empty() && !lines.last().map(|l| l.trim().is_empty()).unwrap_or(true) {
            lines.push(String::new());
        }
        lines.push("services:".to_string());
        lines.extend(rendered);
        (lines.join("\n"), new_services)
    }
}

fn read_git_remote(dir: &Path) -> Option<String> {
    let result = util::run_capture(
        "git",
        &["-C".to_string(), dir.display().to_string(), "remote".to_string(), "get-url".to_string(), "origin".to_string()],
        &util::current_dir(),
    )
    .ok()?;
    if result.code != 0 {
        return None;
    }
    let remote = result.stdout.trim().to_string();
    if remote.is_empty() {
        None
    } else {
        Some(remote)
    }
}

fn find_existing_clone(
    catalog_repo: &repos::RepoEntry,
    estate_root: &Path,
    workspace_root: &Path,
) -> Option<PathBuf> {
    let mut candidates = vec![estate_root.join(&catalog_repo.name)];
    if workspace_root != estate_root {
        candidates.push(workspace_root.join(&catalog_repo.name));
    }
    for candidate in candidates {
        if path_exists(&candidate.join(".git")) {
            if let Some(remote) = read_git_remote(&candidate) {
                if remote == catalog_repo.git {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

struct ResolvedTarget {
    target: String,
    catalog_repo: Option<repos::RepoEntry>,
    service_dir: PathBuf,
    services_label: String,
    needs_clone: bool,
    reused_existing: bool,
}

fn resolve_compose_target(target: &str, estate_root: &Path, workspace_root: &Path) -> Result<ResolvedTarget, String> {
    if let Ok(catalog_repo) = repos::find_repo_by_name(target) {
        if let Some(repo) = catalog_repo {
            let existing = find_existing_clone(&repo, estate_root, workspace_root);
            let service_dir = existing.clone().unwrap_or_else(|| estate_root.join(&repo.name));
            let needs_clone = existing.is_none();
            if needs_clone && path_exists(&service_dir) {
                return Err(format!("Refusing to clone into existing non-git path: {}", service_dir.display()));
            }
            return Ok(ResolvedTarget {
                target: target.to_string(),
                catalog_repo: Some(repo),
                service_dir,
                services_label: target.to_string(),
                needs_clone,
                reused_existing: existing.is_some(),
            });
        }
    }

    let cwd = util::current_dir();
    let mut candidate = cwd.join(target);
    if !path_exists(&candidate) {
        candidate = estate_root.join(target);
    }
    if !path_exists(&candidate) {
        return Err(format!(
            "\"{target}\" is neither a known LXS/domain nor a directory \
(checked relative to the current directory and to the estate root {}).",
            estate_root.display()
        ));
    }
    let relative = candidate.strip_prefix(estate_root).map_err(|_| {
        format!(
            "{} is outside the estate root {} -- move/clone it under there first.",
            candidate.display(),
            estate_root.display()
        )
    })?;
    let rel_str = relative.to_string_lossy().to_string();
    if rel_str.starts_with("..") {
        return Err(format!(
            "{} is outside the estate root {} -- move/clone it under there first.",
            candidate.display(),
            estate_root.display()
        ));
    }
    if rel_str.contains('/') {
        return Err(format!(
            "{} is nested more than one level under {} -- \
eco compose add only auto-detects immediate estate-root subdirectories; \
declare a deeper path by hand in ecompose.yml's services: block instead.",
            candidate.display(),
            estate_root.display()
        ));
    }
    let label = candidate
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| target.to_string());
    Ok(ResolvedTarget {
        target: target.to_string(),
        catalog_repo: None,
        service_dir: candidate,
        services_label: label,
        needs_clone: false,
        reused_existing: false,
    })
}

fn pick_targets_interactively(manifest_content: &str) -> Result<Vec<String>, String> {
    let catalog = repos::read_repo_catalog()?;
    let lines = split_lines(manifest_content);
    let items: Vec<checklist::ChecklistItem> = catalog
        .iter()
        .filter(|repo| repo.name != "eco" && !domain_already_declared(&lines, &repo.name))
        .map(|repo| {
            let desc = if repo.description.is_empty() {
                "No description".to_string()
            } else {
                repo.description.clone()
            };
            checklist::ChecklistItem {
                id: repo.name.clone(),
                label: format!("{} - {desc}", repo.name),
            }
        })
        .collect();

    if items.is_empty() {
        util::println_stdout("Every known domain is already composed into this estate.");
        return Ok(Vec::new());
    }
    let (requires_by, required_by) = checklist::build_repo_dependency_maps(&catalog);
    checklist::run_checklist(
        &items,
        "Select repos to compose into this estate",
        "Controls: ↑/↓ move, x or space toggle, Enter confirm",
        Some(&requires_by),
        Some(&required_by),
        1,
        &[],
        &[],
    )
}

fn run_compose_add(args: &[String]) -> Result<(), String> {
    let yes_flag = args.iter().any(|a| a == "--yes" || a == "-y");
    let positional: Vec<String> = args.iter().filter(|a| !a.starts_with('-')).cloned().collect();
    let target = positional.first().cloned();

    let cwd = util::current_dir();
    let manifest_path = ecompose::resolve_ecompose_file(".", &cwd)?;
    if !path_exists(&manifest_path) {
        return Err(format!(
            "No ecompose.yml found at {} -- run \"eco init\" first to create one.",
            manifest_path.display()
        ));
    }

    let manifest_dir = manifest_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| cwd.clone());
    let estate_root = manifest_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| manifest_dir.clone());
    let workspace_root = workspace::find_workspace_root(&estate_root)?;
    let initial_content = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("Cannot read {}: {e}", manifest_path.display()))?;

    let targets = match target {
        Some(t) => vec![t],
        None => pick_targets_interactively(&initial_content)?,
    };
    if targets.is_empty() {
        return Ok(());
    }

    let mut resolved_targets = Vec::new();
    for one_target in &targets {
        resolved_targets.push(resolve_compose_target(one_target, &estate_root, &workspace_root)?);
    }

    let reused: Vec<_> = resolved_targets.iter().filter(|r| r.reused_existing).collect();
    if !reused.is_empty() {
        let mut out = String::new();
        out.push_str("Already cloned elsewhere in the workspace -- reusing in place, not cloning a duplicate:\n");
        for resolved in &reused {
            out.push_str(&format!("  {} -> {}\n", resolved.catalog_repo.as_ref().map(|c| c.name.clone()).unwrap_or_default(), resolved.service_dir.display()));
        }
        out.push('\n');
        print!("{out}");
    }

    let to_clone: Vec<_> = resolved_targets.iter().filter(|r| r.needs_clone).collect();
    if !to_clone.is_empty() {
        let mut out = String::new();
        out.push_str("Will clone:\n");
        for resolved in &to_clone {
            if let Some(repo) = &resolved.catalog_repo {
                out.push_str(&format!(
                    "  {} ({}, branch {}) -> {}\n",
                    repo.name,
                    repo.git,
                    repo.branch,
                    resolved.service_dir.display()
                ));
            }
        }
        print!("{out}");
        let confirm_clone = yes_flag
            || checklist::confirm_with_single_key(
                &format!("\nClone {} repo(s) into the estate root?", to_clone.len()),
                false,
            )?;
        if !confirm_clone {
            return Err("Cancelled.".to_string());
        }
        for resolved in &to_clone {
            let repo = resolved.catalog_repo.as_ref().ok_or("missing repo")?;
            let cwd = util::current_dir();
            let result = util::run_capture(
                "git",
                &[
                    "clone".to_string(),
                    "--branch".to_string(),
                    repo.branch.clone(),
                    repo.git.clone(),
                    resolved.service_dir.display().to_string(),
                ],
                &cwd,
            )?;
            if result.code != 0 {
                return Err(result.stderr.trim().to_string());
            }
            util::println_stdout(&format!("Cloned {}", repo.name));
        }
        util::println_stdout("");
    }

    let mut content = initial_content.clone();
    let mut domains_to_add: Vec<String> = Vec::new();
    let mut all_added_services: Vec<detect::DetectedService> = Vec::new();

    for resolved in &resolved_targets {
        let services = detect::discover_services_at(&resolved.services_label, &resolved.service_dir);
        let real_rel_path = resolved
            .service_dir
            .strip_prefix(&estate_root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| resolved.services_label.clone());

        if real_rel_path != resolved.services_label {
            let mut adjusted = services.clone();
            for service in adjusted.iter_mut() {
                if service.path == resolved.services_label {
                    service.path = real_rel_path.clone();
                } else if service.path.starts_with(&resolved.services_label) {
                    service.path = format!("{real_rel_path}{}", &service.path[resolved.services_label.len()..]);
                }
            }
            let _ = &mut adjusted;
        }

        if !services.is_empty() {
            let mut out = String::new();
            out.push_str(&format!("Detected services for {}:\n", resolved.target));
            for service in &services {
                let runtimes = if service.runtimes.is_empty() {
                    "(none)".to_string()
                } else {
                    service.runtimes.join(", ")
                };
                out.push_str(&format!("  {} -- path: {}, runtimes: {}\n", service.name, service.path, runtimes));
            }
            print!("{out}");
        } else {
            util::println_stdout(&format!(
                "No services detected for {} (looked for pom.xml/Cargo.toml/package.json).",
                resolved.target
            ));
        }

        if let Some(repo) = &resolved.catalog_repo {
            if !domain_already_declared(&split_lines(&content), &repo.name) {
                domains_to_add.push(repo.name.clone());
            }
        }

        let (with_services, added) = insert_services(&content, &services);
        content = with_services;
        all_added_services.extend(added);
    }

    if domains_to_add.is_empty() && all_added_services.is_empty() {
        util::println_stdout(&format!(
            "\nNothing new to add -- everything selected is already declared in {}.",
            manifest_path.display()
        ));
        return Ok(());
    }

    let mut placements: std::collections::HashMap<String, (Option<String>, String)> = std::collections::HashMap::new();
    if !domains_to_add.is_empty() {
        if yes_flag {
            for name in &domains_to_add {
                placements.insert(name.clone(), (None, "optional".to_string()));
            }
        } else {
            let catalog = repos::read_repo_catalog()?;
            for repo_name in &domains_to_add {
                let repo = catalog.iter().find(|r| &r.name == repo_name);
                let default_branch = repo.map(|r| r.branch.clone()).unwrap_or_else(|| "main".to_string());
                let branch_input = crate::checklist::prompt_line(&format!(
                    "  {repo_name} branch [{default_branch}]: "
                ))?;
                let branch_override = if branch_input.is_empty() || branch_input == default_branch {
                    None
                } else {
                    Some(branch_input)
                };
                let selection = checklist::run_checklist(
                    &[
                        checklist::ChecklistItem { id: "dev".to_string(), label: format!("{repo_name} in local dev (optional)") },
                        checklist::ChecklistItem { id: "prod".to_string(), label: format!("{repo_name} in prod (mandatory)") },
                    ],
                    &format!("  {repo_name} environments"),
                    "  Controls: ↑/↓ move, space toggle, Enter confirm",
                    None,
                    None,
                    1,
                    &["dev".to_string(), "prod".to_string()],
                    &["prod".to_string()],
                )?;
                let dev = if selection.iter().any(|s| s == "dev") { "optional" } else { "disabled" };
                placements.insert(repo_name.clone(), (branch_override, dev.to_string()));
            }
        }
    }

    util::println_stdout("");
    if !domains_to_add.is_empty() {
        util::println_stdout("Will add to domains:");
        for name in &domains_to_add {
            let (branch, dev) = placements.get(name).cloned().unwrap_or((None, "optional".to_string()));
            let branch_note = branch.as_ref().map(|b| format!(" (branch: {b})")).unwrap_or_default();
            let dev_note = if dev == "disabled" {
                " [prod-only]"
            } else if dev == "optional" {
                " [dev: optional]"
            } else {
                ""
            };
            util::println_stdout(&format!("  - {name}{branch_note}{dev_note}"));
        }
    }
    if !all_added_services.is_empty() {
        util::println_stdout(&format!(
            "Will add to services: {}",
            all_added_services.iter().map(|s| s.name.clone()).collect::<Vec<_>>().join(", ")
        ));
    }

    let confirm_write = yes_flag
        || checklist::confirm_with_single_key(&format!("\nUpdate {}?", manifest_path.display()), false)?;
    if !confirm_write {
        return Err("Cancelled.".to_string());
    }

    for name in &domains_to_add {
        let (branch, dev) = placements.get(name).cloned().unwrap_or((None, "optional".to_string()));
        content = insert_domain(&content, name, branch.as_deref(), Some(&dev));
    }

    std::fs::write(&manifest_path, &content)
        .map_err(|e| format!("Cannot write {}: {e}", manifest_path.display()))?;
    util::println_stdout(&format!("Updated {}", manifest_path.display()));
    util::println_stdout(&format!("Next: run \"eco configure\" from {}", manifest_dir.display()));
    Ok(())
}

fn run_compose_refresh(args: &[String]) -> Result<(), String> {
    let yes_flag = args.iter().any(|a| a == "--yes" || a == "-y");
    let positional: Vec<String> = args.iter().filter(|a| !a.starts_with('-')).cloned().collect();
    let target = positional.first().cloned().ok_or("Usage: eco compose refresh <repo-name-or-path> [--yes]")?;

    let cwd = util::current_dir();
    let manifest_path = ecompose::resolve_ecompose_file(".", &cwd)?;
    let manifest_dir = manifest_path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| cwd.clone());
    let estate_root = manifest_dir.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| manifest_dir.clone());
    let workspace_root = workspace::find_workspace_root(&estate_root)?;
    let resolved = resolve_compose_target(&target, &estate_root, &workspace_root)?;
    let services = detect::discover_services_at(&resolved.services_label, &resolved.service_dir);
    if services.is_empty() {
        return Err(format!("No services detected for {target}."));
    }
    let original = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("Cannot read {}: {e}", manifest_path.display()))?;
    // replaceDomainServices: keep services not belonging to this domain, replace the rest
    let mut lines = split_lines(&original);
    let next = if let Some((start, end)) = find_block(&lines, "services") {
        let mut retained: Vec<String> = Vec::new();
        let mut index = start + 1;
        while index < end {
            let start_i = index;
            if !lines[index].starts_with("  ") || lines[index].starts_with("   ") {
                retained.push(lines[index].clone());
                index += 1;
                continue;
            }
            // a service block: "  name:" until next "  name:" or end
            index += 1;
            while index < end && !(lines[index].starts_with("  ") && !lines[index].starts_with("    ")) {
                index += 1;
            }
            let block = &lines[start_i..index];
            let path_line = block.iter().find(|l| l.trim_start().starts_with("path:"));
            let service_path = path_line
                .map(|l| l.trim_start().trim_start_matches("path:").trim().to_string())
                .unwrap_or_default();
            if !service_path.starts_with(&resolved.services_label) && service_path != resolved.services_label {
                retained.extend(block.iter().cloned());
            }
        }
        let mut rendered: Vec<String> = Vec::new();
        for service in &services {
            rendered.push(detect::render_service_block(service));
            rendered.push(String::new());
        }
        let mut out = Vec::new();
        out.extend_from_slice(&lines[..start + 1]);
        out.extend(rendered);
        out.extend(retained);
        out.extend_from_slice(&lines[end..]);
        out.join("\n")
    } else {
        insert_services(&original, &services).0
    };

    util::println_stdout(&format!(
        "Will refresh services for {}: {}",
        resolved.services_label,
        services.iter().map(|s| s.name.clone()).collect::<Vec<_>>().join(", ")
    ));
    let confirmed = yes_flag
        || checklist::confirm_with_single_key(&format!("\nUpdate {}?", manifest_path.display()), false)?;
    if !confirmed {
        return Err("Cancelled.".to_string());
    }
    std::fs::write(&manifest_path, &next).map_err(|e| format!("Cannot write {}: {e}", manifest_path.display()))?;
    util::println_stdout(&format!("Updated {}", manifest_path.display()));
    Ok(())
}

fn run_compose_expose(args: &[String]) -> Result<(), String> {
    let service_name = args.first().cloned();
    let hostname = args.get(1).cloned();
    let (Some(service_name), Some(hostname)) = (service_name, hostname) else {
        return Err("Usage: eco compose expose <service> <hostname>".to_string());
    };
    let cwd = util::current_dir();
    let manifest_path = ecompose::resolve_ecompose_file(".", &cwd)?;
    let original = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("Cannot read {}: {e}", manifest_path.display()))?;

    // insertAdditionalExposure
    let mut lines = split_lines(&original);
    let expose = find_block(&lines, "expose").ok_or("No expose: block exists in this manifest.")?;
    let duplicate = lines[expose.0 + 1..expose.1]
        .iter()
        .any(|l| l.trim() == format!("hostname: {hostname}"));
    let next = if duplicate {
        original.clone()
    } else {
        let rendered = vec![format!("    - hostname: {hostname}"), format!("      service: {service_name}")];
        let additional_index = lines
            .iter()
            .enumerate()
            .find(|(idx, l)| *idx > expose.0 && *idx < expose.1 && l.trim() == "additional:")
            .map(|(i, _)| i);
        let at = match additional_index {
            Some(ai) => insertion_point_for(&lines, (ai, expose.1)),
            None => insertion_point_for(&lines, expose),
        };
        let mut out = Vec::new();
        out.extend_from_slice(&lines[..at]);
        if additional_index.is_none() {
            out.push("  additional:".to_string());
        }
        out.extend(rendered);
        out.extend_from_slice(&lines[at..]);
        out.join("\n")
    };

    if next == original {
        util::println_stdout(&format!("{hostname} is already exposed."));
        return Ok(());
    }
    util::println_stdout(&format!(
        "Will expose {service_name} at {hostname} and make it available to declared PUBLIC_<DOMAIN>_URL consumers."
    ));
    let confirmed = checklist::confirm_with_single_key(&format!("\nUpdate {}?", manifest_path.display()), false)?;
    if !confirmed {
        return Err("Cancelled.".to_string());
    }
    std::fs::write(&manifest_path, &next).map_err(|e| format!("Cannot write {}: {e}", manifest_path.display()))?;
    util::println_stdout(&format!("Updated {}", manifest_path.display()));
    Ok(())
}

pub fn run_compose(args: &[String]) -> Result<(), String> {
    let (subcommand, rest) = match args.first() {
        Some(s) => (s.as_str(), &args[1..]),
        None => ("", &args[0..0]),
    };
    match subcommand {
        "add" => run_compose_add(rest),
        "refresh" => run_compose_refresh(rest),
        "expose" => run_compose_expose(rest),
        _ => Err(format!(
            "Unknown compose subcommand: {subcommand}\n\nUsage: eco compose add|refresh|expose ..."
        )),
    }
}
