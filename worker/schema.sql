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
