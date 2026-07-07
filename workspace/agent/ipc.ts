import { z } from "zod";

/**
 * The IPC protocol between the Rust supervisor and the agent harness.
 * Requests arrive on stdin, events leave on stdout — one JSON object per line.
 * Zod-validated at this boundary per the project code standards.
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

export type AgentRequest = z.infer<typeof AgentRequestSchema>;

export type AgentEvent =
  | { type: "token"; id: string; text: string }
  | { type: "tool"; id: string; name: string; status: "start" | "done" }
  | { type: "done"; id: string; used_file_tools: boolean }
  | { type: "error"; id: string; message: string }
  | { type: "session"; id: string; session_id: string }
  | { type: "shell"; id: string; action: "open_app"; app: string }
  | { type: "notify"; id: string; title: string; body: string };

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
    yield parsed.data;
  }
}
