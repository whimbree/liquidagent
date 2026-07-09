"use strict";
/** Get a shell element by id. Ids are known-present in shell.html, so this is
 *  asserted non-null; specific element types are narrowed at use sites.
 *  @param {string} id @returns {HTMLElement} */
const $ = (id) => /** @type {HTMLElement} */ (document.getElementById(id));
/** @param {string} id @returns {HTMLInputElement} */
const $in = (id) => /** @type {HTMLInputElement} */ ($(id));
/** @param {string} id @returns {HTMLIFrameElement} */
const $if = (id) => /** @type {HTMLIFrameElement} */ ($(id));

/**
 * @typedef {{x:number,y:number,w:number,h:number,maximized?:boolean,minimized?:boolean}} WinGeom
 * @typedef {{id:number|null,x:number,y:number,w:number,h:number,maximized?:boolean,minimized?:boolean}} ChatWinGeom
 * @typedef {{name:string,apps:string[]}} Folder
 * @typedef {{accent?:string,wallpaper?:string}} Appearance
 * @typedef {{windows:Record<string,WinGeom>,chatWindows?:ChatWinGeom[],folders?:Record<string,Folder>,iconOrder?:string[],appearance?:Appearance,chat?:{x:number,y:number}}} Layout
 * @typedef {{id:string,name:string,icon:string,description:string,has_backend?:boolean,backend?:{state:string},window?:{width?:number,height?:number,minWidth?:number,minHeight?:number}}} App
 * @typedef {{id:number,title:string}} Conversation
 * @typedef {{title:string,body:string,ts:number}} TrayNotification
 * @typedef {{el:HTMLElement,wid:number,id:number|null,currentBot:HTMLElement|null,log:HTMLElement,input:HTMLInputElement,send:HTMLButtonElement,stop:HTMLButtonElement,status:HTMLElement,title:HTMLElement,convlist:HTMLElement,geom:WinGeom}} ChatWin
 * @typedef {{el:HTMLElement,icon:string,label:string}} SwitcherItem
 * @typedef {{icon:string,label:string,run:()=>void}} PaletteItem
 * @typedef {{type:string,conversation_id?:number,text?:string,name?:string,message?:string,session_id?:string,action?:string,app?:string,title?:string,body?:string,apps?:App[],status?:{state?:string,reasoning?:string},busy?:boolean,mode?:string}} WsEvent
 */
const RECONNECT_DELAY_MS = 1500;
const LAYOUT_SAVE_DEBOUNCE_MS = 600;
const isMobile = () => window.matchMedia("(max-width: 720px)").matches;

/* ---------- tiny markdown ---------- */
/** @param {string} s */
function escapeHtml(s){return s.replace(/&/g,"&amp;").replace(/</g,"&lt;").replace(/>/g,"&gt;");}
/** @param {string} src */
function renderMarkdown(src){
  const parts = src.split(/```/); let html = "";
  for (let i = 0; i < parts.length; i++) {
    const seg = parts[i] ?? "";
    if (i % 2 === 1) {
      const body = seg.replace(/^[a-zA-Z0-9_-]*\n/, "");
      html += `<pre><code>${escapeHtml(body)}</code></pre>`; continue;
    }
    let t = escapeHtml(seg);
    t = t.replace(/`([^`\n]+)`/g, "<code>$1</code>");
    t = t.replace(/\*\*([^*\n]+)\*\*/g, "<b>$1</b>");
    t = t.replace(/\bhttps?:\/\/[^\s<]+/g, u => `<a href="${u}" target="_blank" rel="noopener">${u}</a>`);
    html += t.split(/\n{2,}/).map(p => `<p>${p.replace(/\n/g,"<br>")}</p>`).join("");
  }
  return html;
}

/* ---------- auth ---------- */
let token = localStorage.getItem("liquid_token");
/** @param {string} path @param {RequestInit} [options] */
async function api(path, options = {}) {
  const headers = /** @type {Record<string,string>} */ (Object.assign({ "Content-Type": "application/json" }, options.headers));
  if (token) headers["Authorization"] = `Bearer ${token}`;
  const response = await fetch(path, Object.assign({}, options, { headers }));
  if (response.status === 401) { showLogin(false); throw new Error("unauthorized"); }
  return response;
}
let passwordIsSet = true;
async function boot() {
  const status = await (await fetch("/api/auth/status")).json();
  passwordIsSet = status.password_set;
  if (token && passwordIsSet) {
    const probe = await fetch("/api/apps", { headers: { Authorization: `Bearer ${token}` } });
    if (probe.ok) { enterShell((await probe.json()).apps); return; }
    token = null; localStorage.removeItem("liquid_token");
  }
  showLogin(!passwordIsSet);
}
/** @param {boolean} isSetup */
function showLogin(isSetup) {
  $("shell").classList.remove("active");
  $("login").style.display = "flex";
  $("login-hint").textContent = isSetup
    ? "First boot — choose a password (8+ characters)." : "Welcome back.";
}
$("login-form").onsubmit = async (e) => {
  e.preventDefault();
  const password = $in("password").value;
  const path = passwordIsSet ? "/api/auth/login" : "/api/auth/setup";
  const response = await fetch(path, { method:"POST",
    headers:{ "Content-Type":"application/json" }, body: JSON.stringify({ password }) });
  const body = await response.json();
  if (!response.ok) { $("login-err").textContent = body.error ?? "failed"; return; }
  token = body.token; localStorage.setItem("liquid_token", body.token);
  enterShell((await (await api("/api/apps")).json()).apps);
};

/* ---------- shell state ---------- */
/** @type {App[]} */
let apps = [];
/** @type {Layout} */
let layout = { windows: {} };   // appId -> {x,y,w,h}
let zCounter = 10;
/** @type {Map<string, HTMLElement>} */
const openWindows = new Map();  // appId -> element

/** @param {App[]} appList */
async function enterShell(appList) {
  apps = appList;
  $("login").style.display = "none";
  $("shell").classList.add("active");
  try { layout = await (await api("/api/shell")).json(); } catch { layout = { windows: {} }; }
  if (!layout || typeof layout !== "object") layout = { windows: {} };
  if (!layout.windows) layout.windows = {};
  if (!Array.isArray(layout.chatWindows)) layout.chatWindows = [];
  applyAppearance();
  loadNotifs(); updateTrayBadge();
  renderGrid();
  if (!isMobile()) {
    for (const id of Object.keys(layout.windows)) {
      if (apps.some(a => a.id === id)) openApp(id, false);
    }
  }
  await loadConversations();
  if (!isMobile()) {
    for (const cw of layout.chatWindows) {
      if (cw.id == null || conversations.some(c => c.id === cw.id)) openChatWindow(cw.id, undefined, cw);
    }
  }
  connect();
  initPush();
  initPipeline();
}

/* ---------- deploy pipeline ---------- */
async function initPipeline() {
  try {
    const p = await (await api("/api/pipeline")).json();
    renderMode(p.mode);
    renderPipeline(p.status);
  } catch {}
}
/** @param {string} mode */
function renderMode(mode) {
  const pill = $("modepill");
  pill.textContent = mode;
  pill.classList.toggle("reviewed", mode === "reviewed");
}
/** @param {{state?:string,reasoning?:string}|null} status */
function renderPipeline(status) {
  const bar = $("pipeline");
  const msg = $("pipeline-msg");
  const approve = $("pipeline-approve");
  bar.className = "";
  if (!status || status.state === "clean") {
    bar.classList.remove("show");
    $("home").classList.remove("pushed");
    return;
  }
  bar.classList.add("show");
  $("home").classList.add("pushed");
  if (status.state === "reviewing") {
    bar.classList.add("reviewing");
    msg.textContent = "⏳ Reviewing a change before it goes live…";
    approve.hidden = true;
  } else if (status.state === "rejected") {
    bar.classList.add("rejected");
    msg.textContent = "⚠️ A change was rejected and is not live: " + (status.reasoning || "");
    approve.hidden = false;
  }
}
$("modepill").onclick = async () => {
  const current = $("modepill").textContent;
  const next = current === "reviewed" ? "vibe" : "reviewed";
  try {
    await api("/api/pipeline", { method: "PUT", body: JSON.stringify({ mode: next }) });
    renderMode(next);
    toast(next === "reviewed" ? "Reviewed mode — changes get checked before going live" : "Vibe mode — changes ship immediately");
  } catch { toast("Couldn't change mode"); }
};
$("pipeline-approve").onclick = async () => {
  try { await api("/api/pipeline/approve", { method: "POST" }); toast("Deployed"); }
  catch { toast("Approve failed"); }
};
$("pipeline-dismiss").onclick = () => { $("pipeline").classList.remove("show"); $("home").classList.remove("pushed"); };

/* ---------- push notifications ---------- */
/** @type {ServiceWorkerRegistration | null} */
let swReg = null;
async function initPush() {
  if (!("serviceWorker" in navigator)) return;
  try { swReg = await navigator.serviceWorker.register("/sw.js"); } catch {}
  updateBell();
}
/** @param {string} b64 */
function b64ToU8(b64) {
  const pad = "=".repeat((4 - (b64.length % 4)) % 4);
  const raw = atob((b64 + pad).replace(/-/g, "+").replace(/_/g, "/"));
  return Uint8Array.from(raw, (c) => c.charCodeAt(0));
}
async function currentSubscription() {
  try { return swReg ? await swReg.pushManager.getSubscription() : null; } catch { return null; }
}
async function updateBell() {
  $("bell").textContent = (await currentSubscription()) ? "🔔" : "🔕";
}
$("bell").onclick = async () => {
  if (!swReg) { toast("Notifications need a service worker (https or localhost)"); return; }
  const existing = await currentSubscription();
  try {
    if (existing) {
      await api("/api/push/unsubscribe", { method: "POST", body: JSON.stringify({ endpoint: existing.endpoint }) });
      await existing.unsubscribe();
      toast("Notifications off");
    } else {
      const permission = await Notification.requestPermission();
      if (permission !== "granted") { toast("Notifications blocked by the browser"); return; }
      const { key } = await (await api("/api/push/key")).json();
      const sub = await swReg.pushManager.subscribe({
        userVisibleOnly: true,
        applicationServerKey: b64ToU8(key),
      });
      const json = sub.toJSON();
      await api("/api/push/subscribe", {
        method: "POST",
        body: JSON.stringify({ endpoint: sub.endpoint, keys: { p256dh: json.keys?.p256dh, auth: json.keys?.auth } }),
      });
      toast("Notifications on 🔔");
      api("/api/push/test", { method: "POST" }); // instant proof it works
    }
  } catch (err) { toast("Push setup failed: " + String(err)); }
  updateBell();
};

/* ---------- unread state ---------- */
const unread = new Set();
function updateUnreadUi() {
  const has = unread.size > 0;
  $("chatfab").classList.toggle("unread", has);
  $("mobilechat").classList.toggle("unread", has); // in-app top-bar chat button
}

/* ---------- agent-busy indicator (serialized worker) ---------- */
/** @type {string | null} */
let busyOn = null;
function updateBusyIndicator() {
  const el = $("busy");
  if (busyOn) { el.textContent = `liquid is working on ${busyOn}…`; el.classList.add("show"); }
  else { el.classList.remove("show"); }
}

/* ---------- home grid: icons, reorder, and folders (SHELL.json metadata) ---------- */
// Folders are pure shell metadata; apps stay flat on disk. layout.folders maps
// folderId -> { name, apps:[appId] }; layout.iconOrder is a list of tokens
// (an appId, or "f:"+folderId) giving the grid arrangement.
function folders() { return layout.folders || (layout.folders = {}); }
/** @param {string} id */
function appFolder(id) { for (const [fid, f] of Object.entries(folders())) if (f.apps.includes(id)) return fid; return null; }
function newFolderId() { return "fld" + Math.random().toString(36).slice(2, 8); }
function pruneFolders() { for (const [fid, f] of Object.entries(folders())) if (!f.apps || f.apps.length === 0) delete folders()[fid]; }

function orderedItems() {
  pruneFolders();
  const order = layout.iconOrder || [];
  const fids = new Set(Object.keys(folders()));
  const loose = new Set(apps.map(a => a.id).filter(id => !appFolder(id)));
  const items = [], seen = new Set();
  for (const tok of order) {
    if (typeof tok !== "string") continue;
    if (tok.startsWith("f:")) { const fid = tok.slice(2); if (fids.has(fid) && !seen.has(tok)) { items.push({ kind: "folder", id: fid }); seen.add(tok); } }
    else if (loose.has(tok) && !seen.has(tok)) { items.push({ kind: "app", id: tok }); seen.add(tok); }
  }
  for (const fid of fids) if (!seen.has("f:" + fid)) items.push({ kind: "folder", id: fid });
  for (const id of loose) if (!seen.has(id)) items.push({ kind: "app", id });
  return items;
}
function saveIconOrder() {
  layout.iconOrder = [...$("grid").children].map(c => {
    const el = /** @type {HTMLElement} */ (c);
    return el.dataset.kind === "folder" ? "f:" + el.dataset.id : (el.dataset.id ?? "");
  });
  scheduleLayoutSave();
}

/** @param {App} app */
function makeAppIcon(app) {
  const el = document.createElement("div");
  el.className = "appicon"; el.dataset.kind = "app"; el.dataset.id = app.id; el.draggable = true;
  const failed = app.backend && app.backend.state === "failed";
  el.innerHTML = `<div class="glyph">${escapeHtml(app.icon)}${failed ? '<span class="badge">⚠️</span>' : ""}</div>
                  <div class="label">${escapeHtml(app.name)}</div>`;
  el.title = failed ? `${app.name} — its backend crashed; ask liquid to fix it` : (app.description || app.name);
  el.onclick = () => openApp(app.id, true);
  el.addEventListener("dragstart", () => el.classList.add("dragging"));
  el.addEventListener("dragend", () => { el.classList.remove("dragging"); dropItem(el); });
  el.oncontextmenu = (e) => { e.preventDefault(); showAppMenu(app, e.clientX, e.clientY); };
  /** @type {ReturnType<typeof setTimeout> | null} */
  let pressTimer = null;
  el.addEventListener("touchstart", (e) => { pressTimer = setTimeout(() => { pressTimer = null; const t = e.touches[0]; if (t) showAppMenu(app, t.clientX, t.clientY); }, 550); }, { passive: true });
  el.addEventListener("touchend", () => { if (pressTimer) clearTimeout(pressTimer); });
  el.addEventListener("touchmove", () => { if (pressTimer) clearTimeout(pressTimer); });
  return el;
}
/** @param {string} fid */
function makeFolderIcon(fid) {
  const f = /** @type {Folder} */ (folders()[fid]);
  const el = document.createElement("div");
  el.className = "folder"; el.dataset.kind = "folder"; el.dataset.id = fid; el.draggable = true;
  const minis = f.apps.slice(0, 4).map(id => { const a = apps.find(x => x.id === id); return `<span>${a ? escapeHtml(a.icon) : ""}</span>`; }).join("");
  el.innerHTML = `<div class="folder-glyph">${minis}</div><div class="label">${escapeHtml(f.name || "Folder")}</div>`;
  el.onclick = () => openFolder(fid);
  el.addEventListener("dragstart", () => el.classList.add("dragging"));
  el.addEventListener("dragend", () => { el.classList.remove("dragging"); dropItem(el); });
  return el;
}
function renderGrid() {
  const grid = $("grid"); grid.innerHTML = "";
  for (const item of orderedItems()) grid.appendChild(item.kind === "app" ? makeAppIcon(/** @type {App} */(apps.find(a => a.id === item.id))) : makeFolderIcon(item.id));
  $("empty").hidden = apps.length > 0;
}

// Drag: reorder in free space, or drop an app onto another app/folder to group.
/** @type {HTMLElement | null} */
let combineTarget = null;
function clearCombine() { if (combineTarget) { combineTarget.classList.remove("combine-target"); combineTarget = null; } }
$("grid").addEventListener("dragover", (e) => {
  e.preventDefault();
  const dragging = /** @type {HTMLElement | null} */ ($("grid").querySelector(".dragging"));
  if (!dragging) return;
  clearCombine();
  if (dragging.dataset.kind === "app") { // only apps combine (no nested folders)
    const over = /** @type {HTMLElement | null} */ (document.elementFromPoint(e.clientX, e.clientY)?.closest(".appicon, .folder") ?? null);
    if (over && over !== dragging) {
      const r = over.getBoundingClientRect();
      const centered = e.clientX > r.left + r.width * 0.28 && e.clientX < r.right - r.width * 0.28
                    && e.clientY > r.top + r.height * 0.28 && e.clientY < r.bottom - r.height * 0.28;
      if (centered) { combineTarget = over; over.classList.add("combine-target"); return; }
    }
  }
  /** @type {Element | null} */
  let ref = null;
  for (const el of $("grid").querySelectorAll(".appicon:not(.dragging), .folder:not(.dragging)")) {
    const r = el.getBoundingClientRect();
    const cx = r.left + r.width / 2, cy = r.top + r.height / 2;
    if (e.clientY < cy - r.height / 2 || (Math.abs(e.clientY - cy) <= r.height / 2 && e.clientX < cx)) { ref = el; break; }
  }
  if (ref) $("grid").insertBefore(dragging, ref); else $("grid").appendChild(dragging);
});
/** @param {HTMLElement} el */
function dropItem(el) {
  if (combineTarget && el.dataset.kind === "app") {
    const appId = el.dataset.id ?? "", target = combineTarget, tid = target.dataset.id ?? "";
    clearCombine();
    if (target.dataset.kind === "folder") { const f = folders()[tid]; if (f) f.apps.push(appId); }
    else { const fid = newFolderId(); folders()[fid] = { name: "Folder", apps: [tid, appId] }; }
    renderGrid(); saveIconOrder();
    return;
  }
  clearCombine();
  saveIconOrder();
}

/* ---------- folder view ---------- */
/** @param {string} fid */
function openFolder(fid) {
  const f = folders()[fid]; if (!f) return;
  $in("fv-name").value = f.name || "Folder"; $("fv-name").dataset.fid = fid;
  const g = $("fv-grid"); g.innerHTML = "";
  for (const id of f.apps) {
    const a = apps.find(x => x.id === id); if (!a) continue;
    const el = document.createElement("div"); el.className = "appicon";
    el.innerHTML = `<div class="glyph">${escapeHtml(a.icon)}</div><div class="label">${escapeHtml(a.name)}</div>
                    <button class="fv-remove" title="Remove from folder">✕</button>`;
    /** @type {HTMLElement} */ (el.querySelector(".glyph")).onclick = () => { closeFolder(); openApp(a.id, true); };
    /** @type {HTMLElement} */ (el.querySelector(".fv-remove")).onclick = (e) => { e.stopPropagation(); removeFromFolder(fid, a.id); };
    g.appendChild(el);
  }
  $("folder-view").classList.add("open");
}
function closeFolder() { $("folder-view").classList.remove("open"); }
/** @param {string} fid @param {string} appId */
function removeFromFolder(fid, appId) {
  const f = folders()[fid]; if (!f) return;
  f.apps = f.apps.filter(x => x !== appId);
  scheduleLayoutSave();
  renderGrid();
  if (f.apps.length === 0) closeFolder(); else openFolder(fid);
}
$("fv-name").onchange = () => {
  const fid = $("fv-name").dataset.fid;
  const f = fid ? folders()[fid] : undefined;
  if (f) { f.name = $in("fv-name").value.trim() || "Folder"; scheduleLayoutSave(); renderGrid(); }
};
$("fv-close").onclick = closeFolder;
$("folder-view").onclick = (e) => { if (e.target === $("folder-view")) closeFolder(); };

/* ---- app context menu: lifecycle actions are conversations ---- */
/** @param {string} prefill */
function askLiquid(prefill) {
  summonChat();
  $in("input").value = prefill;
  $("input").focus();
  $in("input").setSelectionRange(prefill.length, prefill.length);
}
/** @param {App} app @param {number} x @param {number} y */
function showAppMenu(app, x, y) {
  const menu = $("appmenu");
  menu.innerHTML = "";
  /** @type {(null | [string, () => void, string?])[]} */
  const items = [
    ["Open", () => openApp(app.id, true)],
    null,
    ["Rename…", () => askLiquid(`Rename the ${app.name} app to: `)],
    ["Change icon…", () => askLiquid(`Change the ${app.name} app's icon to: `)],
    ["Improve…", () => askLiquid(`Improve the ${app.name} app: `)],
    ["What changed?", async () => {
      const data = await (await api(`/api/apps/${encodeURIComponent(app.id)}/log`)).json();
      const lines = data.commits.slice(0, 8).map((/** @type {{hash:string,subject:string}} */ c) => `${c.hash} ${c.subject}`).join("\n");
      toast(lines || "No history yet");
    }],
    ["Publish as its own repo…", async () => {
      const remote = prompt(`Publish ${app.name} to a git remote (its history is carved out and pushed as main):`);
      if (!remote) return;
      toast("Publishing…");
      try {
        const res = await api(`/api/apps/${encodeURIComponent(app.id)}/graduate`, {
          method: "POST", body: JSON.stringify({ remote }),
        });
        if (res.ok) { const b = await res.json(); toast(`Published to ${b.remote} (${b.ref})`); }
        else { const b = await res.json().catch(() => ({})); toast(b.error || "Publish failed"); }
      } catch { toast("Publish failed"); }
    }],
    null,
    ["Delete…", () => askLiquid(
      `Delete the ${app.name} app (remove apps/${app.id} and commit; git history keeps a copy).`
    ), "danger"],
  ];
  for (const item of items) {
    if (item === null) { menu.appendChild(document.createElement("hr")); continue; }
    const [label, action, cls] = item;
    const btn = document.createElement("button");
    btn.textContent = label;
    if (cls) btn.className = cls;
    btn.onclick = () => { hideAppMenu(); action(); };
    menu.appendChild(btn);
  }
  menu.classList.add("open");
  const w = menu.offsetWidth, h = menu.offsetHeight;
  menu.style.left = Math.min(x, innerWidth - w - 8) + "px";
  menu.style.top = Math.min(y, innerHeight - h - 8) + "px";
}
function hideAppMenu() { $("appmenu").classList.remove("open"); }
addEventListener("pointerdown", (e) => { if (!(/** @type {Element|null} */(e.target))?.closest("#appmenu")) hideAppMenu(); });

/** @type {ReturnType<typeof setTimeout> | undefined} */
let toastTimer;
/** @param {string} text */
function toast(text) {
  const el = $("toast");
  el.textContent = text;
  el.classList.add("show");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => el.classList.remove("show"), 3200);
}

/* ---------- notification tray (in-shell history; distinct from OS push) ---------- */
/** @type {TrayNotification[]} */
let notifications = [];
let unseenNotifs = 0;
function loadNotifs() { try { notifications = JSON.parse(localStorage.getItem("liquid_notifs") || "[]"); } catch { notifications = []; } }
function saveNotifs() { localStorage.setItem("liquid_notifs", JSON.stringify(notifications.slice(0, 50))); }
function updateTrayBadge() {
  const b = $("tray-btn");
  b.dataset.count = unseenNotifs > 99 ? "99+" : String(unseenNotifs);
  b.classList.toggle("has-unseen", unseenNotifs > 0);
}
/** @param {string} title @param {string} [body] */
function addNotification(title, body) {
  notifications.unshift({ title, body: body || "", ts: Date.now() / 1000 });
  notifications = notifications.slice(0, 50);
  saveNotifs();
  unseenNotifs++; updateTrayBadge();
  if ($("tray-panel").classList.contains("open")) renderTray();
}
function renderTray() {
  const list = $("tray-list"); list.innerHTML = "";
  if (notifications.length === 0) { list.innerHTML = `<div class="tray-empty">No notifications yet.</div>`; return; }
  for (const n of notifications) {
    const el = document.createElement("div"); el.className = "tray-item";
    el.innerHTML = `<div class="tray-title">${escapeHtml(n.title)}</div>` +
      (n.body ? `<div class="tray-body">${escapeHtml(n.body)}</div>` : "") +
      `<div class="tray-time">${new Date(n.ts * 1000).toLocaleString()}</div>`;
    list.appendChild(el);
  }
}
$("tray-btn").onclick = () => {
  const open = $("tray-panel").classList.toggle("open");
  if (open) { unseenNotifs = 0; updateTrayBadge(); renderTray(); }
};
$("tray-clear").onclick = () => { notifications = []; saveNotifs(); renderTray(); };
addEventListener("pointerdown", (e) => {
  const t = /** @type {Element|null} */ (e.target);
  if (!t?.closest("#tray-panel") && !t?.closest("#tray-btn")) $("tray-panel").classList.remove("open");
});

/* ---------- windows ---------- */
/** @type {ReturnType<typeof setTimeout> | undefined} */
let saveTimer;
function scheduleLayoutSave() {
  clearTimeout(saveTimer);
  saveTimer = setTimeout(() => {
    api("/api/shell", { method:"PUT", body: JSON.stringify(layout) }).catch(() => {});
  }, LAYOUT_SAVE_DEBOUNCE_MS);
}

/** @param {string} id @param {boolean} [focus] */
function openApp(id, focus) {
  const app = apps.find(a => a.id === id);
  if (!app) return;
  if (isMobile()) { openMobileApp(app); return; }
  if (openWindows.has(id)) { bringToFront(/** @type {HTMLElement} */(openWindows.get(id))); return; }

  const saved = /** @type {Partial<WinGeom>} */ (layout.windows[id] ?? {});
  const win = document.createElement("div");
  win.className = "window";
  win.dataset.app = id;
  const x = saved.x ?? (60 + openWindows.size * 40);
  const y = saved.y ?? (60 + openWindows.size * 40);
  // First-open size comes from the app's declared geometry; after that, the
  // remembered per-app geometry wins.
  const wpx = saved.w ?? app.window?.width ?? 420, hpx = saved.h ?? app.window?.height ?? 520;
  win.style.cssText = `left:${x}px; top:${y}px; width:${wpx}px; height:${hpx}px; z-index:${++zCounter};`;
  if (app.window?.minWidth) win.style.minWidth = app.window.minWidth + "px";
  if (app.window?.minHeight) win.style.minHeight = app.window.minHeight + "px";
  win.innerHTML = `
    <div class="titlebar">
      <span class="ticon">${escapeHtml(app.icon)}</span>
      <span class="tname">${escapeHtml(app.name)}</span>
      <button class="history" title="History">🕘</button>
      <button class="reload" title="Reload">↻</button>
      <button class="min" title="Minimize">–</button>
      <button class="max" title="Maximize">▢</button>
      <button class="close" title="Close">✕</button>
    </div>
    <iframe src="/app/${encodeURIComponent(id)}/" title="${escapeHtml(app.name)}"></iframe>`;
  $("shell").appendChild(win);
  openWindows.set(id, win);
  layout.windows[id] = { x, y, w: wpx, h: hpx, maximized: !!saved.maximized, minimized: !!saved.minimized };
  if (saved.maximized) win.classList.add("maximized");
  if (saved.minimized) win.classList.add("minimized");
  scheduleLayoutSave();
  renderDock();

  win.addEventListener("pointerdown", () => bringToFront(win));
  /** @type {HTMLElement} */ (win.querySelector(".close")).onclick = (e) => {
    e.stopPropagation();
    win.remove(); openWindows.delete(id);
    delete layout.windows[id];
    scheduleLayoutSave();
    renderDock();
  };
  /** @type {HTMLElement} */ (win.querySelector(".reload")).onclick = (e) => {
    e.stopPropagation();
    /** @type {HTMLIFrameElement} */ (win.querySelector("iframe")).src = `/app/${encodeURIComponent(id)}/`;
  };
  /** @type {HTMLElement} */ (win.querySelector(".max")).onclick = (e) => { e.stopPropagation(); toggleMaximize(win); };
  /** @type {HTMLElement} */ (win.querySelector(".min")).onclick = (e) => { e.stopPropagation(); minimizeWindow(win); };
  /** @type {HTMLElement} */ (win.querySelector(".history")).onclick = async (e) => {
    e.stopPropagation();
    const existing = win.querySelector(".histpop");
    if (existing) { existing.remove(); return; }
    const pop = document.createElement("div");
    pop.className = "histpop";
    pop.innerHTML = `<div class="none">Loading…</div>`;
    win.appendChild(pop);
    try {
      const data = await (await api(`/api/apps/${encodeURIComponent(id)}/log`)).json();
      pop.innerHTML = data.commits.length === 0
        ? `<div class="none">No history yet.</div>`
        : data.commits.map((/** @type {{timestamp:number,hash:string,subject:string}} */ c) => {
            const when = new Date(c.timestamp * 1000).toLocaleString(undefined,
              { month:"short", day:"numeric", hour:"2-digit", minute:"2-digit" });
            return `<div class="commit"><code>${escapeHtml(c.hash)}</code>
                    <span>${escapeHtml(c.subject)}</span><small>${when}</small></div>`;
          }).join("");
    } catch { pop.innerHTML = `<div class="none">Couldn't load history.</div>`; }
  };

  // drag, snap, and double-click-to-maximize via the shared window dragger
  enableWinDrag(win, () => persistWindow(win));

  // persist native CSS resize
  new ResizeObserver(() => {
    if (!openWindows.has(id)) return;
    persistWindow(win);
  }).observe(win);

  // loading ghost + crash overlay (apps serve same-origin, so we can watch them)
  const iframe = /** @type {HTMLIFrameElement} */ (win.querySelector("iframe"));
  const loading = document.createElement("div");
  loading.className = "win-loading"; loading.innerHTML = `<div class="spinner"></div>`;
  win.appendChild(loading);
  const crash = document.createElement("div");
  crash.className = "crash-overlay";
  crash.innerHTML = `<div class="crash-card"><div class="crash-emoji">💥</div>
    <div class="crash-title">This app hit an error</div>
    <div class="crash-detail"></div>
    <button class="crash-fix">Ask liquid to fix it</button>
    <button class="crash-reload ghost">Reload</button></div>`;
  win.appendChild(crash);
  let crashDetail = "";
  const showCrash = (/** @type {string} */ detail) => {
    crashDetail = detail || "an unknown error";
    /** @type {HTMLElement} */ (crash.querySelector(".crash-detail")).textContent = crashDetail;
    crash.classList.add("show");
  };
  /** @type {HTMLElement} */ (crash.querySelector(".crash-fix")).onclick = () => askLiquid(`The ${app.name} app hit this error: ${crashDetail}. Please fix it.`);
  /** @type {HTMLElement} */ (crash.querySelector(".crash-reload")).onclick = () => { crash.classList.remove("show"); iframe.src = `/app/${encodeURIComponent(id)}/`; };
  iframe.addEventListener("load", () => {
    if (loading.parentElement) loading.remove();
    try {
      const cw = iframe.contentWindow;
      if (cw) {
        cw.addEventListener("error", (ev) => showCrash(ev.message || "a script error"));
        cw.addEventListener("unhandledrejection", (ev) => showCrash(String((ev.reason && ev.reason.message) || ev.reason || "an error")));
      }
    } catch {}
  });
  iframe.addEventListener("error", () => { if (loading.parentElement) loading.remove(); showCrash("the app failed to load"); });
  if (app.backend && app.backend.state === "failed") showCrash("its backend crashed — check the app’s server code");

  if (focus) bringToFront(win);
}

/** @param {HTMLElement} win */
function persistWindow(win) {
  const id = win.dataset.app; if (!id) return;
  const prev = /** @type {Partial<WinGeom>} */ (layout.windows[id] ?? {});
  // While maximized/minimized the live offsets aren't the restore geometry —
  // keep the previous geometry and just record the state flags.
  const normal = !win.classList.contains("maximized") && !win.classList.contains("minimized");
  const geom = normal
    ? { x: win.offsetLeft, y: win.offsetTop, w: win.offsetWidth, h: win.offsetHeight }
    : { x: prev.x ?? 0, y: prev.y ?? 0, w: prev.w ?? 420, h: prev.h ?? 520 };
  layout.windows[id] = { ...geom,
    maximized: win.classList.contains("maximized"),
    minimized: win.classList.contains("minimized") };
  scheduleLayoutSave();
}

// Minimize / maximize work on any .window (app or chat); persistence dispatches.
/** @param {HTMLElement} win */
function persistWin(win) { win.classList.contains("chatwin") ? saveChatWins() : persistWindow(win); }
/** @param {HTMLElement} win */
function minimizeWindow(win) { win.classList.add("minimized"); renderDock(); persistWin(win); }
/** @param {HTMLElement} win */
function restoreWindow(win) { win.classList.remove("minimized"); bringToFront(win); renderDock(); persistWin(win); }
/** @param {HTMLElement} win */
function toggleMaximize(win) { win.classList.toggle("maximized"); persistWin(win); }

/** @param {HTMLElement} win */
function bringToFront(win) {
  win.style.zIndex = String(++zCounter);
  document.querySelectorAll(".window").forEach(w => w.classList.toggle("focused", w === win));
}

function renderDock() {
  const dock = $("dock");
  dock.innerHTML = "";
  const add = (/** @type {HTMLElement} */ win, /** @type {string} */ icon, /** @type {string} */ label) => {
    const btn = document.createElement("button");
    btn.textContent = icon; btn.title = label;
    if (win.classList.contains("minimized")) btn.classList.add("mini");
    btn.onclick = () => win.classList.contains("minimized") ? restoreWindow(win) : bringToFront(win);
    dock.appendChild(btn);
  };
  for (const [id, win] of openWindows) {
    const app = apps.find(a => a.id === id);
    if (app) add(win, app.icon, app.name);
  }
  for (const w of chatWins.values()) add(w.el, "💬", w.title.textContent || "Chat");
  dock.classList.toggle("visible", openWindows.size + chatWins.size > 0);
}

/* ---------- window switcher / tiling / tidy ---------- */
function allWindows() {
  /** @type {SwitcherItem[]} */
  const list = [];
  for (const [id, win] of openWindows) { const app = apps.find(a => a.id === id); if (app) list.push({ el: win, icon: app.icon, label: app.name }); }
  for (const w of chatWins.values()) list.push({ el: w.el, icon: "💬", label: w.title.textContent || "Chat" });
  return list.sort((a, b) => (parseInt(b.el.style.zIndex) || 0) - (parseInt(a.el.style.zIndex) || 0));
}
/** @type {{items:SwitcherItem[],sel:number}|null} */
let switcher = null;
/** @param {SwitcherItem} it */
function focusSwitcherItem(it) { it.el.classList.contains("minimized") ? restoreWindow(it.el) : bringToFront(it.el); }
function openSwitcher() {
  const items = allWindows();
  if (items.length === 0) return;
  if (items.length === 1) { focusSwitcherItem(/** @type {SwitcherItem} */(items[0])); return; }
  switcher = { items, sel: 1 }; // 1 = the window behind the current top
  renderSwitcher();
}
function renderSwitcher() {
  if (!switcher) return;
  const box = $("switcher"); box.innerHTML = "";
  const sel = switcher.sel;
  switcher.items.forEach((it, i) => {
    const tile = document.createElement("div");
    tile.className = "switch-tile" + (i === sel ? " sel" : "");
    tile.innerHTML = `<span class="sicon">${escapeHtml(it.icon)}</span><span class="slabel">${escapeHtml(it.label)}</span>`;
    tile.onclick = () => commitSwitcher(i);
    box.appendChild(tile);
  });
  box.classList.add("open");
}
function cycleSwitcher() { if (switcher) { switcher.sel = (switcher.sel + 1) % switcher.items.length; renderSwitcher(); } }
/** @param {number} [i] */
function commitSwitcher(i) {
  if (!switcher) return;
  const it = switcher.items[i ?? switcher.sel];
  $("switcher").classList.remove("open"); switcher = null;
  if (it) focusSwitcherItem(it);
}
function cancelSwitcher() { if (switcher) { $("switcher").classList.remove("open"); switcher = null; } }

function focusedWindow() { return /** @type {HTMLElement|null} */ (document.querySelector(".window.focused:not(.minimized)")); }
/** @param {HTMLElement} win */
function closeWindow(win) { /** @type {HTMLElement|null} */ (win.querySelector(".close"))?.click(); }
/** @param {HTMLElement} win @param {boolean} left */
function snapHalf(win, left) {
  win.classList.remove("maximized");
  const half = Math.floor(innerWidth / 2);
  win.style.left = (left ? 0 : half) + "px"; win.style.top = "0px";
  win.style.width = half + "px"; win.style.height = innerHeight + "px";
  persistWin(win);
}
function tidyWindows() {
  let i = 0;
  for (const el of document.querySelectorAll(".window:not(.minimized)")) {
    const win = /** @type {HTMLElement} */ (el);
    win.classList.remove("maximized");
    win.style.left = (40 + i * 34) + "px"; win.style.top = (48 + i * 34) + "px";
    win.style.width = "460px"; win.style.height = "540px";
    bringToFront(win); persistWin(win); i++;
  }
}

/* ---------- command palette (⌘K) ---------- */
/** @param {string} text */
function askLiquidSend(text) { summonChat(); $in("input").value = text; /** @type {HTMLFormElement} */ ($("composer")).requestSubmit(); }
/** @param {string} q @returns {PaletteItem[]} */
function paletteItems(q) {
  const query = q.toLowerCase().trim();
  /** @type {PaletteItem[]} */
  const items = [];
  for (const a of apps) items.push({ icon: a.icon, label: `Open ${a.name}`, run: () => openApp(a.id, true) });
  items.push({ icon: "💬", label: "New chat window", run: () => isMobile() ? newConversation() : openChatWindow(null) });
  items.push({ icon: "🪟", label: "Tidy windows", run: () => tidyWindows() });
  items.push({ icon: "⚙", label: "Open settings", run: () => { $("panelbg").classList.add("open"); loadSettings(); } });
  items.push({ icon: "🔔", label: "Show notifications", run: () => $("tray-btn").click() });
  let filtered = query ? items.filter(it => it.label.toLowerCase().includes(query)) : items;
  // "Ask liquid" is the fallback, below any matching apps/actions.
  if (query) filtered = [...filtered.slice(0, 7), { icon: "✨", label: `Ask liquid: “${q.trim()}”`, run: () => askLiquidSend(q.trim()) }];
  return filtered.slice(0, 8);
}
/** @type {{items:PaletteItem[],sel:number}|null} */
let palette = null;
/** @param {string} q */
function renderPalette(q) {
  const items = paletteItems(q);
  palette = { items, sel: 0 };
  const box = $("palette-results"); box.innerHTML = "";
  items.forEach((it, i) => {
    const el = document.createElement("div");
    el.className = "pal-item" + (i === 0 ? " sel" : "");
    el.innerHTML = `<span class="pal-icon">${escapeHtml(it.icon)}</span><span>${escapeHtml(it.label)}</span>`;
    el.onclick = () => runPalette(i);
    box.appendChild(el);
  });
}
function openPalette() { $in("palette-input").value = ""; renderPalette(""); $("palette").classList.add("open"); $("palette-input").focus(); }
function closePalette() { $("palette").classList.remove("open"); palette = null; }
/** @param {number} d */
function movePalette(d) {
  if (!palette || !palette.items.length) return;
  palette.sel = (palette.sel + d + palette.items.length) % palette.items.length;
  const sel = palette.sel;
  [...$("palette-results").children].forEach((el, i) => el.classList.toggle("sel", i === sel));
}
/** @param {number} [i] */
function runPalette(i) { const it = palette && palette.items[i ?? palette.sel]; closePalette(); if (it) it.run(); }
$("palette-input").addEventListener("input", () => renderPalette($in("palette-input").value));
$("palette-input").addEventListener("keydown", (e) => {
  if (e.key === "ArrowDown") { e.preventDefault(); movePalette(1); }
  else if (e.key === "ArrowUp") { e.preventDefault(); movePalette(-1); }
  else if (e.key === "Enter") { e.preventDefault(); runPalette(); }
  else if (e.key === "Escape") { e.preventDefault(); closePalette(); }
});
$("palette").onclick = (e) => { if (e.target === $("palette")) closePalette(); };

/* ---------- chat windows: independent, navigable chat surfaces on the desk ---------- */
// A chat window is its own view: it shows ONE conversation at a time but can
// switch to any (its own ☰ list). Keyed by a stable window id, not by
// conversation — two windows may show the same chat, and a window can change
// which chat it shows.
/** @type {Map<number, ChatWin>} */
const chatWins = new Map();     // windowId -> window object
/** @type {ChatWin[]} */
const pendingNewChats = [];     // windows that sent a new-conversation message
let chatWinSeq = 0;

// Shared window dragger for any .window (app or chat): titlebar drag, edge
// snaps (top→maximize, left/right→half), restore-a-maximized-window-on-move,
// and double-click-to-maximize.
/** @param {HTMLElement} win @param {(() => void)=} onDrop */
function enableWinDrag(win, onDrop) {
  const bar = /** @type {HTMLElement} */ (win.querySelector(".titlebar"));
  bar.addEventListener("dblclick", (e) => { if (/** @type {Element|null} */(e.target)?.tagName !== "BUTTON") toggleMaximize(win); });
  bar.addEventListener("pointerdown", (e) => {
    if (/** @type {Element|null} */(e.target)?.tagName === "BUTTON") return;
    e.preventDefault();
    bringToFront(win);
    win.classList.add("dragging");
    let startX = e.clientX - win.offsetLeft, startY = e.clientY - win.offsetTop, unmaxed = false;
    const move = (/** @type {PointerEvent} */ ev) => {
      // first movement on a maximized window restores it to float under the cursor
      if (win.classList.contains("maximized") && !unmaxed) {
        win.classList.remove("maximized");
        const rw = win.offsetWidth;
        win.style.left = Math.max(0, Math.min(ev.clientX - rw / 2, innerWidth - rw)) + "px";
        win.style.top = "0px";
        startX = ev.clientX - win.offsetLeft; startY = ev.clientY - win.offsetTop; unmaxed = true;
      }
      win.style.left = Math.max(0, Math.min(ev.clientX - startX, innerWidth - 80)) + "px";
      win.style.top  = Math.max(0, Math.min(ev.clientY - startY, innerHeight - 60)) + "px";
    };
    const up = (/** @type {PointerEvent} */ ev) => {
      removeEventListener("pointermove", move);
      removeEventListener("pointerup", up);
      win.classList.remove("dragging");
      // edge/corner snapping: top-center → maximize, corners → quarters, sides → halves
      const S = 18;
      const nl = ev.clientX <= S, nr = ev.clientX >= innerWidth - S;
      const nt = ev.clientY <= S, nb = ev.clientY >= innerHeight - S;
      const half = Math.floor(innerWidth / 2), hh = Math.floor(innerHeight / 2);
      if (nt && !nl && !nr) { win.classList.add("maximized"); if (onDrop) onDrop(); return; }
      if ((nl || nr) && (nt || nb)) {
        win.style.left = (nl ? 0 : half) + "px"; win.style.top = (nt ? 0 : hh) + "px";
        win.style.width = half + "px"; win.style.height = hh + "px";
      } else if (nl || nr) {
        win.style.left = (nl ? 0 : half) + "px"; win.style.top = "0px";
        win.style.width = half + "px"; win.style.height = innerHeight + "px";
      }
      if (onDrop) onDrop();
    };
    addEventListener("pointermove", move);
    addEventListener("pointerup", up);
  });
}

/** @param {HTMLElement} el */
function nearBottom(el) { return el.scrollHeight - el.scrollTop - el.clientHeight < 80; }
/** @param {number} sec */
function fmtTime(sec) { return new Date(sec * 1000).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" }); }
/** @type {WeakMap<HTMLElement, HTMLElement>} */
const scrollBtns = new WeakMap();
/** @param {HTMLElement} logEl */
function syncScrollBtn(logEl) { const b = scrollBtns.get(logEl); if (b) b.classList.toggle("show", !nearBottom(logEl)); }
// Streaming updates go through the message body so the timestamp survives.
/** @param {HTMLElement} el @param {string} html */
function setBubble(el, html) {
  const logEl = el.parentElement;
  const stick = logEl ? nearBottom(logEl) : false;
  const b = el.querySelector(".mbody"); if (b) b.innerHTML = html;
  if (logEl) { if (stick) logEl.scrollTop = logEl.scrollHeight; syncScrollBtn(logEl); }
}
/** @param {HTMLElement} logEl @param {string} cls @param {string} text @param {boolean} [asMarkdown] @param {number} [ts] */
function bubbleIn(logEl, cls, text, asMarkdown = false, ts) {
  const stick = nearBottom(logEl);
  const el = document.createElement("div");
  el.className = "msg " + cls;
  const t = ts || Date.now() / 1000;
  el.title = new Date(t * 1000).toLocaleString();
  el.innerHTML = `<span class="mbody"></span><span class="ts">${fmtTime(t)}</span>`;
  const body = /** @type {HTMLElement} */ (el.querySelector(".mbody"));
  if (asMarkdown) body.innerHTML = renderMarkdown(text); else body.textContent = text;
  logEl.appendChild(el);
  if (stick) logEl.scrollTop = logEl.scrollHeight;
  syncScrollBtn(logEl);
  return el;
}
/** @param {HTMLElement} logEl @param {{role:string,content:string,created_at?:number}} m */
function renderMsgInto(logEl, m) {
  if (m.role === "user") bubbleIn(logEl, "user", m.content, false, m.created_at);
  else if (m.role === "assistant") bubbleIn(logEl, "bot", m.content, true, m.created_at);
  else if (m.role === "scheduled") bubbleIn(logEl, "scheduled", m.content, false, m.created_at);
  else bubbleIn(logEl, "errmsg", m.content, false, m.created_at);
}

// Persist the open windows (conversation + geometry + state). Geometry is only
// refreshed from live offsets when the window is in its normal (floating) state.
function saveChatWins() {
  layout.chatWindows = [...chatWins.values()].map(w => {
    if (!w.el.classList.contains("maximized") && !w.el.classList.contains("minimized")) {
      w.geom = { x: w.el.offsetLeft, y: w.el.offsetTop, w: w.el.offsetWidth, h: w.el.offsetHeight };
    }
    return { id: w.id, ...w.geom,
      maximized: w.el.classList.contains("maximized"),
      minimized: w.el.classList.contains("minimized") };
  });
  scheduleLayoutSave();
}

// Point a window at a conversation (or null for a fresh one) — same window.
/** @param {ChatWin} w @param {number|null} id */
async function setConv(w, id) {
  w.id = id; w.currentBot = null;
  const conv = conversations.find(c => c.id === id);
  w.title.textContent = conv ? conv.title : "New chat";
  w.log.innerHTML = "";
  if (id != null) {
    const data = await (await api(`/api/conversations/${id}/messages`)).json();
    if (w.id !== id) return; // navigated away while loading
    for (const m of data.messages) renderMsgInto(w.log, m);
    const pending = streams.get(id);
    if (pending) { w.currentBot = bubbleIn(w.log, "bot", ""); setBubble(w.currentBot, renderMarkdown(pending.raw)); }
    unread.delete(id); updateUnreadUi();
  }
  w.stop.hidden = !(id != null && streams.has(id));
  w.input.focus();
  saveChatWins(); renderDock();
}

/** @param {ChatWin} w */
function renderWinConvList(w) {
  const box = w.convlist; box.innerHTML = "";
  if (conversations.length === 0) {
    const none = document.createElement("div");
    none.className = "conv"; none.style.color = "var(--dim)"; none.textContent = "No conversations yet";
    box.appendChild(none); return;
  }
  for (const conv of conversations) {
    const el = document.createElement("div");
    el.className = "conv" + (conv.id === w.id ? " active" : "");
    if (unread.has(conv.id)) { const d = document.createElement("span"); d.className = "unreaddot"; el.appendChild(d); }
    const label = document.createElement("span"); label.textContent = conv.title || "Untitled";
    const del = document.createElement("span"); del.className = "del"; del.textContent = "✕";
    del.onclick = async (e) => {
      e.stopPropagation();
      if (!confirm(`Delete "${conv.title}"?`)) return;
      await api(`/api/conversations/${conv.id}`, { method: "DELETE" });
      conversations = conversations.filter(c => c.id !== conv.id);
      for (const cw of chatWins.values()) if (cw.id === conv.id) setConv(cw, null);
      if (activeConversation === conv.id) newConversation();
      renderConvList(); renderWinConvList(w);
    };
    el.append(label, del);
    el.onclick = () => { box.classList.remove("open"); setConv(w, conv.id); };
    box.appendChild(el);
  }
}

// Route one streaming event into a chat window showing that conversation.
/** @param {ChatWin} w @param {WsEvent} ev */
function chatWinApply(w, ev) {
  if (ev.conversation_id == null || w.id !== ev.conversation_id) return;
  switch (ev.type) {
    case "token":
      if (!w.currentBot) w.currentBot = bubbleIn(w.log, "bot", "");
      setBubble(w.currentBot, renderMarkdown(streams.get(ev.conversation_id)?.raw ?? ""));
      w.stop.hidden = false; break;
    case "tool":
      w.status.textContent = `⚙ ${ev.name}…`; w.stop.hidden = false; break;
    case "done":
      w.currentBot = null; w.status.textContent = ""; w.stop.hidden = true; break;
    case "error":
      bubbleIn(w.log, "errmsg", ev.message ?? ""); w.currentBot = null;
      w.status.textContent = ""; w.stop.hidden = true; break;
  }
}

// Open a new chat window. `convId` is what it starts on (null = a fresh chat);
// it can navigate anywhere afterwards via its own ☰ switcher.
/** @param {number|null} convId @param {{x:number,y:number}=} pos @param {Partial<ChatWinGeom>=} saved */
async function openChatWindow(convId, pos, saved) {
  if (isMobile()) { // phones: one fullscreen chat, no windows
    if (convId != null) await openConversation(convId);
    summonChat();
    return;
  }
  saved = saved || {};
  const win = document.createElement("div");
  win.className = "window chatwin";
  const W = saved.w ?? 380, H = saved.h ?? 460;
  const x = saved.x ?? (pos ? Math.max(8, Math.min(pos.x - W / 2, innerWidth - W - 8)) : 100 + chatWins.size * 40);
  const y = saved.y ?? (pos ? Math.max(8, Math.min(pos.y - 20, innerHeight - H - 8)) : 80 + chatWins.size * 40);
  win.style.cssText = `left:${x}px; top:${y}px; width:${W}px; height:${H}px; z-index:${++zCounter};`;
  win.innerHTML = `
    <div class="titlebar">
      <button class="wconv" title="Conversations">☰</button>
      <span class="tname"></span>
      <button class="wnew" title="New conversation">＋</button>
      <button class="min" title="Minimize">–</button>
      <button class="max" title="Maximize">▢</button>
      <button class="close" title="Close">✕</button>
    </div>
    <div class="chatconvlist"></div>
    <div class="chatlog"></div>
    <button class="scrollbtn" title="Jump to latest">↓</button>
    <div class="chatstatus"></div>
    <form class="chatcompose">
      <input placeholder="Ask liquid…" autocomplete="off">
      <button type="button" class="stop" hidden title="Stop">⏹</button>
      <button type="submit" class="send">Send</button>
    </form>`;
  $("shell").appendChild(win);
  /** @type {ChatWin} */
  const w = {
    el: win, wid: ++chatWinSeq, id: null, currentBot: null,
    log: /** @type {HTMLElement} */ (win.querySelector(".chatlog")),
    input: /** @type {HTMLInputElement} */ (win.querySelector("input")),
    send: /** @type {HTMLButtonElement} */ (win.querySelector(".send")),
    stop: /** @type {HTMLButtonElement} */ (win.querySelector(".stop")),
    status: /** @type {HTMLElement} */ (win.querySelector(".chatstatus")),
    title: /** @type {HTMLElement} */ (win.querySelector(".tname")),
    convlist: /** @type {HTMLElement} */ (win.querySelector(".chatconvlist")),
    geom: { x, y, w: W, h: H },
  };
  chatWins.set(w.wid, w);
  const scrollBtn = /** @type {HTMLElement} */ (win.querySelector(".scrollbtn"));
  scrollBtns.set(w.log, scrollBtn);
  w.log.addEventListener("scroll", () => syncScrollBtn(w.log));
  scrollBtn.onclick = () => { w.log.scrollTop = w.log.scrollHeight; syncScrollBtn(w.log); };
  if (saved.maximized) win.classList.add("maximized");
  if (saved.minimized) win.classList.add("minimized");
  w.send.disabled = !(ws && ws.readyState === WebSocket.OPEN);

  win.addEventListener("pointerdown", () => bringToFront(win));
  /** @type {HTMLElement} */ (win.querySelector(".wconv")).onclick = (e) => { e.stopPropagation(); renderWinConvList(w); w.convlist.classList.toggle("open"); };
  /** @type {HTMLElement} */ (win.querySelector(".wnew")).onclick = (e) => { e.stopPropagation(); w.convlist.classList.remove("open"); setConv(w, null); };
  /** @type {HTMLElement} */ (win.querySelector(".min")).onclick = (e) => { e.stopPropagation(); minimizeWindow(win); };
  /** @type {HTMLElement} */ (win.querySelector(".max")).onclick = (e) => { e.stopPropagation(); toggleMaximize(win); };
  /** @type {HTMLElement} */ (win.querySelector(".close")).onclick = (e) => {
    e.stopPropagation();
    win.remove(); chatWins.delete(w.wid);
    const pi = pendingNewChats.indexOf(w); if (pi >= 0) pendingNewChats.splice(pi, 1);
    saveChatWins(); renderDock();
  };
  /** @type {HTMLFormElement} */ (win.querySelector("form")).onsubmit = (e) => {
    e.preventDefault();
    const content = w.input.value.trim();
    if (!content || !ws || ws.readyState !== WebSocket.OPEN) return;
    bubbleIn(w.log, "user", content);
    w.currentBot = null;
    if (w.id == null && !pendingNewChats.includes(w)) pendingNewChats.push(w);
    ws.send(JSON.stringify({ type: "user_message", content, conversation_id: w.id }));
    w.input.value = "";
  };
  w.stop.onclick = () => {
    if (ws && ws.readyState === WebSocket.OPEN && w.id != null) {
      ws.send(JSON.stringify({ type: "stop", conversation_id: w.id }));
      w.status.textContent = "stopping…";
    }
  };
  enableWinDrag(win, saveChatWins);
  new ResizeObserver(() => { if (chatWins.has(w.wid)) saveChatWins(); }).observe(win);

  await setConv(w, convId ?? null);
  bringToFront(win);
  renderDock();
}

/* ---------- mobile app view ---------- */
/** @param {App} app */
function openMobileApp(app) {
  $("mobiletitle").textContent = `${app.icon} ${app.name}`;
  $if("mobileframe").src = `/app/${encodeURIComponent(app.id)}/`;
  $("mobileapp").classList.add("open");
  document.body.classList.add("app-open"); // hides the FAB; chat moves to the top bar
}
function closeMobileApp() {
  $("mobileapp").classList.remove("open");
  document.body.classList.remove("app-open");
  $if("mobileframe").src = "about:blank";
}
$("mobileback").onclick = closeMobileApp;
$("mobilereload").onclick = () => { $if("mobileframe").src = $if("mobileframe").src; };
$("mobilechat").onclick = () => summonChat(); // reach liquid without leaving the app

/* ---------- chat ---------- */
/** @type {Conversation[]} */
let conversations = [];
/** @type {number | null} */
let activeConversation = null;
/** @type {HTMLElement | null} */
let currentBot = null;
// conversationId -> { raw } : assistant text still streaming, kept in memory
// independent of which conversation is on screen (the server only persists a
// reply on "done", so this is what survives switching chats mid-stream).
/** @type {Map<number, {raw:string}>} */
const streams = new Map();
/** @type {WebSocket | null} */
let ws = null;

async function loadConversations() {
  conversations = (await (await api("/api/conversations")).json()).conversations;
  if (conversations[0]) await openConversation(conversations[0].id);
}
function renderConvList() {
  const box = $("convlist");
  box.innerHTML = "";
  for (const conv of conversations) {
    const el = document.createElement("div");
    el.className = "conv" + (conv.id === activeConversation ? " active" : "");
    const label = document.createElement("span");
    label.textContent = conv.title || "Untitled";
    if (unread.has(conv.id)) {
      const dot = document.createElement("span");
      dot.className = "unreaddot";
      el.appendChild(dot);
    }
    const del = document.createElement("span");
    del.className = "del"; del.textContent = "✕";
    del.onclick = async (e) => {
      e.stopPropagation();
      if (!confirm(`Delete "${conv.title}"?`)) return;
      await api(`/api/conversations/${conv.id}`, { method: "DELETE" });
      conversations = conversations.filter(c => c.id !== conv.id);
      if (activeConversation === conv.id) newConversation();
      renderConvList();
    };
    el.append(label, del);
    el.onclick = async () => { $("convlist").classList.remove("open"); await openConversation(conv.id); };
    box.appendChild(el);
  }
}
/** @param {string} cls @param {string} text @param {boolean} [asMarkdown] @param {number} [ts] */
function bubble(cls, text, asMarkdown = false, ts) { return bubbleIn($("log"), cls, text, asMarkdown, ts); }
/** @param {number} id */
async function openConversation(id) {
  activeConversation = id; currentBot = null;
  unread.delete(id); updateUnreadUi();
  const conv = conversations.find(c => c.id === id);
  $("convtitle").textContent = conv ? conv.title : "liquid";
  renderConvList();
  const data = await (await api(`/api/conversations/${id}/messages`)).json();
  if (activeConversation !== id) return; // a newer switch superseded this load
  $("log").innerHTML = "";
  for (const m of data.messages) renderMsgInto($("log"), m);
  // Restore a reply that's still streaming into this conversation.
  const pending = streams.get(id);
  if (pending) {
    currentBot = bubble("bot", "");
    setBubble(currentBot, renderMarkdown(pending.raw));
  }
  setStreaming(!!pending);
}
function newConversation() {
  activeConversation = null; currentBot = null;
  $("log").innerHTML = "";
  $("convtitle").textContent = "New conversation";
  renderConvList();
  setStreaming(false);
  $("input").focus();
}
$("newchat").onclick = newConversation; // new conversation in the panel
$("newwin").onclick = () => { if (!isMobile()) openChatWindow(activeConversation); }; // pop out / new window
$("convtoggle").onclick = () => { renderConvList(); $("convlist").classList.toggle("open"); };
scrollBtns.set($("log"), $("scrolldown"));
$("log").addEventListener("scroll", () => syncScrollBtn($("log")));
$("scrolldown").onclick = () => { $("log").scrollTop = $("log").scrollHeight; syncScrollBtn($("log")); };

/* ---- summonable chat: double-tap the desk and liquid appears there ---- */
/** @param {number} [x] @param {number} [y] */
function summonChat(x, y) {
  const chat = $("chat");
  chat.classList.add("open");
  document.body.classList.add("chat-open");
  if (activeConversation !== null) { unread.delete(activeConversation); updateUnreadUi(); renderConvList(); }
  if (!isMobile()) {
    if (typeof x === "number" && typeof y === "number") {
      const w = chat.offsetWidth, h = chat.offsetHeight;
      const px = Math.max(8, Math.min(x - w / 2, innerWidth - w - 8));
      const py = Math.max(8, Math.min(y - 20, innerHeight - h - 8));
      chat.style.left = px + "px"; chat.style.top = py + "px";
      layout.chat = { x: px, y: py };
      scheduleLayoutSave();
    } else if (layout.chat) {
      chat.style.left = layout.chat.x + "px"; chat.style.top = layout.chat.y + "px";
    } else { // default: bottom-right, where the bubble was
      chat.style.left = Math.max(8, innerWidth - chat.offsetWidth - 16) + "px";
      chat.style.top = Math.max(8, innerHeight - chat.offsetHeight - 16) + "px";
    }
  }
  adjustChatForKeyboard();
  $("input").focus();
}
function closeChat() {
  $("chat").classList.remove("open");
  document.body.classList.remove("chat-open");
}
$("chatclose").onclick = closeChat;
$("chatfab").onclick = () => summonChat();
$("home").addEventListener("dblclick", (e) => {
  if (/** @type {Element|null} */(e.target)?.closest(".appicon")) return; // empty desk only
  if (isMobile()) summonChat(e.clientX, e.clientY);
  else openChatWindow(null, { x: e.clientX, y: e.clientY }); // a new chat window here
});
addEventListener("keydown", (e) => {
  // command palette: ⌘K / Ctrl+K
  if ((e.metaKey || e.ctrlKey) && (e.key === "k" || e.key === "K")) { e.preventDefault(); openPalette(); return; }
  // window switcher: Ctrl+` (Alt/Ctrl+Tab belong to the OS/browser)
  if (e.key === "`" && e.ctrlKey) { e.preventDefault(); switcher ? cycleSwitcher() : openSwitcher(); return; }
  if (switcher) {
    if (e.key === "Escape") { e.preventDefault(); cancelSwitcher(); return; }
    if (e.key === "Enter") { e.preventDefault(); commitSwitcher(); return; }
  }
  // tiling/actions on the focused window: Ctrl+Alt+Arrows / W / T
  if (e.ctrlKey && e.altKey) {
    if (e.key === "t" || e.key === "T") { e.preventDefault(); tidyWindows(); return; }
    const fw = focusedWindow();
    if (fw) {
      if (e.key === "ArrowUp") { e.preventDefault(); toggleMaximize(fw); return; }
      if (e.key === "ArrowDown") { e.preventDefault(); minimizeWindow(fw); return; }
      if (e.key === "w" || e.key === "W") { e.preventDefault(); closeWindow(fw); return; }
      if (e.key === "ArrowLeft") { e.preventDefault(); snapHalf(fw, true); return; }
      if (e.key === "ArrowRight") { e.preventDefault(); snapHalf(fw, false); return; }
    }
  }
  if (e.key === "Escape") { closeChat(); $("convlist").classList.remove("open"); hideAppMenu(); }
});
addEventListener("keyup", (e) => { if (switcher && !e.ctrlKey) commitSwitcher(); });

// drag the chat panel by its header (desktop)
/** @type {HTMLElement} */ ($("chat").querySelector("header")).addEventListener("pointerdown", (e) => {
  if (isMobile() || /** @type {Element|null} */(e.target)?.tagName === "BUTTON") return;
  e.preventDefault();
  const chat = $("chat");
  const startX = e.clientX - chat.offsetLeft;
  const startY = e.clientY - chat.offsetTop;
  const move = (/** @type {PointerEvent} */ ev) => {
    chat.style.left = Math.max(0, Math.min(ev.clientX - startX, innerWidth - 80)) + "px";
    chat.style.top  = Math.max(0, Math.min(ev.clientY - startY, innerHeight - 60)) + "px";
  };
  const up = () => {
    removeEventListener("pointermove", move);
    removeEventListener("pointerup", up);
    layout.chat = { x: chat.offsetLeft, y: chat.offsetTop };
    scheduleLayoutSave();
  };
  addEventListener("pointermove", move);
  addEventListener("pointerup", up);
});

// keep the composer above the on-screen keyboard (mobile)
function adjustChatForKeyboard() {
  const chat = $("chat");
  if (!window.visualViewport || !isMobile() || !chat.classList.contains("open")) {
    chat.style.height = ""; return;
  }
  chat.style.height = window.visualViewport.height + "px";
}
if (window.visualViewport) {
  window.visualViewport.addEventListener("resize", adjustChatForKeyboard);
}

function connect() {
  const proto = location.protocol === "https:" ? "wss" : "ws";
  ws = new WebSocket(`${proto}://${location.host}/ws?token=${encodeURIComponent(token ?? "")}`);
  ws.onopen = () => { $in("send").disabled = false; for (const w of chatWins.values()) w.send.disabled = false; };
  ws.onclose = () => { $in("send").disabled = true; for (const w of chatWins.values()) w.send.disabled = true; setTimeout(connect, RECONNECT_DELAY_MS); };
  ws.onmessage = (raw) => {
    /** @type {WsEvent | null} */
    let ev = null;
    try { ev = JSON.parse(raw.data); } catch { return; }
    if (!ev) return;
    if (ev.type === "apps_changed") {
      const before = new Set(apps.map(a => a.id));
      apps = ev.apps ?? [];
      renderGrid();
      const fresh = apps.filter(a => !before.has(a.id));
      for (const app of fresh) { toast(`${app.icon} ${app.name} is on your home screen`); addNotification(`${app.icon} ${app.name}`, "Added to your home screen"); }
      // drop windows of apps that no longer exist
      for (const [id, win] of openWindows) {
        if (!apps.some(a => a.id === id)) { win.remove(); openWindows.delete(id); }
      }
      renderDock();
      return;
    }
    if (ev.type === "shell_command") {
      if (ev.action === "open_app") {
        const app = apps.find(a => a.id === ev.app);
        if (app) { openApp(app.id, true); toast(`liquid opened ${app.icon} ${app.name}`); }
      }
      return;
    }
    if (ev.type === "notify") {
      toast(`🔔 ${ev.title}: ${ev.body}`);
      addNotification(ev.title ?? "", ev.body);
      return;
    }
    if (ev.type === "pipeline") {
      renderPipeline(ev.status ?? null);
      return;
    }
    if (ev.type === "agent_busy") {
      // The single agent worker serializes queries. Show what it's on when
      // it's NOT the conversation you're looking at, so chat never seems dead.
      if (ev.busy && ev.conversation_id !== activeConversation) {
        busyOn = ev.title || "another task";
      } else if (!ev.busy) {
        busyOn = null;
      }
      updateBusyIndicator();
      return;
    }
    // Everything below is per-conversation and always carries a conversation id.
    if (ev.conversation_id == null) return;
    if (ev.type === "conversation_created") {
      conversations.unshift({ id: ev.conversation_id, title: ev.title ?? "" });
      // If a chat window started this conversation, bind it (takes priority
      // over the primary panel adopting it).
      const wnew = pendingNewChats.shift();
      if (wnew) {
        wnew.id = ev.conversation_id;
        wnew.title.textContent = ev.title ?? "";
        saveChatWins(); renderDock();
      } else if (activeConversation === null) {
        activeConversation = ev.conversation_id;
        $("convtitle").textContent = ev.title ?? "";
      }
      renderConvList();
      return;
    }
    if (ev.type === "done" || ev.type === "error") {
      // Mark unread only if no visible surface is showing this conversation.
      const shownPrimary = activeConversation === ev.conversation_id && $("chat").classList.contains("open");
      const shownWindow = [...chatWins.values()].some(w => w.id === ev.conversation_id && !w.el.classList.contains("minimized"));
      if (!shownPrimary && !shownWindow) { unread.add(ev.conversation_id); updateUnreadUi(); renderConvList(); }
    }
    // Buffer streaming assistant text per-conversation, on screen or not — the
    // reply isn't persisted server-side until "done", so this is what survives
    // switching chats mid-stream. openConversation() restores from here.
    if (ev.type === "token") {
      const s = streams.get(ev.conversation_id) ?? { raw: "" };
      s.raw += ev.text;
      streams.set(ev.conversation_id, s);
    } else if (ev.type === "done" || ev.type === "error") {
      streams.delete(ev.conversation_id);
    }
    // Fan the event out to any chat windows bound to this conversation.
    for (const w of chatWins.values()) chatWinApply(w, ev);
    if (ev.conversation_id !== activeConversation) return;
    switch (ev.type) {
      case "token": {
        if (!currentBot) currentBot = bubble("bot", "");
        setBubble(currentBot, renderMarkdown(streams.get(ev.conversation_id)?.raw ?? ""));
        setStreaming(true);
        break;
      }
      case "tool":
        $("status").textContent = `⚙ ${ev.name}…`;
        $("chatfab").classList.add("busy");
        setStreaming(true);
        break;
      case "done":
        currentBot = null; $("status").textContent = "";
        $("chatfab").classList.remove("busy");
        setStreaming(false);
        break;
      case "error":
        bubble("errmsg", ev.message ?? ""); currentBot = null;
        $("status").textContent = ""; $("chatfab").classList.remove("busy");
        setStreaming(false);
        break;
    }
  };
}

/** @param {boolean} active */
function setStreaming(active) {
  $("stopbtn").hidden = !active;
}
$("stopbtn").onclick = () => {
  if (ws && ws.readyState === WebSocket.OPEN && activeConversation !== null) {
    ws.send(JSON.stringify({ type: "stop", conversation_id: activeConversation }));
    $("status").textContent = "stopping…";
  }
};
$("composer").onsubmit = (e) => {
  e.preventDefault();
  const content = $in("input").value.trim();
  if (!content || !ws || ws.readyState !== WebSocket.OPEN) return;
  bubble("user", content);
  currentBot = null;
  // Queries serialize; if liquid is mid-task, say so instead of looking dead.
  if (busyOn) $("status").textContent = `queued — liquid is finishing ${busyOn}`;
  ws.send(JSON.stringify({ type:"user_message", content, conversation_id: activeConversation }));
  $in("input").value = "";
};

/* ---------- settings / control panel ---------- */
const WALLPAPERS = {
  midnight: "#101014", slate: "#15151c",
  aurora: "linear-gradient(160deg,#0f1020,#161a2e 60%,#1a2233)",
  dusk: "linear-gradient(160deg,#141018,#1e1622 60%,#241a26)",
  forest: "linear-gradient(160deg,#0e1512,#12201a)",
};
const ACCENTS = ["#3d5296", "#7a5cc0", "#3f8f6b", "#b5683e", "#b03e5e", "#3f7fb0"];
function applyAppearance() {
  const a = /** @type {Appearance} */ (layout.appearance ?? {});
  document.documentElement.style.setProperty("--accent", a.accent || "#3d5296");
  const wp = /** @type {keyof typeof WALLPAPERS} */ (a.wallpaper && a.wallpaper in WALLPAPERS ? a.wallpaper : "midnight");
  document.body.style.background = WALLPAPERS[wp];
}
function initAppearanceControls() {
  const cur = /** @type {Appearance} */ (layout.appearance ?? {});
  const box = $("accent-swatches"); box.innerHTML = "";
  for (const c of ACCENTS) {
    const b = document.createElement("button");
    b.style.background = c;
    if ((cur.accent || "#3d5296") === c) b.classList.add("sel");
    b.onclick = () => setAppearance({ accent: c });
    box.appendChild(b);
  }
  $in("wallpaper-select").value = cur.wallpaper || "midnight";
}
/** @param {Appearance} patch */
function setAppearance(patch) {
  layout.appearance = Object.assign({}, layout.appearance, patch);
  applyAppearance(); scheduleLayoutSave(); initAppearanceControls();
}
$("wallpaper-select").onchange = () => setAppearance({ wallpaper: $in("wallpaper-select").value });

async function loadSettings() {
  const m = $("model-msg"); m.textContent = ""; m.className = "msg";
  initAppearanceControls();
  try {
    const s = await (await api("/api/settings")).json();
    $in("model-select").value = s.model || "default";
  } catch {}
}
$("model-select").onchange = async () => {
  const m = $("model-msg"); m.className = "msg"; m.textContent = "Saving…";
  try {
    const r = await api("/api/settings", { method:"PUT", body: JSON.stringify({ model: $in("model-select").value }) });
    if (r.ok) { m.className = "msg ok"; m.textContent = "Saved."; }
    else { m.className = "msg err"; m.textContent = "Could not save."; }
  } catch { m.className = "msg err"; m.textContent = "Could not save."; }
};
function closeSettings() {
  $("panelbg").classList.remove("open");
  /** @type {HTMLFormElement} */ ($("pw-form")).reset();
  const m = $("pw-msg"); m.textContent = ""; m.className = "msg";
  const mm = $("model-msg"); mm.textContent = ""; mm.className = "msg";
}
$("settingsbtn").onclick = () => { $("panelbg").classList.add("open"); loadSettings(); };
$("panelclose").onclick = closeSettings;
$("panelbg").onclick = (e) => { if (e.target === $("panelbg")) closeSettings(); };
$("pw-form").onsubmit = async (e) => {
  e.preventDefault();
  const msg = $("pw-msg"); msg.className = "msg";
  const oldp = $in("pw-old").value, np = $in("pw-new").value, np2 = $in("pw-new2").value;
  if (np.length < 8) { msg.className = "msg err"; msg.textContent = "New password must be at least 8 characters."; return; }
  if (np !== np2) { msg.className = "msg err"; msg.textContent = "New passwords don’t match."; return; }
  // 403 (wrong current password) must not trip api()'s 401 sign-out path.
  const r = await api("/api/auth/change_password", { method:"POST",
    body: JSON.stringify({ old_password: oldp, new_password: np }) });
  if (!r.ok) {
    const b = await r.json().catch(() => ({}));
    msg.className = "msg err"; msg.textContent = b.error || "Could not change password."; return;
  }
  const b = await r.json();
  token = b.token; localStorage.setItem("liquid_token", b.token);
  /** @type {HTMLFormElement} */ ($("pw-form")).reset();
  msg.className = "msg ok"; msg.textContent = "Password changed. Other devices were signed out.";
};

boot();
export {};
