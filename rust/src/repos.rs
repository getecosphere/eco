use serde::Deserialize;

// The repo catalog (repos.json) was a private hack from the pre-LXS era — it
// embedded the estate domain git URLs into the binary. It's gone. Eco is now
// source-code-agnostic: reusable capabilities come from the LXS registry and
// source ships from the developer workspace, so no central catalog is needed.
// These helpers remain (empty) so `eco compose add` compiles; they resolve
// from git URLs / local clones instead.

#[derive(Debug, Clone, Deserialize)]
pub struct RepoEntry {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub git: String,
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub requires: Vec<String>,
}

pub fn read_repo_catalog() -> Result<Vec<RepoEntry>, String> {
    Ok(Vec::new())
}

pub fn find_repo_by_name(_name: &str) -> Result<Option<RepoEntry>, String> {
    Ok(None)
}
