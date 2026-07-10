// Headless-chromium screenshot of a served app, for the agent's `screenshot`
// tool (so it can SEE apps it builds). Uses chromium's built-in --screenshot —
// no puppeteer dependency to vendor. Kept separate from harness.ts so it's unit
// testable against a real served app.
import { tmpdir } from "node:os";
import { unlink } from "node:fs/promises";

export type ShotResult = { ok: true; data: string } | { ok: false; error: string };

/** Capture `/app/<app>/<path>` off the local supervisor as a base64 PNG. */
export async function captureAppScreenshot(
  app: string,
  path: string,
  width: number,
  height: number,
): Promise<ShotResult> {
  const chrome =
    Bun.env.LIQUID_CHROME ?? Bun.which("chromium") ?? Bun.which("chromium-browser") ?? Bun.which("google-chrome-stable");
  if (!chrome) return { ok: false, error: "no chromium available (set LIQUID_CHROME on the host)" };
  const port = Bun.env.LIQUID_PORT ?? "3000";
  let url = `http://127.0.0.1:${port}/app/${encodeURIComponent(app)}/${path.replace(/^\/+/, "")}`;
  // Present the screenshot capability so PRIVATE apps render. The initial
  // navigation carries it in the query; the supervisor echoes it as a
  // path-scoped cookie so chromium's subresource requests are authorized too.
  const secret = Bun.env.LIQUID_INTERNAL_SECRET;
  if (secret) url += `${url.includes("?") ? "&" : "?"}__lshot=${encodeURIComponent(secret)}`;
  const out = `${tmpdir()}/liquid-shot-${crypto.randomUUID()}.png`;
  try {
    const proc = Bun.spawn(
      [chrome, "--headless=new", "--no-sandbox", "--disable-gpu", "--hide-scrollbars",
        // VMs often have a tiny /dev/shm; without this chromium crashes there.
        "--disable-dev-shm-usage",
        "--force-device-scale-factor=1", `--window-size=${width},${height}`,
        "--virtual-time-budget=3000", "--run-all-compositor-stages-before-draw",
        `--screenshot=${out}`, url],
      { stdout: "ignore", stderr: "pipe" },
    );
    const killer = setTimeout(() => proc.kill(), 20000);
    await proc.exited;
    clearTimeout(killer);
    const file = Bun.file(out);
    if (!(await file.exists())) {
      // Chromium front-loads harmless warnings (dbus, GL) and prints the fatal
      // error LAST — report the de-noised tail, or the agent (and the human)
      // misdiagnose the failure from the noise.
      const err = await new Response(proc.stderr).text();
      const salient = err
        .split("\n")
        .filter((l) => l.trim() && !/dbus/i.test(l))
        .slice(-4)
        .join(" | ");
      return { ok: false, error: `chromium produced no image: ${salient.slice(-300)}` };
    }
    const bytes = await file.arrayBuffer();
    await unlink(out).catch(() => {});
    if (bytes.byteLength === 0) return { ok: false, error: "empty screenshot" };
    return { ok: true, data: Buffer.from(bytes).toString("base64") };
  } catch (err) {
    return { ok: false, error: err instanceof Error ? err.message : String(err) };
  }
}
