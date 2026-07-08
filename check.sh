#!/usr/bin/env bash
# The full quality gate: Rust + harness tests, both typechecks, and the API +
# shell end-to-end smokes. Run inside the dev shell:
#
#   nix develop --command bash check.sh
#
# Everything here is offline/deterministic (fake harness, no credentials).
set -euo pipefail
cd "$(dirname "$0")"

step() { printf '\n=== %s ===\n' "$1"; }

step "cargo test (Rust supervisor)"
cargo test --quiet

step "bun test (harness IPC/prompt/protocol parity)"
( cd workspace/agent && bun test )

step "typecheck: harness"
( cd workspace/agent && bun run typecheck )

step "typecheck: shell (checkJs)"
( cd workspace/agent && bun run typecheck:shell )

step "e2e: API smoke"
bun run e2e/smoke.ts

step "e2e: shell smoke (headless chromium)"
bun run e2e/shell-smoke.ts

step "e2e: StrongLifts app + backend (offline-first + SQLite)"
bun run e2e/stronglifts.ts

printf '\n✅ ALL CHECKS PASSED\n'
