# ADR 0002 — Polyglot backends (declared run/toolchain), buildless-first

- **Status:** Accepted — 2026-07-08 (trimmed to the YAGNI core; see *Deferred*).
  **Implemented — 2026-07-10** (`backend.run`/`health`/`env`; `toolchain` and
  `build` deferred — see *As implemented*)
- **Deciders:** Bree
- **Relates to:** ADR 0001 (typing), ADR 0003 (full-surface apps + WS proxy),
  ROADMAP P2 (isolation)

## Context

Today an app's backend is hardcoded to `bun run backend/index.ts`
(`backends.rs`), and frontends are served raw (buildless). We want apps to use
**any backend language/framework** — Express, Nest.js, a **Go** binary. The
smallest thing that unlocks that: stop hardcoding the runner; let an app
*declare* how it runs.

Everything else in the first draft of this ADR (a frontend build pipeline, an
egress-filtering proxy, a dedicated build sandbox, per-app flakes) is **deferred
as YAGNI** — liquid is a single-owner tool with a handful of agent-grown apps,
and buildless has carried it fine. Ship the small, high-value core; add the rest
only when a concrete app forces it.

## Decision — the small core

### 1. Manifest gains a declared backend runner (additive)

**As implemented** (`src/apps.rs` `BackendSpec`, `src/backends.rs`):

```json
{
  "name": "Whiteboard", "icon": "🎨",
  "backend": {
    "run": ["mix", "phx.server"],   // argv VECTOR (never a shell string); PORT injected
    "health": "/health",            // optional HTTP readiness path (else: TCP connect)
    "env": {"MIX_ENV": "prod"}      // optional extra env; platform vars always win
  }
}
```

`run` is an argv vector, not a shell string — no quoting bugs, no injection
surface, typed end to end (ADR 0001: a closed contract, validated at the
manifest edge; a bad declaration skips the app with a warning rather than
half-running it). **Defaults preserve today:** `backend/index.ts` present and
no `backend` block → `["bun", "run", "backend/index.ts"]`; neither → KV-only.
Port allocation, 3-strike crash policy, restart-on-change (now the whole app
dir, ignoring `node_modules/`, `data/`, `_build/`, `deps/`), and the
`/app/<id>/api/*` proxy are unchanged.

**The run contract** (supervisor → process, via env; these override any
declared `env` on collision):

| env | meaning |
|---|---|
| `PORT` | port to listen on (the proxy targets it) |
| `LIQUID_APP_ID` | the app id |
| `LIQUID_APP_DATA_DIR` | per-app writable dir (gitignored `data/`) |

Any language reads `PORT` and serves (Elixir:
`System.get_env("PORT")`, Go: `os.Getenv("PORT")`).

**Readiness** (`health`): declared → the supervisor polls HTTP GET on that
path until 2xx; undeclared → a TCP connect suffices. Either way polling never
gives up while the process lives (fast for ~15s, then slow) — a backend that
compiles on first boot (mix, go build) flips to Running whenever it finally
listens.

**Deferred from the original draft** (add when a concrete app forces it):
`toolchain` (nixpkgs attrs resolved via `nix shell`) and `build` (a pre-run
compile step). Today the runtime comes from the host PATH — the dev shell and
the NixOS module's `path` provide `bun` (and `elixir` for the whiteboard);
build-on-boot (mix does this itself) covers the compile step. `nix shell`
resolution would break the offline test gate and adds a moving part no app
needs yet.

### 2. Frontends stay buildless

No frontend build step. For "framework DX," use **import maps + vendored ESM**
(Preact / Vue / lit / htmx dropped in as `.js` files) — offline, self-contained,
zero build. Add a real build pipeline **only** when an actual Svelte/React app
earns it (then, and only then, revisit — it belongs behind the deploy pipeline).

### 3. Toolchains via Nix

`toolchain: ["go_1_22"]` → build/run with those attrs resolved against liquid's
pinned nixpkgs (`nix shell nixpkgs#go_1_22 --command …`): one central pin,
trivial for the agent to author, reproducible. A per-app `flake.nix` escape hatch
(build via `nix develop`) is **design space, deferred** — add it if an app needs a
different nixpkgs pin or a custom environment.

### 4. Health gating is opt-in

If `backend.health` is declared, poll it after spawn and proxy once it's 200
(timeout → mark failed) — for slow-starting backends. If not declared, proxy
immediately (today's behavior). No new default.

## Worked example — a Go backend

```
apps/timers/
  app.json            # the backend block above
  index.html          # buildless (vendored ESM if it wants a framework)
  go.mod
  cmd/server/main.go  # reads PORT, serves /health and /api/*
```

Deploy: pipeline checks out → `nix shell nixpkgs#go_1_22 --command go build -o .bin/server ./cmd/server`
→ spawn `.bin/server` with `PORT` → proxy `/app/timers/api/*`. Built artifacts
(`.bin/`) are gitignored; history stays source-only.

## Security

- **Run** (the Go/Express/Nest process): backends *already* run arbitrary
  agent-authored code as the supervisor user — a Go binary is the **same trust
  level**, just another language. Ships on today's model; moves inside the Phase 2
  box when that lands.
- **Build** (`npm install`, `go mod`): pulls *third-party* code — higher risk. But
  the honest position for a single-owner box: the agent *already* runs unjailed, so
  a dedicated build-only sandbox is a half-measure. Keep any build jail **minimal**
  (deny `$DATA_DIR` / other-app reads so a build can't exfiltrate what it can't see)
  and **fold it into P2** — do not build a separate build-sandbox subsystem, and do
  **not** build an egress proxy (the filesystem jail covers exfiltration; audit /
  allowlist / caching are nice-to-have for a *future* untrusted-apps world).

## Explicitly deferred (YAGNI)

Add each only when a concrete app forces it:
- **Frontend build pipeline** (Vite/Svelte/React) — buildless + vendored ESM first.
- **Egress-filtering proxy** — filesystem jail suffices; revisit for untrusted apps.
- **Dedicated build sandbox** — fold into P2; keep interim jails minimal.
- **Per-app `flake.nix`** — attr-list default covers ~90%.

## Sequencing

1. **Smallest slice:** the declared `run`/`toolchain` generalization of
   `backends.rs` (defaults-preserve-today) + a Go backend proof. Nothing else.
2. Add a deferred piece **only** when a second/third real need appears — and fold
   any sandboxing into P2 rather than a parallel subsystem.

## Compatibility

Additive — today's `name/icon/description` apps are unchanged; graduation carries
`app.json` + source; the agent skill (`prompt.ts`) keeps the **buildless-first**
bias and reaches for a declared backend/toolchain only when the app earns it.
