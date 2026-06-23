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
