/**
 * Permanent guard for the polyglot + full-surface + realtime stack (ADRs
 * 0002/0003), proven by the Phoenix/Elixir collaborative whiteboard:
 *
 *   - the BUILT-IN APP LIBRARY: the whiteboard ships embedded in the binary
 *     and installs into the workspace through POST /api/catalog (copy →
 *     commit → deploy) — the real pick-and-choose flow, not a fixture copy;
 *   - a NON-BUN backend: the supervisor spawns `mix phx.server` from the
 *     manifest's declared argv, injects PORT, and health-gates on /health
 *     (first boot COMPILES the vendored deps — the slow-start path is real);
 *   - surface:"full": Phoenix serves its own HTML/assets; no index.html;
 *   - WebSocket passthrough: Phoenix Channels ride /app/whiteboard/socket;
 *   - visibility:"public": two GUEST browsers (no liquid login, no cookies)
 *     draw together — strokes replicate live, cursors show, Presence counts,
 *     and strokes persist across a page reload (server-side state).
 *
 *   nix develop --command bun run e2e/whiteboard.ts
 *
 * Offline: deps/ is vendored (embedded with the app), hex comes from
 * MIX_PATH, the fake harness needs no credentials.
 */
import puppeteer, { type Page } from "puppeteer-core";
import { existsSync, mkdirSync, readFileSync, rmSync } from "node:fs";
import { spawn } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";

const PORT = 3247;
const BASE = `http://127.0.0.1:${PORT}`;
const REPO = new URL("..", import.meta.url).pathname.replace(/\/$/, "");
const root = join(tmpdir(), `liquid-wb-e2e-${process.pid}`);
const WS = join(root, "workspace");
const DATA = join(root, "data");
const PASS = "wb-e2e-password";
const CHROME = process.env.LIQUID_CHROME ?? Bun.which("chromium") ?? "/run/current-system/sw/bin/chromium";
const APP = `${BASE}/app/whiteboard/`;

let bad = 0;
const ok = (n: string, p: boolean) => { console.log(`${p ? "  ✓" : "  ✗"} ${n}`); if (!p) bad++; };
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
async function until(what: string, fn: () => Promise<boolean>, ms = 30000, step = 400) {
  const start = Date.now();
  while (Date.now() - start < ms) { if (await fn().catch(() => false)) return true; await sleep(step); }
  console.log(`  … timeout waiting for: ${what}`);
  return false;
}
/** Draw a line on the board with real pointer events. */
async function draw(page: Page, from: [number, number], to: [number, number], steps = 12) {
  const box = (await (await page.$("canvas"))!.boundingBox())!;
  const at = (t: number): [number, number] => [
    box.x + box.width * (from[0] + (to[0] - from[0]) * t),
    box.y + box.height * (from[1] + (to[1] - from[1]) * t),
  ];
  await page.mouse.move(...at(0));
  await page.mouse.down();
  for (let i = 1; i <= steps; i++) { await page.mouse.move(...at(i / steps)); await sleep(25); }
  await page.mouse.up();
}

const wb = (page: Page, prop: string) => page.evaluate((p) => (window as any).__wb?.[p], prop);

mkdirSync(root, { recursive: true });
console.log("booting supervisor (fake harness, vibe mode) + chromium…");
const server = spawn("cargo", ["run", "--quiet"], {
  cwd: REPO,
  env: {
    ...process.env,
    LIQUID_FAKE_AGENT: "1",
    LIQUID_PORT: String(PORT),
    LIQUID_WORKSPACE_DIR: WS,
    LIQUID_DATA_DIR: DATA,
    LIQUID_PIPELINE_MODE: "vibe",
    // An EMPTY mix home: the dev machine's ~/.mix (auto-installed rebar3 from
    // a past `mix deps.get`) must not mask what a fresh production box lacks —
    // that exact masking shipped the "Could not find rebar3" crash-loop.
    // Offline builds stand on MIX_PATH (hex) + MIX_REBAR3 alone.
    MIX_HOME: join(root, "mix-home"),
  },
  stdio: ["ignore", "ignore", "inherit"],
});
const browser = await puppeteer.launch({
  executablePath: CHROME, headless: true,
  args: ["--no-sandbox", "--disable-dev-shm-usage", "--window-size=1100,800"],
});
try {
  await until("supervisor up", async () => (await fetch(BASE + "/api/health")).ok, 90000);

  // Install the whiteboard through the built-in app library — the real
  // pick-and-choose flow: embedded copy → workspace commit → deploy.
  const setup = await fetch(BASE + "/api/auth/setup", {
    method: "POST", headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ password: PASS }),
  });
  const token = (await setup.json()).token as string;
  const auth = { Authorization: `Bearer ${token}` };
  const catalog = async () =>
    ((await (await fetch(BASE + "/api/catalog", { headers: auth })).json()).apps ?? []) as any[];
  ok("the library lists the whiteboard, uninstalled",
    (await catalog()).some((a) => a.id === "whiteboard" && a.installed === false));
  const inst = await fetch(BASE + "/api/catalog/whiteboard/install", { method: "POST", headers: auth });
  ok("one click installs it (embedded copy → commit → deploy)", inst.status === 200);
  ok("…and the library now shows it installed and current",
    (await catalog()).some((a) => a.id === "whiteboard" && a.installed && !a.update_available));

  // First boot compiles the vendored Phoenix deps inside the served worktree —
  // the health gate has to carry a genuinely slow-starting backend.
  ok("mix phx.server comes up healthy (cold compile of vendored deps)",
    await until("whiteboard healthy", async () => (await fetch(APP)).status === 200, 300000, 1000));
  const home = await fetch(APP);
  ok("Phoenix serves its own document through the root proxy (surface: full)",
    (await home.text()).includes("Whiteboard"));
  ok("…as a GUEST: no login, no cookies, public visibility", home.status === 200);
  ok("static assets flow through too (vendored phoenix.mjs)",
    (await fetch(APP + "phoenix.mjs")).status === 200);

  // Two guests draw together.
  const [a, b] = [await browser.newPage(), await browser.newPage()];
  const errs: string[] = [];
  for (const p of [a, b]) {
    p.on("pageerror", (e) => errs.push(e.message));
    await p.setViewport({ width: 1100, height: 800 });
  }
  await a.goto(APP, { waitUntil: "networkidle2" });
  await b.goto(APP, { waitUntil: "networkidle2" });
  ok("both guests join the board over Phoenix Channels (WS through the proxy)",
    await until("both joined", async () => (await wb(a, "joined")) === true && (await wb(b, "joined")) === true, 30000));

  ok("Presence shows 2 people on both screens",
    await until("presence=2", async () => (await wb(a, "peers")) === 2 && (await wb(b, "peers")) === 2, 15000));

  const colorA = await wb(a, "color");
  const colorB = await wb(b, "color");
  ok("each artist gets their own color", !!colorA && !!colorB && colorA !== colorB);

  await draw(a, [0.2, 0.3], [0.6, 0.7]);
  ok("a stroke drawn by one guest appears live on the other's canvas",
    await until("stroke on b", async () => (await wb(b, "strokes")) >= 1, 15000));

  // Cursor broadcast: move (not drawing) on B, watch it appear on A.
  {
    const box = (await (await b.$("canvas"))!.boundingBox())!;
    for (let i = 0; i < 10; i++) {
      await b.mouse.move(box.x + 40 + i * 12, box.y + 60 + i * 8);
      await sleep(60);
    }
  }
  ok("live cursors: the other artist's pointer shows up",
    await until("cursor on a", async () => (await wb(a, "cursors")) >= 1, 15000));

  // Persistence: strokes are server-side state, not page state.
  await b.reload({ waitUntil: "networkidle2" });
  ok("after a reload, the drawing is still there (server snapshot)",
    await until("snapshot on reload", async () => (await wb(b, "joined")) === true && (await wb(b, "strokes")) >= 1, 30000));

  const strokesFile = join(DATA, "pipeline", "deployed", "apps", "whiteboard", "data", "strokes.json");
  ok("strokes persist to the app's data/ dir (survives restarts & redeploys)",
    await until("strokes.json written", async () => existsSync(strokesFile) && readFileSync(strokesFile, "utf8").includes("points"), 10000));

  ok("no page errors in either guest browser", errs.length === 0);
  if (errs.length) console.log("  errors:", errs.slice(0, 5));
} finally {
  await browser.close();
  server.kill("SIGTERM");
  await sleep(700);
  rmSync(root, { recursive: true, force: true });
}
console.log(bad === 0 ? "\nWHITEBOARD PASS" : `\nWHITEBOARD FAIL (${bad})`);
process.exit(bad === 0 ? 0 : 1);
