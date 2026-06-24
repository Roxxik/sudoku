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
// Opting out of data sharing drops the passive upload queue (clearOutboxes, see
// openSettings). The daily Submit button posts a daily time explicitly
// (recordDailySolve, opt-out-bypassing), reads its tri-state (dailySubmitState),
// and re-renders live as a submission lands (setDailyChangeListener).
import {
  clearOutboxes,
  recordDailySolve,
  dailySubmitState,
  setDailyChangeListener,
  fetchDailyCounts,
  fetchDailyBoard,
} from "./backend.js";
import { miniBoard, textColumn, copyText, gradeBadge } from "./ui.js";
import { TECHNIQUE_INFO } from "./techniques.js";
import { masksFromUsages } from "./spec.js";
import {
  REVIEW_TIER,
  REVIEW_LESSONS,
  lessonUsages,
  lessonHasDrill,
  lessonSubtitle,
  lessonLabel,
  joinNames,
  reviewIdentity,
  reviewTitle,
  reviewModeLabel,
} from "./review.js";
import {
  DAILY_LEVELS,
  dailyUsages,
  dailySeed,
  dailyLabel,
  dayNumber,
  dayLabel,
  levelName,
  leaderboardWindow,
} from "./daily.js";
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
let onLaunchSpec = () => {};
let onResume = () => {};
let onOpenSpec = () => {};
let onImport = () => {};

let grouped = {}; // tier -> branch -> [entries], sorted by difficulty
let campaignBack = showHome; // where campaignView's back button goes right now

export function initHome(opts) {
  curriculum = opts.curriculum;
  showView = opts.showView;
  onLaunch = opts.onLaunch;
  onLaunchSpec = opts.onLaunchSpec || (() => {});
  onResume = opts.onResume;
  onOpenSpec = opts.onOpenSpec || (() => {});
  onImport = opts.onImport || (() => {});
  grouped = groupCurriculum(curriculum);

  document.getElementById("statsBtn").addEventListener("click", opts.onStats);
  document.getElementById("customBtn").addEventListener("click", opts.onCustom);
  document.getElementById("settingsBack").addEventListener("click", () => settingsReturn());
  document.getElementById("helpBack").addEventListener("click", () => helpReturn());
  document.getElementById("privacyBack").addEventListener("click", () => privacyReturn());
  wireHomeMenu();
  document.getElementById("campaignBack").addEventListener("click", () => campaignBack());
  document.getElementById("dailyBtn").addEventListener("click", openDaily);
  document.getElementById("dailyBack").addEventListener("click", showHome);
  // Flip the daily action buttons live as a submission queues and then lands (the
  // backend resolves the POST asynchronously, and retries can land in the
  // background while the page is open). Only re-render while the daily page shows.
  setDailyChangeListener(() => {
    if (!document.getElementById("dailyView").hidden) renderDailyBody(dayNumber());
  });
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
  renderDailyEntry();
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
// lists its branch names (plus the Review section it carries); a single-branch
// tier (Beginner/Intermediate) lists its technique names.
function tierSubtitle(tier) {
  const branches = grouped[tier];
  if (Object.keys(branches).length > 1) {
    const labels = BRANCH_ORDER.filter((b) => branches[b]).map((b) => BRANCH_LABEL[b]);
    if (tier === REVIEW_TIER) labels.push("Review");
    return labels.join(" · ");
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
  const col = textColumn(title, continueMeta(g));
  const badge = gradeBadge(g.grade);
  if (badge) col.appendChild(badge);
  main.append(miniBoard(g, sizeClass), col);
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
  // Custom games can resurface their spec; campaign/imported/Review/daily games
  // can't (a daily is a fixed puzzle, not a user-editable spec).
  if (g.mode === "custom" && Array.isArray(g.spec) && !reviewOf(g) && !dailyOf(g)) {
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
  const d = dailyOf(g);
  // The "Continue last puzzle" hero uses a generic title, so the daily identity
  // (that it IS a daily, which difficulty, which date) has to live in the meta.
  if (d) return `Daily · ${levelName(d.level)} · ${dayLabel(d.day)} · ${time}`;
  const r = reviewOf(g);
  if (r) return `${reviewTitle(r)} · ${reviewModeLabel(r)} · ${time}`;
  if (g.mode === "custom") return `Custom · ${time}`;
  const mode = g.mode === "drill" ? "Drill" : "Train";
  return `${techniqueName(idFor(g.kindIndex))} · ${mode} · ${time}`;
}

// The title for a game card: a daily puzzle's difficulty, else a Review lesson's
// name, else a custom game's own label, else its technique name.
function gameTitle(g) {
  const d = dailyOf(g);
  if (d) return `Daily · ${levelName(d.level)}`;
  const r = reviewOf(g);
  if (r) return reviewTitle(r);
  return g.mode === "custom" ? g.label || "Custom" : techniqueName(idFor(g.kindIndex));
}

// A custom game that is actually a Review lesson: its { name, mode }, else null.
// A daily game is a force_any custom game too -- and Expert I's spec is identical
// to Review Lesson 1's -- so the daily tag wins first to keep the labels apart.
function reviewOf(g) {
  if (dailyOf(g)) return null;
  if (g.mode !== "custom" || !g.forceAny || !Array.isArray(g.spec)) return null;
  return reviewIdentity(curriculum, g.spec);
}

// A daily game's { day, level } tag, or null. The single source of truth for "is
// this the Puzzle of the day" across the labels and the card menu.
function dailyOf(g) {
  return g && g.daily && typeof g.daily.day === "number" ? g.daily : null;
}

// ---- Puzzle of the day ----
// The Puzzle of the day: four fixed difficulties, each a deterministic puzzle
// derived from today's date (daily.js). The home entry opens this page; each
// difficulty offers Play (unplayed) / Continue (in progress) / Submit (solved,
// post the time) / Submitted (uploaded), plus a stubbed leaderboard. The generated
// game is an ordinary force_any custom game tagged `daily`, so it also shows up in
// Continue and the Stats history.

// Refresh the home "Puzzle of the day" entry's subtitle: today's date plus a
// progress hint once any difficulty has been opened.
function renderDailyEntry() {
  const sub = document.getElementById("dailySub");
  if (!sub) return;
  const day = dayNumber();
  let solved = 0;
  let started = 0;
  for (let i = 0; i < DAILY_LEVELS.length; i++) {
    const g = store.dailyGame(day, i);
    if (!g) continue;
    if (g.status === "solved") solved += 1;
    else started += 1;
  }
  const parts = [dayLabel(day)];
  if (solved) parts.push(`${solved}/${DAILY_LEVELS.length} solved`);
  else if (started) parts.push("in progress");
  sub.textContent = parts.join(" · ");
}

// The daily page: a row per difficulty, each with its blurb, a Play/Continue/
// Submit/Submitted button keyed off today's stored game, and a stubbed leaderboard.
function openDaily() {
  const day = dayNumber();
  setDailyTitle(`Puzzle of the day · ${dayLabel(day)}`);
  renderDailyBody(day);
  showView("dailyView");
}

// (Re)build the per-difficulty cards. Split from openDaily so the change listener
// can refresh the buttons in place as a submission moves through its states.
function renderDailyBody(day) {
  const body = document.getElementById("dailyBody");
  body.replaceChildren(...DAILY_LEVELS.map((level, i) => dailyLevelCard(level, i, day)));
  // Cards render synchronously with a "loading" board; fill the leaderboards from
  // the backend after (fire-and-forget -- a fetch failure leaves the placeholders).
  fillDailyLeaderboards(day);
}

// One difficulty as a card: name, blurb, the state-dependent action button, and a
// leaderboard placeholder (stubbed -- no scores yet).
function dailyLevelCard(level, index, day) {
  const card = document.createElement("div");
  card.className = "daily-level";

  const name = document.createElement("div");
  name.className = "daily-level-name";
  name.textContent = level.name;
  card.appendChild(name);

  const blurb = document.createElement("p");
  blurb.className = "daily-level-blurb";
  blurb.textContent = level.blurb;
  card.appendChild(blurb);

  card.appendChild(dailyActionButton(level, index, day));
  card.appendChild(dailyLeaderboard(index));
  return card;
}

// The per-difficulty action, keyed off today's stored game and its submit state:
//   no game            -> "Play"     (active)   start today's puzzle
//   in progress        -> "Continue" (active)   resume it
//   solved, unsent     -> "Submit"   (active)   post the time (the deliberate act)
//   solved, queued     -> "Submit"   (disabled) in the outbox, not yet accepted
//   solved, accepted   -> "Submitted"(disabled) the backend has it
// The time sits in the sub line from the solve onward.
function dailyActionButton(level, index, day) {
  const btn = document.createElement("button");
  btn.className = "mode-btn daily-play";
  const lab = document.createElement("span");
  lab.className = "mb-label";
  const sub = document.createElement("span");
  sub.className = "mb-sub";

  const g = store.dailyGame(day, index);
  if (g && g.status === "solved") {
    sub.textContent = formatDuration(g.elapsedMs);
    const state = dailySubmitState(g.solve_id);
    if (state === "submitted") {
      btn.classList.add("solved");
      btn.disabled = true;
      lab.textContent = "Submitted";
    } else if (state === "pending") {
      // Queued but not yet accepted: a disabled Submit, retried in the background.
      btn.classList.add("solved");
      btn.disabled = true;
      lab.textContent = "Submit";
    } else {
      lab.textContent = "Submit";
      btn.addEventListener("click", () => submitDaily(g, day));
    }
  } else if (g) {
    lab.textContent = "Continue";
    sub.textContent = formatDuration(g.elapsedMs);
    btn.addEventListener("click", () => onResume(g.id));
  } else {
    lab.textContent = "Play";
    sub.textContent = "";
    btn.addEventListener("click", () => launchDaily(level, index));
  }
  btn.append(lab, sub);
  return btn;
}

// Post a solved daily's time to the backend. Tapping Submit IS the consent, so
// this bypasses the data-sharing opt-out (recordDailySolve gates on a configured
// backend alone). Re-render at once so the button drops to its disabled
// "Submit"/pending look; the change listener flips it to "Submitted" when the POST
// lands. `g` is the light index record -- it already carries every field below.
function submitDaily(g, day) {
  if (!g.daily || !g.solve_id) return;
  recordDailySolve({
    solve_id: g.solve_id,
    day: g.daily.day,
    level: g.daily.level,
    seed: g.seed || null,
    puzzle: g.puzzle,
    solution: g.solution,
    solve_ms: Math.round(g.elapsedMs),
  });
  renderDailyBody(day);
}

// The per-card leaderboard slot. Rendered synchronously with a "loading" state and
// tagged with its level; fillDailyLeaderboards then drops in the real content once
// the backend responds. Keyed by level so the async fill can find its box again.
function dailyLeaderboard(index) {
  const box = document.createElement("div");
  box.className = "daily-leaderboard";
  box.dataset.level = String(index);
  box.textContent = "Leaderboard…";
  return box;
}

// Populate each card's leaderboard from the backend. Two reveals, gated on whether
// the player has JOINED that level's board (tapped Submit -- pending or submitted):
//   not joined -> a teaser: the solver count only, no times (the times are the
//                 post-solve reward; see the design notes).
//   joined     -> the full windowed board (leaderboardWindow), the player's row
//                 highlighted. While the submission is still pending (not yet in the
//                 backend), the player's own time is merged in locally so they see
//                 their projected rank immediately.
// One counts fetch serves every card's teaser; joined levels additionally fetch
// their board. All failures degrade quietly (the placeholder text is replaced with
// a dash). Re-run on every renderDailyBody, so it refreshes as a submission lands.
async function fillDailyLeaderboards(day) {
  const counts = await fetchDailyCounts(day); // { level: n } | null (unavailable)
  const boxes = document.querySelectorAll("#dailyBody .daily-leaderboard");
  for (const box of boxes) {
    const index = Number(box.dataset.level);
    const g = store.dailyGame(day, index);
    const state = g && g.status === "solved" ? dailySubmitState(g.solve_id) : "none";
    const joined = state === "submitted" || state === "pending";
    if (joined) {
      const board = await fetchDailyBoard(day, index);
      if (board) { renderDailyBoard(box, board, g, state); continue; }
      // Board unreadable -> fall through to the teaser (no nudge: already joined).
    }
    if (counts === null) renderDailyUnavailable(box);
    else renderDailyTeaser(box, counts[index] || 0, g, joined);
  }
}

// The board can't be read (no backend / offline). Keep the placeholder look.
function renderDailyUnavailable(box) {
  box.classList.remove("has-board");
  box.replaceChildren();
  box.textContent = "Leaderboard unavailable";
}

// The pre-join teaser: solver count only (no times -- those are the post-solve
// reward), plus a nudge for a player who has solved but not yet submitted, since
// their time isn't on the board until they do. The nudge is suppressed once joined.
function renderDailyTeaser(box, count, g, joined) {
  box.classList.remove("has-board");
  box.replaceChildren();
  let text;
  if (count <= 0) text = "Be the first to solve";
  else text = count === 1 ? "1 solved today" : `${count} solved today`;
  if (!joined && g && g.status === "solved") text += " · Submit to see where you rank";
  box.textContent = text;
}

// The post-join board: the player's rank in the day's field, then the windowed
// list of times with the player's row highlighted. `board` is { count, times }
// (times ascending). When the submission is only `pending` the player isn't in the
// fetched times yet, so their own time is spliced in at its sorted position.
function renderDailyBoard(box, board, g, state) {
  const mine = Math.round(g.elapsedMs);
  const times = board.times.slice();
  if (state !== "submitted") {
    // Pending: not in the backend list yet -- show the projected standing.
    let at = times.findIndex((t) => t > mine);
    if (at < 0) at = times.length;
    times.splice(at, 0, mine);
  }
  const total = times.length;

  // First (and so far only) solver: a "#1 of 1" one-row board says nothing, so
  // mark the milestone instead -- the mirror of the "Be the first to solve" teaser.
  if (total <= 1) {
    box.classList.remove("has-board");
    box.classList.add("first");
    box.replaceChildren();
    box.textContent = "You're the first to solve!";
    return;
  }

  // Competition rank: one past everyone strictly faster (ties share the top slot).
  let strictlyFaster = 0;
  while (strictlyFaster < total && times[strictlyFaster] < mine) strictlyFaster++;
  const rank = strictlyFaster + 1;

  box.classList.add("has-board");
  box.replaceChildren();

  const head = document.createElement("div");
  head.className = "lb-head";
  head.textContent = `#${rank} of ${total}`;
  box.appendChild(head);

  for (const row of leaderboardWindow(rank, total)) {
    const line = document.createElement("div");
    if (row === "gap") {
      line.className = "lb-gap";
      line.textContent = "···";
    } else {
      line.className = "lb-row";
      if (row === rank) line.classList.add("lb-you");
      const r = document.createElement("span");
      r.className = "lb-rank";
      r.textContent = `${row}.`;
      const t = document.createElement("span");
      t.className = "lb-time";
      const ms = times[row - 1];
      t.textContent = ms == null ? "—" : formatDuration(ms);
      line.append(r, t);
    }
    box.appendChild(line);
  }
}

// Generate and open today's puzzle for a difficulty. Built as a force_any custom
// spec (the Review-lesson path) but pinned to a per-(day, difficulty) seed so every
// device gets the same puzzle, and tagged `daily` so it's recognised later.
function launchDaily(level, index) {
  const day = dayNumber();
  const usages = dailyUsages(curriculum, level);
  onLaunchSpec({
    usages,
    label: dailyLabel(level),
    specMasks: masksFromUsages(usages),
    forceAny: true,
    seed: dailySeed(day, index),
    daily: { day, level: index },
    fromForced: false,
  });
}

function setDailyTitle(text) {
  document.getElementById("dailyTitle").textContent = text;
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
  // The Review section sits below the branches: mixed-technique practice once the
  // individual techniques are known. Not a curriculum branch, so it's appended
  // here rather than grouped (see REVIEW_LESSONS).
  if (tier === REVIEW_TIER) {
    grid.appendChild(
      navButton(
        "Review",
        "Mixed practice — a puzzle that needs any one of a set",
        0,
        () => openReview(tier, () => openBranches(tier))
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

// ---- Review lessons ----
// The list of Review lessons under a tier. Each row is a nav-button to a lesson
// page, subtitled with its techniques. `back` is the branch list that opened it.
function openReview(tier, back) {
  setCampaignTitle(`${TIER_LABEL[tier]} · Review`);
  const grid = document.createElement("div");
  grid.className = "nav-grid";
  for (const lesson of REVIEW_LESSONS) {
    grid.appendChild(
      navButton(lesson.name, lessonSubtitle(lesson), 0, () =>
        openReviewLesson(tier, lesson, () => openReview(tier, back))
      )
    );
  }
  const note = document.createElement("div");
  note.className = "mode-explainer";
  note.appendChild(
    explPlain(
      "Each lesson builds a puzzle that needs any one of its techniques, drawn evenly across the set — a mixed refresher once you know them on their own."
    )
  );
  document.getElementById("campaignBody").replaceChildren(grid, note);
  campaignBack = back;
  showView("campaignView");
}

// A Review lesson page: a description over a Train row (always) and a Drill row
// (when the lesson has easier Expert techniques to isolate against — every lesson
// past the first). Each row offers Play and Play from Forced, as a technique page
// does. `back` returns to the lesson list.
function openReviewLesson(tier, lesson, back) {
  setCampaignTitle(lesson.name);

  const page = document.createElement("div");
  page.className = "tech-page";

  const desc = document.createElement("p");
  desc.className = "tech-desc";
  desc.textContent =
    `A puzzle that needs any one of ${joinNames(lesson)}. Each new puzzle requires just ` +
    "one of them, drawn evenly across the set, so the techniques come up in turn.";
  page.appendChild(desc);

  // Drill isolates the lesson's technique against the easier Expert ones; with
  // nothing easier in scope (Lesson 1) there is nothing to isolate, so it's
  // Train-only — mirroring a technique page's `hasDrill`.
  const hasDrill = lessonHasDrill(curriculum, lesson);
  page.appendChild(reviewModeRow(lesson, "train"));
  if (hasDrill) page.appendChild(reviewModeRow(lesson, "drill"));

  page.appendChild(reviewModeNote(hasDrill));

  document.getElementById("campaignBody").replaceChildren(page);
  campaignBack = back;
  showView("campaignView");
}

// One Review mode (Train or Drill) as a row: its label over the start buttons.
function reviewModeRow(lesson, mode) {
  const row = document.createElement("div");
  row.className = "mode-row";

  const head = document.createElement("div");
  head.className = "mode-row-head";
  head.textContent = mode === "drill" ? "Drill" : "Train";
  row.appendChild(head);

  const btns = document.createElement("div");
  btns.className = "mode-buttons";
  btns.appendChild(reviewButton(lesson, mode, "Play", false));
  btns.appendChild(reviewButton(lesson, mode, "Play from Forced", true));
  row.appendChild(btns);
  return row;
}

// A short note on what Train and Drill mean for a lesson, mirroring a technique
// page's. With no Drill (Lesson 1) it explains why the split is absent.
function reviewModeNote(hasDrill) {
  const box = document.createElement("div");
  box.className = "mode-explainer";
  if (hasDrill) {
    box.appendChild(
      explMode("Train", "the easier review techniques, plus the basics, may also be needed to finish the puzzle.")
    );
    box.appendChild(
      explMode("Drill", "only one of this lesson's techniques is required; the easier review techniques won't be needed to finish it.")
    );
  } else {
    box.appendChild(
      explPlain("Only the basics are needed besides one of this lesson's techniques, so there is no separate drill.")
    );
  }
  return box;
}

// A Review start button. Unlike a technique's play button it carries no solved
// badge: a lesson generates a `force_any` (custom) game, which isn't tracked per
// kind. The empty sub keeps its height in line with the technique buttons.
function reviewButton(lesson, mode, label, fromForced) {
  const btn = document.createElement("button");
  btn.className = "mode-btn";
  btn.addEventListener("click", () => launchLesson(lesson, mode, fromForced));

  const lab = document.createElement("span");
  lab.className = "mb-label";
  lab.textContent = label;
  btn.appendChild(lab);

  const sub = document.createElement("span");
  sub.className = "mb-sub";
  btn.appendChild(sub);
  return btn;
}

// Launch a Review lesson through the custom-spec (`force_any`) path: allow the
// basics, force the lesson's set as one disjunction, scope the easier Expert
// techniques per `mode`, and hand it to the same generation flow the custom
// builder uses. The generator pins one set member per puzzle. `fromForced` seeds
// the board up to that technique (allowed = baseline minus the forced set), as a
// custom forced-start does.
function launchLesson(lesson, mode, fromForced) {
  const usages = lessonUsages(curriculum, lesson, mode);
  onLaunchSpec({
    usages,
    label: lessonLabel(lesson),
    specMasks: masksFromUsages(usages),
    forceAny: true,
    fromForced,
  });
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
        "Light mode",
        "Use the light colour theme. Turn off for the dark theme.",
        settings.lightModeOn(),
        (on) => {
          settings.setLightMode(on);
          // Re-theme live. The DOM mapping lives in index.html's inline
          // bootstrap so it runs before first paint too; reuse it here.
          if (window.__setTheme) window.__setTheme(on);
        }
      ),
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
      ),
      settingSelect(
        "Highlight matching digit",
        "When a digit is selected, light up where it already appears.",
        [
          ["spots", "Cells, marks, and open spots"],
          ["all", "Cells and marks"],
          ["placed", "Cells only"],
          ["none", "None"],
        ],
        settings.highlightMode(),
        settings.setHighlightMode
      ),
      settingToggle(
        "Disable finished digits",
        "Grey out a digit's button once all nine of it are placed.",
        settings.disableFinishedDigitsOn(),
        settings.setDisableFinishedDigits
      ),
      settingToggle(
        "Hints use your notes",
        "Read hints from your Center and Corner notes. Off: hints derive from placed digits alone.",
        settings.hintFromMarksOn(),
        settings.setHintFromMarks
      ),
      settingToggle(
        "Share play data",
        "Help improve the puzzles by sending your solve times and the moves you make while solving. Off by default; nothing leaves your device while it's off. No account and no personal data — see Privacy in the menu.",
        settings.dataSharingOn(),
        (on) => {
          settings.setDataSharing(on);
          // Opting out leaves nothing pending: drop anything already queued for upload.
          if (!on) clearOutboxes();
        }
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
    else if (item.dataset.action === "privacy") openPrivacy(showHome);
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

// A labelled row carrying a native <select> on the right, for a setting with
// more than two choices. `options` is a list of [value, label] pairs; `initial`
// is the stored value and `onChange` is called with the chosen value on each
// pick. A <div> (not the <button> settingToggle uses) so the select owns its own
// clicks rather than fighting a row-level toggle handler.
function settingSelect(name, desc, options, initial, onChange) {
  const row = document.createElement("div");
  row.className = "setting-row";

  const text = document.createElement("div");
  text.className = "setting-text";
  const title = document.createElement("span");
  title.className = "setting-name";
  title.textContent = name;
  const sub = document.createElement("span");
  sub.className = "setting-desc";
  sub.textContent = desc;
  text.append(title, sub);

  const sel = document.createElement("select");
  sel.className = "setting-select";
  for (const [value, label] of options) {
    const opt = document.createElement("option");
    opt.value = value;
    opt.textContent = label;
    if (value === initial) opt.selected = true;
    sel.appendChild(opt);
  }
  sel.addEventListener("change", () => onChange(sel.value));

  row.append(text, sel);
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
  ["Convert corner notes", "Flick the Notes button straight up to turn the selection's corner notes into center notes, merged on top of any center notes already there."],
  ["Flick to a note style", "Flick the Notes button left for corner (Snyder) notes, or right for center notes, to set which note style is active."],
  ["Quick opposite", "With a digit locked, double-tap a cell to do the opposite of your current mode — pencil a note while placing, or place while noting."],
  ["Paint notes", "With a digit locked, a note gesture can sweep: press and drag to pencil that note across every cell the pointer crosses, all as one move. In Notes mode that's a single press-and-drag; while placing digits it's a double-tap-and-drag (placing a value stays one cell). The first cell sets the direction — add or clear — so cells that already match are left untouched."],
  ["Erase", "Tap Erase to clear the value and all notes from the selection, or re-tap a digit you placed. Flick Erase left to clear only corner notes, right to clear only center notes, or straight up to undo the selected cells — restoring the value and notes they had before your last change."],
  ["Undo / Redo", "The Undo and Redo buttons step back and forth through your moves. Flick Redo straight up to redo every step at once."],
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
  ["Two note styles", "Center notes sit as a row in the middle of a cell: jot every digit the cell could still take — its full candidate list. Corner (Snyder) notes tuck into the corners: within a box, pencil a digit in each cell that could still hold it, so when only two or three spots are left the pairs and pointing lines stand out. Most solving runs on center notes; corner notes are a focused tool for tracking one digit across a box."],
  ["Hints read your notes", "Both your center and corner notes feed the hint engine — center notes as a cell's candidate list, corner notes as Snyder pins marking where a digit can still go in a box. It reasons from exactly those notes, so if a candidate is missing, or you use corner notes another way, a hint may not reflect the current state of play — it can point to a technique your real board has already passed. It never shows a wrong deduction, though: if your notes would rule out a cell's real answer, the engine ignores them there and reads candidates from the grid instead. Keep your notes complete and Snyder-style for hints that match where you actually are."],
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

// Where the privacy back button returns to -- mirrors helpReturn.
let privacyReturn = showHome;

// The privacy disclaimer, rendered with the same titled-list helper as Help.
// Keep this in step with what backend.js actually uploads (recordSolve /
// recordMoves) and with the "Share play data" setting (settings.js): the page is
// the user-facing promise that gating backs up. Plain-language for now -- a small,
// friends-only app -- to be reworded when it grows.
const PRIVACY = [
  ["Off by default", "Nothing about your play leaves this device unless you turn on “Share play data” in Settings. It is a pure opt-in: off until you choose otherwise, and turning it back off drops anything still waiting to be sent."],
  ["What is shared", "Only when you opt in, and only your solve times and the actions you take while solving — the same move history that already powers your hints and stats. Nothing more."],
  ["What is never collected", "No IP address, no name or email, no account, and not even an anonymous per-user id. What is sent cannot be tied back to you or your device."],
  ["Why it is collected", "Purely to make the puzzles and the difficulty grading better. The data is used only to improve this app — it is not sold or shared with anyone else."],
  ["Data minimality", "Only the bare minimum needed for that. If a piece of information is not needed to improve the app, it is not collected in the first place."],
  ["A small project", "This is a little app shared among friends."],
];

export function openPrivacy(onBack) {
  privacyReturn = onBack || showHome;
  document
    .getElementById("privacyBody")
    .replaceChildren(helpSection("Privacy", PRIVACY));
  showView("privacyView");
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
