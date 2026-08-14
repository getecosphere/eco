use std::path::{Path, PathBuf};

use crate::ecompose;
use crate::util;

/// Find ecompose.yml walking up from start_dir (direct + *_bootstrap / *_core sibling).
fn find_ecompose_file(start_dir: &Path) -> Result<PathBuf, String> {
    let mut dir = std::fs::canonicalize(start_dir).unwrap_or_else(|_| start_dir.to_path_buf());
    loop {
        let direct = dir.join("ecompose.yml");
        if direct.is_file() {
            return Ok(direct);
        }
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    if let Some(name) = entry.file_name().to_str() {
                        if name.ends_with("_bootstrap") || name.ends_with("_core") {
                            let candidate = entry.path().join("ecompose.yml");
                            if candidate.is_file() {
                                return Ok(candidate);
                            }
                        }
                    }
                }
            }
        }
        let parent = match dir.parent() {
            Some(p) => p.to_path_buf(),
            None => break,
        };
        if parent == dir {
            break;
        }
        dir = parent;
    }
    Err("No ecompose.yml found. Run from inside a project directory.".to_string())
}

/// Tolerant extraction of PM2 app name -> port from a generated
/// ecosystem.config.js. The generated file is a CommonJS module of the form
/// module.exports = { apps: [ { name: "..", env: { PORT: N, ... } }, ... ] }.
/// We scan object blocks and pull the first `name:` and any PORT/SERVER_PORT
/// integer inside that block's env.
pub fn read_ports_from_ecosystem(ecosystem_path: &Path) -> std::collections::HashMap<String, String> {
    let mut ports: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let content = match std::fs::read_to_string(ecosystem_path) {
        Ok(c) => c,
        Err(_) => return ports,
    };
    let blocks = split_blocks(&content);
    for (name, block) in blocks {
        if let Some(port) = extract_port_from_block(&block) {
            ports.insert(name, port);
        }
    }
    ports
}
/// Split content into app blocks by `{`/`}` depth tracking starting at each
/// `name:` after an `apps:` array opening. Returns (name, block_text).
fn split_blocks(content: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let chars: Vec<char> = content.chars().collect();
    let mut i = 0usize;
    let n = chars.len();
    // find "apps: [" 
    let mut in_apps = false;
    while i < n {
        if !in_apps {
            let window: String = chars[i..n.min(i + 8)].iter().collect();
            if window.starts_with("apps:") {
                in_apps = true;
                i += 5;
                continue;
            }
            i += 1;
            continue;
        }
        // We're inside apps; find the first `{`
        if chars[i] == '{' {
            // read block balanced
            let mut depth = 1usize;
            let mut j = i + 1;
            while j < n && depth > 0 {
                match chars[j] {
                    '{' => depth += 1,
                    '}' => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
            let block_text: String = chars[i..j.min(n)].iter().collect();
            let name = extract_name(&block_text);
            if let Some(name) = name {
                out.push((name, block_text));
            }
            i = j;
            continue;
        }
        if chars[i] == ']' {
            break;
        }
        i += 1;
    }
    out
}

fn extract_name(block: &str) -> Option<String> {
    let idx = block.find("name:")?;
    let rest = &block[idx + 5..];
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('"').or_else(|| rest.strip_prefix('\'')).unwrap_or(rest);
    let mut end = 0;
    for (i, ch) in rest.char_indices() {
        if ch == '"' || ch == '\'' || ch == ',' {
            end = i;
            break;
        }
    }
    if end == 0 && !rest.is_empty() {
        end = rest.trim_end().len();
    }
    Some(rest[..end].trim().to_string())
}

fn extract_port_from_block(block: &str) -> Option<String> {
    // find env: { ... }
    let env_idx = block.find("env:")?;
    let rest = &block[env_idx + 4..];
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('{')?;
    let chars: Vec<char> = rest.chars().collect();
    let mut depth = 1usize;
    let mut j = 0usize;
    while j < chars.len() && depth > 0 {
        match chars[j] {
            '{' => depth += 1,
            '}' => depth -= 1,
            _ => {}
        }
        j += 1;
    }
    let env_text: String = chars[..j.min(chars.len())].iter().collect();
    for key in ["SERVER_PORT", "PORT"] {
        let mut search = 0usize;
        while let Some(rel) = env_text[search..].find(&format!("{key}:")) {
            let after = &env_text[search + rel + key.len() + 1..];
            let after = after.trim_start();
            let mut num = String::new();
            for c in after.chars() {
                if c.is_ascii_digit() {
                    num.push(c);
                } else {
                    break;
                }
            }
            if !num.is_empty() {
                return Some(num);
            }
            search += rel + key.len() + 1;
        }
    }
    None
}

fn runtime_label(runtimes: &[String]) -> String {
    if runtimes.is_empty() {
        util::dim("—")
    } else {
        runtimes.join(", ")
    }
}

pub fn run_show(_args: &[String]) -> Result<(), String> {
    let cwd = util::current_dir();
    let file_path = find_ecompose_file(&cwd)?;
    let content = std::fs::read_to_string(&file_path)
        .map_err(|e| format!("Failed to read {}: {e}", file_path.display()))?;

    let project_name = ecompose::parse_project_name(&content);
    let ct = ecompose::parse_ct_metadata(&content);
    let services = ecompose::parse_services(&content);
    let expose = ecompose::parse_expose(&content);
    let deploy = ecompose::parse_deploy(&content);

    let ecosystem_path = file_path.parent().unwrap_or(Path::new(".")).join("ecosystem.config.js");
    let ports = read_ports_from_ecosystem(&ecosystem_path);

    let out = &mut String::new();
    use std::fmt::Write as _;
    let _ = writeln!(out, "\n{}", util::sep(48));
    let _ = writeln!(out, "  {}", util::bold(&project_name));
    let _ = writeln!(out, "  {}", util::dim(&file_path.display().to_string()));
    let _ = writeln!(out, "{}", util::sep(48));
    let _ = writeln!(out);

    // smallest port first
    let mut with_port: Vec<&ecompose::Service> = services
        .iter()
        .filter(|svc| ports.contains_key(&format!("{project_name}-{}", svc.name)))
        .collect();
    with_port.sort_by_key(|svc| {
        ports
            .get(&format!("{project_name}-{}", svc.name))
            .and_then(|p| p.parse::<u64>().ok())
            .unwrap_or(u64::MAX)
    });
    if let Some(first) = with_port.first() {
        let app_name = format!("{project_name}-{}", first.name);
        let first_port = ports.get(&app_name).cloned().unwrap_or_default();
        let _ = writeln!(
            out,
            "  {}  {}  {}",
            util::bold("open first"),
            util::cyan(&format!("http://localhost:{first_port}")),
            util::dim(&format!("({})", first.name))
        );
        let _ = writeln!(out);
    }

    if !ct.get("id").unwrap_or(&String::new()).is_empty() || !ct.get("hostname").unwrap_or(&String::new()).is_empty() {
        let _ = writeln!(out, "  {}", util::bold("Container"));
        if let Some(id) = ct.get("id") {
            if !id.is_empty() {
                let _ = writeln!(out, "    {}        {}", util::cyan("id"), id);
            }
        }
        if let Some(hostname) = ct.get("hostname") {
            if !hostname.is_empty() {
                let _ = writeln!(out, "    {}  {}", util::cyan("hostname"), hostname);
            }
        }
        if let Some(cores) = ct.get("cores") {
            if !cores.is_empty() {
                let _ = writeln!(out, "    {}     {}", util::cyan("cores"), cores);
            }
        }
        if let Some(memory) = ct.get("memory") {
            if !memory.is_empty() {
                let _ = writeln!(out, "    {}    {} MB", util::cyan("memory"), memory);
            }
        }
        let _ = writeln!(out);
    }

    if !services.is_empty() {
        let _ = writeln!(out, "  {}", util::bold("Services"));
        let mut sorted = services.clone();
        sorted.sort_by(|a, b| {
            let pa = ports
                .get(&format!("{project_name}-{}", a.name))
                .and_then(|p| p.parse::<u64>().ok())
                .unwrap_or(u64::MAX);
            let pb = ports
                .get(&format!("{project_name}-{}", b.name))
                .and_then(|p| p.parse::<u64>().ok())
                .unwrap_or(u64::MAX);
            pa.cmp(&pb)
        });
        for svc in &sorted {
            let app_name = format!("{project_name}-{}", svc.name);
            let port = ports.get(&app_name);
            let _ = writeln!(out, "\n    {}", util::bold(&svc.name));
            let path_display = if svc.path.is_empty() { util::dim("—") } else { svc.path.clone() };
            let _ = writeln!(out, "      {}      {}", util::cyan("path"), path_display);
            let _ = writeln!(out, "      {}  {}", util::cyan("runtimes"), runtime_label(&svc.runtimes));
            if let Some(p) = port {
                let _ = writeln!(out, "      {}      {}", util::cyan("port"), p);
            }
        }
        let _ = writeln!(out);
    }

    if util::to_bool(expose.get("enabled").map(|s| s.as_str()).unwrap_or("")) || !expose.hostname().is_empty() {
        let _ = writeln!(out, "  {}", util::bold("Expose"));
        if !expose.hostname().is_empty() {
            let _ = writeln!(out, "    {}  {}", util::cyan("hostname"), expose.hostname());
        }
        if !expose.service().is_empty() {
            let _ = writeln!(out, "    {}   {}", util::cyan("service"), expose.service());
        }
        let _ = writeln!(out);
    }

    let github = deploy.get("github").cloned().unwrap_or_default();
    if util::to_bool(github.get("enabled").map(|s| s.as_str()).unwrap_or("")) {
        let _ = writeln!(out, "  {}", util::bold("Deploy"));
        if let Some(branch) = github.get("branch") {
            if !branch.is_empty() {
                let _ = writeln!(out, "    {}        {}", util::cyan("branch"), branch);
            }
        }
        if let Some(wp) = github.get("webhook_port") {
            if !wp.is_empty() {
                let _ = writeln!(out, "    {}  {}", util::cyan("webhook_port"), wp);
            }
        }
        let _ = writeln!(out);
    }

    let _ = writeln!(out, "{}\n", util::sep(48));
    print!("{out}");
    Ok(())
}
