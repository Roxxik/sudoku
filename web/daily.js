"use strict";

// Puzzle of the day: four fixed difficulties, each a deterministic puzzle derived
// from the calendar date so every device generates the SAME puzzle for a given day
// and difficulty. There is no server feed (yet -- a cache is planned); the seed is
// computed here and handed to the generation worker, which pins its RNG to it.
//
// Each difficulty is an ordinary `force_any` custom spec (the same path the Expert
// "Review lessons" use, see review.js): a set of techniques of which the seed pins
// exactly one to force, with the rest -- and the easier basics -- merely allowed.
// So "force one, allow the others" falls straight out of the existing generator.

import { OFF, ALLOW, FORCE, NUM_KINDS } from "./spec.js";

// The day boundary is 02:30 UTC (not midnight), so a new puzzle lands at a time
// that reads as "the small hours" across the friends' timezones rather than
// mid-evening. 2.5 hours in ms.
const DAILY_RESET_MS = 2.5 * 60 * 60 * 1000;
const DAY_MS = 24 * 60 * 60 * 1000;

// The trunk basics every difficulty leans on (singles + both locked-candidates
// orientations) -- mirrors review.js's REVIEW_BASE_IDS. Allowed so the puzzle is
// solvable up to whatever it forces; including both LC orientations keeps the
// generator's batched fast path eligible.
const TRUNK_BASE_IDS = ["naked-single", "hidden-single", "lc-pointing", "lc-claiming"];

// The four daily difficulties, in ascending order (the index is also the per-day
// seed variant, so reordering this list would reseed every difficulty -- keep it
// stable). Each level forces ONE of its `force` set (chosen per-day by the seed,
// via the worker's force_any) and additionally Allows its `allow` techniques; the
// non-pinned `force` members are auto-allowed by force_any, so "force one, allow
// the rest" needs no extra entry. Everything else is Off.
//   Beginner     -- a singles-only puzzle (hidden or naked single is the hardest step).
//   Intermediate -- one of the three Intermediate techniques, the basics behind it.
//   Expert I     -- pairs or an X-Wing (Review Lesson 1's set), trunk behind it.
//   Expert II    -- one of the harder Expert techniques (Review Lessons 2-4 plus
//                   W-Wing); everything easier is allowed.
export const DAILY_LEVELS = [
  {
    key: "beginner",
    name: "Beginner",
    blurb: "A gentle puzzle solved with singles alone.",
    force: ["hidden-single", "naked-single"],
    allow: [],
  },
  {
    key: "intermediate",
    name: "Intermediate",
    blurb: "Needs one of the Intermediate techniques: a naked single, or a locked candidate.",
    force: ["naked-single", "lc-pointing", "lc-claiming"],
    allow: ["hidden-single"],
  },
  {
    key: "expert1",
    name: "Expert I",
    blurb: "Needs a pair or an X-Wing, on top of the basics.",
    force: ["naked-pair", "hidden-pair", "x-wing"],
    allow: TRUNK_BASE_IDS,
  },
  {
    key: "expert2",
    name: "Expert II",
    blurb: "Needs one of the hardest techniques: a triple or quad, a bigger fish, or a wing.",
    force: [
      "naked-triple",
      "hidden-triple",
      "swordfish",
      "xy-wing",
      "xyz-wing",
      "naked-quad",
      "hidden-quad",
      "jellyfish",
      "w-wing",
    ],
    // Everything easier than the forced set is allowed (the basics plus the
    // Expert I techniques).
    allow: [...TRUNK_BASE_IDS, "naked-pair", "hidden-pair", "x-wing"],
  },
];

// The current puzzle-day index: an integer that ticks over at 02:30 UTC. Stable
// for a whole day, so it keys both the seed and the "already played today" lookup.
// `now` is overridable for testing; defaults to the wall clock.
export function dayNumber(now = Date.now()) {
  return Math.floor((now - DAILY_RESET_MS) / DAY_MS);
}

// A human label for a puzzle-day -- the calendar date the day belongs to (shown on
// the daily page and on a daily game's Continue / history card). The day's
// representative instant is its 02:30 UTC boundary; formatting that in UTC yields
// the right calendar date regardless of the viewer's timezone.
export function dayLabel(day) {
  const instant = day * DAY_MS + DAILY_RESET_MS;
  return new Date(instant).toLocaleDateString(undefined, {
    timeZone: "UTC",
    weekday: "short",
    day: "numeric",
    month: "short",
  });
}

// The difficulty name for a stored daily game's level index (DAILY_LEVELS order),
// or "" if out of range (a record from a future build with more levels).
export function levelName(index) {
  return DAILY_LEVELS[index]?.name || "";
}

// The pinned RNG seed for (day, level index), as a u64 decimal string (the form
// the worker parses and returns). A splitmix64 hash of the day and variant gives a
// well-mixed seed that differs per difficulty, so the four daily puzzles are
// unrelated. BigInt throughout: a u64 won't fit a JS Number.
export function dailySeed(day, index) {
  return splitmix64(BigInt(day) * 16n + BigInt(index)).toString();
}

const U64_MASK = (1n << 64n) - 1n;

// Standard splitmix64 finalizer over a 64-bit input -- a cheap, well-distributed
// integer hash. Masked to 64 bits at each step so the result is a valid u64.
function splitmix64(x) {
  let z = (x + 0x9e3779b97f4a7c15n) & U64_MASK;
  z = ((z ^ (z >> 30n)) * 0xbf58476d1ce4e5b9n) & U64_MASK;
  z = ((z ^ (z >> 27n)) * 0x94d049bb133111ebn) & U64_MASK;
  return (z ^ (z >> 31n)) & U64_MASK;
}

// The per-kind usage array for a difficulty: its basics + non-forced set members
// Allowed, its set Forced (folded into one force_any in the worker when forceAny
// rides the request -- so pair this with forceAny = true). Built from the
// curriculum so the ids resolve to the live kind indices.
export function dailyUsages(curriculum, level) {
  const usages = new Array(NUM_KINDS).fill(OFF);
  for (const id of level.allow) setUsage(curriculum, usages, id, ALLOW);
  for (const id of level.force) setUsage(curriculum, usages, id, FORCE);
  return usages;
}

// A short stored label for a daily game (a fallback; the Continue / history cards
// render "Daily · date · difficulty" from the game's `daily` tag instead).
export function dailyLabel(level) {
  return `Daily ${level.name}`;
}

function setUsage(curriculum, usages, id, code) {
  const t = curriculum.find((e) => e.id === id);
  if (t) usages[t.kindIndex] = code;
}
