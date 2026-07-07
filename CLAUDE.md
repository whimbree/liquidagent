# CLAUDE.md — working on liquid

liquid is a self-hosted, git-versioned AI agent and personal software factory:
a Rust supervisor + a Bun/TypeScript agent harness (Claude Agent SDK) + a
single-file web shell. You talk to it; it builds apps into a git-versioned
workspace; the apps appear on a home screen you can open, arrange, and use.

This file orients an agent (or human) working *on* liquid's platform code.
For the product vision and roadmap see `ROADMAP.md`; for the deep design and
its rationale see `agent-project.md`.

## The shape of it

```
Browser (shell.html)  ──HTTP/WS──►  Rust supervisor (localhost only)
                                      ├─ spawns: Bun agent harness (Claude Agent SDK)
                                      ├─ spawns: per-app Bun backends
                                      ├─ serves: apps from the DEPLOYED git worktree
                                      └─ owns:   SQLite, auth, pipeline, scheduler, push
```

Two git trees, and the difference is the whole security model:
- **`workspace_dir`** (`dev-workspace/`, or `$DATA_DIR/workspace` deployed) —
  the agent's. It works here; its commits advance HEAD. Memory files,
  `SHELL.json`, `CRONS.json`, `PULSE.json` are read *live* from here.
- **`served_dir`** (`$DATA_DIR/pipeline/deployed/`) — a git *worktree* checked
  out at the deployed commit, owned by the supervisor. Apps are served and
  backends run from HERE. The supervisor only ever checks it out to a commit
  the pipeline approved. This is the anti-forgery boundary: "what the agent
  wrote" and "what is live" are distinct.

## Rust modules (`src/`)

| Module | Owns |
|---|---|
| `main.rs` | Wiring, routing, `AppState`, the agent-event recorder loop, deploy reconciliation |
| `config.rs` | `LIQUID_*` env → `Config`. Dev defaults are absolute (anchored to `CARGO_MANIFEST_DIR`) so `cargo run` works from any cwd |
| `db.rs` | SQLite (rusqlite, bundled, WAL). Conversations, messages, settings, auth sessions, per-app KV, push subs. `Db::open_in_memory()` for tests |
| `auth.rs` | scrypt password (log2_n=15 — see note below), SHA-256 session tokens |
| `agent.rs` | Agent harness child process: spawn, JSON-lines IPC, respawn with backoff, `AgentRequest`/`AgentEvent` |
| `ws.rs` | Chat WebSocket. `ClientMessage` in, `ServerEvent` out (the client-facing event union) |
| `api.rs` | REST: conversations, auth, pipeline. `require_auth` middleware (Bearer or `?token=`) |
| `apps.rs` | App manifest scanning, traversal-safe static serving, per-app KV, `SHELL.json`, per-app git log |
| `backends.rs` | Per-app Bun backend lifecycle (spawn/restart/crash-policy) + the `/app/<id>/api/*` reverse proxy |
| `deploy.rs` | `DeployManager`: the worktree, pipeline modes (vibe/reviewed), the review gate |
| `scheduler.rs` | 60s tick; PULSE + CRON from workspace JSON → agent queries in the "⏰ Scheduled" conversation |
| `push.rs` | Web Push: RFC 8291 (aes128gcm) + RFC 8292 (VAPID), hand-rolled on p256/hkdf/aes-gcm |

## Agent harness (`workspace/agent/`)

Bun + Claude Agent SDK. `harness.ts` is the real one; `fake-harness.ts` speaks
the same IPC with no model (for offline tests). `review.ts` is the one-shot
read-only reviewer; `fake-review.ts` approves unless the diff says `REJECT_ME`.
`ipc.ts` is the Zod-validated line protocol; `prompt.ts` builds the system
prompt (identity + memory files + app/backend/pipeline skills) fresh every query.

**The harness resolves `claude` from PATH** (`Bun.which`, or `LIQUID_CLAUDE_BIN`)
and passes it as `pathToClaudeCodeExecutable` — the SDK's bundled CLI is a
dynamically-linked binary NixOS can't run. The flake devShell provides
`pkgs.claude-code`.

## Invariants — don't break these

- **Apps serve from `served_dir`, not `workspace_dir`.** `apps.rs` static
  serving and `backends.rs` cwd both use `served_dir`. Memory/SHELL/CRONS use
  `workspace_dir`. Getting this wrong either serves un-reviewed code (security)
  or makes the pipeline a no-op.
- **The agent never writes pipeline state.** Review records and the deployed
  worktree live in `$DATA_DIR`, not the workspace. The supervisor computes
  review diffs itself (`deployed..HEAD`) — never trusts an agent-supplied diff.
- **The reviewer fails closed.** Any reviewer error/timeout is a REJECTED, so
  unreviewable code never auto-deploys.
- **Path traversal is charset+component checked** (`apps.rs` `is_safe_app_id` /
  `sanitize_relative`). There are tests; keep them green.
- **One agent query at a time.** The harness is single-session; requests queue
  in the supervisor mpsc. This is deliberate — two agents can't safely commit
  to one git repo concurrently. Don't add a second concurrent harness on the
  same workspace.
- **Honest Phase 0/1 limitation:** the agent harness runs as the *same unix
  user* as the supervisor, so the anti-forgery boundary is structural, not
  hardened — a determined agent could write `$DATA_DIR`. Closing this is Phase
  2 (sandboxed harness). The real boundary today is the host + systemd
  hardening + the microvm.

## Running & testing

```sh
nix develop                       # cargo, rustc, bun, claude-code, chromium

# offline (no credentials): fake harness + fake reviewer
LIQUID_FAKE_AGENT=1 cargo run     # http://localhost:3000

# real agent (needs Claude Code creds or ANTHROPIC_API_KEY)
cargo run

cargo test                        # Rust unit tests (in-memory DB, no network)
cd workspace/agent && bun test    # IPC + prompt tests
bun run e2e/smoke.ts              # full-stack smoke: boots a fake-harness
                                  # supervisor, exercises auth/apps/KV/traversal/
                                  # pipeline(reject+override+approve)/graduation/
                                  # persistence, tears down. No credentials, CI-safe.
nix build .#checks.x86_64-linux.module-boots   # boots the NixOS module in a VM
```

`e2e/smoke.ts` is the checked-in regression guard (fake harness, deterministic).
Beyond it, milestone flows were verified by driving the real agent and the shell
in headless chromium (`puppeteer-core`) from throwaway scripts. The pattern that
repeatedly paid off — and caught real bugs unit tests missed (a 10s scrypt
login, a git-less nix sandbox breaking the build) — is: **boot it and drive the
real flow, observe behavior**, don't just typecheck. When changing platform
code, run `cargo test` + `bun run e2e/smoke.ts`; for anything user-facing, drive
the shell in a browser too.

Env knobs: `LIQUID_PORT`, `LIQUID_WORKSPACE_DIR`, `LIQUID_DATA_DIR`,
`LIQUID_PIPELINE_MODE` (vibe|reviewed), `LIQUID_FAKE_AGENT`, `LIQUID_CLAUDE_BIN`,
`LIQUID_AGENT_CMD`, `LIQUID_REVIEW_CMD`.

## Deploy

NixOS module: `nix/module.nix`, `services.liquidagent`. Binds localhost — front
it with your reverse proxy + SSO. `pkgs.claude-code` is unfree (allow it).
`nix build .#agent-deps` is the Bun-vendoring fixed-output derivation (hash
pinned in `flake.nix`); if it flaps across machines, switch to `bun2nix`.

Watch on first deploy: WebSocket upgrade headers through the proxy; Web Push
needs HTTPS; iOS push needs the PWA installed (16.4+); scrypt login is ~sub-300ms
in release but ~2.6s in a debug build.

## Conventions

Rust: no `unwrap()` in non-test code (use `?`/`context`/`expect("why")`), named
constants over magic numbers, domain errors via `anyhow::Context`. TypeScript:
strict, no `any`, Zod at the IPC boundary. Commit messages: imperative, explain
*why*, no attribution trailers. Never commit `.env`, `dev-data/`, `dev-workspace/`.
