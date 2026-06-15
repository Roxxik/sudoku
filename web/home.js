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
import { miniBoard, textColumn } from "./ui.js";
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

let grouped = {}; // tier -> branch -> [entries], sorted by difficulty
let lastActiveId = null; // most-recent in-progress game, for "Continue last"
let campaignBack = showHome; // where campaignView's back button goes right now

export function initHome(opts) {
  curriculum = opts.curriculum;
  showView = opts.showView;
  onLaunch = opts.onLaunch;
  onResume = opts.onResume;
  grouped = groupCurriculum(curriculum);

  document.getElementById("statsBtn").addEventListener("click", opts.onStats);
  document.getElementById("customBtn").addEventListener("click", opts.onCustom);
  document.getElementById("campaignBack").addEventListener("click", () => campaignBack());
  document.getElementById("puzzlesBack").addEventListener("click", showHome);
  document
    .getElementById("continueLastBtn")
    .addEventListener("click", () => lastActiveId && onResume(lastActiveId));
  document.getElementById("continueListBtn").addEventListener("click", openPuzzles);
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
  // earns its button as soon as anything has been solved.
  document.getElementById("statsBtn").hidden = !hasAnySolve(stats);
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
  const lastBtn = document.getElementById("continueLastBtn");
  const listBtn = document.getElementById("continueListBtn");

  lastBtn.hidden = active.length < 1;
  listBtn.hidden = active.length < 2;

  if (active.length >= 1) {
    lastActiveId = active[0].id;
    lastBtn.replaceChildren(
      miniBoard(active[0], "mini-lg"),
      textColumn("Continue last puzzle", continueMeta(active[0]))
    );
  } else {
    lastActiveId = null;
  }
  if (active.length >= 2) {
    // No board preview here -- this is a link to the list, not a single puzzle.
    listBtn.replaceChildren(textColumn("Continue a puzzle", `${active.length} in progress`));
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
    const btn = document.createElement("button");
    btn.className = "continue-item";
    btn.addEventListener("click", () => onResume(g.id));
    btn.append(miniBoard(g, "mini-xl"), textColumn(gameTitle(g), continueMeta(g)));
    li.appendChild(btn);
    list.appendChild(li);
  }
  showView("puzzlesView");
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

function hasAnySolve(stats) {
  for (const k of Object.values(stats))
    for (const m of Object.values(k)) if (m.count >= 1) return true;
  return false;
}

function idFor(kindIndex) {
  return curriculum.find((t) => t.kindIndex === kindIndex)?.id || "";
}
