# Backend prototype — solve log on a Cloudflare Worker + D1

A first, deliberately small backend for the Sudoku app: when a puzzle is solved, the
client POSTs the solve to a Cloudflare Worker, which appends it to a D1 (SQLite) table.
Plus one script to download the database. Nothing else.

> Status: **PLAN, nothing built.** Branch `worktree-cloudflare-backend`. The worker will
> stay a thin CRUD layer with no domain logic now or later — all Sudoku logic stays in the
> wasm client. This doc is the contract; implement against it.

## Scope

**In:** one write endpoint (`POST /solves`), a D1 table, a fire-and-forget client call from
the solve hook, a shared-secret header, locked CORS, and a download script. Plus (built on
top, see *Offline outbox + failure capture*): a localStorage retry queue and a schemaless
`POST /errors` capture endpoint backed by a second table. And (see *Move-log sync*): a
`POST /moves` endpoint + table that captures the player's move timeline during play, not just
at solve, keyed by the same `solve_id`.

**Out (deferred, each noted at the end):** the later auth scheme, custom domain / hosting the
frontend on Cloudflare Pages, any read/query/stats endpoint, binary `.sqlite` export, rate
limiting.

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
  solve_id   TEXT    NOT NULL UNIQUE,   -- per-solve UUID minted on the client
  seed       TEXT,                      -- decimal u64 string; NULL on pre-seed puzzles
  puzzle     TEXT    NOT NULL,          -- 81 chars, '.' = empty
  solution   TEXT    NOT NULL,          -- 81 chars
  solve_ms   INTEGER NOT NULL,          -- final elapsedMs
  created_at TEXT    NOT NULL DEFAULT (datetime('now'))
);
```

`solve_id` is a per-puzzle id minted on the client **at game creation** (`store.createGame`),
not the game id. Inserts use `INSERT OR IGNORE`, so an offline-retry that re-sends an
already-stored solve is a no-op. Minting it up front (rather than per solve) does two jobs: it
keeps the offline outbox a frontend-only change with no schema churn, and — because it's stable
for the whole life of the puzzle — it is the **join key** between the solve row and the
move-log rows synced *during* play (see *Move-log sync* below). Old game records predate the
field; `onSolved` falls back to a one-off `crypto.randomUUID()` for those (no move history).

A second, deliberately schemaless table captures client-side upload failures (see *Offline
outbox + failure capture*). It stores one unvalidated JSON blob per failed attempt — the very
payload `/solves` rejected — so a failure is diagnosable from the downloaded DB without
touching the device.

```sql
CREATE TABLE IF NOT EXISTS client_errors (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  payload    TEXT    NOT NULL,           -- raw JSON the client POSTed; unvalidated
  created_at TEXT    NOT NULL DEFAULT (datetime('now'))
);
```

A third table holds the player's **move timeline** for one puzzle — synced periodically during
play and on close, not just at solve, so an abandoned or in-progress puzzle still shows where
people stall. One row per puzzle, keyed by `solve_id` (so it joins `solves`, or stands alone
when the puzzle was never finished); a whole-timeline snapshot upsert (the `events` array
replaces the stored one) guarded so a stale snapshot can't shrink it. See *Move-log sync*.

```sql
CREATE TABLE IF NOT EXISTS move_logs (
  solve_id    TEXT    PRIMARY KEY,        -- joins solves.solve_id; minted at game creation
  puzzle      TEXT    NOT NULL,           -- 81 chars (out of the timeline's `session` event)
  seed        TEXT,                       -- decimal u64 string; NULL on pre-seed puzzles
  solved      INTEGER NOT NULL,           -- 0 in-progress/abandoned, 1 once solved
  event_count INTEGER NOT NULL,           -- events array length; the upsert monotonicity guard
  last_t      INTEGER NOT NULL,           -- last event's solve-elapsed ms (progress at a glance)
  events      TEXT    NOT NULL,           -- JSON array of the full move timeline
  client_version TEXT NOT NULL,
  created_at  TEXT    NOT NULL DEFAULT (datetime('now')),
  updated_at  TEXT    NOT NULL DEFAULT (datetime('now'))
);
```

A fourth table holds **Puzzle-of-the-day solve times**, kept apart from `solves` because they
have a different *consent model*: a daily time lands here only when the player taps **Submit**
(an explicit act that **bypasses** the data-sharing opt-out), whereas `solves` is the passive,
privacy-gated grader feed. `(day, level)` names the daily puzzle — `day` is the puzzle-day index
(`daily.js dayNumber`, ticking at 02:30 UTC) and `level` the difficulty's index into
`DAILY_LEVELS` — and a covering index on `(day, level, solve_ms)` makes the eventual "fastest
solves for one day/difficulty" leaderboard query an ordered range scan with no table sort. Only
the **write** path is wired now; the leaderboard read endpoint is deferred. See *Daily solves*.

```sql
CREATE TABLE IF NOT EXISTS daily_solves (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  solve_id   TEXT    NOT NULL UNIQUE,     -- stable per-puzzle id; resubmit -> INSERT OR IGNORE no-op
  day        INTEGER NOT NULL,            -- puzzle-day index (daily.js dayNumber)
  level      INTEGER NOT NULL,            -- difficulty index into DAILY_LEVELS
  seed       TEXT,                        -- decimal u64 string of the pinned daily seed
  puzzle     TEXT    NOT NULL,            -- 81 chars, '.' = empty
  solution   TEXT    NOT NULL,            -- 81 chars
  solve_ms   INTEGER NOT NULL,            -- final elapsedMs
  client_version TEXT NOT NULL,
  created_at TEXT    NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_daily_solves_day_level
  ON daily_solves (day, level, solve_ms);
```

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
name = "backend"
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
  body:    { "solves": [ { solve_id, seed, puzzle, solution, solve_ms }, ... ] }
  200:     { "inserted": <n> }      // n = rows actually written (OR IGNORE may drop dupes)
  400:     malformed body
  401:     bad/missing x-api-key
  405:     method other than POST/OPTIONS on /solves
```

A single solve is a one-element array. The handler validates each entry (81-char
puzzle/solution, integer `solve_ms`, non-empty `solve_id`), then writes them with
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
      "INSERT OR IGNORE INTO solves (solve_id, seed, puzzle, solution, solve_ms) VALUES (?,?,?,?,?)"
    );
    const res = await env.DB.batch(
      solves.map((s) => stmt.bind(s.solve_id, s.seed ?? null, s.puzzle, s.solution, s.solve_ms))
    );
    const inserted = res.reduce((n, r) => n + (r.meta?.changes ?? 0), 0);
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
const ENDPOINT = "https://backend.<subdomain>.workers.dev/solves"; // from `wrangler deploy`
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
  solve_id: crypto.randomUUID(),
  seed: game.seed,            // may be null on old records
  puzzle: game.puzzle,
  solution: game.solution,
  solve_ms: finalMs,
});
```

(`import * as backend from "./backend.js";` at the top of [`web/play.js`](../web/play.js),
alongside its existing `store`/`gen` imports.) Verify with `trunk build` only — no frontend
tests. The sketch above is the *original* fire-and-forget body; the shipped
[`web/backend.js`](../web/backend.js) extends it with the outbox + capture below (the
`onSolved()` call site is unchanged).

## Offline outbox + failure capture

Frontend-only durability over the already batch-capable `/solves`, plus a server-side record of
failures so they're diagnosable without inspecting the device. No `/solves` contract change.

**Outbox.** `recordSolve` is enqueue-then-flush: the solve is appended to a localStorage queue
(`sudoku.backend.outbox.v1`, capped at the newest 100) *before* the POST, so it survives a
closed tab or a never-resolving request. A flush sends the **whole queue in one batch** and
reconciles against the result. Three retry triggers drain it: **lazily on the next solve**
(batched with it), on the **`online`** event, and **once on load**. `solve_id` + `INSERT OR
IGNORE` make every re-send idempotent, so an over-eager retry just writes nothing. A single
`flushing` guard serializes concurrent triggers.

**Failure-status policy** (how a flush reconciles the queue):

| Outcome              | Queue        | /errors capture            |
|----------------------|--------------|----------------------------|
| 2xx                  | drop (sent)  | —                          |
| 401                  | **keep**     | — (dashboard; a fixed build re-delivers) |
| other 4xx (e.g. 400) | drop         | yes — the rejected payload |
| 5xx                  | keep, retry  | yes, **once** (`errorReported` flag) |
| network reject       | keep, retry  | — (no network to report over) |

A solve that fails the client-side mirror of the worker's `valid()` is never enqueued (so one
bad entry can't 400 a whole batch); it's reported to `/errors` as `kind:"invalid_solve"` so it
stays visible.

**`POST /errors` — the capture endpoint.** Deliberately schemaless: it checks `x-api-key` and
nothing else, then stores the raw body verbatim in `client_errors` (capped at 64 KB). A
bad/missing key still 401s, which shows in the Cloudflare dashboard — that is the only gate.
The endpoint is the `/solves` sibling on the same worker (`ENDPOINT.replace(/\/solves$/,
"/errors")`), so `backend-config.js` needs no change. Reports are best-effort, never keepalive,
and have no outbox of their own (a lost report is just lost — failures never recurse).

```
POST /errors
  headers: content-type: application/json, x-api-key: <secret>
  body:    any JSON (unvalidated; e.g. { kind, status, solves: [...], client_version, user_agent })
  200:     { "ok": true }      // body stored verbatim, truncated to 64 KB
  401:     bad/missing x-api-key
  405:     method other than POST/OPTIONS
```

The captured rows ride down with the existing `scripts/db-download` SQL dump — no new tooling.

## Move-log sync

The grader wants more than the final time: it wants the *path* — and, crucially, the paths of
puzzles people **never finish**, where they stalled. So the move timeline (`tracker.js`, the
`sudoku.track.<id>` localStorage log) is now uploaded too, not just the solve.

**Shape — whole-snapshot upsert, keyed by `solve_id`.** Each sync sends the *entire* current
timeline as one JSON array; the worker upserts it into `move_logs` (`ON CONFLICT(solve_id) DO
UPDATE`). Snapshots (not per-event rows or deltas) keep the client trivial and every upload
self-contained: a missed sync is fully recovered by the next. The upsert's `WHERE
excluded.event_count >= move_logs.event_count` is a **monotonicity guard** — an out-of-order or
stale retry whose timeline is shorter than what's stored is a no-op, so it can never roll the
log back. `event_count` is taken from the array length server-side, so it can't be faked by a
mismatched field. A row can exist here with **no** matching `solves` row (puzzle abandoned);
`puzzle`/`seed` are duplicated out of the timeline's opening `session` event so such a row is
analyzable without a join.

**When it syncs (`play.js`):** a genuine pause (which the app's `visibilitychange→hidden`
handler already triggers when the tab hides), a **periodic tick** every 30 s while the clock
runs, a `pagehide` last-chance on hard unload, and once on solve (carrying the final `solved`
event, `solved:1`). Coarse points only — never per keystroke.

**Outbox — coalescing by `solve_id`.** A second localStorage outbox mirrors the solve outbox
but, because a move log is a growing snapshot re-sent many times, it **coalesces**: enqueuing a
newer snapshot for a `solve_id` drops the one already queued, bounding the queue to one entry
per puzzle in flight (capped at 50 puzzles). Same retry triggers as solves (`online`, load) and
the same failure-status policy, with two differences: a 4xx capture sends a **compact summary**
(`solve_id`/`event_count`/`last_t`, not the full timelines — they'd blow the 64 KB `/errors`
cap), and a 5xx is kept-for-retry **without** a once-report (move logs re-sync constantly, so a
persistent 5xx would spam `/errors`). Post-delivery cleanup compares event counts, not just
`solve_id`, so a snapshot that grew mid-flush survives.

```
POST /moves
  headers: content-type: application/json, x-api-key: <secret>
  body:    { "logs": [ { solve_id, puzzle, seed, solved, last_t, events: [...], client_version }, ... ] }
  200:     { "upserted": <n> }   // n = rows actually written (guard may no-op some)
  400:     bad json / a log failing validation / events serialized over 512 KB
  401:     bad/missing x-api-key
  405:     method other than POST/OPTIONS
```

**Privacy.** The timeline carries only move data, settings, and the game's own random ids — no
device info or identifier (the `user_agent` that `/errors` records is *not* sent here). A
user-facing privacy notice and a possible future opt-in ("use my solve data to improve
grading") are tracked separately.

## Daily solves

The Puzzle of the day (`daily.js`) has its own **Submit** flow, separate from the passive feeds
above. The defining difference is *consent*: `recordSolve`/`recordMoves` upload automatically
**only** when the player has opted into data sharing, whereas tapping **Submit** on a daily IS
the consent — so a daily submission **bypasses the opt-out** entirely. It is gated on a
configured backend alone, never on `dataSharingOn`, and `clearOutboxes` (opting out) leaves it
untouched. The two never share a row: dailies go to `daily_solves`, not `solves`.

**The button is a tri-state**, which the client tracks with two localStorage stores rather than
one outbox:

- `sudoku.backend.daily.outbox.v1` — the **pending** queue (coalesced per `solve_id`), like the
  other outboxes; an entry here means *queued, not yet accepted*.
- `sudoku.backend.daily.done.v1` — a capped list of **accepted** `solve_id`s. A delivered outbox
  entry is removed, but "Submitted" must persist across that and across sessions, so the final
  state lives here, not in the (now-empty) outbox.

`dailySubmitState(solve_id)` reads the pair → `submitted` (in done) / `pending` (in outbox) /
`none`, driving the overview button **Play → Continue → Submit → Submit (disabled, queued) →
Submitted**. A `setDailyChangeListener` hook lets the page flip the button live as the POST lands
(or as a background retry succeeds) without a global event bus. Same retry triggers (`online`,
load) and failure-status policy as `/solves`; on 2xx the `solve_id`s move to the done-set.

```
POST /daily
  headers: content-type: application/json, x-api-key: <secret>
  body:    { "solves": [ { solve_id, day, level, seed, puzzle, solution, solve_ms, client_version }, ... ] }
  200:     { "inserted": <n> }   // n = rows actually written (INSERT OR IGNORE on solve_id)
  400:     bad json / a solve failing validation
  401:     bad/missing x-api-key
  405:     method other than POST/OPTIONS
```

The **leaderboard read path is deliberately not wired** — this only stores the daily solves and
their timings. The `(day, level, solve_ms)` index is laid down now so the eventual top-N query
lands cheaply.

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

Local loop, full stack: **`scripts/dev-local`** (`--lan` to serve on `0.0.0.0` for phones on the
LAN) starts an ephemeral local Worker + local D1 and `trunk serve`s the frontend wired to post at
it — no source edits, the endpoint/key are injected via build env vars and the Worker's CORS
origin/key come from a gitignored `worker/.dev.vars`. The local D1 persists under
`worker/.wrangler/` (gitignored); `rm -rf worker/.wrangler` to reset. For a worker-only smoke
test, `wrangler dev` plus a `curl` POST still works.

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

`solve_id` + `INSERT OR IGNORE` already make re-sends idempotent, so the deferred offline outbox
composes with this for free.

> A client-version field + a "your app is out of date, refresh" nudge is a *separate* concern
> (frontend freshness, not wire compatibility) and is left open for discussion — see the design
> notes, not built here.

## Deferred (and why each is cheap to add on top of this)

- **The real auth scheme.** Replaces/augments the shared header; user has a plan. The worker
  stays thin — auth is a header/token check, not domain logic.
- **Custom domain / frontend on Cloudflare Pages.** Lets CORS relax to same-origin later;
  until then, locked CORS.
- **Reads / stats endpoints, binary `.sqlite` export, rate limiting / Turnstile** — none needed
  to validate "solves land in a table I can download."
- **The daily leaderboard read endpoint.** The write path + covering index exist; surfacing the
  scores is a `GET /daily?day=&level=` (or similar) plus the UI to render it. Deferred.

## Files this touches

```
worker/wrangler.toml        new
worker/schema.sql           new (+ client_errors, move_logs, daily_solves tables)
worker/src/index.js         new (+ POST /errors, /moves, /daily routes)
web/backend.js              new (+ solve/move/daily outboxes, retry, /errors capture)
web/play.js                 + move-sync + daily Submit wiring in the solved screen
web/home.js                 daily overview tri-state Submit button + listener
scripts/db-download         new (chmod +x)
docs/backend-prototype.md   this doc
```
