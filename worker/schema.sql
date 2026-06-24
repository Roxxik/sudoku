-- One table, append-only, one row per solve event.
-- Apply with: wrangler d1 execute sudoku --remote --file schema.sql
--        (and --local for the wrangler dev loop).
CREATE TABLE IF NOT EXISTS solves (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  solve_id   TEXT    NOT NULL UNIQUE,   -- per-solve UUID minted on the client
  seed       TEXT,                      -- decimal u64 string; NULL on pre-seed puzzles
  puzzle     TEXT    NOT NULL,          -- 81 chars, '.' = empty
  solution   TEXT    NOT NULL,          -- 81 chars
  solve_ms   INTEGER NOT NULL,          -- final elapsedMs
  client_version TEXT    NOT NULL,       -- frontend build: short git commit, '-dirty' if built from a modified tree
  created_at TEXT    NOT NULL DEFAULT (datetime('now'))
);

-- Client-side upload failures, captured for inspection (see web/backend.js's
-- /errors reporter). Deliberately schemaless: one unvalidated JSON blob per
-- failed attempt, so the very payload /solves rejected -- or a solve that failed
-- client-side validation -- is recoverable without inspecting the device. The
-- worker checks only the API key before inserting here.
CREATE TABLE IF NOT EXISTS client_errors (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  payload    TEXT    NOT NULL,           -- raw JSON the client POSTed; unvalidated
  created_at TEXT    NOT NULL DEFAULT (datetime('now'))
);

-- The player's move timeline for one puzzle, synced periodically DURING play and
-- on close -- not just at solve -- so an abandoned or in-progress puzzle still
-- shows where people stalled. One row per puzzle, keyed by the same solve_id the
-- solves row uses (minted client-side at game creation); a row may exist here
-- with NO matching solves row (puzzle never finished). Whole-timeline snapshot
-- upsert: each sync replaces `events` with the latest full log, so a missed sync
-- is fully recovered by the next and re-sends are idempotent. The upsert is
-- guarded on event_count (see the worker) so an out-of-order/stale snapshot can
-- never shorten the stored log. puzzle/seed are duplicated out of the timeline's
-- opening `session` event so an unsolved puzzle is analyzable without a join.
CREATE TABLE IF NOT EXISTS move_logs (
  solve_id    TEXT    PRIMARY KEY,        -- joins solves.solve_id; minted at game creation
  puzzle      TEXT    NOT NULL,           -- 81 chars, '.' = empty
  seed        TEXT,                       -- decimal u64 string; NULL on pre-seed puzzles
  solved      INTEGER NOT NULL,           -- 0 in-progress/abandoned, 1 once a 'solved' event exists
  event_count INTEGER NOT NULL,           -- length of the events array; the upsert monotonicity guard
  last_t      INTEGER NOT NULL,           -- last event's solve-elapsed ms (progress without parsing events)
  events      TEXT    NOT NULL,           -- JSON array of the full move timeline
  client_version TEXT NOT NULL,           -- frontend build that wrote this snapshot
  created_at  TEXT    NOT NULL DEFAULT (datetime('now')),
  updated_at  TEXT    NOT NULL DEFAULT (datetime('now'))
);

-- Puzzle-of-the-day solve times, kept DELIBERATELY SEPARATE from `solves`: a daily
-- solve is uploaded only when the player taps Submit (an explicit, opt-out-bypassing
-- act -- see web/backend.js recordDailySolve), whereas `solves` is the passive,
-- privacy-gated grader feed. The two never share a row. `day` is the puzzle-day
-- index (daily.js dayNumber, ticks at 02:30 UTC) and `level` is the difficulty's
-- index into DAILY_LEVELS, so (day, level) names one daily puzzle; the same
-- deterministic seed gives every device that puzzle. solve_id (minted at game
-- creation) is the idempotency key: a resent submission is an INSERT OR IGNORE
-- no-op. This is the WRITE path only -- the leaderboard READ endpoint isn't wired
-- yet, but the index below is laid down now so it lands cheaply when it is.
CREATE TABLE IF NOT EXISTS daily_solves (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  solve_id   TEXT    NOT NULL UNIQUE,     -- stable per-puzzle id; resubmit -> idempotent no-op
  day        INTEGER NOT NULL,            -- puzzle-day index (daily.js dayNumber)
  level      INTEGER NOT NULL,            -- difficulty index into DAILY_LEVELS
  seed       TEXT,                        -- decimal u64 string of the pinned daily seed
  puzzle     TEXT    NOT NULL,            -- 81 chars, '.' = empty
  solution   TEXT    NOT NULL,            -- 81 chars
  solve_ms   INTEGER NOT NULL,            -- final elapsedMs
  client_version TEXT NOT NULL,           -- frontend build that submitted this solve
  created_at TEXT    NOT NULL DEFAULT (datetime('now'))
);

-- The leaderboard read path is "fastest solves for one (day, level)". This index
-- covers exactly that: an ordered range scan over (day, level) already sorted by
-- solve_ms, so the top-N query needs no table sort.
CREATE INDEX IF NOT EXISTS idx_daily_solves_day_level
  ON daily_solves (day, level, solve_ms);
