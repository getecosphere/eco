use crate::embedded;
use crate::util;

const INSTALLERS: &[(&str, &str)] = &[
    ("minio", "install-minio.sh"),
    ("onnxruntime", "install-onnxruntime.sh"),
];

fn install_help() {
    let text = r#"eco install

Install infra-level tooling that isn't tied to any one project's
ecompose.yml -- run once per machine/CT, shared by every estate on it.

Usage:
  eco install minio

  minio         Installs MinIO (prebuilt binary) and starts it running locally.
                Prints the endpoint/credentials to paste into "eco startproject"'s
                object storage prompt, or directly into an existing
                ecompose.yml's storage.minio block.
  onnxruntime   Installs the onnxruntime shared library used by RAG/embedding
                services (rag domain). On Linux/CTs it is placed at
                /opt/eco-tools/libonnxruntime.so; on macOS via Homebrew.
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
            let status = std::process::Command::new("bash")
                .arg(&script_path)
                .stdin(std::process::Stdio::inherit())
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit())
                .current_dir(&cwd)
                .status()
                .map_err(|e| format!("Cannot run {script_name}: {e}"))?;
            if status.success() {
                Ok(())
            } else {
                Err(crate::util::describe_status(script_name, &status))
            }        }
    }
}
