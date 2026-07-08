# ADR 0001 — Effect, Schema, and typing discipline

- **Status:** Accepted — 2026-07-07
- **Deciders:** Bree
- **Scope:** liquid's TypeScript surfaces. The Rust supervisor is out of scope (it
  has its own conventions: no `unwrap`, named constants, `anyhow::Context`).

## Context

liquid has three distinct TS surfaces, and today they share almost no discipline:

| Surface | Today | Size |
|---|---|---|
| Harness / platform I/O (`workspace/agent/`) | Bun+TS, Zod at the IPC edge, hand-rolled respawn/backoff, `throw`-based errors | small (~200 core lines) |
| Shell (`static/shell.html`) | **untyped** inline JS in a `<script>` tag, no build step | large (~2,000 lines) |
| Per-app backends | agent-*generated* Bun servers; "buildless apps first" | open-ended |

Recurring pain: wire-protocol values (`"token"`/`"done"`/`"tool"`, roles, setting
keys, pipeline states, model aliases) are **raw strings duplicated across Rust and
TS**, and the shell — our largest and most bug-prone TS body — has no compiler
watching it. Several bugs this cycle (field-clearing, the streaming refactor)
would have been caught by types.

The constraint that shapes everything: **"software is grown, not installed."**
Discipline must not ossify the buildless, dependency-light nature of *grown* apps.

## Decision

Adopt quality discipline **by tier**, not as a blanket. The governing line:

> Effect and the build step govern **platform code we write**. They never touch
> **software the agent grows**.

| Tier | Effect? | Types? | Build? |
|---|---|---|---|
| Harness / platform I/O | **Yes** (pilot first) | strict + Schema | yes |
| Shell | **No** (Effect is ceremony for DOM glue) | strict TS, discriminated/branded | **yes** (new) |
| Generated apps | **No** | encouraged, never required | **No** — stays buildless |

### 1. Effect + Schema in the harness (pilot)

The harness is effectful and concurrent — spawning the Agent SDK, streaming,
IPC, the single-query invariant, interrupt/stop, resource cleanup. That is
Effect's sweet spot: typed errors (`Data.TaggedError`), `Scope` resource safety,
structured concurrency. And **`effect/Schema` replaces Zod** at the IPC boundary —
one library, encode *and* decode, instead of Zod-plus-hand-rolled.

```ts
class HarnessSpawnError extends Data.TaggedError("HarnessSpawnError")<{
  readonly cause: unknown;
}> {}
// Effect<AgentEvent, HarnessSpawnError | QueryError, ClaudeSdk>
```

Honest cost: Effect is a paradigm, not a sprinkle — steep, invasive learning
curve. The harness is small, so ROI is "moderate now, compounding as the platform
grows." Therefore **pilot, live with it, then decide on wider adoption** — do not
declare total adoption up front.

### 2. No stringly-typed domain values — literal/tagged unions, **not `enum`**

Every closed set is modeled as a type, defined once per side:

```ts
type Role = "user" | "assistant" | "scheduled" | "error";
const AgentEventKind = Schema.Literal("token", "tool", "done", "error", "session");
```

We deliberately **do not use TS `enum`**:
- `enum` emits runtime code and **isn't erasable** — clashes with Bun/Node
  type-stripping and `verbatimModuleSyntax`; `const enum` breaks `isolatedModules`.
- numeric enums permit unsafe assignment; enums are nominal in a structurally
  typed language, which surprises.
- literal unions / tagged unions / `Schema.Literal` give exhaustiveness *and*
  (via Schema) runtime validation — which `enum` never does. It is also what
  Effect itself uses.

**This is a rule about domain values, not all strings.** User copy, log messages,
and CSS class names stay strings; enum-ifying them is noise people route around.

### 3. Branded IDs

Ids are branded so they can't be crossed:

```ts
type AppId = string & Brand.Brand<"AppId">;
type ConversationId = number & Brand.Brand<"ConversationId">;
```

The other half of "stop being stringly-typed" — a `ConversationId` can never be
passed where an `AppId` is expected.

### 4. Baseline discipline (all platform TS)

- **tsconfig maximal-strict**: `strict`, no `any`, plus `noUncheckedIndexedAccess`,
  `exactOptionalPropertyTypes`, `noImplicitOverride`, `verbatimModuleSyntax`.
- **Exhaustiveness**: discriminated unions + an `assertNever(x: never)` default, so
  adding a variant is a compile error until handled.
- **Errors as values at boundaries** (Effect typed errors in the harness) over `throw`.
- **Validate every I/O edge** with Schema — nothing untyped crosses a process or
  network line.
- **One constants module per side** for the closed sets; delete scattered literals.

### 5. The build-step boundary (the one real architectural change)

Typing the shell means moving its inline JS into a `.ts` module and
bundling+inlining it (tsc cannot check `<script>` inside `.html`). That is a build
step **for platform code** — defensible *because* the rule is: apps stay buildless,
platform gets a build. The supervisor can inline the bundle at build time so the
shell is still served as a single asset.

## Cross-language reality

Wire protocols (`AgentEvent`, `ServerEvent`, IPC) live in **both** Rust and TS. A
single cross-language source of truth is heavy (codegen). The realistic discipline:
one canonical definition per side, kept honest by a **boundary parity test**
(extend the existing IPC test), not a shared generator — until the churn justifies one.

## Consequences

**Good:** the compiler guards our largest codebase (the shell); wire drift between
Rust and TS becomes a test failure; the harness's error paths and resource cleanup
become type-enforced; one validation library (Schema) instead of two patterns.

**Costs:** Effect's learning curve; a build step for the shell (mitigated: apps
unaffected); some up-front churn moving to branded ids and central constants.

## Sequencing

1. **Pilot** Effect + Schema in the harness; replace Zod. Keep `fake-harness` and
   the IPC/prompt tests green.
2. **Shell → strict TS + build**: extract inline JS to `.ts`, add the strict
   tsconfig, introduce literal/tagged/branded types and `assertNever`. No Effect.
3. **Codify** the rules (2–4 above) as a "Code standards" section in `CLAUDE.md`.
4. **Re-evaluate** wider Effect adoption after living with the pilot.

## Resolutions

- **Agent writing Effect platform code: yes, but just-in-time.** The agent may write
  Effect'd platform code when a task requires it; we add an Effect section to
  `prompt.ts` only *when that first happens*, not preemptively (prompt-token cost on
  every query otherwise). Until then humans drive the platform pilot and the agent's
  world stays buildless.
- **Shell reactivity: vanilla DOM under strict TS.** We type the shell but do **not**
  introduce a signals/reactivity library — a separate, larger concern. Revisit only if
  imperative state management becomes the bottleneck.
- **Rust↔TS parity: a boundary parity test now**; codegen only if the protocols churn.
