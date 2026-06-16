"use strict";

// The start page and the campaign navigation.
//
// Start page (homeView), fixed order so entries hold their place as they
// appear/disappear:
//   1. Campaign  -- three tier buttons (Beginner / Intermediate / Expert),
//                   always shown. Tapping a tier drills in.
//   2. Custom puzzle  -- the spec builder (a separator sits above it).
//   3. Continue last puzzle  -- shown when there is >=1 in-progress puzzle.
//   4. Continue a puzzle (list)  -- shown when there are >=2.
// Each tier button carries a subtitle listing its techniques (or, for the
// multi-branch Expert tier, its branch names).
//
// Campaign drill-down (campaignView): tier -> branches (Expert only) -> a
// technique list (shown only when a level holds more than one technique) -> the
// technique page. The technique page has a description, a row per mode (Train,
// and Drill where it differs) offering Play and Play from Forced, and any
// aliases/extra notes. A single-technique level (e.g. Beginner) skips the list and
// opens the page directly. Solved-count rollups appear at every level. The puzzle
// list (puzzlesView) lists all in-progress games.

import * as store from "./store.js";
import * as settings from "./settings.js";
import { miniBoard, textColumn, copyText } from "./ui.js";
import { TECHNIQUE_INFO } from "./techniques.js";
import {
  formatDuration,
  techniqueName,
  BRANCH_LABEL,
  TIER_ORDER,
  TIER_LABEL,
} from "./util.js";

let curriculum = [];
let showView = () => {};
let onLaunch = () => {};
let onResume = () => {};
let onOpenSpec = () => {};
let onImport = () => {};

let grouped = {}; // tier -> branch -> [entries], sorted by difficulty
let campaignBack = showHome; // where campaignView's back button goes right now

export function initHome(opts) {
  curriculum = opts.curriculum;
  showView = opts.showView;
  onLaunch = opts.onLaunch;
  onResume = opts.onResume;
  onOpenSpec = opts.onOpenSpec || (() => {});
  onImport = opts.onImport || (() => {});
  grouped = groupCurriculum(curriculum);

  document.getElementById("statsBtn").addEventListener("click", opts.onStats);
  document.getElementById("customBtn").addEventListener("click", opts.onCustom);
  document.getElementById("settingsBack").addEventListener("click", () => settingsReturn());
  document.getElementById("helpBack").addEventListener("click", () => helpReturn());
  wireHomeMenu();
  document.getElementById("campaignBack").addEventListener("click", () => campaignBack());
  document.getElementById("puzzlesBack").addEventListener("click", showHome);
  document.getElementById("continueListBtn").addEventListener("click", openPuzzles);

  // Per-card overflow menus (the Continue cards) close on any outside click or
  // Escape -- across both the home hero and the puzzles list.
  document.addEventListener("click", () => closeCardMenus());
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") closeCardMenus();
  });
}

function groupCurriculum(list) {
  const g = {};
  for (const t of list) ((g[t.tier] ||= {})[t.branch] ||= []).push(t);
  for (const tier of Object.values(g))
    for (const entries of Object.values(tier)) entries.sort((a, b) => a.difficulty - b.difficulty);
  return g;
}

// ---- Start page ----
export function renderHome() {
  const stats = store.statsByKind();
  renderTiers(stats);
  renderContinue();
  // The Stats page also hosts the solved-puzzle history (a debug aid), so it
  // earns its button as soon as anything -- campaign or custom -- has been
  // solved. Custom games are excluded from statsByKind, so count solved games.
  document.getElementById("statsBtn").hidden = store.solvedGames().length === 0;
  showHome();
}

function showHome() {
  showView("homeView");
}

function renderTiers(stats) {
  const root = document.getElementById("tierButtons");
  root.replaceChildren();
  for (const tier of TIER_ORDER) {
    const branches = grouped[tier];
    if (!branches) continue;
    const entries = Object.values(branches).flat();
    root.appendChild(
      navButton(TIER_LABEL[tier], tierSubtitle(tier), rollup(stats, entries), () => openTier(tier))
    );
  }
}

// The subtitle under a tier button on the home page: a multi-branch tier (Expert)
// lists its branch names; a single-branch tier (Beginner/Intermediate) lists its
// technique names.
function tierSubtitle(tier) {
  const branches = grouped[tier];
  if (Object.keys(branches).length > 1) {
    return BRANCH_ORDER.filter((b) => branches[b])
      .map((b) => BRANCH_LABEL[b])
      .join(" · ");
  }
  return techniqueNames(Object.values(branches).flat());
}

function renderContinue() {
  const active = store.activeGames();
  const lastCard = document.getElementById("continueLast");
  const listBtn = document.getElementById("continueListBtn");

  lastCard.hidden = active.length < 1;
  listBtn.hidden = active.length < 2;
  // The Custom/Continue separator only earns its place when there's a Continue
  // card below it.
  const sep = document.getElementById("continueSep");
  if (sep) sep.hidden = active.length < 1;

  if (active.length >= 1) {
    // The hero keeps its "Continue last puzzle" label (it's the resume-the-most-
    // recent affordance), but now carries the per-puzzle overflow menu too.
    lastCard.replaceChildren(continueCard(active[0], "Continue last puzzle", "mini-lg"));
  } else {
    lastCard.replaceChildren();
  }
  if (active.length >= 2) {
    // No board preview here -- this is a link to the list, not a single puzzle.
    listBtn.replaceChildren(textColumn("Continue a puzzle", `${active.length} in progress`));
  }
}

// One in-progress puzzle as a card: a full-width Resume button (board + title +
// meta) plus an ellipsis overflow menu. Shared by the home hero and the puzzles
// list, which differ only in the title shown and the thumbnail size.
function continueCard(g, title, sizeClass) {
  const card = document.createElement("div");
  card.className = "continue-card";
  const main = document.createElement("button");
  main.className = "continue-item";
  main.addEventListener("click", () => onResume(g.id));
  main.append(miniBoard(g, sizeClass), textColumn(title, continueMeta(g)));
  card.append(main, cardMenu(g));
  return card;
}

// The per-card overflow menu for an in-progress puzzle: Export its clue line, and
// (custom only) reopen its spec in the builder. Deliberately separate from the
// Stats history cards -- those are solved games. The ellipsis toggles the
// dropdown; outside-click/Escape dismissal is wired once in initHome.
function cardMenu(g) {
  const menu = document.createElement("div");
  menu.className = "menu ci-menu";

  const btn = document.createElement("button");
  btn.className = "iconbtn ci-menu-btn";
  btn.setAttribute("aria-label", "Puzzle menu");
  btn.setAttribute("aria-haspopup", "true");
  btn.setAttribute("aria-expanded", "false");
  btn.innerHTML =
    '<svg viewBox="0 0 24 24" width="22" height="22" aria-hidden="true">' +
    '<circle cx="12" cy="5" r="2" fill="currentColor"/>' +
    '<circle cx="12" cy="12" r="2" fill="currentColor"/>' +
    '<circle cx="12" cy="19" r="2" fill="currentColor"/></svg>';

  const list = document.createElement("ul");
  list.className = "menu-list";
  list.hidden = true;
  // Custom games can resurface their spec; campaign/imported games can't.
  if (g.mode === "custom" && Array.isArray(g.spec)) {
    list.appendChild(menuItemLi("Open spec", () => onOpenSpec(g.spec)));
  }
  list.appendChild(exportItemLi(g));
  list.appendChild(menuItemLi("Remove", () => removeGame(g), "danger"));

  btn.addEventListener("click", (e) => {
    e.stopPropagation(); // don't let the document handler immediately re-close
    closeCardMenus(list); // only one card menu open at a time
    const open = list.hidden;
    list.hidden = !open;
    btn.setAttribute("aria-expanded", String(open));
  });

  menu.append(btn, list);
  return menu;
}

// A plain menu row: run `onClick`, then dismiss the menu. `cls` adds an extra
// class (e.g. "danger" for a destructive action).
function menuItemLi(label, onClick, cls) {
  const li = document.createElement("li");
  const b = document.createElement("button");
  b.className = cls ? `menu-item ${cls}` : "menu-item";
  b.textContent = label;
  b.addEventListener("click", (e) => {
    e.stopPropagation();
    closeCardMenus();
    onClick();
  });
  li.appendChild(b);
  return li;
}

// Discard an in-progress puzzle after confirming -- progress is lost, so we
// double-check. Refreshes whatever Continue surface is showing; an emptied
// puzzles subscreen drops back Home.
function removeGame(g) {
  if (!window.confirm("Remove this puzzle? Your progress will be lost.")) return;
  store.deleteGame(g.id);
  renderContinue();
  if (!document.getElementById("puzzlesView").hidden) {
    if (store.activeGames().length === 0) showHome();
    else openPuzzles();
  }
}

// Export = copy the puzzle's clue line (`g.puzzle` is already to_line() output,
// '.' = empty; the solution is never copied), with a brief inline confirmation.
function exportItemLi(g) {
  const li = document.createElement("li");
  const b = document.createElement("button");
  b.className = "menu-item";
  b.textContent = "Export puzzle";
  b.addEventListener("click", (e) => {
    e.stopPropagation(); // keep the menu open to show the confirmation
    const label = b.textContent;
    copyText(g.puzzle).then((ok) => {
      b.textContent = ok ? "Copied!" : "Failed";
      setTimeout(() => {
        b.textContent = label;
        closeCardMenus();
      }, 900);
    });
  });
  li.appendChild(b);
  return li;
}

// Close every open Continue-card menu, optionally keeping `except` open. Card
// menus appear in both the home hero and the puzzles list, so this spans views.
function closeCardMenus(except) {
  for (const list of document.querySelectorAll(".ci-menu .menu-list")) {
    if (list === except || list.hidden) continue;
    list.hidden = true;
    const btn = list.parentElement.querySelector(".ci-menu-btn");
    if (btn) btn.setAttribute("aria-expanded", "false");
  }
}

function continueMeta(g) {
  const time = formatDuration(g.elapsedMs);
  if (g.mode === "custom") return `Custom · ${time}`;
  const mode = g.mode === "drill" ? "Drill" : "Train";
  return `${techniqueName(idFor(g.kindIndex))} · ${mode} · ${time}`;
}

// The title for a game card: a custom game's own label, else its technique name.
function gameTitle(g) {
  return g.mode === "custom" ? g.label || "Custom" : techniqueName(idFor(g.kindIndex));
}

// ---- Campaign drill-down ----
// Branch display order; also the order the Expert tier lists its branches in.
const BRANCH_ORDER = ["fish", "subset", "bivalue", "trunk"];

function openTier(tier) {
  const keys = Object.keys(grouped[tier]);
  // A multi-branch tier (Expert) gets the branch level; a single-branch tier
  // dives straight in: to its technique list, or (Beginner) the lone page.
  if (keys.length > 1) openBranches(tier);
  else openBranchOrTechniques(tier, keys[0], showHome);
}

// One branch: a list of technique pages when it holds more than one technique,
// else straight to that technique's page. `back` is where the page above it goes.
function openBranchOrTechniques(tier, branch, back) {
  const entries = grouped[tier][branch];
  if (entries.length > 1) openTechniqueList(tier, branch, back);
  else openTechnique(tier, branch, entries[0], back);
}

function openBranches(tier) {
  const stats = store.statsByKind();
  setCampaignTitle(TIER_LABEL[tier]);
  const grid = document.createElement("div");
  grid.className = "nav-grid";
  for (const branch of BRANCH_ORDER) {
    const entries = grouped[tier][branch];
    if (!entries) continue;
    grid.appendChild(
      navButton(
        BRANCH_LABEL[branch],
        techniqueNames(entries),
        rollup(stats, entries),
        () => openBranchOrTechniques(tier, branch, () => openBranches(tier))
      )
    );
  }
  const note = document.createElement("div");
  note.className = "mode-explainer";
  note.appendChild(
    explPlain(
      "These branches are only roughly ordered by difficulty — it mainly climbs within each branch. Work through the easier techniques in each branch before going deeper into it."
    )
  );
  document.getElementById("campaignBody").replaceChildren(grid, note);
  campaignBack = showHome;
  showView("campaignView");
}

// A list of technique pages within one tier/branch (shown only when there's more
// than one). Each row is a nav-button to that technique's page, badged with its
// solved count. `back` is the level above (the branch list, or Home).
function openTechniqueList(tier, branch, back) {
  const stats = store.statsByKind();
  const multiBranch = Object.keys(grouped[tier]).length > 1;
  setCampaignTitle(
    multiBranch ? `${TIER_LABEL[tier]} · ${BRANCH_LABEL[branch]}` : TIER_LABEL[tier]
  );
  const grid = document.createElement("div");
  grid.className = "nav-grid";
  for (const t of grouped[tier][branch]) {
    grid.appendChild(
      navButton(
        techniqueName(t.id),
        null,
        store.solvedCountForKind(stats, t.kindIndex),
        () => openTechnique(tier, branch, t, () => openTechniqueList(tier, branch, back))
      )
    );
  }
  document.getElementById("campaignBody").replaceChildren(grid);
  campaignBack = back;
  showView("campaignView");
}

// The technique page: a hand-written description up top, a row per mode (Train
// always, Drill when it differs) each offering Play and Play from Forced, a short
// note on what Train vs Drill mean here, and any aliases / extra notes at the
// bottom. `back` returns to whichever level opened it.
function openTechnique(tier, branch, t, back) {
  const stats = store.statsByKind();
  setCampaignTitle(techniqueName(t.id));

  const page = document.createElement("div");
  page.className = "tech-page";

  const info = TECHNIQUE_INFO[t.id] || {};
  if (info.description) {
    const desc = document.createElement("p");
    desc.className = "tech-desc";
    desc.textContent = info.description;
    page.appendChild(desc);
  }

  // Play from Forced has nothing to place before the easiest technique of all
  // (the hidden single), so only offer it when something easier than this exists.
  const showForced = curriculum.some((k) => k.difficulty < t.difficulty);
  page.appendChild(modeRow(t, "train", stats, showForced));
  if (t.hasDrill) page.appendChild(modeRow(t, "drill", stats, showForced));

  page.appendChild(modeNote(t, tier, branch));

  const extra = techExtra(info);
  if (extra) page.appendChild(extra);

  document.getElementById("campaignBody").replaceChildren(page);
  campaignBack = back;
  showView("campaignView");
}

// One mode (Train or Drill) as a row: its label over the start buttons. Play uses
// the minimal givens; Play from Forced seeds the board with the digits the solver
// places up to the first forced technique (omitted when nothing is easier).
function modeRow(t, mode, stats, showForced) {
  const row = document.createElement("div");
  row.className = "mode-row";

  const head = document.createElement("div");
  head.className = "mode-row-head";
  head.textContent = mode === "drill" ? "Drill" : "Train";
  row.appendChild(head);

  const btns = document.createElement("div");
  btns.className = "mode-buttons";
  btns.appendChild(playButton(t, mode, "Play", false, stats));
  if (showForced) btns.appendChild(playButton(t, mode, "Play from Forced", true, stats));
  row.appendChild(btns);
  return row;
}

// A start button. `fromForced` picks the head-start launch. The solved marker
// (count + best time) under the label is specific to this start type -- a plain
// Play and a Play-from-Forced are tracked separately, so the two buttons in a row
// don't share a count.
function playButton(t, mode, label, fromForced, stats) {
  const btn = document.createElement("button");
  btn.className = "mode-btn";
  btn.addEventListener("click", () => onLaunch(t.kindIndex, mode, fromForced));

  const lab = document.createElement("span");
  lab.className = "mb-label";
  lab.textContent = label;
  btn.appendChild(lab);

  const sub = document.createElement("span");
  sub.className = "mb-sub";
  const m = stats[t.kindIndex]?.[mode + (fromForced ? "Forced" : "")];
  if (m && m.count > 0) {
    btn.classList.add("solved");
    sub.textContent = `${m.count} solved · ${formatDuration(m.bestMs)}`;
  } else {
    sub.textContent = "";
  }
  btn.appendChild(sub);
  return btn;
}

// A short note on what Train and Drill mean for this technique. Phrasing tracks
// CURRICULUM.md: an Intermediate technique sits in a flat tier (peers = the others
// on its page), an Expert one in an ordered branch (peers = the simpler ones above
// it). A train-only technique (Beginner) gets a single shared line.
function modeNote(t, tier, branch) {
  const box = document.createElement("div");
  box.className = "mode-explainer";

  if (!t.hasDrill) {
    // The very easiest technique (hidden single) has nothing simpler to lean on;
    // the others reachable here (the first of an Expert branch) do.
    const hasEasier = curriculum.some((k) => k.difficulty < t.difficulty);
    box.appendChild(
      explPlain(
        hasEasier
          ? "Train builds puzzles that need this technique; the easier techniques you already know may also be needed along the way."
          : "Train builds puzzles that are solved with this technique alone."
      )
    );
    return box;
  }

  const peers =
    tier === "intermediate"
      ? "the other Intermediate techniques"
      : `the simpler ${{ fish: "fishes", subset: "subsets", bivalue: "wings" }[branch]} in this branch`;
  box.appendChild(explMode("Train", `${peers}, plus the basics, may also be needed to finish the puzzle.`));
  box.appendChild(explMode("Drill", `only this technique is required; ${peers} won't be needed to finish it.`));
  return box;
}

// The aliases + extra-info block at the bottom of a technique page, or null when
// the technique has neither.
function techExtra(info) {
  const aliases = info.aliases && info.aliases.length ? info.aliases : null;
  if (!aliases && !info.extra) return null;
  const box = document.createElement("div");
  box.className = "mode-explainer tech-extra";
  if (aliases) box.appendChild(explMode("Also known as", aliases.join(", ")));
  if (info.extra) box.appendChild(explPlain(info.extra));
  return box;
}

function explPlain(text) {
  const p = document.createElement("p");
  p.textContent = text;
  return p;
}

function explMode(label, rest) {
  const p = document.createElement("p");
  const strong = document.createElement("strong");
  strong.textContent = label;
  p.append(strong, ` — ${rest}`);
  return p;
}

function setCampaignTitle(text) {
  document.getElementById("campaignTitle").textContent = text;
}

// ---- Continue-a-puzzle list ----
function openPuzzles() {
  const list = document.getElementById("puzzlesList");
  list.replaceChildren();
  for (const g of store.activeGames()) {
    const li = document.createElement("li");
    li.appendChild(continueCard(g, gameTitle(g), "mini-xl"));
    list.appendChild(li);
  }
  showView("puzzlesView");
}

// ---- Settings ----
// One screen of app-wide preferences, persisted via settings.js. Lives here (a
// static, wasm-free module) so it opens instantly. Reachable from the home
// overflow menu and the play view's menu.

// Where the settings back button returns to. Set per-open by openSettings so the
// screen routes back to wherever it was opened from (Home or the play view) --
// mirrors campaignBack.
let settingsReturn = showHome;

// Open the settings screen. `onBack` is where its top-left back button goes
// (defaults to Home); the play view passes a target that resumes the board.
export function openSettings(onBack) {
  settingsReturn = onBack || showHome;
  document
    .getElementById("settingsBody")
    .replaceChildren(
      settingToggle(
        "Eliminate candidates",
        "When you place a digit, strike it from the Center and Corner notes of every cell that sees it.",
        settings.eliminateCandidatesOn(),
        settings.setEliminateCandidates
      ),
      settingToggle(
        "Show timer",
        "Show a running timer above the board while you play.",
        settings.showTimerOn(),
        settings.setShowTimer
      )
    );
  showView("settingsView");
}

// The home top-bar overflow menu (right of Stats). Settings for now; more items
// will follow. Mirrors the play view's menu: toggle open, dismiss on an outside
// click or Escape while Home is showing.
function wireHomeMenu() {
  const btn = document.getElementById("homeMenuBtn");
  const list = document.getElementById("homeMenuList");
  const close = () => {
    list.hidden = true;
    btn.setAttribute("aria-expanded", "false");
  };
  btn.addEventListener("click", (e) => {
    e.stopPropagation(); // don't let the document handler immediately re-close
    const open = list.hidden;
    list.hidden = !open;
    btn.setAttribute("aria-expanded", String(open));
  });
  list.addEventListener("click", (e) => {
    const item = e.target.closest(".menu-item");
    if (!item) return;
    if (item.dataset.action === "settings") openSettings();
    else if (item.dataset.action === "import") onImport();
    else if (item.dataset.action === "help") openHelp(showHome);
    close();
  });
  const homeVisible = () => !document.getElementById("homeView").hidden;
  document.addEventListener("click", () => homeVisible() && close());
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && homeVisible()) close();
  });
}

// A labelled on/off row that reads as a switch. Writes through `onChange` on each
// tap; `initial` seeds its visual state from the stored value.
function settingToggle(name, desc, initial, onChange) {
  const row = document.createElement("button");
  row.type = "button";
  row.className = "setting-row";
  row.setAttribute("role", "switch");
  row.setAttribute("aria-checked", String(initial));

  const text = document.createElement("div");
  text.className = "setting-text";
  const title = document.createElement("span");
  title.className = "setting-name";
  title.textContent = name;
  const sub = document.createElement("span");
  sub.className = "setting-desc";
  sub.textContent = desc;
  text.append(title, sub);

  const sw = document.createElement("span");
  sw.className = "setting-switch";
  sw.setAttribute("aria-hidden", "true");

  row.append(text, sw);
  row.addEventListener("click", () => {
    const on = row.getAttribute("aria-checked") !== "true";
    row.setAttribute("aria-checked", String(on));
    onChange(on);
  });
  return row;
}

// ---- How to play ----
// A static reference: the rules, a few "good to know" tips, then the play-view
// controls described for the current device (touch gestures on a coarse pointer,
// keyboard shortcuts otherwise). Reached from the home menu and the play menu.
// Lives here (a static, wasm-free module) so it opens instantly, like Settings.

// Where the help back button returns to. Set per-open (Home, or a target that
// resumes the board when opened from play) -- mirrors settingsReturn.
let helpReturn = showHome;

const HELP_RULES = [
  ["Goal", "Fill every empty cell with a digit from 1 to 9."],
  ["No repeats", "Each row, each column, and each 3×3 box must hold all nine digits exactly once."],
  ["One solution", "Every puzzle has a single solution, reachable by logic alone — you never have to guess."],
];

const HELP_TOUCH = [
  ["Select a cell", "Tap it. A single tap always selects just that one cell."],
  ["Select several cells", "Drag across the grid to paint a selection. Double-tap a cell to add it to the selection, or double-tap then drag to add a whole region. A digit, note, or erase then applies to every selected cell at once. Works only while no number is locked."],
  ["Clear the selection", "Tap the empty space around the board to drop the current selection."],
  ["Place a digit", "Select a cell, then tap a number. Tap the same number again to clear it."],
  ["Lock a digit", "Double-tap a number to lock it; every cell you then tap gets that digit. Tap it once more to unlock, or double-tap a different number to switch."],
  ["Pencil notes", "Tap Notes to switch between placing digits and pencilling notes."],
  ["Switch note style", "Double-tap Notes to swap between center notes and corner (Snyder) notes."],
  ["Quick opposite", "With a digit locked, double-tap a cell to do the opposite of your current mode — pencil a note while placing, or place while noting."],
  ["Paint notes", "With a digit locked, a note gesture can sweep: press and drag to pencil that note across every cell the pointer crosses, all as one move. In Notes mode that's a single press-and-drag; while placing digits it's a double-tap-and-drag (placing a value stays one cell). The first cell sets the direction — add or clear — so cells that already match are left untouched."],
  ["Erase", "Tap Erase, or re-tap a digit you placed."],
  ["Undo / Redo", "The Undo and Redo buttons step back and forth through your moves."],
  ["Hint", "Tap Hint to see if you've made a mistake, or which technique could apply next."],
];

const HELP_KEYBOARD = [
  ["Move the selection", "Arrow keys. A single click always selects just one cell."],
  ["Select several cells", "Click and drag to paint a selection, Shift+click to add a cell, or hold Shift with the arrow keys to extend it. A digit, note, or erase then applies to every selected cell at once. Works only while no digit is locked."],
  ["Place a digit", "Press 1–9. Press the same digit again to clear the cell."],
  ["Erase", "0, Backspace, or Delete."],
  ["Pencil notes", "Press n to switch between placing digits and pencilling notes."],
  ["Switch note style", "Press Shift+N to swap between center notes and corner (Snyder) notes."],
  ["Undo", "Ctrl/Cmd+Z."],
  ["Redo", "Ctrl/Cmd+Shift+Z, or Ctrl/Cmd+Y."],
  ["Hint", "The Hint button shows if you've made a mistake, or which technique could apply next."],
];

const HELP_NOTES = [
  ["Restart is undoable", "Your whole attempt rides on the undo stack, so a single Undo brings it all back."],
  ["Two note styles", "Center notes sit as a row in the middle of a cell; corner (Snyder) notes tuck into its corners."],
  ["Hints read your center notes", "Only your center notes feed the hint engine. If a cell's center notes leave out a digit that's still possible, its hints can be inaccurate — keep them complete, or clear them."],
  ["Auto-cleanup", "Placing a digit can strike it from the notes of every cell that sees it — turn this on with “Eliminate candidates” in Settings."],
  ["Play from Forced", "Starts you partway in, with clues filled up to the technique you're practising. Restart clears back to the original puzzle."],
  ["The timer pauses", "It stops while you're on another tab or in Settings, so your solve time stays honest."],
];

export function openHelp(onBack) {
  helpReturn = onBack || showHome;
  const touch = !!(window.matchMedia && window.matchMedia("(pointer: coarse)").matches);
  document
    .getElementById("helpBody")
    .replaceChildren(
      helpSection("Rules", HELP_RULES),
      helpSection("Good to know", HELP_NOTES),
      helpSection(touch ? "Controls" : "Keyboard", touch ? HELP_TOUCH : HELP_KEYBOARD)
    );
  showView("helpView");
}

// A titled definition list: each row pairs an action with how to do it.
function helpSection(title, rows) {
  const sec = document.createElement("section");
  sec.className = "help-section";
  const h = document.createElement("h2");
  h.textContent = title;
  const dl = document.createElement("dl");
  dl.className = "help-list";
  for (const [term, desc] of rows) {
    const dt = document.createElement("dt");
    dt.textContent = term;
    const dd = document.createElement("dd");
    dd.textContent = desc;
    dl.append(dt, dd);
  }
  sec.append(h, dl);
  return sec;
}

// ---- Shared bits ----
function navButton(label, subtitle, solvedCount, onClick) {
  const btn = document.createElement("button");
  btn.className = "nav-btn";
  btn.addEventListener("click", onClick);

  // Label over an optional muted subtitle (the technique/branch list); the badge
  // and the drill-in chevron sit to its right.
  const text = document.createElement("span");
  text.className = "nav-text";
  const name = document.createElement("span");
  name.className = "nav-label";
  name.textContent = label;
  text.appendChild(name);
  if (subtitle) {
    const sub = document.createElement("span");
    sub.className = "nav-sub";
    sub.textContent = subtitle;
    text.appendChild(sub);
  }
  btn.appendChild(text);
  if (solvedCount > 0) btn.appendChild(badge(`${solvedCount} solved`));
  return btn;
}

// Technique display names of a set of entries, joined for a subtitle line.
function techniqueNames(entries) {
  return entries.map((t) => techniqueName(t.id)).join(" · ");
}

function badge(text) {
  const b = document.createElement("span");
  b.className = "count-badge";
  b.textContent = text;
  return b;
}

function rollup(stats, entries) {
  let n = 0;
  for (const e of entries) n += store.solvedCountForKind(stats, e.kindIndex);
  return n;
}

function idFor(kindIndex) {
  return curriculum.find((t) => t.kindIndex === kindIndex)?.id || "";
}
