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
