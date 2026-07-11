/**
 * Permanent shell regression guard. Boots a real supervisor with the offline
 * fake harness, then drives the actual shell in headless chromium — login,
 * deploy an app, window-manager basics, a chat window with streaming, and the
 * command palette. This is the committed counterpart to e2e/smoke.ts (which
 * guards the HTTP/WS API); together they cover backend + shell.
 *
 *   nix develop --command bun run e2e/shell-smoke.ts
 *
 * Fake harness only — no credentials, no model, deterministic. puppeteer-core is
 * bun-auto-installed on first run; chromium comes from the dev shell (PATH) or
 * $LIQUID_CHROME.
 */
import puppeteer from "puppeteer-core";
import { mkdirSync, writeFileSync, rmSync } from "node:fs";
import { spawn, execSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";

const PORT = 3190;
const BASE = `http://127.0.0.1:${PORT}`;
const REPO = new URL("..", import.meta.url).pathname.replace(/\/$/, "");
const root = join(tmpdir(), `liquid-shell-e2e-${process.pid}`);
const WS = join(root, "workspace");
const PASSWORD = "shell-smoke-password";
const CHROME = process.env.LIQUID_CHROME ?? Bun.which("chromium") ?? "/run/current-system/sw/bin/chromium";

let failures = 0;
function check(name: string, pass: boolean) {
  console.log(`${pass ? "  ✓" : "  ✗"} ${name}`);
  if (!pass) failures++;
}
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const git = (a: string) => execSync(`git ${a}`, { cwd: WS }).toString();

mkdirSync(root, { recursive: true });
console.log("booting supervisor (fake harness) + chromium…");
const server = spawn("cargo", ["run", "--quiet"], {
  cwd: REPO,
  env: { ...process.env, LIQUID_FAKE_AGENT: "1", LIQUID_PORT: String(PORT), LIQUID_WORKSPACE_DIR: WS, LIQUID_DATA_DIR: join(root, "data") },
  stdio: ["ignore", "ignore", "inherit"],
});
const browser = await puppeteer.launch({ executablePath: CHROME, headless: true, args: ["--no-sandbox", "--disable-dev-shm-usage", "--window-size=1200,800"] });
try {
  for (let i = 0; i < 220; i++) { try { if ((await fetch(BASE + "/api/health")).ok) break; } catch {} await sleep(400); }
  const page = await browser.newPage();
  await page.setViewport({ width: 1200, height: 800 });
  const errs: string[] = [];
  page.on("pageerror", (e) => errs.push(e.message));

  // first-boot setup -> shell
  await page.goto(BASE, { waitUntil: "networkidle0" });
  await page.type("#password", PASSWORD);
  await page.click("#login-form button");
  await page.waitForFunction(() => document.getElementById("shell")!.classList.contains("active"), { timeout: 20000 });
  check("first-boot setup enters the shell", true);

  // the built-in StrongLifts app is seeded and served on a fresh install
  await page.waitForSelector(".appicon", { timeout: 8000 });
  check("the built-in StrongLifts app ships and appears", (await page.$$eval(".appicon .label", (els) => els.map((e) => e.textContent))).some((l) => /StrongLifts/.test(l || "")));

  // agent grows an app -> icon lands (commit + a fake reply claims file tools -> deploy)
  const dir = join(WS, "apps", "board");
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, "app.json"), JSON.stringify({ name: "Board", icon: "📋", description: "smoke", window: { width: 360, height: 300, minWidth: 300 } }));
  writeFileSync(join(dir, "index.html"), "<!doctype html><h1>board</h1>");
  // a PUBLIC (guest-facing) app rides along, so we can assert its iframe is isolated
  const gdir = join(WS, "apps", "guest");
  mkdirSync(gdir, { recursive: true });
  writeFileSync(join(gdir, "app.json"), JSON.stringify({ name: "Guest", icon: "🌐", description: "public", visibility: "public" }));
  writeFileSync(join(gdir, "index.html"), "<!doctype html><h1>guest</h1>");
  git("add -A"); git('commit -q -m "add board + guest"');
  await page.click("#chatfab");
  await page.waitForFunction(() => !(document.getElementById("send") as HTMLButtonElement).disabled, { timeout: 12000 });
  await page.type("#input", "make it"); await page.click("#send");
  // wait for THIS app specifically (a fresh install also seeds the built-in
  // StrongLifts app, so ".appicon" alone isn't enough).
  await page.waitForFunction(() => [...document.querySelectorAll(".appicon")].some((e) => (e as HTMLElement).dataset.id === "board"), { timeout: 20000 });
  await page.click("#chatclose");
  check("an app the agent built appears on the home screen", true);

  // mobile (clean state, before the desktop window tests): a fullscreen app hides
  // the floating chat FAB — it used to overlap the app's own bottom nav — and
  // relocates chat into the app's top bar.
  await page.setViewport({ width: 390, height: 844, isMobile: true, hasTouch: true });
  await page.waitForSelector('.appicon[data-id="board"]', { timeout: 5000 }); // grid re-renders on resize
  await page.evaluate(() => (document.querySelector('.appicon[data-id="board"]') as HTMLElement).click());
  await page.waitForFunction(() => document.getElementById("mobileapp")!.classList.contains("open"), { timeout: 5000 });
  const fabHidden = await page.$eval("#chatfab", (e) => getComputedStyle(e).display === "none");
  const topBarChat = await page.$eval("#mobilechat", (e) => getComputedStyle(e).display !== "none");
  check("mobile: app opens fullscreen, FAB hidden, chat in the top bar", fabHidden && topBarChat);

  // multitasking: background board, open a second app — both stay alive
  await page.click("#mobileback"); // home (keep board alive)
  await page.waitForSelector('.appicon[data-id="stronglifts"]', { timeout: 4000 });
  await page.evaluate(() => (document.querySelector('.appicon[data-id="stronglifts"]') as HTMLElement).click());
  await page.waitForFunction(() => document.querySelectorAll("#mobileframes iframe").length === 2, { timeout: 5000 });
  check("mobile: opening a second app keeps both alive (2 live iframes)", true);

  // recents overlay lists both and switches between them
  await page.click("#mobilerecents");
  await page.waitForFunction(() => document.getElementById("recents")!.classList.contains("open"), { timeout: 3000 });
  check("mobile: recents lists both open apps", (await page.$$eval("#recents .rtile", (ts) => ts.length)) === 2);
  await page.evaluate(() => { const t = [...document.querySelectorAll("#recents .rtile")].find((el) => /board/i.test(el.textContent || "")); (t as HTMLElement).click(); });
  await page.waitForFunction(() => document.querySelector("#mobileframes iframe.active")?.getAttribute("data-app") === "board", { timeout: 3000 });
  check("mobile: tapping a recents tile switches to that app (state kept)", true);

  // close the other app from recents -> one live iframe left
  await page.click("#mobilerecents");
  await page.waitForFunction(() => document.getElementById("recents")!.classList.contains("open"), { timeout: 3000 });
  await page.evaluate(() => { const t = [...document.querySelectorAll("#recents .rtile")].find((el) => /strong/i.test(el.textContent || "")); (t!.querySelector(".rclose") as HTMLElement).click(); });
  await page.waitForFunction(() => document.querySelectorAll("#mobileframes iframe").length === 1, { timeout: 3000 });
  check("mobile: closing from recents removes that app", true);
  await page.evaluate(() => document.getElementById("recents")!.classList.remove("open"));

  await page.click("#mobileback");
  await page.waitForFunction(() => !document.getElementById("mobileapp")!.classList.contains("open"), { timeout: 3000 });
  check("mobile: home backgrounds apps and restores the FAB", await page.$eval("#chatfab", (e) => getComputedStyle(e).display !== "none"));
  await page.setViewport({ width: 1200, height: 800 }); // back to desktop for the WM tests
  await page.waitForSelector('.appicon[data-id="board"]', { timeout: 5000 }); // grid re-renders on resize

  // open it -> window; minimize -> dock -> restore; maximize
  await page.evaluate(() => (document.querySelector('.appicon[data-id="board"]') as HTMLElement).click());
  await page.waitForSelector(".window:not(.chatwin)", { timeout: 5000 });
  const openWidth = await page.$eval(".window:not(.chatwin)", (el) => (el as HTMLElement).offsetWidth);
  check("window opens at the app's declared width (not the default)", openWidth > 340 && openWidth < 400);

  // a PRIVATE app (agent-authored) stays same-origin so it can use platform KV;
  // a PUBLIC (guest-facing) app is sandboxed to an opaque origin so its JS can't
  // read the shell's origin/localStorage/session token.
  const iframeSandbox = (frag: string) =>
    page.$eval(`.window:not(.chatwin) iframe[src*="/app/${frag}/"]`, (el) => el.getAttribute("sandbox"));
  check("a private app's iframe is NOT sandboxed (same-origin)", (await iframeSandbox("board")) === null);
  await page.evaluate(() => (document.querySelector('.appicon[data-id="guest"]') as HTMLElement).click());
  await page.waitForFunction(() => !!document.querySelector('.window:not(.chatwin) iframe[src*="/app/guest/"]'), { timeout: 5000 });
  const guestSandbox = (await iframeSandbox("guest")) ?? "";
  check("a PUBLIC app's iframe IS sandboxed without allow-same-origin",
    guestSandbox.includes("allow-scripts") && !guestSandbox.includes("allow-same-origin"));
  await page.evaluate(() => document.querySelectorAll('.window:not(.chatwin) iframe[src*="/app/guest/"]').forEach((f) => f.closest(".window")?.querySelector<HTMLElement>(".titlebar .close")?.click()));
  await sleep(150);
  await page.click(".window:not(.chatwin) .titlebar .min");
  await page.waitForFunction(() => document.querySelector(".window:not(.chatwin)")!.classList.contains("minimized"), { timeout: 3000 });
  check("app opens as a window and minimizes to the dock", (await page.$$("#dock button")).length === 1);
  await page.click("#dock button");
  await page.waitForFunction(() => !document.querySelector(".window:not(.chatwin)")!.classList.contains("minimized"), { timeout: 3000 });
  await page.click(".window:not(.chatwin) .titlebar .max");
  await page.waitForFunction(() => document.querySelector(".window:not(.chatwin)")!.classList.contains("maximized"), { timeout: 3000 });
  check("dock restores it and the maximize button works", true);
  await page.click(".window:not(.chatwin) .titlebar .min"); // stash it so the desk is clear
  await sleep(150);

  // a chat window on the desk, with streaming
  await page.evaluate(() => document.elementFromPoint(600, 400)!.dispatchEvent(new MouseEvent("dblclick", { bubbles: true, clientX: 600, clientY: 400 })));
  await page.waitForSelector(".chatwin", { timeout: 4000 });
  await page.$eval(".chatwin", (w) => {
    (w.querySelector("input") as HTMLInputElement).value = "hello from the smoke test";
    (w.querySelector("form") as HTMLFormElement).requestSubmit();
  });
  await page.waitForFunction(() => /fake harness/i.test(document.querySelector(".chatwin .msg.bot .mbody")?.textContent || ""), { timeout: 15000 });
  check("a chat window opens and streams a reply", true);

  // command palette
  await page.keyboard.down("Control"); await page.keyboard.press("k"); await page.keyboard.up("Control");
  await page.waitForFunction(() => document.getElementById("palette")!.classList.contains("open"), { timeout: 3000 });
  await page.type("#palette-input", "board");
  await page.waitForFunction(() => /Open Board/.test(document.querySelector("#palette-results .pal-item.sel")?.textContent || ""), { timeout: 3000 });
  check("the command palette opens and finds the app", true);
  await page.keyboard.press("Escape");

  // settings → System shows the running platform commit ("dev" under cargo
  // run; a sha + GitHub link + up-to-date check on a nix-built deploy)
  await page.click("#settingsbtn");
  await page.waitForFunction(() => /Binary/.test(document.getElementById("sys-build")?.textContent || ""), { timeout: 5000 });
  check("settings shows the running platform build", true);

  // settings → App library: seeded app reads installed, the whiteboard is one
  // click away — and installing it pops the icon onto the home screen live.
  await page.waitForFunction(() => document.querySelectorAll("#catalog-list .cat-row").length >= 2, { timeout: 5000 });
  check("the app library lists the built-ins",
    await page.$eval("#catalog-list", (el) =>
      /Installed ✓/.test(el.querySelector('.cat-row[data-app="stronglifts"]')?.textContent || "") &&
      !!el.querySelector('.cat-row[data-app="whiteboard"] button')));
  await page.click('#catalog-list .cat-row[data-app="whiteboard"] button');
  await page.waitForSelector('.appicon[data-id="whiteboard"]', { timeout: 15000 });
  check("installing from the library lands the app on the home screen, live", true);
  await page.click("#panelclose");

  check("the desktop chat is resizable like a window", (await page.$eval("#chat", (e) => getComputedStyle(e).resize)) === "both");

  // The chat header doubles as a drag handle; its pointerdown handler must NOT
  // preventDefault on interactive controls — that silently kills the model
  // <select>'s native dropdown (regression: "can't click the model selector").
  check("the header drag handle leaves the model picker's pointerdown alone",
    await page.$eval("#modelpick", (sel) => {
      const ev = new PointerEvent("pointerdown", { bubbles: true, cancelable: true });
      sel.dispatchEvent(ev);
      return !ev.defaultPrevented;
    }));

  // new-chat race: send the first message of a fresh chat and switch away
  // IMMEDIATELY. The id now binds at send time (REST create before ws.send), so
  // the reply can't stream into a conversation no surface owns.
  await page.click("#chatfab");
  await page.waitForFunction(() => document.getElementById("chat")!.classList.contains("open"), { timeout: 5000 });
  await page.click("#newchat");
  await page.type("#input", "race check chat");
  await page.$eval("#composer", (f) => (f as HTMLFormElement).requestSubmit());
  await page.click("#convtoggle"); // switch away with zero delay
  await page.evaluate(() => { const c = [...document.querySelectorAll("#convlist .conv")].find((e) => /make it/.test(e.textContent || "")); (c as HTMLElement).click(); });
  await page.click("#convtoggle");
  await page.waitForFunction(() => [...document.querySelectorAll("#convlist .conv")].some((e) => /race check chat/.test(e.textContent || "")), { timeout: 5000 });
  check("new-chat race: the conversation exists exactly once after switching away",
    (await page.$$eval("#convlist .conv", (els) => els.filter((e) => /race check chat/.test(e.textContent || "")).length)) === 1);
  await page.evaluate(() => { const c = [...document.querySelectorAll("#convlist .conv")].find((e) => /race check chat/.test(e.textContent || "")); (c as HTMLElement).click(); });
  await page.waitForFunction(() =>
    [...document.querySelectorAll("#log .msg.user")].some((u) => /race check chat/.test(u.textContent || "")) &&
    [...document.querySelectorAll("#log .msg.bot")].some((b) => /You said/.test(b.textContent || "")), { timeout: 15000 });
  check("new-chat race: switching back shows the message and the reply — nothing lost", true);
  await page.click("#chatclose");

  check("no page errors", errs.length === 0);
  if (errs.length) console.log("  errors:", errs.slice(0, 4));
} finally {
  await browser.close();
  server.kill("SIGTERM");
  await sleep(500);
  rmSync(root, { recursive: true, force: true });
}
console.log(failures === 0 ? "\nSHELL SMOKE PASS" : `\nSHELL SMOKE FAIL (${failures})`);
process.exit(failures === 0 ? 0 : 1);
