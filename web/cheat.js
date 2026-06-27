"use strict";

import { logStorageError } from "./util.js";

// Cheat-mode state, shared by the play view (which toggles it and reacts in its
// UI) and the stats view (which only reads it to decide whether to show the
// debug seed). Kept here so there is one source of truth for the key and the
// default, with no DOM coupling.

export const CHEAT_KEY = "sudoku.cheat";

// A private/dev host where cheat defaults on without an explicit opt-in.
function privateHost() {
  const h = location.hostname;
  return (
    h === "localhost" ||
    h === "127.0.0.1" ||
    /^10\./.test(h) ||
    /^192\.168\./.test(h) ||
    /^172\.(1[6-9]|2\d|3[01])\./.test(h)
  );
}

function cheatStored() {
  try {
    return localStorage.getItem(CHEAT_KEY);
  } catch (err) {
    logStorageError("read", CHEAT_KEY, err);
    return null;
  }
}

// Tri-state: "1" (forced on), "0" (forced off), or unset (default to the host).
// The explicit setting wins, so you can turn cheat OFF even on a private host.
export function cheatOn() {
  const s = cheatStored();
  if (s === "1") return true;
  if (s === "0") return false;
  return privateHost();
}

// The single place that writes the cheat flag, so the play view's toggle and the
// daily page's "turn off cheat" prompt persist it the same way. Callers that need
// to react (play.js: tracking, the Hint-button colour, an open panel) layer that on
// top of this -- this only sets the durable explicit state.
export function setCheatMode(on) {
  try {
    localStorage.setItem(CHEAT_KEY, on ? "1" : "0");
  } catch (err) {
    logStorageError("write", CHEAT_KEY, err);
  }
}
