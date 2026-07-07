# liquid — architecture & design document

> A self-hosted, git-versioned, hypervisor-isolated AI agent and personal software factory.
> Single binary. NixOS first-class. No curl pipes. No required relay. No obfuscated code. No rug-pull licenses. Other platforms when they can be done right.
> For the kind of person who `npm pack`s things that don't have public repos.


---

## Table of contents

1. [Why this exists](#1-why-this-exists)
2. [What we are building](#2-what-we-are-building)
3. [What we are explicitly not building](#3-what-we-are-explicitly-not-building)
4. [High-level architecture](#4-high-level-architecture)
5. [Technology choices](#5-technology-choices)
6. [Component design](#6-component-design)
   - 6.1 [Rust supervisor binary](#61-rust-supervisor-binary)
   - 6.2 [Workspace agent (Bun/TypeScript)](#62-workspace-agent-buntypescript)
   - 6.3 [Svelte dashboard](#63-svelte-dashboard)
   - 6.4 [Agent memory system](#64-agent-memory-system)
   - 6.5 [Scheduler (PULSE + CRON)](#65-scheduler-pulse--cron)
   - 6.6 [MCP integration](#66-mcp-integration)
   - 6.7 [Networking & access](#67-networking--access)
7. [Git workspace substrate](#7-git-workspace-substrate)
8. [Build & deploy pipeline](#8-build--deploy-pipeline)
   - 8.1 [Frontend — instant HMR](#81-frontend--instant-hmr)
   - 8.2 [Backend — async pipeline](#82-backend--async-pipeline)
   - 8.3 [Pipeline modes](#83-pipeline-modes)
   - 8.4 [Agent review team](#84-agent-review-team)
9. [Per-app gVisor sandboxing](#9-per-app-gvisor-sandboxing)
10. [Hyperlight plugin system (v3)](#10-hyperlight-plugin-system-v3)
    - 10.1 [Runtime comparison](#101-runtime-comparison)
    - 10.2 [Wasmtime analysis](#102-wasmtime-analysis)
    - 10.3 [Plugin build pipeline](#103-plugin-build-pipeline)
    - 10.4 [Guest constraints](#104-guest-constraints)
11. [NixOS packaging](#11-nixos-packaging)
    - 11.1 [Flake structure](#111-flake-structure)
    - 11.2 [NixOS module](#112-nixos-module)
    - 11.3 [Microvm integration](#113-microvm-integration)
12. [Security model](#12-security-model)
13. [Code standards](#13-code-standards)
    - 13.1 [TypeScript standards](#131-typescript-standards)
    - 13.2 [Rust standards](#132-rust-standards)
    - 13.3 [General best practices](#133-general-best-practices)
14. [What the original Bloby does wrong](#14-what-the-original-bloby-does-wrong)
15. [LID design documents](#15-lid-design-documents)
    - 15.1 [HLD — high-level design](#151-hld--high-level-design)
    - 15.2 [LLD — supervisor](#152-lld--supervisor)
    - 15.3 [LLD — workspace agent](#153-lld--workspace-agent)
    - 15.4 [LLD — build & deploy pipeline](#154-lld--build--deploy-pipeline)
    - 15.5 [LLD — per-app gVisor sandboxing](#155-lld--per-app-gvisor-sandboxing)
    - 15.6 [LLD — Hyperlight plugin system](#156-lld--hyperlight-plugin-system)
    - 15.7 [LLD — NixOS packaging](#157-lld--nixos-packaging)
16. [Implementation phases](#16-implementation-phases)
17. [Future ideas](#17-future-ideas)
18. [License](#18-license)

---

## 1. Why this exists

The original `bloby-bot` npm package is a genuinely interesting concept — a self-hosted AI agent that maintains persistent memory, builds and modifies its own full-stack workspace, and is accessible from anywhere. The architecture is clever. The execution is not.

**Problems with the original:**

- **Obfuscated code** — Socket.dev flags it. Dead GitHub links for months despite "open source" marketing.
- **License rug-pull** — talked about open-sourcing it, then renamed the project and shipped under BSL 1.1 instead. The public repo never materialised.
- **`curl | sh` installer** — downloads and executes arbitrary code, extracts to `~/.bloby/`, runs immediately.
- **Binary downloads at runtime** — `cloudflared` fetched during startup, validated only by a 10MB file size check.
- **`postinstall` scripts** — run on `npm install` before you have agreed to anything.
- **External relay dependency** — all traffic optionally proxied through `api.bloby.bot`, a third-party server that can see everything.
- **No source control** — the workspace is just files in `~/.bloby/workspace/`. No history, no rollback, no audit trail.
- **No review pipeline** — code goes straight from agent to running process with no tests or security review.
- **Completely hostile to NixOS** — FHS assumptions everywhere, global npm installs, runtime binary downloads.

**The gap in the current landscape:**

- Claude Code — stateless, terminal-bound, every session starts fresh
- Bloby — persistent workspace but opaque, no source control, no review pipeline
- Cursor/Windsurf — IDE-bound, not a daemon, not self-hosted
- Custom agent setups — everyone reinvents the same plumbing badly

This project fills the gap: a personal software factory that runs on your infrastructure, remembers everything, ships reviewed code, and has a complete audit trail of every decision it ever made.

---

## 2. What we are building

A self-hosted AI agent and personal software factory with:

- **Persistent memory** across sessions via agent-maintained markdown files
- **Self-modifying workspace** — a full-stack app the agent freely extends with new modules
- **Git-versioned workspace** — every change committed, full history, rollback at any time
- **Per-app gVisor sandboxing** — each module runs in its own isolated container (Phase 2)
- **Configurable deploy pipeline** — from "just ship it" to "reviewed, tested, and security audited"
- **Agent review team** — parallel subagents that review code before deploy (optional)
- **Instant frontend updates** via Vite HMR
- **Async backend deploys** — pipeline runs in background, supervisor hot-swaps on completion
- **Hyperlight plugin system** — agent-written Rust compiled into hypervisor-isolated sandboxes (Phase 3)
- **Proactive scheduling** — PULSE wake-ups and CRON jobs
- **Nix-first build** — `nix build` is the only build entrypoint; single binary distribution, every artifact reproducible from the flake
- **NixOS module** for microvm/flake-based homelabs

---

## 3. What we are explicitly not building

- A relay server as a *dependency* — the core must never require third-party routing (an optional, self-hostable relay is a future idea, see Section 17)
- Windows or macOS support in v1 — NixOS is the primary, first-class target; portability comes later, via real packages, never via curl pipes or PowerShell one-liners
- Anything that downloads binaries at runtime
- Anything with obfuscated code
- A hosted/SaaS version of the agent itself
- A `curl | sh` installer
- A `postinstall` npm script that modifies the filesystem
- A consumer product for non-technical users (v1)
- A calorie counter app

---

## 4. High-level architecture

```
User (phone/browser)
  |
  | HTTPS via your reverse proxy (SSO in front)
  v
Supervisor binary (Rust)                      port 3000
  |
  +-- HTTP server (axum)                      routes all incoming traffic
  +-- WebSocket server                        real-time chat protocol
  +-- Worker API (embedded)                   SQLite, auth, conversations, settings
  +-- Scheduler                               PULSE + CRON runner
  +-- Agent harness manager                   primary agent + review team orchestration
  +-- Pipeline runner                         build, review, deploy coordination
  +-- gVisor runtime (Phase 2)               per-app container lifecycle management
  +-- Hyperlight runtime (Phase 3)           embedded VMM for plugin execution
  |
  +-- spawns: Workspace backend (Bun)         port 3004   user-editable server
  +-- spawns: Vite dev server                 port 3002   dashboard with HMR
  +-- manages: workspace git repo             agent-modifiable, versioned, reviewable
```

**Routing table:**

| Path | Target | Notes |
|------|--------|-------|
| `/chat/*` | Pre-built chat SPA static files | Survives dashboard crashes |
| `/api/*` | Worker API (embedded) | Auth-gated on mutations |
| `/app/<name>/api/*` | Per-app gVisor container | Routed by app name |
| Everything else | Vite dev server (port 3002) | Dashboard + HMR |

**Sacred vs modifiable boundary:**

| Layer | Who owns it | Agent can modify? |
|-------|-------------|-------------------|
| Rust supervisor | Platform | Never |
| Worker API | Platform | Never |
| Per-app backends | User/agent | Yes — via pipeline |
| Workspace client | User/agent | Yes — instant HMR |
| Memory files | Agent | Yes — directly |
| Rust plugins (Phase 3) | Agent | Yes — via pipeline |

---

## 5. Technology choices

### Rust (supervisor)

Single static binary. `axum` for HTTP/WebSocket, `tokio` for async and process management, `rusqlite` (bundled, no system dep) for SQLite, `hyper` for reverse proxy, `cron` crate for schedule parsing, `serde`/`serde_json` for serialization, `notify` for filesystem watching.

No Node version drama. No `node_modules` in the supervisor. No runtime deps except the Bun binary for the workspace layer and `runsc` (gVisor) for Phase 2.

### Bun + TypeScript (workspace agent and backends)

- Runs TypeScript natively — no `tsc`, no `ts-node`, no `tsx`
- ~7ms cold start — process kill + respawn feels instant, eliminates need for Wasmtime hot-reload complexity
- Built-in test runner (`bun test`) used by the tester review agent
- `Bun.serve()` for workspace backends — faster than Express, typed, no extra deps
- `bun:sqlite` built-in — workspace apps get SQLite without adding `better-sqlite3`
- Web-compatible APIs (`fetch`, `WebSocket`, `ReadableStream`) native

Strict TypeScript throughout — see [Section 13](#13-code-standards).

### Svelte + Vite (dashboard and chat SPA)

Svelte over React:
- Compiled output, no virtual DOM overhead
- Smaller bundle — matters for the chat SPA that must survive dashboard crashes
- Simpler reactivity model for a dashboard that accumulates state over time
- Bun + Vite + Svelte is a clean, consistent stack

Pre-built dashboard SPA. Vite dev server for HMR during operation. Chat SPA built separately as a static bundle in an iframe — isolated Svelte tree, separate build, separate error boundary.

### SQLite (worker database)

`~/.local/share/liquid/memory.db` (XDG-compliant). WAL mode. Bundled via `rusqlite` with `bundled` feature — no system SQLite dep.

### Git (workspace substrate)

The workspace is a git repository. Every agent change is a commit. Every review result committed alongside the code. Full history, rollback, audit trail. See [Section 7](#7-git-workspace-substrate).

### gVisor systrap (Phase 2 per-app isolation)

gVisor with the `systrap` platform — uses Linux seccomp-bpf for syscall interception, **no KVM required**, works inside microvms without nested virtualisation. Each agent-built app runs in its own `runsc` container. Rogue code in one app cannot touch another app or the supervisor.

### Hyperlight (Phase 3 plugin sandbox)

Microsoft's embedded VMM library. KVM on Linux, no guest OS, microsecond function call latency. Agent-written Rust plugins compiled to Hyperlight guest binaries, running in hardware-isolated sandboxes embedded in the supervisor. Requires KVM passthrough to the microvm (NixOS config change for Phase 3).

---

## 6. Component design

### 6.1 Rust supervisor binary

Single distributable artifact. On startup:

1. Parse `~/.config/liquid/config.toml`
2. Initialize SQLite (run migrations)
3. Initialize workspace git repo if first run (copy template, `git init`, initial commit)
4. Bind axum HTTP + WebSocket server
5. Spawn Vite dev server (wait for ready signal)
6. Spawn initial workspace backend (wait for `/health` 200)
7. Start scheduler tokio task
8. Start workspace ref watcher (detects new commits directly — no git hooks, see Section 8.2)
9. Write pidfile to `~/.local/state/liquid/supervisor.pid`

**Hot-swap on deploy:**

```
new_release_detected:
  1. set backend_state = Draining
  2. return 503 + Retry-After on new /app/<name>/api/* requests
  3. wait for in_flight_count == 0 (max 30s, then force)
  4. kill or stop current container/process
  5. spawn new container/process from release artifact
  6. wait for /health 200 (timeout 15s)
  7. set backend_state = Running
  8. broadcast app:deploy-complete to WebSocket clients
```

**Error handling:**

- App crash → restart up to 3 times (reset counter if alive >30s), notify clients
- Vite crash → restart once, log, non-fatal
- Agent IPC error → send `bot:error` to clients, log, continue
- Pipeline failure → log, notify clients, do NOT deploy
- SQLite error → fatal, log and exit

**Auth middleware:**

Bearer token validation on `/api/*` POST/PUT/DELETE. Tokens in SQLite, 7-day expiry, 60s in-memory cache. Auth-exempt: login, health, onboard. App routes unauthenticated — apps handle their own auth or inherit from the nginx SSO layer upstream.

### 6.2 Workspace agent (Bun/TypeScript)

Claude Agent SDK harness. Runs as a Bun child process. Communicates with supervisor via stdin/stdout JSON lines (discriminated unions, Zod-validated at both ends).

**Memory injection at query time:**

```typescript
const MEMORY_FILES = [
  'MYSELF.md',
  'MYHUMAN.md',
  'MEMORY.md',
  'PULSE.json',
  'CRONS.json',
] as const;

async function buildSystemPrompt(workspaceDir: string): Promise<string> {
  const contents = await Promise.all(
    MEMORY_FILES.map((f) => readFileSafe(path.join(workspaceDir, f)))
  );
  const skills = await discoverSkills(path.join(workspaceDir, 'skills'));
  const mcpServers = await loadMcpConfig(path.join(workspaceDir, 'MCP.json'));
  return [
    BASE_SYSTEM_PROMPT,
    formatPipelineMode(currentPipelineMode),
    ...contents,
    formatSkillsIndex(skills),
    formatMcpList(mcpServers),
  ].join('\n\n---\n\n');
}
```

**Pipeline awareness:**

The system prompt tells the agent the current pipeline mode. In `strict` mode it writes more thorough commit messages and comments its code knowing a review team will evaluate it. In `vibe` mode it knows commits deploy immediately.

**Git commit discipline:**

After each logical unit of work, the agent commits with a descriptive message. The system prompt instructs: imperative present tense, explain *why* not just *what*, never commit `.env` or credential files, commit after logical units not after every file write.

**IPC protocol:**

```typescript
type AgentRequest =
  | { type: 'query'; id: string; data: QueryData }
  | { type: 'stop'; id: string }
  | { type: 'clear'; id: string };

type AgentEvent =
  | { type: 'token'; id: string; data: string }
  | { type: 'tool'; id: string; name: string; status: 'start' | 'done' }
  | { type: 'done'; id: string; usedFileTools: boolean }
  | { type: 'error'; id: string; message: string };
```

### 6.3 Svelte dashboard

Pre-built at package time, served as static files from the nix store. Vite dev server for HMR during operation.

Chat SPA is a separate Svelte app in an iframe at `/chat/`. Built independently, served as static files, completely isolated from the dashboard. If the agent crashes the dashboard, the chat stays alive and you can ask it to fix itself.

Chat bubble injected into the dashboard via `widget.js` (vanilla JS). Floating panel, 480px wide, full screen on mobile. `postMessage` protocol between widget and iframe.

**Onboarding wizard (first run, inside chat iframe):**

1. Provider selection (Claude / OpenAI / Ollama)
2. Model selection
3. OAuth or API key auth
4. Done — pipeline mode configured via settings after onboarding

### 6.4 Agent memory system

All memory in files. Agent reads and writes with built-in file tools. No vector database, no embeddings, no external memory API.

| File | Purpose |
|------|---------|
| `MYSELF.md` | Agent identity, personality, operating manual. Agent-authored and maintained. |
| `MYHUMAN.md` | User profile — preferences, context, everything learned about you. |
| `MEMORY.md` | Long-term curated knowledge. Distilled from daily notes. |
| `memory/YYYY-MM-DD.md` | Daily notes. Raw, append-only log. |

All four injected into every system prompt. The agent decides what to write, when, and how to distil daily notes into long-term memory. The commit history of these files is a changelog of everything the agent has learned about you.

### 6.5 Scheduler (PULSE + CRON)

Tokio task in supervisor. Ticks every 60 seconds.

**PULSE** — periodic wake-ups:

```json
{
  "enabled": true,
  "intervalMinutes": 30,
  "quietHours": { "start": "23:00", "end": "07:00" }
}
```

**CRON** — scheduled tasks:

```json
[{
  "id": "uuid",
  "schedule": "0 9 * * *",
  "task": "Check the weather and summarise yesterday's notes into MEMORY.md",
  "enabled": true,
  "oneShot": false
}]
```

`<Message>` blocks in scheduler responses are extracted and sent as Web Push notifications to the user's phone. One-shot crons auto-remove after firing. Quiet hours suppress pulses.

### 6.6 MCP integration

`workspace/MCP.json` configures MCP servers loaded at agent query time. Agent discovers and uses tools automatically.

```json
{
  "servers": [
    {
      "name": "filesystem",
      "type": "stdio",
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/home/user/documents"]
    }
  ]
}
```

MCP servers declared as Nix dependencies where available as packages, otherwise run via `bunx` for community servers.

### 6.7 Networking & access

No cloudflared. No relay. No third-party routing.

**Bring your own reverse proxy:** whatever gateway you already run (nginx, Caddy, Traefik) terminates HTTPS with SSO in front. One upstream and a WebSocket-aware location block per service. That's the entire "relay" problem solved in ~12 lines of config. On the reference homelab this is an nginx gateway microvm; a Tailscale-only deployment with no public exposure works just as well. The supervisor itself never does TLS, auth federation, or tunnelling — that is deployment, not core.

**Port allocation:**

| Port | Service | Bound to |
|------|---------|---------|
| 3000 | Supervisor | configurable interface (default: localhost; your proxy fronts it) |
| 3002 | Vite dev server | 127.0.0.1 only |
| 3004+ | Per-app backends | 127.0.0.1 only |

---

## 7. Git workspace substrate

The workspace is a git repository from day one. This is the most important architectural decision separating this project from the original Bloby.

**What you get for free:**

- Every agent edit is a commit with a message the agent writes — a log of its reasoning
- `git log` is the history of everything ever built
- `git diff` shows exactly what changed between any two points
- Rollback is `git revert` — the agent can do it itself
- Review results recorded in a supervisor-owned audit repo, keyed by workspace commit — a record of why each release was approved or rejected
- The agent's creative output has a changelog
- Push to Gitea for offsite backup

**Workspace repo structure:**

```
workspace-repo/
  apps/                         ← per-app modules (each gets its own gVisor sandbox)
    crm/
      index.ts
      package.json
      tsconfig.json
    finance-tracker/
      index.ts
      package.json
  src/
    plugins/                    ← agent-written Rust Hyperlight guests (Phase 3)
      weather.rs
  client/                       ← agent-modified Svelte dashboard components
    src/
  MYSELF.md
  MYHUMAN.md
  MEMORY.md
  PULSE.json
  CRONS.json
  MCP.json
  apps.toml                     ← app manifest, supervisor reads this
  memory/
  skills/
  .env                          ← gitignored, never committed
  .gitignore
```

**What deliberately does *not* live in the workspace:**

Pipeline state — review outputs, release artifacts, deploy records — lives in `$DATA_DIR/pipeline/`, owned by the supervisor:

```
$DATA_DIR/pipeline/             ← supervisor-owned git repo (append-only audit log)
  reviews/<commit>-<agent>.md   ← review team outputs, committed by the supervisor
  releases/<app>/<commit>/      ← build artifacts, written only by the pipeline runner
  deploys.log                   ← what was deployed, when, from which workspace commit
```

The workspace is agent-writable by definition. If review results or release artifacts lived inside it, the agent could forge an approval or drop an artifact straight into the release directory and skip review entirely. Keeping pipeline state outside the workspace makes the pipeline a structural boundary, not a convention — and the audit repo gives the *pipeline's* decisions the same git-versioned transparency the workspace gets. (Until the agent process itself is sandboxed, this boundary is structural rather than hardened — see Section 12.)

**`apps.toml` — app manifest:**

```toml
[[app]]
name = "crm"
dir = "workspace/apps/crm"
entry = "index.ts"
runtime = "bun"

[[app]]
name = "finance-tracker"
dir = "workspace/apps/finance-tracker"
entry = "index.ts"
runtime = "bun"
```

Supervisor routes `/app/crm/*` → crm sandbox, `/app/finance-tracker/*` → finance-tracker sandbox. Agent adds a new app by adding an entry here and committing. Supervisor detects the change and spawns a new container.

**Agent commit convention:**

- Imperative present tense: "Add WebDAV file browser module" not "Added..."
- Include *why* in the body, not just *what*
- Never commit `.env` or credential files
- Commit after logical units of work, not after every file write

---

## 8. Build & deploy pipeline

### 8.1 Frontend — instant HMR

Agent edits a Svelte component. Vite picks it up in milliseconds. Every connected browser updates instantly. No commit required, no pipeline, no restart. The frontend is the scratchpad — immediate feedback is the point.

### 8.2 Backend — async pipeline

```
agent edits workspace/apps/crm/
  → agent commits with descriptive message
  → supervisor ref watcher sees the new commit (no git hooks — see below)
  → supervisor computes changed files: git diff --name-only <last-deployed>..HEAD
  → if apps/ changed → trigger pipeline for that app only
  → pipeline runs (see modes below)
  → passes → pipeline runner builds artifact into $DATA_DIR/pipeline/releases/crm/<commit>/
  → drain in-flight requests for crm app only
  → hot-swap crm container/process from the pipeline-built artifact
  → new version live, deploy recorded in $DATA_DIR/pipeline/deploys.log
  → broadcast app:deploy-complete to WebSocket clients
```

**Why no git hooks:** hooks live in `.git/hooks/`, which the agent can edit — a hook-triggered pipeline is a pipeline the agent can silently disable or replace. The supervisor watches workspace refs itself, computes diffs itself, and only ever deploys artifacts the pipeline runner built into supervisor-owned storage. The agent's only path to production is a commit that passes.

The deploy is **scoped per app** — editing the CRM doesn't affect the finance tracker. Each app has its own pipeline, its own release artifact, its own hot-swap cycle.

**For Rust plugins (Phase 3):**

```
agent edits workspace/src/plugins/weather.rs
  → agent commits
  → supervisor ref watcher triggers the plugin pipeline
  → security agent reviews (Rust, no tester)
  → passes → cargo build (nix-pinned toolchain, background, async)
  → build succeeds → artifact to $DATA_DIR/pipeline/releases/plugins/<commit>/
  → supervisor restarts itself (systemd Restart=) → loads new Hyperlight guest binary
```

Cargo build is slow but async — the user is not waiting. Supervisor broadcasts status events during the pipeline.

### 8.3 Pipeline modes

Configured in `config.toml`. Changed at any time. Agent is told the current mode in its system prompt.

```toml
[pipeline]
# vibe     — commit and ship immediately
# reviewed — reviewer agent checks before deploy
# strict   — reviewed + tests must pass + security audit
mode = "reviewed"
```

**`vibe` mode:**
```
commit → deploy immediately
```
No review. Good for 2am experiments, throwaway modules, proving something works. Git history still exists — rollback is always available.

**`reviewed` mode:**
```
commit → reviewer agent → deploy (if approved)
                        → open issue in workspace repo (if rejected)
```
Single reviewer checks: does the implementation match the commit message? Any obvious problems?

**`strict` mode:**
```
commit → reviewer agent  ─┐
       → tester agent    ─┼→ all pass → deploy
       → security agent  ─┘           → open issue (any fail)
```
Full parallel review team. Tests written and run. Security audit. All must pass.

### 8.4 Agent review team

Parallel subagents spawned by the supervisor. Each has a narrow, specific system prompt. They only read and critique — never build.

**Reviewer:**
- Read the git diff
- Does implementation match commit message intent?
- Logical errors or missed edge cases?
- Output: `APPROVED` or `REJECTED` + reasoning

**Tester:**
- Read the implementation
- Write tests for new/changed behaviour
- Run `bun test`
- Output: `PASSED` or `FAILED` + test output

**Security:**
- Read the implementation
- Check: credential exposure, injection vulnerabilities, unchecked inputs, unexpected external calls
- Output: `APPROVED` or `FLAGGED` + specific findings

Review agents never read the workspace directly — the supervisor computes `git diff <last-deployed>..<candidate>` itself and hands the diff to them, so a compromised agent cannot feed reviewers a sanitised view. Each review is committed by the supervisor to `$DATA_DIR/pipeline/reviews/<commit-hash>-<agent>.md` before the deploy decision. Permanent paper trail, outside the agent's reach.

**Cost management — changed file routing:**

```typescript
const REVIEW_ROUTING: Record<string, readonly ReviewAgent[]> = {
  'apps/': ['reviewer', 'tester', 'security', 'types'] as const,
  'src/plugins/': ['reviewer', 'security'] as const,
  'PULSE.json': ['reviewer'] as const,
  'CRONS.json': ['reviewer'] as const,
  'MCP.json': ['reviewer'] as const,
} as const;

// Memory files and client/ — no pipeline triggered
const NO_PIPELINE_PATTERNS = [
  'MYSELF.md', 'MYHUMAN.md', 'MEMORY.md',
  'memory/', 'client/',
] as const;
```

In `vibe` mode, no review agents are spawned.

---

## 9. Per-app gVisor sandboxing

**Phase 2 feature. Phase 1 ships with bare Bun processes.**

Each agent-built app runs in its own gVisor `runsc` container. The interface is identical to Phase 1 — the supervisor still proxies HTTP to the app, the app still exposes a Bun HTTP server. The only change is the process spawning mechanism: `Command::new("bun")` becomes `Command::new("runsc")`.

**Why gVisor, why systrap:**

gVisor intercepts syscalls at the Sentry (user-space kernel) level. The `systrap` platform uses Linux seccomp-bpf — no KVM required, works inside microvms without nested virtualisation. Each app gets a full syscall interception boundary.

**The security model with per-app sandboxing:**

```
microvm@liquid
  └── supervisor (Rust, bare process, trusted)
        ├── gVisor sandbox: app-crm          (agent-built)
        ├── gVisor sandbox: app-finance       (agent-built)
        └── gVisor sandbox: app-weather       (agent-built)
```

Rogue code in `app-crm` cannot:
- Touch `app-finance`'s data
- Access the supervisor filesystem
- Reach other microvms (microvm network rules prevent this)
- Escape to the host (two layers: gVisor + microvm boundary)

**Memory overhead:** ~20-50MB per gVisor sandbox. Acceptable on a 62GB homelab.

**Migration from Phase 1 to Phase 2:**

Zero code changes in apps or the pipeline. The supervisor's process spawning abstraction is the only thing that changes. Apps don't know or care whether they're running bare or sandboxed.

---

## 10. Hyperlight plugin system (v3)

**Phase 3 feature. Not needed for Phase 1 or 2.**

### 10.1 Runtime comparison

| | Hyperlight | gVisor | Wasmtime |
|--|------------|--------|---------|
| **Cold start** | ~1-2ms | Comparable to containers | Sub-ms (AOT) |
| **Isolation** | Hardware VM (KVM) | Syscall interception (seccomp-bpf) | Software (linear memory) |
| **Compatibility** | Purpose-built guests only | Broad Linux compatibility | Wasm guests only |
| **Our use** | Rust plugins (Phase 3) | Per-app backends (Phase 2) | N/A |

Hyperlight occupies a unique position: hardware-level VM isolation with cold starts 1-2 orders of magnitude faster than Firecracker because it skips booting a guest OS entirely. The tradeoff is guests must be purpose-built.

### 10.2 Wasmtime analysis

Wasmtime was considered as an alternative for instant backend hot-reload. The argument: Wasmtime can swap a `.wasm` module at runtime in sub-milliseconds without process kill. However:

- With Bun's ~7ms cold start, process kill + respawn is already imperceptibly fast — the Wasmtime hot-reload advantage largely disappears
- Rust → `wasm32-wasip2` compilation is still slow `cargo build` — same async pipeline problem as Hyperlight guests
- No mature Rust web framework targets `wasm32-wasip2` well today — the agent couldn't write standard Bun/Express-style backends
- `hyperlight-unikraft` (run Node.js inside Hyperlight via Unikraft guest kernel) is the more interesting long-term path for hardware-isolating the workspace backend without rewriting it in Rust

**Decision:** Bun child processes for Phase 1, gVisor containers for Phase 2, Hyperlight for Phase 3 plugin compute. Wasmtime deferred unless Rust/Wasm web frameworks mature significantly.

### 10.3 Plugin build pipeline

```
agent writes workspace/src/plugins/weather.rs
  → agent commits: "Add weather plugin for PULSE morning briefing"
  → pipeline detects src/plugins/ changed
  → security agent reviews
  → approved → cargo build --target [hyperlight-guest-target] (nix-pinned toolchain)
  → build succeeds → artifact to $DATA_DIR/pipeline/releases/plugins/<commit>/
  → supervisor restarts itself (systemd) → loads new Hyperlight guest binary
  → plugin available for agent to call
```

`cargo` is a Nix dependency, pinned to a specific version. No toolchain downloads at runtime.

**Runtime Rust plugin architecture:**

The agent can also write Rust that compiles at runtime in a more experimental flow:
- Agent writes plugin, commits
- Async background job: `cargo build` (non-blocking, ~30s+)
- On success: artifact written to the pipeline release dir, supervisor restarts
- Old supervisor drains, new supervisor loads new plugin
- Slow but async — user gets a "plugin deploying" notification, then "plugin ready"

### 10.4 Guest constraints

Hyperlight guests have no OS, no syscalls. Agent-written Rust must:
- Link against the Hyperlight guest library
- Use only host functions the supervisor explicitly exposes
- Have no std networking, no filesystem access, no arbitrary syscalls
- Be statically linked

**Supervisor-exposed host functions:**

```rust
// The ONLY things a Hyperlight guest plugin can do
fn host_log(message: String) -> Result<()>;
fn host_http_get(url: String) -> Result<String>;       // allowlisted domains only
fn host_read_memory_file(filename: String) -> Result<String>; // read-only
fn host_notify(message: String) -> Result<()>;         // send Web Push notification
```

The agent learns these constraints via `workspace/skills/hyperlight-plugins/SKILL.md`. A plugin that tries to do anything outside this set simply cannot compile.

---

## 11. NixOS packaging

**Nix-first principles:**

- `nix build` is the only build entrypoint — dev, CI, and deploy all go through the flake
- Network access only inside fixed-output derivations (Bun dependency vendoring, cargo vendoring via `Cargo.lock`) — the main build runs fully offline
- Toolchains (rustc, bun, and later runsc) are pinned flake inputs — no rustup, no nvm, nothing fetched at runtime
- `nix develop` is the CI environment; `nix flake check` runs clippy (pedantic, warnings-as-errors), `cargo test`, `tsc --noEmit`, `bun test`, and `cargo audit`
- Static assets embedded into the binary at compile time via `env!()` — no runtime path discovery
- NixOS is the primary, first-class target and must always work. The core (Rust + Bun) is portable by construction; macOS and eventually Windows are later goals via real packages (nix-darwin, Homebrew, winget) — the isolation layers (gVisor, Hyperlight/KVM) are Linux-only and remain optional precisely so the core stays portable

### 11.1 Flake structure

```
liquid/
  flake.nix                   # packages, nixosModules, devShells
  Cargo.toml
  Cargo.lock
  src/                        # Rust supervisor
    main.rs
    http.rs
    ws.rs
    worker/
      mod.rs
      db.rs
      auth.rs
    scheduler.rs
    agent.rs
    pipeline.rs
    review.rs
    sandbox.rs                # Phase 2: gVisor container lifecycle
    hyperlight.rs             # Phase 3: Hyperlight plugin runtime
    config.rs
  workspace/                  # shipped with the package
    agent/
      package.json            # Bun lockfile
      tsconfig.json           # strict: true, noImplicitAny: true
      agent.ts
      memory.ts
      skills.ts
      pipeline-client.ts
    client/                   # Svelte dashboard
      src/
      vite.config.ts
    chat/                     # Chat SPA (Svelte, built separately)
      src/
      vite.chat.config.ts
  module.nix
  default-workspace/          # template copied on first run
    MYSELF.md
    MYHUMAN.md
    MEMORY.md
    PULSE.json
    CRONS.json
    MCP.json
    apps.toml
    .gitignore
    skills/
      hyperlight-plugins/SKILL.md
```

### 11.2 NixOS module

```nix
{ config, lib, pkgs, ... }:

with lib;

let cfg = config.services.liquidagent; in {
  options.services.liquidagent = {
    enable = mkEnableOption "liquid AI agent";

    port = mkOption {
      type = types.port;
      default = 3000;
    };

    dataDir = mkOption {
      type = types.str;
      default = "/var/lib/liquid";
    };

    user = mkOption { type = types.str; default = "liquid"; };
    group = mkOption { type = types.str; default = "liquid"; };

    pipelineMode = mkOption {
      type = types.enum [ "vibe" "reviewed" "strict" ];
      default = "reviewed";
      description = ''
        Deploy pipeline mode.
        vibe     = commit and ship immediately, no review
        reviewed = single reviewer agent before deploy
        strict   = reviewer + tester + security agent, all must pass
      '';
    };

    openFirewall = mkOption { type = types.bool; default = false; };
  };

  config = mkIf cfg.enable {
    users.users.${cfg.user} = {
      isSystemUser = true;
      group = cfg.group;
      home = cfg.dataDir;
      createHome = true;
    };

    users.groups.${cfg.group} = {};

    systemd.services.liquidagent = {
      description = "liquid AI agent supervisor";
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];

      environment = {
        DATA_DIR = cfg.dataDir;
        PORT = toString cfg.port;
        PIPELINE_MODE = cfg.pipelineMode;
      };

      serviceConfig = {
        ExecStart = "${pkgs.liquid}/bin/liquid";
        User = cfg.user;
        Group = cfg.group;
        Restart = "on-failure";
        RestartSec = "5s";
        NoNewPrivileges = true;
        PrivateTmp = true;
        ProtectSystem = "strict";
        ReadWritePaths = [ cfg.dataDir ];
        ProtectHome = true;
        CapabilityBoundingSet = "";
        SystemCallFilter = "@system-service";
      };
    };

    networking.firewall.allowedTCPPorts = mkIf cfg.openFirewall [ cfg.port ];
  };
}
```

### 11.3 Microvm integration (optional deployment profile)

The NixOS module above runs identically on plain NixOS — a laptop, a bare-metal server, a VPS. The microvm layout below is the reference homelab's deployment choice, documented because it's what the author runs. Nothing in the core design depends on it; it's one way (a good way) to put a hard network boundary around a `bypassPermissions` agent.

```nix
# microvms/liquid.nix
{ inputs, lib, ... }:
{
  microvm = {
    vcpu = 2;
    mem = 1024;
  };

  imports = [
    inputs.self.nixosModules.liquid
    ./defaults.nix        # existing per-VM security/networking defaults
  ];

  microvm.shares = [{
    tag = "liquid-services";
    source = "/ocean/services/liquid";
    mountPoint = "/var/lib/liquid";
  }];

  services.liquidagent = {
    enable = true;
    port = 3000;
    dataDir = "/var/lib/liquid";
    pipelineMode = "reviewed";
  };
}
```

ZFS dataset:

```bash
zfs create ocean/services/liquid
```

Workspace persists across rebuilds — lives on `ocean/services/liquid` via virtiofsd. Binary is in the nix store. Data is on ZFS. Clean separation, same pattern as immich and nextcloud.

**First-run workspace initialization:**

On first start, supervisor checks if `$DATA_DIR/workspace/.git` exists. If not: copy `default-workspace/` from nix store, `git init`, initial commit. Workspace is now writable and owned by the data dir.

---

## 12. Security model

**Priority order: transparency > capability > isolation.** The property this project is built around is that everything the agent does is declarable, version-controlled, and auditable — you can always answer "what changed, when, and why." Isolation layers (gVisor, microvm, Hyperlight) harden that story in later phases, but they are optional and layered; they are not what makes the design trustworthy. Morphy's failure wasn't weak sandboxing — it was opacity.

### Trust boundaries

| Boundary | Mechanism | Enforced from |
|---------|-----------|---------------|
| Supervisor vs workspace code | Separate process; workspace cannot import supervisor code | Phase 0 |
| Pipeline state vs agent | `$DATA_DIR/pipeline/` lives outside the workspace; supervisor computes diffs itself; no git hooks | Phase 0 (structural) |
| Supervisor vs host | systemd hardening (scoped `ReadWritePaths`, no capabilities) | Phase 0 |
| Agent process vs `$DATA_DIR` | **Not enforced yet** — same unix user, system prompt only | Phase 2 (agent sandbox) |
| App vs app | gVisor systrap per-app containers | Phase 2 |
| Plugin vs supervisor | Hyperlight KVM isolation, controlled host function API | Phase 3 |
| Service vs rest of network | Deployment choice — bare host, VM, or microvm | Yours, any phase |

### Pipeline integrity

The pipeline is only a boundary if the agent cannot influence it except by passing review:

- **No git hooks.** Hooks live in `.git/hooks/`, which the agent can edit. The supervisor watches workspace refs itself.
- **Supervisor computes diffs.** Review agents receive a diff the supervisor computed (`<last-deployed>..<candidate>`), never a view the agent assembled.
- **Pipeline state lives outside the workspace.** Reviews, artifacts, and deploy records go to `$DATA_DIR/pipeline/` (Section 7). The supervisor never reads deploy inputs from an agent-writable path.
- **Deploys only from pipeline-built artifacts.** The hot-swap loads what the pipeline runner built, never what sits in the workspace.

### Honest limitations (Phase 0/1)

Until Phase 2, the agent harness is a bare Bun process running as the same unix user as the supervisor, with `bypassPermissions` and Bash. It *could* write to `$DATA_DIR/pipeline/` or the SQLite database despite the structural separation. Plainly:

- In Phase 0/1 the real security boundary is **the host and unix user you run this on**, plus the systemd hardening below
- The pipeline defends against mistakes, drift, and bad code — not against a genuinely adversarial agent process
- Everything is still auditable after the fact (git history, audit repo), which is the property we actually prioritise
- If you want a hard boundary on day one, deploy it in a VM or microvm (Section 11.3) — cheap on NixOS, but a deployment choice, not a design dependency
- Phase 2 closes the gap: the agent harness moves into its own sandbox (gVisor, or a dedicated unix user with scoped `ReadWritePaths`), making pipeline state unreachable

### Prompt injection & memory

Memory files are injected into every system prompt. Anything written into `MYHUMAN.md` or `MEMORY.md` — including content the agent absorbed from an attacker-controlled web page, email, or MCP tool result — speaks with system-prompt authority in every future session. The review pipeline covers code; nothing reviews memory writes. Mitigations, in increasing order of cost:

1. Memory writes are git commits — poisoning is auditable and revertible after the fact (Phase 0)
2. Optional reviewer pass on memory-file diffs in `strict` mode (future, see Section 17)
3. Quarantining untrusted tool output from memory-writing turns (open research problem)

This is a known-open problem across every agent-memory system, not something this design pretends to solve.

### `bypassPermissions` rationale

The Claude Agent SDK's `bypassPermissions` mode gives the agent full tool access without confirmation prompts. This is intentional — the agent must act autonomously when you're not at a terminal. Safety rails, in the order they actually matter:

1. Git history + audit repo — every change committed, every decision recorded
2. Review pipeline — in `reviewed`/`strict` mode, code is reviewed before it runs
3. Working directory boundary — system prompt constrains agent to `workspace/`
4. systemd hardening — scoped filesystem writes, no capabilities, no privilege escalation
5. Deployment isolation (optional) — VM/microvm gives a hard network boundary
6. gVisor per-app + agent sandbox (Phase 2) — rogue code contained to its sandbox
7. Hyperlight (Phase 3) — plugin code hardware-isolated with no syscalls

### Credentials

| Credential | Storage | Notes |
|-----------|---------|-------|
| Claude OAuth token | `$DATA_DIR/credentials/claude.json` | Auto-refresh via PKCE |
| Portal password | SQLite (scrypt, random 16-byte salt) | Manual rotation |
| VAPID keys | SQLite | Generated on first boot |
| App secrets | `workspace/apps/<name>/.env` | gitignored per-app |

Nothing in the nix store. Nothing in plaintext config that ends up in git.

### Systemd hardening

`NoNewPrivileges`, `PrivateTmp`, `ProtectSystem=strict`, `ReadWritePaths` scoped to data dir, `ProtectHome`, `CapabilityBoundingSet=""`, `SystemCallFilter=@system-service`.

---

## 13. Code standards

### 13.1 TypeScript standards

All TypeScript — agent harness, worker types, IPC protocol, WebSocket messages, pipeline types — must meet these standards.

**`tsconfig.json` (base):**

```json
{
  "compilerOptions": {
    "strict": true,
    "noImplicitAny": true,
    "noUncheckedIndexedAccess": true,
    "exactOptionalPropertyTypes": true,
    "noImplicitReturns": true,
    "noFallthroughCasesInSwitch": true,
    "forceConsistentCasingInFileNames": true,
    "esModuleInterop": true,
    "moduleResolution": "bundler",
    "target": "ESNext",
    "module": "ESNext"
  }
}
```

**Rules — TypeScript:**

- **No `any`.** Not `as any`, not `as unknown as X`, not `// @ts-ignore`. No exceptions.
- **Discriminated unions** for all message types — the compiler enforces exhaustive handling via switch
- **Zod** for runtime validation at every trust boundary: IPC messages, MCP responses, Claude SDK events, HTTP request bodies, `apps.toml` parsing
- **No `!` non-null assertions** unless provably safe with a comment explaining why
- **All async functions** return typed Promises — no implicit `Promise<any>`
- **No swallowed errors** — no empty catch blocks, no `catch (e) {}`
- **Named constants** for all magic values — no bare strings or numbers in logic

```typescript
// Wrong
const TIMEOUT = 30_000;
setTimeout(fn, 30_000);

// Right
const BACKEND_DRAIN_TIMEOUT_MS = 30_000 as const;
setTimeout(fn, BACKEND_DRAIN_TIMEOUT_MS);
```

**Naming conventions:**

- Variables and functions: `camelCase`
- Types and interfaces: `PascalCase`
- Constants: `SCREAMING_SNAKE_CASE`
- Files: `kebab-case.ts`
- No abbreviations unless universally understood (`id`, `url`, `http`, `db`)

**Example — correct IPC typing with Zod:**

```typescript
import { z } from 'zod';

const AgentEventSchema = z.discriminatedUnion('type', [
  z.object({ type: z.literal('token'), id: z.string(), data: z.string() }),
  z.object({ type: z.literal('tool'), id: z.string(), name: z.string(), status: z.enum(['start', 'done']) }),
  z.object({ type: z.literal('done'), id: z.string(), usedFileTools: z.boolean() }),
  z.object({ type: z.literal('error'), id: z.string(), message: z.string() }),
]);

type AgentEvent = z.infer<typeof AgentEventSchema>;

// Compiler enforces exhaustive handling — missing case = compile error
function handleAgentEvent(event: AgentEvent): void {
  switch (event.type) {
    case 'token': return handleToken(event.data);
    case 'tool': return handleTool(event.name, event.status);
    case 'done': return handleDone(event.usedFileTools);
    case 'error': return handleError(event.message);
  }
}
```

### 13.2 Rust standards

- **No `unwrap()`** in non-test code — use `?`, `map_err`, or explicit `expect("reason")` with a comment
- **No `clone()` as a crutch** — understand ownership, clone only when necessary and document why
- **Named constants** via `const` — no magic numbers in logic
- **`#[must_use]`** on all `Result` and `Option` returning functions where ignoring is a bug
- **Error types** — define domain-specific error enums, don't use `Box<dyn Error>` everywhere
- **`clippy::all` and `clippy::pedantic`** in CI — warnings are errors

```rust
// Wrong
const TIMEOUT: u64 = 30000;

// Right
const BACKEND_DRAIN_TIMEOUT_MS: u64 = 30_000;
const MAX_BACKEND_RESTART_ATTEMPTS: u32 = 3;
const BACKEND_STABLE_UPTIME_THRESHOLD_MS: u64 = 30_000;
```

**Naming conventions:**

- Types, traits, enums: `PascalCase`
- Functions, variables, modules: `snake_case`
- Constants: `SCREAMING_SNAKE_CASE`
- No abbreviations unless idiomatic in Rust (`fn`, `impl`, `str`, `buf`)

### 13.3 General best practices

**DRY (Don't Repeat Yourself):**
- Extract repeated logic into named functions immediately — if you write the same thing twice, it has a name
- Shared types live in a `shared/` module, not duplicated across files
- Configuration constants live in one place — `config.rs` in Rust, a `constants.ts` in TypeScript

**Naming:**
- Names explain intent, not implementation: `drainAndSwapBackend()` not `processRequest()`
- Boolean variables are questions: `isRunning`, `hasFileTools`, `shouldRestart`
- Functions that return booleans start with `is`, `has`, `can`, `should`
- Avoid negation in names: `isEnabled` not `isNotDisabled`
- No single-letter variables outside of loop indices and short closures

**No magic values:**

```typescript
// Wrong
if (restartCount >= 3) { ... }
await sleep(30_000);

// Right
const MAX_BACKEND_RESTART_ATTEMPTS = 3 as const;
const BACKEND_DRAIN_TIMEOUT_MS = 30_000 as const;
if (restartCount >= MAX_BACKEND_RESTART_ATTEMPTS) { ... }
await sleep(BACKEND_DRAIN_TIMEOUT_MS);
```

**Comments:**
- Comments explain *why*, not *what* — the code says what, comments say why
- No commented-out code in commits — use git history
- Every public function/type has a doc comment
- Every non-obvious constant has a comment explaining the value's origin

**Functions:**
- Functions do one thing — if you need "and" to describe what a function does, split it
- Maximum ~40 lines per function — if longer, it's doing too much
- Early returns over nested conditionals

**Error handling:**
- Every error has context — not `Err(e)` but `Err(e).context("spawning backend process")`
- Errors propagate up with `?`, not swallowed with `unwrap()` or empty catch blocks
- Distinguish recoverable errors (restart the backend) from fatal errors (SQLite corruption → exit)

**Testing:**
- Every pure function has unit tests
- Integration tests for the pipeline runner and IPC protocol
- The tester review agent writes tests for agent-generated code — but the platform code has its own tests too

---

## 14. What the original Bloby does wrong

| Issue | Original Bloby | This project |
|-------|---------------|-------------|
| Distribution | `curl \| sh` + npm postinstall | `nix run github:you/liquid` |
| Binary integrity | 10MB file size check | Nix hash |
| Cloudflared | Downloaded at runtime | Not used — nginx gateway |
| Relay | `api.bloby.bot` sees all traffic, required for stable URLs | None required — your infrastructure; optional self-hostable relay is a someday idea |
| Source code | Obfuscated, dead GitHub links | Actual open source |
| Install scripts | `postinstall.js` modifies filesystem | Nothing runs at install |
| Source control | None — files in `~/.bloby/` | Git from day one |
| Deploy pipeline | None | Configurable (vibe/reviewed/strict) |
| Code review | None | Optional agent team |
| Rollback | Impossible | `git revert` |
| Audit trail | None | Every commit, every review |
| App isolation | None | gVisor per-app (Phase 2) |
| Data location | `~/.bloby/` (non-standard) | XDG-compliant |
| NixOS | Completely broken | First-class |
| Language | Node.js, obfuscated JS | Rust supervisor, Bun/TypeScript strict |
| Frontend | React | Svelte |
| TypeScript | No stated standard | Strict, no `any`, Zod at boundaries |
| Plugin isolation | None | Hyperlight KVM (Phase 3) |
| Platform | Windows-first curl/PowerShell installers | NixOS first-class; macOS/Windows eventually, as real packages, like civilised people |
| License | BUSL 1.1 | MIT |
| Supply chain | Socket.dev 72/100, obfuscated deps | `cargo audit` + `bun audit` in CI |

---

## 15. LID design documents

Using [Linked-Intent Development](https://github.com/jszmajda/lid). Design documents are the source of truth. Code is compiled output.

### 15.1 HLD — high-level design

**System name:** liquid (liq-**ui**-d — there's a UI in there; `liquidagent` where a longer handle is needed)

**Purpose:** A self-hosted AI agent and personal software factory. Persistent memory. Self-modifying, git-versioned workspace. Per-app isolation. Configurable review pipeline. Your infrastructure.

**Core architectural decisions:**

**Decision 1 — Rust supervisor + Bun workspace**

Options:
- A: Pure Node — fat, NixOS hostile, no single binary
- B: Pure Rust — cleanest, but Claude Agent SDK is TypeScript-only
- **C: Rust supervisor + Bun workspace child process** — single binary, Rust owns reliable infrastructure, Bun owns AI SDK integration. Chosen.

**Decision 2 — Git as workspace substrate**

Options:
- A: Files only (original Bloby) — simple, no history, no rollback
- **B: Git repository** — full history, rollback, pipeline, paper trail. Chosen.

**Decision 3 — Configurable pipeline modes**

Options:
- A: Always review — correct but slow
- B: Never review — fast but no safety net
- **C: `vibe` / `reviewed` / `strict` configurable** — user chooses the tradeoff. Chosen.

**Decision 4 — Frontend vs backend deploy strategy**

Frontend: instant Vite HMR. Backend: async pipeline. Different trust levels, different feedback loops. Not negotiable.

**Decision 5 — Per-app module system**

One monolithic backend vs per-app processes. Per-app wins: scoped deploys, scoped pipelines, gVisor isolation drops in cleanly in Phase 2 without any restructuring.

**Decision 6 — Svelte over React**

Compiled output, no virtual DOM, smaller bundle, simpler reactivity, fits the stack.

**Decision 7 — Bun over Node**

~7ms cold start eliminates the Wasmtime hot-reload argument. Native TypeScript. Built-in test runner. `Bun.serve()` is faster and simpler than Express.

**Decision 8 — gVisor systrap for Phase 2, Hyperlight for Phase 3**

gVisor systrap: no KVM needed, works in microvms, syscall-level isolation, Linux-compatible, Bun runs without modification.
Hyperlight: hardware isolation, microsecond latency, but requires purpose-built guests. Right tool for compute plugins, not HTTP servers.
Wasmtime: deferred — Bun's cold start makes it unnecessary for hot-reload, and Rust/Wasm web frameworks aren't mature enough for the agent to write them reliably.

### 15.2 LLD — supervisor

**Key data structures:**

```rust
struct Config {
    port: u16,
    data_dir: PathBuf,
    pipeline_mode: PipelineMode,
}

enum PipelineMode {
    Vibe,
    Reviewed,
    Strict,
}

enum BackendState {
    Starting,
    Running,
    Draining { in_flight: Arc<AtomicU32> },
    Stopped,
}

struct AppInstance {
    name: String,
    port: u16,
    state: BackendState,
    restart_count: u32,
    last_started: Instant,
    process: Option<Child>,          // Phase 1: bare Bun process
    container_id: Option<String>,    // Phase 2: gVisor container
}
```

**Startup sequence:**

```
1. parse config.toml
2. init SQLite, run migrations
3. check workspace git repo (init from template if first run)
4. bind axum server on config.port
5. spawn vite (wait for __VITE_READY__ on stdout, timeout 30s)
6. read apps.toml, spawn each app (wait for /health 200, timeout 30s)
7. start scheduler tokio task
8. start workspace ref watcher tokio task (polls workspace HEAD, debounced — no git hooks)
9. write pidfile
10. log __READY__
```

**apps.toml watcher:**

Supervisor also watches `workspace/apps.toml` for changes. When a new app is added, supervisor spawns it. When an app is removed, supervisor drains and stops it. This means the agent can add entirely new apps just by editing `apps.toml` and committing — no supervisor restart needed.

### 15.3 LLD — workspace agent

**Responsibility:** Wrap Claude Agent SDK, inject memory, manage sessions, emit IPC events, know pipeline mode.

**Session management:** `conversationId → claudeSessionId` in-memory Map. Lost on restart — acceptable since conversation history persists in SQLite for display.

**Skills discovery:**

```typescript
interface Skill {
  readonly name: string;
  readonly description: string;
  readonly bodyPath: string;
}

async function discoverSkills(skillsDir: string): Promise<readonly Skill[]> {
  const entries = await fs.readdir(skillsDir, { withFileTypes: true });
  const skills = await Promise.all(
    entries
      .filter((entry) => entry.isDirectory())
      .map((entry) => loadSkillFromDir(path.join(skillsDir, entry.name)))
  );
  return skills.filter((s): s is Skill => s !== null);
}
```

### 15.4 LLD — build & deploy pipeline

**Trigger:** the supervisor's ref watcher notices a new commit on the workspace repo and computes `git diff --name-only <last-deployed>..HEAD` itself. No git hooks (agent-editable), no agent-supplied file lists. The changed-file list feeds the routing logic below; review outputs and artifacts go to `$DATA_DIR/pipeline/`, never into the workspace.

**Changed file routing logic:**

```typescript
const PIPELINE_TRIGGERS: ReadonlyMap<string, readonly ReviewAgent[]> = new Map([
  ['apps/', ['reviewer', 'tester', 'security', 'types']],
  ['src/plugins/', ['reviewer', 'security']],
  ['PULSE.json', ['reviewer']],
  ['CRONS.json', ['reviewer']],
  ['MCP.json', ['reviewer']],
]);

const NO_PIPELINE_PREFIXES = [
  'MYSELF.md', 'MYHUMAN.md', 'MEMORY.md', 'memory/', 'client/',
] as const;

function determineReviewAgents(
  changedFiles: readonly string[],
  mode: PipelineMode,
): readonly ReviewAgent[] {
  if (mode === 'vibe') return [];
  if (changedFiles.every((f) => NO_PIPELINE_PREFIXES.some((p) => f.startsWith(p)))) return [];

  const agents = new Set<ReviewAgent>();
  for (const [prefix, required] of PIPELINE_TRIGGERS) {
    if (changedFiles.some((f) => f.startsWith(prefix))) {
      required.forEach((a) => agents.add(a));
    }
  }
  if (mode === 'reviewed') {
    agents.delete('tester');
    agents.delete('types');
  }
  return [...agents];
}
```

**Review agent system prompts:**

```typescript
const REVIEWER_PROMPT = `
You are a code reviewer. You will be given a git diff.
Your job: does the implementation match the commit message intent?
Are there logical errors or missed edge cases?
Respond with exactly: APPROVED or REJECTED, then a blank line, then your reasoning.
Do not suggest improvements. Do not rewrite code. Just review.
` as const;

const SECURITY_PROMPT = `
You are a security auditor. You will be given a git diff.
Your job: check for credential exposure, injection vulnerabilities,
unchecked inputs, unsafe dependencies, unexpected external network calls.
Respond with exactly: APPROVED or FLAGGED, then a blank line, then your findings.
If FLAGGED, be specific about what and where.
` as const;
```

### 15.5 LLD — per-app gVisor sandboxing

**Responsibility:** Replace bare Bun process spawning with `runsc` container lifecycle. Interface unchanged.

**Phase 1 (bare Bun):**

```rust
fn spawn_app(app: &AppConfig) -> Result<Child> {
    Command::new("bun")
        .arg("run")
        .arg(&app.entry)
        .current_dir(&app.dir)
        .env("PORT", app.port.to_string())
        .spawn()
        .map_err(|e| AppError::SpawnFailed { name: app.name.clone(), source: e })
}
```

**Phase 2 (gVisor):**

```rust
fn spawn_app_sandboxed(app: &AppConfig) -> Result<Child> {
    Command::new("runsc")
        .args(["--platform=systrap", "run"])
        .arg(format!("--bundle={}", app.bundle_dir.display()))
        .arg(&app.name)
        .spawn()
        .map_err(|e| AppError::SpawnFailed { name: app.name.clone(), source: e })
}
```

The abstraction over these two is a trait:

```rust
trait AppRuntime {
    fn spawn(&self, app: &AppConfig) -> Result<AppProcess>;
    fn stop(&self, process: &AppProcess) -> Result<()>;
}

struct BunRuntime;
struct GVisorRuntime { runsc_path: PathBuf }
```

Switching from Phase 1 to Phase 2 is a config flag change. All other code is unchanged.

### 15.6 LLD — Hyperlight plugin system

**Host function registry:**

```rust
struct HostFunctionRegistry {
    functions: HashMap<String, Box<dyn HostFunction>>,
    http_allowlist: Vec<String>,
}

impl HostFunctionRegistry {
    fn default_with_config(config: &PluginConfig) -> Self {
        let mut registry = Self::new(config.http_allowlist.clone());
        registry.register(HOST_FN_LOG, host_log_impl);
        registry.register(HOST_FN_HTTP_GET, host_http_get_impl);
        registry.register(HOST_FN_READ_MEMORY_FILE, host_read_memory_file_impl);
        registry.register(HOST_FN_NOTIFY, host_notify_impl);
        registry
    }
}

// Named constants for host function names — no magic strings
const HOST_FN_LOG: &str = "host_log";
const HOST_FN_HTTP_GET: &str = "host_http_get";
const HOST_FN_READ_MEMORY_FILE: &str = "host_read_memory_file";
const HOST_FN_NOTIFY: &str = "host_notify";
```

### 15.7 LLD — NixOS packaging

**Build process:**

Bun doesn't speak npm's lockfile and `buildNpmPackage` doesn't speak `bun.lock`, so Bun dependencies are vendored via fixed-output derivations pinned to the lockfile (maintain the `outputHash` by hand, or generate with `bun2nix`). Postinstall scripts are disabled (`--ignore-scripts`) — any dependency that genuinely needs a build step gets handled explicitly. The main build then runs fully offline. Requires the textual `bun.lock` (Bun ≥ 1.2), not the binary `bun.lockb`.

```nix
let
  bunDeps = name: src: pkgs.stdenvNoCC.mkDerivation {
    name = "liquid-${name}-deps";
    inherit src;
    nativeBuildInputs = [ pkgs.bun ];
    buildPhase = "bun install --frozen-lockfile --ignore-scripts --no-progress";
    installPhase = "cp -r node_modules $out";
    dontFixup = true;
    outputHashMode = "recursive";
    outputHashAlgo = "sha256";
    outputHash = "sha256-...";   # updated whenever bun.lock changes
  };

  bunApp = name: src: pkgs.stdenvNoCC.mkDerivation {
    name = "liquid-${name}";
    inherit src;
    nativeBuildInputs = [ pkgs.bun ];
    buildPhase = ''
      ln -s ${bunDeps name src} node_modules
      bun run build
    '';
    installPhase = "cp -r dist $out";
  };

  dashboard = bunApp "dashboard" ./workspace/client;
  chatSpa   = bunApp "chat" ./workspace/chat;
  agentDeps = bunDeps "agent" ./workspace/agent;

in pkgs.rustPlatform.buildRustPackage {
  pname = "liquid";
  version = "0.1.0";
  src = ./.;
  cargoLock.lockFile = ./Cargo.lock;

  preBuild = ''
    export DASHBOARD_DIR=${dashboard}
    export CHAT_SPA_DIR=${chatSpa}
    export AGENT_DIR=${./workspace/agent}
    export AGENT_DEPS_DIR=${agentDeps}
    export DEFAULT_WORKSPACE_DIR=${./default-workspace}
  '';
}
```

Static file locations embedded at compile time via `env!()` macros. No runtime path discovery. No downloading anything.

**Known risk:** Bun's `node_modules` layout (symlinks, platform-specific binaries) can fight `outputHashMode = "recursive"` determinism. If hashes flap across machines, switch to `bun2nix`-generated per-package derivations. Bun-on-Nix is the least-trodden path in this stack — budget real time for `EARS-NIX-001` and treat it as Phase 0 work, not a packaging afterthought, since Nix-first is a core requirement.

---

## 16. Implementation phases

### Phase 0 — walking skeleton

The riskiest integration proven first, end to end: Rust ⇄ Bun IPC streaming the Claude Agent SDK, behind real auth, reachable from a phone. Vibe mode only. Ugly is fine. The point is to start *living with it* — the whole project is a bet that a persistent agent with memory is good day-to-day, and that bet should be tested in weeks, not after the review pipeline is polished.

- [ ] `EARS-P0-001` supervisor boots: `config.toml`, SQLite init, pidfile, graceful shutdown
- [ ] `EARS-P0-002` first-run workspace init from template + `git init` + initial commit
- [ ] `EARS-P0-003` Bun agent harness child process: IPC protocol, token streaming end to end
- [ ] `EARS-P0-004` minimal chat page — a single static HTML+JS file over WebSocket, not the Svelte SPA yet
- [ ] `EARS-P0-005` session auth (scrypt password, 7-day tokens)
- [ ] `EARS-P0-006` memory file injection into system prompt
- [ ] `EARS-P0-007` agent commits its own changes (vibe mode — no pipeline yet, but history from day one)
- [ ] `EARS-P0-008` single workspace backend: spawn, health check, restart on file change
- [ ] `EARS-P0-009` `nix build` produces the whole thing; NixOS module runs it behind your reverse proxy

**Phase 0 completion criterion:** you talk to it from your phone, it edits its workspace, and `git log` shows its work.

### Phase 1 — it works and it works right

Everything else, done properly, on top of the skeleton. No shortcuts that create Phase 2 debt.

**EARS-SUP: Supervisor**
- [ ] `EARS-SUP-001` axum server starts on configured port
- [ ] `EARS-SUP-002` static file serving (dashboard, chat SPA)
- [ ] `EARS-SUP-003` reverse proxy to Vite dev server
- [ ] `EARS-SUP-004` per-app routing via `apps.toml`
- [ ] `EARS-SUP-005` SQLite init and migrations
- [ ] `EARS-SUP-006` pidfile write/cleanup
- [ ] `EARS-SUP-007` SIGINT/SIGTERM graceful shutdown
- [ ] `EARS-SUP-008` `config.toml` loading with typed struct and named constants
- [ ] `EARS-SUP-009` `apps.toml` watcher — spawn new apps on change

**EARS-WRK: Worker API**
- [ ] `EARS-WRK-001` conversation CRUD (paginated, cursor-based)
- [ ] `EARS-WRK-002` message CRUD
- [ ] `EARS-WRK-003` settings key-value
- [ ] `EARS-WRK-004` session auth (scrypt password, 7-day tokens)
- [ ] `EARS-WRK-005` Claude OAuth PKCE flow
- [ ] `EARS-WRK-006` health endpoint
- [ ] `EARS-WRK-007` VAPID key generation, Web Push subscription management

**EARS-GIT: Git workspace substrate**
- [ ] `EARS-GIT-001` first-run workspace init from template
- [ ] `EARS-GIT-002` `git init` + initial commit on first run
- [ ] `EARS-GIT-003` supervisor ref watcher — new workspace commits detected without git hooks

**EARS-WS: WebSocket and agent harness**
- [ ] `EARS-WS-001` WebSocket upgrade and auth
- [ ] `EARS-WS-002` auto-reconnect with exponential backoff
- [ ] `EARS-WS-003` heartbeat ping/pong (every 25s)
- [ ] `EARS-WS-004` message queue during disconnection
- [ ] `EARS-AGT-001` Bun child process spawn and lifecycle
- [ ] `EARS-AGT-002` IPC protocol (stdin/stdout JSON lines, Zod-validated at both ends)
- [ ] `EARS-AGT-003` token streaming from SDK through IPC to WebSocket
- [ ] `EARS-AGT-004` file change detection and app restart trigger
- [ ] `EARS-AGT-005` Claude Agent SDK integration (strict TypeScript, no `any`)
- [ ] `EARS-AGT-006` memory file injection into system prompt
- [ ] `EARS-AGT-007` skills discovery and index injection
- [ ] `EARS-AGT-008` MCP server loading from `MCP.json`
- [ ] `EARS-AGT-009` pipeline mode awareness in system prompt

**EARS-PIPE: Build & deploy pipeline**
- [ ] `EARS-PIPE-001` changed file detection via supervisor-computed git diff (`<last-deployed>..HEAD`)
- [ ] `EARS-PIPE-002` review agent spawning based on changed files + mode
- [ ] `EARS-PIPE-003` parallel reviewer / tester / security / types agents
- [ ] `EARS-PIPE-004` review results committed to `$DATA_DIR/pipeline/reviews/` (supervisor-owned audit repo)
- [ ] `EARS-PIPE-005` build trigger on review pass
- [ ] `EARS-PIPE-006` release artifact to `$DATA_DIR/pipeline/releases/<app>/<commit>/`
- [ ] `EARS-PIPE-007` deploy triggered by pipeline completion in-process — no filesystem signalling, nothing deployable from agent-writable paths
- [ ] `EARS-PIPE-008` hot-swap per app (drain → kill → spawn → ready)
- [ ] `EARS-PIPE-009` deploy notification to WebSocket clients + `deploys.log` record
- [ ] `EARS-PIPE-010` vibe mode bypasses review, deploys immediately

**EARS-SCH: Scheduler**
- [ ] `EARS-SCH-001` 60-second tick loop (tokio task)
- [ ] `EARS-SCH-002` PULSE interval evaluation
- [ ] `EARS-SCH-003` quiet hours suppression
- [ ] `EARS-SCH-004` CRON expression evaluation
- [ ] `EARS-SCH-005` one-shot cron auto-removal
- [ ] `EARS-SCH-006` `<Message>` block extraction and Web Push dispatch

**EARS-UI: Svelte dashboard and chat SPA**
- [ ] `EARS-UI-001` dashboard builds cleanly with Vite + Svelte
- [ ] `EARS-UI-002` chat SPA builds as separate Svelte bundle
- [ ] `EARS-UI-003` widget.js + iframe `postMessage` communication
- [ ] `EARS-UI-004` onboarding wizard (provider, auth, done)
- [ ] `EARS-UI-005` message list with pagination and infinite scroll
- [ ] `EARS-UI-006` streaming token rendering
- [ ] `EARS-UI-007` tool invocation display
- [ ] `EARS-UI-008` file attachment (image, audio, document)
- [ ] `EARS-UI-009` PWA manifest and push notification subscription
- [ ] `EARS-UI-010` deploy status indicator per app (pipeline running / deploying / live)
- [ ] `EARS-UI-011` git log viewer (workspace commit history in dashboard)
- [ ] `EARS-UI-012` app sidebar — each module gets a nav entry as it's built

**EARS-NIX: NixOS packaging**
- [ ] `EARS-NIX-001` `pkgs.liquid` derivation builds successfully
- [ ] `EARS-NIX-002` `nix run github:you/liquid` starts the supervisor
- [ ] `EARS-NIX-003` NixOS module with `services.liquidagent` options
- [ ] `EARS-NIX-004` systemd service with hardening options
- [ ] `EARS-NIX-005` runs as a plain NixOS module on any host; microvm profile provided as an optional example
- [ ] `EARS-NIX-006` first-run workspace initialization from nix store template
- [ ] `EARS-NIX-007` `nix flake check` passes
- [ ] `EARS-NIX-008` `cargo audit` + `bun audit` in CI, fail on high severity

**Phase 1 completion criteria:**
- [ ] Audit: no credentials in nix store
- [ ] Audit: agent cannot influence deploy decisions except by passing the pipeline (no hooks, no agent-writable pipeline paths, supervisor-computed diffs)
- [ ] Audit: workspace backend binds localhost only; network exposure is the reverse proxy's job
- [ ] Audit: no hardcoded paths, XDG-compliant throughout
- [ ] Audit: `tsc --noEmit` passes with strict config
- [ ] Audit: no `any` in TypeScript codebase
- [ ] Audit: `clippy::pedantic` passes with zero warnings
- [ ] Write README (honest, no fake claims, no dead GitHub links)
- [ ] Tag v1.0.0

### Phase 2 — it's safe

gVisor systrap per app. Interface unchanged — swap `BunRuntime` for `GVisorRuntime`.

- [ ] `EARS-GVSR-001` `runsc` declared as Nix dependency, pinned version
- [ ] `EARS-GVSR-002` `AppRuntime` trait implemented for both `BunRuntime` and `GVisorRuntime`
- [ ] `EARS-GVSR-003` OCI bundle generation per app from Bun workspace
- [ ] `EARS-GVSR-004` `runsc` container lifecycle (spawn, drain, stop, restart)
- [ ] `EARS-GVSR-005` `config.toml` flag to switch between `bun` and `gvisor` runtime
- [ ] `EARS-GVSR-006` verify all app types work under systrap
- [ ] `EARS-GVSR-007` memory overhead benchmarked, acceptable
- [ ] `EARS-GVSR-008` agent harness itself sandboxed (gVisor, or dedicated unix user + scoped `ReadWritePaths`) — closes the Phase 0/1 same-user gap so `$DATA_DIR/pipeline/` and the platform DB are actually unreachable
- [ ] Tag v2.0.0

### Phase 3 — it's powerful

Hyperlight plugins. KVM passthrough to microvm. Research territory.

- [ ] `EARS-HL-001` Hyperlight embedded in supervisor
- [ ] `EARS-HL-002` host function registry with named constants
- [ ] `EARS-HL-003` plugin load from compiled guest binary
- [ ] `EARS-HL-004` snapshot/restore per plugin call
- [ ] `EARS-HL-005` plugin build pipeline (cargo, nix-pinned toolchain)
- [ ] `EARS-HL-006` `hyperlight-plugins` SKILL.md for agent
- [ ] `EARS-HL-007` KVM passthrough in microvm NixOS config
- [ ] `EARS-HL-008` example plugin: morning weather briefing
- [ ] Tag v3.0.0

---

## 17. Future ideas

Deliberately deferred to keep scope manageable:

- **Ollama integration** — local LLM, no API costs, good for PULSE tasks
- **Voice input** — local Whisper.cpp, no OpenAI dependency
- **Multi-workspace profiles** — separate workspace per context (work, personal, home automation)
- **Workspace backup** — PULSE task that syncs daily notes offsite
- **`hyperlight-unikraft`** — run the Node/Bun workspace backend inside a Hyperlight VM using Unikraft as guest kernel, giving hardware isolation without rewriting apps in Rust (v4)
- **`liquid update`** — `nix flake update` wrapper, rebuilds and restarts cleanly
- **Docker Compose path** — once Phase 1 is solid, a non-NixOS path for broader adoption
- **Nextcloud integration** — WebDAV + WOPI, agent builds it as a workspace module in Phase 1 using its MCP tools
- **Federation** — multiple instances sharing workspace modules or memory
- **Memory write review** — reviewer agent pass on `MYSELF.md`/`MYHUMAN.md`/`MEMORY.md` diffs to catch prompt-injected memory poisoning (see Section 12)
- **macOS support** — nix-darwin module + launchd; the core (Rust + Bun) is portable by construction, isolation layers stay Linux-only
- **Windows support** — much later; supervisor as a Windows service, real installer (winget/msi), never a PowerShell curl-pipe
- **Hosted relay service** — a morphy-style convenience proxy for people without their own gateway. The non-gross version: strictly optional (the core never depends on it), open source, self-hostable, and clearly documented as "your traffic transits this box." Potentially the sustainability story, definitely not Phase 1.

---

## 18. License

MIT. No BUSL. No restrictions. No change date needed.

```
MIT License

Copyright (c) 2026 [your name]

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

---

*liquid. built with intent. no curl pipes were harmed.*
