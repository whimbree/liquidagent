# ADR 0004 — The built-in app library (install any time, git-native updates)

- **Status:** Accepted & implemented — 2026-07-10
- **Deciders:** Bree
- **Relates to:** ADR 0002/0003 (the whiteboard is the flagship library app),
  ROADMAP M10 (app templates / import)

## Context

`default-workspace/` was a first-boot-only template: whatever apps it carried
were seeded once, and existing installs could never receive new built-ins
(the whiteboard shipped and nobody's home screen changed). The vision: the
apps that ship with liquid are a **library** you can pick from at any time —
and because installed copies are *grown* afterwards (the agent evolves them),
"update" must respect local evolution, not clobber it.

## Decision

1. **Everything under `default-workspace/apps/` is the library.** build.rs
   stages it (filtering `_build/`, `data/`, `node_modules/`, `.git`) and
   `include_dir!` embeds it in the binary — installs work offline, upgrades of
   the platform upgrade the library. First boot still seeds a starter app
   (StrongLifts) through the same extraction path; nothing else auto-installs.
2. **Install = copy + commit = ownership transfer.** `POST
   /api/catalog/{id}/install` extracts into the workspace and makes a
   pathspec-scoped commit. From then on the app is the owner's; the library
   copy never auto-updates it.
3. **Versioning is git + a content-addressed marker — no semver.** Each
   install/update commit includes `.library.json` holding the sha256 of the
   library content installed. "Update available" is an exact hash comparison;
   the app's own history is its changelog (already first-class in the shell).
4. **Update is a real 3-way merge.** Base = the last marker commit (fallback:
   the app's creation commit, which was pristine library content), ours = the
   workspace copy, theirs = the new library content committed on a temporary
   worktree branch. Clean merges commit and deploy; **conflicts stop before
   any commit** and wait in the working tree — the agent resolves them today,
   the in-shell IDE (M8) will offer a human surface later. `replace` is the
   escape hatch: pristine library copy in a new commit, history keeps the fork.
5. **Library commits are pre-approved.** The bytes are platform-shipped and a
   human clicked, so install/update deploys directly instead of riding the
   agent review gate — guarded by refusing whenever *anything* undeployed is
   pending, so a direct deploy can never smuggle unreviewed agent commits out.

## Consequences

**Good:** existing installs adopt new built-ins with one click; updates
respect local evolution; every state transition is a commit (auditable,
revertable); the whiteboard e2e now exercises the real install flow.

**Costs:** the binary carries the library (~4 MB with the whiteboard's
vendored deps); a conflicted merge leaves the workspace mid-merge until
resolved (deliberate — that's the resolve surface; the pipeline serves only
commits, so nothing half-merged deploys); pre-marker installs have an
approximate merge base (their creation commit).

## Guards

`catalog.rs` unit tests (embedded content hygiene; merge keeps both sides;
conflicts stop uncommitted), `e2e/smoke.ts` (list state, install/update
guards, diverge → replace → redeploy), `e2e/shell-smoke.ts` (library UI,
live icon pop-in on install), `e2e/whiteboard.ts` (installs via the endpoint,
then the full Phoenix/guest flow).
