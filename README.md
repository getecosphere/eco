# Eco

**Compose reusable domains into self-sustaining application estates.**

Eco is a developer tool for building and running applications by composing
reusable, versioned *Linux Services* (LXS) instead of rewriting the same
capabilities from scratch. Build once. Publish once. Compose everywhere.

```
source code ──► binary ──► LXS ──► the world
```

## The idea in one line

**Your machine is the build farm.** You build on your own hardware and ship
executables to the server. The server is a pure *executable runner* — it never
compiles, never pulls a repo, never runs `npm install`. No Docker, no build
server, no `node_modules` piling up on production.

## How it works

```
developer machine                       ecosphere server                    estate
─────────────────                       ────────────────                    ─────
eco up --remote ──► build + ship ──► eco serve agent ──► installs ──► systemd runs it
  (your machine)                        (deploy endpoint)               (no build)
```

- **Build locally.** Rust services are cross-compiled on your machine
  (`x86_64-unknown-linux-musl` — a static binary). Frontends are built, and
  Node backends can be Bun-compiled into a single linux-x64 binary.
- **Ship the binary.** `eco up --remote` sends the artifacts to the server.
- **The server runs it.** systemd units, cgroup resource limits, journald logs.
  No compiler, no build cache, no `node_modules` on the CT.

Rust and Go compile to a single static binary — no runtime, no interpreter, no
dependency drift. That property is what turns a codebase into a *product*:
build once, publish once, compose everywhere.

## LXS: reusable capabilities

An **LXS** is the atomic unit of Eco:

- a **single compiled binary** (Rust or Go)
- **one bounded domain** — auth, photos, notifications, email, chat, storage…
- a **declared contract** — required/optional env, database, network, resources
- **self-sufficient** — no language runtime, no framework, no build step on the host

Because a binary is self-contained, an LXS can be moved, sold, and composed the
way no interpreted application ever could.

### The LXS registry

The **LXS registry** is the distribution layer — identity, versions,
artifacts, manifests, checksums, contracts, and retrieval. An estate composes a
capability by version:

```yaml
services:
  auth-backend:
    lxs: auth@1.0.2
    grants:
      secrets: [JWT_SECRET, MONGODB_URI]
```

`eco up` pulls the verified binary, installs it, and checks that the grants
satisfy the contract — no compiler, no source, no build step on the server.

## The manifest: ecompose.yml

One file declares the whole estate — what it publishes, what it composes, and
how its services run:

```yaml
project: stuff8
main: stuff8

estates:
  stuff8:
    hostname: stuff8.com
    services:
      - auth
      - chat
      - storage
      - frontend

services:
  auth:
    lxs: auth@1.0.2
  frontend:
    path: frontend
    runtimes: [node@20]
```

`main` marks the primary estate (like `main()` in C++); a project can declare
many estates and pick which one to deploy with `eco up --remote <estate>`.

## Why not Docker?

Docker isolates *per service*; most teams actually want to isolate *per
application*. Eco runs services as **native processes** inside a Proxmox CT —
application-level isolation with none of the per-service container overhead:

| | Docker | Eco |
|---|---|---|
| Runtime unit | container per service | native process per service |
| Isolation | per service | per application (one CT) |
| Dev workstation load | images, layers, daemon | plain processes |
| Server build farm | needed | **none — your machine is the farm** |
| `node_modules` on server | yes | **no** — single binaries |
| Registry | image registry | LXS registry (executable capabilities) |

For teams whose daily driver is an AI assistant, this matters even more: the
agent edits, restarts, and gets feedback in seconds — not after a container
image pull and rebuild.

## Show it publicly — free

`eco serve` gives your locally-running app a real public URL in one command —
`https://<name>.getecosphere.com`, through a Cloudflare tunnel, HTTPS handled
for you:

```bash
eco up dev            # your app is live on localhost
eco serve mentoring   # ...and now the world can see it
```

No domain to buy. No DNS to configure. No server to rent. Your laptop *is* the
server, and Eco handles everything between it and the internet. Start free; pay
only when the app earns.

## Open core

- **`eco`** (this repo, public) — the CLI you can audit: `ecompose.yml`
  parsing, `eco init`, `eco up --remote`, `eco up dev`, `lxs`, `serve`,
  `git`. No secrets, no agent. Anyone can verify the CLI does nothing sneaky.
- **`eco-server`** (private) — the control plane that runs on the ecosphere
  server: the `eco serve` agent, deploy orchestration, Cloudflare automation,
  and the resource registry.

The public CLI talks to the private server over an authenticated HTTP API — a
clean, inspectable boundary. The business stays in the server.

## Install

```bash
curl -fsSL https://getecosphere.com/install.sh | sh
```

## Documentation

- **Public docs site** — https://eco.stuff8.com
  (source: [`getecosphere/eco_docs_composition`](https://github.com/getecosphere/eco_docs_composition))
- **LXS Registry** — [`getecosphere/lxs-registry`](https://github.com/getecosphere/lxs-registry)
- **Reference estates** — [`getecosphere/stuff8`](https://github.com/getecosphere/stuff8)

## Building the CLI

```bash
cd rust && cargo build --release    # → rust/target/release/eco
```

## License

Released under the MIT License. © 2026 Eco.
