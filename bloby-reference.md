# Morphy

https://npmx.dev/package/morphyagent

A self-hosted AI agent that runs on the user's machine with a full-stack workspace it can modify, a chat interface for remote control, and a relay system for public access via custom domains.

---

## System Overview

Morphy is three separate codebases working together:

1. **Morphy Bot** (this repo) -- runs on the user's machine. Supervisor process, worker API, workspace app, chat UI.
2. **Morphy Relay** (separate server at `api.morphyagent.com`) -- optional cloud service that maps `username.morphyagent.com` to the user's Cloudflare tunnel. Routes HTTP and WebSocket traffic. Only used with Quick Tunnels.
3. **Cloudflare Tunnel** -- exposes `localhost:3000` to the internet. Two modes:
   - **Quick Tunnel** -- zero-config, no account needed, random `*.trycloudflare.com` URL that changes on restart. Optionally paired with the relay for a permanent domain.
   - **Named Tunnel** -- persistent URL with the user's own domain. Requires a Cloudflare account and DNS setup. No relay needed.

The user chooses their tunnel mode during `morphy init` via an interactive selector.

---

## Process Architecture

When `morphy start` runs, the CLI spawns a single supervisor process. The supervisor runs the worker API in-process, spawns the backend as a child process, and manages the full lifecycle:

```
CLI (bin/cli.js)
  |
  spawns
  v
Supervisor (supervisor/index.ts)        port 3000    HTTP server + WebSocket + worker API (in-process)
  |
  +-- Worker routes (worker/index.ts)   in-process   Express API, SQLite, auth, conversations
  +-- Vite Dev Server                   port 3002    Serves workspace/client with HMR
  +-- Backend (workspace/backend/)      port 3004    User's custom Express server (child process)
  +-- cloudflared (tunnel)              --           Exposes port 3000 to the internet (quick or named)
  +-- Scheduler (supervisor/scheduler)  --           PULSE + CRON job runner (in-process)
```

Port allocation: base port (default 3000), Vite = base+2, backend = base+4.

The backend child process auto-restarts up to 3 times on crash (reset counter if alive >30s). The supervisor catches SIGINT/SIGTERM and tears everything down gracefully.

---

## Supervisor (supervisor/index.ts)

The supervisor is a raw `http.createServer` that routes every incoming request:

| Path | Target | Notes |
|---|---|---|
| `/bloby/widget.js` | Direct file serve | Chat bubble script, no-cache |
| `/sw.js`, `/bloby/sw.js` | Embedded service worker | PWA + push notification support |
| `/app/api/*` | Backend (port 3004) | Strips `/app/api` prefix before forwarding |
| `/api/*` | Worker Express app (in-process) | Auth middleware checks Bearer token on mutations |
| `/bloby/*` | Static files from `dist-chat/` | Pre-built chat SPA. HTML: no-cache. Hashed assets: immutable, 1yr max-age |
| Everything else | Vite dev server (port 3002) | Dashboard + HMR |

WebSocket upgrades:
- `/bloby/ws` -- Morphy chat. Auth-gated via query param token. Handled by an in-process `WebSocketServer`.
- Everything else -- proxied to Vite dev server for HMR.

The supervisor also:
- Manages the Cloudflare tunnel lifecycle (start, stop, health watchdog every 30s) for both quick and named tunnels
- Registers with the relay and maintains heartbeats (quick tunnel mode only)
- Runs the Claude Agent SDK when users send chat messages
- Restarts the backend process when Claude edits workspace files
- Broadcasts `app:hmr-update` to all connected dashboard clients after file changes
- Watches `workspace/backend/` for `.ts/.js/.json` changes and auto-restarts the backend
- Watches workspace root for `.env` changes (backend restart), `.restart` trigger, and `.update` trigger (deferred morphy update)
- Runs the PULSE/CRON scheduler
- Serves an embedded service worker for PWA + push notifications

### Auth Middleware

The supervisor validates Bearer tokens on `/api/*` POST/PUT/DELETE requests by calling the worker's `getSession()` database function directly (no HTTP round-trip). Token results are cached for 60 seconds. Auth-exempt routes (login, onboard, health, push, auth endpoints) skip this check.

The `/app/api/*` route has no auth -- the user's workspace backend handles its own authentication.

---

## Worker (worker/index.ts)

Express app that runs in-process within the supervisor (no separate child process). Owns the database and all platform API logic. The supervisor calls `createWorkerApp()` at startup, which initializes the database, VAPID keys, and file storage, then returns the Express app. All API responses set `Cache-Control: no-store, no-cache, must-revalidate` to prevent stale responses through the relay/CDN.

**Database:** `~/.morphy/memory.db` (SQLite via better-sqlite3, WAL mode)

Tables:
- `conversations` -- chat sessions (id, title, model, session_id, timestamps)
- `messages` -- individual messages (role, content, tokens_in, tokens_out, model, audio_data, attachments as JSON)
- `settings` -- key-value store (onboard config, provider, model, portal credentials, etc.)
- `sessions` -- auth tokens with 7-day expiry
- `push_subscriptions` -- Web Push endpoints with VAPID keys (endpoint, keys_p256dh, keys_auth)

Auto-migrations add missing columns on startup (session_id, audio_data, attachments).

Key endpoints:
- `/api/conversations` -- CRUD for conversations and messages (paginated with `before` cursor)
- `/api/settings` -- key-value read/write
- `/api/onboard` -- saves wizard configuration (provider, model, portal password, whisper key, names)
- `/api/onboard/status` -- returns current setup state (names, portal, whisper, provider, handle)
- `/api/portal/login` -- password auth (POST with JSON body or GET with Basic Auth header)
- `/api/portal/validate-token` -- session token validation (POST or GET)
- `/api/portal/verify-password` -- check password without creating session
- `/api/whisper/transcribe` -- audio-to-text via OpenAI Whisper API (10MB limit)
- `/api/handle/*` -- check availability, register, change relay handles
- `/api/context/current`, `/api/context/set`, `/api/context/clear` -- tracks which conversation is active
- `/api/auth/claude/*` -- Claude OAuth start, exchange, status
- `/api/auth/codex/*` -- OpenAI OAuth start, cancel, status
- `/api/push/*` -- VAPID public key, subscribe, unsubscribe, send notifications, status
- `/api/files/*` -- static file serving from attachment storage
- `/api/health` -- health check

Portal passwords are hashed with scrypt (random 16-byte salt, stored as `salt:hash`).

---

## Workspace

The workspace is a full-stack app template that Claude can freely modify. It lives at `workspace/` in the project and gets copied to `~/.morphy/workspace/` on first install.

```
workspace/
  client/             React + Vite + Tailwind dashboard
    src/App.tsx        Main app entry (error boundary, rebuild overlay, onboard iframe)
    index.html         PWA manifest, service worker registration, widget script
  backend/
    index.ts           Express server template (reads .env, opens app.db)
  .env                 Environment variables for the backend
  app.db               SQLite database for workspace data
  MYSELF.md            Agent identity and personality
  MYHUMAN.md           Everything the agent knows about the user
  MEMORY.md            Long-term curated knowledge
  PULSE.json           Periodic wake-up config (interval, quiet hours)
  CRONS.json           Scheduled tasks with cron expressions
  memory/              Daily notes (YYYY-MM-DD.md files, append-only)
  skills/              Skill folders (SKILL.md with name+description frontmatter)
  MCP.json             MCP server configuration (optional)
  files/               Attachment storage (audio, images, documents)
```

The backend runs on port 3004, accessed at `/app/api/*` through the supervisor proxy. The `/app/api` prefix is stripped before reaching the backend, so routes are defined as `/health` not `/app/api/health`.

The frontend is served by Vite with HMR. When Claude edits files, Vite picks up changes instantly for the frontend. The supervisor restarts the backend process after any file write by the agent.

The workspace is the only directory Claude is allowed to modify. The system prompt explicitly tells it never to touch `supervisor/`, `worker/`, `shared/`, or `bin/`.

---

## Agent Memory System

The agent has no persistent memory between sessions except through files in the workspace:

| File | Purpose |
|---|---|
| `MYSELF.md` | Agent identity, personality, operating manual. The agent's self-authored description of who it is. |
| `MYHUMAN.md` | Profile of the user -- preferences, context, everything the agent has learned about them. |
| `MEMORY.md` | Long-term curated knowledge. Distilled from daily notes into durable insights. |
| `memory/YYYY-MM-DD.md` | Daily notes. Raw, append-only log of events and observations for that day. |

All four are injected into the system prompt at query time by `bloby-agent.ts`. The agent reads and writes these files itself -- there's no external process managing them.

---

## Scheduler (supervisor/scheduler.ts)

The scheduler runs in-process within the supervisor, checking every 60 seconds.

### PULSE

Periodic wake-ups configured in `workspace/PULSE.json`:

```json
{ "enabled": true, "intervalMinutes": 30, "quietHours": { "start": "23:00", "end": "07:00" } }
```

When a pulse fires, the scheduler triggers the agent with a system-generated prompt. The agent can check in, review notes, or take proactive action. Quiet hours suppress pulses.

### CRONS

Scheduled tasks configured in `workspace/CRONS.json`:

```json
[{ "id": "...", "schedule": "0 9 * * *", "task": "Check the weather", "enabled": true, "oneShot": false }]
```

Uses `cron-parser` to match cron expressions against the current minute. One-shot crons are auto-removed after firing.

When a cron or pulse fires:
1. The scheduler calls `startBlobyAgentQuery` with the task
2. It extracts `<Message>` blocks from the agent's response
3. It sends push notifications for those messages
4. If the agent used file tools (Write/Edit), the backend is restarted

---

## Skills

The agent auto-discovers skills in `workspace/skills/`. Each skill is a folder containing a `SKILL.md` whose YAML frontmatter carries `name` (= folder name) and `description`. All three harnesses surface that metadata in context and load the body on demand: the Claude harness mirrors skills into `workspace/.claude/skills` and enables them via the Agent SDK's `skills` option, the Codex harness mirrors them into `workspace/.codex/skills` and primes them via `skills/list`, and the Pi harness injects a name+description index into the system prompt (see `supervisor/harnesses/skills.ts`).

MCP servers can be configured in `workspace/MCP.json`. The agent loads them at query time and logs which servers are active.

---

## Web Push Notifications

The worker generates VAPID keys on first boot and stores them in settings. The chat SPA requests notification permission, subscribes via the Push API, and sends the subscription endpoint + keys to the worker.

When the scheduler (or any server-side event) needs to notify the user, it calls `POST /api/push/send` which fans out `web-push` notifications to all stored subscriptions. Expired subscriptions are auto-cleaned.

---

## Morphy Chat

The chat UI is a standalone React SPA built separately (`vite.chat.config.ts` -> `dist-chat/`). It runs inside an iframe injected by `widget.js` on the dashboard.

### Why an iframe?

If Claude introduces a bug that crashes the dashboard, the chat stays alive. The user can still talk to Claude and ask for a fix. The chat and dashboard are completely isolated -- different React trees, different build outputs, different error boundaries.

### Widget (supervisor/widget.js)

Vanilla JS that injects:
- A floating video bubble (60px, bottom-right corner, with Safari fallback image)
- A slide-out panel (480px wide, full screen on mobile) containing the iframe at `/bloby/`
- A backdrop for dismissal
- Keyboard dismiss (Escape key)

Communicates with the iframe via `postMessage`:
- `bloby:close` -- iframe requests panel close
- `bloby:install-app` -- iframe requests PWA install prompt
- `bloby:show-ios-install` -- iframe requests iOS-specific install modal
- `bloby:onboard-complete` -- iframe notifies onboarding finished, reloads bubble
- `bloby:rebuilding` -- agent started modifying files, show rebuild overlay
- `bloby:rebuilt` -- rebuild complete, dashboard should reload
- `bloby:build-error` -- build failed, show error overlay
- `bloby:hmr-update` -- supervisor notifies dashboard of file changes

Panel state persisted in localStorage (`bloby_widget_open`) to survive HMR reloads. Bubble hidden during onboarding.

### Chat Protocol (WebSocket)

Client -> Server:
- `user:message` -- `{ content, conversationId?, attachments? }` where attachments are `{ type, name, mediaType, data(base64) }`
- `user:stop` -- abort current agent query
- `user:clear-context` -- clear conversation and agent session
- `whisper:transcribe` -- `{ audio: base64 }` (bypasses relay POST limitation)
- `settings:save` -- `{ ...settings }` (bypasses relay POST limitation)

Server -> Client:
- `bot:typing` -- agent started
- `bot:token` -- streamed text chunk
- `bot:tool` -- tool invocation (name, status)
- `bot:response` -- final complete response
- `bot:error` -- error message
- `bot:done` -- query complete, includes `usedFileTools` flag
- `chat:conversation-created` -- new conversation ID assigned
- `chat:sync` -- message from another connected client
- `chat:state` -- stream state on reconnect (catches up missed tokens)
- `chat:cleared` -- context was cleared
- `app:hmr-update` -- file changes detected, dashboard should reload

### WebSocket Client (ws-client.ts)

Auto-reconnects with exponential backoff (1s -> 8s cap). Queues messages during disconnection. Sends heartbeat pings every 25 seconds. Auth token passed as query parameter on connect.

### PWA Support

The chat SPA handles:
- `beforeinstallprompt` for Android PWA install
- iOS-specific install modal with step-by-step instructions
- Standalone mode detection
- Push notification subscription on first load

---

## Claude Agent SDK Integration (supervisor/bloby-agent.ts)

When the configured provider is Anthropic, chat messages are routed through the Claude Agent SDK instead of a raw API call.

- Runs with `permissionMode: bypassPermissions` -- full tool access, no confirmation prompts
- Working directory: `workspace/`
- Max 50 turns per query
- System prompt: Claude Code preset + custom addendum from `worker/prompts/bloby-system-prompt.txt`
- Sessions tracked in-memory (conversationId -> sessionId) for multi-turn context
- Memory files (MYSELF.md, MYHUMAN.md, MEMORY.md, PULSE.json, CRONS.json) injected into system prompt
- Skills auto-discovered from `workspace/skills/`
- MCP servers loaded from `workspace/MCP.json`
- File attachments encoded as base64 documents/images in the SDK prompt

The agent has access to all Claude Code tools (Read, Write, Edit, Bash, Grep, Glob, etc.). After a query completes, the supervisor checks if Write or Edit tools were used. If so, it restarts the backend and broadcasts an HMR update.

OAuth tokens are managed by `worker/claude-auth.ts` using PKCE flow against `claude.ai`. Tokens are stored in the macOS Keychain (primary) or `~/.claude/.credentials.json` (fallback). Refresh tokens are used to renew access tokens with a 5-minute expiry buffer.

For non-Anthropic providers (OpenAI, Ollama), the supervisor falls back to `ai.chat()` with simple message history -- no agent tools, no file access.

---

## Tunnel + Relay System

### Problem

The user's machine is behind NAT. We need a public URL so they can access their bot from their phone.

### Tunnel Modes

The user selects a tunnel mode during `morphy init`. The mode is stored in `~/.morphy/config.json` as `tunnel.mode`.

#### Quick Tunnel (default)

Zero configuration. No Cloudflare account needed.

```
Phone browser
  |
  | https://bruno.morphyagent.com          (via relay, optional)
  v
Morphy Relay (api.morphyagent.com)          Cloud server, maps username -> tunnel URL
  |
  | https://random-abc.trycloudflare.com
  v
Cloudflare Quick Tunnel              Ephemeral tunnel, changes on restart
  |
  | http://localhost:3000
  v
Supervisor                           User's machine
```

- Spawns: `cloudflared tunnel --url http://localhost:3000 --no-autoupdate`
- Extracts tunnel URL from stdout (regex match for `*.trycloudflare.com`)
- URL changes on every restart -- the relay provides the stable domain layer on top
- Optionally register with Morphy Relay for a permanent `open.morphyagent.com/username` or `morphyagent.com/username` URL

#### Named Tunnel

Persistent URL with the user's own domain. Requires a Cloudflare account + domain.

```
Phone browser
  |
  | https://bot.mydomain.com
  v
Cloudflare Named Tunnel              Persistent tunnel, URL never changes
  |
  | http://localhost:3000
  v
Supervisor                           User's machine
```

- Setup via `morphy tunnel setup` (interactive: login, create tunnel, generate config, print CNAME instructions)
- Spawns: `cloudflared tunnel --config <configPath> run <name>`
- URL is the user's domain (from config), no stdout parsing needed
- No relay needed -- the user's domain is already permanent
- Requires a DNS CNAME record pointing to `<uuid>.cfargotunnel.com`

### Cloudflare Tunnel Binary (supervisor/tunnel.ts)

- Auto-downloads `cloudflared` binary to `~/.morphy/bin/` on first run (validates minimum 10MB file size)
- Health check: HEAD request to tunnel URL with 5s timeout
- Watchdog runs every 30s, detects sleep/wake gaps (>60s between ticks), auto-restarts dead tunnels
- Quick tunnel restart: re-extracts new URL from stdout, updates relay if configured
- Named tunnel restart: restarts the process, URL doesn't change

### Relay Server (separate codebase, Quick Tunnel only)

Node.js/Express + http-proxy + MongoDB. Hosted on Railway. Only used when the user opts into Quick Tunnel mode and registers a handle.

**Registration flow:**
1. User picks a username during onboarding
2. Bot calls `POST /api/register` with username + tier
3. Relay stores tokenHash (SHA-256) in MongoDB, returns raw token + relay URL
4. Bot stores token in `~/.morphy/config.json`
5. Bot calls `PUT /api/tunnel` with its cloudflared URL
6. Relay marks bot online, starts accepting proxied traffic

**Request proxying:**
1. Request hits `bruno.morphyagent.com`
2. Subdomain middleware extracts username + tier from hostname
3. MongoDB lookup returns the bot's tunnel URL
4. `proxy.web(req, res, { target: tunnelUrl })` forwards everything -- headers, body, method
5. Response streams back to the user's browser

**Presence:**
- Bot sends `POST /api/heartbeat` every 30 seconds with its tunnel URL
- Relay considers a bot stale if no heartbeat in 120 seconds
- Stale bots get a 503 offline page (auto-refreshes every 15s)
- On graceful shutdown, bot calls `POST /api/disconnect`

**Domain tiers:**
| Tier | Subdomain | Path shortcut | Cost |
|---|---|---|---|
| Premium | `bruno.morphyagent.com` | `morphyagent.com/bruno` | $5/mo |
| Free | `bruno.open.morphyagent.com` | `open.morphyagent.com/bruno` | Free |

Same username can exist on both tiers independently. Compound unique index on `username + tier`.

**WebSocket proxying:**
The relay listens for HTTP upgrade events outside of Express middleware. This is critical -- Express middleware (body parsing, CORS) must not touch WebSocket upgrades. The upgrade handler parses the subdomain, looks up the bot, and calls `proxy.ws()`.

### Critical Constraint: POST Bodies Through the Relay

The relay's `express.json()` middleware must run AFTER the subdomain resolver, not before. If body parsing runs first, it consumes the request stream and `http-proxy` has nothing to forward. This was a real bug -- the fix was scoping `express.json()` to `/api` routes only, letting proxied traffic pass through with raw streams intact.

The Morphy chat has additional workarounds for this (sending settings and whisper data over WebSocket instead of POST), but with the relay fix these are no longer strictly necessary. They remain as defense-in-depth.

---

## Onboarding (supervisor/chat/OnboardWizard.tsx)

Multi-step wizard shown on first launch (inside the Morphy chat iframe):

1. **Welcome** -- intro screen
2. **Provider** -- choose Claude (Anthropic) or OpenAI Codex
3. **Model** -- select specific model (Opus, Sonnet, Haiku / GPT-5.x variants)
4. **Auth** -- OAuth PKCE flow for Claude or OpenAI, or manual API key entry
5. **Handle** -- register a public username with the relay (checks availability in real time)
6. **Portal** -- set username/password for remote access
7. **Whisper** -- optional voice transcription setup with OpenAI key
8. **Done** -- completion screen

Settings are saved via WebSocket (`settings:save` message) to bypass relay POST limitations.

---

## CLI (bin/cli.js)

The CLI is the user-facing entry point. Commands:

| Command | Description |
|---|---|
| `morphy init` | First-time setup: interactive tunnel mode chooser (Quick or Named), creates config, installs cloudflared, installs + starts the background service |
| `morphy start` | Start the background service (says so if already running; `--foreground` for debugging) |
| `morphy stop` | Stop the background service and any stray Morphy processes |
| `morphy restart` | Stop + start (identical to running both commands back to back) |
| `morphy status` | Health check via `/api/health` + supervisor pidfile: state, PID, uptime, URLs, log freshness, update check |
| `morphy logs` | Last 80 log lines with a provenance header (`-f` to follow, `-n <num>` for more) |
| `morphy update` | Downloads latest from npm registry, updates code directories, rebuilds UI, restarts only if it was running |
| `morphy tunnel` | Named tunnel management (subcommands below) |
| `morphy daemon` | Service management (launchd on macOS, systemd on Linux): install, start, stop, restart, status, logs, uninstall |
| `morphy help` | Full command list including advanced commands (`password-reset`, `x402`) |

Unknown or misspelled commands error out with a "did you mean" suggestion — they never silently start the bot. Every command runs Morphy as a background service and returns the terminal; the supervisor writes `~/.morphy/supervisor.json` (pid, startedAt, version, port) at boot, which the CLI uses as the single source of truth for status/logs.

**`morphy tunnel` subcommands:**
| Subcommand | Description |
|---|---|
| `morphy tunnel setup` | Interactive named tunnel setup: login to Cloudflare, create tunnel, enter domain, generate config YAML, print CNAME instructions |
| `morphy tunnel status` | Show current tunnel mode and configuration |
| `morphy tunnel reset` | Switch back to quick tunnel mode |

**`morphy init` tunnel chooser:**

During init, the user is presented with an interactive arrow-key menu to choose their tunnel mode:

- **Quick Tunnel** (Easy and Fast) -- Random CloudFlare tunnel URL on every start/update. Optionally use Morphy Relay for a permanent `open.morphyagent.com/username` handle (free) or a premium `morphyagent.com/username` handle ($5 one-time).
- **Named Tunnel** (Advanced) -- Persistent URL with your own domain. Requires a Cloudflare account + domain. Use a subdomain like `bot.yourdomain.com` or the root domain.

If Named Tunnel is selected, `morphy init` immediately runs the named tunnel setup flow inline (same as `morphy tunnel setup`).

The CLI spawns the supervisor via `node --import tsx/esm supervisor/index.ts` and waits for readiness markers on stdout (`__TUNNEL_URL__`, `__RELAY_URL__`, `__VITE_WARM__`, `__READY__`, `__TUNNEL_FAILED__`) with a 45-second timeout.

On Linux, `morphy daemon` generates a systemd unit file that runs the supervisor as a service with auto-restart on failure.

---

## Installation

Two installation paths:

**Via curl (production):**
```
curl -fsSL https://morphyagent.com/install | sh
```
The install script (`scripts/install.sh`) detects OS/arch, checks for Node.js >= 18 (or bundles Node 22.14.0), downloads the npm package, extracts to `~/.morphy/`, and adds `morphy` to PATH.

**Via npm (development):**
```
npm install morphy
```
The `postinstall` script (`scripts/postinstall.js`) copies code directories to `~/.morphy/`, runs `npm install --omit=dev` there, builds the chat UI if missing, and creates a `morphy` symlink.

Windows: `scripts/install.ps1` (PowerShell equivalent).

---

## Data Locations

| Path | Contents |
|---|---|
| `~/.morphy/config.json` | Port, username, AI provider, tunnel mode/config, relay token, tunnel URL |
| `~/.morphy/cloudflared-config.yml` | Named tunnel config (generated by `morphy tunnel setup`) |
| `~/.morphy/memory.db` | SQLite -- conversations, messages, settings, sessions, push subscriptions |
| `~/.morphy/bin/cloudflared` | Cloudflare tunnel binary |
| `~/.morphy/workspace/` | User's workspace copy (client, backend, memory files, skills, config) |
| `~/.codex/codedeck-auth.json` | OpenAI OAuth tokens |
| `~/.claude/.credentials.json` | Claude OAuth tokens (Linux/Windows) |
| macOS Keychain `Claude Code-credentials` | Claude OAuth tokens (macOS, source of truth) |

---

## File Map

### Sacred (never modified by the agent)

```
bin/cli.js                          CLI entry point, startup sequence, update logic, daemon management
supervisor/
  index.ts                          HTTP server, request routing, WebSocket handler, process orchestration
  backend.ts                        Backend process spawn/stop/restart
  tunnel.ts                         Cloudflare tunnel lifecycle (quick + named), health watchdog
  vite-dev.ts                       Vite dev server startup for dashboard HMR
  bloby-agent.ts                    Claude Agent SDK wrapper, session management, memory injection
  scheduler.ts                      PULSE + CRON scheduler, 60s tick, push notification dispatch
  file-saver.ts                     Attachment storage (audio, images, documents)
  widget.js                         Chat bubble + panel injected into dashboard
  chat/
    chat-main.tsx                  Chat SPA entry -- auth, WS connection, push subscription, PWA
    onboard-main.tsx                Onboard SPA entry
    OnboardWizard.tsx               Multi-step setup wizard
    ARCHITECTURE.md                 Network topology and relay workaround docs
    src/
      hooks/useChat.ts              Base chat state management
      hooks/useBlobyChat.ts         Morphy-specific chat: DB persistence, sync, pagination, streaming
      lib/ws-client.ts              WebSocket client with reconnect + queue
      lib/auth.ts                   Token storage and auth fetch wrapper
      components/Chat/
        ChatView.tsx                Main chat container
        InputBar.tsx                Text input, file/camera attachments, voice recording
        MessageBubble.tsx           Markdown rendering, syntax highlighting, attachments
        MessageList.tsx             Paginated message history with infinite scroll
        AudioBubble.tsx             Audio player for voice messages
        ImageLightbox.tsx           Image viewer modal
        TypingIndicator.tsx         "Bot is typing..." animation
      components/LoginScreen.tsx    Portal login UI
worker/
  index.ts                          Express API app -- all platform endpoints (runs in-process via createWorkerApp())
  db.ts                             SQLite schema, CRUD operations, migrations
  claude-auth.ts                    Claude OAuth PKCE flow, token refresh, Keychain integration
  codex-auth.ts                     OpenAI OAuth PKCE flow, local callback server on port 1455
  prompts/bloby-system-prompt.txt   System prompt that constrains the agent
shared/
  config.ts                         Load/save ~/.morphy/config.json
  paths.ts                          All path constants (PKG_DIR, DATA_DIR, WORKSPACE_DIR)
  relay.ts                          Relay API client (register, heartbeat, disconnect, tunnel update)
  ai.ts                             AI provider abstraction (Anthropic, OpenAI, Ollama) with streaming
  logger.ts                         Colored console logging with timestamps
scripts/
  install.sh                        User-facing install script (curl-piped), Node bundling
  install.ps1                       Windows PowerShell installer
  postinstall.js                    npm postinstall: copies files to ~/.morphy/, builds UI, creates symlink
```

### Workspace (agent-modifiable)

```
workspace/
  client/
    index.html                      Dashboard HTML shell, PWA manifest, widget script tag
    src/main.tsx                     React DOM entry
    src/App.tsx                      Dashboard root -- error boundary, rebuild overlay
    src/components/                  Dashboard UI components
  backend/
    index.ts                        Express server template with .env loading and SQLite
  .env                              Environment variables
  app.db                            Workspace SQLite database
  MYSELF.md                         Agent identity and personality
  MYHUMAN.md                        User profile (agent-maintained)
  MEMORY.md                         Long-term curated knowledge
  PULSE.json                        Periodic wake-up configuration
  CRONS.json                        Scheduled task definitions
  memory/                           Daily notes (YYYY-MM-DD.md)
  skills/                           Skill folders (SKILL.md with frontmatter)
  MCP.json                          MCP server configuration (optional)
  files/                            Uploaded file storage (audio/, images/, documents/)
```

---

## Key Design Decisions

**Why does the worker run in-process but the backend is a separate child process?**
The worker is trusted platform code (auth, database, API) -- it shares the same lifecycle as the supervisor. The backend runs user-editable workspace code that Claude can modify at any time. Keeping it as a separate process means a bug Claude introduces only crashes the backend, not the whole system. The supervisor and chat stay alive so the user can ask Claude to fix it. The backend also uses Node.js module hooks to enforce a sandbox boundary, preventing workspace code from importing packages outside `workspace/node_modules/`.

**Why serve the chat from pre-built static files instead of Vite?**
The chat must survive dashboard crashes. If Vite dies or the workspace frontend throws, the chat iframe loads from `dist-chat/` which is just static files. No build process, no dev server dependency.

**Why WebSocket for chat instead of HTTP streaming?**
The relay couldn't reliably forward POST bodies (now fixed). WebSocket was the workaround. It also gives us bidirectional real-time communication, multi-device sync, and heartbeat detection for free.

**Why bypassPermissions on the agent?**
The whole point is that the user talks to Claude from their phone and Claude does whatever's needed. Confirmation prompts would require a terminal session that doesn't exist. The workspace directory boundary + the system prompt are the safety rails.

**Why two tunnel modes?**
Quick Tunnel is the default for simplicity -- zero configuration, no Cloudflare account needed. The tradeoff is the URL changes on restart, which is why the relay exists as an optional stable domain layer. Named Tunnel is for advanced users who want full control -- their own domain, no dependency on the relay, permanent URLs. Both modes are offered during `morphy init`.

**Why two Vite configs?**
`vite.config.ts` builds the workspace dashboard (user-facing app). `vite.chat.config.ts` builds the Morphy chat SPA. They're separate apps with separate entry points, bundled independently. The chat is pre-built at publish time; the dashboard runs as a dev server with HMR.

**Why memory files instead of a database for agent memory?**
Files are the natural interface for the Claude Agent SDK -- it can read and write them with its built-in tools. No custom tool needed, no API integration. The agent manages its own memory with the same tools it uses to edit code.

---

## License

Morphy is licensed under the **Business Source License 1.1** (BSL 1.1).

- **Permitted:** self-hosted personal/internal use, modifications, redistribution, deploying Morphy for clients (consulting/integration), and replacing the `morphyagent.com` relay with your own private infrastructure.
- **Restricted:** offering Morphy as a hosted/managed/multi-tenant service that operates a relay or marketplace competing with `morphyagent.com`.
- **Change Date:** 2028-04-30 — on this date the license auto-converts to **Apache License 2.0**.

See [LICENSE](./LICENSE) for the full terms. For commercial licensing inquiries, contact `legal@morphyagent.com`.
