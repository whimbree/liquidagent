import { z } from "zod";
import {
  conversationId,
  type AgentEventType,
  type ConversationId,
  type ShellAction,
  type ToolStatus,
} from "./protocol";

/**
 * The IPC protocol between the Rust supervisor and the agent harness.
 * Requests arrive on stdin, events leave on stdout — one JSON object per line.
 * Zod-validated at this boundary per the project code standards.
 *
 * The closed sets (event/request types, tool status, shell action) come from
 * protocol.ts and are parity-checked against the Rust side; ids are branded.
 */

export const AgentRequestSchema = z.discriminatedUnion("type", [
  z.object({
    type: z.literal("query"),
    id: z.string(),
    prompt: z.string(),
    session_id: z.string().optional(),
    model: z.string().optional(),
  }),
  z.object({ type: z.literal("stop"), id: z.string() }),
]);

/**
 * The validated request. Zod checks the shape; the `id` brand is a compile-time
 * refinement applied at the read boundary (see readRequests).
 */
export type AgentRequest =
  | { type: "query"; id: ConversationId; prompt: string; session_id?: string; model?: string }
  | { type: "stop"; id: ConversationId };

export type AgentEvent =
  | { type: "token"; id: ConversationId; text: string }
  | { type: "tool"; id: ConversationId; name: string; status: ToolStatus }
  | { type: "done"; id: ConversationId; used_file_tools: boolean }
  | { type: "error"; id: ConversationId; message: string }
  | { type: "session"; id: ConversationId; session_id: string }
  | { type: "shell"; id: ConversationId; action: ShellAction; app: string }
  | { type: "notify"; id: ConversationId; title: string; body: string };

// Compile-time check: every event's discriminant is a known wire type.
type _EventsAreWireTypes = AgentEvent["type"] extends AgentEventType ? true : never;
const _eventsAreWireTypes: _EventsAreWireTypes = true;

/** stdout is the IPC channel — events only. Human-readable logging goes to stderr. */
export function emit(event: AgentEvent): void {
  console.log(JSON.stringify(event));
}

export function logToStderr(message: string): void {
  console.error(`[harness] ${message}`);
}

/** Read stdin line by line, parsing and validating each request. */
export async function* readRequests(): AsyncGenerator<AgentRequest> {
  for await (const line of console) {
    const trimmed = line.trim();
    if (trimmed.length === 0) continue;
    let raw: unknown;
    try {
      raw = JSON.parse(trimmed);
    } catch {
      logToStderr(`ignoring non-JSON input line: ${trimmed}`);
      continue;
    }
    const parsed = AgentRequestSchema.safeParse(raw);
    if (!parsed.success) {
      logToStderr(`ignoring invalid request: ${parsed.error.message}`);
      continue;
    }
    // Brand the conversation id at the boundary.
    yield { ...parsed.data, id: conversationId(parsed.data.id) } as AgentRequest;
  }
}
