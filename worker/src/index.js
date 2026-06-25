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
