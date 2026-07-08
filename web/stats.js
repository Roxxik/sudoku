"use strict";

// The Stats page: an aggregate of every solved puzzle, one row per
// technique/mode that has at least one solve, with count, best, and average
// time (same data as the tree badges, laid out in full), followed by a flat
// newest-first history of solved puzzles -- a thumbnail of each finished board,
// plus the generation seed when cheat mode is on (a debug aid).

import * as store from "./store.js";
import { formatDuration, hintLabel, techniqueName, TIER_ORDER, TIER_LABEL } from "./util.js";
import { miniBoard, textColumn, copyText, gradeBadge } from "./ui.js";
import {
  modeOf,
  isCustom,
  statsBuckets,
  campaignStats,
  reviewStats,
  dailyStats,
  countOf,
  mergeBuckets,
  avgOf,
} from "./modes.js";
import { REVIEW_LESSONS } from "./review.js";
import { DAILY_LEVELS, levelName } from "./daily.js";
import { cheatOn } from "./cheat.js";

// The Stats screen collapses each mode's plain Play and Play-from-Forced into one row
// (the aggregator keeps them apart for the campaign/Review buttons; the forced split
// is a home-page detail, not a stats-screen one). Each entry maps a row label to the
// fine keys it folds together.
const MERGED_STARTS = [
  ["Train", ["train", "trainForced"]],
  ["Drill", ["drill", "drillForced"]],
];

let curriculum = [];
let onOpenSpec = () => {};

export function initStats(opts) {
  curriculum = opts.curriculum;
  onOpenSpec = opts.onOpenSpec || (() => {});
  document.getElementById("statsBack").addEventListener("click", opts.onHome);
}

export function renderStats() {
  const body = document.getElementById("statsBody");
  const stats = statsBuckets(store.solvedGames());
  body.replaceChildren();

  // Headline totals across every solved puzzle -- custom/imported included (they're
  // solved too, even though they have no per-mode rows below).
  const solved = store.solvedGames();
  if (solved.length === 0) {
    const p = document.createElement("p");
    p.className = "hint-note";
    p.textContent = "No puzzles solved yet. Finish one to start tracking your times.";
    body.appendChild(p);
    return;
  }
  const totalMs = solved.reduce((sum, g) => sum + (g.elapsedMs || 0), 0);
  body.appendChild(summaryCard(solved.length, totalMs));

  // Campaign: one section per tier (curriculum order), rows per solved kind.
  for (const tier of TIER_ORDER) {
    const entries = curriculum
      .filter((t) => t.tier === tier && countOf(campaignStats(stats, t.kindIndex)) > 0)
      .sort((a, b) => a.difficulty - b.difficulty);
    if (entries.length === 0) continue;
    const rows = [];
    for (const t of entries) rows.push(...mergedRows(campaignStats(stats, t.kindIndex), techniqueName(t.id)));
    body.append(sectionHeading(TIER_LABEL[tier]), statsTable(rows));
  }

  // Review: rows per lesson (Train/Drill merged), if any solved.
  const reviewRows = [];
  for (const lesson of REVIEW_LESSONS) reviewRows.push(...mergedRows(reviewStats(stats, lesson.key), lesson.name));
  if (reviewRows.length) body.append(sectionHeading("Review"), statsTable(reviewRows));

  // Daily: a row per difficulty level, if any solved (no Train/Drill split there).
  const daily = dailyStats(stats);
  const dailyRows = [];
  for (let i = 0; i < DAILY_LEVELS.length; i++) {
    const m = daily[String(i)];
    if (m) dailyRows.push(dataRow(levelName(i), "Daily", m));
  }
  if (dailyRows.length) body.append(sectionHeading("Puzzle of the day"), statsTable(dailyRows));

  body.appendChild(historySection());
}

// The Train/Drill rows for one group object (a campaign kind or a Review lesson),
// each folding its plain Play + Play-from-Forced solves; a start with no solves is
// skipped.
function mergedRows(groupObj, name) {
  const rows = [];
  for (const [label, keys] of MERGED_STARTS) {
    const m = mergeBuckets(...keys.map((k) => groupObj[k]));
    if (m) rows.push(dataRow(name, label, m));
  }
  return rows;
}

function sectionHeading(text) {
  const h = document.createElement("h2");
  h.className = "stats-tier";
  h.textContent = text;
  return h;
}

function statsTable(rows) {
  const table = document.createElement("table");
  table.className = "stats-table";
  table.appendChild(headerRow());
  for (const r of rows) table.appendChild(r);
  return table;
}

// ---- Solved-puzzle history ----
// A flat list of every solved puzzle, most-recently-solved first: a thumbnail of
// the finished board with its technique, mode, solve time and date. Under cheat
// mode each card also shows the generator seed (for reproducing/debugging a
// specific puzzle); puzzles solved before seeds were recorded show none.
function historySection() {
  const wrap = document.createElement("div");
  const h = document.createElement("h2");
  h.className = "stats-tier";
  h.textContent = "Solved puzzles";
  wrap.appendChild(h);

  const list = document.createElement("div");
  list.className = "hist-list";
  for (const g of store.solvedGames()) list.appendChild(historyCard(g));
  wrap.appendChild(list);
  return wrap;
}

function historyCard(g) {
  const card = document.createElement("div");
  card.className = "hist-item";
  // Title + mode label come from the mode registry: a daily's difficulty + the
  // puzzle's date, a Review lesson + Train/Drill, a custom label, or a technique +
  // Train/Drill.
  const m = modeOf(g);
  const title = m.title(g);
  const modeLabel = m.statsLabel(g);
  const meta = `${modeLabel} · ${formatDuration(g.elapsedMs)} · ${hintLabel(g)} · ${solvedDate(g)}`;
  const col = textColumn(title, meta);
  const badge = gradeBadge(g.grade);
  if (badge) col.appendChild(badge);
  if (cheatOn()) {
    const seed = document.createElement("span");
    seed.className = "ci-meta hist-seed";
    seed.textContent = g.seed ? `Seed: ${g.seed}` : "Seed: (not recorded)";
    col.appendChild(seed);
    const attempts = document.createElement("span");
    attempts.className = "ci-meta hist-seed";
    attempts.textContent =
      g.attempts != null ? `Attempts: ${g.attempts}` : "Attempts: (not recorded)";
    col.appendChild(attempts);
  }
  card.append(miniBoard(g, "mini-lg"), col, cardActions(g));
  return card;
}

// The card's right-hand actions: every card can copy its puzzle; a custom card
// also offers "Open spec" to resurface its spec in the builder.
function cardActions(g) {
  // Only a custom game exposes the "Open spec" affordance; every other kind (review,
  // daily, campaign, imported) just gets the copy button.
  if (!isCustom(g)) return copyButton(g);
  const wrap = document.createElement("div");
  wrap.className = "hist-actions";
  wrap.append(openSpecButton(g), copyButton(g));
  return wrap;
}

// Reopen the custom-spec builder pre-loaded with this game's spec, so it can be
// regenerated or tweaked (the spec is otherwise only built fresh each time).
function openSpecButton(g) {
  const btn = document.createElement("button");
  btn.type = "button";
  btn.className = "hist-copy";
  btn.textContent = "Open spec";
  btn.title = "Open this custom spec in the builder";
  btn.addEventListener("click", () => onOpenSpec(g.spec));
  return btn;
}

// Export the puzzle's clue line (`g.puzzle` is already to_line() output, '.' =
// empty; never the solution) to the clipboard, with a brief inline confirmation.
function copyButton(g) {
  const btn = document.createElement("button");
  btn.type = "button";
  btn.className = "hist-copy";
  btn.textContent = "Export";
  btn.title = "Export the puzzle (clues only)";
  btn.addEventListener("click", () => {
    const label = btn.textContent;
    copyText(g.puzzle).then((ok) => {
      btn.textContent = ok ? "Copied" : "Failed";
      setTimeout(() => (btn.textContent = label), 1000);
    });
  });
  return btn;
}

function solvedDate(g) {
  return new Date(g.solvedAt || g.createdAt).toLocaleDateString();
}


function summaryCard(totalSolved, totalMs) {
  const card = document.createElement("div");
  card.className = "stats-summary";
  card.innerHTML = `
    <div><span class="ss-num">${totalSolved}</span><span class="ss-label">solved</span></div>
    <div><span class="ss-num">${formatDuration(totalMs)}</span><span class="ss-label">total time</span></div>
  `;
  return card;
}

function headerRow() {
  const tr = document.createElement("tr");
  for (const [label, cls] of [
    ["Technique", "col-tech"],
    ["Mode", "col-mode"],
    ["Solved", "col-num"],
    ["Best", "col-num"],
    ["Avg", "col-num"],
  ]) {
    const th = document.createElement("th");
    th.textContent = label;
    th.className = cls;
    tr.appendChild(th);
  }
  return tr;
}

function dataRow(name, modeLabel, m) {
  const tr = document.createElement("tr");
  const cells = [
    [name, "col-tech"],
    [modeLabel, "col-mode"],
    [String(m.count), "col-num"],
    [formatDuration(m.bestMs), "col-num"],
    [formatDuration(avgOf(m)), "col-num"],
  ];
  for (const [text, cls] of cells) {
    const td = document.createElement("td");
    td.textContent = text;
    td.className = cls;
    tr.appendChild(td);
  }
  return tr;
}
