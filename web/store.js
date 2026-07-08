"use strict";

import { logStorageError } from "./util.js";

// Persistence for played puzzles. Everything the start page shows -- the
// in-progress "Continue" list, the per-mode best times, the solved-count badges
// in the tree, and the Stats page -- is derived from one flat list of game
// records in localStorage. A game is NEVER deleted when solved: it flips to
// "solved" with its final time attached, so the same record feeds both the
// timings and the aggregate stats.
//
// STORAGE IS TWO-TIERED. A game's board state (placements, pencil marks) and its
// undo/redo timeline grow without bound as you play -- the history can dwarf the
// rest of the record. The start page and Stats never need any of that; they only
// read metadata (mode, times, status, grade...). So the heavy, per-keystroke
// mutable state lives under its OWN key, separate from the listing:
//
//   INDEX_KEY  -> array of light metadata records (one per game) -- the listing.
//   game.<id>  -> { value, centerMarks, cornerMarks, history, redo, elapsedMs }
//                 the heavy mutable state for that one game.
//
// The payoff: checkpointing a move (saveProgress) writes ONE game's key and
// never touches the index or any other game, so its cost is independent of how
// many puzzles you've ever played. Listing reads parse only the small index.

const INDEX_KEY = "sudoku.games.v1";
const N = 81;

// The per-game key prefixes, factored out so reclaimStorage can recognise a game's
// keys by prefix when sweeping the whole localStorage namespace.
const GAME_PREFIX = "sudoku.game.";
const TRACK_PREFIX = "sudoku.track.";

function gameKey(id) {
  return `${GAME_PREFIX}${id}`;
}

// The move-tracking timeline for a game lives under its own third key, separate
// from both the index and the heavy board state. It is append-mostly and can
// grow without bound, so keeping it off the per-keystroke heavy key leaves that
// save path O(1). Frontend-only capture for the grader (see tracker.js); never
// sent anywhere.
function trackKey(id) {
  return `${TRACK_PREFIX}${id}`;
}

// The fields that live in the per-game heavy key rather than the index. These are
// the ones that change as you play; everything else is listing metadata.
const HEAVY_FIELDS = ["value", "centerMarks", "cornerMarks", "history", "redo", "elapsedMs", "revealedHints"];

// ---- Low-level load/save ----

// The index is small (a handful of scalar fields plus two 81-char strings per
// game) and always read/written whole. It carries NO board state or history.
function loadIndex() {
  let raw;
  try {
    raw = localStorage.getItem(INDEX_KEY);
  } catch (err) {
    logStorageError("read", INDEX_KEY, err);
    return [];
  }
  if (!raw) return [];
  let games;
  try {
    games = JSON.parse(raw);
  } catch (err) {
    // Corrupt/old data shouldn't brick the app; start fresh.
    logStorageError("read", INDEX_KEY, err);
    return [];
  }
  if (!Array.isArray(games)) return [];
  // Lazy one-time migration from the original single-blob format, where each
  // record carried its full board state + undo/redo history inline. Split the
  // heavy fields out into per-game keys and rewrite the index light.
  if (games.some(isLegacyRecord)) {
    games = migrateLegacy(games);
    saveIndex(games);
  }
  return games;
}

function saveIndex(games) {
  try {
    localStorage.setItem(INDEX_KEY, JSON.stringify(games));
  } catch (err) {
    // Quota or privacy mode: play still works this session, just isn't saved.
    logStorageError("write", INDEX_KEY, err);
  }
}

// Read one game's heavy state ({} if absent/corrupt -- getGame merges over the
// index entry, and loadGame tolerates missing fields).
function readHeavy(id) {
  try {
    const raw = localStorage.getItem(gameKey(id));
    return raw ? JSON.parse(raw) || {} : {};
  } catch (err) {
    logStorageError("read", gameKey(id), err);
    return {};
  }
}

function writeHeavy(id, heavy) {
  try {
    localStorage.setItem(gameKey(id), JSON.stringify(heavy));
  } catch (err) {
    // Quota or privacy mode: tolerated, as with the index.
    logStorageError("write", gameKey(id), err);
  }
}

function deleteHeavy(id) {
  try {
    localStorage.removeItem(gameKey(id));
  } catch (err) {
    logStorageError("delete", gameKey(id), err);
  }
}

// ---- Move-tracking log (per game) ----
// The events array for one game ([] if absent/corrupt). Loading it on session
// start lets a puzzle played across several sittings keep one continuous
// timeline rather than starting fresh each time it's reopened.
export function loadTrack(id) {
  try {
    const raw = localStorage.getItem(trackKey(id));
    if (!raw) return [];
    const data = JSON.parse(raw);
    return Array.isArray(data && data.events) ? data.events : [];
  } catch (err) {
    logStorageError("read", trackKey(id), err);
    return [];
  }
}

// Overwrite a game's whole event log. Wrapped in a small envelope so the schema
// can be versioned later without ambiguity. Tolerates quota/privacy failures the
// same way the index and heavy writes do -- tracking must never break play.
export function saveTrack(id, events) {
  try {
    localStorage.setItem(trackKey(id), JSON.stringify({ v: 1, events }));
  } catch (err) {
    /* quota or privacy mode: the log just won't persist this session. */
    logStorageError("write", trackKey(id), err);
  }
}

function deleteTrack(id) {
  try {
    localStorage.removeItem(trackKey(id));
  } catch (err) {
    logStorageError("delete", trackKey(id), err);
  }
}

// An old single-blob record still carries its heavy fields inline (including the
// legacy single `marks` grid). The migrated index records carry none of them.
function isLegacyRecord(g) {
  return (
    g &&
    ("value" in g ||
      "centerMarks" in g ||
      "cornerMarks" in g ||
      "marks" in g ||
      "history" in g ||
      "redo" in g)
  );
}

// Split each legacy record into a heavy per-game key + a light index entry.
// elapsedMs stays in BOTH: authoritative (live timer) in the heavy key, plus a
// copy in the index so the start page can list play time without a heavy read.
function migrateLegacy(games) {
  const light = [];
  for (const g of games) {
    if (!g || !g.id) continue;
    writeHeavy(g.id, {
      value: g.value,
      centerMarks: g.centerMarks || g.marks, // legacy single mark grid -> center
      cornerMarks: g.cornerMarks,
      history: g.history,
      redo: g.redo,
      elapsedMs: g.elapsedMs,
    });
    // Drop the heavy fields from the index entry; keep everything else (incl. the
    // elapsedMs copy, which is not in this strip list).
    const { value, centerMarks, cornerMarks, marks, history, redo, ...meta } = g;
    light.push(meta);
  }
  return light;
}

// A short unique id without pulling in a uuid dep. crypto.randomUUID when
// available, else a timestamp+random fallback.
function newId() {
  if (typeof crypto !== "undefined" && crypto.randomUUID) return crypto.randomUUID();
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

// ---- Game records ----
// A game record, as seen by the rest of the app (getGame merges the two tiers):
// {
//   id,
//   kind: "campaign"|"review"|"daily"|"custom"|"imported" -- the mode discriminator,
//         the single source of "what kind of game is this" (see modes.js modeOf).
//         Absent on records from before this field; the one-time mode migration
//         (modes.js) stamps it at boot, so live code treats it as always present.
//   kindIndex: campaign only -- the curriculum kind (null otherwise),
//   mode: "train"|"drill" for campaign and review (the sub-variant); "custom" on
//         custom/daily/imported (a vestige -- identity comes from `kind`),
//   lesson: review only -- the stable REVIEW_LESSONS key (null otherwise),
//   solve_id: stable per-puzzle backend id minted at creation (the join key
//             between the solves row and the move_logs row); absent on old records
//             (onSolved mints a one-off id for those).
//   puzzle, solution: 81-char lines ('.' = empty),
//   givens: clue count,
//   seed: decimal string of the u64 generator seed (debug only; absent on old
//         records). Kept so cheat mode can display it and reproduce the puzzle.
//   attempts: rejection-sampling attempts the generator spent (debug only; absent
//             on old records). Shown under cheat next to the seed.
//   grade: { rating: 0..1, bands, band: 0..4, bottleneckCount, dryFirings,
//            longestDryRun, scanWork, open, alts, camo, scarcity: number|null } --
//            the generator's difficulty grade. `rating` is the continuous within-
//            technique score; `band` is its 5-band cut (drives the thumbnail badge
//            and the play-view difficulty pill). The raw signals feed the cheat-mode
//            breakdown (`camo` is the Tier-E near-miss count). null on old records
//            (predates grading); pre-5-band records have band 0..2 and lack
//            rating/bands/open/alts.
//   value: number[81]         player placements (0 = empty),       [heavy]
//   fromForced: bool          started via "Play from Forced" (a head start was
//                             pre-placed). Tracked separately from a plain Play in
//                             the per-mode stats; absent on old records -> false.
//   centerMarks: number[][81] centred "usual" pencil notes per cell,  [heavy]
//   cornerMarks: number[][81] Snyder corner notes per cell,           [heavy]
//   (legacy records may carry `marks`; migration folds it into centerMarks)
//   history: snapshot[]  undo stack (states to undo *to*),            [heavy]
//   redo: snapshot[]     redo stack (states an undo stepped away from),[heavy]
//     a snapshot is { v: number[81], c: number[][81], n: number[][81] } --
//     value + center/corner marks-as-arrays (legacy snapshots carry `m` -> center),
//   elapsedMs: accumulated play time,  [heavy = live; index keeps a listing copy]
//   hints: number of distinct hint entries charged (mistake reveals + technique
//          locations); 0 on a hint-free solve. The dual-score axis for the daily
//          leaderboard. Mirrored in the index for a heavy-free read.
//   cheated: bool   sticky -- true once cheat mode was on at any point this game.
//                   Disqualifies the daily leaderboard (submitDaily refuses it).
//   revealedHints: string[]  the charged entry keys, for dedup across reopens/
//                            sittings, so re-viewing a revealed move is free. [heavy]
//   status: "active"|"solved",
//   createdAt, solvedAt (ms epoch, solvedAt null until solved)
// }
// Fields tagged [heavy] live in the per-game key; the rest are the index entry.
//
// Per-kind fields (all null/absent when not applicable):
//   spec: number[16]      the per-kind usage codes -- present on every spec-built
//                         game (custom/review/daily), used to re-generate a similar
//                         puzzle. Campaign builds its spec on the fly (campaign.js)
//                         and does NOT store one; imported has none.
//   specMasks: {baseline, inScope, forced}  the hint-tree masks. Stored on EVERY
//                         generated game now (campaign included -- built in JS at
//                         launch), so play.js reads them uniformly.
//   forceAny: bool        the Forced kinds are one disjunction (review/daily).
//   label: string         a short title (custom's Forced techniques; "Imported").
//   daily: { day, level } daily only -- `day` is the puzzle-day index (daily.js
//                         dayNumber), `level` the index into DAILY_LEVELS. Drives
//                         the daily page's already-played lookup.
// A CUSTOM game exposes its spec in the builder ("Open spec"); no other kind does.

// Create and persist a fresh active game from a generated puzzle. `kind` is the mode
// discriminator (see modes.js); `kindIndex`/`mode` are the campaign sub-key, `lesson`
// the review one, `spec`/`specMasks`/`label`/`daily` the spec-built extras; `seed`
// and `attempts` are the worker's debug metadata (cheat-mode display). `value` is
// an optional 81-cell starting placement (the "Play from Forced" head start —
// digits the solver placed up to the first forced technique); it defaults to a
// blank board. These are ordinary player placements, not givens, so Restart wipes
// them back to the minimal clues.
export function createGame({
  kind = null,
  kindIndex = null,
  mode = null,
  lesson = null,
  spec = null,
  specMasks = null,
  forceAny = false,
  label = null,
  daily = null,
  puzzle,
  solution,
  givens,
  seed,
  attempts,
  grade = null,
  value = null,
  fromForced = false,
}) {
  const id = newId();
  const now = Date.now();
  // Light listing entry.
  const meta = {
    id,
    // Stable per-puzzle id for the backend, minted up front (not at solve time)
    // so mid-solve move-log syncs and the eventual solve row share one key. See
    // backend.recordMoves / recordSolve.
    solve_id: newId(),
    kind,
    kindIndex,
    mode,
    lesson: lesson || null,
    spec,
    specMasks,
    forceAny: !!forceAny,
    label,
    daily: daily || null,
    puzzle,
    solution,
    givens,
    seed: seed || null,
    attempts: attempts ?? null,
    grade: grade || null,
    fromForced: !!fromForced,
    elapsedMs: 0,
    // Hint accounting (kept in the light index so the daily Submit / listing can
    // read them without a heavy load). `hints` is the number of distinct hint
    // entries the player has been charged for (see play.js: a solution-surfaced
    // mistake, or a technique's location once "Reveal where" exposed it); `cheated`
    // is the sticky disqualifier -- true once cheat mode was on at any point, which
    // bars the daily leaderboard. The dedup keys themselves live in the heavy
    // record (`revealedHints`).
    hints: 0,
    cheated: false,
    status: "active",
    createdAt: now,
    lastPlayedAt: now,
    solvedAt: null,
  };
  // Heavy per-game state.
  const heavy = {
    value: Array.isArray(value) && value.length === N ? value.slice() : new Array(N).fill(0),
    centerMarks: Array.from({ length: N }, () => []),
    cornerMarks: Array.from({ length: N }, () => []),
    history: [],
    redo: [],
    elapsedMs: 0,
    // Persistent dedup set for the hint counter: entry keys already charged, so a
    // move re-drilled (or a mistake re-viewed) across panel reopens and sittings is
    // free to look at again. The count mirrored into the index is its length.
    revealedHints: [],
  };
  const games = loadIndex();
  games.push(meta);
  saveIndex(games);
  writeHeavy(id, heavy);
  return { ...meta, ...heavy };
}

// The full merged record (index entry + heavy state). Heavy fields win on
// overlap, so elapsedMs reflects the live per-keystroke value, not the index's
// listing copy. null if the game is gone.
export function getGame(id) {
  const meta = loadIndex().find((g) => g.id === id);
  if (!meta) return null;
  return { ...meta, ...readHeavy(id) };
}

// Permanently drop a game from the store (the Continue card's "Remove"). Unlike
// solving -- which keeps the record, flipped to "solved" for the stats -- this
// erases it outright, so it's gated behind a confirmation in the UI.
export function deleteGame(id) {
  saveIndex(loadIndex().filter((g) => g.id !== id));
  deleteHeavy(id);
  deleteTrack(id); // drop the move-tracking log along with the game
}

// Drop a game's heavy board + undo/redo timeline and its move-tracking log, while
// KEEPING its light index entry. This is the retention boundary for a FINISHED
// puzzle: the index record feeds the stats forever, but the editable working state
// is meaningless once solved (a solved game is never reopened) and its move timeline
// has already been handed to the backend, so neither has any further local use.
// Called by play.onSolved and, retroactively, by reclaimStorage.
export function discardWorkingState(id) {
  deleteHeavy(id);
  deleteTrack(id);
}

// One-time-per-load sweep that enforces the retention policy on data already on the
// device. Two kinds of reclaimable keys are removed:
//   - the working state (heavy + track) of every SOLVED game -- the same drop
//     onSolved now does, applied retroactively to games solved by older builds;
//   - any heavy/track key whose game id is no longer in the index (an orphan from an
//     interrupted delete or a long-gone build).
// Active games are left fully intact. Cheap: one index parse plus a single pass over
// the key namespace. Returns the count of keys dropped (logged to the console so a
// reclaim is visible). Run at startup before the first render.
export function reclaimStorage() {
  const status = new Map(loadIndex().map((g) => [g.id, g.status]));
  // Snapshot the key list first: removing entries mid-iteration shifts the live
  // localStorage indices and would skip keys.
  const keys = [];
  try {
    for (let i = 0; i < localStorage.length; i++) keys.push(localStorage.key(i));
  } catch (err) {
    logStorageError("read", "<enumerate>", err);
    return 0;
  }
  let removed = 0;
  for (const key of keys) {
    if (key == null) continue;
    const prefix = key.startsWith(GAME_PREFIX) ? GAME_PREFIX : key.startsWith(TRACK_PREFIX) ? TRACK_PREFIX : null;
    if (!prefix) continue;
    const st = status.get(key.slice(prefix.length));
    // Unknown id -> orphan; solved -> finished, working state no longer needed.
    if (st === undefined || st === "solved") {
      try {
        localStorage.removeItem(key);
        removed++;
      } catch (err) {
        logStorageError("delete", key, err);
      }
    }
  }
  if (removed) console.info(`[storage] reclaimStorage dropped ${removed} stale key(s)`);
  return removed;
}

// Checkpoint the player's in-progress board as one keystroke's save. This is the
// hot path: it overwrites ONLY the game's heavy key (heavy must be the complete
// set of heavy fields) and never reads or rewrites the index or any other game,
// so the cost is one game's state regardless of library size.
export function saveProgress(id, heavy) {
  writeHeavy(id, heavy);
}

// Merge `patch` into the stored game and persist, routing each field to its tier
// (heavy fields to the per-game key, the rest to the index). Returns the updated
// merged record (or null if it's gone). Used for lifecycle transitions (solve,
// restart, reopen) -- NOT for per-keystroke checkpoints; those use saveProgress.
export function updateGame(id, patch) {
  const heavyPatch = {};
  const indexPatch = {};
  for (const k of Object.keys(patch)) {
    if (HEAVY_FIELDS.includes(k)) heavyPatch[k] = patch[k];
    else indexPatch[k] = patch[k];
  }
  // elapsedMs is authoritative in the heavy record (the live timer) but the index
  // keeps a copy so the start page can list play time without a heavy read.
  // Mirror it into the index whenever a caller updates it.
  if ("elapsedMs" in patch) indexPatch.elapsedMs = patch.elapsedMs;

  if (Object.keys(heavyPatch).length) {
    const heavy = readHeavy(id);
    Object.assign(heavy, heavyPatch);
    writeHeavy(id, heavy);
  }
  if (Object.keys(indexPatch).length) {
    const games = loadIndex();
    const i = games.findIndex((g) => g.id === id);
    if (i === -1) return null;
    Object.assign(games[i], indexPatch);
    saveIndex(games);
  }
  return getGame(id);
}

// Every game record (the raw index), unfiltered and unsorted. The one-time mode
// migration (modes.migrateModes) uses this to stamp `kind` on legacy records; the
// listing selectors below are the usual filtered/sorted reads.
export function allGames() {
  return loadIndex();
}

// In-progress games, most-recently-PLAYED first -- the "Continue last" target
// and the "Continue a puzzle" list. Falls back to createdAt for old records that
// predate lastPlayedAt.
export function activeGames() {
  const key = (g) => g.lastPlayedAt || g.createdAt;
  return loadIndex()
    .filter((g) => g.status === "active")
    .sort((a, b) => key(b) - key(a));
}

// The stored daily game for a given puzzle-day + difficulty level, or null if that
// difficulty hasn't been played today. Drives the daily page's per-difficulty
// state (Play when null, Continue while active, Solved once solved). Reads the
// index only -- the listing carries the `daily` tag and the status. If two exist
// for the same key (shouldn't happen -- the page only creates one), the most
// recently created wins.
export function dailyGame(day, level) {
  let found = null;
  for (const g of loadIndex()) {
    if (g.daily && g.daily.day === day && g.daily.level === level) {
      if (!found || (g.createdAt || 0) > (found.createdAt || 0)) found = g;
    }
  }
  return found;
}

// Solved games, most-recently-SOLVED first -- the Stats page history list.
// Falls back to createdAt for any record missing solvedAt.
export function solvedGames() {
  const key = (g) => g.solvedAt || g.createdAt;
  return loadIndex()
    .filter((g) => g.status === "solved")
    .sort((a, b) => key(b) - key(a));
}

// ---- Aggregates ----
// The per-mode stat buckets that feed the tree badges and the Stats page now live in
// modes.js (statsBuckets over solvedGames()), so they can dispatch through the mode
// registry. store.js stays mode-agnostic: it just serves the raw game lists above.
