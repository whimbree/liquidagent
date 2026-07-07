import { describe, expect, test } from "bun:test";
import { AgentRequestSchema } from "./ipc";

describe("AgentRequestSchema", () => {
  test("accepts a query with session", () => {
    const parsed = AgentRequestSchema.safeParse({
      type: "query", id: "7", prompt: "hi", session_id: "abc",
    });
    expect(parsed.success).toBe(true);
  });

  test("accepts a query without session", () => {
    expect(AgentRequestSchema.safeParse({ type: "query", id: "7", prompt: "hi" }).success).toBe(true);
  });

  test("accepts stop", () => {
    expect(AgentRequestSchema.safeParse({ type: "stop", id: "7" }).success).toBe(true);
  });

  test("rejects unknown type", () => {
    expect(AgentRequestSchema.safeParse({ type: "explode", id: "7" }).success).toBe(false);
  });

  test("rejects missing prompt", () => {
    expect(AgentRequestSchema.safeParse({ type: "query", id: "7" }).success).toBe(false);
  });

  test("rejects non-string id", () => {
    expect(AgentRequestSchema.safeParse({ type: "query", id: 7, prompt: "x" }).success).toBe(false);
  });
});
