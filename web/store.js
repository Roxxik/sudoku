"use strict";

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

function gameKey(id) {
  return `sudoku.game.${id}`;
}

// The fields that live in the per-game heavy key rather than the index. These are
// the ones that change as you play; everything else is listing metadata.
const HEAVY_FIELDS = ["value", "centerMarks", "cornerMarks", "history", "redo", "elapsedMs"];

// ---- Low-level load/save ----

// The index is small (a handful of scalar fields plus two 81-char strings per
// game) and always read/written whole. It carries NO board state or history.
function loadIndex() {
  let raw;
  try {
    raw = localStorage.getItem(INDEX_KEY);
  } catch {
    return [];
  }
  if (!raw) return [];
  let games;
  try {
    games = JSON.parse(raw);
  } catch {
    // Corrupt/old data shouldn't brick the app; start fresh.
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
  } catch {
    // Quota or privacy mode: play still works this session, just isn't saved.
  }
}

// Read one game's heavy state ({} if absent/corrupt -- getGame merges over the
// index entry, and loadGame tolerates missing fields).
function readHeavy(id) {
  try {
    const raw = localStorage.getItem(gameKey(id));
    return raw ? JSON.parse(raw) || {} : {};
  } catch {
    return {};
  }
}

function writeHeavy(id, heavy) {
  try {
    localStorage.setItem(gameKey(id), JSON.stringify(heavy));
  } catch {
    // Quota or privacy mode: tolerated, as with the index.
  }
}

function deleteHeavy(id) {
  try {
    localStorage.removeItem(gameKey(id));
  } catch {
    /* ignore */
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
//   id, kindIndex, mode: "train"|"drill"|"custom",   (Beginner uses "train")
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
//   status: "active"|"solved",
//   createdAt, solvedAt (ms epoch, solvedAt null until solved)
// }
// Fields tagged [heavy] live in the per-game key; the rest are the index entry.
//
// A CUSTOM game (mode "custom", built in custom.js) has no single curriculum
// kind: kindIndex is null and three extra fields carry its spec --
//   spec: number[16]      per-kind usage codes (re-generate the same spec),
//   specMasks: {baseline, inScope, forced}  for the hint tree,
//   label: string         a short title from its Forced techniques.
// Campaign games leave all three null.

// Create and persist a fresh active game from a generated puzzle. `kindIndex` is
// null for a custom game; `spec`/`specMasks`/`label` are the custom extras; `seed`
// and `attempts` are the worker's debug metadata (cheat-mode display). `value` is
// an optional 81-cell starting placement (the "Play from Forced" head start —
// digits the solver placed up to the first forced technique); it defaults to a
// blank board. These are ordinary player placements, not givens, so Restart wipes
// them back to the minimal clues.
export function createGame({
  kindIndex = null,
  mode,
  spec = null,
  specMasks = null,
  forceAny = false,
  label = null,
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
    kindIndex,
    mode,
    spec,
    specMasks,
    forceAny: !!forceAny,
    label,
    puzzle,
    solution,
    givens,
    seed: seed || null,
    attempts: attempts ?? null,
    grade: grade || null,
    fromForced: !!fromForced,
    elapsedMs: 0,
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

// In-progress games, most-recently-PLAYED first -- the "Continue last" target
// and the "Continue a puzzle" list. Falls back to createdAt for old records that
// predate lastPlayedAt.
export function activeGames() {
  const key = (g) => g.lastPlayedAt || g.createdAt;
  return loadIndex()
    .filter((g) => g.status === "active")
    .sort((a, b) => key(b) - key(a));
}

// Solved games, most-recently-SOLVED first -- the Stats page history list.
// Falls back to createdAt for any record missing solvedAt.
export function solvedGames() {
  const key = (g) => g.solvedAt || g.createdAt;
  return loadIndex()
    .filter((g) => g.status === "solved")
    .sort((a, b) => key(b) - key(a));
}

// ---- Aggregates for badges and the Stats page ----

// Per (kindIndex, start) summary over SOLVED games, where `start` splits each mode
// by whether it was a plain Play or a "Play from Forced":
//   { [kindIndex]: { train, trainForced, drill, drillForced } }
// each value { count, bestMs, avgMs, lastMs }. Starts with no solves are omitted.
// A plain Play and a forced-start solve are distinct accomplishments, so they
// never share a bucket. bestMs is the fastest solve, avgMs the mean.
export function statsByKind() {
  const out = {};
  for (const g of loadIndex()) {
    if (g.status !== "solved") continue;
    // Custom-spec games aren't a single curriculum kind, so they stay out of the
    // per-kind badges and Stats table.
    if (typeof g.kindIndex !== "number") continue;
    const k = (out[g.kindIndex] ||= {});
    const key = g.mode + (g.fromForced ? "Forced" : "");
    const m = (k[key] ||= { count: 0, bestMs: Infinity, avgMs: 0, lastMs: 0, _sum: 0 });
    m.count += 1;
    m._sum += g.elapsedMs;
    m.avgMs = m._sum / m.count;
    m.bestMs = Math.min(m.bestMs, g.elapsedMs);
    if ((g.solvedAt || 0) >= (m._lastAt || 0)) {
      m.lastMs = g.elapsedMs;
      m._lastAt = g.solvedAt || 0;
    }
  }
  return out;
}

// Total solved count for one kind across every mode/start -- the technique-row
// and tier/branch badges.
export function solvedCountForKind(stats, kindIndex) {
  const k = stats[kindIndex];
  if (!k) return 0;
  let n = 0;
  for (const m of Object.values(k)) n += m.count || 0;
  return n;
}
