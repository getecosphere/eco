use crate::cloudflare;
use crate::ecompose;
use crate::github;
use crate::util;

fn log(message: &str) {
    util::println_stdout(&format!("[eco webhook-clean] {message}"));
}

fn github_request(pathname: &str, token: &str, method: &str, body: Option<&serde_json::Value>) -> Result<serde_json::Value, String> {
    let result = github::github_request_public(
        &format!("https://api.github.com{pathname}"),
        token,
        method,
        body,
        "eco-webhook-clean",
    )?;
    Ok(result.unwrap_or(serde_json::Value::Null))
}

fn cloudflare_api_public(pathname: &str, token: &str, method: &str, body: Option<&serde_json::Value>) -> Result<serde_json::Value, String> {
    let result = github::cf_request(
        &format!("https://api.cloudflare.com/client/v4{pathname}"),
        token,
        method,
        body,
    )?;
    Ok(result)
}

fn split_repo(full_name: &str) -> (String, String) {
    let parts: Vec<&str> = full_name.split('/').collect();
    (
        parts.first().map(|s| s.to_string()).unwrap_or_default(),
        parts.get(1).map(|s| s.to_string()).unwrap_or_default(),
    )
}

fn load_project_deployment(input: &str, cwd: &std::path::Path) -> Result<Deployment, String> {
    let deployment = crate::commands::up::load_project_deployment(input, cwd)?;
    Ok(Deployment {
        project: deployment.project,
        expose: deployment.expose,
        deploy: deployment.deploy,
        ctid: deployment.ctid,
        ct_project_root: deployment.ct_project_root,
    })
}

struct Deployment {
    project: String,
    expose: ecompose::Expose,
    deploy: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
    ctid: String,
    ct_project_root: String,
}

pub fn run_webhook_clean(args: &[String]) -> Result<(), String> {
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let input = args.iter().find(|a| !a.starts_with("--")).cloned().unwrap_or_else(|| ".".to_string());
    let github_token = util::env_var_or("GITHUB_TOKEN", "");
    if github_token.is_empty() {
        return Err("GITHUB_TOKEN is required for webhook cleanup.".to_string());
    }

    let cwd = util::current_dir();
    let deployment = load_project_deployment(&input, &cwd)?;
    let github_deploy = crate::commands::up::resolve_deploy_github_config(&deployment.project, &deployment.expose, &deployment.deploy);
    let Some(github_deploy) = github_deploy else {
        log(&format!("{} has no deploy.github.enabled configuration; nothing to clean.", deployment.project));
        return Ok(());
    };
    let account = deployment.expose.cloudflare_account();
    let current_url = github_deploy.webhook_url.clone();
    let webhook_path = github_deploy.path.clone();

    log(&format!(
        "project={} currentWebhook={current_url}{}",
        deployment.project,
        if dry_run { " (dry-run)" } else { "" }
    ));
    log(&format!("account={}", if account.is_empty() { "(none)" } else { &account }));

    // Authoritative repo list from the estate's receiver config
    let mut repos: Vec<String> = Vec::new();
    let raw = util::run_capture(
        "pct",
        &[
            "exec".to_string(),
            deployment.ctid.clone(),
            "--".to_string(),
            "bash".to_string(),
            "-lc".to_string(),
            format!("cat {}/.eco/deploy/github-webhook.json", deployment.ct_project_root),
        ],
        &cwd,
    );
    match raw {
        Ok(r) if r.code == 0 => {
            if let Ok(config) = serde_json::from_str::<serde_json::Value>(&r.stdout) {
                if let Some(entries) = config.get("repos").and_then(|v| v.as_array()) {
                    repos = entries
                        .iter()
                        .filter_map(|e| e.get("fullName").and_then(|f| f.as_str()).map(|s| s.to_string()))
                        .collect();
                }
            }
        }
        Ok(r) => log(&format!("could not read receiver config in CT {}: {}", deployment.ctid, r.stderr.trim())),
        Err(e) => log(&format!("could not read receiver config in CT {}: {}", deployment.ctid, e)),
    }
    if repos.is_empty() {
        log("no configured repos found; nothing to clean.");
        return Ok(());
    }

    // 1) GitHub webhooks
    let mut broken_hosts: Vec<String> = Vec::new();
    let mut removed_hooks = 0;
    for full_name in &repos {
        let (owner, repo) = split_repo(full_name);
        if owner.is_empty() || repo.is_empty() {
            continue;
        }
        let hooks = match github_request(&format!("/repos/{owner}/{repo}/hooks"), &github_token, "GET", None) {
            Ok(h) => h,
            Err(e) => {
                log(&format!("  skip {full_name}: {e}"));
                continue;
            }
        };
        let hook_list = hooks.as_array().cloned().unwrap_or_default();
        for hook in &hook_list {
            let url = hook
                .get("config")
                .and_then(|c| c.get("url"))
                .and_then(|u| u.as_str())
                .unwrap_or("");
            if !url.contains(&webhook_path) {
                continue;
            }
            if url == current_url {
                continue;
            }
            let hostname = url::Url::parse(url).ok().map(|u| u.host_str().unwrap_or("").to_string()).unwrap_or_default();
            if hostname.is_empty() {
                continue;
            }
            let resolves = github::dns_resolves_via_doh(&hostname);
            if resolves {
                log(&format!("  keep {full_name}: {url} (resolves)"));
                continue;
            }
            let hook_id = hook.get("id").and_then(|i| i.as_i64().map(|n| n.to_string())).unwrap_or_default();
            log(&format!(
                "  {} {full_name}: {url} (hook {hook_id}, unresolvable)",
                if dry_run { "would remove" } else { "removed" }
            ));
            if !broken_hosts.contains(&hostname) {
                broken_hosts.push(hostname);
            }
            if !dry_run {
                match github_request(&format!("/repos/{owner}/{repo}/hooks/{hook_id}"), &github_token, "DELETE", None) {
                    Ok(_) => removed_hooks += 1,
                    Err(e) => log(&format!("  failed to remove {full_name}: {url} -> {e}")),
                }
            } else {
                removed_hooks += 1;
            }
        }
    }
    log(&format!("{} {} broken webhook(s)", if dry_run { "would remove" } else { "removed" }, removed_hooks));

    // 2) Cloudflare DNS
    if !account.is_empty() && !broken_hosts.is_empty() {
        let env = cloudflare::get_cloudflare_env(&account);
        if env.token.is_empty() || env.zone_id.is_empty() {
            log(&format!("skipping DNS cleanup: Cloudflare env missing for account \"{account}\""));
        } else {
            let mut zone_name = String::new();
            match cloudflare_api_public(&format!("/zones/{}", env.zone_id), &env.token, "GET", None) {
                Ok(zone) => zone_name = zone.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string(),
                Err(e) => log(&format!("skipping DNS cleanup: {e}")),
            }
            if !zone_name.is_empty() {
                for hostname in &broken_hosts {
                    if hostname.ends_with(&format!(".{zone_name}")) {
                        continue;
                    }
                    for candidate in [hostname.clone(), format!("{hostname}.{zone_name}")] {
                        let encoded = github::url_encode(&candidate);
                        match cloudflare_api_public(
                            &format!("/zones/{}/dns_records?name={encoded}&per_page=100", env.zone_id),
                            &env.token,
                            "GET",
                            None,
                        ) {
                            Ok(records) => {
                                let match_record = records
                                    .as_array()
                                    .and_then(|arr| arr.iter().find(|r| r.get("name").and_then(|n| n.as_str()) == Some(&candidate)))
                                    .cloned();
                                if let Some(record) = match_record {
                                    let id = record.get("id").and_then(|i| i.as_str()).unwrap_or("");
                                    log(&format!("  {} DNS record {candidate} from zone {zone_name}", if dry_run { "would remove" } else { "removed" }));
                                    if !dry_run && !id.is_empty() {
                                        let _ = cloudflare_api_public(
                                            &format!("/zones/{}/dns_records/{id}", env.zone_id),
                                            &env.token,
                                            "DELETE",
                                            None,
                                        );
                                    }
                                }
                            }
                            Err(e) => log(&format!("  failed DNS cleanup {candidate}: {e}")),
                        }
                    }
                }
            }
        }
    } else if broken_hosts.is_empty() {
        log("no broken hook hostnames to reconcile in DNS");
    }
    log("done");
    Ok(())
}
