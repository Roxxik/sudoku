"use strict";

// The Stats page: an aggregate of every solved puzzle, one row per
// technique/mode that has at least one solve, with count, best, and average
// time. Same data as the tree badges, laid out in full.

import * as store from "./store.js";
import { formatDuration, techniqueName, TIER_ORDER, TIER_LABEL } from "./util.js";

let curriculum = [];

export function initStats(opts) {
  curriculum = opts.curriculum;
  document.getElementById("statsBack").addEventListener("click", opts.onHome);
}

export function renderStats() {
  const body = document.getElementById("statsBody");
  const stats = store.statsByKind();
  body.replaceChildren();

  // Headline totals across everything solved.
  let totalSolved = 0;
  let totalMs = 0;
  for (const k of Object.values(stats)) {
    for (const m of Object.values(k)) {
      totalSolved += m.count;
      totalMs += m.avgMs * m.count;
    }
  }

  if (totalSolved === 0) {
    const p = document.createElement("p");
    p.className = "hint-note";
    p.textContent = "No puzzles solved yet. Finish one to start tracking your times.";
    body.appendChild(p);
    return;
  }

  body.appendChild(
    summaryCard(totalSolved, totalMs)
  );

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
      for (const mode of ["train", "drill"]) {
        const m = stats[t.kindIndex][mode];
        if (m) table.appendChild(dataRow(t, mode, m));
      }
    }
    body.appendChild(table);
  }
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

function dataRow(t, mode, m) {
  const tr = document.createElement("tr");
  const cells = [
    [techniqueName(t.id), "col-tech"],
    [mode === "drill" ? "Drill" : "Train", "col-mode"],
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
