/**
 * Permanent regression guard for the built-in StrongLifts app and, through it,
 * the app-backend contract: a shipped app with a Bun + bun:sqlite backend that
 * the supervisor spawns, proxies, and gives a writable data/ dir.
 *
 * Drives the real flow in headless chromium: log a set OFFLINE → it queues
 * locally and does NOT reach the DB → back ONLINE → the queue flushes into
 * SQLite → analytics are computed from the rows by SQL → after wiping the local
 * cache, history restores from the DB. This is the offline-first + durable-SQL
 * story end to end.
 *
 *   nix develop --command bun run e2e/stronglifts.ts
 *
 * Fake harness only — no credentials, deterministic. puppeteer-core is
 * bun-auto-installed; chromium comes from the dev shell (PATH) or $LIQUID_CHROME.
 */
import puppeteer from "puppeteer-core";
import { mkdirSync, rmSync } from "node:fs";
import { spawn } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";

const PORT = 3231;
const BASE = `http://127.0.0.1:${PORT}`;
const REPO = new URL("..", import.meta.url).pathname.replace(/\/$/, "");
const root = join(tmpdir(), `liquid-sl-e2e-${process.pid}`);
const PASS = "sl-e2e-password";
const CHROME = process.env.LIQUID_CHROME ?? Bun.which("chromium") ?? "/run/current-system/sw/bin/chromium";
const A = `${BASE}/app/stronglifts/api`;

let bad = 0;
const ok = (n: string, p: boolean) => { console.log(`${p ? "  ✓" : "  ✗"} ${n}`); if (!p) bad++; };
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const jget = async (p: string) => { const r = await fetch(A + p); return r.ok ? r.json() : null; };

mkdirSync(root, { recursive: true });
console.log("booting supervisor (fake harness) + chromium…");
const server = spawn("cargo", ["run", "--quiet"], {
  cwd: REPO,
  env: { ...process.env, LIQUID_FAKE_AGENT: "1", LIQUID_PORT: String(PORT), LIQUID_WORKSPACE_DIR: join(root, "workspace"), LIQUID_DATA_DIR: join(root, "data") },
  stdio: ["ignore", "ignore", "inherit"],
});
const browser = await puppeteer.launch({ executablePath: CHROME, headless: true, args: ["--no-sandbox", "--disable-dev-shm-usage", "--window-size=1200,900"] });
try {
  for (let i = 0; i < 220; i++) { try { if ((await fetch(BASE + "/api/health")).ok) break; } catch {} await sleep(400); }
  // the app's own backend spawns lazily on first deploy — wait for its health
  let backendUp = false;
  for (let i = 0; i < 120; i++) { try { if ((await fetch(A + "/health")).ok) { backendUp = true; break; } } catch {} await sleep(500); }
  ok("the StrongLifts Bun+SQLite backend spawns and is healthy", backendUp);

  const page = await browser.newPage();
  await page.setViewport({ width: 1200, height: 900 });
  const errs: string[] = [];
  page.on("pageerror", (e) => errs.push(e.message));
  page.on("dialog", (d) => d.accept());

  await page.goto(BASE, { waitUntil: "networkidle0" });
  await page.type("#password", PASS);
  await page.click("#login-form button");
  await page.waitForFunction(() => document.getElementById("shell")!.classList.contains("active"), { timeout: 20000 });

  await page.waitForSelector(".appicon", { timeout: 8000 });
  const labels = await page.$$eval(".appicon .label", (els) => els.map((e) => e.textContent));
  ok("StrongLifts is seeded on the home screen (fresh install)", labels.some((l) => /StrongLifts/.test(l || "")));

  await page.evaluate(() => (document.querySelector('.appicon[data-id="stronglifts"]') as HTMLElement).click());
  await page.waitForSelector(".window:not(.chatwin) iframe", { timeout: 5000 });
  await sleep(600);
  const frame = () => page.frames().find((f) => f.url().includes("/app/stronglifts"));
  ok("the app window loads the StrongLifts iframe", !!frame());
  const f = frame()!;

  // onboarding -> start -> log the first set of the first lift (squat)
  await f.waitForFunction(() => /Welcome/.test(document.querySelector("#title")?.textContent || ""), { timeout: 5000 });
  await f.$$eval(".finish", (bs) => (bs[0] as HTMLElement).click());
  await f.waitForFunction(() => document.querySelectorAll(".ex").length === 3, { timeout: 4000 });
  await f.$eval(".ex .set", (s) => (s as HTMLElement).click());
  await f.waitForFunction(() => document.querySelector(".ex .set")!.classList.contains("done"), { timeout: 3000 });
  ok("onboarding starts Workout A and a set logs (5 reps)", true);

  // --- go OFFLINE, finish the workout: it must queue, not reach the DB ---
  await page.setOfflineMode(true);
  await sleep(100);
  await f.$$eval(".finish", (bs) => (bs.find((b) => /Finish/.test(b.textContent || "")) as HTMLElement).click());
  await sleep(500);
  const queued = await f.evaluate(() => JSON.parse(localStorage.getItem("sl5x5.state.v2") || "{}").queue?.length || 0);
  ok("finishing offline queues the workout locally", queued >= 1);
  const before = await jget("/workouts");
  ok("...and it has NOT reached the SQL DB yet (offline)", Array.isArray(before) && before.length === 0);

  // --- back ONLINE: the queue flushes to the SQL backend ---
  await page.setOfflineMode(false);
  await f.evaluate(() => window.dispatchEvent(new Event("online")));
  let after: any = [];
  for (let i = 0; i < 20; i++) { after = await jget("/workouts"); if (Array.isArray(after) && after.length) break; await sleep(300); }
  ok("coming back online flushes the workout into the DB", Array.isArray(after) && after.length === 1);
  ok("the stored workout has the squat set at 5 reps", !!after[0]?.lifts?.some((l: any) => l.key === "squat" && l.reps.includes(5)));

  // --- analytics are computed from the rows by SQL ---
  const stats = await jget("/analytics");
  ok("analytics report the workout", stats?.totals?.workouts === 1);
  ok("analytics compute a per-lift series + est 1RM for squat", stats?.lifts?.squat?.sets >= 1 && stats.lifts.squat.e1rm != null);

  // the Analytics tab renders it in the UI
  await f.$eval('nav button[data-tab="stats"]', (b) => (b as HTMLElement).click());
  await f.waitForFunction(() => /workouts/.test(document.querySelector("#main .stat")?.textContent || ""), { timeout: 5000 }).catch(() => {});
  ok("the Analytics tab renders the summary + a lift chart", await f.$eval("#main", (m) => !!m.querySelector(".stat") && !!m.querySelector(".lc svg")));

  // --- wipe the LOCAL cache and reopen: it restores history from the DB ---
  await page.evaluate(() => { localStorage.removeItem("sl5x5.state.v2"); localStorage.removeItem("sl5x5.state.v1"); });
  await page.reload({ waitUntil: "networkidle0" });
  await page.waitForFunction(() => document.getElementById("shell")!.classList.contains("active"), { timeout: 20000 });
  await page.evaluate(() => (document.querySelector('.appicon[data-id="stronglifts"]') as HTMLElement).click());
  await page.waitForSelector(".window:not(.chatwin) iframe", { timeout: 5000 });
  await sleep(800);
  const f2 = frame()!;
  await f2.$eval('nav button[data-tab="history"]', (b) => (b as HTMLElement).click()).catch(() => {});
  await f2.waitForFunction(() => document.querySelector("#main .h") !== null, { timeout: 8000 }).catch(() => {});
  ok("after wiping the local cache, history restores from the SQL DB", await f2.$eval("#main", (m) => !!m.querySelector(".h")));

  ok("no page errors", errs.length === 0);
  if (errs.length) console.log("  errors:", errs.slice(0, 5));
} finally {
  await browser.close();
  server.kill("SIGTERM");
  await sleep(500);
  rmSync(root, { recursive: true, force: true });
}
console.log(bad === 0 ? "\nSTRONGLIFTS PASS" : `\nSTRONGLIFTS FAIL (${bad})`);
process.exit(bad === 0 ? 0 : 1);
