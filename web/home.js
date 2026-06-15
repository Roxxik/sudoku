"use strict";

// The start page and the campaign navigation.
//
// Start page (homeView), fixed order so entries hold their place as they
// appear/disappear:
//   1. Campaign  -- three tier buttons (Beginner / Intermediate / Expert),
//                   always shown. Tapping a tier drills in.
//   2. Continue last puzzle  -- shown when there is >=1 in-progress puzzle.
//   3. Continue a puzzle (list)  -- shown when there are >=2.
//
// Campaign drill-down (campaignView): tier -> branches (Expert only) ->
// techniques, each technique offering Train/Drill. Solved-count rollups appear
// at every level. The puzzle list (puzzlesView) lists all in-progress games.

import * as store from "./store.js";
import * as settings from "./settings.js";
import { miniBoard, textColumn, copyText } from "./ui.js";
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
    root.appendChild(navButton(TIER_LABEL[tier], rollup(stats, entries), () => openTier(tier)));
  }
}

function renderContinue() {
  const active = store.activeGames();
  const lastCard = document.getElementById("continueLast");
  const listBtn = document.getElementById("continueListBtn");

  lastCard.hidden = active.length < 1;
  listBtn.hidden = active.length < 2;

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
function openTier(tier) {
  const keys = Object.keys(grouped[tier]);
  // A multi-branch tier (Expert) gets the branch level; a single-branch tier
  // (Beginner/Intermediate -- Trunk only) goes straight to its techniques.
  if (keys.length > 1) openBranches(tier);
  else openTechniques(tier, keys[0]);
}

function openBranches(tier) {
  const stats = store.statsByKind();
  setCampaignTitle(TIER_LABEL[tier]);
  const grid = document.createElement("div");
  grid.className = "nav-grid";
  for (const branch of ["fish", "subset", "bivalue", "trunk"]) {
    const entries = grouped[tier][branch];
    if (!entries) continue;
    grid.appendChild(
      navButton(BRANCH_LABEL[branch], rollup(stats, entries), () => openTechniques(tier, branch))
    );
  }
  const note = document.createElement("div");
  note.className = "mode-explainer";
  note.appendChild(
    explPlain(
      "These branches are only roughly ordered by difficulty — it mainly climbs within each branch. Work through the easier techniques in a branch before going deeper into it."
    )
  );
  document.getElementById("campaignBody").replaceChildren(grid, note);
  campaignBack = showHome;
  showView("campaignView");
}

function openTechniques(tier, branch) {
  const stats = store.statsByKind();
  const multiBranch = Object.keys(grouped[tier]).length > 1;
  setCampaignTitle(
    multiBranch ? `${TIER_LABEL[tier]} · ${BRANCH_LABEL[branch]}` : TIER_LABEL[tier]
  );
  document
    .getElementById("campaignBody")
    .replaceChildren(techniqueList(grouped[tier][branch], stats), modeExplainer(tier, branch));
  // Back up to the branch list if we came through it, else to Home.
  campaignBack = multiBranch ? () => openBranches(tier) : showHome;
  showView("campaignView");
}

// A short, tier/branch-aware note at the bottom of a techniques page. A shared
// line states what both modes have in common (the chosen technique always
// appears; earlier tiers stay available), then Train vs Drill differ only in
// whether this page's peers are usable. Phrasing tracks CURRICULUM.md: an
// Intermediate page is a flat tier (peers = all the others here), an Expert page
// is an ordered branch (peers = the simpler ones above it). Beginner is
// train-only, so it just gets a one-line description.
function modeExplainer(tier, branch) {
  const box = document.createElement("div");
  box.className = "mode-explainer";

  if (tier === "beginner") {
    box.appendChild(
      explPlain(
        "Beginner puzzles need only hidden singles — the one spot in a row, column, or box where a digit can still go."
      )
    );
    return box;
  }

  // The same-page peers, named per the page's shape; and which earlier tier the
  // player is assumed to bring.
  const peers =
    tier === "intermediate"
      ? "the other techniques on this page"
      : `the simpler ${{ fish: "fishes", subset: "subsets", bivalue: "wings" }[branch]} above the one you picked`;
  const basics =
    tier === "intermediate"
      ? "hidden singles are"
      : "all the Beginner and Intermediate techniques are";

  box.appendChild(
    explPlain(`The technique you pick is guaranteed to appear, and ${basics} also needed throughout the puzzle.`)
  );
  box.appendChild(explMode("Train", `${peers} may also be needed.`));
  box.appendChild(
    explMode("Drill", `only the technique you pick is required; ${peers} won't be needed to finish the puzzle.`)
  );
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

function techniqueList(entries, stats) {
  const ul = document.createElement("ul");
  ul.className = "tech-list";
  for (const t of entries) ul.appendChild(techniqueRow(t, stats));
  return ul;
}

function techniqueRow(t, stats) {
  const li = document.createElement("li");
  li.className = "tech-row";

  const head = document.createElement("div");
  head.className = "tech-head";
  const name = document.createElement("span");
  name.className = "tech-name";
  name.textContent = techniqueName(t.id);
  head.appendChild(name);

  const solved = store.solvedCountForKind(stats, t.kindIndex);
  if (solved > 0) head.appendChild(badge(`${solved} solved`));
  li.appendChild(head);

  const modes = document.createElement("div");
  modes.className = "mode-buttons";
  modes.appendChild(modeButton(t, "train", stats));
  if (t.hasDrill) modes.appendChild(modeButton(t, "drill", stats));
  li.appendChild(modes);

  return li;
}

// A Train/Drill button. Shows the per-mode best time when one exists; with no
// solves it shows just the centered label (no placeholder dash).
function modeButton(t, mode, stats) {
  const btn = document.createElement("button");
  btn.className = "mode-btn";
  btn.addEventListener("click", () => onLaunch(t.kindIndex, mode));

  const label = document.createElement("span");
  label.className = "mb-label";
  label.textContent = mode === "drill" ? "Drill" : "Train";
  btn.appendChild(label);

  // Per-mode solved marker: Train and Drill are tracked separately, so the
  // solved count + best time live on each button (not just the technique row).
  // Always render the sub line (empty when unsolved) so the buttons keep the
  // same size -- no jump.
  const sub = document.createElement("span");
  sub.className = "mb-sub";
  const m = stats[t.kindIndex]?.[mode];
  if (m && m.count > 0) {
    btn.classList.add("solved");
    sub.textContent = `${m.count} solved · ${formatDuration(m.bestMs)}`;
  } else {
    sub.textContent = "";
  }
  btn.appendChild(sub);
  return btn;
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

// ---- Shared bits ----
function navButton(label, solvedCount, onClick) {
  const btn = document.createElement("button");
  btn.className = "nav-btn";
  btn.addEventListener("click", onClick);
  const text = document.createElement("span");
  text.className = "nav-label";
  text.textContent = label;
  btn.appendChild(text);
  if (solvedCount > 0) btn.appendChild(badge(`${solvedCount} solved`));
  return btn;
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
