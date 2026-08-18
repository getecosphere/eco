use std::collections::HashMap;
use std::env;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

/// Wire protocol version between the `eco up --remote` client and the
/// `eco serve` agent. This is an integer that bumps ONLY when the deploy
/// contract changes incompatibly — the payload tarball layout, the endpoint
/// shapes, or required request headers. It is NOT the semver release version.
///
/// The agent and the client must speak the same protocol: the agent rejects
/// deploy requests whose `X-Eco-Protocol` does not match, so a stale client
/// can't ship a payload the agent mis-reads (e.g. the artifacts/bin layout
/// change). Bump this when you change what a deploy payload/request means.
pub const PROTOCOL_VERSION: u32 = 5;

/// HTTP header the client sends to declare its protocol version, and the
/// semver release it was built from (for the upgrade hint in error messages).
pub const PROTOCOL_HEADER: &str = "x-eco-protocol";
pub const CLIENT_VERSION_HEADER: &str = "x-eco-client-version";

/// Human-friendly upgrade instruction shown when client/agent protocols differ.
pub fn protocol_mismatch_msg(client: &str, client_semver: &str, agent_semver: &str) -> String {
    format!(
        "Your eco ({client_semver}) speaks deploy protocol {client}, but the agent ({agent_semver}) speaks {PROTOCOL_VERSION}. \
These are incompatible — a stale client can silently mis-deploy. Run `eco update` (or reinstall from getecosphere.com) and retry."
    )
}

/// Human-readable summary of a process exit: "terminated by signal N" on
/// Unix, otherwise "exited with code N". Portable across Windows.
pub fn describe_status(command: &str, status: &std::process::ExitStatus) -> String {
    #[cfg(unix)]
    {
        if let Some(signal) = status.signal() {
            return format!("{command} terminated by signal {signal}");
        }
    }
    format!("{command} exited with code {}", status.code().unwrap_or(-1))
}

/// Set the executable bit on a file. Unix-only; Windows uses the `.exe`
/// extension, so this is a no-op there.
#[cfg(unix)]
pub fn make_executable(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
}

#[cfg(not(unix))]
pub fn make_executable(_path: &std::path::Path) {}

/// True when stdout is a TTY and NO_COLOR is unset.
pub fn color_enabled() -> bool {
    env::var("NO_COLOR").is_err() && std::io::stdout().is_terminal()
}

pub fn bold(s: &str) -> String {
    if color_enabled() {
        format!("\x1b[1m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn dim(s: &str) -> String {
    if color_enabled() {
        format!("\x1b[2m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn cyan(s: &str) -> String {
    if color_enabled() {
        format!("\x1b[36m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn yellow(s: &str) -> String {
    if color_enabled() {
        format!("\x1b[33m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn green(s: &str) -> String {
    if color_enabled() {
        format!("\x1b[32m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn red(s: &str) -> String {
    if color_enabled() {
        format!("\x1b[31m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn sep(n: usize) -> String {
    if color_enabled() {
        format!("\x1b[2m{}\x1b[0m", "─".repeat(n))
    } else {
        "─".repeat(n)
    }
}

pub fn cmd_bold_cyan(s: &str) -> String {
    if color_enabled() {
        format!("\x1b[1m\x1b[36m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

/// Wrapper around std::io::IsTerminal so `use` sites stay clean.
pub trait IsTerminal {
    fn is_terminal(&self) -> bool;
}

impl IsTerminal for std::io::Stdout {
    fn is_terminal(&self) -> bool {
        std::io::IsTerminal::is_terminal(self)
    }
}

impl IsTerminal for std::io::Stdin {
    fn is_terminal(&self) -> bool {
        std::io::IsTerminal::is_terminal(self)
    }
}

/// A captured child process result.
#[derive(Debug, Clone)]
pub struct Captured {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Run a command inheriting stdio. Returns Err on signal or non-zero exit.
pub fn run_command(command: &str, args: &[String], cwd: &Path) -> Result<(), String> {
    run_command_env(command, args, cwd, &env::vars().collect::<HashMap<_, _>>())
}

pub fn run_command_env(
    command: &str,
    args: &[String],
    cwd: &Path,
    env_map: &HashMap<String, String>,
) -> Result<(), String> {
    let mut child = build_command(command, args, cwd, env_map).spawn().map_err(|e| {
        format!("Unable to run {command}: {e}")
    })?;
    let status = child.wait().map_err(|e| format!("{command} wait failed: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(describe_status(command, &status))
    }
}

/// True on Unix only; Windows signals are not supported this way.
#[cfg(unix)]
pub fn child_kill(pid: i32, signal: i32) -> bool {
    // returns true if the process exists (signal 0) or was signaled
    let result = unsafe { libc::kill(pid, signal) };
    result == 0
}

/// Windows fallback: no libc kill, so treat as "not found".
#[cfg(not(unix))]
pub fn child_kill(pid: i32, _signal: i32) -> bool {
    let _ = pid;
    false
}

/// Run a command capturing stdout/stderr (both piped), stdio stdin ignored.
pub fn run_capture(command: &str, args: &[String], cwd: &Path) -> Result<Captured, String> {
    run_capture_env(command, args, cwd, &env::vars().collect::<HashMap<_, _>>())
}

pub fn run_capture_env(
    command: &str,
    args: &[String],
    cwd: &Path,
    env_map: &HashMap<String, String>,
) -> Result<Captured, String> {
    let child = build_command(command, args, cwd, env_map)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Unable to run {command}: {e}"))?;
    let output = child.wait_with_output().map_err(|e| format!("{command} wait failed: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    Ok(Captured {
        code: output.status.code().unwrap_or(-1),
        stdout,
        stderr,
    })
}

fn build_command(
    command: &str,
    args: &[String],
    cwd: &Path,
    env_map: &HashMap<String, String>,
) -> Command {
    let mut cmd = Command::new(command);
    cmd.args(args);
    cmd.current_dir(cwd);
    cmd.env_clear();
    for (k, v) in env_map {
        cmd.env(k, v);
    }
    cmd
}

/// Capture helper that resolves exit code to a `Result`: Err carries stderr.
pub fn run_capture_or_err(command: &str, args: &[String], cwd: &Path) -> Result<String, String> {
    let result = run_capture(command, args, cwd)?;
    if result.code != 0 {
        return Err(result.stderr.trim().to_string());
    }
    Ok(result.stdout.trim().to_string())
}

/// Read an env var value from a `KEY=value` dotenv-style text block.
pub fn read_env_value(content: &str, key: &str) -> String {
    for line in content.split('\n') {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix(&format!("{key}=")) {
            return rest.trim().to_string();
        }
    }
    String::new()
}

/// Read an env var value as an Option (None when the key is absent or empty).
pub fn read_env_value_opt(content: &str, key: &str) -> Option<String> {
    let value = read_env_value(content, key);
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

/// List a directory's entries sorted by file name (Node's readdir returns a
/// deterministic order; std read_dir does not, which would break command
/// output parity across commands that walk estates).
pub fn sorted_dir_entries(dir: &Path) -> Vec<std::fs::DirEntry> {
    let mut entries: Vec<std::fs::DirEntry> = std::fs::read_dir(dir)
        .map(|e| e.flatten().collect())
        .unwrap_or_default();
    entries.sort_by_key(|e| e.file_name());
    entries
}

/// List immediate child directories sorted by name.
pub fn sorted_subdirs(dir: &Path) -> Vec<std::fs::DirEntry> {
    sorted_dir_entries(dir)
        .into_iter()
        .filter(|e| e.path().is_dir())
        .collect()
}

/// Check whether a command exists on PATH.
pub fn command_on_path(command: &str) -> bool {
    which_capture(command).code == 0
}

pub fn which_capture(command: &str) -> Captured {
    match run_capture("which", &[command.to_string()], &current_dir()) {
        Ok(c) => c,
        Err(_) => Captured { code: 1, stdout: String::new(), stderr: String::new() },
    }
}

pub fn current_dir() -> PathBuf {
    env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

pub fn home_dir() -> String {
    env::var("HOME").unwrap_or_else(|_| "/root".to_string())
}

pub fn env_var_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

pub fn hostname() -> String {
    match run_capture("hostname", &[], &current_dir()) {
        Ok(c) if c.code == 0 => c.stdout.trim().to_string(),
        _ => "localhost".to_string(),
    }
}

/// Shell-single-quote a value for embedding in a bash command string.
pub fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Shell-escape for embedding inside a double-quoted ssh command string.
pub fn shell_quote_double(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('`', "\\`")
}

pub fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| format!("\"{}\"", value))
}

pub fn strip_quotes(value: &str) -> String {
    let trimmed = value.trim();
    if (trimmed.starts_with('"') && trimmed.ends_with('"'))
        || (trimmed.starts_with('\'') && trimmed.ends_with('\''))
    {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn sleep_ms(ms: u64) {
    std::thread::sleep(std::time::Duration::from_millis(ms));
}

pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub fn println_stdout(s: &str) {
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{s}");
}

pub fn print_stdout(s: &str) {
    let mut out = std::io::stdout();
    let _ = write!(out, "{s}");
    let _ = out.flush();
}

pub fn eprintln_stderr(s: &str) {
    eprintln!("{s}");
}

pub fn parse_github_coordinates(remote_url: &str) -> Result<(String, String), String> {
    let mut normalized = remote_url
        .replace("ssh://", "")
        .replace("git@", "")
        .trim_start_matches("https://")
        .to_string();
    // strip any host prefix like github.com: (handled by the generic cleanup)
    if let Some(idx) = normalized.find(':') {
        let (left, right) = normalized.split_at(idx);
        if !left.contains('/') && !right[1..].contains('/') {
            // e.g. git@github.com:owner/repo -> owner/repo
        }
        // ssh form handled below
    }
    normalized = normalized
        .trim_start_matches('/')
        .trim_end_matches(".git")
        .to_string();
    // For git@host:owner/repo we removed "git@" already -> host:owner/repo
    if let Some(idx) = normalized.find(':') {
        normalized = normalized[idx + 1..].to_string();
    }
    // For https://host/owner/repo we stripped scheme+host already.
    let parts: Vec<&str> = normalized.split('/').collect();
    if parts.len() < 2 || parts[0].is_empty() || parts[1].is_empty() {
        return Err(format!("Cannot parse GitHub repo coordinates from remote URL: {remote_url}"));
    }
    Ok((parts[0].to_string(), parts[1].to_string()))
}

pub fn parse_args_flagged(args: &[String]) -> (Vec<String>, HashMap<String, String>) {
    let mut positionals = Vec::new();
    let mut options: HashMap<String, String> = HashMap::new();
    let mut i = 0;
    while i < args.len() {
        let token = &args[i];
        if let Some(rest) = token.strip_prefix("--") {
            if let Some(eq) = rest.find('=') {
                options.insert(rest[..eq].to_string(), rest[eq + 1..].to_string());
            } else {
                let next = args.get(i + 1);
                if let Some(n) = next {
                    if !n.starts_with("--") {
                        options.insert(rest.to_string(), n.clone());
                        i += 1;
                    } else {
                        options.insert(rest.to_string(), "true".to_string());
                    }
                } else {
                    options.insert(rest.to_string(), "true".to_string());
                }
            }
        } else {
            positionals.push(token.clone());
        }
        i += 1;
    }
    (positionals, options)
}

pub fn parse_args_flag_values(args: &[String]) -> HashMap<String, String> {
    parse_args_flagged(args).1
}

/// Split flag-style args keeping bare flags as "true" values (ct.js style).
pub fn parse_ct_options(args: &[String]) -> (Vec<String>, HashMap<String, String>) {
    let mut options: HashMap<String, String> = HashMap::new();
    let mut positionals = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if let Some(key) = arg.strip_prefix("--") {
            if key == "start" || key == "dry-run" || key == "keep-clone" {
                options.insert(key.to_string(), "true".to_string());
            } else {
                let next = args.get(i + 1);
                if let Some(n) = next {
                    if !n.starts_with("--") {
                        options.insert(key.to_string(), n.clone());
                        i += 1;
                    } else {
                        return (positionals, options); // missing value
                    }
                } else {
                    return (positionals, options);
                }
            }
        } else {
            positionals.push(arg.clone());
        }
        i += 1;
    }
    (positionals, options)
}

/// True if any arg equals one of the given tokens.
pub fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

pub fn has_flag_prefix(args: &[String], prefix: &str) -> bool {
    args.iter().any(|a| a.starts_with(prefix))
}

pub fn to_bool(value: &str) -> bool {
    matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

pub fn is_numeric(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|c| c.is_ascii_digit())
}

/// Platform of the current machine: darwin | linux.
pub fn platform() -> String {
    if cfg!(target_os = "macos") {
        "darwin".to_string()
    } else {
        "linux".to_string()
    }
}

/// Architecture: x64 | arm64.
pub fn arch() -> String {
    match std::env::consts::ARCH {
        "x86_64" => "x64".to_string(),
        "aarch64" => "arm64".to_string(),
        other => other.to_string(),
    }
}

/// Spawn a child in the background inheriting stdio; returns the Child.
pub fn spawn_inherit(command: &str, args: &[String], cwd: &Path) -> Result<Child, String> {
    Command::new(command)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("Unable to run {command}: {e}"))
}
