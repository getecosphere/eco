// eco db — managed databases for a project.
//
// `eco db add mongo|postgres` declares the project's one core database in
// ecompose.yml (never by hand) and records it with the agent, which enforces
// the account's plan quota and the one-core-DB-per-estate rule. The actual
// database + user are created on the data CT at deploy time by the existing
// pipeline. `eco db list` shows the account's databases.
use std::path::PathBuf;

use crate::commands::account::resolve_api_credentials;
use crate::ecompose;
use crate::util;

fn db_help() {
    println!(
        "eco db\n\n\
         Usage:\n  \
         eco db add <mongo|postgres>   declare the project's managed core database\n  \
         eco db list                   list this account's managed databases\n\n\
         Each estate has ONE core database (mongo XOR postgres). Adding a different\n\
         type switches the estate. Quotas follow your plan (Starter: 1 mongo + 1 postgres)."
    );
}

fn api_post_json(url: &str, api_key: &str, body: &serde_json::Value) -> Result<serde_json::Value, String> {
    let response = match ureq::post(url)
        .set("Authorization", &format!("Bearer {api_key}"))
        .set("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(30))
        .send_string(&serde_json::to_string(body).unwrap_or_default())
    {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(e) => return Err(format!("agent request failed: {e}")),
    };
    let status = response.status();
    let text = response.into_string().unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({"raw": text}));
    if (200..300).contains(&status) {
        Ok(v)
    } else {
        Err(v.get("error").and_then(|e| e.as_str()).unwrap_or(&text).to_string())
    }
}

fn api_get_json(url: &str, api_key: &str) -> Result<serde_json::Value, String> {
    let response = match ureq::get(url)
        .set("Authorization", &format!("Bearer {api_key}"))
        .timeout(std::time::Duration::from_secs(30))
        .call()
    {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(e) => return Err(format!("agent request failed: {e}")),
    };
    let status = response.status();
    let text = response.into_string().unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({"raw": text}));
    if (200..300).contains(&status) {
        Ok(v)
    } else {
        Err(v.get("error").and_then(|e| e.as_str()).unwrap_or(&text).to_string())
    }
}

fn find_ecompose() -> Result<(PathBuf, String, String), String> {
    let cwd = util::current_dir();
    let file = ecompose::resolve_ecompose_file("", &cwd).or_else(|_| ecompose::resolve_ecompose_file(".", &cwd))?;
    let content = ecompose::read_text_file(&file).unwrap_or_default();
    let project = ecompose::parse_project_name(&content);
    if project.is_empty() {
        return Err("ecompose.yml has no `project:` line".to_string());
    }
    Ok((file, project, content))
}

// Set or replace the top-level `data:` block so it declares exactly one core
// database type pointing at the platform data CT (203).
fn upsert_data_block(content: &str, db_type: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut in_data = false;
    let mut data_seen = false;
    let mut written = false;
    for line in lines {
        let trimmed = line.trim_start();
        if !in_data {
            if trimmed == "data:" {
                in_data = true;
                data_seen = true;
                out.push("data:".to_string());
                // rewrite children to the requested type
                out.push(format!("  {db_type}: 203"));
                written = true;
                continue;
            }
            out.push(line.to_string());
            continue;
        }
        if line.starts_with("  ") || line.trim().is_empty() {
            continue; // drop the old data children (replaced above)
        }
        in_data = false;
        out.push(line.to_string());
    }
    if !data_seen {
        out.push(String::new());
        out.push("data:".to_string());
        out.push(format!("  {db_type}: 203"));
    }
    let _ = written;
    format!("{}\n", out.join("\n"))
}

// Give every source service (`path:`) the database type + its connection-URI
// grant, so configgen injects MONGODB_URI/DATABASE_URL into the app. Rewrites
// an existing `database:` (switch) and merges the URI into grants.secrets.
fn mark_source_services(content: &str, db_type: &str, uri_key: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut in_services = false;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();
        if trimmed == "services:" {
            in_services = true;
            out.push(line.to_string());
            i += 1;
            continue;
        }
        if in_services && !line.starts_with(' ') && !line.trim().is_empty() {
            in_services = false;
            out.push(line.to_string());
            i += 1;
            continue;
        }
        if in_services && trimmed.starts_with("path:") {
            out.push(line.to_string());
            i += 1;
            let indent = &line[..line.len() - line.trim_start().len()];
            let mut has_database = false;
            let mut has_grant = false;
            while i < lines.len() && lines[i].starts_with(' ') {
                let child = lines[i];
                let ctrim = child.trim_start();
                if ctrim.starts_with("database:") {
                    out.push(format!("{indent}database: {db_type}"));
                    has_database = true;
                    i += 1;
                } else if ctrim.starts_with("grants:") {
                    out.push(child.to_string());
                    has_grant = true;
                    i += 1;
                    // rewrite the secrets list to include the URI key (and drop
                    // the other DB's key when switching mongo <-> postgres)
                    if i < lines.len() && lines[i].trim_start().starts_with("secrets:") {
                        let body = lines[i].trim_start().trim_start_matches("secrets:");
                        let other = if uri_key == "MONGODB_URI" { "DATABASE_URL" } else { "MONGODB_URI" };
                        if let Some(list) = body.trim().trim_start_matches('[').strip_suffix(']') {
                            let mut items: Vec<&str> = list
                                .split(',')
                                .map(|s| s.trim())
                                .filter(|s| !s.is_empty() && *s != other)
                                .collect();
                            if !items.contains(&uri_key) {
                                items.push(uri_key);
                            }
                            out.push(format!("{indent}  secrets: [{}]", items.join(", ")));
                        }
                        i += 1;
                    }
                } else {
                    out.push(child.to_string());
                    i += 1;
                }
            }
            if !has_database {
                out.push(format!("{indent}database: {db_type}"));
            }
            if !has_grant {
                out.push(format!("{indent}grants:"));
                out.push(format!("{indent}  secrets: [{uri_key}]"));
            }
            continue;
        }
        out.push(line.to_string());
        i += 1;
    }
    format!("{}\n", out.join("\n"))
}

// eco storage — managed object storage for a project.
pub fn run_storage(args: &[String]) -> Result<(), String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("help");
    match sub {
        "help" | "--help" | "-h" | "" => {
            println!("eco storage\n\nUsage:\n  eco storage add      allocate the project's managed storage bucket (MinIO)\n  eco storage list     list this account's storage buckets");
            Ok(())
        }
        "list" => {
            let (api_url, api_key) = resolve_api_credentials()?;
            let api_url = if api_url.is_empty() { "https://api.getecosphere.com".to_string() } else { api_url };
            let base = api_url.trim_end_matches('/').to_string();
            let v = api_get_json(&format!("{base}/v1/storage"), &api_key)?;
            println!("Plan: {}", v.get("plan").and_then(|p| p.as_str()).unwrap_or("free"));
            println!("  storage quota: {} GB", v.get("storage_quota_gb").and_then(|x| x.as_u64()).unwrap_or(0));
            if let Some(buckets) = v.get("buckets").and_then(|b| b.as_array()) {
                if buckets.is_empty() {
                    println!("  (no buckets yet — `eco storage add`)");
                }
                for b in buckets {
                    println!(
                        "  {} -> {}",
                        b.get("project").and_then(|p| p.as_str()).unwrap_or(""),
                        b.get("bucket").and_then(|p| p.as_str()).unwrap_or("")
                    );
                }
            }
            Ok(())
        }
        "add" => {
            let (ecompose_path, project, content) = find_ecompose()?;
            let (api_url, api_key) = resolve_api_credentials()?;
            let api_url = if api_url.is_empty() { "https://api.getecosphere.com".to_string() } else { api_url };
            let base = api_url.trim_end_matches('/').to_string();
            let v = api_post_json(&format!("{base}/v1/storage"), &api_key, &serde_json::json!({"project": project}))?;
            let bucket = v.get("bucket").and_then(|b| b.as_str()).unwrap_or("").to_string();
            if bucket.is_empty() {
                return Err("storage allocation did not return a bucket".to_string());
            }
            // Grant the S3_* env to every source service so the app can upload.
            const S3_GRANTS: [&str; 5] = ["S3_ENDPOINT", "S3_REGION", "S3_BUCKET", "S3_ACCESS_KEY", "S3_SECRET_KEY"];
            let updated = grant_source_services(&content, &S3_GRANTS);
            std::fs::write(&ecompose_path, updated).map_err(|e| format!("write {}: {e}", ecompose_path.display()))?;
            util::println_stdout(&format!("Managed storage bucket `{bucket}` allocated for {project}"));
            util::println_stdout(&format!("S3_* grants added to source services in {}", ecompose_path.display()));
            util::println_stdout("Next: `eco up dev` (local) or `eco up --remote` — the bucket is provisioned on deploy.");
            Ok(())
        }
        _ => Err("usage: eco storage add  |  eco storage list".to_string()),
    }
}

// Add the given env-keys to every source service's grants.secrets.
fn grant_source_services(content: &str, keys: &[&str]) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut in_services = false;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();
        if trimmed == "services:" {
            in_services = true;
            out.push(line.to_string());
            i += 1;
            continue;
        }
        if in_services && !line.starts_with(' ') && !line.trim().is_empty() {
            in_services = false;
            out.push(line.to_string());
            i += 1;
            continue;
        }
        if in_services && trimmed.starts_with("path:") {
            out.push(line.to_string());
            i += 1;
            let indent = &line[..line.len() - line.trim_start().len()];
            let mut has_grant = false;
            let mut existing: Vec<String> = Vec::new();
            while i < lines.len() && lines[i].starts_with(' ') {
                let child = lines[i];
                let ctrim = child.trim_start();
                if ctrim.starts_with("grants:") {
                    has_grant = true;
                    out.push(child.to_string());
                    i += 1;
                    if i < lines.len() && lines[i].trim_start().starts_with("secrets:") {
                        let body = lines[i].trim_start().trim_start_matches("secrets:");
                        if let Some(list) = body.trim().trim_start_matches('[').strip_suffix(']') {
                            existing = list.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                        }
                        i += 1;
                    }
                } else {
                    out.push(child.to_string());
                    i += 1;
                }
            }
            if !has_grant {
                out.push(format!("{indent}grants:"));
            }
            for key in keys {
                if !existing.iter().any(|e| e == key) {
                    existing.push(key.to_string());
                }
            }
            out.push(format!("{indent}  secrets: [{}]", existing.join(", ")));
            continue;
        }
        out.push(line.to_string());
        i += 1;
    }
    format!("{}\n", out.join("\n"))
}

pub fn run_db(args: &[String]) -> Result<(), String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("help");
    match sub {
        "help" | "--help" | "-h" | "" => {
            db_help();
            Ok(())
        }
        "list" => {
            let (api_url, api_key) = resolve_api_credentials()?;
            let api_url = if api_url.is_empty() { "https://api.getecosphere.com".to_string() } else { api_url };
            let base = api_url.trim_end_matches('/').to_string();
            let v = api_get_json(&format!("{base}/v1/db"), &api_key)?;
            let plan = v.get("plan").and_then(|p| p.as_str()).unwrap_or("free");
            println!("Plan: {plan}");
            println!("  managed mongo quota: {}   postgres quota: {}", v.get("mongo_quota").and_then(|x| x.as_u64()).unwrap_or(0), v.get("postgres_quota").and_then(|x| x.as_u64()).unwrap_or(0));
            if let Some(dbs) = v.get("databases").and_then(|d| d.as_array()) {
                if dbs.is_empty() {
                    println!("  (no managed databases yet — `eco db add mongo|postgres`)");
                }
                for db in dbs {
                    println!(
                        "  {} -> {}",
                        db.get("project").and_then(|p| p.as_str()).unwrap_or(""),
                        db.get("db_type").and_then(|t| t.as_str()).unwrap_or("")
                    );
                }
            }
            Ok(())
        }
        "add" => {
            let db_type = args.get(1).map(|s| s.as_str()).unwrap_or("").to_lowercase();
            if !["mongo", "postgres"].contains(&db_type.as_str()) {
                return Err("usage: eco db add <mongo|postgres>".to_string());
            }
            let (ecompose_path, project, content) = find_ecompose()?;
            let (api_url, api_key) = resolve_api_credentials()?;
            let api_url = if api_url.is_empty() { "https://api.getecosphere.com".to_string() } else { api_url };
            let base = api_url.trim_end_matches('/').to_string();

            // Record with the agent first (quota + one-core-DB enforcement).
            let uri_key = if db_type == "mongo" { "MONGODB_URI" } else { "DATABASE_URL" };
            match api_post_json(&format!("{base}/v1/db"), &api_key, &serde_json::json!({"project": project, "db_type": db_type})) {
                Ok(v) => {
                    util::println_stdout(&format!("Managed {db_type} recorded for {project} ({})", v.get("plan").and_then(|p| p.as_str()).unwrap_or("free")));
                }
                Err(e) => return Err(e),
            }

            // Declare it in ecompose.yml (never by hand).
            let updated = mark_source_services(&upsert_data_block(&content, &db_type), &db_type, uri_key);
            std::fs::write(&ecompose_path, updated).map_err(|e| format!("write {}: {e}", ecompose_path.display()))?;

            util::println_stdout(&format!("Declared data: {} in {}", db_type, ecompose_path.display()));
            util::println_stdout(&format!("Next: `eco up dev` (local) or `eco up --remote` — the database + user are provisioned on deploy."));
            Ok(())
        }
        _ => Err("usage: eco db add <mongo|postgres>  |  eco db list".to_string()),
    }
}
