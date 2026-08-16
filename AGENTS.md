# AGENTS.md — Working with Eco

Operational context for AI agents that build **with** Eco or contribute **to**
the Eco CLI. Read this before writing any code. The README sells the idea; this
file tells you how to actually work.

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
  repo, never runs `npm install`. No Docker, no build cache, no `node_modules`
  on the CT.
- An **LXS** is one compiled binary (Rust or Go), one bounded domain, a declared
  contract (env / db / network / resources), and zero runtime dependency.
- The **LXS registry** is the distribution layer: identity, versions, artifacts,
  manifests, checksums, contracts, and retrieval.

Never put a build step on the server. If something needs compiling, it happens
locally first and ships as an executable.

## Repo layout (the CLI itself)

- `rust/` — the eco CLI (Rust). Build: `cd rust && cargo build --release` → `rust/target/release/eco`
- `docs/` — proof/metrics writeups (e.g. `proof-metrics.md`)
- `install.sh`, `provision.sh`, `configure.sh`, `*.sh` — bundled scripts
- `assets/`, `dist/`, `.github/` — release assets & CI

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
eco up --remote                # build locally → ship executables → systemd on the CT
eco serve <subdomain>          # expose a local app: https://<sub>.getecosphere.com
```

- Detected frameworks: rust, go, spring-boot (java), nextjs, astro, vite, nuxt,
  node (plain), static. Use `eco init --no-detect` to scaffold blank.
- `eco up` (dev + remote) checks composed LXS against the registry for newer
  versions by default; disable with `--no-lxs-check` or `ECO_NO_LXS_CHECK=1`.
- Version management: `eco lxs outdated`, `eco lxs update [name]`,
  `eco lxs remove <name>`.
- `.eco/state.json` (gitignored) binds a folder to its estate + registry
  (default `getecosphere/lxs-registry`).
- `ECO_GITHUB_API_KEY` is only needed for LXS publish / remote repo automation.
- Useful ops: `eco show`, `eco ports list`, `eco db clear <service>`,
  `eco ct status`, `eco stress`, `eco sync`.

## Building & publishing an LXS (author side)

An LXS is a Rust (axum) crate with a declared contract plus a docs bundle.
Consumers get only the **binary + docs** — never the source.

```bash
eco lxs new <name>              # scaffold: lxs.yml, src/, docs/, CI workflow
# edit lxs.yml to declare the contract (below)
eco lxs build                   # cross-compile (cargo-zigbuild) → writes artifacts.json
eco lxs publish <name>[@<v>]    # auto-bump patch; --minor / --major for features/breaking
git push origin main --tags     # push the registry (or let the tag-triggered CI do it)
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
- **Never** reach for Docker. Eco runs services as native processes inside a
  Proxmox CT — application-level isolation, per-service container overhead is
  the anti-pattern here.
- Keep LXS **single-domain**: one bounded capability per binary; never let one
  LXS own another domain's responsibilities.
- Prefer **musl static binaries** (`x86_64-unknown-linux-musl`); a glibc build
  is not self-contained for the CT.
- When you change a capability, bump the LXS version (`--minor` features,
  `--major` breaking) and record it in `docs/changelog.md`.
- The gateway (Caddyfile) is **auto-generated** by `eco up --remote`
  (configgen). Route `/api/<svc>/*`-style prefixes there — do not hand-edit
  generated Caddyfiles.
- Verify after deploy with `eco show` / `eco ports list` and a real
  curl on the public URL, not just a backend-only probe.

## Learning more

- Public docs: https://eco.stuff8.com (source: `getecosphere/eco_docs_composition`)
- LXS Registry: https://github.com/getecosphere/lxs-registry
- Reference estates: https://github.com/getecosphere/stuff8
- Proof writeup (9 frameworks composing one auth binary): `docs/proof-metrics.md`
