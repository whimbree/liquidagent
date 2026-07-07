# liquid

A self-hosted, git-versioned AI agent and personal software factory.
Rust supervisor, Bun/TypeScript agent harness (Claude Agent SDK), NixOS first-class.

**Status: Phase 0 walking skeleton.** This is the irreducible core, not a product:
a supervisor that spawns the agent harness, streams tokens over WebSocket to a
minimal chat page, and keeps the agent's workspace in git. No review pipeline,
no dashboard, no auth yet — see [agent-project.md](./agent-project.md) for the
full design and phasing.

## Quickstart

```sh
nix develop

# Offline smoke test — fake harness, no credentials needed
LIQUID_FAKE_AGENT=1 cargo run
# → open http://localhost:3000 and say something

# Real agent — needs Claude Code credentials (or ANTHROPIC_API_KEY)
cd workspace/agent && bun install && cd ../..
cargo run
```

The agent's workspace lives in `./dev-workspace` (created and `git init`ed on
first boot, overridable via `LIQUID_WORKSPACE_DIR`). Every change the agent
makes is meant to be a commit — `git -C dev-workspace log` is the audit trail.

## Spikes

The three riskiest integrations, each runnable on its own — see
[spikes/README.md](./spikes/README.md):

1. **SDK on Bun** — `bun run workspace/agent/harness.ts --once "hello"`
2. **Bun deps under Nix** — `nix build .#agent-deps` (fixed-output derivation)
3. **Rust ⇄ Bun IPC** — `LIQUID_FAKE_AGENT=1 cargo run` + the chat page

## Configuration

| Env var | Default | Meaning |
|---|---|---|
| `LIQUID_PORT` | `3000` | HTTP/WS listen port (binds localhost only) |
| `LIQUID_WORKSPACE_DIR` | `./dev-workspace` | Agent-modifiable workspace |
| `LIQUID_AGENT_CMD` | `bun run workspace/agent/harness.ts` | Harness spawn command |
| `LIQUID_FAKE_AGENT` | unset | Set to use the offline fake harness |

Network exposure is your reverse proxy's job (nginx/Caddy/Tailscale with SSO
in front) — the supervisor never does TLS or tunnelling. No relay, no
`curl | sh`, no runtime binary downloads.

## License

[MIT](./LICENSE).
