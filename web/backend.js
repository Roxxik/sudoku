// Solve-log backend client. Posts a solved game to the Cloudflare Worker
// (see docs/backend-prototype.md). Online-only by design: a missed POST is
// simply lost for now -- offline durability (an outbox + retry over the
// already batch-capable endpoint) is a deferred, frontend-only addition.

// Filled in after `wrangler deploy` and `wrangler secret put API_KEY`. Both are
// public in the shipped bundle by necessity: there is no server-side render to
// inject them, and the secret only buys a speed bump against drive-by bots (a
// human with devtools can read it). Treat the table as public-writable.
const ENDPOINT = "https://sudoku-backend.<subdomain>.workers.dev/solves";
const API_KEY  = "<paste-the-same-secret>";

// True until the constants above are replaced with the deployed values. While
// unconfigured we skip the POST entirely so the prototype doesn't fire a doomed
// request (and log a console error) on every solve before the backend exists.
const CONFIGURED = !ENDPOINT.includes("<subdomain>") && !API_KEY.includes("<paste");

// Fire-and-forget: recording a solve must never affect the solve flow, so this
// never throws and never awaits into the UI. An offline, slow, or failed POST
// is swallowed. `keepalive` lets the request outlive any navigation the solved
// dialog triggers. `solve` is { client_id, seed, puzzle, solution, solve_ms }.
export function recordSolve(solve) {
  if (!CONFIGURED) return;
  try {
    fetch(ENDPOINT, {
      method: "POST",
      headers: { "content-type": "application/json", "x-api-key": API_KEY },
      body: JSON.stringify({ solves: [solve] }),
      keepalive: true,
    }).catch(() => {});
  } catch {}
}
