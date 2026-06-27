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
import { REVIEW_LESSONS, lessonUsages, lessonHasDrill } from "./review.js";
import { techniqueName, TIER_ORDER, TIER_LABEL, logStorageError } from "./util.js";

let curriculum = [];
let showView = () => {};
let onGenerate = () => {};
let onHome = () => {};

// The spec under construction, one usage code per kind. Persisted (see
// loadUsages/saveUsages) so a half-built spec survives both a trip Home and a
// page reload -- the builder reopens exactly where it was left.
let usages = loadUsages();
// false: every Forced technique must be needed (a conjunction, the default).
// true: fold the Forced set into one `force_any` disjunction -- the puzzle needs
// *some* member, not all of them. Persisted alongside `usages`.
let forceAny = loadForceAny();
let chipEls = []; // kindIndex -> chip button, for in-place updates
let playBtn = null; // the two start buttons (Play / Play from first forced)
let forcedBtn = null;
let presetSel = null;
let forceModeEl = null; // the "Require any" switch row, for programmatic refresh

const USAGE_LABEL = ["Off", "Allow", "Force", "Concede"];
const USAGE_CLASS = ["off", "allow", "force", "concede"];

// Last-state persistence. The builder is otherwise stateless across reloads, so
// stash the usage array under one key; a malformed/old value falls back to a
// blank (all-Off) spec rather than bricking the page.
const USAGES_KEY = "sudoku.custom.usages";
const FORCE_ANY_KEY = "sudoku.custom.forceAny";

function loadUsages() {
  try {
    const raw = localStorage.getItem(USAGES_KEY);
    if (!raw) return new Array(NUM_KINDS).fill(OFF);
    const arr = JSON.parse(raw);
    if (
      Array.isArray(arr) &&
      arr.length === NUM_KINDS &&
      arr.every((u) => Number.isInteger(u) && u >= OFF && u <= 3)
    ) {
      return arr;
    }
  } catch (err) {
    // Corrupt/old data: start blank.
    logStorageError("read", USAGES_KEY, err);
  }
  return new Array(NUM_KINDS).fill(OFF);
}

function saveUsages() {
  try {
    localStorage.setItem(USAGES_KEY, JSON.stringify(usages));
  } catch (err) {
    // Quota or privacy mode: the spec just won't persist this session.
    logStorageError("write", USAGES_KEY, err);
  }
}

function loadForceAny() {
  try {
    return localStorage.getItem(FORCE_ANY_KEY) === "1";
  } catch (err) {
    logStorageError("read", FORCE_ANY_KEY, err);
    return false;
  }
}

function saveForceAny() {
  try {
    localStorage.setItem(FORCE_ANY_KEY, forceAny ? "1" : "0");
  } catch (err) {
    // Quota or privacy mode: the toggle just won't persist this session.
    logStorageError("write", FORCE_ANY_KEY, err);
  }
}

export function initCustom(opts) {
  curriculum = opts.curriculum;
  showView = opts.showView;
  onGenerate = opts.onGenerate;
  onHome = opts.onHome;
  document.getElementById("customBack").addEventListener("click", onHome);
}

// Open the builder. With no argument it paints from the current `usages` (kept
// between opens so a half-built spec survives a trip Home); pass a usage array
// (e.g. resurfaced from a solved custom game in Stats) to load that spec instead.
// Presets are enabled once the wasm bridge (which computes them) is up.
export function openCustom(initial) {
  if (Array.isArray(initial)) {
    usages = initial.slice();
    saveUsages(); // a resurfaced spec becomes the new remembered state
  }
  renderBody();
  showView("customView");
  ready().then(() => {
    if (presetSel) presetSel.disabled = false;
  });
}

function renderBody() {
  const body = document.getElementById("customBody");
  body.replaceChildren(
    presetRow(),
    explainer(),
    techniqueGroups(),
    forceModeRow(),
    generateButtons()
  );
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
  // The Review lessons as force_any templates: each loads the lesson's set as one
  // disjunction (Force + "Require any" on), scoped per mode. Drill is offered only
  // where the lesson has easier Expert techniques to isolate against (Lesson 1
  // doesn't, so it's Train-only), matching the campaign.
  const reviewGroup = document.createElement("optgroup");
  reviewGroup.label = "Expert · Review";
  REVIEW_LESSONS.forEach((lesson, i) => {
    reviewGroup.appendChild(option(`review:${i}:train`, `${lesson.name} — Train`));
    if (lessonHasDrill(curriculum, lesson)) {
      reviewGroup.appendChild(option(`review:${i}:drill`, `${lesson.name} — Drill`));
    }
  });
  sel.appendChild(reviewGroup);
  sel.addEventListener("change", () => applyPreset(sel.value));
  presetSel = sel;

  box.append(label, sel);
  return box;
}

function applyPreset(value) {
  if (!value) return;
  if (value === "blank") {
    usages = new Array(NUM_KINDS).fill(OFF);
    forceAny = false; // a blank spec forces nothing -- "all" mode is the honest state
  } else if (value.startsWith("review:")) {
    // A Review lesson template: the whole disjunction at once. forceAny on is what
    // makes the Forced set a `force_any` rather than a conjunction.
    const [, idxStr, mode] = value.split(":");
    const lesson = REVIEW_LESSONS[Number(idxStr)];
    if (!lesson) return;
    usages = lessonUsages(curriculum, lesson, mode);
    forceAny = true;
  } else {
    const [idxStr, mode] = value.split(":");
    const w = bindings();
    if (!w) return;
    let masks;
    try {
      masks = w.specMasks(Number(idxStr), mode === "drill");
    } catch {
      return; // bridge hiccup: leave the chips as they were
    }
    usages = usagesFromMasks(masks);
    forceAny = false; // a single-technique preset is a plain conjunction
  }
  saveUsages();
  saveForceAny();
  refreshAllChips();
  refreshForceMode();
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
  saveUsages();
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

// ---- All / Any forcing ----
// A switch sitting between the technique list and the start buttons. Off (the
// default) keeps the conjunctive meaning of Force -- every Forced technique must
// be needed. On folds the Forced set into one `force_any` disjunction, so the
// puzzle need only require *some* member (only meaningful with two or more
// Forced). The choice rides the request through to gen_worker.rs.
function forceModeRow() {
  const row = document.createElement("button");
  row.type = "button";
  row.className = "setting-row";
  row.setAttribute("role", "switch");
  row.setAttribute("aria-checked", String(forceAny));

  const text = document.createElement("span");
  text.className = "setting-text";
  const name = document.createElement("span");
  name.className = "setting-name";
  name.textContent = "Require any, not all";
  const desc = document.createElement("span");
  desc.className = "setting-desc";
  desc.textContent =
    "On, the puzzle needs just one of your forced techniques rather than every one of them.";
  text.append(name, desc);

  const sw = document.createElement("span");
  sw.className = "setting-switch";

  row.append(text, sw);
  row.addEventListener("click", () => {
    forceAny = !forceAny;
    row.setAttribute("aria-checked", String(forceAny));
    saveForceAny();
  });
  forceModeEl = row;
  return row;
}

// Reflect the current `forceAny` on the switch -- used when a preset sets it
// programmatically (the click handler updates it for direct taps).
function refreshForceMode() {
  if (forceModeEl) forceModeEl.setAttribute("aria-checked", String(forceAny));
}

// ---- Generate ----
// Two ways to start the assembled spec: Play (the minimal puzzle) and Play from
// first forced (a head start up to the easiest Forced technique). A spec can force
// several techniques, so this stops before the first one -- hence "first".
function generateButtons() {
  const wrap = document.createElement("div");
  wrap.className = "spec-actions";
  playBtn = actionButton("Play", () => generate(false));
  forcedBtn = actionButton("Play from first forced", () => generate(true));
  wrap.append(playBtn, forcedBtn);
  return wrap;
}

function actionButton(label, onClick) {
  const btn = document.createElement("button");
  btn.type = "button";
  btn.className = "spec-generate";
  btn.textContent = label;
  btn.addEventListener("click", onClick);
  return btn;
}

// A spec needs at least one Forced technique to be worth playing.
function refreshGenerate() {
  const ok = hasForce(usages);
  if (playBtn) playBtn.disabled = !ok;
  if (forcedBtn) forcedBtn.disabled = !ok;
}

function generate(fromForced) {
  if (!hasForce(usages)) return;
  const snapshot = usages.slice();
  const req = {
    usages: snapshot,
    label: labelFor(snapshot),
    specMasks: masksFromUsages(snapshot),
    forceAny,
  };
  // "Play from first forced" advances the board with the spec's ALLOWED techniques
  // until a Forced one is needed; app.js derives that allowed set from specMasks,
  // so the forced-start flag is all that's extra here.
  if (fromForced) req.fromForced = true;
  onGenerate(req);
}

// A short title from the Forced techniques. Conjunctive forcing joins with " + "
// ("Naked Pair + X-Wing"); the any-mode disjunction joins with " or " ("Naked
// Pair or X-Wing"), matching the relaxed requirement.
function labelFor(u) {
  const idxs = forcedIndices(u);
  if (!idxs.length) return "Custom";
  const names = idxs.map((i) => techniqueName(idFor(i)));
  return names.join(forceAny ? " or " : " + ");
}

// ---- Shared bits ----
function explainer() {
  const box = document.createElement("div");
  box.className = "mode-explainer spec-explainer";
  box.appendChild(plain("Tap a technique to cycle how it's used. Force at least one to play."));
  box.appendChild(modeLine("Allow", "may be used to solve the puzzle."));
  box.appendChild(modeLine("Force", "must be needed at least once."));
  box.appendChild(
    modeLine(
      "Concede",
      "kept available so a forced technique can't be sidestepped by it — but the puzzle will be solvable without using it."
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
