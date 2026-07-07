/**
 * One-shot code reviewer. Reads a git diff on stdin, runs a single Claude
 * Agent SDK query with a narrow reviewer prompt, and prints a verdict as JSON
 * on the last stdout line: {"verdict":"APPROVED"|"REJECTED","reasoning":"..."}.
 *
 * Read-only: the reviewer never edits or builds. It only judges the diff the
 * supervisor computed.
 */
import { query } from "@anthropic-ai/claude-agent-sdk";

const REVIEW_SYSTEM = `
You are a code reviewer for a personal software factory. You are given a git
diff of a change the agent wants to deploy to its human's home screen.

Judge only these:
- Does the change look coherent and match its apparent intent?
- Any obvious bugs, broken syntax, or logic errors?
- Anything unsafe: credential exposure, obvious injection, requests to
  external hosts that don't fit the app's stated purpose?

Be pragmatic — this is a personal tool, not production infrastructure. Approve
reasonable work; reject only for real problems (broken code, security issues,
clearly incomplete changes). Do not demand tests or nitpick style.

Respond with EXACTLY one line of JSON and nothing else:
{"verdict":"APPROVED","reasoning":"one or two sentences"}
or
{"verdict":"REJECTED","reasoning":"what's wrong, specifically"}
`.trim();

function resolveClaudeBinary(): string {
  const found = Bun.env.LIQUID_CLAUDE_BIN ?? Bun.which("claude");
  if (found === null) {
    console.error("[review] no claude binary found");
    process.exit(1);
  }
  return found;
}

const diff = await Bun.stdin.text();
if (diff.trim().length === 0) {
  console.log(JSON.stringify({ verdict: "APPROVED", reasoning: "empty diff" }));
  process.exit(0);
}

let text = "";
try {
  for await (const message of query({
    prompt: `Review this diff:\n\n\`\`\`diff\n${diff}\n\`\`\``,
    options: {
      pathToClaudeCodeExecutable: resolveClaudeBinary(),
      systemPrompt: REVIEW_SYSTEM,
      permissionMode: "default",
      allowedTools: [], // read-only: the reviewer judges, never acts
      maxTurns: 1,
    },
  })) {
    if (message.type === "assistant") {
      for (const block of message.message.content) {
        if (block.type === "text") text += block.text;
      }
    }
  }
} catch (err) {
  console.log(JSON.stringify({ verdict: "REJECTED", reasoning: `reviewer error: ${String(err)}` }));
  process.exit(0);
}

// Extract the JSON verdict from the model's reply.
const match = text.match(/\{[^{}]*"verdict"[^{}]*\}/);
if (match) {
  console.log(match[0]);
} else {
  const approved = /\bAPPROVED\b/i.test(text) && !/\bREJECTED\b/i.test(text);
  console.log(JSON.stringify({
    verdict: approved ? "APPROVED" : "REJECTED",
    reasoning: text.slice(0, 300) || "no verdict produced",
  }));
}
