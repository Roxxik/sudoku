"use strict";

// The custom-spec builder view.
//
// Reached from Home (below Campaign, above the in-progress puzzles). Where the
// campaign offers fixed Train/Drill presets for one technique, this exposes the
// full Spec API: tag each technique Off / Allow / Force / Concede, optionally
// starting from a campaign technique's train/drill preset, then generate a puzzle
// for the assembled spec. The usage array round-trips through spec.js -- to the
// generation worker (which rebuilds an explicit Spec) and to the play view's hint
// tree (via the stored masks).

import { bindings, ready } from "./wasm.js";
import {
  OFF,
  NUM_KINDS,
  usagesFromMasks,
  masksFromUsages,
  forcedIndices,
  hasForce,
} from "./spec.js";
import { techniqueName, TIER_ORDER, TIER_LABEL } from "./util.js";

let curriculum = [];
let showView = () => {};
let onGenerate = () => {};
let onHome = () => {};

// The spec under construction, one usage code per kind. Kept across opens so a
// half-built spec survives a trip Home.
let usages = new Array(NUM_KINDS).fill(OFF);
let chipEls = []; // kindIndex -> chip button, for in-place updates
let genBtn = null;
let presetSel = null;

const USAGE_LABEL = ["Off", "Allow", "Force", "Concede"];
const USAGE_CLASS = ["off", "allow", "force", "concede"];

export function initCustom(opts) {
  curriculum = opts.curriculum;
  showView = opts.showView;
  onGenerate = opts.onGenerate;
  onHome = opts.onHome;
  document.getElementById("customBack").addEventListener("click", onHome);
}

// Open the builder: paint it from the current `usages`, then enable presets once
// the wasm bridge (which computes them) is up.
export function openCustom() {
  renderBody();
  showView("customView");
  ready().then(() => {
    if (presetSel) presetSel.disabled = false;
  });
}

function renderBody() {
  const body = document.getElementById("customBody");
  body.replaceChildren(presetRow(), explainer(), techniqueGroups(), generateButton());
  refreshAllChips();
  refreshGenerate();
}

// ---- Preset selector ----
// Seeds the chips from a campaign technique's isolated train/drill spec, so the
// player can start from a known-good spec and tweak it.
function presetRow() {
  const box = document.createElement("div");
  box.className = "spec-preset";

  const label = document.createElement("label");
  label.htmlFor = "customPreset";
  label.textContent = "Start from a campaign spec";

  const sel = document.createElement("select");
  sel.id = "customPreset";
  sel.disabled = true; // enabled once the wasm bridge has loaded (presets call it)
  sel.appendChild(option("", "Choose a preset…"));
  sel.appendChild(option("blank", "Blank (all off)"));
  for (const tier of TIER_ORDER) {
    const entries = tierEntries(tier);
    if (!entries.length) continue;
    const og = document.createElement("optgroup");
    og.label = TIER_LABEL[tier];
    for (const t of entries) {
      og.appendChild(option(`${t.kindIndex}:train`, `${techniqueName(t.id)} — Train`));
      if (t.hasDrill) og.appendChild(option(`${t.kindIndex}:drill`, `${techniqueName(t.id)} — Drill`));
    }
    sel.appendChild(og);
  }
  sel.addEventListener("change", () => applyPreset(sel.value));
  presetSel = sel;

  box.append(label, sel);
  return box;
}

function applyPreset(value) {
  if (!value) return;
  if (value === "blank") {
    usages = new Array(NUM_KINDS).fill(OFF);
  } else {
    const [idxStr, mode] = value.split(":");
    const w = bindings();
    if (!w) return;
    let masks;
    try {
      masks = w.specMasksIsolated(Number(idxStr), mode === "drill");
    } catch {
      return; // bridge hiccup: leave the chips as they were
    }
    usages = usagesFromMasks(masks);
  }
  refreshAllChips();
  refreshGenerate();
}

// After a manual chip edit the select no longer describes the spec, so drop it
// back to the placeholder rather than claim a preset that's been changed.
function markPresetEdited() {
  if (presetSel) presetSel.value = "";
}

// ---- Technique list ----
function techniqueGroups() {
  chipEls = new Array(NUM_KINDS).fill(null);
  const wrap = document.createElement("div");
  wrap.className = "spec-groups";
  for (const tier of TIER_ORDER) {
    const entries = tierEntries(tier);
    if (!entries.length) continue;
    const h = document.createElement("h2");
    h.className = "spec-group-head";
    h.textContent = TIER_LABEL[tier];
    wrap.appendChild(h);
    const ul = document.createElement("ul");
    ul.className = "spec-list";
    for (const t of entries) ul.appendChild(techniqueRow(t));
    wrap.appendChild(ul);
  }
  return wrap;
}

function techniqueRow(t) {
  const li = document.createElement("li");
  li.className = "spec-row";

  const name = document.createElement("span");
  name.className = "spec-name";
  name.textContent = techniqueName(t.id);

  const chip = document.createElement("button");
  chip.type = "button";
  chip.className = "spec-chip";
  chip.addEventListener("click", () => cycle(t.kindIndex));
  chipEls[t.kindIndex] = chip;

  li.append(name, chip);
  return li;
}

// Tap cycles Off -> Allow -> Force -> Concede -> Off.
function cycle(i) {
  usages[i] = (usages[i] + 1) % 4;
  refreshChip(i);
  refreshGenerate();
  markPresetEdited();
}

function refreshChip(i) {
  const chip = chipEls[i];
  if (!chip) return;
  const u = usages[i];
  chip.textContent = USAGE_LABEL[u];
  chip.className = `spec-chip ${USAGE_CLASS[u]}`;
}

function refreshAllChips() {
  for (let i = 0; i < chipEls.length; i++) refreshChip(i);
}

// ---- Generate ----
function generateButton() {
  const btn = document.createElement("button");
  btn.type = "button";
  btn.id = "customGenerate";
  btn.className = "spec-generate";
  btn.textContent = "Generate";
  btn.addEventListener("click", generate);
  genBtn = btn;
  return btn;
}

// A spec needs at least one Forced technique to be worth generating.
function refreshGenerate() {
  if (genBtn) genBtn.disabled = !hasForce(usages);
}

function generate() {
  if (!hasForce(usages)) return;
  const snapshot = usages.slice();
  onGenerate({
    usages: snapshot,
    label: labelFor(snapshot),
    specMasks: masksFromUsages(snapshot),
  });
}

// A short title from the Forced techniques, e.g. "X-Wing" or "Naked Pair + X-Wing".
function labelFor(u) {
  const idxs = forcedIndices(u);
  if (!idxs.length) return "Custom";
  return idxs.map((i) => techniqueName(idFor(i))).join(" + ");
}

// ---- Shared bits ----
function explainer() {
  const box = document.createElement("div");
  box.className = "mode-explainer spec-explainer";
  box.appendChild(plain("Tap a technique to cycle how it's used. Force at least one to generate."));
  box.appendChild(modeLine("Allow", "may be used to solve the puzzle."));
  box.appendChild(modeLine("Force", "must be needed at least once."));
  box.appendChild(
    modeLine(
      "Concede",
      "kept available so a forced technique can't be sidestepped by it — but the puzzle isn't promised solvable with it."
    )
  );
  return box;
}

function plain(text) {
  const p = document.createElement("p");
  p.textContent = text;
  return p;
}

function modeLine(label, rest) {
  const p = document.createElement("p");
  const strong = document.createElement("strong");
  strong.textContent = label;
  p.append(strong, ` — ${rest}`);
  return p;
}

function option(value, text) {
  const o = document.createElement("option");
  o.value = value;
  o.textContent = text;
  return o;
}

function tierEntries(tier) {
  return curriculum
    .filter((t) => t.tier === tier)
    .sort((a, b) => a.difficulty - b.difficulty);
}

function idFor(kindIndex) {
  return curriculum.find((t) => t.kindIndex === kindIndex)?.id || "";
}
