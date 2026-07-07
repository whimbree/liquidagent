/**
 * Offline fake reviewer for deterministic pipeline tests. Approves any diff
 * unless it contains the literal marker REJECT_ME, so both paths are testable
 * without a model.
 */
const diff = await Bun.stdin.text();
if (diff.includes("REJECT_ME")) {
  console.log(JSON.stringify({ verdict: "REJECTED", reasoning: "diff contains the REJECT_ME marker" }));
} else {
  console.log(JSON.stringify({ verdict: "APPROVED", reasoning: "fake reviewer approves" }));
}
