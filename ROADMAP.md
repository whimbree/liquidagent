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

### M6+ — hardening & delight (existing Phase 2/3 track)

gVisor per app **and per harness**, scheduler (PULSE/CRON) with Web Push
("your agent noticed something overnight"), voice input, per-app origins for
real browser-level isolation, Hyperlight plugins.

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
