use crate::ecompose;
use crate::util;
use std::path::Path;

fn ct_help() {
    let text = r#"eco ct

Manage Proxmox CT lifecycle from eco.

Usage:
  eco ct create <project> [overrides]
  eco ct start <project|ctid>
  eco ct stop <project|ctid>
  eco ct reboot <project|ctid>
  eco ct status <project|ctid>
  eco ct template <project|ctid> --name <name> --clone-id <ctid> [options]

Create override options:
  --id <ctid>             Override CT ID from ecompose.yml
  --hostname <name>       Override CT hostname
  --cores <n>             Override cores
  --memory <mb>           Override memory
  --swap <mb>             Override swap
  --disk <gb>             Override root disk size in GB
  --storage <name>        Override rootfs storage, e.g. local-lvm
  --template <ref>        Override template reference for pct create
  --bridge <name>         Override network bridge, e.g. vmbr0
  --ip <value>            Override IP, default from ecompose.yml
  --gateway <value>       Optional gateway when using static IP
  --unprivileged <0|1>    Override unprivileged flag
  --password <value>      Optional root password
  --start                 Start the CT after creation
  --dry-run               Print the pct commands without executing them

Template options (turns an already-provisioned CT into a reusable
Proxmox vztmpl archive -- see "Custom CT Template Strategy" in
eco/README.md):
  --name <name>            Required. Base name for the archive, e.g. "eco-rust-base"
  --clone-id <ctid>        Required. CT ID to use for the disposable clone this builds from
  --version <version>      Version tag in the filename (default: today, YYYYMMDD)
  --workspace-root <path>  Project workspace to wipe before exporting (default: /opt/projects)
  --mongo-data-dir <path>  MongoDB data dir to wipe before exporting (default: /var/lib/mongodb)
  --dumpdir <path>         Where vzdump writes the archive (default: /var/lib/vz/template/cache)
  --storage <name>         Storage for the clone's rootfs (default: same as source)
  --keep-clone             Don't destroy the disposable clone after export (for debugging)
  --dry-run                Print the plan without executing it

The source CT keeps running untouched -- this clones it, cleans
project-specific state (code, PM2 state, DB data, SSH keys, machine-id)
out of the clone, exports the clone as an archive, then destroys the
clone. Never run this against a CT you want to keep as-is; always
point --clone-id at a fresh, disposable ID.

Examples:
  eco ct create assessment --dry-run
  eco ct create assessment --start
  eco ct status assessment
  eco ct stop 101
  eco ct template 105 --name eco-rust-base --clone-id 199 --dry-run
  eco ct template rust --name eco-rust-base --clone-id 199
"#;
    print!("{text}");
}

fn required(options: &std::collections::HashMap<String, String>, key: &str) -> Result<String, String> {
    options
        .get(key)
        .filter(|v| !v.is_empty())
        .cloned()
        .ok_or_else(|| format!("Missing required option --{key}"))
}

fn build_net0(options: &std::collections::HashMap<String, String>) -> Result<String, String> {
    let bridge = required(options, "bridge")?;
    let ip = options.get("ip").cloned().unwrap_or_else(|| "dhcp".to_string());
    let mut parts = vec![format!("name=eth0"), format!("bridge={bridge}"), format!("ip={ip}")];
    if let Some(gateway) = options.get("gateway") {
        parts.push(format!("gw={gateway}"));
    }
    Ok(parts.join(","))
}

fn build_create_args(name: &str, options: &std::collections::HashMap<String, String>) -> Result<Vec<String>, String> {
    let id = required(options, "id")?;
    let template = required(options, "template")?;
    let storage = required(options, "storage")?;
    let disk = required(options, "disk")?;
    let hostname = options.get("hostname").cloned().unwrap_or_else(|| name.to_string());
    let cores = options.get("cores").cloned().unwrap_or_else(|| "2".to_string());
    let memory = options.get("memory").cloned().unwrap_or_else(|| "4096".to_string());
    let swap = options.get("swap").cloned().unwrap_or_else(|| "1024".to_string());
    let unprivileged = options.get("unprivileged").cloned().unwrap_or_else(|| "1".to_string());
    let rootfs = format!("{storage}:{disk}");

    let mut args = vec![
        "create".to_string(),
        id,
        template,
        "--hostname".to_string(),
        hostname,
        "--cores".to_string(),
        cores,
        "--memory".to_string(),
        memory,
        "--swap".to_string(),
        swap,
        "--rootfs".to_string(),
        rootfs,
        "--net0".to_string(),
        build_net0(options)?,
        "--unprivileged".to_string(),
        unprivileged,
    ];
    if let Some(password) = options.get("password") {
        args.push("--password".to_string());
        args.push(password.clone());
    }
    Ok(args)
}

async fn _noop() {}

fn run_command(command: &str, args: &[String], cwd: &Path) -> Result<(), String> {
    util::run_command(command, args, cwd)
}

fn run_capture(command: &str, args: &[String], cwd: &Path) -> Result<util::Captured, String> {
    util::run_capture(command, args, cwd)
}

fn resolve_project_ct_options(project_input: &str, cwd: &Path) -> Result<(String, std::collections::HashMap<String, String>), String> {
    let deployment = ecompose::read_ecompose(project_input, cwd)?;
    let ct_options = ecompose::parse_ct_metadata(&deployment.content);
    for key in ["id", "template", "storage", "disk", "bridge"] {
        if !ct_options.contains_key(key) {
            return Err(format!("Missing ct.{key} in {}", deployment.file_path.display()));
        }
    }
    Ok((deployment.file_path.display().to_string(), ct_options))
}

fn resolve_ct_id_input(input: &str, cwd: &Path) -> Result<String, String> {
    if input.is_empty() {
        return Ok(String::new());
    }
    if input.chars().all(|c| c.is_ascii_digit()) {
        return Ok(input.to_string());
    }
    let (_, ct_options) = resolve_project_ct_options(input, cwd)?;
    Ok(ct_options.get("id").cloned().unwrap_or_default())
}

fn run_create(args: &[String]) -> Result<(), String> {
    let (positionals, options) = util::parse_ct_options(args);
    let project = positionals.first().cloned().ok_or("Missing project.")?;
    let cwd = util::current_dir();
    let (file_path, manifest_options) = resolve_project_ct_options(&project, &cwd)?;

    let mut merged = manifest_options;
    for (k, v) in options.iter() {
        merged.insert(k.clone(), v.clone());
    }
    let name = merged
        .get("name")
        .or_else(|| merged.get("hostname"))
        .cloned()
        .unwrap_or_else(|| project.clone());
    let create_args = build_create_args(&name, &merged)?;

    let mut commands: Vec<(String, Vec<String>)> = vec![("pct".to_string(), create_args)];
    if merged.get("start").map(|v| v == "true").unwrap_or(false) {
        let id = required(&merged, "id")?;
        commands.push(("pct".to_string(), vec!["start".to_string(), id]));
    }

    if merged.get("dry-run").map(|v| v == "true").unwrap_or(false) {
        util::println_stdout("eco ct create plan");
        util::println_stdout(&format!("Manifest: {file_path}\n"));
        for (command, command_args) in &commands {
            util::println_stdout(&format!("{command} {}", command_args.join(" ")));
        }
        return Ok(());
    }

    for (command, command_args) in commands {
        run_command(&command, &command_args, &util::current_dir())?;
    }
    Ok(())
}

fn run_simple_pct(subcommand: &str, args: &[String]) -> Result<(), String> {
    let target = args.first().cloned().ok_or(format!("Missing CT ID for \"eco ct {subcommand}\""))?;
    let cwd = util::current_dir();
    let ctid = resolve_ct_id_input(&target, &cwd)?;
    run_command("pct", &[subcommand.to_string(), ctid], &util::current_dir())
}

fn wait_for_ct_exec(ctid: &str, attempts: usize, delay_ms: u64) -> Result<(), String> {
    for _ in 0..attempts {
        let result = run_capture("pct", &["exec".to_string(), ctid.to_string(), "--".to_string(), "true".to_string()], &util::current_dir())?;
        if result.code == 0 {
            return Ok(());
        }
        util::sleep_ms(delay_ms);
    }
    Err(format!(
        "CT {ctid} did not become exec-ready in time (waited {}ms).",
        attempts * delay_ms as usize
    ))
}

fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn build_cleanup_script(workspace_root: &str, mongo_data_dir: &str) -> String {
    [
        "pm2 delete all >/dev/null 2>&1 || true",
        "rm -f /root/.pm2/dump.pm2",
        "rm -rf /root/.pm2/logs/*",
        &format!("rm -rf {}/*", sh_quote(workspace_root)),
        "systemctl stop mongod >/dev/null 2>&1 || true",
        &format!("rm -rf {}/*", sh_quote(mongo_data_dir)),
        "rm -rf /root/.ssh",
        "rm -f /etc/ssh/ssh_host_*",
        "truncate -s 0 /etc/machine-id",
        "apt-get clean",
        "rm -f /root/.bash_history",
        "find /var/log -type f \\( -name '*.log' -o -name '*.log.*' \\) -delete 2>/dev/null || true",
    ]
    .join(" && ")
}

fn rename_latest_vzdump_archive(dumpdir: &str, clone_id: &str, final_archive_path: &str) -> Result<(), String> {
    let entries = std::fs::read_dir(dumpdir)
        .map_err(|e| format!("Cannot read dumpdir {dumpdir}: {e}"))?;
    let prefix = format!("vzdump-lxc-{clone_id}-");
    let mut matches: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
        .filter(|name| name.starts_with(&prefix) && name.ends_with(".tar.zst"))
        .collect();
    matches.sort();
    let latest = matches.pop().ok_or_else(|| {
        format!(
            "No vzdump archive found for CT {clone_id} in {dumpdir} (expected a file starting with \"{prefix}\")."
        )
    })?;
    let source = std::path::Path::new(dumpdir).join(&latest);
    std::fs::rename(&source, final_archive_path)
        .map_err(|e| format!("Cannot rename {} -> {}: {e}", source.display(), final_archive_path))
}

fn run_template(args: &[String]) -> Result<(), String> {
    let (positionals, options) = util::parse_ct_options(args);
    let source = positionals.first().cloned().ok_or("Missing source CT.")?;
    let name = required(&options, "name")?;
    let clone_id = required(&options, "clone-id")?;
    let version = options
        .get("version")
        .cloned()
        .unwrap_or_else(|| {
            chrono::Utc::now().format("%Y%m%d").to_string()
        });
    let workspace_root = options.get("workspace-root").cloned().unwrap_or_else(|| "/opt/projects".to_string());
    let mongo_data_dir = options.get("mongo-data-dir").cloned().unwrap_or_else(|| "/var/lib/mongodb".to_string());
    let dumpdir = options.get("dumpdir").cloned().unwrap_or_else(|| "/var/lib/vz/template/cache".to_string());
    let keep_clone = options.get("keep-clone").map(|v| v == "true").unwrap_or(false);
    let storage = options.get("storage").cloned();

    let cwd = util::current_dir();
    let source_ctid = resolve_ct_id_input(&source, &cwd)?;
    if source_ctid == clone_id {
        return Err("--clone-id must be different from the source CT -- this command destroys the clone when it's done.".to_string());
    }

    let final_archive_name = format!("{name}_{version}_amd64.tar.zst");
    let final_archive_path = format!("{dumpdir}/{final_archive_name}");

    let source_status = run_capture("pct", &["status".to_string(), source_ctid.clone()], &util::current_dir())?;
    let source_is_running = source_status.code == 0 && source_status.stdout.contains("status: running");
    let snapshot_name = if source_is_running {
        Some(format!("eco-template-{}", chrono::Utc::now().timestamp_millis()))
    } else {
        None
    };

    let mut clone_args = vec![
        "clone".to_string(),
        source_ctid.clone(),
        clone_id.clone(),
        "--hostname".to_string(),
        format!("{name}-template-build"),
        "--full".to_string(),
        "1".to_string(),
    ];
    if let Some(snapshot) = &snapshot_name {
        clone_args.push("--snapname".to_string());
        clone_args.push(snapshot.clone());
    }
    if let Some(storage) = &storage {
        clone_args.push("--storage".to_string());
        clone_args.push(storage.clone());
    }

    let cleanup_script = build_cleanup_script(&workspace_root, &mongo_data_dir);

    let mut steps: Vec<(String, Option<(String, Vec<String>)>, bool, bool)> = Vec::new();
    if let Some(snapshot) = &snapshot_name {
        steps.push((format!("Snapshot running source CT {source_ctid} (required for a full clone of a live container)"),
            Some(("pct".to_string(), vec!["snapshot".to_string(), source_ctid.clone(), snapshot.clone()])),
            false,
            false,
        ));
    }
    steps.push((format!("Clone CT {source_ctid} -> {clone_id} (full clone, source untouched)"),
        Some(("pct".to_string(), clone_args)),
        false,
        false,
    ));
    steps.push((format!("Start clone CT {clone_id}"),
        Some(("pct".to_string(), vec!["start".to_string(), clone_id.clone()])),
        false,
        false,
    ));
    steps.push((format!("Wait for CT {clone_id} to be exec-ready"), None, true, false));
    steps.push((format!("Clean project-specific state inside CT {clone_id}"),
        Some(("pct".to_string(), vec!["exec".to_string(), clone_id.clone(), "--".to_string(), "bash".to_string(), "-lc".to_string(), cleanup_script])),
        false,
        false,
    ));
    steps.push((format!("Stop clone CT {clone_id}"),
        Some(("pct".to_string(), vec!["stop".to_string(), clone_id.clone()])),
        false,
        false,
    ));
    steps.push((format!("Export CT {clone_id} as a template archive"),
        Some(("vzdump".to_string(), vec![clone_id.clone(), "--mode".to_string(), "stop".to_string(), "--compress".to_string(), "zstd".to_string(), "--dumpdir".to_string(), dumpdir.clone()])),
        false,
        false,
    ));
    steps.push((format!("Rename exported archive to {final_archive_name}"), None, false, true));

    if let Some(snapshot) = &snapshot_name {
        steps.push((format!("Remove temporary snapshot {snapshot} from source CT {source_ctid}"),
            Some(("pct".to_string(), vec!["delsnapshot".to_string(), source_ctid.clone(), snapshot.clone()])),
            false,
            false,
        ));
    }
    if !keep_clone {
        steps.push((format!("Destroy temporary clone CT {clone_id}"),
            Some(("pct".to_string(), vec!["destroy".to_string(), clone_id.clone()])),
            false,
            false,
        ));
    }

    if options.get("dry-run").map(|v| v == "true").unwrap_or(false) {
        let mut out = String::new();
        out.push_str("eco ct template plan\n");
        out.push_str(&format!("Source CT: {source_ctid}\n"));
        out.push_str(&format!("Clone CT:  {clone_id}{}\n", if keep_clone { " (kept after export)" } else { " (destroyed after export)" }));
        out.push_str(&format!("Template:  {final_archive_path}\n\n"));
        for (description, cmd, _, _) in &steps {
            match cmd {
                Some((command, command_args)) => {
                    let rendered: Vec<String> = command_args.iter().map(|a| if a.contains(' ') { sh_quote(a) } else { a.clone() }).collect();
                    out.push_str(&format!("{description}\n  {command} {}\n", rendered.join(" ")));
                }
                None => out.push_str(&format!("{description}\n")),
            }
        }
        print!("{out}");
        return Ok(());
    }

    for (description, cmd, wait, rename) in &steps {
        util::println_stdout(&format!("==> {description}"));
        if *wait {
            wait_for_ct_exec(&clone_id, 20, 1000)?;
            continue;
        }
        if *rename {
            rename_latest_vzdump_archive(&dumpdir, &clone_id, &final_archive_path)?;
            continue;
        }
        if let Some((command, command_args)) = cmd {
            run_command(command, command_args, &util::current_dir())?;
        }
    }

    util::println_stdout(&format!("\nTemplate ready: {final_archive_path}"));
    util::println_stdout(&format!("Use it in an ecompose.yml with: ct.template: local:vztmpl/{final_archive_name}"));
    util::println_stdout("(adjust the \"local:\" storage prefix if --dumpdir targets a different storage.)");
    Ok(())
}

pub fn run_ct(args: &[String]) -> Result<(), String> {
    let (subcommand, rest) = match args.first() {
        Some(s) => (s.as_str(), &args[1..]),
        None => ("", &args[0..0]),
    };
    match subcommand {
        "" | "help" | "--help" | "-h" => {
            ct_help();
            Ok(())
        }
        "create" => run_create(rest),
        "start" | "stop" | "reboot" | "status" => run_simple_pct(subcommand, rest),
        "template" => run_template(rest),
        other => Err(format!("Unknown CT subcommand: {other}\n\nRun \"eco ct help\" for usage.")),
    }
}
