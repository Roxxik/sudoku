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

let selected = null; // index 0..80, or null
// Input mode: NORMAL places values, CENTER/CORNER pencil the two note kinds. The
// Notes button cycles NORMAL -> CENTER -> CORNER -> NORMAL.
const MODE_NORMAL = 0, MODE_CENTER = 1, MODE_CORNER = 2;
const NOTE_LABELS = ["Notes", "Center", "Corner"];
let noteMode = MODE_NORMAL;
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
// `snapshot`).
function snapshotToJSON(s) {
  return {
    v: s.v,
    c: s.c.map((set) => [...set]),
    n: s.n.map((set) => [...set]),
  };
}

function snapshotFromJSON(s) {
  // Legacy persisted snapshots only had `m` (the old single mark grid) -> center,
  // mirroring the same migration loadGame applies to a record's top-level marks.
  const c = s.c || s.m || [];
  return {
    v: (s.v || []).slice(),
    c: c.map((a) => new Set(a)),
    n: (s.n || []).map((a) => new Set(a)),
  };
}

function applySnapshot(s) {
  for (let i = 0; i < N; i++) {
    value[i] = s.v[i];
    centerMarks[i] = new Set(s.c[i]);
    cornerMarks[i] = new Set(s.n[i]);
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
  render();
}

function redo() {
  if (redoStack.length === 0) return;
  history.push(snapshot()); // the state we leave becomes undoable again
  applySnapshot(redoStack.pop());
  updateHistoryButtons();
  persist();
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
  persist();
}

// Resume the clock, unless the puzzle is already solved.
export function resume() {
  if (!finished && game && runStart === null) {
    runStart = performance.now();
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

    cell.addEventListener("pointerdown", (e) => {
      e.preventDefault();
      onCellTap(i);
    });

    boardEl.appendChild(cell);
    cells.push(cell);
  }
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
  // The highlighted digit is the pen-locked digit if any, else whatever sits
  // in the selected cell.
  const highlightDigit =
    activeDigit !== 0 ? activeDigit : selected !== null ? digitAt(selected) : 0;

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

    // Highlight classes.
    cell.classList.toggle("selected", i === selected);
    cell.classList.toggle("peer", selected !== null && isPeer(selected, i));
    cell.classList.toggle(
      "same",
      highlightDigit !== 0 && i !== selected && d === highlightDigit
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
function select(i) {
  selected = i;
  render();
}

// Cell-tap state for pen-lock double-tap detection (place <-> note).
let lastCell = -1;
let lastCellTime = 0;
let cellSinglePushed = false; // the last cell single-tap recorded an undo entry

// Board tap. Without a pen-locked digit this just selects the cell. With one,
// a tap places that digit (no selection highlight needed while placing), and a
// double-tap on the same cell instead applies the opposite mode -- so if normal
// taps place values, double-tapping pencils a mark, and vice-versa.
function onCellTap(i) {
  if (finished) return;
  if (activeDigit === 0) {
    select(i);
    return;
  }

  const now = performance.now();
  const isDouble = i === lastCell && now - lastCellTime < DOUBLE_MS;
  lastCellTime = now;

  if (isDouble) {
    lastCell = -1; // don't let a third quick tap read as another double
    // Roll the single tap back, then commit the opposite as one undo step.
    if (cellSinglePushed) {
      applySnapshot(history.pop());
      cellSinglePushed = false;
      updateHistoryButtons();
    }
    commit(() => applyOppositeToCell(i, activeDigit)); // the other thing
    render();
    return;
  }

  lastCell = i;
  cellSinglePushed = commit(() => applyDigitToCell(i, activeDigit));
  render();
}

function moveSelection(dr, dc) {
  if (selected === null) {
    select(0);
    return;
  }
  let r = Math.floor(selected / 9) + dr;
  let c = (selected % 9) + dc;
  r = (r + 9) % 9;
  c = (c + 9) % 9;
  select(r * 9 + c);
}

// Place / toggle a digit (or pencil mark) into a specific cell, per the global
// Notes mode. Clues are locked. Shared by the digit pad (cell-first) and the
// pen-lock single tap (digit-first).
function toggleMark(set, d) {
  if (set.has(d)) set.delete(d);
  else set.add(d);
}

function applyDigitToCell(i, d) {
  if (given[i] !== 0) return;
  if (noteMode === MODE_NORMAL) {
    // Toggle off if re-entering the same digit; placing clears both note kinds.
    value[i] = value[i] === d ? 0 : d;
    centerMarks[i].clear();
    cornerMarks[i].clear();
  } else {
    // Pencil marks are meaningless once a value is placed.
    if (value[i] !== 0) return;
    toggleMark(noteMode === MODE_CORNER ? cornerMarks[i] : centerMarks[i], d);
  }
}

// The pen-lock double-tap action: the opposite of a normal tap, pairing a placed
// value with the active note kind (CENTER in Normal/Center mode, CORNER in Corner
// mode). In Normal mode a normal tap places the value, so this pencils that note
// instead -- clearing the value first so the note is visible. In a note mode a
// normal tap pencils the note, so this places the value instead.
function applyOppositeToCell(i, d) {
  if (given[i] !== 0) return;
  if (noteMode === MODE_NORMAL) {
    value[i] = 0;
    toggleMark(centerMarks[i], d);
  } else {
    value[i] = value[i] === d ? 0 : d;
    centerMarks[i].clear();
    cornerMarks[i].clear();
  }
}

function inputDigit(d) {
  if (finished || selected === null) return false;
  const pushed = commit(() => applyDigitToCell(selected, d));
  render();
  return pushed;
}

// Set the pen-locked digit (0 clears it) and refresh the highlight.
function setLock(d) {
  activeDigit = d;
  if (activeDigit !== 0) selected = null; // pen mode owns the highlight
  updateDigitButtons();
  render();
}

function erase() {
  if (finished || selected === null) return;
  commit(() => {
    if (given[selected] !== 0) return;
    value[selected] = 0;
    centerMarks[selected].clear();
    cornerMarks[selected].clear();
  });
  render();
}

// Cycle the input mode: Normal -> Center notes -> Corner (Snyder) notes -> Normal.
function cycleNoteMode() {
  noteMode = (noteMode + 1) % 3;
  updateNotesButton();
}

// Reflect the current mode on the Notes button: its label names the mode, the
// data-mode drives the active colour (accent for Center, accent2 for Corner).
function updateNotesButton() {
  if (!notesBtn) return;
  notesBtn.textContent = NOTE_LABELS[noteMode];
  notesBtn.setAttribute("aria-pressed", String(noteMode !== MODE_NORMAL));
  notesBtn.dataset.mode = String(noteMode);
}

// Clear all of the player's work and return to the puzzle's starting clues. The
// clock restarts too -- a restart is a fresh attempt. (The clues live in
// `given`, so we only wipe the editable state.)
function restart() {
  closeHint();
  for (let i = 0; i < N; i++) {
    value[i] = 0;
    centerMarks[i] = new Set();
    cornerMarks[i] = new Set();
  }
  history.length = 0;
  redoStack.length = 0;
  selected = null;
  activeDigit = 0;
  noteMode = MODE_NORMAL;
  finished = false;
  timerBase = 0;
  runStart = performance.now();
  // Reset double-tap gesture state so a stray pending double can't pop the
  // now-empty undo stack.
  lastDigit = 0;
  lastCell = -1;
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
    history: [],
    redo: [],
  });
  game = store.getGame(game.id);
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
  selected = null;
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
  const cellDigits = new Uint8Array(N);
  for (let i = 0; i < N; i++) cellDigits[i] = digitAt(i);
  let steps, isSolved;
  try {
    const board = new wasm.Board(cellDigits);
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
    const cellDigits = new Uint8Array(N);
    for (let i = 0; i < N; i++) cellDigits[i] = digitAt(i);
    try {
      const board = new wasm.Board(cellDigits);
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
  selected = null;
  activeDigit = 0;
  noteMode = MODE_NORMAL;
  finished = g.status === "solved";
  timerBase = g.elapsedMs || 0;
  runStart = null;

  lastDigit = 0;
  lastCell = -1;
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
// from (absent on puzzles made before seeds were recorded). Re-run whenever the
// game changes (loadGame) or cheat is toggled mid-game (setCheat).
function updateSeedLine() {
  const el = document.getElementById("playSeed");
  if (!el) return;
  const seed = game && game.seed;
  if (seed && cheatOn()) {
    el.textContent = `Seed: ${seed}`;
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
  notesBtn.addEventListener("click", cycleNoteMode);
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
      moveSelection(-1, 0);
    } else if (e.key === "ArrowDown") {
      moveSelection(1, 0);
    } else if (e.key === "ArrowLeft") {
      moveSelection(0, -1);
    } else if (e.key === "ArrowRight") {
      moveSelection(0, 1);
    } else if (e.key === "n" || e.key === "N") {
      cycleNoteMode();
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
        item.textContent = ok ? "Copied!" : "Copy failed";
        setTimeout(() => {
          item.textContent = label;
          closeMenu();
        }, 900);
      });
      return;
    }
    if (item.dataset.action === "restart") restart();
    else if (item.dataset.action === "generate") onNewPuzzle(game);
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
export function initPlay({ curriculum, onHome: home, onNewPuzzle: newPuzzle }) {
  onHome = home;
  onNewPuzzle = newPuzzle;
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
