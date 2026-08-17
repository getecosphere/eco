# Route Protection & ACL — design

**Principle: all deny, explicit allow.** Every route a service exposes is
declared with an access level; undeclared routes are denied. Modeled on Spring
Security's "deny by default, allow explicitly" — but declarative, at the edge,
for the whole estate.

This is the anti-"admin route publicly exposed" mechanism. A vibecoder ships an
LXS, forgets to protect a route, and the edge still refuses it.

## The model

ACL is a **declared contract at two layers**, aggregated before enforcement:

```
LXS author declares its own routes  ─┐
                                     ├─► aggregated ACL ──► gateway enforces
Estate composer can only tighten ────┘        (deny by default)
```

### Layer 1 — per-LXS (author side): `lxs.yml`

The LXS ships with its own security contract, so its capabilities are known
before composition. `eco lxs new` scaffolds this **deny-by-default** (empty
`routes`, `default: deny`).

```yaml
# lxs.yml
access:
  default: deny
  routes:
    - path: /health
      level: public
    - path: /api/auth/login
      level: public        # token issuance must be reachable
    - path: /api/users/*
      level: auth          # any valid token
    - path: /api/users/me
      level: auth
    - path: /api/admin/*
      level: role:admin    # token + role claim
```

Levels: `public` | `auth` | `role:<name>`.

### Layer 2 — estate scope (composer side): `ecompose.yml`

The estate composes LXS and may **only tighten** — never loosen an author's
declared protection. Overrides add routes or raise the level; they cannot
downgrade `auth` to `public` or `role:admin` to `auth`.

```yaml
services:
  profile:
    lxs: profile@1.0.0
    access:
      routes:
        - path: /api/profile/admin-export
          level: role:admin   # tightened from auth
```

### Aggregation

Effective route table = union of all LXS-declared routes, with estate overrides
applied (tighten/add only). The result is a single ACL table per estate that the
gateway renders. An LXS whose contract is `default: deny` and declares nothing
exposes **nothing**.

## Enforcement

The aggregated ACL renders into the gateway. Default-deny at the edge:
an undeclared path under `/api/*` returns **403** — it is never forwarded and
never falls through to a catch-all.

Three enforcement targets, chosen by when the pain arrives:

| Phase | Target | What it enforces | Cost |
|---|---|---|---|
| P1 | configgen → Caddy | path-level deny-by-default, public matchers, 403 fallback | none (pure configgen) |
| P2 | authz middleware LXS | JWT verify + role claims on protected routes | one HTTP hop on protected routes |
| P3 | gateway LXS (replaces Caddy) | routing + JWT + roles natively, identity propagation | new binary, owned edge cases |

### P1 — generated Caddy, deny-by-default

`configgen` emits explicit `@public` matchers from the ACL table, and a final
403 fallback for any undeclared `/api/*` path. Stock Caddy can route by path
but **cannot verify JWT/roles** (apt Caddy has no JWT plugin) — so P1 covers
path-level exposure only.

### P2 — authz middleware LXS

A `route-guard@1.0.0` LXS: reads the ACL table, verifies the Bearer token
against the auth LXS's signing key, checks role claims, and either forwards
(and injects identity headers) or 403s. Caddy routes protected paths through it.
One bounded domain, versioned, publishable — exactly the LXS playbook.

### P3 — gateway LXS replaces Caddy

Since cloudflared already terminates TLS at the tunnel edge, the gateway is a
plain HTTP reverse-proxy + authorization engine. It reads the same aggregated
ACL table, verifies JWTs, enforces roles, and injects
`X-Eco-User: <sub>,roles=<...>` upstream — so backend LXS **trust the edge** and
stop re-verifying tokens themselves. ACL becomes a first-class product, versioned
and composable like any LXS.

## Identity propagation

The gateway (P2/P3) verifies the token once and injects identity upstream:

```
req + Bearer ──► gateway ──verifies──► X-Eco-User: 42,roles=admin
                                        │
                                        ▼
                                   backend LXS (trusts edge)
```

Backend LXS contract documents the trusted `X-Eco-User` header
(`contract.env.optional`). Only the gateway may set it; the gateway strips any
client-supplied `X-Eco-User` before forwarding.

## Guardrails

- `eco lxs new` scaffolds `access.default: deny` with empty routes — the author
  must explicitly open every route.
- `eco up` fails a composition that loosens protection (estate override
  downgrading an LXS level), or declares an access level the gateway cannot
  enforce (e.g. `role:` under P1) — print the exact fix.
- The generated gateway is an artifact. Manifest and generator are edited; the
  Caddyfile (P1) or gateway config (P3) is never hand-edited.
