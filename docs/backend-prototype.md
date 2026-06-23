# Backend prototype — solve log on a Cloudflare Worker + D1

A first, deliberately small backend for the Sudoku app: when a puzzle is solved, the
client POSTs the solve to a Cloudflare Worker, which appends it to a D1 (SQLite) table.
Plus one script to download the database. Nothing else.

> Status: **PLAN, nothing built.** Branch `worktree-cloudflare-backend`. The worker will
> stay a thin CRUD layer with no domain logic now or later — all Sudoku logic stays in the
> wasm client. This doc is the contract; implement against it.

## Scope

**In:** one write endpoint (`POST /solves`), a D1 table, a fire-and-forget client call from
the solve hook, a shared-secret header, locked CORS, and a download script.

**Out (deferred, each noted at the end):** offline outbox + retry, the later auth scheme,
custom domain / hosting the frontend on Cloudflare Pages, any read/query/stats endpoint,
binary `.sqlite` export, rate limiting.

## Why this shape

The frontend is a **static** Trunk/wasm SPA deployed to **GitHub Pages**
([`.github/workflows/deploy.yml`](../.github/workflows/deploy.yml)) — a different origin from
any Worker. So:

- **CORS is mandatory** and will be **locked** to the Pages origin (not `*`). The app is
  served at `https://roxxik.github.io/sudoku/`, so the origin is **`https://roxxik.github.io`**
  (CORS matches scheme + host only — the `/sudoku` path is not part of the origin).
- There is no server-side render and no env-injection for the shipped JS, so the Worker URL
  and the shared secret are **hardcoded constants in the bundle** (the secret is therefore
  visible to anyone who opens devtools — see "Shared secret" below; treat the table as
  public-writable).

Everything the prototype stores already exists together at one point in the client. A solved
game ([`web/store.js`](../web/store.js)) carries `seed` (decimal-string u64 reproducer; may be
`null` on old records), `puzzle` and `solution` (81-char, `.` = empty in the puzzle), and the
final `elapsedMs`. The single write site is `onSolved()` in
[`web/play.js:578`](../web/play.js#L578), where `game.seed`, `game.puzzle`, `game.solution`
and `finalMs` are all in scope.

## Data model

One table, append-only, one row per solve event.

```sql
-- worker/schema.sql
CREATE TABLE IF NOT EXISTS solves (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  client_id  TEXT    NOT NULL UNIQUE,   -- per-solve UUID minted on the client
  seed       TEXT,                      -- decimal u64 string; NULL on pre-seed puzzles
  puzzle     TEXT    NOT NULL,          -- 81 chars, '.' = empty
  solution   TEXT    NOT NULL,          -- 81 chars
  solve_ms   INTEGER NOT NULL,          -- final elapsedMs
  created_at TEXT    NOT NULL DEFAULT (datetime('now'))
);
```

`client_id` is a fresh `crypto.randomUUID()` minted **per solve** (not the game id — a game can
be restarted and re-solved, so the game id is not unique per solve). Inserts use
`INSERT OR IGNORE`, so a later offline-retry that re-sends an already-stored solve is a no-op.
This is the only reason `client_id` exists now: it makes the deferred offline outbox a
frontend-only change with no schema churn.

## The Worker

Plain JS module worker, no build step, no TypeScript. wrangler is the only dependency.

```
worker/
  wrangler.toml
  schema.sql
  src/index.js
```

```toml
# worker/wrangler.toml
name = "sudoku-backend"
main = "src/index.js"
compatibility_date = "2026-06-23"

[[d1_databases]]
binding = "DB"
database_name = "sudoku"
database_id = ""   # filled in after `wrangler d1 create sudoku`
```

### Request contract — batch-capable from day one

The body is always a list, so the future batched (offline-flush) endpoint is the *same*
endpoint with a longer array — no second route, no contract change.

```
POST /solves
  headers: content-type: application/json, x-api-key: <secret>
  body:    { "solves": [ { client_id, seed, puzzle, solution, solve_ms }, ... ] }
  200:     { "inserted": <n> }      // n = rows actually written (OR IGNORE may drop dupes)
  400:     malformed body
  401:     bad/missing x-api-key
  405:     method other than POST/OPTIONS on /solves
```

A single solve is a one-element array. The handler validates each entry (81-char
puzzle/solution, integer `solve_ms`, non-empty `client_id`), then writes them with
`env.DB.batch([...])` of prepared `INSERT OR IGNORE` statements.

### `src/index.js` skeleton

```js
const ALLOW_ORIGIN = "https://roxxik.github.io"; // app at /sudoku/; CORS matches origin only (no path)

function cors(resp) {
  resp.headers.set("Access-Control-Allow-Origin", ALLOW_ORIGIN);
  resp.headers.set("Vary", "Origin");
  return resp;
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);

    if (request.method === "OPTIONS") {
      // preflight
      return cors(new Response(null, {
        status: 204,
        headers: {
          "Access-Control-Allow-Methods": "POST, OPTIONS",
          "Access-Control-Allow-Headers": "content-type, x-api-key",
          "Access-Control-Max-Age": "86400",
        },
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

    const stmt = env.DB.prepare(
      "INSERT OR IGNORE INTO solves (client_id, seed, puzzle, solution, solve_ms) VALUES (?,?,?,?,?)"
    );
    const res = await env.DB.batch(
      solves.map((s) => stmt.bind(s.client_id, s.seed ?? null, s.puzzle, s.solution, s.solve_ms))
    );
    const inserted = res.reduce((n, r) => n + (r.meta?.changes ?? 0), 0);
    return cors(new Response(JSON.stringify({ inserted }), {
      status: 200, headers: { "content-type": "application/json" },
    }));
  },
};

function valid(s) {
  return s && typeof s.client_id === "string" && s.client_id.length > 0
    && typeof s.puzzle === "string"   && s.puzzle.length === 81
    && typeof s.solution === "string" && s.solution.length === 81
    && Number.isInteger(s.solve_ms)
    && (s.seed == null || typeof s.seed === "string");
}
```

### Shared secret

`x-api-key` is checked against `env.API_KEY`, set out-of-repo with `wrangler secret put API_KEY`.
The **same literal** goes into the client bundle, so it is readable by anyone who opens the
Network tab — it is *not* a secret from a human looking at the site. What it buys: drive-by bots
scanning `*.workers.dev` send no such header and get 401s. It raises the bar from "zero effort"
to "open devtools once," which filters background noise and nothing more. The real abuse control
is the **later auth scheme** (user already has a plan); this header is the prototype speed bump.
Treat the data as public-writable.

## Frontend wiring

A new module, plus one call in the existing solve hook. No other client changes.

```js
// web/backend.js  (new)
const ENDPOINT = "https://sudoku-backend.<subdomain>.workers.dev/solves"; // from `wrangler deploy`
const API_KEY  = "<paste-the-same-secret>"; // public in the bundle by necessity (see doc)

// Fire-and-forget: an offline or failed POST must never affect the solve flow, so this never
// throws and never awaits into the UI. `keepalive` lets it outlive the navigation that the
// solved dialog may trigger. Offline durability (an outbox + retry) is a later addition; for
// now a missed solve is simply lost.
export function recordSolve(solve) {
  try {
    fetch(ENDPOINT, {
      method: "POST",
      headers: { "content-type": "application/json", "x-api-key": API_KEY },
      body: JSON.stringify({ solves: [solve] }),
      keepalive: true,
    }).catch(() => {});
  } catch {}
}
```

```js
// web/play.js — inside onSolved(), right after the updateGame(...status:"solved") call (~line 587)
backend.recordSolve({
  client_id: crypto.randomUUID(),
  seed: game.seed,            // may be null on old records
  puzzle: game.puzzle,
  solution: game.solution,
  solve_ms: finalMs,
});
```

(`import * as backend from "./backend.js";` at the top of [`web/play.js`](../web/play.js),
alongside its existing `store`/`gen` imports.) Verify with `trunk build` only — no frontend
tests.

## Download script

```bash
#!/usr/bin/env bash
# scripts/db-download — download the remote D1 "sudoku" DB as a SQL dump.
#   scripts/db-download [outfile]      (default: sudoku-dump.sql at repo root)
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:-$root/sudoku-dump.sql}"
( cd "$root/worker" && wrangler d1 export sudoku --remote --output "$out" )
echo "wrote $out"
```

SQL dump (not a binary `.sqlite`) because it's simpler and the format is easy to change later;
turning the dump into a real database is one line when wanted (`sqlite3 sudoku.sqlite < dump.sql`).
`chmod +x`, matching the other extension-less executables in [`scripts/`](../scripts/).

## One-time setup / deploy order

1. `cd worker && wrangler d1 create sudoku` → paste the printed `database_id` into `wrangler.toml`.
2. `wrangler d1 execute sudoku --remote --file schema.sql` (and `--local` for local dev).
3. `wrangler secret put API_KEY` → paste a generated key; put the **same** literal in `web/backend.js`.
4. `wrangler deploy` → copy the `*.workers.dev` URL into `ENDPOINT` in `web/backend.js`.
5. `ALLOW_ORIGIN` in `src/index.js` is `https://roxxik.github.io` (the confirmed Pages origin).
6. Wire the `onSolved()` call, `trunk build`, push to master → Pages redeploys.

Local loop: `wrangler dev` (with `--local` D1) and a `curl` POST to smoke-test the contract
before deploying.

## Updating the deployed worker

The worker deploy is **manual** and **separate from the frontend**. The frontend auto-deploys
on every push to master ([`.github/workflows/deploy.yml`](../.github/workflows/deploy.yml) →
GitHub Pages); the worker has **no CI** and is pushed by hand with `wrangler`. Automating it
would mean storing a Cloudflare API token as a repo secret and adding a `wrangler-action` job —
deferred, not needed to iterate.

That decoupling is a feature, not a gap: **always deploy the worker before the frontend that
depends on the change**, so an old, cached, or offline client never reaches a server that no
longer understands it (see *Backwards compatibility* below).

Three kinds of change, three workflows:

**1. Worker code only** (handler logic, validation, CORS) — no schema, no contract change:

```bash
cd worker && wrangler deploy
```

Live in seconds, atomically. Roll back with `wrangler deployments list` then
`wrangler rollback [id]`. No frontend rebuild needed.

**2. Schema change** (new column, index, or table). Use D1's **tracked migrations** rather than
ad-hoc `d1 execute`, so applied state is recorded (in the `d1_migrations` table) and re-runs are
no-ops:

```bash
cd worker
wrangler d1 migrations create sudoku add_client_version   # -> migrations/0002_add_client_version.sql
#   edit that file, e.g.:  ALTER TABLE solves ADD COLUMN client_version TEXT;
wrangler d1 migrations apply sudoku --local               # dev DB first
wrangler d1 migrations apply sudoku --remote              # then prod
```

Migrations are **forward-only and additive by rule**: `ADD COLUMN` (nullable or with a default),
`CREATE TABLE`, `CREATE INDEX`. Never `DROP`/rename a column an existing client still writes.
D1 has no transactional DDL rollback, so a bad migration is fixed by a *new* forward migration,
not a revert. Apply the migration, then `wrangler deploy` the code that reads/writes the new
column.

> The initial table currently ships as `schema.sql` applied with `wrangler d1 execute --file`
> (setup step 2). To bring it under migration tracking, move it to `migrations/0001_init.sql`
> and run `wrangler d1 migrations apply`; until then, treat `schema.sql` as migration 0001 by hand.

**3. Request/response contract change** (the shape `web/backend.js` sends or expects back). This
is the one that needs care because of cached/offline clients. Order of operations is fixed:
deploy a worker that accepts **both** the old and new shapes, *then* ship the frontend that uses
the new shape. Never the reverse. See *Backwards compatibility*.

## Backwards compatibility

The frontend is a static bundle on GitHub Pages with **no service worker**. The content-hashed
assets (wasm, CSS) are cache-busted per build; the plain ES modules copied as-is (`play.js`,
`backend.js`, …) keep stable names and ride Pages' `Cache-Control: max-age=600` + ETag, so an
*online* returning visitor is at most ~10 minutes stale. A genuinely **offline / long-lived
tab** runs whatever bundle it loaded — arbitrarily old. So the server must assume a client can
POST an **old payload shape at any time**.

The load-bearing guarantee is therefore one rule, not a version handshake:

- **Additive-only contract.** New request fields are optional with a server default; never remove
  a field, never repurpose a field's meaning, never tighten validation on an existing field. The
  `valid()` check only ever *gains* `(s.newField == null || …)` clauses. Under this rule every old
  client keeps working forever with no coordination.

`client_id` + `INSERT OR IGNORE` already make re-sends idempotent, so the deferred offline outbox
composes with this for free.

> A client-version field + a "your app is out of date, refresh" nudge is a *separate* concern
> (frontend freshness, not wire compatibility) and is left open for discussion — see the design
> notes, not built here.

## Deferred (and why each is cheap to add on top of this)

- **Offline outbox + retry.** A localStorage queue appended in `onSolved()`, flushed on the
  `online` event / next load via the **already batch-capable** endpoint; `INSERT OR IGNORE` on
  `client_id` makes retries idempotent. Frontend-only; no schema or API change.
- **The real auth scheme.** Replaces/augments the shared header; user has a plan. The worker
  stays thin — auth is a header/token check, not domain logic.
- **Custom domain / frontend on Cloudflare Pages.** Lets CORS relax to same-origin later;
  until then, locked CORS.
- **Reads / stats endpoints, binary `.sqlite` export, rate limiting / Turnstile** — none needed
  to validate "solves land in a table I can download."

## Files this touches

```
worker/wrangler.toml        new
worker/schema.sql           new
worker/src/index.js         new
web/backend.js              new
web/play.js                 +1 import, +1 call in onSolved()
scripts/db-download         new (chmod +x)
docs/backend-prototype.md   this doc
```
