-- One table, append-only, one row per solve event.
-- Apply with: wrangler d1 execute sudoku --remote --file schema.sql
--        (and --local for the wrangler dev loop).
CREATE TABLE IF NOT EXISTS solves (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  client_id  TEXT    NOT NULL UNIQUE,   -- per-solve UUID minted on the client
  seed       TEXT,                      -- decimal u64 string; NULL on pre-seed puzzles
  puzzle     TEXT    NOT NULL,          -- 81 chars, '.' = empty
  solution   TEXT    NOT NULL,          -- 81 chars
  solve_ms   INTEGER NOT NULL,          -- final elapsedMs
  created_at TEXT    NOT NULL DEFAULT (datetime('now'))
);
