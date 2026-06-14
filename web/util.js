"use strict";

// Format a millisecond duration as m:ss (or h:mm:ss past an hour). Used for the
// solved time and the Stats / badge timings.
export function formatDuration(ms) {
  const total = Math.max(0, Math.round(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const pad = (n) => String(n).padStart(2, "0");
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`;
}

// Title-case display name for a curriculum technique id (kebab-case from
// `kinds::NAMES`). Wing/fish initialisms are upper-cased; "lc" expands to
// "Locked Candidates"; everything else is word-capitalized.
const DISPLAY = {
  "naked-single": "Naked Single",
  "hidden-single": "Hidden Single",
  "lc-pointing": "Locked Candidates (Pointing)",
  "lc-claiming": "Locked Candidates (Claiming)",
  "naked-pair": "Naked Pair",
  "hidden-pair": "Hidden Pair",
  "naked-triple": "Naked Triple",
  "hidden-triple": "Hidden Triple",
  "naked-quad": "Naked Quad",
  "hidden-quad": "Hidden Quad",
  "x-wing": "X-Wing",
  swordfish: "Swordfish",
  jellyfish: "Jellyfish",
  "xy-wing": "XY-Wing",
  "xyz-wing": "XYZ-Wing",
  "w-wing": "W-Wing",
};

export function techniqueName(id) {
  if (DISPLAY[id]) return DISPLAY[id];
  // Fallback for any kind not in the table: word-capitalize the kebab id.
  return id
    .split("-")
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
    .join(" ");
}

// Branch display labels for the campaign tree.
export const BRANCH_LABEL = {
  trunk: "Trunk",
  fish: "Single-digit (Fish)",
  subset: "Subsets",
  bivalue: "Bivalue chains",
};

// Tier display labels, in curriculum order.
export const TIER_ORDER = ["beginner", "intermediate", "expert", "master"];
export const TIER_LABEL = {
  beginner: "Beginner",
  intermediate: "Intermediate",
  expert: "Expert",
  master: "Master",
};
