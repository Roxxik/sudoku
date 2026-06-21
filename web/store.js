"use strict";

// Persistence for played puzzles. Everything the start page shows -- the
// in-progress "Continue" list, the per-mode best times, the solved-count badges
// in the tree, and the Stats page -- is derived from one flat list of game
// records in localStorage. A game is NEVER deleted when solved: it flips to
// "solved" with its final time attached, so the same record feeds both the
// timings and the aggregate stats.

const KEY = "sudoku.games.v1";
const N = 81;

// ---- Low-level load/save ----
// The whole list lives under one key. It's small (a handful of 81-char strings
// per game) and always read/written whole; no need for IndexedDB yet.
function loadAll() {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return [];
    const games = JSON.parse(raw);
    return Array.isArray(games) ? games : [];
  } catch {
    // Corrupt/old data shouldn't brick the app; start fresh.
    return [];
  }
}

function saveAll(games) {
  try {
    localStorage.setItem(KEY, JSON.stringify(games));
  } catch {
    // Quota or privacy mode: play still works this session, just isn't saved.
  }
}

// A short unique id without pulling in a uuid dep. crypto.randomUUID when
// available, else a timestamp+random fallback.
function newId() {
  if (typeof crypto !== "undefined" && crypto.randomUUID) return crypto.randomUUID();
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

// ---- Game records ----
// {
//   id, kindIndex, mode: "train"|"drill"|"custom",   (Beginner uses "train")
//   puzzle, solution: 81-char lines ('.' = empty),
//   givens: clue count,
//   seed: decimal string of the u64 generator seed (debug only; absent on old
//         records). Kept so cheat mode can display it and reproduce the puzzle.
//   attempts: rejection-sampling attempts the generator spent (debug only; absent
//             on old records). Shown under cheat next to the seed.
//   value: number[81]         player placements (0 = empty),
//   fromForced: bool          started via "Play from Forced" (a head start was
//                             pre-placed). Tracked separately from a plain Play in
//                             the per-mode stats; absent on old records -> false.
//   centerMarks: number[][81] centred "usual" pencil notes per cell,
//   cornerMarks: number[][81] Snyder corner notes per cell,
//   (legacy records may carry `marks`; play.js loads it as centerMarks)
//   history: snapshot[]  undo stack (states to undo *to*),
//   redo: snapshot[]     redo stack (states an undo stepped away from),
//     a snapshot is { v: number[81], c: number[][81], n: number[][81] } --
//     value + center/corner marks-as-arrays (legacy snapshots carry `m` -> center),
//   elapsedMs: accumulated play time,
//   status: "active"|"solved",
//   createdAt, solvedAt (ms epoch, solvedAt null until solved)
// }
// (Old records predating history/redo simply lack those fields -> treated as
// empty stacks on load.)
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
  value = null,
  fromForced = false,
}) {
  const game = {
    id: newId(),
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
    fromForced: !!fromForced,
    value: Array.isArray(value) && value.length === N ? value.slice() : new Array(N).fill(0),
    centerMarks: Array.from({ length: N }, () => []),
    cornerMarks: Array.from({ length: N }, () => []),
    history: [],
    redo: [],
    elapsedMs: 0,
    status: "active",
    createdAt: Date.now(),
    lastPlayedAt: Date.now(),
    solvedAt: null,
  };
  const games = loadAll();
  games.push(game);
  saveAll(games);
  return game;
}

export function getGame(id) {
  return loadAll().find((g) => g.id === id) || null;
}

// Permanently drop a game from the store (the Continue card's "Remove"). Unlike
// solving -- which keeps the record, flipped to "solved" for the stats -- this
// erases it outright, so it's gated behind a confirmation in the UI.
export function deleteGame(id) {
  saveAll(loadAll().filter((g) => g.id !== id));
}

// Merge `patch` into the stored game and persist. Returns the updated record (or
// null if it's gone). Used to checkpoint player work, elapsed time, and the
// solved transition.
export function updateGame(id, patch) {
  const games = loadAll();
  const i = games.findIndex((g) => g.id === id);
  if (i === -1) return null;
  games[i] = { ...games[i], ...patch };
  saveAll(games);
  return games[i];
}

// In-progress games, most-recently-PLAYED first -- the "Continue last" target
// and the "Continue a puzzle" list. Falls back to createdAt for old records that
// predate lastPlayedAt.
export function activeGames() {
  const key = (g) => g.lastPlayedAt || g.createdAt;
  return loadAll()
    .filter((g) => g.status === "active")
    .sort((a, b) => key(b) - key(a));
}

// Solved games, most-recently-SOLVED first -- the Stats page history list.
// Falls back to createdAt for any record missing solvedAt.
export function solvedGames() {
  const key = (g) => g.solvedAt || g.createdAt;
  return loadAll()
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
  for (const g of loadAll()) {
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
