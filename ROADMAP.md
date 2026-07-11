# liquid roadmap — from chat to personal computer

> The vision: a personal computing environment where software is grown, not
> installed. You describe an app; your agent builds it; it appears on your
> home screen as a real thing you can open, use, arrange, and evolve. Chat is
> the command line. The workspace is the disk. Git is the time machine. The
> shell — the part that doesn't exist yet — is the desktop.

This document turns that into shippable milestones. Each milestone is named
for its demo: the one moment you show someone. Nothing ships until its demo
works on a phone or a laptop, whichever it's for.

---

## The paradigm

**One app model, two presentations.**

- **Small screens (launcher):** a home grid of app icons, like Springboard.
  Tap to open full-screen, swipe/back to return. Chat is a persistent bubble
  overlaying everything.
- **Large screens (desk):** the same apps as draggable, resizable windows on
  a canvas. Positions persist. A dock shows what's running. Chat docks to
  the side or floats.

An **app** is a directory the agent creates in its workspace:

```
apps/calculator/
  app.json          # manifest: name, icon (emoji), kind, entry
  index.html        # buildless frontend — vanilla ESM, no build step
  backend/index.ts  # OPTIONAL Bun server (kind: "full"), added when needed
```

Design decisions that make this tractable (each cuts a mountain of scope):

1. **Buildless apps first.** App frontends are plain HTML/ESM served as
   static files. No Vite-per-app, no build pipeline, no bundler. The agent
   writes a file; you refresh; it's live. Build-step apps (Svelte etc.) come
   much later, via the pipeline, for apps that earn it.
2. **Apps render in iframes.** Crash isolation now (a broken app can't take
   down the shell), gVisor-per-app alignment later. The shell survives
   anything an app does; the chat survives anything the shell does.
3. **Platform KV storage.** The supervisor exposes
   `/api/kv/<app>/<key>` (scoped per app, SQLite-backed). A buildless
   frontend can be *stateful* with zero backend — which means "todo list",
   "habit tracker", "notes" all ship in the no-backend era. Backends become
   something an app graduates into, not a prerequisite.
4. **One workspace repo.** Apps live inside the agent's workspace as
   subdirectories — one history, one audit trail. Apps that grow up get
   **exported**: the agent creates a standalone repo (Gitea/GitHub) with the
   app's history carved out. Prototype in the workspace, graduate to a repo.
5. **Shell state is agent-writable.** Window positions, grid order, icons —
   a `SHELL.json` in the workspace. The agent can arrange your desktop,
   because arranging your desktop is just editing a file it owns.

---

## Milestones

### M0 — "It remembers me" ✅ (shipped)

Chat streams, memory files inject, the agent commits its work, graceful
shutdown, NixOS module. Demo: tell it your name, kill the harness, ask a
fresh session who you are.

### M1 — "Daily driver" ✅ (shipped)

Make it something you actually live with, from your phone, before building
the desktop on top.

- SQLite (rusqlite, bundled): conversations + messages persisted; chat
  history survives refresh and restart; conversation list + switch + new
- Auth: scrypt password, 7-day session tokens, WS token auth (P0-005)
- Chat page polish: markdown rendering, mobile viewport, PWA manifest
  (installable, no push yet)
- Deploy to the homelab behind the nginx/SSO gateway; use it for real
- Harness session map: conversationId → claudeSessionId (multi-conversation
  context)

**Demo:** breakfast, phone, ask it something; lunch, laptop, the
conversation is still there.
**Risk:** low — all known patterns. **Est: 1–2 focused weekends.**

### M2 — "The first app" ✅ (shipped) ← *the calculator moment*

The app model, minimum shell, and the agent knowing how to build apps.

- App model v1: `apps/<name>/app.json` + `index.html`; supervisor watches
  `apps/`, serves `/app/<name>/` static; `/api/apps` lists manifests;
  WS `apps_changed` event on create/change/delete
- Platform KV: `/api/kv/<app>/<key>` GET/PUT/DELETE, SQLite-backed
- Shell v1: home grid of app icons (from `/api/apps`), live icon pop-in when
  the agent creates an app (with a "building…" ghost state during the
  query), tap to open in a sandboxed iframe; **on wide screens, apps open as
  draggable windows** (title bar, close button, z-order — minimal WM, no
  resize/snap yet); chat becomes an overlay bubble on the shell
- Agent app-building skill: system-prompt section teaching the manifest,
  buildless conventions, the KV API, and "commit when the app works"
- Shell control tool: the agent can open/close apps for you ("open the
  calculator") via a WS control message

**Demo:** "build me a calculator" → ghost icon appears while it works → icon
lands → click → calculator opens in a window → drag it around → it still
works tomorrow.
**Risk:** medium — the shell is new UI surface; everything else is plumbing
we've already proven. **Est: 2–3 focused weekends.**

### M3 — "A place that’s yours" 🔶 (core shipped: history popover, maximize, dock, agent open_app tool)

The desk becomes a real environment, not a demo.

- Window manager grown up: resize, maximize, snap-ish behavior, dock of
  running apps, keyboard switcher; launcher gets reorder + folders-later
- `SHELL.json` persisted layout (agent-writable: "tidy my desktop",
  "put the tracker next to the calendar")
- App lifecycle UX: rename, change icon, delete (with git safety net),
  "show me what changed" (per-app git log in the shell)
- Chat ↔ app deep links: agent messages can reference apps ("I put it on
  your home screen") that open on tap

**Demo:** three apps open side by side, arranged how you like; refresh —
identical; ask the agent to redesign one while you're using another.
**Est: ~2 weekends.**

### M4 — "Apps with engines" ✅ (shipped)

Per-app backends, for apps that outgrow KV.

- `kind: "full"` apps: supervisor spawns `backend/index.ts` (Bun) with an
  allocated port, proxies `/app/<name>/api/*`, health checks, restart on
  crash (3-strike rule) and on file change
- Per-app SQLite convention (`apps/<name>/data/app.db`, gitignored)
- Agent skill update: when to add a backend, how to wire fetch calls

**Demo:** "build a workout tracker that keeps history" — server-side data,
survives everything.
**Est: 1–2 weekends.** (This is `apps.toml`/hot-swap from the design doc,
scoped to bare Bun processes — the gVisor swap point stays clean.)

### M5 — "The factory" ✅ (shipped: reviewed pipeline + anti-forgery boundary + app graduation)

The pipeline from the design doc, now that there's something worth guarding.

- Pipeline modes land: `vibe` (default) / `reviewed`; supervisor-owned
  pipeline state in `$DATA_DIR/pipeline/` per the anti-forgery design
- App graduation: "make this its own project" → agent creates a standalone
  repo (Gitea/GitHub), exports the app with history, wires the remote
- App updates as commits with visible diffs in the shell ("what did you
  change?")

**Demo:** promote the tracker to its own repo; review a change before it
deploys.

> **M0–M5 are shipped.** What follows is the road to *polish* — the difference
> between "a working demo" and "a personal computer you'd choose over a Mac."
> Some of it already partially exists (scheduler/PULSE/CRON + Web Push landed
> early, inside M4/M5); those are folded in below where they belong. The
> product milestones (M6–M10) are shippable on today's single-owner homelab.
> The **phases** (P2–P5) are the deep platform: bigger, deployment-coupled, and
> sequenced behind one coherent piece of work — the isolation substrate.

### M6 — "Mission Control" (the Control Panel)

There is no settings surface yet. The plumbing exists (`db.set_setting`,
`auth.set_password`); this is the container almost everything else slots into.
Ships as a built-in app.

- **Account:** change password (verify old → `set_password`); list/revoke
  active sessions; "log out everywhere"
- **Agent:** model picker (persist a `model` setting → harness passes it to
  `query()`; today it uses the CLI default); reasoning-effort/thinking toggle
- **Usage:** capture the `usage` + `total_cost_usd` the harness currently
  *discards* from the SDK `result` message → per-conversation + cumulative
  tokens and $. **Honest gap:** "how much of my Claude *plan* is left" has **no
  supported API** — `/usage` is a TUI-only command with no headless/`-p`
  surface. Show *consumption*; leave a `remaining` panel stubbed until there's
  a real endpoint (or a clearly-labeled best-effort probe behind a flag)
- **Appearance:** dark/light/accent, wallpaper
- **Notifications:** push-subscription management, per-source toggles
- **Storage:** disk-per-app, workspace size, prune
- **Pipeline:** mode toggle (exists) + review history
- **System:** version, health, restart, in-shell log viewer; git-remote/backup
  config

### M7 — "The desktop, finished" (completes M3’s 🔶)

Resize, maximize, dock, per-app geometry persistence, and history popover
already shipped. This closes the gap to real-WM feel.

- **App-declared geometry:** extend the manifest (`RawManifest` currently only
  has `name/icon/description`) with `width`/`height`/`minWidth`/`minHeight`/
  `maxWidth`/`maxHeight` (+ optional aspect-lock); the shell honors them on open
  instead of one hardcoded default size
- **Snapping / light tiling:** edge + halves/quarters snap zones; keyboard
  window-switcher; minimize-to-dock
- **Folders:** grid grouping as **`SHELL.json` metadata**, *not* filesystem
  dirs — app IDs and routes stay stable (iOS-style); drag-to-reorder, grid pages
- **Command palette:** ⌘K spotlight — launch apps, run shell actions, or ask
  the agent, from one bar
- **In-shell notification tray** (distinct from OS push) + global search across
  apps *and* conversations
- **Voice input (fully local):** push-to-talk dictation into chat via
  **`whisper.cpp`** (linked into the Rust supervisor with `whisper-rs`; model a
  Nix-pinned artifact). Browser captures audio → supervisor transcribes with a
  `base.en`/`small.en` model → text into the composer. Near-instant for short
  utterances on any modern multi-core CPU — no GPU, no cloud, no API key,
  nothing leaves the box. Model size is a Control-Panel setting (M6); a GPU is
  only needed if you want `medium`/`large`
- **Polish pass:** animations, loading/ghost states, the "app crashed — ask
  liquid to fix it" state that pre-fills the console error into chat

### M8 — "Read your own code" (the IDE)

- **Full VS Code in-browser** via **`openvscode-server` + Open-VSX** (MIT;
  *not* Microsoft's proprietary build/marketplace), proxied at `/ide` — reusing
  the **existing** `backends.rs` reverse-proxy pattern; Nix-packaged, version
  pinned. File tree, editor, git panel, diff view
- **Decision required — edit authorship:** do in-IDE edits land as **your own
  git commits** (a second author beside the agent — recommended; keeps history
  honest) or is the IDE **read-only**, with all writes routed through the agent?
- **Terminal-in-IDE is deferred to P3** — code-server's integrated terminal is
  arbitrary execution; it turns on when the isolation substrate does

### M9 — "Many minds" (multi-agent)

The harness process is already swappable (`LIQUID_AGENT_CMD`), but the IPC +
prompt are Claude-shaped. Generalize the seam.

- **Agent-adapter contract:** spawn · stream tokens · report tool-use · resume;
  today's `harness.ts` becomes the reference **Claude adapter**
- **Codex adapter** (or others) behind the same contract; picker in the Control
  Panel; per-conversation agent + model
- **Cost budgets & rate-limit awareness**; **interrupt/cancel** a running query;
  an **activity view** (what the agent is doing, live tool stream)
- **Conversation management:** rename · search · export · delete
- **Memory editor:** edit `MYHUMAN.md` / memory files from the shell (they're
  read live from the workspace)

### M10 — "A real platform" (app model v2)

- **Manifest schema v2:** geometry (M7), `kind`, `version`, `category`, and a
  **per-app permissions** block — which platform APIs, network egress, KV
  scope, secrets
- **Per-app secrets/env vault** (encrypted at rest); app **templates**/scaffolds
- **Per-app origins** (wildcard subdomain through the proxy) for real
  browser-level isolation — retires the "same-origin apps can call the chat WS"
  caveat
- **Jobs dashboard:** surface PULSE/CRON (the scheduler exists; it has no UI) —
  see/enable/disable/trigger scheduled agent runs
- **Rollback** (pipeline history → revert a deploy) and **backend-health
  dashboard**; **import** a graduated app back, or install an external one
- **Inter-app data** — a scoped, permissioned intent/shared-store bus so apps
  can compose
- **Browser extension** (see `docs/ideas/browser-extension.md`) — liquid's
  sensor inside the rest of the web: debug captures (tab pixels + console +
  network), "make me one like this" design capture, a research clipper,
  page-to-app extraction, eyes on authenticated pages. Deferred until a need
  bites; the no-install capture paths (shell 📸 via getDisplayMedia, PWA
  share_target, paste/drop) cover the screenshot flows today

---

## The deep platform — phases (deployment-coupled)

### P2 — Portable isolation (the substrate) ← *unlocks the whole capability tier*

Reframed from "sandbox the harness (a chore)" to **the single boundary that
makes the terminal, arbitrary packages, and GUI apps safe at once.** liquid
becomes the *orchestrator* of an isolation backend (like it already spawns Bun
backends) — the boundary just upgrades from "another process" to "another
kernel." Closes the documented Phase 0/1 hole (agent runs as the same unix
user → could write `$DATA_DIR`).

- **An `Isolation` trait**, not a hard dependency on any one tech — OS-agnostic
  core, pluggable backends (mirrors how `LIQUID_AGENT_CMD` swaps the harness).
  **Never couple the runtime to `microvm.nix`** (that would require NixOS)
- **The isolation ladder** (each rung a backend):
  1. **unix-user + namespaces** (bubblewrap / systemd hardening) — cheap,
     Linux, meaningful
  2. **gVisor** (`runsc`) — a **Nix-built OCI rootfs**, *no KVM needed*,
     ~10–30% syscall overhead, weaker (shared-kernel) boundary, some
     syscall-compat gaps. The "no-KVM" answer
  3. **microVM** — **firecracker / cloud-hypervisor called directly over its
     API socket** (not via `microvm.nix`), KVM, near-native, real kernel
     boundary. The strong rung
- **Split Nix's two jobs:** let Nix *build* the guest rootfs+kernel (reproducible,
  version-pinned **build artifact**); let the **supervisor own the runtime
  call**. Reproducible guests *without* NixOS-runtime coupling
- **The boundary (virtiofs shares):** workspace git tree *in* (agent still
  commits), host Nix store *in* read-only (so `nix shell` resolves instantly,
  no re-download), **`$DATA_DIR` stays out** — that's the whole point
- **Granularity is a knob — start coarse (one box):** the **supervisor stays
  *outside*** as the trusted root (owns `$DATA_DIR`, auth, the deploy gate);
  **agent + app backends + terminal all go *inside* one shared guest.** Simplest
  to ship, lowest overhead, and it already closes the real single-owner threat
  (host + anti-forgery protection, crash containment) — the honest asterisk
  that's been in CLAUDE.md since day one. Design the `Isolation` trait so the
  *unit* is a parameter; tightening to **per-app boxes** later is policy, not a
  rewrite. **Split when** you install an untrusted third-party app, go
  multi-user (P5), or an app backend handles untrusted network data — so one
  popped app can't reach the workspace, the agent, or another app's secrets
- Guest lifecycle (disposable vs long-lived); per-guest network policy; nested-
  virt gate if liquid itself runs in a VM

### P3 — The capability tier (rides on P2)

All of these are the *same* security gate wearing different costumes; they turn
on together once P2 exists.

- **In-shell terminal** (xterm.js) — inside the isolation boundary
- **Arbitrary packages, the Nix-native way:** `nix shell nixpkgs#anything` in a
  disposable/declared guest against the shared read-only store — versioned, not
  imperative. Per-app declared deps become part of the app's flake
- **VS Code's integrated terminal** (from M8) becomes safe here
- **Wayland GUI-app forwarding:** headless compositor (`cage` / `sway
  --headless`) → encode → **VNC (`wayvnc` → noVNC canvas)** or **WebRTC**
  (Selkies-style, lower latency) → input piped back. Solved art (Kasm / Neko).
  On NixOS: declarative, per-app, on-brand
- **GPU passthrough** (virtio-gpu / venus) for the GUI path

### P4 — Portability (macOS / Windows first-class)

Isolation is inherently OS-specific — there is **no single portable VMM**
(firecracker is Linux/KVM-only). Portability = the P2 interface with per-OS
backends, exactly how podman-machine / Lima do it.

- **macOS:** Hypervisor.framework via **`libkrun`** (one library, KVM-on-Linux +
  HVF-on-macOS behind one API — collapses two backends into one) or vfkit
- **Windows:** WHP / Hyper-V (or WSL2)
- Near-native on every OS via its *own* hardware hypervisor — the slow
  emulation path (QEMU TCG, 5–20× slower) is never shipped
- OS-agnostic path/process abstractions in the core; non-Nix packaging (mac app
  bundle, Windows service/installer)

### P5 — Multi-user & the hosted relay

- Accounts / multi-user (the isolation from P2–P4 is a prerequisite)
- The optional **self-hostable hosted relay** (morphy-style, but MIT and
  transparent) — someone else's liquid without running the metal
- Audit log, rate limiting, passkeys/2FA

---

## Continuous tracks (not milestones — always on)

- **Observability:** in-shell log viewer, per-app crash reporting, usage/health
  dashboards
- **Data / sync / backup:** git-remote backup of the workspace, multi-device
  sync (the workspace *is* git), whole-state export/import, secrets encrypted at
  rest
- **Quality:** keep `e2e/smoke.ts` green and grow it; a browser-driven test per
  user-facing feature; keep the NixOS VM boot check passing — *boot it and drive
  the real flow*, always

---

## The hard problems, named honestly

| Problem | When it bites | Position |
|---|---|---|
| Same-origin apps can call platform APIs (incl. chat WS) | M2 | Acceptable at first: apps are your own agent's code and every byte is in git. Real fix is per-app origins (wildcard subdomain through the proxy) — M6. Do scope KV writes per-app from day one. |
| Agent builds a broken app | M2 | Iframe contains the blast; shell shows an "app crashed — ask liquid to fix it" state that pre-fills a chat message with the console error. This turns failure into a feature. |
| App state schema drift as the agent edits apps | M4 | Apps own their data; the skill teaches "migrate, don't drop". Git history is the recovery path. |
| Windows vs. phone divergence | M2–M3 | One app model, two shells. Apps must not assume window size; the skill mandates responsive CSS. |
| When does an app deserve a backend? | M2–M4 | Default no. KV until the app needs server logic or shared state. The agent decides and says why. |
| Repo-per-app temptation | M5 | Resist until graduation — one workspace repo keeps the audit trail whole. Export is one-way and explicit. |
| "Plan usage remaining" isn't queryable | M6 | No supported API; `/usage` is TUI-only. Show *consumption* (tokens/$ from the SDK result), stub *remaining*, fill it when there's a real endpoint. Don't ship a brittle scraper as a core feature. |
| Human edits vs. agent authorship | M8 | A values fork: in-IDE edits commit as *you* (a second, honest author) vs. read-only + route through the agent. Recommend the former; either way, the git history stays truthful. |
| "One portable VMM" is a mirage | P2/P4 | Isolation is OS-specific; firecracker is Linux/KVM-only. Don't chase a universal VMM or ship emulation (5–20× slower). Build an interface + per-OS *fast* backends (KVM/HVF/WHP), gVisor as the Linux no-KVM rung. |
| Coupling the runtime to `microvm.nix` | P2 | Would make NixOS mandatory forever. Use Nix to *build* the guest image; own the firecracker/API runtime call yourself. Nix as builder, not orchestrator. |
| The capability tier is one security gate | P3 | Terminal, VS Code's terminal, arbitrary packages, and Wayland GUI apps are the *same* arbitrary-execution boundary. They don't ship piecemeal ahead of P2 — they ride it together. |
| gVisor ≠ a VM | P2 | It's a shared-kernel syscall substitute: no KVM, weaker boundary, compat gaps, no nested virt. Great as the no-KVM rung; not a substitute for the microVM rung when the boundary must be hard. |
| One shared box vs. per-app boxes | P2 | One box protects the *host* but is a flat trust domain *inside* — fine for a single owner running only their own agent's apps. Start there; make granularity a knob so per-app boxes become a config change, not a rewrite, when untrusted apps / multi-user arrive. |

---

## Sequencing rationale

M1 before M2 because a desktop full of apps attached to an amnesiac chat is
a demo, not a product — persistence and auth make it a daily driver, and
daily use generates the app ideas M2 needs. M2's calculator moment lands the
emotional core early (the entire remaining roadmap is "more of that,
sturdier"). Backends wait until M4 because KV makes the first five apps
possible without them, and the pipeline waits until M5 because reviewing
code matters only once apps do things worth guarding.

**Cumulative estimate to the calculator moment (end of M2): roughly 3–5
focused build sessions.** To the full "place that's yours" (M3): add ~2.
Estimates assume Claude Code does the grind and the spikes keep holding.
