use crate::ecompose;
use crate::util;
use std::path::Path;

const K6_VERSION: &str = "0.54.0";

struct K6Download {
    key: &'static str,
    url: String,
    extract: String,
}

fn k6_downloads() -> Vec<K6Download> {
    vec![
        K6Download {
            key: "linux-x64",
            url: format!("https://github.com/grafana/k6/releases/download/v{K6_VERSION}/k6-v{K6_VERSION}-linux-amd64.tar.gz"),
            extract: format!("k6-v{K6_VERSION}-linux-amd64/k6"),
        },
        K6Download {
            key: "darwin-x64",
            url: format!("https://github.com/grafana/k6/releases/download/v{K6_VERSION}/k6-v{K6_VERSION}-macos-amd64.zip"),
            extract: format!("k6-v{K6_VERSION}-macos-amd64/k6"),
        },
        K6Download {
            key: "darwin-arm64",
            url: format!("https://github.com/grafana/k6/releases/download/v{K6_VERSION}/k6-v{K6_VERSION}-macos-arm64.zip"),
            extract: format!("k6-v{K6_VERSION}-macos-arm64/k6"),
        },
    ]
}

fn tools_dir() -> String {
    format!("{}/.eco/tools", util::home_dir())
}

fn k6_bin_path() -> String {
    format!("{}/k6", tools_dir())
}

fn platform_key() -> String {
    format!("{}-{}", util::platform(), util::arch())
}

#[derive(Default)]
struct StressOptions {
    vus: String,
    duration: String,
    ramp_up: String,
    hostname: Option<String>,
    dry_run: bool,
}

fn parse_options(args: &[String]) -> Result<(StressOptions, Vec<String>), String> {
    let mut options = StressOptions {
        vus: "100".to_string(),
        duration: "30s".to_string(),
        ramp_up: "10s".to_string(),
        hostname: None,
        dry_run: false,
    };
    let mut positionals = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--dry-run" {
            options.dry_run = true;
            i += 1;
            continue;
        }
        if !arg.starts_with("--") {
            positionals.push(arg.clone());
            i += 1;
            continue;
        }
        let key = arg[2..].to_string();
        let value = args.get(i + 1).cloned().ok_or_else(|| format!("Missing value for option {arg}"))?;
        if value.starts_with("--") {
            return Err(format!("Missing value for option {arg}"));
        }
        match key.as_str() {
            "vus" => options.vus = value,
            "duration" => options.duration = value,
            "ramp-up" => options.ramp_up = value,
            "hostname" => options.hostname = Some(value),
            other => return Err(format!("Unknown option: --{other}")),
        }
        i += 2;
    }
    Ok((options, positionals))
}

fn k6_script(target_url: &str, vus: &str, duration: &str, ramp_up: &str) -> String {
    let url = target_url.trim_end_matches('/').to_string();
    format!(
        r#"import http from "k6/http";
import {{ check, sleep }} from "k6";

export const options = {{
  stages: [
    {{ duration: "{ramp_up}", target: {vus} }},
    {{ duration: "{duration}", target: {vus} }},
    {{ duration: "{ramp_up}", target: 0 }},
  ],
  thresholds: {{
    http_req_duration: ["p(95)<2000"],
    http_req_failed: ["rate<0.05"],
  }},
  summaryTimeUnit: "ms",
}};

const BASE = "{url}";

export default function () {{
  const pageRes = http.get(BASE + "/");
  check(pageRes, {{
    "GET / status 2xx": (r) => r.status >= 200 && r.status < 400,
  }});

  sleep(2);

  const apiRes = http.get(BASE + "/api");
  check(apiRes, {{
    "GET /api request made": (r) => r.status < 500,
  }});

  sleep(1);

  const authRes = http.get(BASE + "/auth-api");
  check(authRes, {{
    "GET /auth-api request made": (r) => r.status < 500,
  }});

  sleep(2);
}}

export function handleSummary(data) {{
  const m = data.metrics;
  const dr = m["http_req_duration"] ? m["http_req_duration"].values : null;
  const fr = m["http_req_failed"] ? m["http_req_failed"].values : null;
  const ch = m.checks ? m.checks.values : null;
  const summary = {{
    timestamp: new Date().toISOString(),
    target_url: BASE,
    virtual_users: {vus},
    peak_duration_s: "{duration}",
    ramp_up_s: "{ramp_up}",
    http_reqs: m.http_reqs ? m.http_reqs.values.count : 0,
    http_req_duration_avg_ms: dr ? dr.avg.toFixed(2) : 0,
    http_req_duration_p95_ms: dr ? dr["p(95)"].toFixed(2) : 0,
    http_req_failed_rate: fr ? fr.rate.toFixed(4) : 0,
    checks_passed: ch ? ch.passes : 0,
    checks_total: ch ? (ch.passes + ch.fails) : 0,
  }};
  return {{
    "stdout": JSON.stringify(summary, null, 2),
  }};
}}
"#
    )
}

fn download_file(url: &str, dest: &str) -> Result<(), String> {
    let req = ureq::get(url);
    let response = req.call().map_err(|e| format!("Download failed: {e} from {url}"))?;
    let mut bytes = Vec::new();
    use std::io::Read;
    let mut reader = response.into_reader();
    reader.read_to_end(&mut bytes).map_err(|e| format!("read download: {e}"))?;
    std::fs::write(dest, bytes).map_err(|e| format!("write download: {e}"))
}

fn run_capture_quiet(command: &str, args: &[String], cwd: &Path) -> Result<String, String> {
    let result = util::run_capture(command, args, cwd)?;
    if result.code != 0 {
        let detail = if !result.stderr.trim().is_empty() { result.stderr.trim() } else { result.stdout.trim() };
        return Err(format!("{command} exited with code {}: {detail}", result.code));
    }
    Ok(result.stdout.trim().to_string())
}

fn ensure_k6() -> Result<String, String> {
    let bin_path = k6_bin_path();
    if std::path::Path::new(&bin_path).exists() {
        if let Ok(version) = run_capture_quiet(&bin_path, &["version".to_string()], &util::current_dir()) {
            eprintln!("k6 found: {version}");
            return Ok(bin_path);
        }
    }
    eprintln!("k6 not found. Provisioning...");

    let key = platform_key();
    let downloads = k6_downloads();
    let download = downloads
        .iter()
        .find(|d| d.key == key)
        .ok_or_else(|| {
            format!(
                "No k6 binary available for {}/{}.\nSupported platforms: linux-x64, darwin-x64, darwin-arm64.",
                util::platform(),
                util::arch()
            )
        })?;
    let url = &download.url;
    let extract = &download.extract;
    let ext = if url.ends_with(".zip") { ".zip" } else { ".tar.gz" };
    let archive_path = format!("{}/k6-{K6_VERSION}{ext}", tools_dir());
    std::fs::create_dir_all(tools_dir()).map_err(|e| e.to_string())?;

    eprintln!("Downloading k6 v{K6_VERSION} for {key}...");
    if let Err(e) = download_file(url, &archive_path) {
        let _ = std::fs::remove_file(&archive_path);
        return Err(format!("Failed to download k6: {e}"));
    }

    let cwd = util::current_dir();
    let extract_result = if ext == ".zip" {
        run_capture_quiet("unzip", &["-o".to_string(), archive_path.clone(), "-d".to_string(), tools_dir()], &cwd)
    } else {
        run_capture_quiet("tar", &["-xzf".to_string(), archive_path.clone(), "-C".to_string(), tools_dir()], &cwd)
    };
    if let Err(e) = extract_result {
        let _ = std::fs::remove_file(&archive_path);
        return Err(format!("Failed to extract k6 archive: {e}"));
    }

    let inner_path = format!("{}/{extract}", tools_dir());
    if !std::path::Path::new(&inner_path).exists() {
        return Err(format!("k6 binary not found at expected path after extraction: {inner_path}"));
    }
    run_capture_quiet("mv", &[inner_path.clone(), bin_path.clone()], &cwd)?;
    util::make_executable(std::path::Path::new(&bin_path));
    let _ = std::fs::remove_file(&archive_path);

    let version = run_capture_quiet(&bin_path, &["version".to_string()], &cwd)?;
    eprintln!("k6 provisioned: {version}");
    Ok(bin_path)
}

pub fn run_stress(args: &[String]) -> Result<(), String> {
    let (options, positionals) = parse_options(args)?;
    let input = positionals.first().cloned().unwrap_or_else(|| ".".to_string());

    let hostname;
    let mut file_path = String::new();
    if let Some(h) = &options.hostname {
        hostname = h.clone();
    } else {
        let deployment = ecompose::read_ecompose(&input, &util::current_dir())?;
        file_path = deployment.file_path.display().to_string();
        let expose = ecompose::parse_expose(&deployment.content);
        hostname = expose.hostname();
    }

    if hostname.is_empty() {
        return Err(
            "No hostname found. Set expose.hostname in ecompose.yml or pass --hostname.".to_string()
        );
    }

    let target_url = format!("https://{hostname}");

    if options.dry_run {
        let mut lines = vec![
            format!("Target:    {target_url}"),
            format!("VUs:       {}", options.vus),
            format!("Ramp-up:   {}", options.ramp_up),
            format!("Duration:  {}", options.duration),
            format!("k6 bin:    {}", k6_bin_path()),
        ];
        if !file_path.is_empty() {
            lines.push(format!("Manifest:  {file_path}"));
        }
        util::println_stdout(&lines.join("\n"));
        return Ok(());
    }

    eprintln!("Stress-testing {target_url}\n  VUs: {}  ramp-up: {}  duration: {}\n", options.vus, options.ramp_up, options.duration);

    let k6 = ensure_k6()?;
    let script_path = format!("{}/stress-test.js", tools_dir());
    let script = k6_script(&target_url, &options.vus, &options.duration, &options.ramp_up);
    std::fs::write(&script_path, script).map_err(|e| e.to_string())?;

    eprintln!("Running k6...\n");
    let status = std::process::Command::new(&k6)
        .args(["run".to_string(), "--quiet".to_string(), script_path])
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|e| format!("k6: {e}"))?;
    if !status.success() {
        return Err(format!("k6 exited with code {}", status.code().unwrap_or(-1)));
    }
    Ok(())
}
