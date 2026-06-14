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
//   id, kindIndex, mode: "train"|"drill",   (Beginner uses "train")
//   puzzle, solution: 81-char lines ('.' = empty),
//   givens: clue count,
//   value: number[81]   player placements (0 = empty),
//   marks: number[][81]  pencilled candidate digits per cell,
//   elapsedMs: accumulated play time,
//   status: "active"|"solved",
//   createdAt, solvedAt (ms epoch, solvedAt null until solved)
// }

// Create and persist a fresh active game from a generated puzzle.
export function createGame({ kindIndex, mode, puzzle, solution, givens }) {
  const game = {
    id: newId(),
    kindIndex,
    mode,
    puzzle,
    solution,
    givens,
    value: new Array(N).fill(0),
    marks: Array.from({ length: N }, () => []),
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

// ---- Aggregates for badges and the Stats page ----

// Per (kindIndex, mode) summary over SOLVED games:
//   { [kindIndex]: { train: {count, bestMs, avgMs, lastMs}, drill: {...} } }
// Modes with no solves are omitted. bestMs is the fastest solve, avgMs the mean.
export function statsByKind() {
  const out = {};
  for (const g of loadAll()) {
    if (g.status !== "solved") continue;
    const k = (out[g.kindIndex] ||= {});
    const m = (k[g.mode] ||= { count: 0, bestMs: Infinity, avgMs: 0, lastMs: 0, _sum: 0 });
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

// Total solved count for one kind across both modes -- the technique-row badge.
export function solvedCountForKind(stats, kindIndex) {
  const k = stats[kindIndex];
  if (!k) return 0;
  return (k.train?.count || 0) + (k.drill?.count || 0);
}
