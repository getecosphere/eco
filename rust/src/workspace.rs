use std::path::{Path, PathBuf};

pub fn find_workspace_root(start_dir: &Path) -> Result<PathBuf, String> {
    let mut current = std::fs::canonicalize(start_dir).unwrap_or_else(|_| start_dir.to_path_buf());
    loop {
        if current.join("eco").exists()
            || current.join("core").exists()
            || current.join(".eco").is_dir()
        {
            return Ok(current);
        }
        let parent = match current.parent() {
            Some(p) => p.to_path_buf(),
            None => break,
        };
        if parent == current {
            break;
        }
        current = parent;
    }
    Err("Could not find a workspace root containing eco/, core/, or a .eco marker. Run this command from inside a SuperApp workspace.".to_string())
}

pub fn find_estate_root(start_dir: &Path) -> Result<PathBuf, String> {
    let workspace_root = find_workspace_root(start_dir)?;
    let resolved_start = std::fs::canonicalize(start_dir).unwrap_or_else(|_| start_dir.to_path_buf());
    let relative = resolved_start.strip_prefix(&workspace_root).unwrap_or(&resolved_start);
    if relative.as_os_str().is_empty() {
        return Ok(workspace_root);
    }
    if let Some(segment) = relative.components().next() {
        let segment_str = segment.as_os_str().to_string_lossy().to_string();
        if segment_str == "." || segment_str == "eco" || segment_str == "core" {
            return Ok(workspace_root);
        }
        return Ok(workspace_root.join(segment_str));
    }
    Ok(workspace_root)
}
