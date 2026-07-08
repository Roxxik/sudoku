"use strict";

// Shared vocabulary for the per-technique generation spec the UI builds.
//
// Mirrors generator-lab's `Usage`: a technique is Off (out of scope), Allowed
// (part of the solvable toolbox), Forced (must fire at least once), or Conceded
// (granted only to the isolation check, never promised solvable). The custom-spec
// builder (custom.js) edits a 16-entry usage array; the generation worker rebuilds
// an explicit `Spec` from it (see gen_worker.rs), and the hint tree classifies
// steps against the masks below.

export const OFF = 0;
export const ALLOW = 1;
export const FORCE = 2;
export const CONCEDE = 3;

// Number of technique kinds (lab::kinds::NUM). The curriculum has one entry per
// kind, so this is just its length; named here so spec helpers don't depend on it.
export const NUM_KINDS = 16;

// The three bitmasks the hint tree reads, derived from a usage array.
//   baseline = Allowed | Forced   (the intended toolbox -> Allowed/Forced hints)
//   inScope  = baseline | Conceded
//   forced   = Forced only
// Matches the wasm `specMasks` shape, so a custom game's stored masks drop into
// the same play.js classification as a campaign game's.
export function masksFromUsages(usages) {
  let baseline = 0;
  let inScope = 0;
  let forced = 0;
  for (let i = 0; i < usages.length; i++) {
    const u = usages[i];
    if (u === OFF) continue;
    inScope |= 1 << i;
    if (u === ALLOW || u === FORCE) baseline |= 1 << i;
    if (u === FORCE) forced |= 1 << i;
  }
  return { baseline, inScope, forced };
}

// Indices of the Forced techniques -- the spec's headline requirement, used to
// label the generated puzzle.
export function forcedIndices(usages) {
  const out = [];
  for (let i = 0; i < usages.length; i++) if (usages[i] === FORCE) out.push(i);
  return out;
}

// A spec is only meaningful to generate once it requires something.
export function hasForce(usages) {
  return usages.some((u) => u === FORCE);
}
