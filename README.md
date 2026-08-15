# eco

Private CLI and primary implementation for the Ecology DDD workflow.

`eco` manages large-scale projects using Domain-Driven Design by decomposing them into independent, self-sustaining domains that can be reused and recomposed into new projects.

Proxmox CT provides the infrastructure boundary; `eco` provides the application composition boundary. Services run natively inside CTs — one CT may host one or more application estates, each declared by its own `ecompose.yml`.

## Implementation: a compiled Rust binary

`eco` is a **compiled Rust binary** (see `rust/`). All command logic is
implemented in Rust, and every bundled Bash workflow script (`configure.sh`,
`provision.sh`, `git.sh`, `install-*.sh`, `tree.sh`, and the
starter `ecology-mark.webp`) is **embedded inside the binary** via
`include_str!`/`include_bytes!`, then extracted to `~/.cache/eco/bundled/` (or
`$ECO_BUNDLED_ROOT`) on first run and executed through `bash`.

No Node.js runtime is needed to run the CLI. The former
`src/bin/registry-cli.js` is now the internal `eco registry` subcommand. The
old Node `src/` tree and `package.json` were removed after the migration.

Build: `cd rust && cargo build --release` → `rust/target/release/eco`.

## Releasing the eco binary

`eco` ships as a **single static binary** per platform, installed by
`install.sh` (download from the release base URL, default the estate host) and
never via `npm`/`node`. Cross-compile for all platforms with the repo's build
script — it provisions its own pinned toolchain (rustup targets + zig for C
cross-linking) so nothing is hand-installed:

```bash
./build-release.sh    # macOS (native) + Linux x86_64 (musl) + Windows x86_64 (gnu)
```

Artifacts land in `dist/<target-triple>/eco[.exe]`. The host runs the Linux
binary (see [docs/releasing.md](docs/releasing.md)); a CT keeps its own copy
of the binary for `eco up`, refreshed inside the CT by the deploy script.

## When you change eco (the daily loop)

**Runbook:** if you are setting up a new dev machine (or the M1), see
[docs/continue-from-m1.md](docs/continue-from-m1.md) — it documents the full
toolchain, the M1 workflow, and the current estate/systemd state.

eco behavior only reaches production through the **binary on the Proxmox host**:
at `eco up` time the host ships its *own running* binary into every CT — it is
copied into the bundle at `bin/eco` (see `rust/src/embedded.rs`) and pushed as
`temp-tar:eco`, then installed onto the CT's PATH. A code change is **not live**
until the host runs the new build. Order of operations:

1. **Change the code** in `rust/` (or an embedded `*.sh` script).
2. **Compile.** For local dev on a Mac: `cd rust && cargo build --release` →
   `rust/target/release/eco` (macOS build — enough to test `eco show`,
   `eco up dev`, etc.). For the host/CTs: `./build-release.sh` →
   `dist/x86_64-unknown-linux-musl/eco` (Linux musl; the host and every CT run
   this one).
3. **Put the new binary on the host.** Publish `dist/` and run
   `curl -fsSL https://getecosphere.com/install.sh | sh` on the host, **or**
   scp the Linux build straight to the host: `scp dist/x86_64-unknown-linux-musl/eco host:/usr/local/bin/eco`.
   Verify with `eco --version` on the host. The `eco serve` agent is a
   long-lived process — restart it after any binary swap
   (`systemctl restart eco-serve` or `pkill -f 'eco serve'` + relaunch).
4. **Commit and push** the eco repo changes.

**Without step 3 the host keeps deploying the old binary into CTs** — the code
may be in git but never run in production. Every subsequent `eco up` refreshes
the CTs with whatever `/usr/local/bin/eco` the host has at that moment.

## Homepage proof section (eco_docs)

Before writing or updating the **proof section** on the eco_docs homepage
(`eco_docs_composition/frontend/index.md`), **always read the live stress-test
report first** — `https://eco.stuff8.com/case-study/stress-test` (source of
truth; the checked-in copy is
`eco_docs_composition/frontend/case-study/stress-test.md`).

Do **not** copy the report's headline claims verbatim. The page's headline and
key observations say "zero failures," but the tables contradict that: the
Stuff8 table records **0.17% failures at 4,000 VUs**, and the raw-data tables
list small non-zero failure figures at 4,000 and 5,000 VUs. "Zero failures at
every test level" is not accurate.

Homepage proof wording must therefore say:

> Sustained a 5,000-concurrent-VU synthetic workload on a $300 mini PC with graceful degradation.

Never write "Zero failures at every test level" on the homepage.

## LXS Registry & Public Domains

The reusable LXS domains are **public** under the [`getecosphere`](https://github.com/getecosphere)
account so users can contribute. This is the operating contract agents must follow.

### Where things live

| | Repo | Visibility |
|---|---|---|
| LXS domains (auth, storage, notifications, chat, email-manager, profile, articles) | `github.com/getecosphere/<domain>` | **public** — contribution surface |
| LXS Registry domain (`registry` — browse/categorize the registry; the reference LXS domain) | `github.com/getecosphere/registry` | **public** — the canonical "how to build an LXS" example |
| LXS Registry (binaries + manifests) | `github.com/getecosphere/lxs-registry` | **public read-only** — anyone can `eco lxs pull`; publishing is gated |
| eco CLI source | `kelastanpatembok/ecology-ddd` | private (dev identity) |

Private dev checkouts live under the estate workspaces (`stuff8/<domain>`,
`getecosphere/<domain>`) and point at `kelastanpatembok` origins. The **working
registry** (`ECO_LXS_REGISTRY`) on the dev machine and Proxmox host is cloned
from `getecosphere/lxs-registry`.

### Identity rule (never break this)

- **Public publishes** (`eco lxs publish`) commit as the canonical publisher:
  `Eko SW <swdev.bali@gmail.com>`. This is hardcoded in `eco lxs publish` —
  do not change it.
- Public repo history was rewritten to that identity (anonymized); the private
  `kelastanpatembok` repos keep the real author identity.
- Use SSH host `github-getecosphere` (key `~/.ssh/id_ed25519_getecosphere`) for
  pushes to public repos; `github-kelastanpatembok` for private ones.
- `github.com/getecosphere` is the SaaS account whose PAT is
  `GITHUB_GETECOSPHERE_API_KEY` in `~/.zshrc`; `ECO_GITHUB_API_KEY` is the
  private `kelastanpatembok` account PAT.

### Contribution loop

```
fork → improve → PR → maintainer merges → tag vX.Y.Z → CI publishes
```

Each public domain repo has `.github/workflows/lxs-publish.yml`: on a `v*` tag,
CI cross-compiles the LXS (musl), publishes `name@version` to the registry under
Eko SW, and pushes the version + tag. The `GETECOSPHERE_TOKEN` secret on
each repo authorizes the registry push.

### Publish contract (the review standard for contributions)

- **Immutable versions** — never mutate a published `name@version`; fix forward.
- **Semantic versioning** + bump `contract.version` on breaking changes so
  estates detect incompatibility before runtime.
- **Explicit contracts** in `lxs.yml`: required/optional env, db, network,
  resources. Third-party LXS must not implicitly get every Estate resource
  (grant via `ecompose.yml` `grants:`).
- **No secrets** in repos or manifests.
- Declare runtime honestly (`self-contained-static`, `runtime.dependencies`).
- New LXS start `unverified`; never label `verified` without a real verification
  process.

### LXS docs bundle (required — agents can only read the docs, not the binary)

Consumers receive **only the binary + manifest + docs bundle** from the
registry. They cannot read the source, so the docs bundle *is* the interface —
for humans and for AI agents. **Every LXS MUST ship a `docs/` bundle**, and
`eco lxs publish` **fails** if the required files are missing (they are copied
into the registry version dir and served by the registry backend):

```
docs/
  README.md      # REQUIRED — the agent-facing index: 1-paragraph capability,
                 #   what it owns/never owns, composition example (ecompose.yml
                 #   with grants), and links to the files below.
  api.md         # REQUIRED — endpoint table + full request/response JSON for
                 #   happy paths AND errors (exact status codes + error body
                 #   shape + rate-limit behavior).
  changelog.md   # REQUIRED — per-version notes + migration notes for every
                 #   breaking contract.version bump.
  examples.sh    # RECOMMENDED — executable smoke test (set -euo pipefail, curl,
                 #   golden request→response pairs) that can be run against a
                 #   pulled binary or a live estate URL.
  openapi.json   # RECOMMENDED — OpenAPI 3.0 spec; machine-readable so agents
                 #   can generate clients without reading prose.
  gotchas.md     # RECOMMENDED — deployment/ops constraints learned from
                 #   production (env coupling, runtime deps, size caps, edge
                 #   limits) that are invisible in a binary.
```

Rules that matter:

- **The docs are part of the version.** They are immutable like the binary and
  `lxs.yml`; never edit a published `name@version` docs dir — fix forward.
- **Write docs from the actual code**, never from memory. Route paths, JSON
  shapes, env defaults, and error codes in `api.md`/`examples.sh`/`openapi.json`
  must match the backend exactly; example responses should be copy-pasted from
  real handlers.
- **The domain `README.md` must index the bundle** — a "## Docs" section listing
  `docs/README.md`, `docs/api.md`, `docs/changelog.md`, `docs/examples.sh`,
  `docs/openapi.json`, `docs/gotchas.md` with a one-line purpose each, so agents
  opening the repo (e.g. to improve the LXS) find the docs too.
- **New LXS created by AI**: when scaffolding a capability (via `eco lxs new`
  or by hand), create the `docs/` bundle in the same session that writes the
  backend — the publish step will refuse to ship without `README.md`,
  `api.md`, and `changelog.md`.
- Verification (`verified` status) implies the `examples.sh` smoke test passes
  against the packaged binary.

### Working with the registry

- LXS is resolved **from the remote registry**. `eco lxs search/list/pull/verify`
  and `eco up` (for `lxs:` services) all fast-forward the local
  `ECO_LXS_REGISTRY` clone from `https://github.com/getecosphere/lxs-registry.git`
  first — cloning it on first use if it does not exist. A host that is offline
  keeps serving the last-synced clone (with a warning). The old "remember to
  `git pull`" step is no longer needed for reading.
- `eco lxs build` / `publish` still write into `ECO_LXS_REGISTRY`
  (default `~/projects/lxs-registry`); publishing requires a real
  `getecosphere/lxs-registry` clone on that origin, and the operator still
  pushes it after `eco lxs publish`.
- Source-composed domains (`path:` services) ship from the developer workspace
  via `eco up --remote`; there is no central `repos.json` catalog anymore. A
  service declared `lxs: <name>@<version>` is resolved purely from the registry.
- To publish a new LXS version: bump the domain, `eco lxs build` +
  `eco lxs publish <name>@<version>`, then push the registry. The public CI
  loop is the same for community contributions.

### Creating a reusable LXS domain

The **`registry` domain** (`getecosphere/registry`, published **`registry@1.0.0`**)
is the canonical reference: a Rust **Actix** backend that clones/refreshes the
`getecosphere/lxs-registry` repo, parses every `lxs.yml`, and serves
`/api/lxs`, `/api/lxs/categories`, `/api/lxs/:name`; plus a self-contained
**Leptos SSR** frontend (`frontend/`) for browsing/categorizing packages. It
has **zero persistence** and runs `self-contained-static` — the smallest
real LXS shape. The published LXS is the backend API; the Leptos UI composes
separately via `path: registry/frontend` (live at `registry.getecosphere.com`).

To create a new reusable LXS domain:

1. **Domain repo** under `getecosphere/<name>` with `backend/` (and a
   `frontend/` only when the domain ships a standalone UI). Zero sibling-domain
   imports; the domain contract lives in `lxs.yml`.
2. **Declare the contract honestly** in `lxs.yml`: required/optional env,
   db, network, resources; `runtime.base: self-contained-static` with real
   `runtime.dependencies`; start `status: unverified`.
3. **Build + publish**:
   `eco lxs build` (cross-compiles `linux/amd64` musl) then
   `eco lxs publish <name>@<version>` — this writes the version dir + manifest
   + checksums into `ECO_LXS_REGISTRY` and tags `name-version`.
4. **Push the registry** to `github.com/getecosphere/lxs-registry` as Eco
   Creator. Never mutate a published `name@version`; fix forward by bumping.
5. **Consume from estates**: `ecompose.yml` →
   `services.<name>: { lxs: <name>@<version>, grants: { secrets: [...] } }`.
   `eco up` rejects a grant set that does not satisfy the contract. A domain
   can also compose from source (`path: <name>/backend`) while it is still
   in development — the registry UI does this today.

Community contributions keep the fork → PR → merge → `v*` tag loop; the repo's
`.github/workflows/lxs-publish.yml` publishes on a `v*` tag. Full guide:
[docs/creating-domains.md](docs/creating-domains.md).

## Topic Index

| Topic | File |
|---|---|
| **Working Agreements** — rules that apply to every task on every estate | [docs/working-agreements.md](docs/working-agreements.md) |
| **Overview & Structure** — role of eco, docs location, manifest model, execution model, host vs workspace-side, data ownership, development principles | [docs/overview.md](docs/overview.md) |
| **Architecture Doctrine** — DDD platform model, startproject, superadmin setup flow | [docs/architecture-doctrine.md](docs/architecture-doctrine.md) |
| **Design Philosophy** — design principles, reusable domain goal, relationship to Docker, scaling model, CT template strategy, scaling stages | [docs/design-philosophy.md](docs/design-philosophy.md) |
| **CTs & Resource Registry** — one CT hosting multiple estates, per-estate generated state, SQLite resource registry, multi-tenant databases | [docs/cts-and-registry.md](docs/cts-and-registry.md) |
| **Expose & Object Storage** — MinIO, Cloudflare Tunnel, gateway contract, routing convention, Cloudflare API tokens, multiple accounts | [docs/expose-and-storage.md](docs/expose-and-storage.md) |
| **Tailscale Admin Access** — operator SSH access, subnet router setup, gotchas | [docs/tailscale.md](docs/tailscale.md) |
| **Dev vs Prod URL Policy** — generated URL differences, adding frontend frameworks, LAN access for remote dev CTs | [docs/dev-vs-prod-url.md](docs/dev-vs-prod-url.md) |
| **`eco up` Contract** — what a successful `eco up` guarantees | [docs/eco-up-contract.md](docs/eco-up-contract.md) |
| **CI/CD** — deploys are dev/CI-initiated via `eco up --remote`; no GitHub webhooks | [docs/cicd.md](docs/cicd.md) |
| **Database & Port Troubleshooting** — PostgreSQL setup, gateway port stability, service port collisions, high-load crash loops | [docs/database-and-ports.md](docs/database-and-ports.md) |
| **Rust Build** — build on the developer machine, ship binaries (`eco up --remote`), no in-CT compile, hash-based skip | [docs/rust.md](docs/rust.md) |
| **Remote Deploy (eco serve agent)** — host-side HTTP agent + API keys, `eco up --remote` cross-compile on the dev machine, `.sqlx` offline requirement, `.env` handling | [docs/agent.md](docs/agent.md) |
| **LXS Registry & Public Domains** — public `getecosphere` repos, publisher identity, contribution loop + CI publish, publish contract | this page ("LXS Registry & Public Domains") |
| **Releasing & Installing** — cross-compile for macOS/Linux/Windows, `install.sh`, host install, eco inside a CT | [docs/releasing.md](docs/releasing.md) |
| **Commands Reference** — GitHub API setup, `eco sendemail`, `eco update`, `eco prox clearenv`, `eco prox showports`, `eco sync` | [docs/commands-reference.md](docs/commands-reference.md) |
| **The `rag` Domain** — reusable RAG support domain, contract, runtime, composition | [docs/rag-domain.md](docs/rag-domain.md) |
| **getecosphere.com** — the eco public estate: homepage, install.sh, auth (signup/signin), managed-estates dashboard | [docs/getecosphere.md](docs/getecosphere.md) |
| **Real-Deployment Notes** — email setup, Cloudflare adapter, gotchas from production | [docs/deployment-notes.md](docs/deployment-notes.md) |
| **Creating a New Domain** — DDD domain creation guide, no domain bleed rules, structure, contract, checklist; LXS-packaged domains with the `registry` reference | [docs/creating-domains.md](docs/creating-domains.md) |
| **Frontend UX Principles** — dashboard design, workflow-oriented UI, status visibility, semantic colors, efficiency, accessibility, SEO & OpenGraph | [docs/frontend-ux-principles.md](docs/frontend-ux-principles.md) |
