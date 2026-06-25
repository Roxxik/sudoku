// Thin CRUD Worker for the Sudoku solve log. No domain logic now or later --
// all Sudoku logic stays in the wasm client. This handler only validates the
// shape of a solve and appends it to D1. See docs/backend-prototype.md.

// The app is served from GitHub Pages at https://roxxik.github.io/sudoku/.
// CORS matches scheme + host only (the /sudoku path is not part of the origin),
// and is locked to that one origin rather than "*". Overridable per request via
// the ALLOW_ORIGIN var (see fetch) -- local dev needs a different origin.
const ALLOW_ORIGIN = "https://roxxik.github.io";

// Cap on a stored client-error payload. The /errors endpoint is public-writable
// (same speed-bump key as /solves), so this is the lone guard against a large-blob
// dump -- a real failed batch (outbox is client-capped) sits well under it.
const MAX_ERROR_BYTES = 64 * 1024;

// Cap on one move-log timeline's serialized events. A real solve runs a few
// hundred events (tens of KB); this is the public-writable /moves endpoint's
// guard against a single oversized blob. A log over the cap fails validation
// (400s the batch) rather than truncating the timeline silently.
const MAX_LOG_BYTES = 512 * 1024;

// --- Status dashboard ---------------------------------------------------------
// The day boundary and difficulty roster mirror web/daily.js (DAILY_RESET_MS,
// DAY_MS, the dayNumber math and the DAILY_LEVELS order/names), so GET /status can
// name "today" and label the daily board exactly as the client does. Kept as a
// tiny local copy -- the worker shares no module with the frontend bundle.
const DAILY_RESET_MS = 2.5 * 60 * 60 * 1000;
const DAY_MS = 24 * 60 * 60 * 1000;
const DAILY_LEVEL_NAMES = ["Beginner", "Intermediate", "Expert I", "Expert II"];
const WEEKDAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

export default {
  async fetch(request, env) {
    const url = new URL(request.url);

    // CORS origin: the locked production origin above by default, overridden by
    // the ALLOW_ORIGIN var when set -- scripts/dev-local sets "*" so any
    // localhost/LAN origin is accepted in dev. Vary: Origin keeps caches honest.
    const allowOrigin = env.ALLOW_ORIGIN || ALLOW_ORIGIN;
    const cors = (resp) => {
      resp.headers.set("Access-Control-Allow-Origin", allowOrigin);
      resp.headers.set("Vary", "Origin");
      return resp;
    };

    // Guard the whole routing body: a thrown handler error (e.g. a D1 schema
    // drift) would otherwise escape as a bare 500 with no CORS header, which the
    // browser reports only as an opaque CORS failure. Catch it, log it, and return
    // the 500 WITH the CORS header so the real status reaches the client and the
    // stack reaches the worker log. (Indented flat -- it wraps the entire body.)
    try {
    if (request.method === "OPTIONS") {
      // CORS preflight for the cross-origin requests from Pages: the POST writes
      // and the GET that reads the daily leaderboard (the lone read path). The
      // x-api-key header makes the GET non-simple, so it preflights too; the long
      // Max-Age means that costs one round trip a day, not one per board fetch.
      return cors(new Response(null, {
        status: 204,
        headers: {
          "Access-Control-Allow-Methods": "GET, POST, OPTIONS",
          "Access-Control-Allow-Headers": "content-type, x-api-key",
          "Access-Control-Max-Age": "86400",
        },
      }));
    }

    // Status dashboard. A read-only ops view of the backend's health and recent
    // activity -- D1 reachability, per-table row counts with recency deltas and
    // last-write times, the client-error alarm (windowed over 7 DAYS, since errors
    // are rare), today's daily board, and every frontend build seen in the last
    // week. Two shapes off one collector, so they can never drift:
    //   GET /status       -> a self-contained HTML dashboard (manual refresh)
    //   GET /status.json  -> the same numbers as JSON (curl / a cron alarm)
    // Key-gated like the rest: aggregate counts plus a rejected-payload preview are
    // not public. The HTML route alone serves a key-prompt form when the key is
    // ABSENT (so it is browser-usable from a bookmark); a present-but-WRONG key
    // still 401s, so a brute-force shows up as 401s the way it does on /solves.
    if (url.pathname === "/status" || url.pathname === "/status.json") {
      if (request.method !== "GET") return cors(new Response("method", { status: 405 }));
      const key = request.headers.get("x-api-key") || url.searchParams.get("key");
      if (url.pathname === "/status" && !key) return cors(htmlResp(statusLoginHtml()));
      if (key !== env.API_KEY) return cors(new Response("unauthorized", { status: 401 }));

      const now = Date.now();
      const data = await collectStatus(env, dayNumber(now), now);
      if (url.pathname === "/status.json") {
        return cors(new Response(JSON.stringify(data, null, 2), {
          status: 200, headers: { "content-type": "application/json" },
        }));
      }
      return cors(htmlResp(renderStatus(data, key)));
    }

    // Capture endpoint for client-side upload failures. Deliberately schemaless:
    // it checks the API key and nothing else, then stores the raw body verbatim,
    // so the exact payload /solves rejected (or a solve that failed client-side
    // validation) is recoverable from the backend without inspecting the device.
    // A bad/missing key still 401s (visible in the dashboard); that is the gate.
    if (url.pathname === "/errors") {
      if (request.method !== "POST") return cors(new Response("method", { status: 405 }));
      if (request.headers.get("x-api-key") !== env.API_KEY)
        return cors(new Response("unauthorized", { status: 401 }));
      let raw = "";
      try { raw = await request.text(); } catch {}
      if (raw.length > MAX_ERROR_BYTES) raw = raw.slice(0, MAX_ERROR_BYTES);
      await env.DB.prepare("INSERT INTO client_errors (payload) VALUES (?)").bind(raw).run();
      return cors(new Response(JSON.stringify({ ok: true }), {
        status: 200, headers: { "content-type": "application/json" },
      }));
    }

    // Move-timeline sync. One row per puzzle keyed by solve_id, whole-snapshot
    // upsert: the incoming events array REPLACES the stored one, guarded so a
    // shorter (stale/out-of-order) snapshot can't shrink it. Synced periodically
    // mid-solve and on close, so abandoned puzzles land too -- a row here may have
    // no matching /solves row. Same key gate and batch shape as /solves.
    if (url.pathname === "/moves") {
      if (request.method !== "POST") return cors(new Response("method", { status: 405 }));
      if (request.headers.get("x-api-key") !== env.API_KEY)
        return cors(new Response("unauthorized", { status: 401 }));

      let mbody;
      try { mbody = await request.json(); } catch { return cors(new Response("bad json", { status: 400 })); }
      const logs = mbody && Array.isArray(mbody.logs) ? mbody.logs : null;
      if (!logs || !logs.every(validLog)) return cors(new Response("bad logs", { status: 400 }));

      // The WHERE on the conflict path is the monotonicity guard: an upsert whose
      // events array is no longer than the stored one is a no-op, so a retry that
      // races a fresher snapshot can't roll the timeline back. event_count is taken
      // from the array length (not the client field) so the guard can't be fooled
      // by a mismatched count. solved/last_t still ride the same snapshot.
      let upserted = 0;
      if (logs.length) {
        const stmt = env.DB.prepare(
          `INSERT INTO move_logs (solve_id, puzzle, seed, solved, event_count, last_t, events, client_version)
             VALUES (?,?,?,?,?,?,?,?)
           ON CONFLICT(solve_id) DO UPDATE SET
             puzzle = excluded.puzzle, seed = excluded.seed, solved = excluded.solved,
             event_count = excluded.event_count, last_t = excluded.last_t,
             events = excluded.events, client_version = excluded.client_version,
             updated_at = datetime('now')
           WHERE excluded.event_count >= move_logs.event_count`
        );
        const res = await env.DB.batch(
          logs.map((s) => stmt.bind(
            s.solve_id, s.puzzle, s.seed ?? null, s.solved ? 1 : 0,
            s.events.length, s.last_t, JSON.stringify(s.events), s.client_version
          ))
        );
        upserted = res.reduce((n, r) => n + (r.meta?.changes ?? 0), 0);
      }
      return cors(new Response(JSON.stringify({ upserted }), {
        status: 200, headers: { "content-type": "application/json" },
      }));
    }

    // Puzzle-of-the-day solve times. Written to the SEPARATE daily_solves table,
    // not `solves`: a daily solve lands here only when the player taps Submit (an
    // explicit act that bypasses the data-sharing opt-out -- see web/backend.js),
    // so it is kept apart from the passive, privacy-gated /solves feed. Same key
    // gate and batch shape as /solves; INSERT OR IGNORE on the unique solve_id
    // makes a resent submission idempotent.
    if (url.pathname === "/daily") {
      // Read path: the daily leaderboard. Two shapes, both anonymous (no identity is
      // stored -- see daily_solves):
      //   GET /daily?day=D                     -> { day, counts: { level: n } }          (overview teaser)
      //   GET /daily?day=D&level=L[&solve_id=S] -> { day, level, count, times, hints, mine } (one board)
      // Same key gate as the writes. The board ships the FULL ordered list (times +
      // the parallel hints column): the client needs the total and the rows around
      // the player's rank to render its window, and at friends-scale that is a few
      // rows, not a payload worth paginating. `mine` is the 0-based index of the
      // caller's own solve_id in that order (or -1), so the client can dedup its own
      // row when projecting a rank without ever seeing anyone else's id.
      if (request.method === "GET") {
        if (request.headers.get("x-api-key") !== env.API_KEY)
          return cors(new Response("unauthorized", { status: 401 }));
        const day = intParam(url, "day");
        if (day === null) return cors(new Response("bad day", { status: 400 }));

        const levelRaw = url.searchParams.get("level");
        if (levelRaw === null) {
          const rows = await env.DB.prepare(
            "SELECT level, COUNT(*) AS n FROM daily_solves WHERE day = ? GROUP BY level"
          ).bind(day).all();
          const counts = {};
          for (const r of rows.results) counts[r.level] = r.n;
          return cors(new Response(JSON.stringify({ day, counts }), {
            status: 200, headers: { "content-type": "application/json" },
          }));
        }

        const level = intParam(url, "level");
        if (level === null) return cors(new Response("bad level", { status: 400 }));
        // The board ranks by HINTS first, then time: a clean solve outranks any
        // hinted one, ties broken by the faster time. `hints` parallels `times` in
        // that rank order (the dual-score column). solve_id is read only to locate
        // the caller's own row index (`mine`); it is never echoed back.
        const sid = url.searchParams.get("solve_id");
        const rows = await env.DB.prepare(
          "SELECT solve_id, solve_ms, hints FROM daily_solves WHERE day = ? AND level = ? ORDER BY hints, solve_ms, created_at"
        ).bind(day, level).all();
        const times = rows.results.map((r) => r.solve_ms);
        const hints = rows.results.map((r) => r.hints);
        const mine = sid ? rows.results.findIndex((r) => r.solve_id === sid) : -1;
        return cors(new Response(JSON.stringify({ day, level, count: times.length, times, hints, mine }), {
          status: 200, headers: { "content-type": "application/json" },
        }));
      }

      if (request.method !== "POST") return cors(new Response("method", { status: 405 }));
      if (request.headers.get("x-api-key") !== env.API_KEY)
        return cors(new Response("unauthorized", { status: 401 }));

      let dbody;
      try { dbody = await request.json(); } catch { return cors(new Response("bad json", { status: 400 })); }
      const solves = dbody && Array.isArray(dbody.solves) ? dbody.solves : null;
      if (!solves || !solves.every(validDaily)) return cors(new Response("bad solves", { status: 400 }));

      let inserted = 0;
      if (solves.length) {
        const stmt = env.DB.prepare(
          "INSERT OR IGNORE INTO daily_solves (solve_id, day, level, seed, puzzle, solve_ms, hints, client_version) VALUES (?,?,?,?,?,?,?,?)"
        );
        const res = await env.DB.batch(
          solves.map((s) => stmt.bind(s.solve_id, s.day, s.level, s.seed ?? null, s.puzzle, s.solve_ms, s.hints ?? 0, s.client_version))
        );
        inserted = res.reduce((n, r) => n + (r.meta?.changes ?? 0), 0);
      }
      return cors(new Response(JSON.stringify({ inserted }), {
        status: 200, headers: { "content-type": "application/json" },
      }));
    }

    if (url.pathname !== "/solves") return cors(new Response("not found", { status: 404 }));
    if (request.method !== "POST")  return cors(new Response("method", { status: 405 }));
    if (request.headers.get("x-api-key") !== env.API_KEY)
      return cors(new Response("unauthorized", { status: 401 }));

    let body;
    try { body = await request.json(); } catch { return cors(new Response("bad json", { status: 400 })); }
    const solves = body && Array.isArray(body.solves) ? body.solves : null;
    if (!solves || !solves.every(valid)) return cors(new Response("bad solves", { status: 400 }));

    // INSERT OR IGNORE keyed on the per-solve solve_id makes a re-sent solve
    // (the deferred offline-retry case) an idempotent no-op. D1's batch() rejects
    // a zero-length list, so a validly-shaped empty request (e.g. a future
    // offline-flush with nothing queued) is a no-op rather than a batch call.
    let inserted = 0;
    if (solves.length) {
      const stmt = env.DB.prepare(
        "INSERT OR IGNORE INTO solves (solve_id, seed, puzzle, solution, solve_ms, client_version) VALUES (?,?,?,?,?,?)"
      );
      const res = await env.DB.batch(
        solves.map((s) => stmt.bind(s.solve_id, s.seed ?? null, s.puzzle, s.solution, s.solve_ms, s.client_version))
      );
      inserted = res.reduce((n, r) => n + (r.meta?.changes ?? 0), 0);
    }
    return cors(new Response(JSON.stringify({ inserted }), {
      status: 200, headers: { "content-type": "application/json" },
    }));
    } catch (e) {
      console.error("worker error:", (e && e.stack) || e);
      return cors(new Response("internal error", { status: 500 }));
    }
  },
};

// A required integer query param, or null if absent / not a base-10 integer. Used
// by the GET /daily read path to parse day and level (both small non-negative
// ints); a malformed value 400s rather than silently scanning the wrong key.
function intParam(url, name) {
  const raw = url.searchParams.get(name);
  if (raw === null || !/^-?\d+$/.test(raw)) return null;
  return Number(raw);
}

function valid(s) {
  return s && typeof s.solve_id === "string" && s.solve_id.length > 0
    && typeof s.puzzle === "string"   && s.puzzle.length === 81
    && typeof s.solution === "string" && s.solution.length === 81
    && Number.isInteger(s.solve_ms)
    && (s.seed == null || typeof s.seed === "string")
    && typeof s.client_version === "string" && s.client_version.length > 0;
}

// Shape check for a /daily entry. (day, level) name the daily puzzle (both required
// integers); the seed merely reproduces the puzzle and may be null. `hints` (dual
// score) is optional -- an older client omits it, defaulting at bind time -- but if
// present must be a non-negative integer.
function validDaily(s) {
  return s && typeof s.solve_id === "string" && s.solve_id.length > 0
    && Number.isInteger(s.day)
    && Number.isInteger(s.level)
    && typeof s.puzzle === "string"   && s.puzzle.length === 81
    && Number.isInteger(s.solve_ms)
    && (s.seed == null || typeof s.seed === "string")
    && (s.hints == null || (Number.isInteger(s.hints) && s.hints >= 0))
    && typeof s.client_version === "string" && s.client_version.length > 0;
}

// Shape check for a /moves entry. `events` is the raw timeline array (the worker
// stringifies it for storage); `solved` is coerced to 0/1 at bind time, so any
// truthy/falsy value is accepted here. event_count is recomputed server-side, so
// it isn't validated. Rejects a timeline whose serialized form exceeds the cap.
function validLog(s) {
  return s && typeof s.solve_id === "string" && s.solve_id.length > 0
    && typeof s.puzzle === "string" && s.puzzle.length === 81
    && (s.seed == null || typeof s.seed === "string")
    && Array.isArray(s.events)
    && Number.isInteger(s.last_t)
    && typeof s.client_version === "string" && s.client_version.length > 0
    && JSON.stringify(s.events).length <= MAX_LOG_BYTES;
}

// --- Status dashboard helpers -------------------------------------------------

// Puzzle-day index that ticks over at 02:30 UTC -- mirrors web/daily.js dayNumber.
function dayNumber(now) {
  return Math.floor((now - DAILY_RESET_MS) / DAY_MS);
}

// The calendar date a puzzle-day belongs to, formatted at its 02:30 UTC boundary
// (so the label is the right date in any viewer's timezone) -- mirrors web/daily.js
// dayLabel, but built from local arrays to avoid an Intl dependency in the worker.
function dayLabel(day) {
  const d = new Date(day * DAY_MS + DAILY_RESET_MS);
  return `${WEEKDAYS[d.getUTCDay()]} ${d.getUTCDate()} ${MONTHS[d.getUTCMonth()]}`;
}

// A compact "Nm ago" for a stored "YYYY-MM-DD HH:MM:SS" UTC timestamp (D1 writes
// datetime('now')), or "never" when the column is NULL (an empty table).
function ago(ts, nowMs) {
  if (!ts) return "never";
  const t = Date.parse(ts.replace(" ", "T") + "Z");
  if (Number.isNaN(t)) return ts;
  const s = Math.max(0, Math.round((nowMs - t) / 1000));
  if (s < 60) return `${s}s ago`;
  const m = Math.round(s / 60); if (m < 60) return `${m}m ago`;
  const h = Math.round(m / 60); if (h < 24) return `${h}h ago`;
  return `${Math.round(h / 24)}d ago`;
}

// Thousands-separated integer, without leaning on a locale in the worker runtime.
function fmt(n) {
  return String(n ?? 0).replace(/\B(?=(\d{3})+(?!\d))/g, ",");
}

// Minimal escaper for the few dynamic strings the page emits (version strings, the
// error preview, date labels).
function esc(s) {
  return String(s).replace(/[&<>"]/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
}

// An HTML 200 with no-store: the dashboard is always live, never cached.
function htmlResp(body) {
  return new Response(body, {
    status: 200,
    headers: { "content-type": "text/html; charset=utf-8", "cache-control": "no-store" },
  });
}

// One D1 round trip for the whole page: a SELECT 1 ping (timed for the health dot),
// per-table aggregates, the latest client error, every recent frontend build, and
// the daily board/week. `today` is the current day index; `now` the wall clock for
// the recency deltas. Returns a plain object the JSON route serializes and the HTML
// route renders, so the two views can never drift. A thrown batch (D1 down / schema
// drift) is caught and reported as a degraded health state rather than a bare 500.
async function collectStatus(env, today, now) {
  const q = (sql) => env.DB.prepare(sql);

  // total rows, rows newer than `window`, and the most recent timestamp. SUM over a
  // boolean comparison counts the recent rows; COALESCE turns the empty-table NULL
  // into 0. `window` is a SQLite datetime modifier, e.g. '-1 days' or '-7 days'.
  const recencyAgg = (table, window) =>
    `SELECT COUNT(*) AS total,
            COALESCE(SUM(created_at >= datetime('now', '${window}')), 0) AS recent,
            MAX(created_at) AS latest
       FROM ${table}`;

  const t0 = Date.now();
  let res, dbOk = true, dbErr = null;
  try {
    res = await env.DB.batch([
      q("SELECT 1 AS ok"),
      q(recencyAgg("solves", "-1 days")),
      q(recencyAgg("daily_solves", "-1 days")),
      // move_logs carries no recency delta -- split solved vs still-in-progress.
      q(`SELECT COUNT(*) AS total,
                COALESCE(SUM(solved = 1), 0) AS solved,
                MAX(updated_at) AS latest
           FROM move_logs`),
      // client_errors windowed over 7 DAYS, not a day: errors are rare, so a 24h
      // window would usually read 0 and hide a recent regression.
      q(recencyAgg("client_errors", "-7 days")),
      q(`SELECT substr(payload, 1, 240) AS preview, created_at
           FROM client_errors ORDER BY id DESC LIMIT 1`),
      // Every frontend build that reported a solve in the last 7 days (all of
      // them, not a top-N), with its first/last sighting, newest-introduced first
      // -- so the most recently deployed build sorts to the top. MIN/MAX over the
      // window date the build's appearance; first_seen is what distinguishes
      // "newer" even when two builds are both still reporting (last_seen ~ now for
      // both). The version string is a short git hash, which does NOT sort by age.
      q(`SELECT client_version AS v, COUNT(*) AS n,
                MIN(created_at) AS first_seen, MAX(created_at) AS last_seen
           FROM solves
          WHERE created_at >= datetime('now', '-7 days')
          GROUP BY client_version
          ORDER BY first_seen DESC, n DESC`),
      // Today's daily board: per-level submission counts for the current day index.
      q("SELECT level, COUNT(*) AS n FROM daily_solves WHERE day = ? GROUP BY level").bind(today),
      // Daily submissions for the last 7 day-indices (today back through today-6).
      q("SELECT day, COUNT(*) AS n FROM daily_solves WHERE day >= ? GROUP BY day ORDER BY day DESC").bind(today - 6),
    ]);
  } catch (e) {
    dbOk = false;
    dbErr = String((e && e.message) || e);
  }
  const pingMs = Date.now() - t0;

  const head = {
    ok: dbOk, nowMs: now, db: { ok: dbOk, error: dbErr, pingMs },
    today: { day: today, label: dayLabel(today) },
  };
  if (!dbOk) return head;

  const first = (r) => (r && r.results && r.results[0]) || {};
  const rows = (r) => (r && r.results) || [];

  const s = first(res[1]), dl = first(res[2]), m = first(res[3]),
        er = first(res[4]), ep = first(res[5]);
  const mTotal = m.total || 0, mSolved = m.solved || 0;
  const todayCounts = rows(res[7]);

  return {
    ...head,
    tables: {
      solves:       { total: s.total || 0,  recent: s.recent || 0,  latest: s.latest || null },
      daily_solves: { total: dl.total || 0, recent: dl.recent || 0, latest: dl.latest || null },
      move_logs:    { total: mTotal, solved: mSolved, inProgress: mTotal - mSolved, latest: m.latest || null },
      client_errors:{ total: er.total || 0, recent: er.recent || 0, latest: er.latest || null,
                      lastPreview: ep.preview || null, lastAt: ep.created_at || null },
    },
    versions: rows(res[6]).map((r) => ({
      version: r.v, count: r.n, firstSeen: r.first_seen, lastSeen: r.last_seen,
    })),
    board: DAILY_LEVEL_NAMES.map((name, i) => ({
      name, count: (todayCounts.find((r) => r.level === i) || {}).n || 0,
    })),
    week: rows(res[8]).map((r) => ({ day: r.day, label: dayLabel(r.day), count: r.n })),
  };
}

// The full HTML document shell. No <meta refresh> by design -- the page is
// refreshed manually (the header carries a refresh link); the footer's generated-at
// time is the only freshness cue needed.
function statusDoc(body) {
  return `<!doctype html><html lang="en"><head>` +
    `<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">` +
    `<meta name="robots" content="noindex"><title>Sudoku backend status</title>` +
    `<style>${STATUS_CSS}</style></head><body>${body}</body></html>`;
}

// Shown by GET /status when no key is supplied: a plain GET form that reloads as
// /status?key=... A present-but-wrong key never reaches here -- it 401s in the route.
function statusLoginHtml() {
  return statusDoc(
    `<header><h1>Sudoku backend</h1></header>` +
    `<div class="sub">Enter the API key to view the status dashboard.</div>` +
    `<form class="login" method="get" action="/status">` +
    `<input type="password" name="key" placeholder="API key" autofocus autocomplete="off">` +
    `<button type="submit">View status</button></form>`);
}

function statusHeader(d, refresh) {
  const dot = d.db.ok ? "ok" : "bad";
  const text = d.db.ok ? "Operational" : "Database error";
  return `<header><h1>Sudoku backend</h1>` +
    `<span class="status"><span class="dot ${dot}"></span>${text}</span></header>` +
    `<div class="sub">D1 ping ${d.db.pingMs} ms &middot; day ${d.today.day} ` +
    `(${esc(d.today.label)}) &middot; <a href="${refresh}">refresh</a></div>`;
}

// Render the collected status object as the HTML dashboard. `key` is echoed only
// into the refresh link (it is already in the address bar -- no new exposure).
function renderStatus(d, key) {
  const refresh = `?key=${encodeURIComponent(key)}`;
  if (!d.db.ok) {
    return statusDoc(statusHeader(d, refresh) +
      `<section class="panel err"><h2>Database error</h2>` +
      `<pre class="errpre">${esc(d.db.error || "unknown")}</pre></section>`);
  }

  const now = d.nowMs, t = d.tables;
  const errHot = t.client_errors.recent > 0;
  const line = (txt, cls = "") => `<div class="line ${cls}">${txt}</div>`;
  const card = (cls, title, big, lines) =>
    `<div class="card ${cls}"><div class="card-h">${title}</div>` +
    `<div class="big">${fmt(big)}</div>${lines.join("")}</div>`;

  const cards =
    card("", "Solves", t.solves.total, [
      line(`+${fmt(t.solves.recent)} &middot; 24h`),
      line(`last ${ago(t.solves.latest, now)}`),
    ]) +
    card("", "Daily solves", t.daily_solves.total, [
      line(`+${fmt(t.daily_solves.recent)} &middot; 24h`),
      line(`last ${ago(t.daily_solves.latest, now)}`),
    ]) +
    card("", "Move logs", t.move_logs.total, [
      line(`${fmt(t.move_logs.solved)} solved`),
      line(`${fmt(t.move_logs.inProgress)} in progress`),
      line(`last ${ago(t.move_logs.latest, now)}`),
    ]) +
    card(errHot ? "hot" : "", "Client errors", t.client_errors.total, [
      line(`+${fmt(t.client_errors.recent)} &middot; 7d`, errHot ? "bad" : ""),
      line(`last ${ago(t.client_errors.latest, now)}`),
    ]);

  const board = d.board.map((r) =>
    `<tr><td>${esc(r.name)}</td><td class="num">${fmt(r.count)}</td></tr>`).join("");

  const weekMax = Math.max(1, ...d.week.map((w) => w.count));
  const week = d.week.length
    ? d.week.map((w) =>
        `<tr><td class="day">${esc(w.label)}</td>` +
        `<td class="barcell"><span class="bar" style="width:${Math.round(w.count / weekMax * 100)}%"></span></td>` +
        `<td class="num">${fmt(w.count)}</td></tr>`).join("")
    : `<tr><td colspan="3" class="muted">no submissions</td></tr>`;

  const versionHead =
    `<tr><th>Version</th><th class="num">Solves</th>` +
    `<th class="when">First seen</th><th class="when">Last seen</th></tr>`;
  const versions = d.versions.length
    ? versionHead + d.versions.map((v) =>
        `<tr><td class="mono">${esc(v.version)}</td><td class="num">${fmt(v.count)}</td>` +
        `<td class="when">${ago(v.firstSeen, now)}</td>` +
        `<td class="when">${ago(v.lastSeen, now)}</td></tr>`).join("")
    : `<tr><td colspan="4" class="muted">none in the last 7 days</td></tr>`;

  const errPanel = t.client_errors.lastPreview
    ? `<section class="panel ${errHot ? "warn" : ""}">` +
      `<h2>Most recent client error <span class="muted">${ago(t.client_errors.lastAt, now)}</span></h2>` +
      `<pre class="errpre">${esc(t.client_errors.lastPreview)}</pre></section>`
    : "";

  return statusDoc(
    statusHeader(d, refresh) +
    `<div class="grid">${cards}</div>` +
    `<div class="panels">` +
      `<section class="panel"><h2>Today's daily board ` +
      `<span class="muted">${esc(d.today.label)}</span></h2><table>${board}</table></section>` +
      `<section class="panel"><h2>Daily submissions <span class="muted">7 days</span></h2>` +
      `<table>${week}</table></section>` +
    `</div>` +
    `<section class="panel"><h2>Client versions <span class="muted">last 7 days, newest first</span></h2>` +
    `<table>${versions}</table></section>` +
    errPanel +
    `<footer>backend worker &middot; D1 "sudoku" &middot; generated ${esc(new Date(now).toISOString())}</footer>`);
}

const STATUS_CSS = `
:root{--bg:#0d1117;--panel:#161b22;--border:#30363d;--fg:#e6edf3;--muted:#8b949e;--accent:#58a6ff;--green:#3fb950;--red:#f85149;--amber:#d29922}
*{box-sizing:border-box}
body{margin:0 auto;max-width:1000px;padding:24px;background:var(--bg);color:var(--fg);font:14px/1.5 ui-sans-serif,system-ui,-apple-system,"Segoe UI",Roboto,sans-serif}
a{color:var(--accent);text-decoration:none}
a:hover{text-decoration:underline}
.mono,.errpre{font-family:ui-monospace,SFMono-Regular,Menlo,monospace}
header{display:flex;align-items:center;justify-content:space-between;flex-wrap:wrap;gap:8px}
h1{margin:0;font-size:18px;font-weight:600}
.status{display:inline-flex;align-items:center;gap:8px;font-weight:600}
.dot{width:10px;height:10px;border-radius:50%;background:var(--muted)}
.dot.ok{background:var(--green)}
.dot.bad{background:var(--red)}
.sub{margin:4px 0 20px;color:var(--muted);font-size:13px}
.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(170px,1fr));gap:12px;margin-bottom:20px}
.card{background:var(--panel);border:1px solid var(--border);border-radius:8px;padding:14px 16px}
.card.hot{border-color:var(--red)}
.card-h{color:var(--muted);font-size:12px;text-transform:uppercase;letter-spacing:.04em;margin-bottom:6px}
.big{font-size:26px;font-weight:700;line-height:1.1;font-variant-numeric:tabular-nums}
.line{color:var(--muted);font-size:13px;margin-top:3px}
.line.bad{color:var(--red)}
.panels{display:grid;grid-template-columns:1fr 1fr;gap:12px}
@media(max-width:680px){.panels{grid-template-columns:1fr}}
.panel{background:var(--panel);border:1px solid var(--border);border-radius:8px;padding:14px 16px;margin-bottom:12px}
.panel.warn{border-color:var(--amber)}
.panel.err{border-color:var(--red)}
h2{margin:0 0 10px;font-size:13px;font-weight:600;color:var(--muted);text-transform:uppercase;letter-spacing:.04em}
h2 .muted{text-transform:none;letter-spacing:0;font-weight:400;margin-left:6px}
table{width:100%;border-collapse:collapse}
td{padding:4px 0;border-bottom:1px solid var(--border)}
th{padding:4px 0;border-bottom:1px solid var(--border);text-align:left;font-size:11px;font-weight:600;color:var(--muted);text-transform:uppercase;letter-spacing:.04em}
tr:last-child td{border-bottom:0}
.num{text-align:right;font-variant-numeric:tabular-nums;font-weight:600;white-space:nowrap}
.when{text-align:right;white-space:nowrap;font-variant-numeric:tabular-nums}
td.when{color:var(--muted);font-weight:400}
.muted{color:var(--muted)}
.day{white-space:nowrap;padding-right:10px}
.barcell{width:100%;padding:4px 10px}
.bar{display:inline-block;height:10px;min-width:2px;border-radius:3px;background:var(--accent);vertical-align:middle}
.errpre{margin:0;padding:10px;max-height:180px;overflow:auto;background:var(--bg);border:1px solid var(--border);border-radius:6px;font-size:12px;color:var(--muted);white-space:pre-wrap;word-break:break-all}
.login{display:flex;gap:8px;flex-wrap:wrap}
.login input{flex:1;min-width:200px;padding:8px 10px;background:var(--panel);border:1px solid var(--border);border-radius:6px;color:var(--fg);font-size:14px}
.login button{padding:8px 14px;background:var(--accent);border:0;border-radius:6px;color:#0d1117;font-weight:600;cursor:pointer}
footer{margin-top:20px;color:var(--muted);font-size:12px}
`;
