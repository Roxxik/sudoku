"use strict";

// App-wide settings, persisted to localStorage. Shared by the play view (which
// reads them while you place) and the settings screen (home.js, which reads and
// writes them). Kept DOM-free so there is one source of truth per key and its
// default -- mirrors cheat.js.

const ELIMINATE_KEY = "sudoku.settings.eliminateCandidates";
const SHOW_TIMER_KEY = "sudoku.settings.showTimer";
const HIGHLIGHT_KEY = "sudoku.settings.highlight";
const DISABLE_FINISHED_KEY = "sudoku.settings.disableFinishedDigits";
const HINT_FROM_MARKS_KEY = "sudoku.settings.hintFromMarks";
// Keep this string in sync with the inline theme bootstrap in index.html, which
// reads it before any module loads (to paint the right theme without a flash).
const LIGHT_MODE_KEY = "sudoku.settings.lightMode";

function stored(key) {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function store(key, on) {
  try {
    localStorage.setItem(key, on ? "1" : "0");
  } catch {
    // Quota or privacy mode: the setting just won't persist this session.
  }
}

// Whether placing a digit strikes it from peers' Center and Corner notes.
// Default OFF -- absence of the key reads as off. Governs placement in every mode;
// "Apply easiest"/"Apply" (cheat-only buttons) eliminate regardless of this
// setting (see play.js).
export function eliminateCandidatesOn() {
  return stored(ELIMINATE_KEY) === "1";
}

export function setEliminateCandidates(on) {
  store(ELIMINATE_KEY, on);
}

// Whether the play view shows a running elapsed-time readout above the board.
// Default OFF. The solve clock always runs (for the solved time + stats); this
// only governs whether it's displayed.
export function showTimerOn() {
  return stored(SHOW_TIMER_KEY) === "1";
}

export function setShowTimer(on) {
  store(SHOW_TIMER_KEY, on);
}

// How the board highlights the active/selected digit. Peer (row/col/box)
// highlighting is independent and always shown; this only governs the same-digit
// highlight:
//   "spots"  -- everything "all" does, PLUS tint the empty cells where the digit
//               can still go: board-legal (no peer holds it) and not ruled out
//               by the player's own center / corner notes
//   "all"    -- light up matching placed cells AND matching pencil marks (default)
//   "placed" -- light up only matching placed cells, leave the marks plain
//   "none"   -- no same-digit highlight at all
// Default "all" -- an unknown / absent key reads back as "all".
export const HIGHLIGHT_MODES = ["spots", "all", "placed", "none"];

export function highlightMode() {
  const v = stored(HIGHLIGHT_KEY);
  return HIGHLIGHT_MODES.includes(v) ? v : "all";
}

export function setHighlightMode(mode) {
  try {
    localStorage.setItem(HIGHLIGHT_KEY, mode);
  } catch {
    // Quota or privacy mode: the setting just won't persist this session.
  }
}

// Whether the input pad greys out a digit button once all nine of that digit are
// placed. Default ON -- an absent key reads as on. When on, a finished digit
// can't be tapped (so it also can't stay pen-locked -- the play view drops the
// lock the moment it completes).
export function disableFinishedDigitsOn() {
  return stored(DISABLE_FINISHED_KEY) !== "0";
}

export function setDisableFinishedDigits(on) {
  store(DISABLE_FINISHED_KEY, on);
}

// What the hint engine reasons over:
//   on  -- digits + marks: fold the player's Center notes (a candidate list) and
//          Corner (Snyder) pins into each cell's candidate set, so hints follow
//          the player's own deductions (default).
//   off -- placed digits alone: ignore notes entirely and re-derive candidates
//          from placements.
// Default ON -- an absent key reads as on.
export function hintFromMarksOn() {
  return stored(HINT_FROM_MARKS_KEY) !== "0";
}

export function setHintFromMarks(on) {
  store(HINT_FROM_MARKS_KEY, on);
}

// The colour theme: on -> light (default), off -> dark. Default ON -- an absent
// key reads as on, matching index.html's pre-paint bootstrap. This module only
// owns the stored value; the DOM mapping (the data-theme attribute + address-bar
// colour) lives in window.__setTheme, defined inline in index.html so it can run
// before first paint. Callers that flip this should also call window.__setTheme.
export function lightModeOn() {
  return stored(LIGHT_MODE_KEY) !== "0";
}

export function setLightMode(on) {
  store(LIGHT_MODE_KEY, on);
}
