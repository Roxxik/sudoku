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
  const mode = g.mode === "drill" ? "Drill" : "Train";
  const meta = `${mode} · ${formatDuration(g.elapsedMs)} · ${solvedDate(g)}`;
  const col = textColumn(techniqueName(idFor(g.kindIndex)), meta);
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
  card.append(miniBoard(g, "mini-lg"), col, copyButton(g));
  return card;
}

// Export the puzzle's clue line (`g.puzzle` is already to_line() output, '.' =
// empty; never the solution) to the clipboard, with a brief inline confirmation.
function copyButton(g) {
  const btn = document.createElement("button");
  btn.type = "button";
  btn.className = "hist-copy";
  btn.textContent = "Copy";
  btn.title = "Copy the puzzle (clues only)";
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
