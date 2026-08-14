use crate::commands;

pub fn run_cli(argv: &[String]) -> Result<(), String> {
    let (command, rest) = match argv.first() {
        Some(c) => (c.as_str(), &argv[1..]),
        None => ("help", &argv[0..0]),
    };

    match command {
        "help" | "--help" | "-h" => commands::help::show_help(),
        "version" | "--version" | "-v" => {
            println!("eco {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "init" => commands::wrappers::run_init(rest),
        "install" => commands::install::run_install(rest),
        "configure" => commands::wrappers::run_configure(rest),
        "show" => commands::show::run_show(rest),
        "tree" => commands::wrappers::run_tree(rest),

        "startproject" => commands::startproject::run_startproject(rest),
        "adopt" => commands::adopt::run_adopt(rest),
        "clearstarterproject" => commands::clearstarterproject::run_clear_starter_project(rest),
        "compose" => commands::compose::run_compose(rest),
        "provision" => commands::wrappers::run_provision(rest),
        "proxy" => commands::proxy::run_proxy(rest),
        "prox" => commands::prox::run_prox(rest),
        "ports" => commands::ports::run_ports(rest),
        "update" => commands::wrappers::run_update(rest),
        "up" => commands::up::run_up(rest),
        "expose" => commands::up::run_expose(rest),
        "webhook-clean" => commands::webhook_clean::run_webhook_clean(rest),
        "git" => commands::wrappers::run_git(rest),
        "ct" => commands::ct::run_ct(rest),
        "db" => commands::db::run_db(rest),
        "dev" => commands::wrappers::run_dev(rest),

        "rust" => commands::rust_cmd::run_rust(rest),
        "lxs" => commands::lxs::run_lxs(rest),
        "stress" => commands::stress::run_stress(rest),
        "sync" => commands::sync::run_sync(rest),
        "sync-staging" => commands::sync::run_sync_staging(rest),
        "sendemail" => commands::sendemail::run_sendemail(rest),
        // internal: registry CLI mirror used by bundled bash scripts
        "registry" => commands::registry_cmd::run_registry_cli(rest),
        // internal: materialize the embedded configure.sh to a target path
        "__bundle-configure-sh" => {
            let dest = rest.first().cloned().unwrap_or_default();
            if dest.is_empty() {
                return Err("usage: eco __bundle-configure-sh <dest>".to_string());
            }
            crate::embedded::materialize_configure_sh(&dest)
        }
        _ => Err(format!(
            "Unknown command: {command}\n\nRun \"eco help\" for usage."
        )),
    }
}
