"use strict";

// App shell: boots the wasm bridge, loads the curriculum, wires the three views
// (Home / Play / Stats), and owns navigation plus the generation flow (loading
// overlay backed by the cancellable web worker).

// Only the modules needed to paint Home are imported statically; the rest are
// dynamically imported after first paint (see boot). curriculum is a static
// import so it ships in the module graph -- no runtime fetch.
import * as store from "./store.js";
import * as home from "./home.js";
import CURRICULUM from "./curriculum.js";

// Heavy/non-Home modules, bound once `heavyReady` resolves.
let wasm, gen, play, stats, custom;
let heavyReady;

// ---- View routing ----
const VIEWS = ["homeView", "campaignView", "customView", "puzzlesView", "playView", "statsView", "settingsView"];

function showView(id) {
  for (const v of VIEWS) {
    document.getElementById(v).hidden = v !== id;
  }
}

function goHome() {
  // home.renderHome() repaints the start page and shows homeView itself (it owns
  // navigation between the home/campaign/puzzles views).
  home.renderHome();
}

async function goStats() {
  await heavyReady;
  showView("statsView");
  stats.renderStats();
}

// The custom-spec builder lives in a heavy module (it talks to the wasm bridge
// for presets and the worker to generate), so opening it waits for heavyReady.
async function openCustomView() {
  await heavyReady;
  custom.openCustom();
}

// Open the custom-spec builder pre-loaded with a saved game's spec (the Continue
// card's "Open spec"). Like openCustomView, it needs the heavy custom module.
async function openSpecFromHome(spec) {
  await heavyReady;
  custom.openCustom(spec);
}

// Import a puzzle from a pasted clue line (the home menu's "Import puzzle"). The
// wasm bridge solves it for the solution + clue count -- so far this only has to
// re-accept what Export produces; richer parsing is a later concern. The result
// is stored as a custom game (no spec) and opened. Needs the wasm bridge + play,
// neither of which the first paint depends on, so it awaits heavyReady.
async function importPuzzle() {
  const raw = window.prompt("Paste a puzzle (81 cells, '.' for blanks):");
  if (raw == null) return; // cancelled
  const line = raw.trim();
  if (!line) return;
  await heavyReady;
  const w = await wasm.ready();
  let result;
  try {
    result = w.solveLine(line);
  } catch (e) {
    window.alert(e && e.message ? e.message : "Could not import this puzzle.");
    return;
  }
  const game = store.createGame({
    mode: "custom",
    label: "Imported",
    puzzle: result.puzzle,
    solution: result.solution,
    givens: result.givens,
  });
  play.loadGame(game);
  goPlay();
}

function goPlay() {
  showView("playView");
}

// Open Settings from the play view: freeze the solve clock, and route the
// settings back button to return to the board and resume where it left off.
function openSettingsFromPlay() {
  play.pause();
  home.openSettings(() => {
    showView("playView");
    play.resume();
  });
}

// ---- Generation overlay ----
// A modal spinner over generation. The worker runs off the main thread, so the
// page stays responsive and Cancel can terminate it mid-generation.
const overlay = {
  el: null,
  text: null,
  retryBtn: null,
  cancelBtn: null,
};

// The in-flight/last generation request, so the error screen's "Keep searching"
// can restart it uncapped. A campaign request is { kindIndex, mode }; a custom
// one is { usages, label, specMasks }.
let lastLaunch = null;

function showLoading(label) {
  overlay.el.classList.remove("error");
  overlay.text.textContent = label;
  overlay.retryBtn.hidden = true;
  overlay.cancelBtn.textContent = "Cancel";
  overlay.el.hidden = false;
}

function showLoadError(message, retriable) {
  overlay.el.classList.add("error");
  overlay.text.textContent = message;
  // "Keep searching" lifts the attempt budget, so it only helps when the search
  // merely ran out of attempts (retriable). Other failures -- a bad request, a
  // solver error -- won't be fixed by retrying, so they get only "Close".
  overlay.retryBtn.hidden = !(retriable && lastLaunch !== null);
  overlay.cancelBtn.textContent = "Close";
}

function hideLoading() {
  overlay.el.hidden = true;
}

// Generate a fresh puzzle for `req`, store it as a new game, and open it. `req`
// is either a campaign request ({ kindIndex, mode }) or a custom-spec one
// ({ usages, label, specMasks } from custom.js). Cancel terminates the worker and
// returns to where we were. `uncapped` (the error screen's "Keep searching")
// lifts the worker's attempt budget so a hard target keeps trying until it
// succeeds or is cancelled.
async function launch(req, uncapped = false) {
  await heavyReady; // need gen + play wired
  lastLaunch = req;
  showLoading(uncapped ? "Still searching…" : "Generating puzzle…");
  let result;
  try {
    const genReq = req.usages
      ? { usages: req.usages, uncapped }
      : { target: req.kindIndex, drill: req.mode === "drill", uncapped };
    result = await gen.generate(genReq);
  } catch (e) {
    if (e && e.name === "AbortError") {
      hideLoading(); // user cancelled
    } else {
      showLoadError(e && e.message ? e.message : "Generation failed.", !!(e && e.retriable));
    }
    return;
  }
  hideLoading();
  // The worker also returns debug metadata (decimal-string u64 seed + the attempt
  // count) for the cheat-mode display.
  const game = req.usages
    ? store.createGame({
        mode: "custom",
        spec: req.usages,
        specMasks: req.specMasks,
        label: req.label,
        puzzle: result.puzzle,
        solution: result.solution,
        givens: result.givens,
        seed: result.seed,
        attempts: result.attempts,
      })
    : store.createGame({
        kindIndex: req.kindIndex,
        mode: req.mode,
        puzzle: result.puzzle,
        solution: result.solution,
        givens: result.givens,
        seed: result.seed,
        attempts: result.attempts,
      });
  play.loadGame(game);
  goPlay();
}

// Re-generate a fresh puzzle for an existing game's spec (the play view's "New
// puzzle"). A custom game replays its stored usage array; a campaign game its
// (kindIndex, mode).
function regenerate(g) {
  launch(
    g.spec
      ? { usages: g.spec, label: g.label, specMasks: g.specMasks }
      : { kindIndex: g.kindIndex, mode: g.mode }
  );
}

async function resume(gameId) {
  await heavyReady; // need play wired
  const game = store.getGame(gameId);
  if (!game) {
    goHome();
    return;
  }
  play.loadGame(game);
  goPlay();
}

function wireOverlay() {
  overlay.el = document.getElementById("loadingOverlay");
  overlay.text = document.getElementById("loadingText");
  overlay.retryBtn = document.getElementById("loadingRetry");
  overlay.cancelBtn = document.getElementById("loadingCancel");
  overlay.retryBtn.addEventListener("click", () => {
    // Only reachable from the error state, where lastLaunch holds the failed
    // request; re-run it uncapped.
    if (lastLaunch) launch(lastLaunch, true);
  });
  overlay.cancelBtn.addEventListener("click", () => {
    if (gen && gen.isGenerating()) gen.cancel(); // rejects launch() with AbortError
    else hideLoading(); // error state: just dismiss
  });
}

// Keep the solve clock honest across tab switches: pause when hidden, resume
// when visible (only matters while the play view is up).
function wireVisibility() {
  document.addEventListener("visibilitychange", () => {
    if (!play || document.getElementById("playView").hidden) return;
    if (document.hidden) play.pause();
    else play.resume();
  });
}

// ---- Boot ----
// First paint of Home depends only on the statically-imported store + home +
// curriculum, so it renders as early as the browser can parse those. Everything
// heavier (the board, stats, the generation worker client, the wasm bridge) is
// dynamically imported afterwards; the navigation handlers await `heavyReady`.
function boot() {
  wireOverlay();
  wireVisibility();

  home.initHome({
    curriculum: CURRICULUM,
    showView,
    onLaunch: (kindIndex, mode) => launch({ kindIndex, mode }),
    onResume: resume,
    onStats: goStats,
    onCustom: openCustomView,
    onOpenSpec: openSpecFromHome,
    onImport: importPuzzle,
  });
  goHome(); // <-- first paint: real saved puzzles + campaign tree

  heavyReady = (async () => {
    const [p, s, g, w, c] = await Promise.all([
      import("./play.js"),
      import("./stats.js"),
      import("./gen.js"),
      import("./wasm.js"),
      import("./custom.js"),
    ]);
    play = p;
    stats = s;
    gen = g;
    wasm = w;
    custom = c;
    play.initPlay({
      curriculum: CURRICULUM,
      onHome: goHome,
      onNewPuzzle: regenerate,
      onSettings: openSettingsFromPlay,
    });
    stats.initStats({
      curriculum: CURRICULUM,
      onHome: goHome,
      onOpenSpec: (spec) => custom.openCustom(spec),
    });
    custom.initCustom({
      curriculum: CURRICULUM,
      showView,
      onGenerate: launch,
      onHome: goHome,
    });
    // Warm the wasm bridge so the first Hint is ready; Home and generation
    // (the worker) don't depend on it.
    wasm.ready();
  })();
}

boot();
