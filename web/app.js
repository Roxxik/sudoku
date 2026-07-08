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
import { campaignUsages } from "./campaign.js";
import { masksFromUsages } from "./spec.js";
import { migrateModes } from "./modes.js";

// Heavy/non-Home modules, bound once `heavyReady` resolves.
let wasm, gen, play, stats, custom;
let heavyReady;

// ---- View routing ----
const VIEWS = ["homeView", "campaignView", "dailyView", "customView", "puzzlesView", "playView", "statsView", "settingsView", "helpView", "privacyView"];

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
    kind: "imported",
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

// The spec's ALLOWED techniques (baseline minus the forced ones) for a forced-start
// launch -- only these advance the board, so the head start follows the intended
// path and stops at the forced technique. Every mode now carries its spec masks
// (campaign builds them in JS via campaignUsages, exactly like Custom/Review/Daily),
// so this is a plain derivation. Null when there are no masks -> a plain start.
function allowedMask(masks) {
  return masks ? masks.baseline & ~masks.forced : null;
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

// Open How-to-play from the play view: same pause/resume routing as Settings, so
// reading the controls doesn't run the solve clock.
function openHelpFromPlay() {
  play.pause();
  home.openHelp(() => {
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
  // Every mode generates from the same explicit usage array. Campaign builds it in
  // JS (campaign.js) rather than sending a privileged kindIndex to the worker;
  // Custom/Review/Daily bring their own. The spec masks (hint tree + forced-start
  // head start) fall straight out of the usages and are stored on the record, so
  // every mode carries them the same way.
  const isCampaign = typeof req.kindIndex === "number";
  const usages = isCampaign ? campaignUsages(CURRICULUM, req.kindIndex, req.mode) : req.usages;
  const specMasks = isCampaign ? masksFromUsages(usages) : req.specMasks;
  let result;
  try {
    const genReq = { usages, uncapped, forceAny: req.forceAny };
    // A daily request carries a pinned seed so every device generates the same
    // puzzle; ordinary requests omit it and the worker draws its own.
    if (req.seed != null) genReq.seed = req.seed;
    result = await gen.generate(genReq);
  } catch (e) {
    if (e && e.name === "AbortError") {
      hideLoading(); // user cancelled
    } else {
      showLoadError(e && e.message ? e.message : "Generation failed.", !!(e && e.retriable));
    }
    return;
  }
  // "Play from Forced": advance the solver to the first forced technique and seed
  // the board with the digits placed along the way. Computed before the spinner
  // clears so any brief solve cost stays hidden; a null result (wasm not warmed,
  // or nothing easier to place -- e.g. hidden single) falls back to a plain start.
  // The cutoff is the forced technique's difficulty: a custom spec passes its own
  // (its first/easiest forced technique); a campaign launch derives it from the
  // target kind.
  let startValue = null;
  if (req.fromForced) {
    await wasm.ready(); // the head start runs on the main-thread solver bridge
    const allowed = allowedMask(specMasks);
    if (allowed) {
      startValue = play.forcedStartValues(result.puzzle, allowed);
      if (startValue) {
        // Keep only the advanced placements: givens live in the puzzle line, not
        // in value[], so the board preview and Restart treat them as clues.
        for (let i = 0; i < startValue.length; i++) {
          if (result.puzzle[i] >= "1" && result.puzzle[i] <= "9") startValue[i] = 0;
        }
      }
    }
  }
  hideLoading();
  // The worker also returns debug metadata (decimal-string u64 seed + the attempt
  // count) for the cheat-mode display.
  const game = isCampaign
    ? store.createGame({
        kind: "campaign",
        kindIndex: req.kindIndex,
        mode: req.mode,
        specMasks,
        puzzle: result.puzzle,
        solution: result.solution,
        givens: result.givens,
        seed: result.seed,
        attempts: result.attempts,
        grade: result.grade,
        value: startValue,
        fromForced: !!req.fromForced,
      })
    : store.createGame({
        kind: req.kind || "custom",
        mode: req.mode || "custom",
        lesson: req.lesson || null,
        spec: usages,
        specMasks,
        forceAny: !!req.forceAny,
        label: req.label,
        daily: req.daily || null,
        puzzle: result.puzzle,
        solution: result.solution,
        givens: result.givens,
        seed: result.seed,
        attempts: result.attempts,
        grade: result.grade,
        value: startValue,
        fromForced: !!req.fromForced,
      });
  play.loadGame(game);
  goPlay();
}

// Re-generate a fresh puzzle for an existing game (the play view's "New puzzle").
// A campaign game replays its (kindIndex, mode); a Review game keeps its lesson +
// mode so the new puzzle stays a Review game; other spec games (a daily's pinned
// puzzle isn't reproducible as "another like it", so it drops to custom) replay
// their usage array as a plain custom puzzle. An imported/specless game has nothing
// to regenerate.
function regenerate(g) {
  if (typeof g.kindIndex === "number") {
    launch({ kindIndex: g.kindIndex, mode: g.mode });
    return;
  }
  if (!Array.isArray(g.spec)) return; // imported / specless: nothing to replay
  const review = g.kind === "review";
  launch({
    kind: review ? "review" : "custom",
    lesson: review ? g.lesson : null,
    mode: review ? g.mode : null,
    usages: g.spec,
    label: g.label,
    specMasks: g.specMasks,
    forceAny: g.forceAny,
  });
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
  // Enforce the storage retention policy once, up front: reclaim the heavy board +
  // move log of every already-solved game (older builds kept them) and any orphaned
  // per-game keys, so a long-time player's store isn't carrying dozens of finished
  // puzzles' undo timelines against the quota. Cheap and synchronous; runs before the
  // first read so Home lists from an already-pruned index.
  store.reclaimStorage();

  // Stamp the persisted `kind` discriminator (plus recovered Review lesson/mode and
  // backfilled campaign spec masks) onto records from older builds, so every surface
  // reads the mode as a field instead of re-sniffing it. Runs before the first render;
  // idempotent on later boots.
  migrateModes();

  wireOverlay();
  wireVisibility();

  home.initHome({
    curriculum: CURRICULUM,
    showView,
    onLaunch: (kindIndex, mode, fromForced) => launch({ kindIndex, mode, fromForced }),
    onLaunchSpec: (req) => launch(req),
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
      onHelp: openHelpFromPlay,
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
