use crate::util;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

const DEFAULT_PORT: u16 = 8790;
const DEFAULT_KEYS_FILE: &str = "/etc/eco/agent-keys";
const MAX_BODY_BYTES: u64 = 512 * 1024 * 1024;

struct Request {
    method: String,
    url: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

fn parse_request(buf: &[u8]) -> Option<Request> {
    let hdr_end = buf.windows(4).position(|w| w == b"\r\n\r\n")?;
    let header_text = String::from_utf8_lossy(&buf[..hdr_end]);
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let url = parts.next()?.to_string();
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            headers.insert(key.trim().to_lowercase().to_string(), value.trim().to_string());
        }
    }
    let body = buf[hdr_end + 4..].to_vec();
    Some(Request { method, url, headers, body })
}

fn read_request(stream: &mut std::net::TcpStream) -> Option<Request> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 65536];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if let Some(hdr_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let header_text = String::from_utf8_lossy(&buf[..hdr_end]);
                    let content_length = header_text
                        .lines()
                        .find_map(|l| l.to_lowercase().strip_prefix("content-length:").and_then(|v| v.trim().parse::<u64>().ok()))
                        .unwrap_or(0);
                    if content_length > MAX_BODY_BYTES {
                        return None;
                    }
                    if buf.len() as u64 >= hdr_end as u64 + 4 + content_length {
                        break;
                    }
                }
            }
            Err(_) => break,
        }
    }
    parse_request(&buf)
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        500 => "Internal Server Error",
        _ => "Error",
    }
}

fn text_response(status: u16, body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status} {}\r\n{CORS}Content-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        reason_phrase(status),
        body.len()
    )
    .into_bytes()
}

const CORS: &str = "Access-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: Authorization, Content-Type\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Max-Age: 86400\r\n";

fn json_response(status: u16, value: &serde_json::Value) -> Vec<u8> {
    let body = value.to_string();
    format!(
        "HTTP/1.1 {status} {}\r\n{CORS}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        reason_phrase(status),
        body.len()
    )
    .into_bytes()
}

fn timing_safe_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    let mut difference = 0u8;
    for (x, y) in a_bytes.iter().zip(b_bytes.iter()) {
        difference |= x ^ y;
    }
    difference == 0
}

fn load_api_keys() -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    let env_key = util::env_var_or("ECO_AGENT_API_KEY", "");
    if !env_key.is_empty() && !keys.contains(&env_key) {
        keys.push(env_key);
    }
    let keys_file = util::env_var_or("ECO_AGENT_KEYS_FILE", DEFAULT_KEYS_FILE);
    if let Ok(content) = std::fs::read_to_string(&keys_file) {
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let key = trimmed.split_whitespace().next().unwrap_or(trimmed).to_string();
            if !key.is_empty() && !keys.contains(&key) {
                keys.push(key);
            }
        }
    }
    keys
}

fn authorized(keys: &[String], request_headers: &HashMap<String, String>) -> bool {
    let Some(token) = bearer_token(request_headers) else {
        return false;
    };
    keys.iter().any(|key| timing_safe_eq(key, &token))
        || crate::runtime::account::account_for_key(&token).is_some()
}

fn bearer_token(request_headers: &HashMap<String, String>) -> Option<String> {
    let header = request_headers.get("authorization")?;
    header.strip_prefix("Bearer ").map(|s| s.to_string())
}

fn parse_account_body(body: &[u8]) -> Option<(String, String)> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let email = value.get("email")?.as_str()?.to_string();
    let password = value.get("password")?.as_str()?.to_string();
    Some((email, password))
}

fn log(message: &str, details: &str) {
    let suffix = if details.is_empty() { String::new() } else { format!(" {details}") };
    println!("[eco serve] {message}{suffix}");
}

pub fn run_serve(args: &[String]) -> Result<(), String> {
    if args.first().map(|s| s.as_str()) == Some("gen-key") {
        return gen_key(args);
    }

    let mut port = DEFAULT_PORT;
    let mut host = "0.0.0.0".to_string();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--port" => {
                port = args.get(i + 1).and_then(|v| v.parse().ok()).ok_or_else(|| "eco serve --port <port>".to_string())?;
                i += 2;
            }
            "--host" => {
                host = args.get(i + 1).cloned().unwrap_or_default();
                i += 2;
            }
            other => return Err(format!("Unknown eco serve option: {other}")),
        }
    }

    let keys = load_api_keys();
    if keys.is_empty() {
        return Err(
            "No API keys configured for eco serve. Set ECO_AGENT_API_KEY or add keys to the keys file (default /etc/eco/agent-keys).\nGenerate one with: eco serve gen-key"
                .to_string(),
        );
    }

    let listener = TcpListener::bind((host.as_str(), port)).map_err(|e| format!("eco serve bind {host}:{port}: {e}"))?;
    println!(
        "[eco serve] listening on {host}:{port} with {} API key(s)\n[eco serve] endpoints:\n  GET  /v1/health\n  GET  /v1/estates\n  GET  /v1/estates/<project>/services/<service>/env\n  POST /v1/estates/<project>/deploy",
        keys.len()
    );

    for stream in listener.incoming() {
        let Ok(mut stream) = stream else {
            continue;
        };
        stream.set_read_timeout(Some(Duration::from_secs(900))).ok();
        let Some(req) = read_request(&mut stream) else {
            let _ = stream.write_all(&json_response(400, &serde_json::json!({"error": "Bad request"})));
            continue;
        };
        let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();

        if req.method == "OPTIONS" {
            let _ = stream.write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: Authorization, Content-Type\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Max-Age: 86400\r\n\r\n");
            continue;
        }
        if req.method == "GET" && req.url == "/v1/health" {
            log("health", &peer);
            let _ = stream.write_all(&json_response(200, &serde_json::json!({
                "ok": true,
                "version": env!("CARGO_PKG_VERSION"),
                "protocol": crate::util::PROTOCOL_VERSION,
            })));
            continue;
        }

        // Public account routes (signup/login) — no auth required.
        if req.method == "POST" && req.url.trim_end_matches('/').ends_with("/v1/account/signup") {
            log("account-signup", &peer);
            match parse_account_body(&req.body) {
                Some((email, password)) => match crate::runtime::account::signup(&email, &password) {
                    Ok((api_key, account)) => {
                        let _ = stream.write_all(&json_response(
                            200,
                            &serde_json::json!({"api_key": api_key, "email": account.email, "tier": account.tier, "payload_cap_mb": account.payload_cap_mb}),
                        ));
                    }
                    Err(e) => {
                        let _ = stream.write_all(&json_response(400, &serde_json::json!({"error": e})));
                    }
                },
                None => {
                    let _ = stream.write_all(&json_response(400, &serde_json::json!({"error": "expected {\"email\":...,\"password\":...}"})));
                }
            }
            continue;
        }
        if req.method == "POST" && req.url.trim_end_matches('/').ends_with("/v1/account/login") {
            log("account-login", &peer);
            match parse_account_body(&req.body) {
                Some((email, password)) => match crate::runtime::account::login(&email, &password) {
                    Ok(api_key) => {
                        let _ = stream.write_all(&json_response(200, &serde_json::json!({"api_key": api_key, "email": email.trim().to_lowercase(), "tier": "free"})));
                    }
                    Err(e) => {
                        let _ = stream.write_all(&json_response(401, &serde_json::json!({"error": e})));
                    }
                },
                None => {
                    let _ = stream.write_all(&json_response(400, &serde_json::json!({"error": "expected {\"email\":...,\"password\":...}"})));
                }
            }
            continue;
        }

        if !authorized(&keys, &req.headers) {
            log("unauthorized", &format!("{peer} {} {}", req.method, req.url));
            let _ = stream.write_all(&json_response(401, &serde_json::json!({"error": "Unauthorized"})));
            continue;
        }

        let url_without_query = req.url.split('?').next().unwrap_or(&req.url);
        let staging = req.url.contains("staging=1");
        let segments: Vec<&str> = url_without_query.trim_matches('/').split('/').filter(|s| !s.is_empty()).collect();

        if req.method == "GET" && segments.as_slice() == ["v1", "account", "me"] {
            log("account-me", &peer);
            match bearer_token(&req.headers).and_then(|t| crate::runtime::account::account_for_key(&t)) {
                Some(account) => {
                    let _ = stream.write_all(&json_response(
                        200,
                        &serde_json::json!({"email": account.email, "tier": account.tier, "payload_cap_mb": account.payload_cap_mb}),
                    ));
                }
                None => {
                    let _ = stream.write_all(&json_response(401, &serde_json::json!({"error": "invalid API key"})));
                }
            }
            continue;
        }
        if req.method == "GET" && segments.as_slice() == ["v1", "account", "deploys"] {
            log("account-deploys", &peer);
            match bearer_token(&req.headers) {
                Some(token) => {
                    let deploys = crate::runtime::account::list_deploys(&token, 20);
                    let _ = stream.write_all(&json_response(200, &serde_json::json!({"deploys": deploys})));
                }
                None => {
                    let _ = stream.write_all(&json_response(401, &serde_json::json!({"error": "invalid API key"})));
                }
            }
            continue;
        }
        if req.method == "POST" && segments.as_slice() == ["v1", "account", "rotate-key"] {
            log("account-rotate-key", &peer);
            match bearer_token(&req.headers).ok_or_else(|| "invalid API key".to_string()).and_then(|t| crate::runtime::account::rotate_key(&t)) {
                Ok(key) => {
                    let _ = stream.write_all(&json_response(200, &serde_json::json!({"api_key": key})));
                }
                Err(e) => {
                    let _ = stream.write_all(&json_response(401, &serde_json::json!({"error": e})));
                }
            }
            continue;
        }

        if req.method == "GET" && segments.as_slice() == ["v1", "estates"] {
            log("list-estates", &peer);
            let estates = crate::commands::up::agent_list_estates();
            let _ = stream.write_all(&json_response(200, &serde_json::json!({ "estates": estates })));
            continue;
        }

        if req.method == "GET" && segments.len() == 6 && segments[0] == "v1" && segments[1] == "estates" && segments[3] == "services" && segments[5] == "env" {
            let project = segments[2];
            let service = segments[4];
            log("service-env", &format!("{peer} {project} {service}{}", if staging { " (staging)" } else { "" }));
            match crate::commands::up::agent_read_service_env(project, service, staging) {
                Ok(env) => {
                    let _ = stream.write_all(&text_response(200, &env));
                }
                Err(e) => {
                    log("service-env-missing", &format!("{project} {service}: {e}"));
                    let _ = stream.write_all(&json_response(404, &serde_json::json!({"error": e})));
                }
            }
            continue;
        }

        if req.method == "POST" && segments.len() == 4 && segments[0] == "v1" && segments[1] == "estates" && segments[3] == "deploy" {
            let project = segments[2];
            log("deploy-start", &format!("{peer} {project}{} bytes={}", if staging { " (staging)" } else { "" }, req.body.len()));
            let token = bearer_token(&req.headers).unwrap_or_default();
            let size_mb = (req.body.len() / (1024 * 1024)) as u64;
            match crate::commands::up::agent_handle_deploy(project, &req.body, staging) {
                Ok(summary) => {
                    log("deploy-complete", &format!("{project}{}", if staging { " (staging)" } else { "" }));
                    crate::runtime::account::record_deploy(&token, project, "success", size_mb);
                    let _ = stream.write_all(&text_response(200, &summary));
                }
                Err(e) => {
                    log("deploy-failed", &format!("{project}: {e}"));
                    crate::runtime::account::record_deploy(&token, project, "failed", size_mb);
                    let _ = stream.write_all(&text_response(500, &format!("deploy failed: {e}")));
                }
            }
            continue;
        }

        if req.method == "GET" && segments.len() == 4 && segments[0] == "v1" && segments[1] == "estates" && segments[3] == "deploy-status" {
            let project = segments[2];
            let deploy_id: Option<i64> = req.url.split('?').nth(1).and_then(|q| {
                q.split('&').find(|kv| kv.starts_with("id=")).and_then(|kv| kv[3..].parse().ok())
            });
            let status = match deploy_id {
                Some(id) => crate::runtime::account::deploy_status_by_id(id),
                None => crate::runtime::account::latest_deploy_status_for_project(project),
            };
            let _ = stream.write_all(&json_response(200, &serde_json::json!({"project": project, "deploy_id": deploy_id, "status": status})));
            continue;
        }

        // Chunked payload upload for large deploys: the client POSTs the payload
        // in <100MB chunks (Cloudflare's free-tier request limit) as
        // ?part=<i>&total=<N>. Chunks are stored per-part and the last one
        // reassembles + triggers the deploy.
        if req.method == "POST" && segments.len() == 4 && segments[0] == "v1" && segments[1] == "estates" && segments[3] == "deploy-upload" {
            let project = segments[2];
            let part: usize = req.url.split('?').nth(1).and_then(|q| {
                q.split('&').find(|kv| kv.starts_with("part=")).and_then(|kv| kv[5..].parse().ok())
            }).unwrap_or(0);
            let total: usize = req.url.split('?').nth(1).and_then(|q| {
                q.split('&').find(|kv| kv.starts_with("total=")).and_then(|kv| kv[6..].parse().ok())
            }).unwrap_or(0);
            log("deploy-upload", &format!("{peer} {project} part {part}/{total} bytes={}", req.body.len()));
            let parts_dir = format!("/tmp/eco-upload-{project}");
            let _ = std::fs::create_dir_all(&parts_dir);
            let part_path = format!("{parts_dir}/part-{part:04}");
            if let Err(e) = std::fs::write(&part_path, &req.body) {
                let _ = stream.write_all(&json_response(500, &serde_json::json!({"error": format!("write chunk: {e}")})));
                continue;
            }
            if part + 1 >= total {
                // Reassemble + deploy asynchronously (the deploy takes minutes;
                // the client must not wait on it through the tunnel, which
                // times out at ~100s).
                let payload_path = format!("/tmp/eco-remote-{project}.tar.gz");
                let reassemble = (|| -> Result<(), String> {
                    let mut out = std::fs::File::create(&payload_path).map_err(|e| format!("create payload: {e}"))?;
                    use std::io::Write;
                    for i in 0..total {
                        let p = format!("{parts_dir}/part-{i:04}");
                        let bytes = std::fs::read(&p).map_err(|e| format!("read part {i}: {e}"))?;
                        out.write_all(&bytes).map_err(|e| format!("write part {i}: {e}"))?;
                    }
                    let _ = std::fs::remove_dir_all(&parts_dir);
                    Ok(())
                })();
                if let Err(e) = reassemble {
                    let _ = stream.write_all(&json_response(500, &serde_json::json!({"error": e})));
                    continue;
                }
                let token = bearer_token(&req.headers).unwrap_or_default();
                let deploy_project = project.to_string();
                let deploy_staging = staging;
                let deploy_payload = payload_path.clone();
                let deploy_id = crate::runtime::account::record_deploy_running(&token, &deploy_project, 0);
                std::thread::spawn(move || {
                    let bytes = match std::fs::read(&deploy_payload) {
                        Ok(b) => b,
                        Err(_) => return,
                    };
                    let size_mb = (bytes.len() / (1024 * 1024)) as u64;
                    if let Some(id) = deploy_id {
                        crate::runtime::account::update_deploy_status(id, "running");
                        match crate::commands::up::agent_handle_deploy(&deploy_project, &bytes, deploy_staging) {
                            Ok(_) => crate::runtime::account::update_deploy_status(id, "success"),
                            Err(e) => {
                                eprintln!("[eco-agent] async deploy of {} failed: {}", deploy_project, e);
                                crate::runtime::account::update_deploy_status(id, "failed");
                            }
                        }
                    } else {
                        match crate::commands::up::agent_handle_deploy(&deploy_project, &bytes, deploy_staging) {
                            Ok(_) => crate::runtime::account::record_deploy(&token, &deploy_project, "success", size_mb),
                            Err(_) => crate::runtime::account::record_deploy(&token, &deploy_project, "failed", size_mb),
                        }
                    }
                });
                let _ = stream.write_all(&json_response(202, &serde_json::json!({"ok": true, "deploy_started": true, "project": project, "deploy_id": deploy_id})));
            } else {
                let _ = stream.write_all(&json_response(202, &serde_json::json!({"ok": true, "part": part, "total": total})));
            }
            continue;
        }

        // scp-based deploy: the client uploaded the payload to a well-known
        // path (resilient against lossy links that drop large HTTP bodies);
        // this endpoint just triggers the deploy reading that file.
        if req.method == "POST" && segments.len() == 4 && segments[0] == "v1" && segments[1] == "estates" && segments[3] == "deploy-file" {
            let project = segments[2];
            // Version gate: the client and agent must speak the same deploy
            // protocol, or a stale client ships a payload the agent mis-reads
            // (silent mis-deploy bugs are the worst). Reject mismatches loudly.
            let client_protocol = req
                .headers
                .get(crate::util::PROTOCOL_HEADER)
                .and_then(|v| v.parse::<u32>().ok());
            let client_semver = req.headers.get(crate::util::CLIENT_VERSION_HEADER).cloned().unwrap_or_default();
            let agent_protocol = crate::util::PROTOCOL_VERSION;
            let mismatch = match client_protocol {
                Some(p) => p != agent_protocol,
                None => true, // older client that never sent the header
            };
            if mismatch {
                let msg = crate::util::protocol_mismatch_msg(
                    &client_protocol.map(|p| p.to_string()).unwrap_or_else(|| "?".to_string()),
                    if client_semver.is_empty() { "(unknown)" } else { &client_semver },
                    env!("CARGO_PKG_VERSION"),
                );
                let _ = stream.write_all(&text_response(400, &msg));
                continue;
            }
            let payload_path = format!("/tmp/eco-remote-{project}.tar.gz");
            log("deploy-file-start", &format!("{peer} {project}{} path={}", if staging { " (staging)" } else { "" }, payload_path));
            let bytes = match std::fs::read(&payload_path) {
                Ok(b) => b,
                Err(e) => {
                    let _ = stream.write_all(&text_response(500, &format!("cannot read payload {payload_path}: {e}")));
                    continue;
                }
            };
            let token = bearer_token(&req.headers).unwrap_or_default();
            let size_mb = (bytes.len() / (1024 * 1024)) as u64;
            match crate::commands::up::agent_handle_deploy(project, &bytes, staging) {
                Ok(summary) => {
                    let _ = std::fs::remove_file(&payload_path);
                    crate::runtime::account::record_deploy(&token, project, "success", size_mb);
                    let _ = stream.write_all(&text_response(200, &summary));
                }
                Err(e) => {
                    crate::runtime::account::record_deploy(&token, project, "failed", size_mb);
                    let _ = stream.write_all(&text_response(500, &format!("deploy failed: {e}")));
                }
            }
            continue;
        }

        // Temporary public URL for a locally-running dev app: `eco serve <sub>`.
        // POST /v1/serve {subdomain, origin, port}  → reserve + DNS + tunnel token
        // GET  /v1/serve                            → list active leases
        // DELETE /v1/serve/<subdomain>              → release + cleanup
        if req.method == "POST" && segments.as_slice() == ["v1", "serve"] {
            log("serve-reserve", &format!("{peer} {}", req.body.len()));
            let account = crate::runtime::serve::default_account();
            let zone = crate::runtime::serve::default_zone();
            let body: serde_json::Value = match serde_json::from_slice(&req.body) {
                Ok(v) => v,
                Err(_) => {
                    let _ = stream.write_all(&json_response(400, &serde_json::json!({"error": "expected {\"subdomain\":...,\"origin\":...,\"port\":...}"})));
                    continue;
                }
            };
            let subdomain = body.get("subdomain").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let origin = body.get("origin").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let port = body.get("port").and_then(|v| v.as_u64()).unwrap_or(3000) as u16;
            let owner = bearer_token(&req.headers)
                .and_then(|t| crate::runtime::account::account_for_key(&t))
                .map(|a| a.email)
                .unwrap_or_else(|| "host-key".to_string());
            match crate::runtime::serve::reserve_subdomain(&subdomain, &owner, &origin, port, &account, &zone) {
                Ok((hostname, tunnel_token)) => {
                    let _ = stream.write_all(&json_response(200, &serde_json::json!({
                        "hostname": hostname,
                        "tunnel_token": tunnel_token,
                        "account": account,
                        "url": format!("https://{hostname}")
                    })));
                }
                Err(e) => {
                    let _ = stream.write_all(&json_response(409, &serde_json::json!({"error": e})));
                }
            }
            continue;
        }
        if req.method == "GET" && segments.as_slice() == ["v1", "serve"] {
            log("serve-list", &peer);
            let leases = crate::runtime::serve::list_leases();
            let _ = stream.write_all(&json_response(200, &serde_json::json!({"serves": leases})));
            continue;
        }
        if req.method == "DELETE" && segments.len() == 3 && segments[0] == "v1" && segments[1] == "serve" {
            let subdomain = segments[2].to_string();
            log("serve-release", &format!("{peer} {subdomain}"));
            let account = crate::runtime::serve::default_account();
            let zone = crate::runtime::serve::default_zone();
            match crate::runtime::serve::release_subdomain(&subdomain, &account, &zone) {
                Ok(Some(hostname)) => {
                    let _ = stream.write_all(&json_response(200, &serde_json::json!({"released": hostname})));
                }
                Ok(None) => {
                    let _ = stream.write_all(&json_response(404, &serde_json::json!({"error": "no lease for that subdomain"})));
                }
                Err(e) => {
                    let _ = stream.write_all(&json_response(500, &serde_json::json!({"error": e})));
                }
            }
            continue;
        }

        log("not-found", &format!("{peer} {} {}", req.method, req.url));
        let _ = stream.write_all(&json_response(404, &serde_json::json!({"error": "Not found"})));
    }

    Ok(())
}

fn gen_key(args: &[String]) -> Result<(), String> {
    let write = args.iter().any(|a| a == "--write");
    let mut bytes = [0u8; 32];
    rand::Rng::fill(&mut rand::thread_rng(), &mut bytes);
    let key = crate::registry::hex_encode(&bytes);
    if write {
        let keys_file = util::env_var_or("ECO_AGENT_KEYS_FILE", DEFAULT_KEYS_FILE);
        let parent = std::path::Path::new(&keys_file).parent().map(|p| p.to_path_buf());
        if let Some(parent) = parent {
            let _ = std::fs::create_dir_all(&parent);
        }
        let mut content = std::fs::read_to_string(&keys_file).unwrap_or_default();
        if !content.ends_with('\n') && !content.is_empty() {
            content.push('\n');
        }
        content.push_str(&key);
        content.push('\n');
        std::fs::write(&keys_file, content).map_err(|e| format!("write keys file: {e}"))?;
        println!("[eco serve] wrote new API key to {keys_file}");
    }
    println!("{key}");
    Ok(())
}
