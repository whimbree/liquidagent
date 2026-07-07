# Phase 0 spikes

The design doc (§16, `agent-project.md`) gates everything on three integrations.
Each is runnable independently; if all three pass, the rest of Phase 0/1 is grind,
not risk.

## Spike 1 — Claude Agent SDK on Bun

The SDK is built and tested against Node; Bun's Node-compat is good but not
total, and this is the single load-bearing dependency.

```sh
cd workspace/agent
bun install
bun run harness.ts --once "say hello and create hello.txt in your workspace"
```

**Pass:** streamed text appears, tool use is logged to stderr, and the file
lands in the workspace. Auth comes from Claude Code credentials
(`~/.claude/.credentials.json`) or `ANTHROPIC_API_KEY`.

**Status: PASSED** (2026-07-06, Bun 1.3.13, SDK 0.3.202, NixOS). One finding,
already worked around in `harness.ts`: the SDK's bundled Claude Code CLI is a
**dynamically linked generic-Linux binary** that NixOS cannot execute (exit
127, `Could not start dynamically linked executable`). The harness passes
`pathToClaudeCodeExecutable` resolved from PATH (`Bun.which("claude")`,
override via `LIQUID_CLAUDE_BIN`), and the flake devShell provides
`pkgs.claude-code`. With that, streaming, tool use, and session init all work
under Bun.

**Fallback if it misbehaves elsewhere:** run the harness on Node via the flake
instead — costs nothing architecturally; the harness barely uses Bun-specific
APIs (`Bun.env`, `Bun.argv`, `Bun.sleep`, `for await (const line of console)`).

## Spike 2 — Bun dependency vendoring under Nix

Nix-first is a hard requirement, and Bun-on-Nix is the least-trodden path in
this stack. The flake exposes the agent's `node_modules` as a fixed-output
derivation pinned to `bun.lock`.

```sh
nix build .#agent-deps
# First run fails with a hash mismatch (outputHash is lib.fakeHash):
# copy the "got: sha256-..." value into flake.nix, build again.
```

**Pass:** the build succeeds AND produces the same hash on a second machine
(or after `nix store gc` + rebuild). If the hash flaps (Bun's node_modules
layout has symlinks and platform-specific bits), switch to per-package
derivations via `bun2nix` — that's the known fallback, budget time for it.

**Status: PASSED on one machine** (2026-07-06,
`sha256-QMECHQrJdH75Gigz//pm325Sm654fIQ72m0WPq+LrQ4=` pinned in flake.nix).
Cross-machine hash stability still unverified. `nix build .#liquid` (the Rust
supervisor) also builds clean.

## Spike 3 — Rust ⇄ Bun IPC streaming

The path everything sits on: WebSocket → axum → JSON-lines over stdin →
harness → SDK → tokens back up the same pipe. The fake harness exercises it
offline (no credentials, no network):

```sh
LIQUID_FAKE_AGENT=1 cargo run
# open http://localhost:3000, send a message, watch tokens stream
```

**Pass:** tokens render incrementally in the chat page; killing the harness
process (`pkill -f fake-harness`) gets it respawned by the supervisor within
~1s and the next message still works.

**Status: PASSED** (2026-07-06) — health endpoint, chat page, WS round trip
with streamed tokens, and respawn-after-kill all verified, with both the fake
harness and the real SDK harness.
