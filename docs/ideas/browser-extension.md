# Idea — a liquid browser extension

- **Status:** future improvement, deliberately NOT built yet
- **Context:** 2026-07-11, while landing the no-extension capture paths

## Why it doesn't exist yet

The screenshot-flow gap was closed without it: the shell's 📸 button
(`getDisplayMedia`, one frame → attachment) covers "send the agent what I
see" on desktop with zero install, the PWA `share_target` covers Android,
paste/drop covers everything else, and the agent's own screenshot tool
covers apps + the shell from its side. An extension is two more codebases
(MV3 for Chromium, WebExtensions for Firefox), signing/distribution or
unpacked installs, per-device setup, and no mobile presence — so it has to
earn its keep with things *only* an extension can do.

## What only an extension can do (the actual pitch)

The browser is where most of a person's digital life happens, and liquid
can't see any of it. An extension is liquid's sensor (and eventually hand)
inside that world:

1. **Debug capture, not just pixels.** One click attaches the visible tab
   *plus* the console log, failing network requests, and the URL. For "the
   app broke" reports this turns a screenshot the agent squints at into a
   stack trace it can fix. (This is the single strongest reason to build it —
   `getDisplayMedia` can never see the console.)
2. **"Make me one like this" — design capture.** On any site: capture a
   full-page screenshot + extracted computed styles (palette, fonts, spacing,
   layout skeleton) and send the bundle to liquid — "build my tracker with
   this look." The agent gets structured design DNA instead of guessing from
   a JPEG. The buildless app model is a perfect target for this.
3. **Research clipper.** Select text/images on any page → "send to liquid"
   with source URL and context. Lands in chat (or straight into workspace
   notes/memory via the agent): "file this under the kitchen-renovation
   project." liquid becomes the place where found things accumulate.
4. **Page-to-app extraction.** A recipe, a table, a schedule, a product list
   → extract the structured data and hand it to the agent to grow an app
   around ("turn this recipe into a card in my cooking app").
5. **Eyes on authenticated pages.** The agent can't fetch your logged-in
   dashboards (bank, utilities, school portal); your browser session can.
   Explicit per-click sharing of a page you're looking at gives the agent
   evidence it can never get itself — with the human as the capability, which
   is the right trust shape.
6. **Watch this page.** "Tell me when this changes / when tickets drop" —
   the extension re-visits (or captures on your visits) and diffs; liquid
   notifies via the existing push channel.
7. **Task assistance on arbitrary sites** (much later, needs real care):
   the agent suggests or performs actions in your session — form filling,
   repetitive flows. This is computer-use-with-your-cookies; the consent,
   scoping, and audit design is the hard part, not the mechanics.

## Sketch, when it's time

- MV3 (+ Firefox port), talks to the liquid API over HTTPS with a dedicated
  token minted in Settings (a scoped role, not the owner session — the
  extension holds it long-term on every browser profile).
- Capture path: `captureVisibleTab` + `chrome.debugger`/`devtools` APIs for
  console+network; POST to a new `/api/inbox` (attachment + metadata) that
  lands in chat like a pasted image with a context block.
- Clipper path: context-menu entries → same inbox with selection/DOM payload.
- Distribution: unpacked/dev-mode first (single-owner tool); stores only if
  it ever matters.

## Trigger to revisit

Build it when one of these actually bites: repeated "here's a screenshot,
what broke?" rounds that console logs would have short-circuited; a real
"copy this site's design" request; or the research-clipper itch showing up
in daily use.
