"use strict";

// Review lessons: the Expert tier's mixed-technique practice, shared between the
// campaign tree (home.js) and the custom-spec builder (custom.js, which offers
// each lesson/mode as a starting template). A lesson groups a few similar-
// difficulty techniques from different branches into one disjunctive set -- a
// puzzle needs *any one* of them (the `force_any` path: the generator pins one
// member per puzzle, so the set's techniques come up evenly across plays). Kept
// in one module so the two callers can't drift on the set membership or the
// usage-array construction.

import { OFF, ALLOW, FORCE, CONCEDE, NUM_KINDS } from "./spec.js";
import { techniqueName } from "./util.js";

// The Review section lives under this tier (a sibling of its branches).
export const REVIEW_TIER = "expert";

// Each lesson groups similar-difficulty techniques (referenced by curriculum id)
// into one disjunctive set. Tuned to the user's grouping by shape and difficulty;
// the lessons are cleanly difficulty-banded (Lesson 1 easiest), which is what lets
// `easierExpertIds` define a Drill variant for every lesson past the first.
export const REVIEW_LESSONS = [
  { name: "Lesson 1", ids: ["naked-pair", "x-wing", "hidden-pair"] },
  { name: "Lesson 2", ids: ["naked-triple", "swordfish", "hidden-triple"] },
  { name: "Lesson 3", ids: ["xy-wing", "xyz-wing"] },
  { name: "Lesson 4", ids: ["naked-quad", "jellyfish", "hidden-quad"] },
];

// The basics every Review puzzle leans on (the whole Trunk: singles + both
// locked-candidates orientations), allowed so a puzzle is solvable up to the
// technique it forces. Mirrors what the train builder grants; including both LC
// orientations keeps the generator's batched fast path eligible.
const REVIEW_BASE_IDS = ["naked-single", "hidden-single", "lc-pointing", "lc-claiming"];

// The usage array for a lesson in a mode: the basics Allowed, the lesson's set
// Forced (folded into one `force_any` set in the worker when forceAny rides the
// request -- so pair this with forceAny = true), and the easier Expert techniques
// scoped by mode -- Allowed in Train (lean-ons that may be needed), Conceded in
// Drill (isolate the lesson's technique against them: solvable without them, but
// they can't sidestep the requirement).
export function lessonUsages(curriculum, lesson, mode) {
  const usages = new Array(NUM_KINDS).fill(OFF);
  for (const id of REVIEW_BASE_IDS) setUsage(curriculum, usages, id, ALLOW);
  for (const id of easierExpertIds(curriculum, lesson)) {
    setUsage(curriculum, usages, id, mode === "drill" ? CONCEDE : ALLOW);
  }
  for (const id of lesson.ids) setUsage(curriculum, usages, id, FORCE);
  return usages;
}

// Expert techniques strictly easier (by difficulty, any branch) than every member
// of the lesson's set -- the lower lessons' techniques, plus W-Wing for Lesson 4.
// These are the Train/Drill lever; empty for Lesson 1 (nothing in Expert is
// easier), which is why it has no Drill. Computed from the curriculum so it tracks
// difficulty changes rather than hardcoding the membership.
export function easierExpertIds(curriculum, lesson) {
  const threshold = Math.min(...lesson.ids.map((id) => difficultyForId(curriculum, id)));
  return curriculum
    .filter((t) => t.tier === "expert" && t.difficulty < threshold)
    .map((t) => t.id);
}

// A lesson has a Drill variant iff it has easier Expert techniques to isolate
// against (every lesson past the first).
export function lessonHasDrill(curriculum, lesson) {
  return easierExpertIds(curriculum, lesson).length > 0;
}

// The set's techniques as a " · " subtitle (matching the branch/tier subtitles).
export function lessonSubtitle(lesson) {
  return lesson.ids.map((id) => techniqueName(id)).join(" · ");
}

// The stored game's label: the set joined with "or", matching the custom builder's
// any-mode label so an in-progress Review puzzle reads as its disjunction.
export function lessonLabel(lesson) {
  return lesson.ids.map((id) => techniqueName(id)).join(" or ");
}

// The set's technique names as a natural-language list ("A, B, or C") for prose.
export function joinNames(lesson) {
  const names = lesson.ids.map((id) => techniqueName(id));
  if (names.length <= 1) return names.join("");
  if (names.length === 2) return `${names[0]} or ${names[1]}`;
  return `${names.slice(0, -1).join(", ")}, or ${names[names.length - 1]}`;
}

function setUsage(curriculum, usages, id, code) {
  const k = kindIndexForId(curriculum, id);
  if (k != null) usages[k] = code;
}

function kindIndexForId(curriculum, id) {
  const t = curriculum.find((e) => e.id === id);
  return t ? t.kindIndex : null;
}

function difficultyForId(curriculum, id) {
  const t = curriculum.find((e) => e.id === id);
  return t ? t.difficulty : 0;
}
