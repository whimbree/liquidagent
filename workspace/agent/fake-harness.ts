/**
 * Fake agent harness — no SDK, no credentials, no network.
 * Speaks the same IPC protocol as harness.ts so the supervisor's spawn/IPC/WS
 * path (spike 3) can be exercised end to end offline:
 *
 *   LIQUID_FAKE_AGENT=1 cargo run
 */
import { emit, logToStderr, readRequests } from "./ipc";

const TOKEN_DELAY_MS = 25;
// A 1x1 PNG — lets offline tests exercise the agent→chat image path.
const FAKE_PNG = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

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
  emit({ type: "session", id: request.id, session_id: `fake-session-${request.id}` });
  emit({ type: "tool", id: request.id, name: "FakeTool", status: "start" });
  await Bun.sleep(TOKEN_DELAY_MS * 4);
  emit({ type: "tool", id: request.id, name: "FakeTool", status: "done" });
  // Simulate the screenshot tool with REALISTIC ordering: the model streams some
  // preamble text, THEN the tool fires mid-turn, then the reply continues. This
  // interleaving is what the split-stream handling (supervisor buffer flush +
  // shell stream reset) exists for — keep it, or the tests go blind to it.
  const shot = /screenshot|show me/i.test(request.prompt);
  if (shot) {
    for (const word of "Here is the screenshot you asked for:".split(" ")) {
      emit({ type: "token", id: request.id, text: `${word} ` });
      await Bun.sleep(TOKEN_DELAY_MS);
    }
    emit({ type: "image", id: request.id, mime: "image/png", data: FAKE_PNG });
  }
  for (const word of words) {
    emit({ type: "token", id: request.id, text: `${word} ` });
    await Bun.sleep(TOKEN_DELAY_MS);
  }
  // Claim file-tool use so the supervisor's app rescan path gets exercised.
  emit({ type: "done", id: request.id, used_file_tools: true });
}
