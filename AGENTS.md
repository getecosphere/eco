# AGENTS.md — Working with Eco

Operational context for AI agents that build **with** Eco or contribute **to**
the Eco CLI. Read this before writing any code. The README sells the idea; this
file tells you how to actually work.

## Source split (public vs private)

This repository (`getecosphere/eco`) is the **public, client-only** build of the
Eco CLI. The host-side control plane — the `eco serve --port 8790` agent, the
deploy engine, Cloudflare automation, accounts, and the infra ops commands
(`eco prox`/`ct`/`proxy`/`ports`/`db`/`sendemail`/`expose`) — lives in the
**private** `getecosphere/eco-server` repo, which builds the eco-server binary
deployed **only to the Proxmox host** (it is not installed on dev machines).

Consequences for this repo:

- `eco up --remote`, `eco up dev`, `eco lxs`, `eco serve <subdomain>` (tunnel
  client), `eco login/signup`, `eco show/stress/sync/init/install/configure`
  are all present and functional.
- The agent-mode dispatch (`eco serve --port` / `gen-key`) is **not** here; the
  private binary owns it. Never re-add host/agent modules (`runtime/`,
  `cloudflare.rs`, `configgen.rs`, `commands/{prox,ct,proxy,ports,db}.rs`) —
  that's a source-leak regression.
- The public binary talks to the agent over HTTP (`ECO_API_URL`), same protocol.

## What Eco is (the mental model)

Eco composes reusable, versioned **Linux Services (LXS)** — single compiled
binaries — into application *estates*. One manifest (`ecompose.yml`) drives the
whole estate from local dev to a live public URL.

**The core principle: your machine is the build farm.**

```
source code ──► binary ──► LXS ──► the world
```

- Code is built **on the developer's machine** and shipped as **static binaries**.
- The server is a **pure executable runner** — it never compiles, never pulls a
  repo, never runs `npm install`. No Docker, no build cache, no `node_modules`.
- An **LXS** is polyglot: any codebase that can ship as a standalone binary —
  Rust, Go, Node via Bun-compile, etc. Each LXS owns one bounded domain, a
  declared contract (env / db / network / resources), and zero runtime dependency.
- The **LXS registry** is the distribution layer: identity, versions, artifacts,
  manifests, checksums, contracts, and retrieval.

Never put a build step on the server. If something needs compiling, it happens
locally first and ships as an executable.

## Built for vibecoders — safety by default

Eco's audience is the AI-assisted builder (the *vibecoder*): an agent plus a
developer iterating fast, with no interest in ops discipline. Their defaults
are sloppy, so Eco's defaults must be safe. Two non-negotiables:

**1. Secrets never leave the machine unguarded.** The common failure is a
committed `.env` or an API key pasted into the repo. `eco up` must run a
guardrail scan before anything ships:

- `.env` / `.env.*` files exist but are not in `.gitignore` → fail, printing
  the exact `.gitignore` line to add.
- `.env`, or secret-shaped values (`sk-…`, `AKIA…`, `AIza…`,
  `-----BEGIN … PRIVATE KEY-----`, `password=`, `token=`), is present in
  *tracked* git files → fail, printing `git rm --cached` + gitignore
  remediation.
- Shipped artifacts (releases, `current/`) never contain live secrets; env
  flows into the estate only through declared `grants.secrets`.

**2. Route protection is default-deny, Spring-Security style.** The hardest
bug is an admin/private route the vibecoder left public. The gateway is
generated from the manifest, so **authorization knowledge lives in the manifest
— never in a hand-edited gateway file**. Model it like Spring Security's
"deny by default, allow explicitly":

- Every route a service exposes is declared with an access level
  (`public` | `auth` | `role:<name>`); undeclared routes are denied.
- `configgen` renders the gateway (Caddyfile today) from those declarations:
  explicit public matchers, JWT/role verification on everything else, and a
  403/404 fallback for undeclared API paths — never a catch-all forward.
- Treat the generated gateway as an artifact: edit the manifest and the
  generator, never the Caddyfile by hand.
- If generated Caddy can't express the model (edge JWT verify, identity
  propagation, role hierarchies), the fix goes into the generator — or into a
  dedicated gateway LXS — not into ad-hoc config.

## Repo layout (the CLI itself)

- `rust/` — the eco CLI (Rust). Build: `cd rust && cargo build --release` → `rust/target/release/eco`
- `docs/` — proof/metrics writeups (e.g. `proof-metrics.md`)
- `install.sh`, `provision.sh`, `configure.sh`, `*.sh` — bundled scripts
- `assets/`, `dist/`, `.github/` — release assets & CI

## provision.sh — cross-platform status

`provision.sh` provisions dev runtimes declared in `ecompose.yml` (`rust`,
`go`, `node@20`, `postgresql@15`, `mongodb@7`, `redis@7`, `python@*`,
`dotnet@*`, …). **Windows is NOT yet supported** — the script only handles
macOS (`brew`) and Debian/Ubuntu (`apt`); an unsupported OS exits with
"Unsupported OS". Add a `MINGW`/PowerShell branch (winget/choco, or the
official installers) before claiming Windows provisioning works. The multi-OS
LXS *build* side (`eco lxs build --arch …/windows/amd64`) already works.

## The manifest: ecompose.yml

One file declares the whole estate. `main` marks the primary estate.

```yaml
project: stuff8
main: stuff8

estates:
  stuff8:
    hostname: stuff8.com
    services: [auth, chat, storage, frontend]

services:
  auth:
    lxs: auth@1.0.2          # a registry LXS binary — no source in the repo
    grants:
      secrets: [JWT_SECRET, MONGODB_URI]
  frontend:
    path: frontend            # a source service built locally, shipped as artifact
    runtimes: [node@20]
```

- `lxs:` entries compose a **registry binary** (verified download, never rebuilt).
- `path:` entries are **source services** built locally, shipped as executables.
- Grants must satisfy the LXS contract (required/optional env).

## Daily workflow (consume side)

```bash
eco init                       # detect framework → write ecompose.yml + .eco/state.json
eco lxs add auth@1.0.0         # compose an LXS binary from the registry
eco up dev                     # run locally (native-arch binaries, PM2)
eco up --remote                # build locally → ship executables → run on the server
eco serve <subdomain>          # expose a local app: https://<sub>.getecosphere.com
```

- `eco up dev` **auto-provisions missing dev runtimes** on your machine (it is
  the build farm). On macOS it installs Homebrew on first use, then the declared
  runtimes (node@20, postgresql@15, mongodb, redis, …) via brew. The first
  Homebrew install asks for your sudo password — run `eco up dev` in an
  interactive terminal once so it can prompt; later runs are non-interactive.
- Detected frameworks: rust, go, spring-boot (java), nextjs, astro, vite, nuxt,
  node (plain), static. Use `eco init --no-detect` to scaffold blank.
- `eco up` (dev + remote) checks composed LXS against the registry for newer
  versions by default; disable with `--no-lxs-check` or `ECO_NO_LXS_CHECK=1`.
- Version management: `eco lxs outdated`, `eco lxs update [name]`,
  `eco lxs remove <name>`.
- `.eco/state.json` (gitignored) binds a folder to its estate + registry
  (default `getecosphere/lxs-registry`).
- `ECO_GITHUB_API_KEY` is used only by maintainers/enterprise publish flows that
  push directly into a registry repo (below). As a contributor you never need it.
- Useful ops: `eco show`, `eco stress`, `eco sync`. (Host/port ops like
  `eco ports list` / `eco db clear` live in the private binary.)

## Building & publishing an LXS (author side)

An LXS is **polyglot** — any service you can ship as a standalone binary (Rust,
Go, Node via Bun-compile, …) plus a declared contract and a docs bundle.
Consumers get only the **binary + docs** — never the source.

```bash
eco lxs new <name>              # scaffold: lxs.yml, src/, docs/, CI workflow
# edit lxs.yml to declare the contract (below)
eco lxs build                   # cross-compile → writes artifacts.json
eco lxs publish <name>[@<v>]    # auto-bump patch; --minor / --major for features/breaking
```

- `eco lxs build` defaults to `linux/amd64` (musl static,
  `x86_64-unknown-linux-musl`); pass `--arch linux/amd64,linux/arm64`. For local
  dev on macOS also build a native artifact (`darwin/aarch64`).
- `eco lxs publish` **requires** the docs bundle: `docs/README.md`,
  `docs/api.md`, `docs/changelog.md`. Also ship `docs/examples.sh`,
  `docs/openapi.json`, `docs/gotchas.md` — agents consume only the docs + binary.
- Publish writes `name/version/{lxs.yml, <arch>/<name>, docs/}` into the local
  registry mirror, then commits + tags it (`name-version`). First publish starts
  at `0.1.0`.
- Consumers fetch with `eco lxs add <name>[@<version>]`; a custom/private
  registry via `eco lxs add <name> --address <registry-repo>`.
- `eco lxs init-registry [folder]` creates a new registry repo.

### How an LXS reaches consumers

An LXS is **not always public** — three paths:

1. **Public registry** — the maintainers push directly into the official
   `getecosphere/lxs-registry` (this needs `ECO_GITHUB_API_KEY`). Outside
   contributors do **not** push: fork the official registry repo, publish to
   your fork, then open a merge request (PR) — the eco team reviews and decides
   whether to merge it into the public registry.
2. **Private LXS** — publish to your own private registry
   (`eco lxs init-registry` → `eco lxs publish` → `eco lxs add <name> --address <your-repo>`).
   Publishing private LXS is an **enterprise-only** feature.
3. **No registry at all** — a source LXS never leaves your machine: register the
   folder with `eco lxs add .`, or simply declare it as a `path:` source service
   in ecompose.yml. Nothing is published; it ships as part of your estate.

## LXS manifest contract

```yaml
name: <name>
version: 0.1.0
category: Infrastructure
publisher: <org>
status: unverified
license: mit
targets: [linux/amd64]
artifacts: {}
contract:
  version: 1
  api: "<name> REST API"
  env:
    required: [SERVER_PORT]
    optional: []
  db: none
  network:
    inbound: [http]
    outbound: []
  resources: { memory: "128m", disk: "256m", startup_seconds: 5 }
runtime:
  base: self-contained-static
  libc: musl
provenance: { source: "", commit: "", built_by: "", built_at: "", target: x86_64-unknown-linux-musl }
```

Registry repo layout:

```
<name>/<version>/
  lxs.yml          # manifest: name, version, contract, artifacts, docs
  <arch>/<name>    # static binary, e.g. linux-amd64/<name>
  docs/            # the docs bundle
```

## Rules for agents

- **Never** put a build/compile/`npm install` step on the server. Build locally, ship binaries.
- **Never** reach for Docker. Eco runs services as native processes on the
  ecosphere server — application-level isolation, per-service container overhead
  is the anti-pattern here. (The underlying host technology is internal — you
  don't need to know or touch it.)
- Keep LXS **single-domain**: one bounded capability per binary; never let one
  LXS own another domain's responsibilities.
- Prefer **musl static binaries** (`x86_64-unknown-linux-musl`); a glibc build
  is not self-contained for the server.
- When you change a capability, bump the LXS version (`--minor` features,
  `--major` breaking) and record it in `docs/changelog.md`.
- The gateway (Caddyfile) is **auto-generated** by `eco up --remote`
  (configgen). Route `/api/<svc>/*`-style prefixes there — do not hand-edit
  generated Caddyfiles. Routes are **deny-by-default**: every exposed route
  must be declared with an access level; never ship a catch-all forward.
- `eco up` runs a **secrets guardrail scan** (committed `.env`, keys in tracked
  files) before shipping. Never bypass it; if it false-positives, refine the
  detector, don't disable the check.
- **Static assets under public `/static/*` are Cloudflare-cached (~4h)**: when a
  frontend ships a changed JS/CSS asset, bump the cache-buster query param in
  the HTML that references it (e.g. `notes.js?v=20260817b`), or browsers serve
  the stale cached file after redeploy. Confirm with
  `curl -sI <url> | grep cf-cache-status` (HIT = cached).
- Verify after deploy with `eco show` / `eco ports list` and a real
  curl on the public URL, not just a backend-only probe.

## Learning more

- Public docs: https://doc.getecosphere.com (source: `getecosphere/eco_docs`)
- LXS Registry: https://github.com/getecosphere/lxs-registry
- Reference estates: https://github.com/getecosphere/stuff8
- Proof writeup (9 frameworks composing one auth binary): `docs/proof-metrics.md`
