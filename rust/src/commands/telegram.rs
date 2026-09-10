// eco telegram — the ops Telegram channel (agent /v1/telegram bridge).
//
//   eco telegram send <key> <message...>         push a plain message
//   eco telegram ask <key> <question...> [--timeout <secs>] [--wait]
//                                                ask and (optionally) block for
//                                                the reply over Telegram
//   eco telegram status <conversation_id> [--wait [<secs>]]
//                                                poll an open question
//   eco telegram bind <key> [--user <id>] [--chat <id>]
//                                                bind a key to a chat
//
// The key is a recipient binding (e.g. "ops" or a user email) managed by the
// telegram LXS. Replies to `ask` arrive over Telegram (reply-to the question,
// or the only open question in the chat); `--wait` polls until answered.
use crate::commands::account::{read_stored_auth, resolve_api_credentials};
use std::time::{Duration, Instant};

fn resolve_api_url() -> String {
    crate::util::env_var_or("ECO_API_URL", "")
        .trim()
        .trim_end_matches('/')
        .to_string()
}

fn creds() -> Result<(String, String), String> {
    resolve_api_credentials()
}

fn post_json_auth(
    api_url: &str,
    api_key: &str,
    path: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let url = format!("{api_url}{path}");
    let response = match ureq::post(&url)
        .set("User-Agent", "eco-cli")
        .set("Authorization", &format!("Bearer {api_key}"))
        .set("Content-Type", "application/json")
        .timeout(Duration::from_secs(30))
        .send_string(&serde_json::to_string(body).unwrap())
    {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            let text = r.into_string().unwrap_or_default();
            let value: serde_json::Value =
                serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({"raw": text}));
            let msg = value
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or(&text)
                .to_string();
            // Render a paid-feature rejection as an actionable upgrade offer.
            let offer = value
                .get("upgrade_to")
                .and_then(|u| u.as_object())
                .map(|u| {
                    format!(
                        "\nThis needs the {} plan (${}/mo). Subscribe with: eco plan subscribe {}",
                        u.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                        u.get("price_usd").and_then(|v| v.as_u64()).unwrap_or(0),
                        u.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                    )
                })
                .unwrap_or_default();
            return Err(format!("HTTP {code}: {msg}{offer}"));
        }
        Err(ureq::Error::Transport(t)) => return Err(format!("network error: {t}")),
    };
    let status = response.status();
    let text = response.into_string().unwrap_or_default();
    let value: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({"raw": text}));
    if (200..300).contains(&status) {
        Ok(value)
    } else {
        let msg = value
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or(&text)
            .to_string();
        Err(msg)
    }
}

fn get_json_auth(
    api_url: &str,
    api_key: &str,
    path: &str,
) -> Result<serde_json::Value, String> {
    let url = format!("{api_url}{path}");
    let response = match ureq::get(&url)
        .set("User-Agent", "eco-cli")
        .set("Authorization", &format!("Bearer {api_key}"))
        .timeout(Duration::from_secs(30))
        .call()
    {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            let text = r.into_string().unwrap_or_default();
            return Err(format!("HTTP {code}: {}", text.chars().take(200).collect::<String>()));
        }
        Err(ureq::Error::Transport(t)) => return Err(format!("network error: {t}")),
    };
    let status = response.status();
    let text = response.into_string().unwrap_or_default();
    let value: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({"raw": text}));
    if (200..300).contains(&status) {
        Ok(value)
    } else {
        let msg = value
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or(&text)
            .to_string();
        Err(msg)
    }
}

fn wait_seconds(rest: &[String], flag: &str) -> Option<u64> {
    rest.iter().position(|a| a == flag).and_then(|i| {
        rest.get(i + 1)
            .and_then(|v| v.parse::<u64>().ok())
    })
}

/// Resolve the header shown above every message so the reader knows which
/// session/topic it belongs to. Order: `--header <text>` > `--from <label>` >
/// `ECO_TELEGRAM_SESSION` > the most recently active opencode session for the
/// current directory (best effort, via the opencode SQLite DB).
fn header_label(rest: &[String]) -> Option<String> {
    for flag in ["--header", "--from"] {
        if let Some(l) = rest
            .iter()
            .position(|a| a == flag)
            .and_then(|i| rest.get(i + 1))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        {
            return Some(l);
        }
    }
    if let Ok(l) = std::env::var("ECO_TELEGRAM_SESSION") {
        let l = l.trim().to_string();
        if !l.is_empty() {
            return Some(l);
        }
    }
    if std::env::var("OPENCODE").is_ok() {
        return opencode_session_label();
    }
    None
}

fn opencode_session_label() -> Option<String> {
    let db = std::env::var("OPENCODE_DB").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{home}/.local/share/opencode/opencode.db")
    });
    if !std::path::Path::new(&db).is_file() {
        return None;
    }
    let cwd = std::env::current_dir().ok()?;
    let cwd = cwd.to_string_lossy().replace('\'', "''");
    let query = format!(
        "SELECT title FROM session WHERE directory='{cwd}' AND title<>'' ORDER BY time_updated DESC LIMIT 1;"
    );
    let out = std::process::Command::new("sqlite3")
        .arg(&db)
        .arg(query)
        .output()
        .ok()?;
    let title = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if title.is_empty() {
        return None;
    }
    let mut t = title;
    if t.chars().count() > 48 {
        t = t.chars().take(45).collect::<String>() + "…";
    }
    Some(t)
}

fn esc_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Build the message body with a **bold** header line. Returns the text and the
/// parse mode (HTML when a header is present). The header is mandatory in
/// spirit: pass `--header "<topic>"` so the reader knows what this is about.
fn format_message(rest: &[String], body: &str) -> (String, Option<String>) {
    match header_label(rest) {
        Some(label) => {
            let head = format!("<b>[{}]</b>", esc_html(&label));
            let full = if body.is_empty() {
                head
            } else {
                format!("{head}\n{}", esc_html(body))
            };
            (full, Some("HTML".to_string()))
        }
        None => (body.to_string(), None),
    }
}

fn poll_conversation(
    api_url: &str,
    api_key: &str,
    id: &str,
    max_wait_secs: u64,
) -> Result<serde_json::Value, String> {
    let start = Instant::now();
    loop {
        let v = get_json_auth(api_url, api_key, &format!("/v1/telegram/conversations/{id}"))?;
        let status = v.get("status").and_then(|s| s.as_str()).unwrap_or("");
        match status {
            "answered" => return Ok(v),
            "expired" => {
                return Err(format!(
                    "question {id} expired without an answer. Re-ask with `eco telegram ask`."
                ))
            }
            "canceled" => return Err(format!("question {id} was canceled.")),
            _ => {}
        }
        if start.elapsed().as_secs() >= max_wait_secs {
            return Err(format!(
                "still awaiting an answer after {max_wait_secs}s — poll again with `eco telegram status {id} --wait`"
            ));
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn help_text() -> String {
    "eco telegram — Telegram ops channel (needs the telegram LXS on the agent host)\n\n\
     Usage:\n\
       eco telegram send <key> <message...>          push a plain message to a bound key\n\
       eco telegram send <key> <caption...> --file <path>\n\
                                                     attach a file (PDF, image, zip, …)\n\
       eco telegram ask <key> <question...>          ask a question over Telegram (opens a conversation)\n\
         [--timeout <secs>]  max wait before the question expires (default 180)\n\
         [--wait]            block until the reply arrives (up to the timeout)\n\
       eco telegram status <conversation_id> [--wait [<secs>]]\n\
                                                     poll an open question until answered\n\
       eco telegram bind <key> --user <id>           bind a private-chat user (must have messaged the bot)\n\
       eco telegram bind <key> --chat <id>           bind a group chat id\n\
       eco telegram chats                            list chats the bot has seen\n\
       eco telegram connect                          link your eco account to @geteco_user_bot\n\
       eco telegram me                               show whether your account is linked\n\
       eco telegram disconnect                       unlink your account\n\
\n\
     Keys are recipient bindings (e.g. \"ops\"). Bind once, then messages just work;\n\
     unbound keys fall back to email (no_channel).\n\
\n\
     MANDATORY: every message is prefixed with a bold header so the reader knows\n\
     what it is about: [--header \"<topic/session>\"] (preferred) >\n\
     $ECO_TELEGRAM_SESSION > the active opencode session title (auto).".to_string()
}

pub fn run_telegram(args: &[String]) -> Result<(), String> {
    let subcommand = args.first().map(|s| s.as_str()).unwrap_or("help");
    let rest = &args[1..];
    if subcommand == "help" || subcommand == "--help" || subcommand == "-h" {
        println!("{}", help_text());
        return Ok(());
    }
    let (api_url, api_key) = creds()?;

    match subcommand {
        "send" => {
            let key = rest
                .first()
                .filter(|a| !a.starts_with("--"))
                .cloned()
                .ok_or("usage: eco telegram send <key> <message...> [--file <path>]")?;
            let file = rest
                .iter()
                .position(|a| a == "--file")
                .and_then(|i| rest.get(i + 1))
                .cloned();
            let raw_text = rest[1..]
                .iter()
                .filter(|a| !a.starts_with("--") && !is_flag_value(rest, a))
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
            if raw_text.is_empty() && file.is_none() {
                return Err("usage: eco telegram send <key> <message...> [--file <path>]".to_string());
            }
            let (text, parse_mode) = format_message(rest, &raw_text);
            let mut body = serde_json::json!({ "key": key.clone(), "text": text });
            if let Some(pm) = &parse_mode {
                body["parse_mode"] = serde_json::json!(pm);
            }
            if let Some(path) = &file {
                let bytes = std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))?;
                use base64::Engine;
                let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                let name = std::path::Path::new(path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "file".to_string());
                body["file"] = serde_json::json!(b64);
                body["filename"] = serde_json::json!(name);
            }
            let v = post_json_auth(&api_url, &api_key, "/v1/telegram/send", &body)?;
            let message_id = v
                .get("message_id")
                .and_then(|m| m.as_i64())
                .unwrap_or(0);
            match &file {
                Some(path) => println!("Sent to '{key}' with attachment {path} (telegram message {message_id})."),
                None => println!("Message sent to '{key}' (telegram message {message_id})."),
            }
            match header_label(rest) {
                Some(l) => println!("  header: [{l}]"),
                None => println!("  header: (none — pass --header \"<topic>\" so the reader knows what this is about)"),
            }
            Ok(())
        }
        "ask" => {
            if rest.is_empty() || rest[0].starts_with("--") {
                return Err("usage: eco telegram ask <key> <question...>".to_string());
            }
            let key = rest[0].clone();
            let want_wait = rest.contains(&"--wait".to_string());
            let timeout = wait_seconds(rest, "--timeout").unwrap_or(180);
            let raw_text = rest[1..]
                .iter()
                .filter(|a| !a.starts_with("--") && !is_flag_value(rest, a))
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
            if raw_text.is_empty() {
                return Err("usage: eco telegram ask <key> <question...>".to_string());
            }
            let (text, parse_mode) = format_message(rest, &raw_text);
            let mut ask_body = serde_json::json!({
                "key": key.clone(),
                "text": text,
                "timeout_secs": timeout,
            });
            if let Some(pm) = &parse_mode {
                ask_body["parse_mode"] = serde_json::json!(pm);
            }
            let v = post_json_auth(&api_url, &api_key, "/v1/telegram/ask", &ask_body)?;
            let id = v
                .get("conversation_id")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            println!("Question sent to '{key}'. (conversation {id}, expires in {timeout}s)");
            if want_wait {
                let answer = poll_conversation(&api_url, &api_key, &id, timeout)?;
                let reply = answer
                    .get("answer")
                    .and_then(|a| a.as_str())
                    .unwrap_or("")
                    .to_string();
                if reply.is_empty() {
                    println!("(question answered, no text)");
                } else {
                    println!("Reply from '{key}': {reply}");
                }
            } else {
                println!("Poll for the reply with: eco telegram status {id} --wait");
            }
            Ok(())
        }
        "status" => {
            if rest.is_empty() || rest[0].starts_with("--") {
                return Err("usage: eco telegram status <conversation_id> [--wait [<secs>]]".to_string());
            }
            let id = rest[0].clone();
            let want_wait = rest.contains(&"--wait".to_string());
            let wait = wait_seconds(rest, "--wait").unwrap_or(180);
            let v = if want_wait {
                poll_conversation(&api_url, &api_key, &id, wait)?
            } else {
                get_json_auth(&api_url, &api_key, &format!("/v1/telegram/conversations/{id}"))?
            };
            let status = v.get("status").and_then(|s| s.as_str()).unwrap_or("");
            let text = v.get("text").and_then(|t| t.as_str()).unwrap_or("");
            let answer = v.get("answer").and_then(|a| a.as_str()).unwrap_or("");
            println!("conversation {id}: {status}");
            println!("  question: {text}");
            if status == "answered" {
                println!("  answer:   {answer}");
            }
            if status == "open" {
                println!("  (still open — `eco telegram status {id} --wait` to block for the reply)");
            }
            Ok(())
        }
        "bind" => {
            if rest.is_empty() || rest[0].starts_with("--") {
                return Err("usage: eco telegram bind <key> [--user <id> | --chat <id>]".to_string());
            }
            let key = rest[0].clone();
            let user = rest
                .iter()
                .position(|a| a == "--user")
                .and_then(|i| rest.get(i + 1))
                .and_then(|v| v.parse::<i64>().ok());
            let chat = rest
                .iter()
                .position(|a| a == "--chat")
                .and_then(|i| rest.get(i + 1))
                .and_then(|v| v.parse::<i64>().ok());
            if user.is_none() && chat.is_none() {
                return Err("usage: eco telegram bind <key> [--user <id> | --chat <id>]".to_string());
            }
            let kind = if chat.is_some() { "group" } else { "private" };
            let mut body = serde_json::json!({ "key": key.clone(), "kind": kind });
            if let Some(u) = user {
                body["telegram_user_id"] = serde_json::json!(u);
            }
            if let Some(c) = chat {
                body["chat_id"] = serde_json::json!(c);
            }
            let v = post_json_auth(&api_url, &api_key, "/v1/telegram/bind", &body)?;
            let bound = v.get("bound").and_then(|b| b.as_bool()).unwrap_or(false);
            if bound {
                println!("'{key}' is bound to a reachable chat.");
            } else {
                println!("'{key}' bound to a user id — but they have not messaged the bot yet (no channel until they do).");
            }
            Ok(())
        }
        "chats" => {
            let _ = read_stored_auth();
            let v = get_json_auth(&api_url, &api_key, "/v1/telegram/bindings")?;
            let bindings = v.get("bindings").and_then(|b| b.as_array());
            if let Some(b) = bindings {
                if b.is_empty() {
                    println!("No telegram bindings yet. Bind one with `eco telegram bind`.");
                }
                for item in b {
                    let key = item.get("key").and_then(|k| k.as_str()).unwrap_or("");
                    let chat = item
                        .get("chat_id")
                        .and_then(|c| c.as_i64())
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "-".into());
                    let user = item
                        .get("telegram_user_id")
                        .and_then(|c| c.as_i64())
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "-".into());
                    println!("  {key:<16} kind={:<8} chat_id={chat:<12} user={user}", 
                        item.get("kind").and_then(|k| k.as_str()).unwrap_or(""));
                }
            }
            Ok(())
        }
        "connect" => {
            let auth = read_stored_auth().ok_or(
                "not logged in — run `eco login` first (the user bot links to your eco account)",
            )?;
            let v = post_json_auth(&auth.api_url, &auth.api_key, "/v1/telegram/connect", &serde_json::json!({}))?;
            let link = v.get("deep_link").and_then(|l| l.as_str()).unwrap_or("");
            if link.is_empty() {
                return Err(format!("no deep link returned: {v}"));
            }
            println!("Opening Telegram to link your account…");
            if !crate::commands::account::open_browser(link) {
                println!("(no browser detected — open this link manually:)");
            } else {
                println!("If it didn't open, use this link:");
            }
            println!("  {link}");
            println!("(link berlaku 15 menit, sekali pakai)");
            Ok(())
        }
        "disconnect" | "unlink" => {
            let auth = read_stored_auth().ok_or("not logged in — run `eco login`")?;
            let v = post_json_auth(&auth.api_url, &auth.api_key, "/v1/telegram/unlink", &serde_json::json!({}))?;
            let removed = v.get("unlinked").and_then(|b| b.as_bool()).unwrap_or(false);
            println!("{}", if removed { "Telegram diputus dari akun." } else { "Tidak ada Telegram yang terhubung." });
            Ok(())
        }
        "me" => {
            let auth = read_stored_auth().ok_or("not logged in — run `eco login`")?;
            let v = get_json_auth(&auth.api_url, &auth.api_key, "/v1/telegram/me")?;
            let linked = v.get("linked").and_then(|b| b.as_bool()).unwrap_or(false);
            println!("akun: {}", v.get("email").and_then(|e| e.as_str()).unwrap_or(""));
            println!("telegram: {}", if linked { "terhubung" } else { "belum terhubung (eco telegram connect)" });
            Ok(())
        }
        _ => Err(help_text()),
    }
}

// True when `arg` is the value that follows a --flag (i.e. should be dropped
// from the free-text join). A tiny helper to keep ask-text parsing honest.
fn is_flag_value(rest: &[String], arg: &String) -> bool {
    rest.windows(2).any(|w| {
        w[0].starts_with("--") && &w[1] == arg
    })
}
