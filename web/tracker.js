"use strict";

// Move tracking for the play view. Records a timeline of the player's actions --
// placements, notes, erases, undo/redo, hint interactions, cheat actions,
// setting changes, pauses -- so the difficulty grader has richer data than the
// solved time alone. This is FRONTEND-ONLY: the log lives in localStorage under
// its own per-game key (see store.loadTrack/saveTrack) and is never sent
// anywhere. It records no device info or identifier beyond the game's own random
// id; it is data capture for grading, not user tracking.
//
// Each event is { t, type, ...data } where `t` is the solve-elapsed time in ms
// (pauses excluded -- the same clock that produces the solved time), the axis
// the grader reasons over. Wall-clock time is recorded once, on the session
// event, so a timeline can be placed in real time without stamping every event.
//
// Granularity matches the undo history: one event per committed change. A
// multi-cell paint is a single event carrying every affected cell, and an
// optimistic edit that a follow-up double-tap rolls back (the same rollback that
// pops the undo stack) drops its event via dropLast, so the log never shows a
// move the player didn't keep.

import * as store from "./store.js";

// The current game's log, held in memory and flushed to localStorage on a short
// debounce plus at lifecycle points (pause, solve, game swap). The in-memory
// array is the source of truth during a session; the store is the durable copy.
let events = [];
let gameId = null;
let dirty = false;
let flushTimer = null;

// Injected once by the play view (setClock): returns the solve-elapsed ms used
// to stamp each event. A callback so the call sites stay a bare track(type,data)
// and don't each have to thread the timer in.
let clock = () => 0;

// Long enough to coalesce a burst of taps into one write, short enough that a
// crash loses only the last moment of play.
const FLUSH_DELAY_MS = 1500;

export function setClock(fn) {
  clock = fn;
}

// Start (or resume) tracking for a game. Flushes any prior game's log first,
// then loads this game's existing log so a puzzle played across several sittings
// accumulates one continuous timeline. `meta` (start time, cheat flag, settings
// snapshot, ...) rides on the session event that opens this sitting.
export function beginSession(id, meta) {
  flush(); // persist the outgoing game under its own id before switching
  gameId = id;
  events = store.loadTrack(id);
  track("session", meta);
}

// Append one event. A no-op until a session has begun (so stray actions before a
// game is loaded are ignored rather than mis-attributed).
export function track(type, data) {
  if (gameId === null) return;
  const ev = { t: Math.round(clock()), type };
  if (data) Object.assign(ev, data);
  events.push(ev);
  dirty = true;
  scheduleFlush();
}

// Drop the most recently recorded event. Called in lockstep with the undo-stack
// rollback an optimistic double-tap performs (see onDigitTap / onLockedPointer
// Down in play.js): the first tap's edit was both pushed to history and recorded
// here, so when the double-tap pops history it pops the matching event too.
export function dropLast() {
  if (gameId === null || events.length === 0) return;
  events.pop();
  dirty = true;
  scheduleFlush();
}

function scheduleFlush() {
  if (flushTimer !== null) return;
  flushTimer = setTimeout(() => {
    flushTimer = null;
    flush();
  }, FLUSH_DELAY_MS);
}

// Persist the log now (a no-op when nothing changed since the last write). Runs
// on the debounce and is forced at pause / solve / game swap so a navigation or
// tab-hide never loses the tail of the timeline.
export function flush() {
  if (flushTimer !== null) {
    clearTimeout(flushTimer);
    flushTimer = null;
  }
  if (!dirty || gameId === null) return;
  store.saveTrack(gameId, events);
  dirty = false;
}
