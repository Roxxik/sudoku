"use strict";

// The campaign's per-technique generation spec, built in JS.
//
// This is the frontend's own copy of Rust's `Spec::train_isolated` /
// `drill_isolated` (generator-lab/src/spec/mod.rs) — a puzzle that FORCES one
// campaign technique, ALLOWS what a player may lean on to reach it, and CONCEDES
// the easier peers it must not be sidestepped by. It is pure taxonomy math over the
// curriculum's tier / branch / difficulty, so it lives here rather than crossing the
// wasm boundary: campaign games now go through the same explicit-spec generation and
// hint-tree masks as Custom / Review / Daily (see spec.js `masksFromUsages`, and the
// id-keyed request in gen.js). The Rust builders stay as the grader's reference copy
// and MAY drift from this — see the header of web/curriculum.js.

import { OFF, ALLOW, FORCE, CONCEDE, NUM_KINDS } from "./spec.js";
import { TIER_ORDER } from "./util.js";

// Tier as a comparable rank (Beginner < Intermediate < Expert < Master), matching
// Rust's `Tier` ordering.
function tierRank(tier) {
  return TIER_ORDER.indexOf(tier);
}

// The usage array (one code per kindIndex) for a campaign technique in a mode
// ("train" | "drill"). Mirrors Rust's train_isolated / drill_isolated exactly:
//   Train: allow the whole Trunk up to the target's tier, plus the simpler-or-equal
//     same-branch Expert techniques; concede the easier cross-branch Expert peers so
//     the target can't be sidestepped by an easier other-branch technique.
//   Drill: allow every easier tier in full; concede the same-tier peers (all of a
//     flat Intermediate tier, or every easier-difficulty Expert peer in any branch),
//     isolating the target against what a player at its level would reach for.
export function campaignUsages(curriculum, kindIndex, mode) {
  const usages = new Array(NUM_KINDS).fill(OFF);
  const target = curriculum.find((t) => t.kindIndex === kindIndex);
  if (!target) return usages;
  const targetRank = tierRank(target.tier);
  const intermediateRank = tierRank("intermediate");
  const drill = mode === "drill";

  for (const t of curriculum) {
    if (t.kindIndex === kindIndex) continue; // the target is forced below
    const tt = tierRank(t.tier);
    if (drill) {
      if (tt < targetRank) {
        usages[t.kindIndex] = ALLOW; // easier tiers allowed in full
      } else if (tt === targetRank) {
        // Beginner is train-only (nothing to concede). Intermediate: concede every
        // peer. Expert/Master: concede every easier-difficulty peer, any branch.
        const concede =
          target.tier === "intermediate" ||
          ((target.tier === "expert" || target.tier === "master") &&
            t.difficulty < target.difficulty);
        if (concede) usages[t.kindIndex] = CONCEDE;
      }
      // tt > targetRank: out of scope.
    } else {
      if (tt > targetRank) continue;
      const allowed =
        tt <= intermediateRank
          ? true // Trunk: always available up to the target's tier
          : t.branch === target.branch && t.difficulty <= target.difficulty;
      if (allowed) usages[t.kindIndex] = ALLOW;
      else if (tt === targetRank && t.difficulty < target.difficulty)
        usages[t.kindIndex] = CONCEDE; // easier cross-branch Expert peer
    }
  }
  usages[kindIndex] = FORCE;
  return usages;
}
