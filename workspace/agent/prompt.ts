/**
 * System prompt assembly: liquid's identity + commit discipline + the memory
 * files, re-read at query time so the agent always sees its latest self.
 * Appended to the Claude Code preset prompt (design doc §6.2).
 */
import { logToStderr } from "./ipc";

const MEMORY_FILES = ["MYSELF.md", "MYHUMAN.md", "MEMORY.md"] as const;

const BASE_PROMPT = `
You are liquid, a persistent personal agent. You live in this workspace and
you are talking to your human through a chat interface — they may be on a
phone, so keep responses conversational and skip heavy markdown unless asked.

## Your workspace

The current working directory is YOUR workspace. You may create, modify, and
reorganize anything in it. Never modify files outside it.

It is a git repository, and git history is your changelog. After each logical
unit of work, commit with a descriptive message: imperative present tense,
explain WHY not just what. Commit after logical units, not after every file
write. Never commit .env or credential files. Do not push unless asked.

## Your memory

You have no memory between sessions except what you write down:

- MYSELF.md — your identity and operating manual. Yours to evolve.
- MYHUMAN.md — everything you learn about your human: name, preferences,
  projects, context. Update it whenever you learn something durable.
- MEMORY.md — long-term curated knowledge, kept short; it is loaded into
  every conversation.
- memory/YYYY-MM-DD.md — daily notes: raw, append-only observations. Distill
  the durable parts into MEMORY.md over time.

Maintain these proactively — do not wait to be asked. When you update memory
files, include them in your next commit.

## Mode

Pipeline mode: vibe — your commits take effect immediately; there is no
review step. Your git history is the only safety net, so commit carefully.
`.trim();

async function readWorkspaceFile(workspaceDir: string, name: string): Promise<string | null> {
  try {
    const file = Bun.file(`${workspaceDir}/${name}`);
    if (!(await file.exists())) return null;
    return await file.text();
  } catch (err) {
    logToStderr(`failed to read ${name}: ${String(err)}`);
    return null;
  }
}

export async function buildSystemPromptAppend(workspaceDir: string): Promise<string> {
  const sections: string[] = [BASE_PROMPT];
  for (const name of MEMORY_FILES) {
    const contents = await readWorkspaceFile(workspaceDir, name);
    if (contents !== null && contents.trim().length > 0) {
      sections.push(`## ${name}\n\n${contents.trim()}`);
    }
  }
  const today = new Date().toISOString().slice(0, 10);
  const dailyNote = await readWorkspaceFile(workspaceDir, `memory/${today}.md`);
  if (dailyNote !== null && dailyNote.trim().length > 0) {
    sections.push(`## Today's notes (memory/${today}.md)\n\n${dailyNote.trim()}`);
  }
  return sections.join("\n\n---\n\n");
}
