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

    if (request.method === "OPTIONS") {
      // CORS preflight for the cross-origin POST from Pages.
      return cors(new Response(null, {
        status: 204,
        headers: {
          "Access-Control-Allow-Methods": "POST, OPTIONS",
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
    // makes a resent submission idempotent. The leaderboard READ path isn't wired
    // yet -- this only stores the solves and their timings.
    if (url.pathname === "/daily") {
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
          "INSERT OR IGNORE INTO daily_solves (solve_id, day, level, seed, puzzle, solution, solve_ms, client_version) VALUES (?,?,?,?,?,?,?,?)"
        );
        const res = await env.DB.batch(
          solves.map((s) => stmt.bind(s.solve_id, s.day, s.level, s.seed ?? null, s.puzzle, s.solution, s.solve_ms, s.client_version))
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
  },
};

function valid(s) {
  return s && typeof s.solve_id === "string" && s.solve_id.length > 0
    && typeof s.puzzle === "string"   && s.puzzle.length === 81
    && typeof s.solution === "string" && s.solution.length === 81
    && Number.isInteger(s.solve_ms)
    && (s.seed == null || typeof s.seed === "string")
    && typeof s.client_version === "string" && s.client_version.length > 0;
}

// Shape check for a /daily entry. Like valid() plus the (day, level) that names
// the daily puzzle; both are required integers (the seed merely reproduces the
// puzzle and may be null on an odd record).
function validDaily(s) {
  return s && typeof s.solve_id === "string" && s.solve_id.length > 0
    && Number.isInteger(s.day)
    && Number.isInteger(s.level)
    && typeof s.puzzle === "string"   && s.puzzle.length === 81
    && typeof s.solution === "string" && s.solution.length === 81
    && Number.isInteger(s.solve_ms)
    && (s.seed == null || typeof s.seed === "string")
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
