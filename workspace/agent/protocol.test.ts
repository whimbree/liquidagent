import { describe, expect, test } from "bun:test";
import contract from "./protocol.json";
import {
  AGENT_EVENT_TYPES,
  AGENT_REQUEST_TYPES,
  SHELL_ACTIONS,
  TOOL_STATUSES,
} from "./protocol";

// The TS half of the Rust↔TS wire-protocol parity guard. The Rust half lives in
// src/agent.rs (`wire_parity`). Both check their own definitions against
// protocol.json, so a change on either side that isn't reflected in the shared
// contract fails a test.
const sorted = (xs: readonly string[]) => [...xs].sort();

describe("wire-protocol parity (TS ↔ protocol.json)", () => {
  test("agent event types match the contract", () => {
    expect(sorted(AGENT_EVENT_TYPES)).toEqual(sorted(contract.agentEventTypes));
  });
  test("agent request types match the contract", () => {
    expect(sorted(AGENT_REQUEST_TYPES)).toEqual(sorted(contract.agentRequestTypes));
  });
  test("tool statuses match the contract", () => {
    expect(sorted(TOOL_STATUSES)).toEqual(sorted(contract.toolStatuses));
  });
  test("shell actions match the contract", () => {
    expect(sorted(SHELL_ACTIONS)).toEqual(sorted(contract.shellActions));
  });
});
