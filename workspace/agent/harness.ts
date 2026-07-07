/**
 * liquid agent harness — wraps the Claude Agent SDK behind the line-based
 * IPC protocol the Rust supervisor speaks (see ipc.ts).
 *
 * Spike 1 (SDK-on-Bun) standalone mode:
 *   bun run harness.ts --once "say hello and name one file in your workspace"
 *
 * Supervised mode (spawned by the supervisor): requests on stdin, events on
 * stdout. Auth comes from Claude Code's existing credentials
 * (~/.claude/.credentials.json / ANTHROPIC_API_KEY) — the SDK resolves them.
 */
import {
  createSdkMcpServer,
  query,
  tool,
  type Query,
  type SDKMessage,
} from "@anthropic-ai/claude-agent-sdk";
import { z } from "zod";
import { emit, logToStderr, readRequests, type AgentEvent } from "./ipc";
import { buildSystemPromptAppend } from "./prompt";

const MAX_TURNS_PER_QUERY = 50;
const FILE_TOOL_NAMES = new Set(["Write", "Edit", "NotebookEdit"]);

const workspaceDir = Bun.env.LIQUID_WORKSPACE_DIR ?? "./dev-workspace";

/**
 * The SDK's bundled CLI is a dynamically linked generic-Linux binary, which
 * NixOS cannot run. Use a real `claude` from PATH (nix-provided) instead,
 * overridable via LIQUID_CLAUDE_BIN.
 */
function resolveClaudeBinary(): string {
  const found = Bun.env.LIQUID_CLAUDE_BIN ?? Bun.which("claude");
  if (found === null) {
    logToStderr("no `claude` binary found on PATH and LIQUID_CLAUDE_BIN unset");
    logToStderr("install Claude Code (e.g. via nix) or set LIQUID_CLAUDE_BIN");
    process.exit(1);
  }
  return found;
}
const claudeBinary = resolveClaudeBinary();

let activeQuery: Query | null = null;
/** Conversation id of the running query — shell tool events carry it. */
let currentQueryId = "0";

/**
 * In-process MCP server giving the agent control of the shell. Tool calls
 * become IPC events; the supervisor fans them out to connected shells.
 */
const shellServer = createSdkMcpServer({
  name: "liquid-shell",
  version: "1.0.0",
  tools: [
    tool(
      "open_app",
      "Open one of your apps on your human's screen. Use it after building or " +
        "updating an app so they see it immediately, or when they ask you to open one.",
      { app: z.string().describe("The app id — its directory name under apps/") },
      async ({ app }) => {
        emit({ type: "shell", id: currentQueryId, action: "open_app", app });
        return { content: [{ type: "text", text: `${app} is now open on their screen.` }] };
      },
    ),
  ],
});

async function startQuery(prompt: string, resumeSessionId?: string): Promise<Query> {
  // Rebuilt every query so memory edits take effect immediately.
  const memoryAppend = await buildSystemPromptAppend(workspaceDir);
  return query({
    prompt,
    options: {
      cwd: workspaceDir,
      pathToClaudeCodeExecutable: claudeBinary,
      systemPrompt: { type: "preset", preset: "claude_code", append: memoryAppend },
      permissionMode: "bypassPermissions",
      allowDangerouslySkipPermissions: true,
      maxTurns: MAX_TURNS_PER_QUERY,
      includePartialMessages: true,
      mcpServers: { "liquid-shell": shellServer },
      ...(resumeSessionId !== undefined ? { resume: resumeSessionId } : {}),
    },
  });
}

/** Translate one SDK message into zero or more IPC events. */
function toEvents(requestId: string, message: SDKMessage, sawFileTool: { value: boolean }): AgentEvent[] {
  switch (message.type) {
    case "system":
      if (message.subtype === "init") {
        logToStderr(`session ${message.session_id} (model ${message.model})`);
        return [{ type: "session", id: requestId, session_id: message.session_id }];
      }
      return [];
    case "stream_event": {
      // Only stream top-level assistant text; subagent streams have a parent.
      if (message.parent_tool_use_id !== null) return [];
      const event = message.event;
      if (event.type === "content_block_delta" && event.delta.type === "text_delta") {
        return [{ type: "token", id: requestId, text: event.delta.text }];
      }
      return [];
    }
    case "assistant": {
      const events: AgentEvent[] = [];
      for (const block of message.message.content) {
        if (block.type === "tool_use") {
          if (FILE_TOOL_NAMES.has(block.name)) sawFileTool.value = true;
          events.push({ type: "tool", id: requestId, name: block.name, status: "start" });
        }
      }
      return events;
    }
    case "result":
      if (message.subtype === "success") {
        return [{ type: "done", id: requestId, used_file_tools: sawFileTool.value }];
      }
      return [
        { type: "error", id: requestId, message: `query failed: ${message.subtype}` },
        { type: "done", id: requestId, used_file_tools: sawFileTool.value },
      ];
    default:
      return [];
  }
}

async function runQuery(requestId: string, prompt: string, sessionId?: string): Promise<void> {
  const sawFileTool = { value: false };
  currentQueryId = requestId;
  const q = await startQuery(prompt, sessionId);
  activeQuery = q;
  try {
    for await (const message of q) {
      for (const event of toEvents(requestId, message, sawFileTool)) {
        emit(event);
      }
    }
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    emit({ type: "error", id: requestId, message });
    emit({ type: "done", id: requestId, used_file_tools: sawFileTool.value });
  } finally {
    activeQuery = null;
  }
}

// --- Spike 1 standalone mode -------------------------------------------------
if (Bun.argv[2] === "--once") {
  const prompt = Bun.argv[3];
  if (prompt === undefined) {
    logToStderr('usage: bun run harness.ts --once "<prompt>"');
    process.exit(2);
  }
  const sawFileTool = { value: false };
  for await (const message of await startQuery(prompt)) {
    for (const event of toEvents("once", message, sawFileTool)) {
      if (event.type === "token") process.stdout.write(event.text);
      if (event.type === "tool") logToStderr(`tool: ${event.name}`);
      if (event.type === "error") logToStderr(`error: ${event.message}`);
    }
  }
  process.stdout.write("\n");
  process.exit(0);
}

// --- Supervised mode ----------------------------------------------------------
async function main(): Promise<void> {
  logToStderr(`harness ready (workspace: ${workspaceDir})`);

  for await (const request of readRequests()) {
    switch (request.type) {
      case "query":
        // Phase 0: one query at a time; requests queue in the supervisor channel.
        await runQuery(request.id, request.prompt, request.session_id);
        break;
      case "stop": {
        const q = activeQuery;
        if (q !== null) {
          logToStderr(`interrupting query ${request.id}`);
          await q.interrupt().catch((err: unknown) => {
            logToStderr(`interrupt failed: ${String(err)}`);
          });
        }
        break;
      }
    }
  }
}

await main();
