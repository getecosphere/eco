use crate::embedded;
use crate::util;

const INSTALLERS: &[(&str, &str)] = &[
    ("minio", "install-minio.sh"),
    ("onnxruntime", "install-onnxruntime.sh"),
    ("cloudflared", "install-cloudflared.sh"),
];

fn install_help() {
    let text = r#"eco install

Install infra-level tooling that isn't tied to any one project's
ecompose.yml -- run once per machine/CT, shared by every estate on it.

Usage:
  eco install minio

  minio         Installs MinIO (prebuilt binary) and starts it locally.
                Credentials are written to Eco's private client config and
                injected from a storage.minio declaration; they are never
                printed or pasted into ecompose.yml.
  onnxruntime   Installs the onnxruntime shared library used by RAG/embedding
                services (rag domain). On Linux/CTs it is placed at
                /opt/eco-tools/libonnxruntime.so; on macOS via Homebrew.
  cloudflared   Installs the cloudflared tunnel binary (prebuilt from Cloudflare).
                Used by "eco serve" for temporary public *.getecosphere.com URLs
                and by the proxy CT for managed Cloudflare tunnels.
"#;
    print!("{text}");
}

pub fn run_install(args: &[String]) -> Result<(), String> {
    let tool = args.first().cloned();
    match tool.as_deref() {
        None | Some("help") | Some("--help") | Some("-h") => {
            install_help();
            return Ok(());
        }
        Some(tool) => {
            let script_name = INSTALLERS
                .iter()
                .find(|(name, _)| *name == tool)
                .map(|(_, script)| *script)
                .ok_or_else(|| {
                    format!("Unknown install target: {tool}\n\nRun \"eco install help\" for usage.")
                })?;
            let script_path = embedded::bundled_script_path(script_name)?;
            let cwd = util::current_dir();
            let mut command = std::process::Command::new("bash");
            command
                .arg(&script_path)
                .stdin(std::process::Stdio::inherit())
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit())
                .current_dir(&cwd);
            // `storage.minio` is an Eco-managed capability: the caller never
            // needs to handle its credential values. Keep setup idempotent and
            // non-printing even when invoked manually from the CLI.
            if tool == "minio" {
                command.arg("--ensure");
            }
            let status = command
                .status()
                .map_err(|e| format!("Cannot run {script_name}: {e}"))?;
            if status.success() {
                Ok(())
            } else {
                Err(crate::util::describe_status(script_name, &status))
            }        }
    }
}
