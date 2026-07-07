import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { buildSystemPromptAppend } from "./prompt";

const workspace = join(tmpdir(), `liquid-prompt-test-${process.pid}`);

beforeAll(() => {
  mkdirSync(join(workspace, "memory"), { recursive: true });
  writeFileSync(join(workspace, "MYSELF.md"), "# MYSELF\nI am a test agent.");
  writeFileSync(join(workspace, "MYHUMAN.md"), "# MYHUMAN\n- Name: bree");
  // MEMORY.md deliberately absent
  const today = new Date().toISOString().slice(0, 10);
  writeFileSync(join(workspace, "memory", `${today}.md`), "- tested the prompt builder");
});

afterAll(() => rmSync(workspace, { recursive: true, force: true }));

describe("buildSystemPromptAppend", () => {
  test("includes base rules, present memory files, and today's note", async () => {
    const prompt = await buildSystemPromptAppend(workspace);
    expect(prompt).toContain("You are liquid");
    expect(prompt).toContain("Building apps");         // app skill section
    expect(prompt).toContain("/api/kv/");              // KV teaching
    expect(prompt).toContain("I am a test agent.");    // MYSELF.md content
    expect(prompt).toContain("- Name: bree");          // MYHUMAN.md content
    expect(prompt).toContain("tested the prompt builder"); // daily note
    expect(prompt).not.toContain("## MEMORY.md");      // absent file -> no section
  });

  test("missing workspace still yields the base prompt", async () => {
    const prompt = await buildSystemPromptAppend("/nonexistent/liquid-test");
    expect(prompt).toContain("You are liquid");
    expect(prompt).not.toContain("## MYSELF.md");
  });
});
