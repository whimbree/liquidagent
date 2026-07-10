# ADR 0003 — Full-surface apps and WebSocket passthrough

- **Status:** Accepted & implemented — 2026-07-10
- **Deciders:** Bree
- **Relates to:** ADR 0002 (polyglot backends — this is its second half)

## Context

ADR 0002 lets an app declare *how its server runs* (`backend.run`). Two
platform assumptions still blocked a real framework backend (the proving
case: a Phoenix/Elixir collaborative whiteboard):

1. **Apps were a static `index.html` + optional `/api` backend.** Phoenix
   serves its *own* HTML, assets, routes, and socket — the platform's static
   serving and iframe-panel model had nowhere to put it.
2. **The reverse proxy was HTTP-only.** Phoenix Channels/LiveView (and any
   realtime app) need a WebSocket upgrade to reach the backend.

## Decision

### 1. `surface: "full" | "panel"` in the manifest (default `"panel"`)

- **panel** (unchanged, the default): the supervisor serves the app's static
  files (GET/HEAD, traversal-safe) and proxies `/app/<id>/api/*` to the
  backend with the prefix stripped.
- **full**: the app owns its whole document. *Every* request under
  `/app/<id>/` — any path, any method, upgrades included — is proxied to the
  backend with only the `/app/<id>` prefix stripped (so the backend sees
  `/api/x` as `/api/x`: it owns its whole path space, nothing is re-rooted).
  No static serving; `index.html` is not required (the backend *is* the app,
  so `surface: "full"` without a `backend` is a manifest error). The shell
  frames it as-is — same iframe, same sandbox-if-public rule.

The literal union lives in `apps.rs` (`Surface`), parsed at the manifest edge
per ADR 0001; unknown values fail the manifest, they don't default silently.

Backends behind `full` should use **relative URLs** (or read
`LIQUID_APP_ID` and prefix explicitly): the browser sees the app at
`/app/<id>/`, the backend sees `/`-rooted paths.

### 2. WebSocket (HTTP/1.1 Upgrade) passthrough in the proxy

The proxy (`backends.rs`) now forwards upgrade handshakes: request headers go
to the backend as-sent; if the backend answers `101 Switching Protocols`,
that response (Sec-WebSocket-Accept & co.) is mirrored to the client and the
two upgraded byte streams are spliced (`copy_bidirectional`) for the life of
the socket. A non-101 answer passes through untouched. This applies to both
proxy paths — `/app/<id>/api/*` for panel apps and the whole namespace for
full-surface apps — and is protocol-agnostic (anything Upgrade-shaped rides
it, not just WebSockets).

Access control is unchanged and runs *before* the handshake is forwarded: a
socket to a private app without the owner cookie (or screenshot capability)
is a 401, never a connection.

## Consequences

**Good:** framework backends (Phoenix, and later anything with its own
router/socket) are first-class apps; realtime panel apps get sockets behind
`/api` with zero manifest changes; the anti-forgery pipeline still governs
everything (full-surface apps deploy from the served worktree like any other).

**Costs / caveats:** a full-surface app's iframe loses the shell's crash
watcher (the app owns its document — its failures are its own to render); the
supervisor keeps one spliced TCP pair per live socket (fine at personal
scale); hop-by-hop header handling is deliberately minimal (Connection/
Upgrade forwarded as-sent) — sufficient for the 1:1 localhost hop it serves.

## Guards

`e2e/smoke.ts` ("ws proxy + full surface"): a full-surface app with no
`index.html` serving its own HTML/assets/POST routes with `/api` preserved; a
WS echo through the panel `/api` proxy; a WS echo through the full-surface
root proxy; a WS to a private app refused unauthenticated. The whiteboard
guard (`e2e/whiteboard.ts`, ADR 0002's proving case) exercises the whole
stack against a real Phoenix backend.
