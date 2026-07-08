// StrongLifts backend — the durable, queryable source of truth.
//
// The frontend works offline from a localStorage cache and queues finished
// workouts; this backend owns the real data in SQLite (bun:sqlite, zero deps).
// Config (units, weights, settings) is a small last-write-wins blob; the
// *history* is proper rows — one per set — so analytics are real SQL
// aggregates (PRs, estimated 1RM, progression, volume) rather than hand-rolled
// loops over a JSON blob.
//
// The supervisor proxies /app/stronglifts/api/* here with the prefix stripped,
// so this process sees /config, /workouts, /analytics, /health. It runs with
// cwd = the app dir, so data/app.db lives in the (gitignored, persisted) data/.
import { Database } from "bun:sqlite";

const db = new Database("data/app.db", { create: true });
db.exec("PRAGMA journal_mode = WAL;");
db.exec("PRAGMA foreign_keys = ON;");
db.run(`CREATE TABLE IF NOT EXISTS config (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  json TEXT NOT NULL,
  updated_at INTEGER NOT NULL
)`);
db.run(`CREATE TABLE IF NOT EXISTS workouts (
  id   TEXT PRIMARY KEY,          -- client-generated; makes re-sync idempotent
  date INTEGER NOT NULL,
  ab   TEXT NOT NULL,
  units TEXT NOT NULL
)`);
db.run(`CREATE TABLE IF NOT EXISTS sets (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  workout_id TEXT NOT NULL REFERENCES workouts(id) ON DELETE CASCADE,
  lift TEXT NOT NULL,
  name TEXT NOT NULL,
  set_index INTEGER NOT NULL,
  weight REAL NOT NULL,
  reps INTEGER NOT NULL           -- logged reps only (0 = attempted & missed)
)`);
db.run(`CREATE INDEX IF NOT EXISTS idx_sets_workout ON sets(workout_id)`);
db.run(`CREATE INDEX IF NOT EXISTS idx_sets_lift ON sets(lift)`);

// The program, mirrored here so "was this a clean 5×5?" is a server-side fact.
const LIFT_SETS: Record<string, number> = { squat: 5, bench: 5, row: 5, ohp: 5, deadlift: 1 };
const LIFT_ORDER = ["squat", "bench", "row", "ohp", "deadlift"] as const;

// ---- shapes coming off the wire (validated defensively, no deps) ----
type WireSet = number | null;
type WireLift = { key: string; name: string; weight: number; reps: WireSet[] };
type WireWorkout = { id: string; date: number; ab: string; units: string; lifts: WireLift[] };

function badRequest(msg: string): never {
  throw new Response(JSON.stringify({ error: msg }), { status: 400, headers: json });
}
function asWorkout(b: unknown): WireWorkout {
  if (typeof b !== "object" || b === null) badRequest("body must be an object");
  const o = b as Record<string, unknown>;
  if (typeof o.id !== "string" || !o.id) badRequest("id required");
  if (typeof o.date !== "number") badRequest("date required");
  if (o.ab !== "A" && o.ab !== "B") badRequest("ab must be A or B");
  if (typeof o.units !== "string") badRequest("units required");
  if (!Array.isArray(o.lifts)) badRequest("lifts required");
  const lifts = o.lifts.map((l): WireLift => {
    const x = l as Record<string, unknown>;
    if (typeof x.key !== "string" || typeof x.name !== "string" || typeof x.weight !== "number" || !Array.isArray(x.reps))
      badRequest("bad lift");
    return {
      key: x.key, name: x.name, weight: x.weight,
      reps: (x.reps as unknown[]).map((r) => (r === null ? null : Number(r))),
    };
  });
  return { id: o.id, date: o.date, ab: o.ab, units: o.units, lifts };
}

// ---- writes ----
const insertWorkout = db.query(`INSERT OR IGNORE INTO workouts (id, date, ab, units) VALUES (?, ?, ?, ?)`);
const insertSet = db.query(`INSERT INTO sets (workout_id, lift, name, set_index, weight, reps) VALUES (?, ?, ?, ?, ?, ?)`);
const saveWorkout = db.transaction((w: WireWorkout): boolean => {
  const res = insertWorkout.run(w.id, w.date, w.ab, w.units);
  if (res.changes === 0) return false; // already stored — idempotent re-sync
  for (const l of w.lifts)
    l.reps.forEach((r, i) => { if (r !== null) insertSet.run(w.id, l.key, l.name, i, l.weight, r); });
  return true;
});

// ---- reads: recent history (for the offline cache) ----
function recentWorkouts(limit: number): WireWorkout[] {
  const rows = db.query(`SELECT id, date, ab, units FROM workouts ORDER BY date DESC LIMIT ?`)
    .all(limit) as { id: string; date: number; ab: string; units: string }[];
  const setsFor = db.query(`SELECT lift, name, weight, reps FROM sets WHERE workout_id = ? ORDER BY set_index`);
  return rows.map((w) => {
    const rows2 = setsFor.all(w.id) as { lift: string; name: string; weight: number; reps: number }[];
    const byLift = new Map<string, WireLift>();
    for (const s of rows2) {
      let l = byLift.get(s.lift);
      if (!l) { l = { key: s.lift, name: s.name, weight: s.weight, reps: [] }; byLift.set(s.lift, l); }
      l.reps.push(s.reps);
    }
    return { id: w.id, date: w.date, ab: w.ab as "A" | "B", units: w.units, lifts: [...byLift.values()] };
  });
}

// ---- reads: analytics, as SQL aggregates ----
function analytics() {
  const lifts: Record<string, {
    current: number | null; best5x5: number | null; e1rm: number | null;
    sets: number; series: { date: number; weight: number }[];
  }> = {};
  const series = db.query(
    `SELECT w.date AS date, MAX(s.weight) AS weight
       FROM sets s JOIN workouts w ON w.id = s.workout_id
      WHERE s.lift = ? GROUP BY w.id ORDER BY w.date`);
  const best = db.query(
    `SELECT MAX(top) AS best FROM (
       SELECT MAX(weight) AS top, MIN(reps) AS lo, COUNT(*) AS c
         FROM sets WHERE lift = ? GROUP BY workout_id
     ) WHERE lo >= 5 AND c >= ?`);
  const e1 = db.query(`SELECT MAX(weight * (1.0 + reps / 30.0)) AS e FROM sets WHERE lift = ? AND reps >= 1`);
  const cnt = db.query(`SELECT COUNT(*) AS n FROM sets WHERE lift = ?`);
  for (const key of LIFT_ORDER) {
    const s = series.all(key) as { date: number; weight: number }[];
    const b = best.get(key, LIFT_SETS[key] ?? 5) as { best: number | null };
    const e = e1.get(key) as { e: number | null };
    const c = cnt.get(key) as { n: number };
    lifts[key] = {
      current: s.length ? (s[s.length - 1]?.weight ?? null) : null,
      best5x5: b?.best ?? null,
      e1rm: e?.e != null ? Math.round(e.e * 10) / 10 : null,
      sets: c?.n ?? 0,
      series: s,
    };
  }
  const volume = db.query(
    `SELECT w.date AS date, w.ab AS ab, SUM(s.weight * s.reps) AS volume
       FROM workouts w JOIN sets s ON s.workout_id = w.id
      GROUP BY w.id ORDER BY w.date`).all() as { date: number; ab: string; volume: number }[];
  const weekAgo = Date.now() - 7 * 864e5;
  const totals = db.query(`SELECT COUNT(*) AS n, MIN(date) AS first FROM workouts`).get() as { n: number; first: number | null };
  const thisWeek = (db.query(`SELECT COUNT(*) AS n FROM workouts WHERE date >= ?`).get(weekAgo) as { n: number }).n;
  return { lifts, volume, totals: { workouts: totals.n, first: totals.first, thisWeek } };
}

// ---- config: a single last-write-wins blob ----
const getConfig = db.query(`SELECT json, updated_at FROM config WHERE id = 1`);
const putConfig = db.query(
  `INSERT INTO config (id, json, updated_at) VALUES (1, ?, ?)
     ON CONFLICT(id) DO UPDATE SET json = excluded.json, updated_at = excluded.updated_at
      WHERE excluded.updated_at >= config.updated_at`);

const json = { "Content-Type": "application/json" } as const;
const ok = (body: unknown) => new Response(JSON.stringify(body), { headers: json });

const port = Number(Bun.env.PORT) || 8787;
Bun.serve({
  port,
  async fetch(req) {
    const url = new URL(req.url);
    const path = url.pathname.replace(/\/+$/, "") || "/";
    try {
      if (req.method === "GET" && path === "/health") return ok({ ok: true });

      if (req.method === "GET" && path === "/config") {
        const row = getConfig.get() as { json: string; updated_at: number } | null;
        return ok(row ? { json: JSON.parse(row.json), updatedAt: row.updated_at } : { json: null, updatedAt: 0 });
      }
      if (req.method === "PUT" && path === "/config") {
        const b = (await req.json()) as { json?: unknown; updatedAt?: unknown };
        if (typeof b?.updatedAt !== "number" || typeof b?.json !== "object" || b.json === null) badRequest("json + updatedAt required");
        putConfig.run(JSON.stringify(b.json), b.updatedAt);
        return new Response(null, { status: 204 });
      }

      if (req.method === "POST" && path === "/workouts") {
        const created = saveWorkout(asWorkout(await req.json()));
        return ok({ created });
      }
      if (req.method === "GET" && path === "/workouts") {
        const limit = Math.min(400, Math.max(1, Number(url.searchParams.get("limit")) || 50));
        return ok(recentWorkouts(limit));
      }

      if (req.method === "GET" && path === "/analytics") return ok(analytics());

      if (req.method === "DELETE" && path === "/all") {
        db.transaction(() => { db.run("DELETE FROM sets"); db.run("DELETE FROM workouts"); db.run("DELETE FROM config"); })();
        return new Response(null, { status: 204 });
      }

      return new Response("not found", { status: 404 });
    } catch (e) {
      if (e instanceof Response) return e; // badRequest throws a Response
      return new Response(JSON.stringify({ error: String(e) }), { status: 500, headers: json });
    }
  },
});
console.log(`stronglifts backend on :${port}`);
