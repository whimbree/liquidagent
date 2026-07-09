/**
 * Canonical wire-protocol vocabulary for the liquid IPC boundary.
 *
 * These literal tuples are the TypeScript source of truth for the closed sets
 * that cross the harness↔supervisor line. They MUST stay in sync with the Rust
 * side (`src/agent.rs`, `#[serde(tag = "type", rename_all = "snake_case")]`).
 * `protocol.test.ts` pins these against `protocol.json`, and a Rust test pins
 * the Rust enums against the same file — so drift on either side fails a test.
 *
 * Rule (ADR 0001): domain values are literal unions, never TS `enum`.
 */

export const AGENT_EVENT_TYPES = ["token", "tool", "done", "error", "session", "shell", "notify", "image"] as const;
export const AGENT_REQUEST_TYPES = ["query", "stop"] as const;
export const TOOL_STATUSES = ["start", "done"] as const;
export const SHELL_ACTIONS = ["open_app"] as const;

export type AgentEventType = (typeof AGENT_EVENT_TYPES)[number];
export type AgentRequestType = (typeof AGENT_REQUEST_TYPES)[number];
export type ToolStatus = (typeof TOOL_STATUSES)[number];
export type ShellAction = (typeof SHELL_ACTIONS)[number];

/** Branded ids: a ConversationId can't be silently mixed with other strings. */
declare const __brand: unique symbol;
export type Brand<T, B extends string> = T & { readonly [__brand]: B };
export type ConversationId = Brand<string, "ConversationId">;
export const conversationId = (s: string): ConversationId => s as ConversationId;

/**
 * Exhaustiveness guard: `default: return assertNever(x)` in a switch over a
 * closed union fails to compile the day a variant is added without a case.
 */
export function assertNever(x: never): never {
  throw new Error(`unhandled variant: ${JSON.stringify(x)}`);
}
