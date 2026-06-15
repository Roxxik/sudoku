"use strict";

// The Stats page: an aggregate of every solved puzzle, one row per
// technique/mode that has at least one solve, with count, best, and average
// time (same data as the tree badges, laid out in full), followed by a flat
// newest-first history of solved puzzles -- a thumbnail of each finished board,
// plus the generation seed when cheat mode is on (a debug aid).

import * as store from "./store.js";
import { formatDuration, techniqueName, TIER_ORDER, TIER_LABEL } from "./util.js";
import { miniBoard, textColumn, copyText } from "./ui.js";
import { cheatOn } from "./cheat.js";

// Per-kind stat buckets, in display order: each mode split into a plain Play and a
// Play-from-Forced (the head start is tracked separately, matching statsByKind).
const STAT_STARTS = [
  ["train", "Train"],
  ["trainForced", "Train · forced"],
  ["drill", "Drill"],
  ["drillForced", "Drill · forced"],
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
  const stats = store.statsByKind();
  body.replaceChildren();

  // Headline totals across every solved puzzle -- custom games included (they're
  // solved too, even though they have no row in the per-kind tables below).
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

  // One section per tier, in curriculum order; rows only for solved kinds.
  for (const tier of TIER_ORDER) {
    const entries = curriculum
      .filter((t) => t.tier === tier && stats[t.kindIndex])
      .sort((a, b) => a.difficulty - b.difficulty);
    if (entries.length === 0) continue;

    const h = document.createElement("h2");
    h.className = "stats-tier";
    h.textContent = TIER_LABEL[tier];
    body.appendChild(h);

    const table = document.createElement("table");
    table.className = "stats-table";
    table.appendChild(headerRow());
    for (const t of entries) {
      for (const [key, label] of STAT_STARTS) {
        const m = stats[t.kindIndex][key];
        if (m) table.appendChild(dataRow(t, label, m));
      }
    }
    body.appendChild(table);
  }

  body.appendChild(historySection());
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
  // A custom game isn't a single curriculum kind: title it by its spec label and
  // mark the mode "Custom" (campaign games show their technique + Train/Drill).
  const isCustom = g.mode === "custom";
  const title = isCustom ? g.label || "Custom" : techniqueName(idFor(g.kindIndex));
  const modeLabel = isCustom ? "Custom" : g.mode === "drill" ? "Drill" : "Train";
  const meta = `${modeLabel} · ${formatDuration(g.elapsedMs)} · ${solvedDate(g)}`;
  const col = textColumn(title, meta);
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
  if (g.mode !== "custom" || !Array.isArray(g.spec)) return copyButton(g);
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

function idFor(kindIndex) {
  return curriculum.find((t) => t.kindIndex === kindIndex)?.id || "";
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

function dataRow(t, modeLabel, m) {
  const tr = document.createElement("tr");
  const cells = [
    [techniqueName(t.id), "col-tech"],
    [modeLabel, "col-mode"],
    [String(m.count), "col-num"],
    [formatDuration(m.bestMs), "col-num"],
    [formatDuration(m.avgMs), "col-num"],
  ];
  for (const [text, cls] of cells) {
    const td = document.createElement("td");
    td.textContent = text;
    td.className = cls;
    tr.appendChild(td);
  }
  return tr;
}
