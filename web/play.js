"use strict";

// The play view: the Sudoku board, input pad, hint pop-up, and -- new versus the
// old single-screen app -- a background solve timer, win detection against the
// stored solution, and checkpointing of the player's work into the game store.
//
// The gesture/undo machinery (optimistic single taps that a follow-up double-tap
// rolls back) is carried over verbatim from the original app; only the data
// source changed: instead of one hardcoded puzzle it loads/saves game records.

import { bindings } from "./wasm.js";
import * as store from "./store.js";
import { formatDuration, techniqueName } from "./util.js";
import { copyText } from "./ui.js";
import { cheatOn, CHEAT_KEY } from "./cheat.js";
import { eliminateCandidatesOn, showTimerOn } from "./settings.js";

const N = 81;

// ---- State ----
// `given[i]`   : 1..9 for a clue, 0 otherwise (immutable for the loaded puzzle).
// `value[i]`   : 1..9 for a user-placed digit, 0 if empty.
// Two independent kinds of pencil mark per cell, each a Set of digits:
//   `centerMarks[i]` : the usual notes, shown as one shrinking row in the middle.
//   `cornerMarks[i]` : Snyder notes, dropped into the cell corners by rank.
// `solution[i]`: 1..9, the unique solution (used only for win detection).
const given = new Array(N).fill(0);
const value = new Array(N).fill(0);
const centerMarks = Array.from({ length: N }, () => new Set());
const cornerMarks = Array.from({ length: N }, () => new Set());
const solution = new Array(N).fill(0);

// Cell selection. Multi-cell selection is active whenever no digit is pen-locked
// (activeDigit === 0), so a place / note / erase can act on every selected cell at
// once. `selection` holds the chosen cell indices; `cursor` is the most-recently
// touched cell -- it drives arrow-key movement and, when exactly one cell is
// selected, the peer / same-digit highlight. A plain single tap or click always
// collapses the selection back to that one cell (see the gesture handlers below).
let selection = new Set(); // selected cell indices (0..80)
let cursor = null; // anchor / last-touched cell, or null
// Input mode is two independent pieces of state:
//   `placing` : true -> taps place values; false -> taps pencil a note.
//   `markKind`: which note kind (Center or Corner) marking uses -- remembered
//               even while placing, so the Notes button can show which kind is
//               armed and a place-mode grid double-tap pencils that same kind.
// The Notes button's single tap flips `placing`; its double tap switches
// `markKind`.
const MODE_CENTER = 1, MODE_CORNER = 2;
let placing = true;
let markKind = MODE_CENTER;
let activeDigit = 0; // 1..9 when a digit is "pen-locked" (highlight mode), 0 otherwise

const cells = []; // DOM nodes, index 0..80
const digitBtns = []; // DOM nodes, index 0..8 -> digit 1..9

// The game record currently being played, and timer bookkeeping. The timer runs
// in the background and is shown only on the solved screen.
let game = null;
let timerBase = 0; // accumulated ms from prior sessions / before the last resume
let runStart = null; // performance.now() while the clock is running, else null
let finished = false; // true once solved -- freezes the timer

// Navigation callbacks supplied by the app shell.
let onHome = () => {};
let onNewPuzzle = () => {};
let onSettings = () => {};
let onHelp = () => {};

let boardEl, notesBtn, undoBtn, redoBtn;

// ---- Undo / redo history ----
// Each entry is a full snapshot of the editable state (values + pencil marks).
// `history` holds the states to undo *to* (one is pushed, before a change, only
// when a mutation actually changes the board, so no-op taps don't waste a step);
// `redoStack` holds the states an undo stepped away from, so redo can return to
// them. A fresh commit clears `redoStack` -- a new move invalidates the redo
// branch. The optimistic double-tap rollbacks reuse `history` directly (see
// onCellTap / onDigitTap), so a place->convert double-tap collapses to one step.
//
// Both stacks ride along in the game record (see persist / loadGame), so the
// whole undo/redo timeline survives closing and reopening a puzzle.
const history = [];
const redoStack = [];

function snapshot() {
  return {
    v: value.slice(),
    c: centerMarks.map((s) => new Set(s)),
    n: cornerMarks.map((s) => new Set(s)),
  };
}

// Snapshots persist in the store, so they have to survive JSON: the per-cell
// pencil-mark Sets (center + corner) serialize as plain arrays and rehydrate back
// into Sets. The value array is JSON-friendly already (`v` is a fresh slice from
// `snapshot`). `t` (the pre-restart elapsed time) only rides restart entries, so
// it's preserved iff present.
function snapshotToJSON(s) {
  const j = {
    v: s.v,
    c: s.c.map((set) => [...set]),
    n: s.n.map((set) => [...set]),
  };
  if (s.t !== undefined) j.t = s.t;
  return j;
}

function snapshotFromJSON(s) {
  // Legacy persisted snapshots only had `m` (the old single mark grid) -> center,
  // mirroring the same migration loadGame applies to a record's top-level marks.
  const c = s.c || s.m || [];
  const out = {
    v: (s.v || []).slice(),
    c: c.map((a) => new Set(a)),
    n: (s.n || []).map((a) => new Set(a)),
  };
  if (s.t !== undefined) out.t = s.t;
  return out;
}

function applySnapshot(s) {
  for (let i = 0; i < N; i++) {
    value[i] = s.v[i];
    centerMarks[i] = new Set(s.c[i]);
    cornerMarks[i] = new Set(s.n[i]);
  }
  // A restart entry carries the pre-restart elapsed time (`t`) so undoing the
  // restart also rewinds the clock; ordinary entries omit it and leave the timer
  // running untouched (undoing a move never rewinds time).
  if (s.t !== undefined) {
    timerBase = s.t;
    runStart = finished ? null : performance.now();
  }
}

function setsEqual(a, b) {
  if (a.size !== b.size) return false;
  for (const d of a) if (!b.has(d)) return false;
  return true;
}

function boardMatches(s) {
  for (let i = 0; i < N; i++) {
    if (value[i] !== s.v[i]) return false;
    if (!setsEqual(centerMarks[i], s.c[i])) return false;
    if (!setsEqual(cornerMarks[i], s.n[i])) return false;
  }
  return true;
}

// Run a board mutation, recording it for undo iff it changed anything.
// Returns whether an entry was pushed.
function commit(mutate) {
  const before = snapshot();
  mutate();
  return commitSnapshot(before);
}

// Finalize an already-applied change as one undo step: push `before` iff the
// board differs from it now, then clear redo, persist and check for a win.
// `commit` takes `before` right before its mutation; gestures that mutate
// optimistically as they go (the locked-digit drag paint) take it once up front
// and call this at the end, so the whole gesture collapses to a single entry.
function commitSnapshot(before) {
  if (boardMatches(before)) return false;
  history.push(before);
  redoStack.length = 0; // a fresh move invalidates the redo branch
  updateHistoryButtons();
  persist();
  checkWin();
  return true;
}

function undo() {
  if (history.length === 0) return;
  redoStack.push(snapshot()); // remember where we are so redo can return here
  applySnapshot(history.pop());
  updateHistoryButtons();
  persist();
  refreshTimer(); // a restart-undo rewinds the clock -- show it at once
  render();
}

function redo() {
  if (redoStack.length === 0) return;
  history.push(snapshot()); // the state we leave becomes undoable again
  applySnapshot(redoStack.pop());
  updateHistoryButtons();
  persist();
  refreshTimer();
  render();
}

function updateHistoryButtons() {
  if (undoBtn) undoBtn.disabled = history.length === 0;
  if (redoBtn) redoBtn.disabled = redoStack.length === 0;
}

// ---- Persistence / timer ----
function elapsedMs() {
  return timerBase + (runStart !== null ? performance.now() - runStart : 0);
}

// Save the player's work and the running elapsed time back to the store.
function persist() {
  if (!game) return;
  store.updateGame(game.id, {
    value: value.slice(),
    centerMarks: centerMarks.map((s) => [...s]),
    cornerMarks: cornerMarks.map((s) => [...s]),
    elapsedMs: elapsedMs(),
    lastPlayedAt: Date.now(),
    history: history.map(snapshotToJSON),
    redo: redoStack.map(snapshotToJSON),
  });
}

// Pause the clock (folding the running span into the accumulated base) and
// checkpoint. Called when leaving the view or hiding the tab.
export function pause() {
  if (runStart !== null) {
    timerBase = elapsedMs();
    runStart = null;
  }
  stopTimer();
  refreshTimer(); // freeze the readout at the paused time
  persist();
}

// Resume the clock, unless the puzzle is already solved.
export function resume() {
  if (!finished && game && runStart === null) {
    runStart = performance.now();
  }
  startTimer();
}

// ---- In-play timer readout ----
// The solve clock (timerBase/runStart) always runs for the solved time + stats;
// these only govern the optional readout above the board (the "Show timer"
// setting) and keep its text fresh once a second while the clock runs.
let timerTick = null;

function refreshTimer() {
  const el = document.getElementById("playTimer");
  if (!el) return;
  const show = showTimerOn();
  el.hidden = !show;
  if (show) el.textContent = formatDuration(elapsedMs());
}

function startTimer() {
  refreshTimer();
  if (timerTick === null && showTimerOn() && runStart !== null) {
    timerTick = setInterval(refreshTimer, 1000);
  }
}

function stopTimer() {
  if (timerTick !== null) {
    clearInterval(timerTick);
    timerTick = null;
  }
}

// ---- Setup ----
function parseLine(line, out) {
  for (let i = 0; i < N; i++) {
    const ch = line[i];
    out[i] = ch >= "1" && ch <= "9" ? Number(ch) : 0;
  }
}

function buildBoard() {
  for (let i = 0; i < N; i++) {
    const r = Math.floor(i / 9);
    const c = i % 9;
    const cell = document.createElement("div");
    cell.className = "cell";
    cell.dataset.idx = i;
    cell.setAttribute("role", "gridcell");
    if (c === 8) cell.classList.add("col8");
    if (r === 8) cell.classList.add("row8");
    if (c === 2 || c === 5) cell.classList.add("boxr");
    if (r === 2 || r === 5) cell.classList.add("boxb");

    // Two note overlays under the value: Snyder corners (a 3x3 slot grid filled
    // by rank in render) and the centred "usual" notes (one shrinking row).
    const cornerEl = document.createElement("div");
    cornerEl.className = "corner-notes";
    for (let s = 0; s < 9; s++) cornerEl.appendChild(document.createElement("span"));
    const centerEl = document.createElement("div");
    centerEl.className = "center-notes";
    const valueEl = document.createElement("span");
    valueEl.className = "value";
    cell.appendChild(cornerEl);
    cell.appendChild(centerEl);
    cell.appendChild(valueEl);

    cell.addEventListener("pointerdown", (e) => onCellPointerDown(i, e));

    boardEl.appendChild(cell);
    cells.push(cell);
  }

  // Drag-paint / release are handled at the board level so the gesture keeps
  // running while the pointer crosses cell borders (and, with pointer capture,
  // even when it leaves the board). The hovered cell is found by hit-testing,
  // which works for both mouse and touch (a touch pointer is otherwise pinned to
  // the cell it started on).
  boardEl.addEventListener("pointermove", onBoardPointerMove);
  boardEl.addEventListener("pointerup", onBoardPointerUp);
  boardEl.addEventListener("pointercancel", onBoardPointerUp);

  // A press on the page background -- anywhere on the play view that isn't the
  // board or a control (the top bar / pad / hint panel) -- drops the selection,
  // the way clicking off a widget dismisses it. Cell and button presses match a
  // region and bubble through untouched.
  document.addEventListener("pointerdown", (e) => {
    if (!playVisible()) return;
    if (e.target.closest("#board, #playPad, #hintPanel, .topbar")) return;
    if (selection.size === 0 && cursor === null) return;
    clearSelection();
    render();
  });
}

// ---- Peer relationship (row / col / box) for selection highlighting. ----
function isPeer(a, b) {
  if (a === b) return false;
  const ra = Math.floor(a / 9), ca = a % 9;
  const rb = Math.floor(b / 9), cb = b % 9;
  if (ra === rb || ca === cb) return true;
  const sameBox =
    Math.floor(ra / 3) === Math.floor(rb / 3) &&
    Math.floor(ca / 3) === Math.floor(cb / 3);
  return sameBox;
}

function digitAt(i) {
  return given[i] || value[i];
}

// ---- Rendering ----
function render() {
  // Peer / same-digit highlighting is a single-cell affordance: it only makes
  // sense when exactly one cell is selected (a multi-cell selection would smear
  // peers all over the board). `single` is that lone cell, else null.
  const single = selection.size === 1 ? selection.values().next().value : null;
  // The highlighted digit is the pen-locked digit if any, else whatever sits in
  // the single selected cell (nothing when several cells are selected).
  const highlightDigit =
    activeDigit !== 0 ? activeDigit : single !== null ? digitAt(single) : 0;

  for (let i = 0; i < N; i++) {
    const cell = cells[i];
    const valueEl = cell.querySelector(".value");
    const d = digitAt(i);

    cell.classList.toggle("given", given[i] !== 0);

    if (d !== 0) {
      valueEl.textContent = d;
      cell.classList.add("filled");
    } else {
      valueEl.textContent = "";
      cell.classList.remove("filled");
    }

    // Pencil marks only show on an empty cell.
    renderNotes(cell, i, d === 0);

    // Highlight classes. Every selected cell gets `selected`; peer / same only
    // light up for a lone selection.
    cell.classList.toggle("selected", selection.has(i));
    cell.classList.toggle("peer", single !== null && isPeer(single, i));
    cell.classList.toggle(
      "same",
      highlightDigit !== 0 && !selection.has(i) && d === highlightDigit
    );
  }
}

// The 3x3 corner slots are stored row-major (0=TL,1=TM,2=TR,3=LM,4=C,5=RM,6=BL,
// 7=BM,8=BR). Corners fill in this order as the count grows -- TL, TR, BL, BR,
// then the edge midpoints, then the centre -- so a few Snyder notes hug the
// corners. The slots that end up occupied are then read out in row-major (visual)
// order, so the digits stay in ascending reading order instead of jumbling (e.g.
// "1 5 2") as more notes appear.
const CORNER_FILL = [0, 2, 6, 8, 1, 7, 3, 5, 4];

// Paint a cell's two note overlays. Center notes are the sorted candidates packed
// into one row (`--n` drives the shrink-to-fit font). Corner notes drop into the
// fixed slots by ascending rank. Both clear out once the cell holds a value.
function renderNotes(cell, i, empty) {
  const centerEl = cell.querySelector(".center-notes");
  const cornerSpans = cell.querySelectorAll(".corner-notes span");
  for (const s of cornerSpans) s.textContent = "";
  if (!empty) {
    centerEl.textContent = "";
    return;
  }
  const center = [...centerMarks[i]].sort((a, b) => a - b);
  centerEl.textContent = center.join("");
  centerEl.style.setProperty("--n", center.length || 1);
  const corner = [...cornerMarks[i]].sort((a, b) => a - b);
  // Use the first N fill slots, but emit digits into them in row-major order.
  const slots = CORNER_FILL.slice(0, corner.length).sort((a, b) => a - b);
  corner.forEach((cd, rank) => {
    cornerSpans[slots[rank]].textContent = cd;
  });
}

function updateDigitButtons() {
  for (let k = 0; k < 9; k++) {
    digitBtns[k].classList.toggle("active", activeDigit === k + 1);
  }
}

// ---- Win detection ----
// Solved iff every cell is filled and matches the stored solution. The solver
// never has to run; we just compare against the solution the generator gave us.
function checkWin() {
  if (finished) return;
  for (let i = 0; i < N; i++) {
    if (digitAt(i) === 0 || digitAt(i) !== solution[i]) return;
  }
  onSolved();
}

function onSolved() {
  finished = true;
  closeHint();
  pause(); // freeze and persist the elapsed time
  const finalMs = elapsedMs();
  game = store.updateGame(game.id, {
    status: "solved",
    solvedAt: Date.now(),
    elapsedMs: finalMs,
  });
  showSolved(finalMs);
}

// ---- Actions ----
// Collapse the selection to a single cell (the plain tap / arrow behaviour).
function select(i) {
  selection = new Set([i]);
  cursor = i;
  render();
}

// Add cells to the current selection (drag-paint, shift / double-tap add,
// shift+arrows). The last one becomes the cursor.
function addToSelection(...idxs) {
  for (const i of idxs) selection.add(i);
  if (idxs.length) cursor = idxs[idxs.length - 1];
  render();
}

// Drop the whole selection (pen-lock takes over the highlight, hint opens, etc.).
function clearSelection() {
  selection = new Set();
  cursor = null;
}

// ---- Multi-cell selection gestures (only while no digit is pen-locked) ----
// The selection grows by dragging across cells and by additive taps. The
// additive modifier is Shift on a keyboard; on touch, where there is no Shift, a
// quick double-tap stands in for it. To keep "every single tap just selects one
// cell" true while still letting a double-tap add, the first tap optimistically
// collapses the selection to that one cell but remembers the prior selection; a
// second tap within DOUBLE_MS restores it and adds the cell, so the double-tap
// nets to an additive add. (Same optimistic-rollback idiom as the pad's pen-lock
// double-tap.) Gestures: tap = select one; drag = paint a fresh selection;
// shift-tap / double-tap = add a cell; shift-drag / double-tap-drag = additive
// paint; shift+arrows = extend by one (see moveSelection).
let gesturePointer = null; // pointerId of the active board gesture, or null
let lastPaintCell = -1; // last cell the drag painted, to skip repeats
let lastSelCell = -1; // last cell tapped, for double-tap-to-add detection
let lastSelTime = 0;
let preTapSelection = null; // selection before an optimistic single-select tap

function onCellPointerDown(i, e) {
  e.preventDefault();
  if (finished) return;
  // With a digit pen-locked a board press places it (and a double-tap pencils
  // the opposite); there is no multi-select in that mode. Dragging after a press
  // that pencils a note paints that note across the cells it crosses.
  if (activeDigit !== 0) {
    onLockedPointerDown(i, e);
    return;
  }

  const now = performance.now();
  const isDouble = i === lastSelCell && now - lastSelTime < DOUBLE_MS;

  gesturePointer = e.pointerId;
  lastPaintCell = i;
  try {
    boardEl.setPointerCapture(e.pointerId);
  } catch {
    /* ignore */
  }

  if (e.shiftKey) {
    // Held modifier: add to the existing selection straight away.
    addToSelection(i);
    preTapSelection = null;
  } else if (isDouble) {
    // Touch additive: undo the first tap's optimistic collapse, then add.
    if (preTapSelection) selection = preTapSelection;
    addToSelection(i);
    preTapSelection = null;
    lastSelCell = -1; // a third quick tap shouldn't read as another double
  } else {
    // Plain tap: optimistically select just this cell, keeping the prior
    // selection in case a double-tap follows to make this additive.
    preTapSelection = selection;
    select(i);
  }
  if (!isDouble) {
    lastSelCell = i;
    lastSelTime = now;
  }
}

// Drag-paint as the pointer enters each new cell. A drag is never a tap, so it
// disarms double-tap detection. With a digit pen-locked a mark gesture paints
// that note across the crossed cells (value placements don't extend); otherwise
// the gesture grows the selection.
function onBoardPointerMove(e) {
  if (gesturePointer === null || e.pointerId !== gesturePointer) return;
  const i = cellIndexAt(e.clientX, e.clientY);
  if (i < 0 || i === lastPaintCell) return;
  lastPaintCell = i;
  if (gestureLocked) {
    lastCell = -1; // a drag is never a tap -> no double-tap next
    if (lockMark) paintLockCell(i);
    return;
  }
  lastSelCell = -1;
  preTapSelection = null;
  if (selection.has(i)) cursor = i;
  else addToSelection(i);
}

function onBoardPointerUp(e) {
  if (gesturePointer === null || e.pointerId !== gesturePointer) return;
  try {
    boardEl.releasePointerCapture(e.pointerId);
  } catch {
    /* ignore */
  }
  gesturePointer = null;
  if (gestureLocked) {
    // Commit the whole gesture -- pressed cell plus any painted cells -- as one
    // undo step (cellSinglePushed lets a following double-tap roll it back).
    cellSinglePushed = commitSnapshot(lockBefore);
    gestureLocked = false;
    lockMark = null;
    lockBefore = null;
    lockPainted = null;
  }
}

// The cell index under a viewport point, or -1 if the point isn't on a board
// cell. Used instead of the event target so a touch-drag (whose pointer is pinned
// to the cell it started on) still paints the cell actually under the finger.
function cellIndexAt(x, y) {
  const el = document.elementFromPoint(x, y);
  const cell = el && el.closest(".cell");
  if (!cell || !boardEl.contains(cell)) return -1;
  return Number(cell.dataset.idx);
}

// Cell-tap state for pen-lock double-tap detection (place <-> note).
let lastCell = -1;
let lastCellTime = 0;
let cellSinglePushed = false; // the last locked gesture recorded an undo entry
// State of the in-flight locked gesture (a board press while a digit is locked).
let gestureLocked = false; // true between a locked pointerdown and its release
let lockBefore = null; // board snapshot at the gesture's start, for a one-step undo
let lockMark = null; // {corner, add, d} brush for a mark-painting drag, else null
let lockPainted = null; // cells already painted this gesture (paint each once)

// Extend a mark-painting drag onto one cell, at most once per gesture. The
// direction is fixed by the anchor (`add`), so a drag only ever adds the mark or
// only ever removes it -- never toggles per cell -- and crossing a cell that
// already matches is a no-op. Values are left untouched; only the active note
// kind on empty, non-clue cells changes.
function paintLockCell(i) {
  if (lockPainted.has(i)) return;
  lockPainted.add(i);
  const { corner, add, d } = lockMark;
  const set = corner ? cornerMarks[i] : centerMarks[i];
  if (add) {
    if (given[i] === 0 && value[i] === 0) set.add(d);
  } else if (given[i] === 0) {
    set.delete(d);
  }
  render();
}

// Board press with a digit pen-locked: the pressed cell takes the normal action
// (place the value, or pencil a note in mark mode), and a double-tap on the same
// cell takes the opposite action -- so place mode's double-tap pencils a note and
// mark mode's double-tap places the value. A drag after the press extends only
// the *note* variants (onBoardPointerMove): placing a value is a single-cell
// affordance, but pencilling marks paints across every cell the pointer crosses.
// The whole gesture commits as one undo step at release (onBoardPointerUp). Only
// reached while locked; the unlocked path is the selection gesture above.
function onLockedPointerDown(i, e) {
  const now = performance.now();
  const isDouble = i === lastCell && now - lastCellTime < DOUBLE_MS;
  const d = activeDigit;

  // A double-tap nets to "the opposite action": roll the prior single tap's
  // commit back first so the whole double stays one undo step.
  if (isDouble && cellSinglePushed) {
    applySnapshot(history.pop());
    cellSinglePushed = false;
    updateHistoryButtons();
  }

  gesturePointer = e.pointerId;
  gestureLocked = true;
  lastPaintCell = i;
  try {
    boardEl.setPointerCapture(e.pointerId);
  } catch {
    /* ignore */
  }

  // Whether this gesture pencils a mark (the only kind a drag extends). The
  // normal action pencils in mark mode; the opposite action pencils in place
  // mode -- i.e. exactly when `placing === isDouble`. For a mark gesture the drag
  // brush adds the note iff the anchor doesn't already carry it (else removes),
  // fixing one direction for the whole drag.
  lockBefore = snapshot();
  if (placing === isDouble) {
    const corner = markKind === MODE_CORNER;
    const set = corner ? cornerMarks[i] : centerMarks[i];
    lockMark = { corner, add: !set.has(d), d };
  } else {
    lockMark = null; // a value placement: only the pressed cell takes it
  }
  lockPainted = new Set([i]);

  // The pressed cell always gets the full single-cell action (value <-> note
  // conversion, peer elimination); the drag, if any, only paints marks.
  (isDouble ? applyOppositeToCell : applyDigitToCell)(i, d);
  render();

  // A double consumes the pair; a single arms one for a quick repeat tap.
  lastCell = isDouble ? -1 : i;
  lastCellTime = now;
}

// Arrow-key movement. `extend` (Shift held) grows the selection by adding the
// cell stepped into; otherwise the selection collapses to that one cell.
function moveSelection(dr, dc, extend) {
  if (cursor === null) {
    select(0);
    return;
  }
  let r = Math.floor(cursor / 9) + dr;
  let c = (cursor % 9) + dc;
  r = (r + 9) % 9;
  c = (c + 9) % 9;
  const ni = r * 9 + c;
  // Extending is a multi-select action, so only while no digit is pen-locked.
  if (extend && activeDigit === 0) addToSelection(ni);
  else select(ni);
}

// Place / toggle a digit (or pencil mark) into a specific cell, per the global
// Notes mode. Clues are locked. Shared by the digit pad (cell-first) and the
// pen-lock single tap (digit-first).
function toggleMark(set, d) {
  if (set.has(d)) set.delete(d);
  else set.add(d);
}

// Whether placing a digit should strike it from peers' Center and Corner marks.
// Driven by the "Eliminate candidates" setting; cheat forces it on because its
// marks-aware hint engine (see solverBoard) needs the displayed notes kept in
// lock-step with the grid as cells fill in. ("Apply easiest" eliminates
// regardless of either -- applyStep calls clearPeerMarks unconditionally.)
function eliminateOn() {
  return cheatOn() || eliminateCandidatesOn();
}

// A placed digit can't be a candidate in any peer, so strike it from their
// pencil notes (both center and corner). Caller invokes inside a `commit`, so it
// is one undo step with the placement.
function clearPeerMarks(i, d) {
  for (let j = 0; j < N; j++) {
    if (j === i || !isPeer(i, j)) continue;
    centerMarks[j].delete(d);
    cornerMarks[j].delete(d);
  }
}

function applyDigitToCell(i, d) {
  if (given[i] !== 0) return;
  if (placing) {
    // Toggle off if re-entering the same digit; placing clears both note kinds.
    value[i] = value[i] === d ? 0 : d;
    centerMarks[i].clear();
    cornerMarks[i].clear();
    // value[i] === d means we placed (not toggled off): clean the peers.
    if (eliminateOn() && value[i] === d) clearPeerMarks(i, d);
  } else {
    // Pencil marks are meaningless once a value is placed.
    if (value[i] !== 0) return;
    toggleMark(markKind === MODE_CORNER ? cornerMarks[i] : centerMarks[i], d);
  }
}

// The pen-lock double-tap action: the opposite of a normal tap. In place mode a
// normal tap places the value, so this pencils a note instead -- in the armed
// `markKind` (Center or Corner), clearing the value first so the note shows. In
// mark mode a normal tap pencils the note, so this places the value instead.
function applyOppositeToCell(i, d) {
  if (given[i] !== 0) return;
  if (placing) {
    value[i] = 0;
    toggleMark(markKind === MODE_CORNER ? cornerMarks[i] : centerMarks[i], d);
  } else {
    value[i] = value[i] === d ? 0 : d;
    centerMarks[i].clear();
    cornerMarks[i].clear();
    if (eliminateOn() && value[i] === d) clearPeerMarks(i, d);
  }
}

// Apply a digit across the whole selection in one undoable step, with uniform
// toggle semantics so a bulk edit is predictable: a value is removed only if
// *every* settable cell already shows it (else it's placed in all); a note is
// removed only if every markable cell already carries it (else added to all).
// For a single selected cell this reduces to the same toggle applyDigitToCell did.
function applyDigitToSelection(d) {
  if (placing) {
    // Values go into non-clue cells.
    const targets = [...selection].filter((i) => given[i] === 0);
    if (!targets.length) return;
    const clearing = targets.every((i) => value[i] === d);
    for (const i of targets) {
      if (clearing) {
        value[i] = 0;
      } else {
        value[i] = d;
        centerMarks[i].clear();
        cornerMarks[i].clear();
      }
    }
    // A placed digit can't be a candidate in any peer of any filled cell.
    if (!clearing && eliminateOn()) for (const i of targets) clearPeerMarks(i, d);
  } else {
    // Notes only go into empty, non-clue cells (a value would hide them).
    const target = markKind === MODE_CORNER ? cornerMarks : centerMarks;
    const targets = [...selection].filter((i) => given[i] === 0 && value[i] === 0);
    if (!targets.length) return;
    const clearing = targets.every((i) => target[i].has(d));
    for (const i of targets) {
      if (clearing) target[i].delete(d);
      else target[i].add(d);
    }
  }
}

function inputDigit(d) {
  if (finished || selection.size === 0) return false;
  const pushed = commit(() => applyDigitToSelection(d));
  render();
  return pushed;
}

// Set the pen-locked digit (0 clears it) and refresh the highlight.
function setLock(d) {
  activeDigit = d;
  if (activeDigit !== 0) clearSelection(); // pen mode owns the highlight
  updateDigitButtons();
  render();
}

// Erase the value and both note kinds from every selected (non-clue) cell, as
// one undo step.
function erase() {
  if (finished || selection.size === 0) return;
  commit(() => {
    for (const i of selection) {
      if (given[i] !== 0) continue;
      value[i] = 0;
      centerMarks[i].clear();
      cornerMarks[i].clear();
    }
  });
  render();
}

// Flip between placing values and pencilling notes (the Notes button's single
// tap and the "n" key).
function togglePlacing() {
  placing = !placing;
  updateNotesButton();
}

// Switch the armed note kind, Center <-> Corner (the Notes button's double tap
// and the "N" key). Leaves place/mark unchanged.
function switchMarkKind() {
  markKind = markKind === MODE_CENTER ? MODE_CORNER : MODE_CENTER;
  updateNotesButton();
}

// Notes button gesture. A single tap flips place/mark; a double tap switches the
// mark kind. Mirrors the pad's optimistic double-tap: the first tap flips placing
// immediately, and a quick second tap reverts that flip and switches the kind
// instead -- so a double-tap nets to "kind switched, place/mark unchanged".
let lastNotesTap = 0;
function onNotesTap() {
  const now = performance.now();
  const isDouble = now - lastNotesTap < DOUBLE_MS;
  lastNotesTap = now;
  if (isDouble) {
    lastNotesTap = 0; // a third quick tap shouldn't read as another double
    placing = !placing; // undo the single tap's flip
    switchMarkKind(); // renders the button
  } else {
    togglePlacing();
  }
}

// Reflect the current mode on the Notes button. In mark mode the label names the
// kind and the button fills with its colour; in place mode it reads "Notes" but
// still carries the armed kind (data-mark) so CSS can draw a coloured border
// hinting what a switch-to-mark -- or a place-mode grid double-tap -- will pencil.
function updateNotesButton() {
  if (!notesBtn) return;
  notesBtn.textContent = placing ? "Notes" : markKind === MODE_CORNER ? "Corner" : "Center";
  notesBtn.setAttribute("aria-pressed", String(!placing));
  notesBtn.dataset.placing = String(placing);
  notesBtn.dataset.mark = markKind === MODE_CORNER ? "corner" : "center";
}

// Clear all of the player's work and return to the puzzle's starting clues. The
// clock restarts too -- a restart is a fresh attempt. (The clues live in
// `given`, so we only wipe the editable state.) Restart is undoable: the whole
// pre-restart attempt -- board plus elapsed time -- rides onto the undo stack, so
// an accidental tap is fully recoverable with Undo.
function restart() {
  closeHint();
  // Push the pre-restart state as one undo entry, tagged with the elapsed time
  // (`t`) so undoing the restart rewinds the clock too. Unlike a normal commit
  // (board only), and unlike the old restart (which wiped the undo history).
  const before = snapshot();
  before.t = elapsedMs();
  history.push(before);
  redoStack.length = 0;

  for (let i = 0; i < N; i++) {
    value[i] = 0;
    centerMarks[i] = new Set();
    cornerMarks[i] = new Set();
  }
  clearSelection();
  activeDigit = 0;
  placing = true;
  markKind = MODE_CENTER;
  finished = false;
  timerBase = 0;
  runStart = performance.now();
  // Reset double-tap gesture state so a stray pending double can't pop the
  // freshly pushed undo entry.
  lastDigit = 0;
  lastCell = -1;
  lastNotesTap = 0;
  lastSelCell = -1;
  preTapSelection = null;
  firstTapWasLocked = false;
  digitSinglePushed = false;
  cellSinglePushed = false;

  updateNotesButton();
  updateDigitButtons();
  updateHistoryButtons();
  store.updateGame(game.id, {
    value: value.slice(),
    centerMarks: centerMarks.map(() => []),
    cornerMarks: cornerMarks.map(() => []),
    elapsedMs: 0,
    status: "active",
    solvedAt: null,
    history: history.map(snapshotToJSON),
    redo: [],
  });
  game = store.getGame(game.id);
  startTimer();
  render();
}

// ---- Hint panel ----
// The hint is an inline panel below the board (it replaces the input pad while
// open) so the board stays visible and we can highlight regions/cells on it as
// detail is revealed. Flow:
//   1. Errors first -- a logical contradiction (provable now) outranks and hides
//      a solved-state mismatch (an entry that differs from the solution).
//   2. Otherwise a staggered tree of available techniques: each row reveals
//      tier -> name -> unit -> cell -> deduction one step at a time, and the row
//      you drill into becomes the single "focused" row driving the highlight.
// A cheat mode (see below) adds Apply buttons.

// The solver returns raw cell indices / 0-based houses; the UI owns the labels.
function cellName(i) {
  return `R${Math.floor(i / 9) + 1}C${(i % 9) + 1}`;
}

function houseLabel(house) {
  const kind = { row: "row", col: "column", box: "box" }[house.kind] || house.kind;
  return `${kind} ${house.index + 1}`;
}

function deductionText(d) {
  return `${cellName(d.cell)} ${d.kind === "place" ? "=" : "≠"} ${d.digit}`;
}

// The 81 cell indices of a house {kind,index}.
function unitCells(house) {
  const out = [];
  if (house.kind === "row") for (let c = 0; c < 9; c++) out.push(house.index * 9 + c);
  else if (house.kind === "col") for (let r = 0; r < 9; r++) out.push(r * 9 + house.index);
  else {
    const r0 = Math.floor(house.index / 3) * 3, c0 = (house.index % 3) * 3;
    for (let r = 0; r < 3; r++) for (let c = 0; c < 3; c++) out.push((r0 + r) * 9 + (c0 + c));
  }
  return out;
}

// ---- Board highlight overlay (driven by the focused hint row) ----
function clearHighlights() {
  for (const cell of cells) cell.classList.remove("hl-unit", "hl-cell", "hl-place", "hl-elim");
}

function highlightStage(step, stage) {
  clearHighlights();
  if (stage === "tier" || stage === "name") return;
  // The unit is a faint always-on context. Focus cells go blue on the focus
  // step and STAY blue on the deduction step, where the affected cells add red
  // (or green for a placement): first blue, then both.
  if (step.house) for (const c of unitCells(step.house)) cells[c].classList.add("hl-unit");
  if (step.focusCells && (stage === "cell" || stage === "deduction")) {
    for (const c of step.focusCells) cells[c].classList.add("hl-cell");
  }
  if (stage === "deduction") {
    for (const d of step.deductions) {
      cells[d.cell].classList.add(d.kind === "place" ? "hl-place" : "hl-elim");
    }
  }
}

// ---- Error detection ----
// Logical: a digit repeated in any row/col/box (a contradiction provable without
// the solution). Returns the offending cell indices.
function logicalErrorCells() {
  const bad = new Set();
  for (let i = 0; i < N; i++) {
    const d = digitAt(i);
    if (d === 0) continue;
    for (let j = i + 1; j < N; j++) {
      if (digitAt(j) === d && isPeer(i, j)) {
        bad.add(i);
        bad.add(j);
      }
    }
  }
  return [...bad];
}

// Solved-state: player entries that differ from the unique solution.
function solvedErrorCells() {
  const bad = [];
  for (let i = 0; i < N; i++) {
    if (value[i] !== 0 && value[i] !== solution[i]) bad.push(i);
  }
  return bad;
}

// ---- Panel open/close ----
function hintTitle(text) {
  document.getElementById("hintTitle").textContent = text;
}

export function closeHint() {
  document.getElementById("hintPanel").hidden = true;
  document.getElementById("playPad").hidden = false;
  clearHighlights();
}

function toggleHintPanel() {
  const panel = document.getElementById("hintPanel");
  if (panel.hidden) openHint();
  else closeHint();
}

// Build a solver Board from the current grid. In cheat mode the player's center
// pencil notes ride along as a per-cell candidate mask (bit d-1 set for each
// noted digit), so the hint engine reasons over the *reduced* candidate set
// instead of re-deriving it from placements alone. That is what lets "Apply
// easiest" progress past singles: a technique elimination narrows the notes, and
// the next hint sees the narrower set (a cell with no center notes means
// "unspecified" -> it stays grid-derived). Corner (Snyder) notes mark where a
// digit can go, not a full candidate set, so they are not forwarded. Off cheat we
// pass no marks, so hints reflect the true position. Caller owns `.free()`.
function solverBoard(wasm) {
  const cellDigits = new Uint8Array(N);
  for (let i = 0; i < N; i++) cellDigits[i] = digitAt(i);
  if (!cheatOn()) return new wasm.Board(cellDigits);
  const cand = new Uint16Array(N); // 0 = unspecified (keep grid-derived)
  for (let i = 0; i < N; i++) {
    if (digitAt(i) !== 0 || centerMarks[i].size === 0) continue;
    let m = 0;
    for (const d of centerMarks[i]) m |= 1 << (d - 1);
    cand[i] = m;
  }
  return new wasm.Board(cellDigits, cand);
}

// Build the panel content for the current board and show it.
function openHint() {
  const body = document.getElementById("hintBody");
  const note = (msg) => {
    const p = document.createElement("p");
    p.className = "hint-note";
    p.textContent = msg;
    body.replaceChildren(p);
  };
  // Drop any cell selection so its highlight (selected/peer/same) doesn't mix
  // with the hint's own region/cell highlights.
  clearSelection();
  render();
  clearHighlights();

  const logical = logicalErrorCells();
  if (logical.length) {
    hintTitle("Mistake");
    body.replaceChildren(errorStage("logical", logical));
    showPanel();
    return;
  }
  const solved = solvedErrorCells();
  if (solved.length) {
    hintTitle("Mistake");
    body.replaceChildren(errorStage("solved", solved));
    showPanel();
    return;
  }

  const wasm = bindings();
  if (!wasm) {
    hintTitle("Hint");
    note("The solver is still loading. Try again in a moment.");
    showPanel();
    return;
  }
  let steps, isSolved;
  try {
    const board = solverBoard(wasm);
    try {
      steps = wasm.hint(board);
      isSolved = board.isSolved();
    } finally {
      board.free();
    }
  } catch {
    hintTitle("Hint");
    note("Couldn't read the board. Check for an obvious slip.");
    showPanel();
    return;
  }

  if (isSolved) {
    hintTitle("Solved");
    note("The puzzle is already solved.");
    showPanel();
    return;
  }
  // First layer: just confirm the board is fine, without revealing what's
  // possible. Revealing drills into the technique tree.
  const masks = specMasksFor(wasm);
  hintTitle("Hint");
  body.replaceChildren(statusStage(steps, masks));
  showPanel();
}

// A bold banner with an icon: a green check for "all good", a red alert for a
// mistake. Drawn as SVG (no emoji).
function statusBanner(ok, text) {
  const el = document.createElement("div");
  el.className = "hint-banner " + (ok ? "ok" : "bad");
  el.innerHTML = ok
    ? '<svg viewBox="0 0 24 24" class="banner-icon" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-width="2.6" stroke-linecap="round" stroke-linejoin="round" d="M4 12.5l5 5 11-11"/></svg>'
    : '<svg viewBox="0 0 24 24" class="banner-icon" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round" d="M12 3l9 16H3z"/><path fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" d="M12 9v4"/><circle cx="12" cy="16.4" r="1.2" fill="currentColor"/></svg>';
  const span = document.createElement("span");
  span.textContent = text;
  el.appendChild(span);
  return el;
}

// The reassurance layer: a prominent "no mistakes" banner plus large, stacked
// actions (reveal the techniques; cheat: apply the easiest move). Applying here
// re-renders this same layer (via openHint) so it can be spammed without leaving.
function statusStage(steps, masks) {
  const box = document.createElement("div");
  box.className = "hint-stage";
  box.appendChild(statusBanner(true, "No mistakes so far."));

  const actions = document.createElement("div");
  actions.className = "hint-actions";

  const reveal = document.createElement("button");
  reveal.className = "hint-bigbtn";
  reveal.textContent = "Show available techniques";
  reveal.addEventListener("click", renderTechStage);
  actions.appendChild(reveal);

  if (cheatOn() && steps.length) {
    const ez = document.createElement("button");
    ez.className = "hint-bigbtn cheat";
    ez.textContent = "Apply easiest";
    ez.addEventListener("click", () => applyStep(easiestStep(steps, masks), openHint));
    actions.appendChild(ez);
  }
  box.appendChild(actions);
  return box;
}

// The easiest move to auto-apply: the lowest-difficulty in-scope step, or the
// lowest-difficulty overall if nothing is in scope at this position.
function easiestStep(steps, masks) {
  const sorted = groupSteps(steps).sort(
    (a, b) => a.rep.technique.difficulty - b.rep.technique.difficulty
  );
  const inScope = sorted.filter((g) => isInScope(g, masks));
  return (inScope[0] || sorted[0]).rep;
}

// Render (or re-render) the technique tree for the current board into the panel.
// Recomputes steps + spec masks each time so it reflects the live board -- used
// both when revealing from the status layer and after a cheat Apply, so applying
// stays on the technique layer instead of bouncing back to "no mistakes".
function renderTechStage() {
  const body = document.getElementById("hintBody");
  const wasm = bindings();
  let steps = [];
  if (wasm) {
    try {
      const board = solverBoard(wasm);
      try {
        steps = wasm.hint(board);
      } finally {
        board.free();
      }
    } catch {
      steps = [];
    }
  }
  const masks = specMasksFor(wasm);
  hintTitle("Available moves");
  if (steps.length) {
    body.replaceChildren(techStage(groupSteps(steps), masks));
  } else {
    const n = document.createElement("p");
    n.className = "hint-note";
    n.textContent = "No technique we've implemented applies here yet.";
    body.replaceChildren(n);
  }
}

function showPanel() {
  document.getElementById("playPad").hidden = true;
  document.getElementById("hintPanel").hidden = false;
}

// ---- Error stage ----
function errorStage(kind, errCells) {
  const box = document.createElement("div");
  box.className = "hint-stage";
  box.appendChild(
    statusBanner(
      false,
      kind === "logical"
        ? "Contradiction — a digit repeats in a row, column, or box."
        : "One of your entries doesn't match the solution."
    )
  );

  const actions = document.createElement("div");
  actions.className = "hint-actions";
  const show = document.createElement("button");
  show.className = "hint-bigbtn";
  show.textContent = "Show me where";
  show.addEventListener("click", () => {
    clearHighlights();
    for (const c of errCells) cells[c].classList.add("hl-elim");
  });
  actions.appendChild(show);

  if (cheatOn()) {
    const fix = document.createElement("button");
    fix.className = "hint-bigbtn cheat";
    fix.textContent = "Erase mistakes";
    fix.addEventListener("click", () => {
      commit(() => {
        for (const c of errCells) {
          if (given[c] === 0 && value[c] !== solution[c]) {
            value[c] = 0;
            centerMarks[c].clear();
            cornerMarks[c].clear();
          }
        }
      });
      render();
      if (!finished) openHint();
      else closeHint();
    });
    actions.appendChild(fix);
  }
  box.appendChild(actions);
  return box;
}

// ---- Technique stage (staggered tree) ----
// Group available steps by technique kind so several hidden singles collapse to
// one row; the representative is the easiest instance (steps are easiest-first).
function groupSteps(steps) {
  const byId = new Map();
  const order = [];
  for (const s of steps) {
    if (!byId.has(s.technique.id)) {
      byId.set(s.technique.id, { id: s.technique.id, name: s.technique.name, rep: s, steps: [] });
      order.push(s.technique.id);
    }
    byId.get(s.technique.id).steps.push(s);
  }
  // `count` and `rep` for convenience (rep = first/easiest instance).
  return order.map((id) => {
    const g = byId.get(id);
    g.count = g.steps.length;
    return g;
  });
}

// The reveal sequence for one step. `includeName` adds the tier+name preamble
// (omitted for instance child rows, whose name is on the group header). The unit
// step is skipped when the technique has no single house (wings/fish), the cell
// step when it carries no focus cells.
function stepStages(step, lead) {
  const s = lead === "full" ? ["tier", "name"] : lead === "name" ? ["name"] : [];
  if (step.house) s.push("unit");
  // Focus cells (the pattern) are their own step, highlighted blue; the
  // deduction step then highlights the affected cells red. Two steps -- no
  // redundant middle.
  if (step.focusCells && step.focusCells.length) s.push("cell");
  s.push("deduction");
  return s;
}

let focusedRow = null; // the row element currently driving the board highlight

function techStage(groups, masks) {
  focusedRow = null;
  const wrap = document.createElement("div");
  wrap.className = "hint-stage";

  // Organize by the puzzle's spec, not by raw difficulty: the toolbox the puzzle
  // is meant to be solved with (Allowed + Forced) is shown; Conceded and
  // out-of-scope techniques the solver also happens to find are tucked behind
  // "Show other techniques". Sorted by difficulty within each group.
  const sorted = [...groups].sort((a, b) => a.rep.technique.difficulty - b.rep.technique.difficulty);
  const primary = sorted.filter((g) => isInScope(g, masks));
  const other = sorted.filter((g) => !isInScope(g, masks));

  // Cheat: a one-tap shortcut that applies the easiest in-scope move (or the
  // easiest overall if nothing is in scope right now).
  if (cheatOn()) {
    const ctl = document.createElement("div");
    ctl.className = "hint-row-ctl";
    const ez = document.createElement("button");
    ez.className = "hint-apply";
    ez.textContent = "Apply easiest";
    ez.addEventListener("click", () => applyStep((primary[0] || sorted[0]).rep));
    ctl.appendChild(ez);
    wrap.appendChild(ctl);
  }

  const ul = document.createElement("ul");
  ul.className = "hint-list";
  for (const g of primary) ul.appendChild(buildRow(g));
  wrap.appendChild(ul);

  // Nothing from the intended toolbox applies at this position.
  if (!primary.length) {
    const note = document.createElement("p");
    note.className = "hint-note";
    note.textContent = "No in-scope technique applies right now.";
    wrap.insertBefore(note, ul);
  }

  if (other.length) {
    const more = document.createElement("button");
    more.className = "hint-more hint-harder";
    more.textContent = `Show other techniques (${other.length})`;
    more.addEventListener("click", () => {
      for (const g of other) ul.appendChild(buildRow(g));
      more.remove();
    });
    wrap.appendChild(more);
  }
  return wrap;
}

function focusRow(li, step, stage) {
  if (focusedRow && focusedRow !== li) focusedRow.classList.remove("focused");
  focusedRow = li;
  li.classList.add("focused");
  highlightStage(step, stage);
}

function buildRow(g) {
  // A single-instance group is one drillable row. A multi-instance group ("· N
  // spots") is a header that expands into one child row per instance.
  return g.count > 1 ? buildMultiRow(g) : buildStepRow(g.steps[0], g, "full");
}

// A header for a multi-instance group: staggers tier -> name, then "Show the N
// spots" expands into a child row per instance (inserted right after it).
function buildMultiRow(g) {
  const stages = ["tier", "name"];
  let r = 0;
  const li = document.createElement("li");
  li.className = "hint-row";

  const refresh = () => {
    li.replaceChildren();
    const text = document.createElement("div");
    text.className = "hint-row-text";
    text.append(...stepContent(g.rep, stages[r], g, "full"));
    li.appendChild(text);

    const ctl = document.createElement("div");
    ctl.className = "hint-row-ctl";
    const more = document.createElement("button");
    more.className = "hint-more";
    if (r < stages.length - 1) {
      more.textContent = "Reveal more";
      more.addEventListener("click", (e) => {
        e.stopPropagation();
        r += 1;
        refresh();
      });
    } else {
      // At the name stage: expand into the individual spots. Each child starts
      // collapsed at its own name (no highlight); revealing it shows the focus
      // cells (blue), so highlights are driven by "Reveal more", not by tapping.
      more.textContent = `Show the ${g.count} spots`;
      more.addEventListener("click", (e) => {
        e.stopPropagation();
        let after = li;
        for (const step of g.steps) {
          const child = buildStepRow(step, g, "name");
          child.classList.add("hint-child");
          after.after(child);
          after = child;
        }
        more.remove();
      });
    }
    ctl.appendChild(more);
    li.appendChild(ctl);
  };

  refresh();
  return li;
}

// A drillable row for a single step. `lead` is "full" (a standalone technique,
// preambled by tier + name) or "name" (an instance under a group header, which
// starts at its own name and then drills location/deduction).
function buildStepRow(step, group, lead) {
  const stages = stepStages(step, lead);
  let r = 0;
  const li = document.createElement("li");
  li.className = "hint-row";

  const refresh = () => {
    li.replaceChildren();
    const text = document.createElement("div");
    text.className = "hint-row-text";
    text.append(...stepContent(step, stages[r], group, lead));
    li.appendChild(text);

    const ctl = document.createElement("div");
    ctl.className = "hint-row-ctl";
    if (r < stages.length - 1) {
      const more = document.createElement("button");
      more.className = "hint-more";
      more.textContent = "Reveal more";
      more.addEventListener("click", (e) => {
        e.stopPropagation();
        r += 1;
        refresh();
        focusRow(li, step, stages[r]);
      });
      ctl.appendChild(more);
    }
    if (cheatOn()) {
      const ap = document.createElement("button");
      ap.className = "hint-apply";
      ap.textContent = "Apply";
      ap.addEventListener("click", (e) => {
        e.stopPropagation();
        applyStep(step);
      });
      ctl.appendChild(ap);
    }
    li.appendChild(ctl);
  };

  // Tapping a row re-focuses it (re-applies its highlight at its current depth).
  li.addEventListener("click", () => focusRow(li, step, stages[r]));
  refresh();
  return li;
}

// The text shown for `step` at a reveal stage (cumulative). `includeName`
// prepends the tier label (at the tier stage) and the technique name (at later
// stages); instance child rows omit it since the header carries the name.
function stepContent(step, stage, group, lead) {
  const nodes = [];
  if (stage === "tier") {
    nodes.push(textSpan(`A ${tierLabelForDifficulty(step.technique.difficulty)} technique`, "hint-primary"));
    return nodes;
  }
  if (lead && lead !== "none") {
    const name = displayName(group.id);
    // The count suffix ("· N spots") belongs on the group header (lead "full"),
    // not on the instance child rows (lead "name").
    const title = lead === "full" && group.count > 1 ? `${name} · ${group.count} spots` : name;
    nodes.push(textSpan(title, "hint-primary"));
  }
  if (stage === "name") return nodes;
  // Location facts accumulate as you drill: unit, then focus cells, then the
  // full deduction.
  const order = ["tier", "name", "unit", "cell", "deduction"];
  const cur = order.indexOf(stage);
  if (step.house && cur >= order.indexOf("unit")) {
    nodes.push(textSpan(`in ${houseLabel(step.house)}`, "hint-secondary"));
  }
  if (step.focusCells && step.focusCells.length && cur >= order.indexOf("cell")) {
    nodes.push(textSpan(`at ${step.focusCells.map(cellName).join(", ")}`, "hint-secondary"));
  }
  if (stage === "deduction") {
    nodes.push(textSpan(step.deductions.map(deductionText).join(", "), "hint-deduction"));
  }
  return nodes;
}

function textSpan(text, cls) {
  const s = document.createElement("span");
  s.className = cls;
  s.textContent = text;
  return s;
}

// Apply a step's deductions through the normal undoable commit path (cheat).
function applyStep(step, rerender) {
  commit(() => {
    for (const d of step.deductions) {
      if (given[d.cell] !== 0) continue;
      if (d.kind === "place") {
        value[d.cell] = d.digit;
        centerMarks[d.cell].clear();
        cornerMarks[d.cell].clear();
        clearPeerMarks(d.cell, d.digit); // already a cheat-only path
      } else {
        // The digit is no longer a candidate in either notation.
        centerMarks[d.cell].delete(d.digit);
        cornerMarks[d.cell].delete(d.digit);
      }
    }
  });
  render();
  // Re-render the layer the Apply was clicked from (technique tree by default;
  // the status layer passes openHint), so applying stays where you are and can
  // be spammed.
  if (finished) closeHint();
  else (rerender || renderTechStage)();
}

// The candidate set for an empty cell: every digit not already placed by a peer
// (row / col / box). Pure board logic -- mirrors what the solver reasons over,
// so the center notes it pens match the candidates an "Apply easiest" elimination
// will strike out.
function candidatesFor(i) {
  const used = new Set();
  for (let j = 0; j < N; j++) {
    if (j === i || !isPeer(i, j)) continue;
    const d = digitAt(j);
    if (d !== 0) used.add(d);
  }
  const cand = new Set();
  for (let d = 1; d <= 9; d++) if (!used.has(d)) cand.add(d);
  return cand;
}

// Cheat: pencil the full candidate set into every empty cell's center notes,
// overwriting them. This is what makes "Apply easiest" eliminations visible -- a
// ≠ deduction only has something to strike once the candidates are penned. Re-run
// it any time to refresh notes that placements have left stale.
function fillAllCandidates() {
  commit(() => {
    for (let i = 0; i < N; i++) {
      if (digitAt(i) !== 0) continue;
      centerMarks[i] = candidatesFor(i);
    }
  });
  render();
}

// ---- Cheat mode ----
// Enabled if the host is a private/dev IP, a persisted flag is set (via the
// console `window.cheat()` or a >=5s long-press on Hint), reflected by a colour
// flip on the Hint button so you can tell it's on.
const LONG_PRESS_MS = 5000;

// cheatOn() and the tri-state CHEAT_KEY default live in cheat.js (shared with the
// stats history). Toggling and the play-view UI reactions stay here.
function setCheat(on) {
  try {
    localStorage.setItem(CHEAT_KEY, on ? "1" : "0");
  } catch {
    /* ignore */
  }
  updateHintButton();
  updateSeedLine();
  // Reflect the new mode in an open panel (adds/removes Apply buttons).
  if (!document.getElementById("hintPanel").hidden) openHint();
}
function toggleCheat() {
  setCheat(!cheatOn());
}
function updateHintButton() {
  const btn = document.getElementById("hint");
  if (btn) btn.classList.toggle("cheat", cheatOn());
}

// ---- Solved screen ----
function showSolved(finalMs) {
  const dialog = document.getElementById("solvedDialog");
  document.getElementById("solvedTime").textContent = formatDuration(finalMs);
  dialog.showModal();
}

// ---- Load a game ----
// Swap in a game record: clues, solution, the player's saved work, and the
// timer. Resets gesture state and starts the clock. The undo/redo stacks are
// restored from the record (older records have none -> empty), so the timeline
// persists across closing and reopening the puzzle.
export function loadGame(g) {
  closeHint();
  game = g;
  parseLine(g.puzzle, given);
  parseLine(g.solution, solution);
  for (let i = 0; i < N; i++) {
    value[i] = g.value[i] || 0;
    // Old saves only carried `marks` (the grid notes) -> load them as center notes.
    const center = g.centerMarks ? g.centerMarks[i] : g.marks ? g.marks[i] : null;
    centerMarks[i] = new Set(center || []);
    cornerMarks[i] = new Set((g.cornerMarks && g.cornerMarks[i]) || []);
  }
  history.length = 0;
  redoStack.length = 0;
  for (const s of g.history || []) history.push(snapshotFromJSON(s));
  for (const s of g.redo || []) redoStack.push(snapshotFromJSON(s));
  clearSelection();
  activeDigit = 0;
  placing = true;
  markKind = MODE_CENTER;
  finished = g.status === "solved";
  timerBase = g.elapsedMs || 0;
  runStart = null;

  lastDigit = 0;
  lastCell = -1;
  lastNotesTap = 0;
  lastSelCell = -1;
  preTapSelection = null;
  firstTapWasLocked = false;
  digitSinglePushed = false;
  cellSinglePushed = false;

  updateNotesButton();
  setTitle(g);
  updateSeedLine();
  updateDigitButtons();
  updateHistoryButtons();
  render();
  resume();
  // Opening a puzzle counts as playing it, so it sorts to the top of Continue
  // even before the first move.
  store.updateGame(game.id, { lastPlayedAt: Date.now() });
}

function setTitle(g) {
  // The play view title shows what the player is working on.
  const h = document.getElementById("playTitle");
  if (!h) return;
  if (g.mode === "custom") {
    h.textContent = g.label ? `Custom · ${g.label}` : "Custom";
    return;
  }
  const name = techniqueName(curriculumIdFor(g.kindIndex));
  const mode = g.mode === "drill" ? "Drill" : "Train";
  h.textContent = `${name} · ${mode}`;
}

// Debug aid: under cheat mode, show the seed the current puzzle was generated
// from and how many generator attempts it took, on a single line (each absent on
// puzzles made before it was recorded). Re-run whenever the game changes
// (loadGame) or cheat is toggled mid-game (setCheat).
function updateSeedLine() {
  const el = document.getElementById("playSeed");
  if (!el) return;
  const parts = [];
  if (game && game.seed) parts.push(`Seed: ${game.seed}`);
  if (game && game.attempts != null) parts.push(`Attempts: ${game.attempts}`);
  if (parts.length && cheatOn()) {
    el.textContent = parts.join(" · ");
    el.hidden = false;
  } else {
    el.textContent = "";
    el.hidden = true;
  }
}

// kindIndex -> kebab id, filled in by initPlay from the curriculum.
let idByKind = {};
function curriculumIdFor(kindIndex) {
  return idByKind[kindIndex] || "";
}

// The hint's player-facing tier comes from the step's curriculum difficulty
// (so it covers every solver technique, not just the 16 in the campaign). Same
// thresholds as the lab's `Tier::of_difficulty`.
const TIER_LABELS = ["Beginner", "Intermediate", "Expert", "Master"];
function tierRank(difficulty) {
  if (difficulty < 15) return 0;
  if (difficulty < 30) return 1;
  if (difficulty < 100) return 2;
  return 3;
}
function tierLabelForDifficulty(difficulty) {
  return TIER_LABELS[tierRank(difficulty)];
}

// Core's cli_name differs from the curriculum id for locked candidates; alias so
// the display name resolves to the nice "Locked Candidates (...)" label.
const ID_ALIAS = { pointing: "lc-pointing", claiming: "lc-claiming" };
function displayName(id) {
  return techniqueName(ID_ALIAS[id] || id);
}

// Core cli_name -> lab::kinds index, so a hint step can be matched against the
// puzzle's spec masks. Techniques the solver finds but the lab doesn't model
// (skyscraper, finned/turbot fish, ...) aren't here -> treated as out-of-scope.
const LAB_KIND = {
  "naked-single": 0,
  "hidden-single": 1,
  pointing: 2,
  claiming: 3,
  "naked-pair": 4,
  "hidden-pair": 5,
  "naked-triple": 6,
  "hidden-triple": 7,
  "naked-quad": 8,
  "hidden-quad": 9,
  "x-wing": 10,
  swordfish: 11,
  jellyfish: 12,
  "xy-wing": 13,
  "xyz-wing": 14,
  "w-wing": 15,
};

// The spec masks driving the hint tree's Allowed/Conceded split. A custom game
// stores its own masks on the record (its spec isn't a single curriculum kind);
// a campaign game derives them from (kindIndex, mode) via the wasm bridge. Null
// on any failure, which the hint path treats as "everything in scope".
function specMasksFor(wasm) {
  if (game && game.specMasks) return game.specMasks;
  try {
    return wasm.specMasks(game.kindIndex, game.mode === "drill");
  } catch {
    return null;
  }
}

// Whether a hint group is part of the puzzle's intended toolbox -- Allowed or
// Forced (baseline = allowed|forced). Conceded and untagged/out-of-scope
// techniques are "other". With no spec, everything is treated as in-scope.
function isInScope(group, masks) {
  if (!masks) return true;
  const idx = LAB_KIND[group.id];
  if (idx === undefined) return false; // not modelled by the lab -> out of scope
  return (masks.baseline & (1 << idx)) !== 0;
}

// "Play from Forced" head start. Run the solver forward from the bare puzzle,
// applying every MODELLED technique easier than the target until none is left,
// and return the resulting 81 placements -- the givens plus everything placed up
// to the point where the forced technique is first needed. It uses the same wasm
// `hint()` engine the player's hints do, so the stopping point matches the board
// they then face. Eliminations made along the way are intentionally dropped (the
// caller keeps no marks); redoing them to make the forced technique apply is part
// of the exercise.
//
// `puzzleLine` is the minimal-clue line; `allowedMask` is the spec's ALLOWED
// techniques only (`1 << labKind`) -- NOT the forced ones, and NOT conceded/off.
// Only allowed techniques advance the board; the forced technique is the stopping
// point (what the player is here to apply), so we never apply it. There is no
// difficulty filter -- any allowed technique progresses the board, of any
// difficulty. Since the puzzle requires its forced technique, the allowed set
// alone gets stuck exactly where a forced technique is first needed. (This also
// stops a degenerate spec -- e.g. hidden-triple forced but the subsets it needs
// turned off -- from being fully solved by off-spec techniques.) The board is
// rebuilt each pass from the placements plus an accumulated candidate mask, so a
// technique's eliminations carry forward instead of being re-offered. Returns null
// on any wasm hiccup or an empty allowed set, so the caller falls back to a plain
// start.
export function forcedStartValues(puzzleLine, allowedMask) {
  const wasm = bindings();
  if (!wasm) return null;
  if (!allowedMask) return null;

  const cells = new Uint8Array(N);
  parseLine(puzzleLine, cells);
  // 0 = unspecified (the Board keeps its grid-derived candidates). Once a
  // technique strikes a candidate from a cell we seed that cell to all-9 and
  // clear the struck bits, so the reduced set rides forward to the next pass.
  const cand = new Uint16Array(N);

  for (let guard = 0; guard < 256; guard++) {
    let steps, board;
    try {
      board = new wasm.Board(cells, cand);
    } catch {
      return null;
    }
    try {
      steps = wasm.hint(board);
    } catch {
      return null;
    } finally {
      board.free();
    }
    // Apply every available allowed step (each is a sound deduction on this same
    // board), then re-derive. Stop once no allowed technique applies -- the board
    // is "at" the forced technique. Forced/conceded/off techniques are not in
    // `allowedMask`, so they're never applied.
    let progressed = false;
    for (const s of steps) {
      const labIdx = LAB_KIND[s.technique.id];
      if (labIdx === undefined) continue;
      if ((allowedMask & (1 << labIdx)) === 0) continue;
      for (const d of s.deductions) {
        if (d.kind === "place") {
          if (cells[d.cell] === 0) {
            cells[d.cell] = d.digit;
            progressed = true;
          }
        } else if (cells[d.cell] === 0) {
          if (cand[d.cell] === 0) cand[d.cell] = 0x1ff;
          const bit = 1 << (d.digit - 1);
          if (cand[d.cell] & bit) {
            cand[d.cell] &= ~bit;
            progressed = true;
          }
        }
      }
    }
    if (!progressed) break;
  }
  return Array.from(cells);
}

// ---- Wiring ----
// Double-tap detection that never delays a single tap: the first tap acts
// immediately and records how to undo itself; a second tap on the same digit
// within DOUBLE_MS rolls that back and runs the double-tap action instead.
//
// Pad gestures:
//   unlocked, tap                          -> place into the selected cell
//   unlocked, double-tap                   -> pen-lock that digit
//   locked,   tap                          -> unlock (place nothing)
//   locked,   double-tap (same digit)      -> unlock
//   locked,   double-tap (different digit) -> switch the lock to that digit
const DOUBLE_MS = 280;
let lastDigit = 0;
let lastTapTime = 0;
// How to undo the most recent single tap if it turns out to be a double:
let firstTapWasLocked = false; // the tap happened while locked (it unlocked)
let prevLock = 0; // the digit that was locked before that unlock
let digitSinglePushed = false; // the single tap placed a digit (undo entry exists)

function onDigitTap(d) {
  if (finished) return;
  const now = performance.now();
  const isDouble = d === lastDigit && now - lastTapTime < DOUBLE_MS;
  lastTapTime = now;

  if (isDouble) {
    lastDigit = 0; // don't let a third quick tap read as another double
    if (firstTapWasLocked) {
      // The first tap already unlocked `prevLock`. Same digit -> stay
      // unlocked; a different digit -> switch the lock to it.
      setLock(d === prevLock ? 0 : d);
    } else {
      // The first tap placed into the selected cell -> roll that back (it was
      // recorded on the undo stack), then pen-lock instead. Net: no edit.
      if (digitSinglePushed) {
        applySnapshot(history.pop());
        digitSinglePushed = false;
        updateHistoryButtons();
      }
      setLock(d);
    }
    return;
  }

  // First tap: act immediately, remembering how a follow-up double undoes it.
  lastDigit = d;
  if (activeDigit !== 0) {
    firstTapWasLocked = true;
    prevLock = activeDigit;
    digitSinglePushed = false;
    setLock(0); // unlock now; place nothing
  } else {
    firstTapWasLocked = false;
    prevLock = 0;
    digitSinglePushed = inputDigit(d);
  }
}

function wirePad() {
  for (const btn of document.querySelectorAll("#playView .key[data-digit]")) {
    const d = Number(btn.dataset.digit);
    digitBtns[d - 1] = btn;
    btn.addEventListener("click", () => onDigitTap(d));
  }
  document.getElementById("erase").addEventListener("click", erase);
  notesBtn.addEventListener("click", onNotesTap);
  if (undoBtn) undoBtn.addEventListener("click", undo);
  if (redoBtn) redoBtn.addEventListener("click", redo);
}

// Keyboard only acts while the play view is on screen.
function playVisible() {
  const v = document.getElementById("playView");
  return v && !v.hidden;
}

function wireKeyboard() {
  window.addEventListener("keydown", (e) => {
    if (!playVisible()) return;
    if (e.key >= "1" && e.key <= "9") {
      inputDigit(Number(e.key));
    } else if (e.key === "0" || e.key === "Backspace" || e.key === "Delete") {
      erase();
    } else if (e.key === "ArrowUp") {
      moveSelection(-1, 0, e.shiftKey);
    } else if (e.key === "ArrowDown") {
      moveSelection(1, 0, e.shiftKey);
    } else if (e.key === "ArrowLeft") {
      moveSelection(0, -1, e.shiftKey);
    } else if (e.key === "ArrowRight") {
      moveSelection(0, 1, e.shiftKey);
    } else if (e.key === "n") {
      togglePlacing(); // place <-> mark
    } else if (e.key === "N") {
      switchMarkKind(); // Center <-> Corner
    } else if (
      // Redo before undo: Shift+Z reports e.key "Z", which the undo branch also
      // matches, so the redo (Ctrl/Cmd+Shift+Z or Ctrl/Cmd+Y) check must win.
      (e.ctrlKey || e.metaKey) &&
      (e.key === "y" || e.key === "Y" || ((e.key === "z" || e.key === "Z") && e.shiftKey))
    ) {
      redo();
    } else if ((e.ctrlKey || e.metaKey) && (e.key === "z" || e.key === "Z")) {
      undo();
    } else {
      return;
    }
    e.preventDefault();
  });
}

function wireHint() {
  const hintBtn = document.getElementById("hint");
  document.getElementById("hintClose").addEventListener("click", closeHint);

  // A normal tap (click) toggles the panel. A >=5s hold toggles cheat mode; the
  // click that ends the hold is swallowed. The pointer timer only detects the
  // hold -- opening rides on the reliable `click` event. No hold animation; the
  // persistent colour flip on the button is the only cheat indicator.
  let timer = null;
  let longFired = false;
  const startHold = () => {
    longFired = false;
    timer = setTimeout(() => {
      longFired = true;
      toggleCheat();
    }, LONG_PRESS_MS);
  };
  const cancelHold = () => {
    if (timer) {
      clearTimeout(timer);
      timer = null;
    }
  };
  hintBtn.addEventListener("pointerdown", startHold);
  hintBtn.addEventListener("pointerup", cancelHold);
  hintBtn.addEventListener("pointerleave", cancelHold);
  hintBtn.addEventListener("pointercancel", cancelHold);
  hintBtn.addEventListener("click", () => {
    if (longFired) {
      longFired = false; // the long-press already toggled cheat; swallow this tap
      return;
    }
    toggleHintPanel();
  });

  // Console activation: `cheat()` toggles, `cheat(true|false)` sets.
  window.cheat = (on) => (on === undefined ? toggleCheat() : setCheat(!!on));
}

// The play view top bar: a home button (save + leave) and an overflow menu
// (New puzzle of the same spec, Restart).
function wireTopbar() {
  const homeBtn = document.getElementById("home");
  const menuBtn = document.getElementById("menuBtn");
  const menuList = document.getElementById("menuList");

  const closeMenu = () => {
    menuList.hidden = true;
    menuBtn.setAttribute("aria-expanded", "false");
  };

  menuBtn.addEventListener("click", (e) => {
    e.stopPropagation(); // don't let the document handler immediately re-close
    const open = menuList.hidden;
    // "Fill all candidates" is a cheat-only affordance; reveal it only while
    // cheat mode is on, re-checked each time the menu opens.
    document.getElementById("menuFill").hidden = !cheatOn();
    menuList.hidden = !open;
    menuBtn.setAttribute("aria-expanded", String(open));
  });

  menuList.addEventListener("click", (e) => {
    const item = e.target.closest(".menu-item");
    if (!item) return;
    // Export = copy the puzzle's clue line (`g.puzzle` is already to_line()
    // output, '.' = empty; the solution is never copied). Keep the menu open
    // briefly to confirm, so stop the click from bubbling to the close handler.
    if (item.dataset.action === "copy") {
      e.stopPropagation();
      const label = item.textContent;
      copyText(game.puzzle).then((ok) => {
        item.textContent = ok ? "Copied!" : "Failed";
        setTimeout(() => {
          item.textContent = label;
          closeMenu();
        }, 900);
      });
      return;
    }
    if (item.dataset.action === "restart") restart();
    else if (item.dataset.action === "fillCandidates") fillAllCandidates();
    else if (item.dataset.action === "settings") onSettings();
    else if (item.dataset.action === "help") onHelp();
    closeMenu();
  });

  // Dismiss the menu on any outside click or Escape (while on the play view).
  document.addEventListener("click", () => {
    if (playVisible()) closeMenu();
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && playVisible()) closeMenu();
  });

  homeBtn.addEventListener("click", () => {
    closeHint();
    pause();
    onHome();
  });
}

function wireSolved() {
  document.getElementById("solvedHome").addEventListener("click", () => {
    document.getElementById("solvedDialog").close();
    onHome();
  });
  document.getElementById("solvedNew").addEventListener("click", () => {
    document.getElementById("solvedDialog").close();
    onNewPuzzle(game);
  });
}

// Build the board and wire all controls once. `curriculum` maps kindIndex -> id
// for the title; the callbacks navigate the app shell.
export function initPlay({ curriculum, onHome: home, onNewPuzzle: newPuzzle, onSettings: settings, onHelp: help }) {
  onHome = home;
  onNewPuzzle = newPuzzle;
  onSettings = settings;
  onHelp = help || (() => {});
  idByKind = {};
  for (const t of curriculum) idByKind[t.kindIndex] = t.id;

  boardEl = document.getElementById("board");
  notesBtn = document.getElementById("notes");
  undoBtn = document.getElementById("undo");
  redoBtn = document.getElementById("redo");

  buildBoard();
  wirePad();
  wireKeyboard();
  wireTopbar();
  wireHint();
  wireSolved();
  updateNotesButton();
  updateHistoryButtons();
  updateHintButton();
}
