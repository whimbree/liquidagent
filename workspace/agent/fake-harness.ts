/**
 * Fake agent harness — no SDK, no credentials, no network.
 * Speaks the same IPC protocol as harness.ts so the supervisor's spawn/IPC/WS
 * path (spike 3) can be exercised end to end offline:
 *
 *   LIQUID_FAKE_AGENT=1 cargo run
 */
import { emit, logToStderr, readRequests } from "./ipc";

const TOKEN_DELAY_MS = 25;

function fakeResponse(prompt: string): string {
  return (
    `You said: "${prompt}". I'm the fake harness — no model behind me, ` +
    `just the IPC pipeline working end to end. Swap me out by unsetting ` +
    `LIQUID_FAKE_AGENT once Claude credentials are set up.`
  );
}

logToStderr("fake harness ready");

for await (const request of readRequests()) {
  if (request.type === "stop") {
    logToStderr(`stop requested for query ${request.id} (no-op in fake harness)`);
    continue;
  }
  const words = fakeResponse(request.prompt).split(" ");
  emit({ type: "tool", id: request.id, name: "FakeTool", status: "start" });
  await Bun.sleep(TOKEN_DELAY_MS * 4);
  emit({ type: "tool", id: request.id, name: "FakeTool", status: "done" });
  for (const word of words) {
    emit({ type: "token", id: request.id, text: `${word} ` });
    await Bun.sleep(TOKEN_DELAY_MS);
  }
  emit({ type: "done", id: request.id, used_file_tools: false });
}
