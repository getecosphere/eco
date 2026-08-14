use crate::util;

pub fn git(args: &[String], cwd: &std::path::Path) -> Result<String, String> {
    let result = util::run_capture("git", args, cwd)?;
    if result.code != 0 {
        return Err(format!(
            "Failed to inspect git repo in {}: {}",
            cwd.display(),
            result.stderr.trim()
        ));
    }
    Ok(result.stdout.trim().to_string())
}

pub fn inspect_current_repo(cwd: &std::path::Path) -> Result<(String, String, String, String), String> {
    let repo_root = git(&["rev-parse".to_string(), "--show-toplevel".to_string()], cwd)?;
    let name = repo_root
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string();
    let branch = git(&["branch".to_string(), "--show-current".to_string()], &std::path::PathBuf::from(&repo_root))?;
    let remote = git(&["remote".to_string(), "get-url".to_string(), "origin".to_string()], &std::path::PathBuf::from(&repo_root))?;
    Ok((repo_root, name, branch, remote))
}
