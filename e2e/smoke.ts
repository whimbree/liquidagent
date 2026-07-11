/**
 * End-to-end smoke suite. Boots a real supervisor with the offline fake
 * harness + fake reviewer (no credentials, deterministic), exercises the
 * cross-cutting flows, and tears it down. This is the regression guard the
 * milestone scratch-tests should have been.
 *
 *   nix develop --command bun run e2e/smoke.ts
 *
 * Uses only fake harnesses, so it never calls a model and is safe in CI.
 */
import { mkdirSync, writeFileSync, rmSync } from "node:fs";
import { execSync, spawn } from "node:child_process";
import { networkInterfaces, tmpdir } from "node:os";
import { join } from "node:path";

const PORT = 3199;
const BASE = `http://127.0.0.1:${PORT}`;
const REPO = new URL("..", import.meta.url).pathname.replace(/\/$/, "");
const root = join(tmpdir(), `liquid-e2e-${process.pid}`);
const WS = join(root, "workspace");
const DATA = join(root, "data");
const REMOTE = join(root, "remote.git");
const PASSWORD = "smoke-test-password";
const SHOT = "smoke-internal-secret";

let failures = 0;
function check(name: string, pass: boolean, detail = "") {
  console.log(`${pass ? "  ✓" : "  ✗"} ${name}${detail ? ` — ${detail}` : ""}`);
  if (!pass) failures++;
}
const git = (a: string, cwd = WS) => execSync(`git ${a}`, { cwd }).toString().trim();
async function j(path: string, opts: RequestInit = {}, token?: string) {
  const headers: Record<string, string> = { "Content-Type": "application/json", ...(opts.headers as any) };
  if (token) headers.Authorization = `Bearer ${token}`;
  const r = await fetch(BASE + path, { ...opts, headers });
  return { status: r.status, body: r.status === 204 ? null : await r.json().catch(() => null) };
}
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
async function until(what: string, fn: () => Promise<boolean>, ms = 25000) {
  const start = Date.now();
  while (Date.now() - start < ms) { if (await fn().catch(() => false)) return; await sleep(400); }
  throw new Error("timeout: " + what);
}
function commitApp(id: string, marker = "", visibility?: "public" | "private") {
  const dir = join(WS, "apps", id);
  mkdirSync(dir, { recursive: true });
  const manifest: Record<string, unknown> = { name: id, icon: "🧪", description: "e2e" };
  if (visibility) manifest.visibility = visibility;
  writeFileSync(join(dir, "app.json"), JSON.stringify(manifest));
  writeFileSync(join(dir, "index.html"), `<!doctype html><h1>${id} ${marker}</h1>`);
  git("add -A");
  git(`commit -q -m "add ${id}"`);
}
// The fake harness claims used_file_tools on every reply, so a chat message
// triggers the supervisor's post-query deploy reconcile.
function nudge(token: string): Promise<void> {
  return new Promise((res, rej) => {
    const ws = new WebSocket(`ws://127.0.0.1:${PORT}/ws?token=${token}`);
    const t = setTimeout(() => { ws.close(); rej(new Error("nudge timeout")); }, 15000);
    ws.onopen = () => ws.send(JSON.stringify({ type: "user_message", content: "nudge", conversation_id: null }));
    ws.onmessage = (m) => { if (JSON.parse(String(m.data)).type === "done") { clearTimeout(t); ws.close(); res(); } };
  });
}

// --- boot ------------------------------------------------------------------------
mkdirSync(root, { recursive: true });
execSync(`git init --bare -q ${REMOTE}`);
console.log("booting supervisor (fake harness + fake reviewer)…");
const server = spawn("cargo", ["run", "--quiet"], {
  cwd: REPO,
  env: {
    ...process.env,
    LIQUID_FAKE_AGENT: "1",
    LIQUID_PORT: String(PORT),
    LIQUID_WORKSPACE_DIR: WS,
    LIQUID_DATA_DIR: DATA,
    LIQUID_PIPELINE_MODE: "reviewed",
    LIQUID_HOST: "0.0.0.0", // so the remote-denial checks can hit a non-loopback address
    LIQUID_INTERNAL_SECRET: SHOT,
  },
  stdio: ["ignore", "ignore", "inherit"],
});
try {
  await until("supervisor up", async () => (await fetch(BASE + "/api/health")).ok, 90000);

  // --- health reports the platform build (dev under cargo run) ---
  {
    const h = await (await fetch(BASE + "/api/health")).json();
    check("health reports the platform build rev", typeof h?.build?.rev === "string" && h.build.rev.length > 0);
  }

  // --- auth ---
  console.log("auth");
  check("unauthenticated request rejected", (await j("/api/conversations")).status === 401);
  check("first-boot setup issues a token", (await j("/api/auth/setup", { method: "POST", body: JSON.stringify({ password: PASSWORD }) })).status === 200);
  const login = await j("/api/auth/login", { method: "POST", body: JSON.stringify({ password: PASSWORD }) });
  check("login returns a token", typeof login.body?.token === "string");
  check("wrong password rejected", (await j("/api/auth/login", { method: "POST", body: JSON.stringify({ password: "nope" }) })).status === 401);
  let token = login.body.token as string;

  // --- account: change password ---
  console.log("account");
  const cp = (body: object, tok?: string) => j("/api/auth/change_password", { method: "POST", body: JSON.stringify(body) }, tok);
  check("change-password rejects wrong current (403, not a 401 sign-out)",
    (await cp({ old_password: "wrong-current", new_password: "a-fine-new-password" }, token)).status === 403);
  check("change-password rejects a too-short new password (400)",
    (await cp({ old_password: PASSWORD, new_password: "short" }, token)).status === 400);
  const NEWPASS = "smoke-test-password-rotated";
  const changed = await cp({ old_password: PASSWORD, new_password: NEWPASS }, token);
  check("change-password succeeds and returns a fresh token", changed.status === 200 && typeof changed.body?.token === "string");
  check("the old token is revoked after a password change", (await j("/api/conversations", {}, token)).status === 401);
  check("the old password no longer logs in", (await j("/api/auth/login", { method: "POST", body: JSON.stringify({ password: PASSWORD }) })).status === 401);
  check("the new password logs in", (await j("/api/auth/login", { method: "POST", body: JSON.stringify({ password: NEWPASS }) })).status === 200);
  token = changed.body.token as string; // re-issued token carries the rest of the suite

  // --- settings: model picker ---
  console.log("settings");
  check("default model is 'default'", (await j("/api/settings", {}, token)).body?.model === "default");
  check("unknown model is rejected (400)",
    (await j("/api/settings", { method: "PUT", body: JSON.stringify({ model: "gpt-4" }) }, token)).status === 400);
  check("a valid model persists",
    (await j("/api/settings", { method: "PUT", body: JSON.stringify({ model: "opus" }) }, token)).status === 204
    && (await j("/api/settings", {}, token)).body?.model === "opus");

  // --- apps + KV + traversal ---
  console.log("apps + storage");
  check("kv put/get", (await j("/api/kv/x/k", { method: "PUT", body: JSON.stringify({ value: "v" }) }, token)).status === 204
    && (await j("/api/kv/x/k", {}, token)).body?.value === "v");
  for (const p of ["/app/x/../MYHUMAN.md", "/app/x/%2e%2e/MYHUMAN.md"]) {
    const r = await fetch(BASE + p);
    check(`traversal blocked (${p})`, !(r.ok && (await r.text()).includes("MYHUMAN")));
  }

  // --- pipeline: reviewed reject -> override ---
  // These test apps are private (no visibility set), so an owner fetch presents
  // the session cookie — the way the shell's iframe requests do.
  const app = (p: string) => fetch(BASE + p, { headers: { Cookie: `liquid_session=${token}` } });
  console.log("pipeline");
  commitApp("bad", "REJECT_ME");
  await nudge(token);
  await until("rejected", async () => (await j("/api/pipeline", {}, token)).body?.status?.state === "rejected");
  check("rejected app is not served", (await app("/app/bad/")).status === 404);
  check("rejection carries reasoning", ((await j("/api/pipeline", {}, token)).body?.status?.reasoning ?? "").length > 0);
  await j("/api/pipeline/approve", { method: "POST" }, token);
  await until("override served", async () => (await app("/app/bad/")).status === 200);
  check("human override deploys", true);

  // --- pipeline: reviewed approve ---
  commitApp("good");
  await nudge(token);
  await until("good served", async () => (await app("/app/good/")).status === 200);
  check("reviewed+approved deploys", (await j("/api/pipeline", {}, token)).body?.status?.state === "clean");

  // --- the built-in app library: list / install-guards / diverge / replace ---
  console.log("app library");
  {
    const cat = async () => ((await j("/api/catalog", {}, token)).body?.apps ?? []) as any[];
    const apps = await cat();
    check("the library lists seeded stronglifts as installed & current",
      apps.some((a) => a.id === "stronglifts" && a.installed && !a.update_available && !a.local_changes));
    check("the library lists the whiteboard (mix runtime), uninstalled",
      apps.some((a) => a.id === "whiteboard" && !a.installed && a.runtime === "mix" && a.surface === "full"));
    check("reinstalling an installed app is refused (409)",
      (await j("/api/catalog/stronglifts/install", { method: "POST" }, token)).status === 409);
    check("updating an uninstalled app is refused (409)",
      (await j("/api/catalog/whiteboard/update", { method: "POST", body: JSON.stringify({ mode: "merge" }) }, token)).status === 409);
    check("installing a non-library app 404s",
      (await j("/api/catalog/notreal/install", { method: "POST" }, token)).status === 404);

    // Diverge the workspace copy (as the agent would), then REPLACE restores
    // the pristine library version in a new commit — history keeps the fork.
    writeFileSync(join(WS, "apps", "stronglifts", "index.html"), "<!doctype html><h1>heavily customized</h1>");
    git("add -A"); git('commit -q -m "customize stronglifts"');
    await nudge(token); // deploy the divergence so the pipeline is quiet again
    await until("divergence deployed", async () => (await j("/api/pipeline", {}, token)).body?.status?.state === "clean");
    check("local evolution is detected", (await cat()).some((a) => a.id === "stronglifts" && a.local_changes));
    const replaced = await j("/api/catalog/stronglifts/update", { method: "POST", body: JSON.stringify({ mode: "replace" }) }, token);
    check("replace restores the library copy in a commit", replaced.status === 200);
    await until("restored app served", async () =>
      !(await (await app("/app/stronglifts/")).text()).includes("heavily customized"));
    check("…and it deploys (the customization is gone, history keeps it)", true);
    check("…after which the copy reads current again",
      (await cat()).some((a) => a.id === "stronglifts" && a.installed && !a.local_changes));
  }

  // --- polyglot backends (ADR 0002): a DECLARED runner, not the bun default ---
  // The app declares its own argv (custom entrypoint at the app root — no
  // backend/index.ts anywhere), a health path the supervisor must poll, and
  // extra env. Proves the run contract: PORT + LIQUID_APP_ID +
  // LIQUID_APP_DATA_DIR injected, declared env passed through.
  console.log("polyglot backend (declared run)");
  {
    const dir = join(WS, "apps", "declared");
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, "app.json"), JSON.stringify({
      name: "Declared", icon: "🧪",
      backend: { run: ["bun", "run", "server.ts"], health: "/health", env: { GREETING: "polyglot" } },
    }));
    writeFileSync(join(dir, "index.html"), "<!doctype html><h1>declared</h1>");
    writeFileSync(join(dir, "server.ts"), `
      Bun.serve({ port: Number(Bun.env.PORT), fetch(req) {
        const p = new URL(req.url).pathname;
        if (p === "/health") return new Response("ok");
        if (p === "/whoami") return Response.json({
          app: Bun.env.LIQUID_APP_ID, data: Bun.env.LIQUID_APP_DATA_DIR, greeting: Bun.env.GREETING,
        });
        return new Response("nope", { status: 404 });
      }});`);
    git("add -A");
    git('commit -q -m "add declared-runner app"');
  }
  await nudge(token);
  await until("declared backend healthy", async () => (await app("/app/declared/api/health")).status === 200);
  const who = await (await app("/app/declared/api/whoami")).json();
  check("declared argv runs (custom entrypoint, no backend/index.ts)", who?.app === "declared");
  check("run contract injects LIQUID_APP_DATA_DIR (the app's data/ dir)",
    typeof who?.data === "string" && who.data.endsWith("/apps/declared/data"));
  check("declared env rides along", who?.greeting === "polyglot");
  await until("declared health-gated status is running", async () => {
    const apps = (await j("/api/apps", {}, token)).body?.apps ?? [];
    return apps.find((a: any) => a.id === "declared")?.backend?.state === "running";
  });
  check("the declared health path gates status to running", true);

  // --- WebSocket passthrough + surface:"full" (ADR 0003) ---
  // sock: a normal panel app whose backend speaks WS behind /api/*.
  // fullapp: surface "full" — NO index.html anywhere; the backend owns the
  // whole document (HTML at /, assets, POST routes, a socket at /live).
  console.log("ws proxy + full surface");
  {
    const sock = join(WS, "apps", "sock");
    mkdirSync(sock, { recursive: true });
    writeFileSync(join(sock, "app.json"), JSON.stringify({
      name: "Sock", icon: "🔌", visibility: "public",
      backend: { run: ["bun", "run", "server.ts"] },
    }));
    writeFileSync(join(sock, "index.html"), "<!doctype html><h1>sock</h1>");
    writeFileSync(join(sock, "server.ts"), `
      Bun.serve({ port: Number(Bun.env.PORT),
        fetch(req, server) {
          if (new URL(req.url).pathname === "/ws")
            return server.upgrade(req) ? undefined : new Response("no", { status: 400 });
          return new Response("ok");
        },
        websocket: { message(ws, m) { ws.send("echo:" + m); } },
      });`);
    const full = join(WS, "apps", "fullapp");
    mkdirSync(full, { recursive: true });
    writeFileSync(join(full, "app.json"), JSON.stringify({
      name: "FullApp", icon: "🖼️", visibility: "public", surface: "full",
      backend: { run: ["bun", "run", "server.ts"], health: "/" },
    }));
    writeFileSync(join(full, "server.ts"), `
      Bun.serve({ port: Number(Bun.env.PORT),
        async fetch(req, server) {
          const u = new URL(req.url);
          if (u.pathname === "/live")
            return server.upgrade(req) ? undefined : new Response("no", { status: 400 });
          if (u.pathname === "/")
            return new Response("<!doctype html><h1>FULL-SURFACE-HOME</h1>", { headers: { "content-type": "text/html" } });
          if (u.pathname === "/assets/app.js")
            return new Response("// asset", { headers: { "content-type": "text/javascript" } });
          if (u.pathname === "/api/echo" && req.method === "POST")
            return Response.json({ got: await req.text() });
          return new Response("nope", { status: 404 });
        },
        websocket: { message(ws, m) { ws.send("full:" + m); } },
      });`);
    git("add -A");
    git('commit -q -m "add ws + full-surface apps"');
  }
  await nudge(token);
  await until("fullapp root proxied", async () => (await fetch(`${BASE}/app/fullapp/`)).status === 200);
  check("a full-surface app serves its OWN document (no index.html on disk)",
    (await (await fetch(`${BASE}/app/fullapp/`)).text()).includes("FULL-SURFACE-HOME"));
  check("…and its assets through the root wildcard",
    (await (await fetch(`${BASE}/app/fullapp/assets/app.js`)).text()).includes("asset"));
  const posted = await fetch(`${BASE}/app/fullapp/api/echo`, { method: "POST", body: "hi" });
  check("…and non-GET methods, with the /api prefix PRESERVED (it owns its paths)",
    posted.status === 200 && (await posted.json())?.got === "hi");
  const wsRoundtrip = (url: string, msg: string) => new Promise<string>((res, rej) => {
    const s = new WebSocket(url);
    const t = setTimeout(() => { s.close(); rej(new Error("ws timeout")); }, 8000);
    s.onopen = () => s.send(msg);
    s.onmessage = (m) => { clearTimeout(t); s.close(); res(String(m.data)); };
    s.onerror = () => { clearTimeout(t); rej(new Error("ws error")); };
  });
  await until("sock backend up", async () => (await fetch(`${BASE}/app/sock/api/`)).status === 200);
  check("a WebSocket upgrades through the /api proxy (panel app)",
    (await wsRoundtrip(`ws://127.0.0.1:${PORT}/app/sock/api/ws`, "ping").catch(String)) === "echo:ping");
  check("a WebSocket upgrades through the root proxy (full-surface app)",
    (await wsRoundtrip(`ws://127.0.0.1:${PORT}/app/fullapp/live`, "ping").catch(String)) === "full:ping");
  check("a WS to a PRIVATE app's backend is refused without auth",
    await new Promise<boolean>((res) => {
      const s = new WebSocket(`ws://127.0.0.1:${PORT}/app/declared/api/ws`);
      const t = setTimeout(() => { s.close(); res(false); }, 8000);
      s.onopen = () => { clearTimeout(t); s.close(); res(false); };
      s.onerror = () => { clearTimeout(t); res(true); };
    }));

  // --- app visibility: private by default, cookie authenticates ---
  console.log("app visibility");
  const rawLogin = await fetch(BASE + "/api/auth/login", {
    method: "POST", headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ password: NEWPASS }),
  });
  check("login sets the HttpOnly session cookie", (rawLogin.headers.get("set-cookie") || "").includes("liquid_session="));
  const cookieMint = await fetch(BASE + "/api/auth/cookie", { method: "POST", headers: { Authorization: `Bearer ${token}` } });
  check("an existing session can mint the cookie", cookieMint.status === 204 && (cookieMint.headers.get("set-cookie") || "").includes("liquid_session="));

  // Loopback is NOT a free pass (the fix): `good` is private, BASE is 127.0.0.1,
  // and a bare fetch has no cookie → denied. This is what stops a guest-reachable
  // app backend (also loopback) from proxy-reading private apps.
  check("loopback alone does not open a private app", (await fetch(`${BASE}/app/good/`)).status === 401);
  // The screenshot capability (what the in-VM tool presents) does open it, and
  // sets the path-scoped cookie so subresources are authorized too.
  const shot = await fetch(`${BASE}/app/good/?__lshot=${SHOT}`);
  check("the screenshot capability opens a private app", shot.status === 200);
  check("…and bootstraps a path-scoped shot cookie", (shot.headers.get("set-cookie") || "").includes("liquid_shot="));
  check("a wrong capability does not open it", (await fetch(`${BASE}/app/good/?__lshot=nope`)).status === 401);

  // A PUBLIC app is reachable by anyone, no auth — even remote.
  commitApp("shared", "", "public");
  await nudge(token);
  await until("public app served", async () => (await app("/app/shared/")).status === 200);
  const lan = Object.values(networkInterfaces()).flat().find((i) => i && !i.internal && i.family === "IPv4")?.address;
  if (lan) {
    const R = `http://${lan}:${PORT}`;
    check("a private app is denied from a remote address (401)", (await fetch(`${R}/app/good/`)).status === 401);
    check("…and its backend proxy is denied too", (await fetch(`${R}/app/good/api/x`)).status === 401);
    check("…but the session cookie opens it", (await fetch(`${R}/app/good/`, { headers: { Cookie: `liquid_session=${token}` } })).status === 200);
    check("…and a bogus cookie does not", (await fetch(`${R}/app/good/`, { headers: { Cookie: "liquid_session=deadbeef" } })).status === 401);
    check("a PUBLIC app is reachable remotely with no auth", (await fetch(`${R}/app/shared/`)).status === 200);
  } else {
    console.log("  (skip) no non-loopback interface for remote-denial checks");
  }

  // --- graduation ---
  console.log("graduation");
  const grad = await j("/api/apps/good/graduate", { method: "POST", body: JSON.stringify({ remote: REMOTE }) }, token);
  check("graduate succeeds", grad.status === 200);
  check("history landed at remote, rooted at app", (() => {
    try {
      const files = git("ls-tree --name-only main", REMOTE);
      return files.includes("index.html") && !files.includes("apps");
    } catch { return false; }
  })());

  // --- persistence across restart ---
  console.log("persistence");
  const convs = (await j("/api/conversations", {}, token)).body?.conversations ?? [];
  check("conversations were recorded", convs.length >= 1);
} finally {
  server.kill("SIGTERM");
  await sleep(500);
  rmSync(root, { recursive: true, force: true });
}

console.log(failures === 0 ? "\nSMOKE PASS" : `\nSMOKE FAIL (${failures})`);
process.exit(failures === 0 ? 0 : 1);
