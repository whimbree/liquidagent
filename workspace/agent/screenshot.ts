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
  const url = `http://127.0.0.1:${port}/app/${encodeURIComponent(app)}/${path.replace(/^\/+/, "")}`;
  const out = `${tmpdir()}/liquid-shot-${crypto.randomUUID()}.png`;
  try {
    const proc = Bun.spawn(
      [chrome, "--headless=new", "--no-sandbox", "--disable-gpu", "--hide-scrollbars",
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
      const err = await new Response(proc.stderr).text();
      return { ok: false, error: `chromium produced no image: ${err.slice(0, 200)}` };
    }
    const bytes = await file.arrayBuffer();
    await unlink(out).catch(() => {});
    if (bytes.byteLength === 0) return { ok: false, error: "empty screenshot" };
    return { ok: true, data: Buffer.from(bytes).toString("base64") };
  } catch (err) {
    return { ok: false, error: err instanceof Error ? err.message : String(err) };
  }
}
