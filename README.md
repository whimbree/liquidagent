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

The agent's workspace lives in `./dev-workspace` (created from the
`default-workspace/` template and `git init`ed with an initial commit on
first boot, overridable via `LIQUID_WORKSPACE_DIR`). Every change the agent
makes is a commit — `git -C dev-workspace log` is the audit trail.

## Memory

The agent has no memory between sessions except files in its workspace,
injected into the system prompt on every query (so edits take effect
immediately):

| File | Purpose |
|---|---|
| `MYSELF.md` | Agent identity and operating manual, self-authored |
| `MYHUMAN.md` | Everything it learns about you |
| `MEMORY.md` | Long-term curated knowledge |
| `memory/YYYY-MM-DD.md` | Daily notes (today's are injected too) |

It maintains these itself and commits the changes — tell it your name, kill
the harness, ask a fresh session who you are: it knows.

## NixOS module

```nix
# flake input: liquidagent.url = "github:whimbree/liquidagent";
imports = [ liquidagent.nixosModules.liquidagent ];
services.liquidagent.enable = true;   # port 3000, state in /var/lib/liquidagent
nixpkgs.config.allowUnfreePredicate = pkg:
  builtins.elem (nixpkgs.lib.getName pkg) [ "claude-code" ];
```

The service runs hardened (localhost bind, `ProtectSystem=strict`, writes
scoped to its state dir). Credentials: either log in once as the service
user (`claude` with `HOME=/var/lib/liquidagent`) or point
`services.liquidagent.environmentFile` at a file with `ANTHROPIC_API_KEY`.

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
