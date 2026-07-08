"use strict";

// The player-facing campaign taxonomy: one entry per technique kind, in the order
// the campaign tree and stats iterate them.
//
// HAND-AUTHORED IN JS. This used to be generated at build from Rust `lab::kinds`
// (a Trunk pre_build hook); it is now the frontend's own source of truth. The Rust
// side (`lab::kinds`, `Spec::train_isolated`/`drill_isolated`, the grader) keeps a
// parallel copy, and the two MAY DRIFT on display metadata (`difficulty`, `tier`,
// `branch`, `hasDrill`) — that is acceptable.
//
// The ONE cross-language contract is the kebab-case `id`: it is what the frontend
// sends to the generator worker (see gen.js -> the id-keyed spec), which maps it
// back to its own kind via `kinds::NAMES`. Keep the ids in step with Rust's names.
//
// `kindIndex` is a JS-internal ordinal (usage-array position + display order). It
// no longer crosses the wasm boundary, so its ordering is a JS concern only — but
// it must stay consistent with play.js's hint-tree kind map, which is DERIVED from
// this array (see play.js `LAB_KIND`) precisely so there is one JS source for it.
//
// Fields: kindIndex (ordinal), id (kebab contract), difficulty (player-facing
// score; drives the tier cut and the within-branch ordering), tier, branch,
// hasDrill (whether a Drill variant differs from Train — false for Beginner and
// the easiest kind of each Expert branch).
export default [
  { kindIndex: 0, id: "naked-single", difficulty: 15, tier: "intermediate", branch: "trunk", hasDrill: true },
  { kindIndex: 1, id: "hidden-single", difficulty: 5, tier: "beginner", branch: "trunk", hasDrill: false },
  { kindIndex: 2, id: "lc-pointing", difficulty: 22, tier: "intermediate", branch: "trunk", hasDrill: true },
  { kindIndex: 3, id: "lc-claiming", difficulty: 26, tier: "intermediate", branch: "trunk", hasDrill: true },
  { kindIndex: 4, id: "naked-pair", difficulty: 32, tier: "expert", branch: "subset", hasDrill: false },
  { kindIndex: 5, id: "hidden-pair", difficulty: 44, tier: "expert", branch: "subset", hasDrill: true },
  { kindIndex: 6, id: "naked-triple", difficulty: 50, tier: "expert", branch: "subset", hasDrill: true },
  { kindIndex: 7, id: "hidden-triple", difficulty: 62, tier: "expert", branch: "subset", hasDrill: true },
  { kindIndex: 8, id: "naked-quad", difficulty: 82, tier: "expert", branch: "subset", hasDrill: true },
  { kindIndex: 9, id: "hidden-quad", difficulty: 92, tier: "expert", branch: "subset", hasDrill: true },
  { kindIndex: 10, id: "x-wing", difficulty: 38, tier: "expert", branch: "fish", hasDrill: false },
  { kindIndex: 11, id: "swordfish", difficulty: 56, tier: "expert", branch: "fish", hasDrill: true },
  { kindIndex: 12, id: "jellyfish", difficulty: 86, tier: "expert", branch: "fish", hasDrill: true },
  { kindIndex: 13, id: "xy-wing", difficulty: 68, tier: "expert", branch: "bivalue", hasDrill: false },
  { kindIndex: 14, id: "xyz-wing", difficulty: 72, tier: "expert", branch: "bivalue", hasDrill: true },
  { kindIndex: 15, id: "w-wing", difficulty: 76, tier: "expert", branch: "bivalue", hasDrill: true },
];
