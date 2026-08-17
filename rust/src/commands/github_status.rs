// `eco setgithubstatus` — set the authenticated GitHub user's profile status
// (the small status line under your name), e.g. "shipping eco v0.3.8 🚀".
//
// GitHub removed the REST /user/status endpoint (404 now); the status is
// updated via the GraphQL `changeUserStatus` mutation instead. Uses the same
// token the CLI uses for repo operations: ECO_GITHUB_API_KEY, or
// GITHUB_SWDEV_ECOSPHERE_API_KEY / GITHUB_TOKEN as fallbacks.

use crate::util;

fn help_text() {
    let text = r#"eco setgithubstatus [--clear] <message>

Sets the authenticated GitHub user's profile status (shown under your name
on GitHub). Message may include an emoji (e.g. "shipping eco v0.3.8 🚀").

Usage:
  eco setgithubstatus "shipping eco v0.3.8 🚀"   set the status
  eco setgithubstatus --clear                    clear the status

Token (first one found in the environment wins):
  ECO_GITHUB_API_KEY
  GITHUB_SWDEV_ECOSPHERE_API_KEY
  GITHUB_TOKEN

If none of these is set, the command explains which one to export.
"#;
    print!("{text}");
}

fn resolve_token() -> Result<String, String> {
    // GitHub user status requires the `user` scope (GraphQL changeUserStatus).
    // Prefer the token most likely to be a personal account with that scope;
    // ECO_GITHUB_API_KEY is a repo-scoped publish token and is tried last.
    for key in ["GITHUB_SWDEV_ECOSPHERE_API_KEY", "GITHUB_TOKEN", "ECO_GITHUB_API_KEY"] {
        let value = util::env_var_or(key, "");
        if !value.is_empty() {
            return Ok(value);
        }
    }
    Err(
        "No GitHub token found. Set one of: GITHUB_SWDEV_ECOSPHERE_API_KEY, \
         GITHUB_TOKEN, or ECO_GITHUB_API_KEY."
        .to_string(),
    )
}

pub fn run_setgithubstatus(args: &[String]) -> Result<(), String> {
    if matches!(args.first().map(|s| s.as_str()), Some("help") | Some("--help") | Some("-h")) {
        help_text();
        return Ok(());
    }

    let clear = args.iter().any(|a| a == "--clear");
    let message: Vec<String> = args.iter().filter(|a| *a != "--clear").cloned().collect();
    if message.is_empty() && !clear {
        return Err("No status message given. Usage: eco setgithubstatus \"<message>\" (or --clear).".to_string());
    }
    let message_text = message.join(" ");

    let token = resolve_token()?;

    // Sanity-check the token before mutating anything.
    let viewer = graphql_query(
        &token,
        r#"{ viewer { login } }"#,
        serde_json::json!({}),
    )?;
    let login = viewer
        .get("viewer")
        .and_then(|v| v.get("login"))
        .and_then(|l| l.as_str())
        .unwrap_or("(unknown)")
        .to_string();

    let query = if clear {
        "mutation ChangeUserStatus($msg: String) { changeUserStatus(input: {message: $msg}) { status { message } } }"
    } else {
        "mutation ChangeUserStatus($msg: String!) { changeUserStatus(input: {message: $msg}) { status { message } } }"
    };
    let variables = if clear {
        serde_json::json!({})
    } else {
        serde_json::json!({"msg": message_text})
    };
    let result = graphql_query(&token, query, variables)?;
    let new_status = result
        .get("changeUserStatus")
        .and_then(|c| c.get("status"))
        .and_then(|s| s.get("message"))
        .and_then(|m| m.as_str())
        .map(|m| m.to_string());

    match new_status {
        Some(s) => {
            if s.is_empty() {
                util::println_stdout(&format!("Cleared GitHub status for @{login}."));
            } else {
                util::println_stdout(&format!("@{login} status → {s}"));
            }
            Ok(())
        }
        None => {
            if clear {
                util::println_stdout(&format!("Cleared GitHub status for @{login}."));
                Ok(())
            } else {
                Err("GitHub did not confirm the status update.".to_string())
            }
        }
    }
}

fn graphql_query(token: &str, query: &str, variables: serde_json::Value) -> Result<serde_json::Value, String> {
    let body = serde_json::json!({
        "query": query,
        "variables": variables,
    });
    let response = ureq::post("https://api.github.com/graphql")
        .set("Authorization", &format!("Bearer {token}"))
        .set("User-Agent", "eco-cli")
        .set("Content-Type", "application/json")
        .send_string(&serde_json::to_string(&body).unwrap_or_default());
    match response {
        Ok(resp) => {
            let text = resp.into_string().unwrap_or_default();
            let value: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
            if let Some(errors) = value.get("errors").and_then(|e| e.as_array()) {
                let first = errors
                    .first()
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown GraphQL error");
                return Err(first.to_string());
            }
            Ok(value.get("data").cloned().unwrap_or(serde_json::Value::Null))
        }
        Err(ureq::Error::Status(code, resp)) => {
            let text = resp.into_string().unwrap_or_default();
            Err(format!("GitHub GraphQL returned HTTP {code}: {}", text.chars().take(200).collect::<String>()))
        }
        Err(ureq::Error::Transport(t)) => Err(format!("GitHub GraphQL transport error: {t}")),
    }
}
