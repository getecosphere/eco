pub fn show_help() -> Result<(), String> {
    let text = r##"eco

eco - manage large-scale projects using Domain-Driven Design by decomposing them into independent, self-sustaining domains that can be reused and recomposed into new projects

Usage:
  eco help
  eco init [dir] [--no-detect]
  eco install <tool>
  eco configure [args...]
  eco show
  eco compose add [repo-name|path]
  eco compose refresh <repo-name|path>
  eco compose expose <service> <hostname>
  eco provision [args...]
  eco update
  eco up [path] [--dry-run]
  eco up dev [path] [--dry-run]
  eco up --remote [--staging]
  eco serve <subdomain> [--port <port>] [--release]
  eco serve stop <subdomain>
  eco serve list
  eco setgithubstatus [--clear] "<message>"   set the GitHub profile status (GraphQL)
  eco sync [--host <hostname>] [--ct <ctid>] [--service <name>] [--staging] [--dry-run]
  eco sync-staging [--host <hostname>] [--service <name>] [--dry-run]
  eco dev flushdns
  eco git [args...]
  eco stress [--vus <n>] [--duration <s>] [--ramp-up <s>] [--hostname <url>] [--dry-run]

Current command groups:
  init        Make the current (or given) directory an eco project: auto-detects
              services already in the folder and writes ecompose.yml (validates
              an existing one instead of overwriting), then .eco/state.json,
              .gitignore, and git init. Use --no-detect to scaffold blank.
  install     Install infra-level tooling not tied to any one project
             (e.g. "eco install minio") -- run once per machine/CT
  configure   Run the bundled eco configure workflow
             Pass --non-interactive to accept defaults without prompts
  show        Show the current service URLs/ports from ecosystem.config.js
  compose     Add a repo to an existing ecompose.yml
             "eco compose add" with no args offers an arrow-key checklist of
             "eco compose add <name>" clones that one catalog repo into the
             estate root and registers it; "eco compose add <path>" registers
             an already-present estate-root subdirectory. Either way it
             detects and appends the right services: entries.
  provision   Run the bundled eco provision workflow
  update      Git pull the eco repository itself
  up          Create a CT and bootstrap the project in the current directory
             Use "eco up dev" for local dev bootstrap
  serve       Expose a locally-running dev app through a temporary public URL
             (e.g. https://<name>.getecosphere.com). Reserves the subdomain
             host-side, checks conflicts, runs a cloudflared tunnel to your
             localhost, and records serve.subdomain in ecompose.yml.
  sync        Sync production MongoDB and PostgreSQL data from the estate's
             application CT to the local dev machine via SSH (mongodump /
             mongorestore, pg_dump / pg_restore). Reads ecompose.yml to
             discover every DB-backed service; pass --service to sync just
             one. Use --dry-run to preview. --staging streams prod CT ->
             staging CT on the Proxmox host.
  sync-staging Same as "eco sync --staging" (prod -> staging database sync).
  git         Run the bundled eco git workflow
  dev         Local development utilities
             "eco dev flushdns" flushes the macOS DNS cache
  stress      Run a k6 stress test against the estate's public hostname
             Reads expose.hostname from ecompose.yml (or pass --hostname).
             Provisions k6 automatically on Linux x64, macOS Intel, or
             macOS Apple Silicon. Options: --vus <n> (default 100),
             --duration <s> (default 30s), --ramp-up <s> (default 10s),
             --hostname <url>, --dry-run

Examples:
  eco init
  eco init myapp --no-detect
  eco install minio
  eco show
  eco compose add
  eco compose add gameserver
  eco compose add ../gameserver
  eco provision --plan
  eco update
  eco up --dry-run
  eco up dev .
  eco configure
  eco git status
  eco stress
  eco stress --vus 1000 --duration 60s --ramp-up 20s
  eco stress --hostname example.com --dry-run

Cloudflare automation env:
  CF_API_TOKEN   Cloudflare API token with tunnel and DNS permissions
  CF_ACCOUNT_ID  Cloudflare account id that owns the tunnel
  CF_ZONE_ID     Cloudflare zone id for the public hostname

  Multiple Cloudflare accounts on one host: set expose.cloudflare_account
  in an estate's ecompose.yml (e.g. "jogjaitcamp") and export
  CF_API_TOKEN_<NAME> / CF_ACCOUNT_ID_<NAME> / CF_ZONE_ID_<NAME> instead
  (e.g. CF_API_TOKEN_JOGJAITCAMP) -- the unsuffixed vars above remain the
  default account for estates that don't set cloudflare_account. Each
  named account gets its own cloudflared process/tunnel in the proxy CT,
  since one tunnel can only belong to one Cloudflare account.

GitHub automation env:
  ECO_GITHUB_API_KEY
                 GitHub token used for LXS publish and remote repo automation
                 (creating/pushing the estate core repo). Not required for
                 `eco init`, local dev, or `eco up --remote` on a host that
                 already holds the estate.
"##;
    print!("{text}");
    Ok(())
}
