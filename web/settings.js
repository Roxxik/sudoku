"use strict";

// App-wide settings, persisted to localStorage. Shared by the play view (which
// reads them while you place) and the settings screen (home.js, which reads and
// writes them). Kept DOM-free so there is one source of truth per key and its
// default -- mirrors cheat.js.

const ELIMINATE_KEY = "sudoku.settings.eliminateCandidates";
const SHOW_TIMER_KEY = "sudoku.settings.showTimer";

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
