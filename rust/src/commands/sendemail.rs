use crate::util;
use std::io::IsTerminal;

fn help_text() {
    let text = r#"eco sendemail

Usage:
  eco sendemail --to <recipient> --subject <text> [--html <html>] [--host <alias>]
  eco sendemail --to <recipient> --subject <text> --file <path>      (read html from file)
  echo "<html>" | eco sendemail --to <recipient> --subject <text>    (read html from stdin)

Sends a transactional email via Brevo using the Proxmox host's credentials
(BREVO_API_KEY, MAIL_FROM_EMAIL, MAIL_FROM_NAME from /root/.bashrc).

Options:
  --to <email>        Recipient email address (required)
  --subject <text>    Email subject (required)
  --html <html>       HTML body (if not using --file or stdin)
  --file <path>       Read HTML body from a local file
  --host <hostname>   SSH host for the Proxmox host (default: prox)

Defaults when called from AI context:
  --to defaults to kampusrwids@gmail.com when the user says "send email to me"
  --from uses MAIL_FROM_EMAIL from the host (no-reply@jogjaitcamp.com)

Examples:
  eco sendemail --to dev@example.com --subject "Deploy done" --html "<p>OK</p>"
  eco sendemail --to user@test.com --subject "Report" --file ./report.html
  echo "<h1>Hello</h1>" | eco sendemail --to me@test.com --subject "Status"
"#;
    print!("{text}");
}

fn shell(ssh_host: &str, cmd: &str) -> Result<String, String> {
    let cwd = util::current_dir();
    let result = util::run_capture("ssh", &[ssh_host.to_string(), cmd.to_string()], &cwd)?;
    if result.code != 0 {
        return Err(format!("ssh exited {}: {}", result.code, result.stderr.trim()));
    }
    Ok(result.stdout.trim().to_string())
}

fn resolve_brevo_credential(host: &str, key_name: &str) -> Result<String, String> {
    let cmd = format!("bash -lc 'echo ${{{key_name}}}'");
    shell(host, &cmd)
}

pub fn run_sendemail(args: &[String]) -> Result<(), String> {
    if matches!(args.first().map(|s| s.as_str()), Some("help") | Some("--help") | Some("-h")) {
        help_text();
        return Ok(());
    }

    let mut options: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let Some(key) = arg.strip_prefix("--") else {
            return Err(format!("Unexpected argument: {arg}"));
        };
        let value = args.get(i + 1).cloned();
        match value {
            Some(v) if !v.starts_with("--") => {
                options.insert(key.to_string(), v);
                i += 1;
            }
            _ => return Err(format!("Missing value for option --{key}")),
        }
        i += 1;
    }

    let to = options
        .get("to")
        .cloned()
        .ok_or("Both --to and --subject are required. Run `eco sendemail help` for usage.")?;
    let subject = options
        .get("subject")
        .cloned()
        .ok_or("Both --to and --subject are required. Run `eco sendemail help` for usage.")?;

    let mut html = options.get("html").cloned().unwrap_or_default();

    if let Some(file) = options.get("file") {
        html = std::fs::read_to_string(file)
            .map_err(|e| format!("Cannot read {file}: {e}"))?;
    }

    if html.is_empty() && !std::io::stdin().is_terminal() {
        use std::io::Read;
        let mut buf = Vec::new();
        std::io::stdin().read_to_end(&mut buf).map_err(|e| e.to_string())?;
        html = String::from_utf8_lossy(&buf).to_string();
    }

    if html.is_empty() {
        return Err("No HTML body provided. Use --html, --file, or pipe content via stdin.".to_string());
    }

    let host = options.get("host").cloned().unwrap_or_else(|| "prox".to_string());

    util::println_stdout(&format!("Sending email to {to}…"));

    let api_key = resolve_brevo_credential(&host, "BREVO_API_KEY")?;
    let from_email = resolve_brevo_credential(&host, "MAIL_FROM_EMAIL")?;
    let from_name = resolve_brevo_credential(&host, "MAIL_FROM_NAME")?;

    if api_key.is_empty() {
        return Err("BREVO_API_KEY is not set on the Proxmox host.".to_string());
    }
    if from_email.is_empty() {
        return Err("MAIL_FROM_EMAIL is not set on the Proxmox host.".to_string());
    }

    let payload = serde_json::json!({
        "sender": { "name": if from_name.is_empty() { "Eco" } else { from_name.as_str() }, "email": from_email },
        "to": [{ "email": to }],
        "subject": subject,
        "htmlContent": html
    });
    let payload_str = serde_json::to_string(&payload).unwrap_or_default();
    let quoted = payload_str.replace('\'', "'\\''");
    let curl_cmd = format!(
        "curl -s -w '\\n%{{http_code}}' -X POST https://api.brevo.com/v3/smtp/email \
-H 'api-key: {api_key}' \
-H 'Content-Type: application/json' \
-d '{quoted}'"
    );

    let raw = shell(&host, &curl_cmd)?;
    let lines: Vec<&str> = raw.split('\n').collect();
    let status_code = lines.last().unwrap_or(&"").trim().to_string();
    let body = lines[..lines.len().saturating_sub(1)].join("\n").trim().to_string();

    if status_code == "201" || status_code == "202" {
        let message_id = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("messageId").and_then(|m| m.as_str()).map(|s| s.to_string()))
            .unwrap_or_default();
        if message_id.is_empty() {
            util::println_stdout("Email sent successfully.");
        } else {
            util::println_stdout(&format!("Email sent successfully (messageId: {message_id})."));
        }
        Ok(())
    } else {
        Err(format!("Brevo returned HTTP {status_code}: {body}"))
    }
}
