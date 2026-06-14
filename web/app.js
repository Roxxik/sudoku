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
let wasm, gen, play, stats;
let heavyReady;

// ---- View routing ----
const VIEWS = ["homeView", "campaignView", "puzzlesView", "playView", "statsView"];

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

function goPlay() {
  showView("playView");
}

// ---- Generation overlay ----
// A modal spinner over generation. The worker runs off the main thread, so the
// page stays responsive and Cancel can terminate it mid-generation.
const overlay = {
  el: null,
  text: null,
  cancelBtn: null,
};

function showLoading(label) {
  overlay.el.classList.remove("error");
  overlay.text.textContent = label;
  overlay.cancelBtn.textContent = "Cancel";
  overlay.el.hidden = false;
}

function showLoadError(message) {
  overlay.el.classList.add("error");
  overlay.text.textContent = message;
  overlay.cancelBtn.textContent = "Close";
}

function hideLoading() {
  overlay.el.hidden = true;
}

// Generate a fresh puzzle for (kindIndex, mode), store it as a new game, and
// open it. Cancel terminates the worker and returns to where we were.
async function launch(kindIndex, mode) {
  await heavyReady; // need gen + play wired
  showLoading("Generating puzzle…");
  let result;
  try {
    result = await gen.generate({ target: kindIndex, drill: mode === "drill" });
  } catch (e) {
    if (e && e.name === "AbortError") {
      hideLoading(); // user cancelled
    } else {
      showLoadError(e && e.message ? e.message : "Generation failed.");
    }
    return;
  }
  hideLoading();
  const game = store.createGame({
    kindIndex,
    mode,
    puzzle: result.puzzle,
    solution: result.solution,
    givens: result.givens,
  });
  play.loadGame(game);
  goPlay();
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
  overlay.cancelBtn = document.getElementById("loadingCancel");
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
    onLaunch: launch,
    onResume: resume,
    onStats: goStats,
  });
  goHome(); // <-- first paint: real saved puzzles + campaign tree

  heavyReady = (async () => {
    const [p, s, g, w] = await Promise.all([
      import("./play.js"),
      import("./stats.js"),
      import("./gen.js"),
      import("./wasm.js"),
    ]);
    play = p;
    stats = s;
    gen = g;
    wasm = w;
    play.initPlay({ curriculum: CURRICULUM, onHome: goHome, onNewPuzzle: launch });
    stats.initStats({ curriculum: CURRICULUM, onHome: goHome });
    // Warm the wasm bridge so the first Hint is ready; Home and generation
    // (the worker) don't depend on it.
    wasm.ready();
  })();
}

boot();
