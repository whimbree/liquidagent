/**
 * Permanent guard for chat image attachments (paste/drop/attach a screenshot).
 * Fake harness, headless chromium. See e2e/smoke.ts for the API counterpart.
 */
// Verifies pasting an image into chat: it shows in the composer strip, sends,
// gets stored + served by the supervisor, renders as a thumbnail in the bubble,
// and survives a reload (rendered from /api/attachments).
import puppeteer from "puppeteer-core";
import { mkdirSync, rmSync } from "node:fs";
import { spawn } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";

const PORT = 3264;
const BASE = `http://127.0.0.1:${PORT}`;
const REPO = new URL("..", import.meta.url).pathname.replace(/\/$/, "");
const root = join(tmpdir(), `liquid-att-${process.pid}`);
const PASS = "att-pass-1";
const CHROME = process.env.LIQUID_CHROME ?? Bun.which("chromium") ?? "/run/current-system/sw/bin/chromium";
const PNG = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
let bad = 0;
const ok = (n: string, p: boolean) => { console.log(`${p ? "  ✓" : "  ✗"} ${n}`); if (!p) bad++; };
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

mkdirSync(root, { recursive: true });
const server = spawn("cargo", ["run", "--quiet"], { cwd: REPO, env: { ...process.env, LIQUID_FAKE_AGENT: "1", LIQUID_PORT: String(PORT), LIQUID_WORKSPACE_DIR: join(root, "workspace"), LIQUID_DATA_DIR: join(root, "data") }, stdio: ["ignore", "ignore", "inherit"] });
const browser = await puppeteer.launch({ executablePath: CHROME, headless: true, args: ["--no-sandbox", "--disable-dev-shm-usage", "--window-size=1200,900"] });
try {
  for (let i = 0; i < 220; i++) { try { if ((await fetch(BASE + "/api/health")).ok) break; } catch {} await sleep(400); }
  const page = await browser.newPage();
  await page.setViewport({ width: 1200, height: 900 });
  const errs: string[] = []; page.on("pageerror", (e) => errs.push(e.message));
  await page.goto(BASE, { waitUntil: "networkidle0" });
  await page.type("#password", PASS); await page.click("#login-form button");
  await page.waitForFunction(() => document.getElementById("shell")!.classList.contains("active"), { timeout: 20000 });
  const token = await page.evaluate(() => localStorage.getItem("liquid_token"));

  await page.click("#chatfab");
  await page.waitForFunction(() => document.getElementById("chat")!.classList.contains("open"), { timeout: 5000 });

  // paste a synthetic PNG into the composer
  await page.evaluate((b64) => {
    const bin = atob(b64); const arr = new Uint8Array(bin.length); for (let i = 0; i < bin.length; i++) arr[i] = bin.charCodeAt(i);
    const file = new File([arr], "shot.png", { type: "image/png" });
    const dt = new DataTransfer(); dt.items.add(file);
    document.getElementById("input")!.dispatchEvent(new ClipboardEvent("paste", { clipboardData: dt, bubbles: true, cancelable: true }));
  }, PNG);
  await page.waitForFunction(() => document.querySelectorAll("#attachstrip .athumb").length === 1, { timeout: 4000 });
  ok("pasting an image shows a thumbnail in the composer strip", true);

  // send it
  await page.type("#input", "what's wrong here?");
  await page.$eval("#composer", (f) => (f as HTMLFormElement).requestSubmit());
  await page.waitForFunction(() => !!document.querySelector("#log .msg.user .athumbs img"), { timeout: 5000 });
  ok("the sent user message shows the image thumbnail", true);
  ok("the composer strip clears after sending", (await page.$$("#attachstrip .athumb")).length === 0);

  await sleep(800); // let it persist + the fake reply land
  const auth = { Authorization: "Bearer " + token };
  const convs = (await (await fetch(BASE + "/api/conversations", { headers: auth })).json()).conversations;
  const cid = convs[0].id;
  const msgs = (await (await fetch(`${BASE}/api/conversations/${cid}/messages`, { headers: auth })).json()).messages;
  const userMsg = msgs.find((m: any) => m.role === "user");
  const att = userMsg?.attachments?.[0];
  ok("the stored user message carries an attachment ref", !!att && att.mime === "image/png");

  // the attachment is served (authed) as a real image
  const img = await fetch(`${BASE}/api/attachments/${att.id}?token=${token}`);
  ok("the attachment is served as image/png", img.ok && (img.headers.get("content-type") || "").includes("image/png"));
  const unauth = await fetch(`${BASE}/api/attachments/${att.id}`);
  ok("...and requires auth", unauth.status === 401);

  // survives reload (rendered from /api/attachments)
  await page.reload({ waitUntil: "networkidle0" });
  await page.waitForFunction(() => document.getElementById("shell")!.classList.contains("active"), { timeout: 20000 });
  await page.click("#chatfab");
  await page.waitForFunction(() => !!document.querySelector("#log .msg.user .athumbs img"), { timeout: 6000 }).catch(() => {});
  ok("the image still renders after a reload", await page.$eval("#log", (l) => !!l.querySelector(".msg.user .athumbs img")));

  ok("no page errors", errs.length === 0); if (errs.length) console.log("  errors:", errs.slice(0, 4));
} finally {
  await browser.close(); server.kill("SIGTERM"); await sleep(500); rmSync(root, { recursive: true, force: true });
}
console.log(bad === 0 ? "\nATTACH_PASS" : `\nATTACH_FAIL (${bad})`);
process.exit(bad === 0 ? 0 : 1);
