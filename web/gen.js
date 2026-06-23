"use strict";

// Page-side client for the generation web worker (wasm/src/bin/gen_worker.rs).
//
// Generation is a synchronous wasm loop that can run up to a second on hard
// targets, so it lives in a worker to keep the page responsive. The only way to
// interrupt a synchronous wasm call is to terminate the worker, so `cancel()`
// does exactly that and respawns a fresh one for the next request. One request
// is in flight at a time (the UI launches a single puzzle at a time).

// The loader shim Trunk emits for the worker binary. Resolved against this
// module's URL so it works wherever the bundle is served from.
const LOADER_URL = new URL("./gen_worker_loader.js", import.meta.url);

let worker = null;
let ready = null; // Promise that resolves once the worker posts { ready: true }
let pending = null; // { resolve, reject } for the in-flight request, or null

function spawn() {
  worker = new Worker(LOADER_URL, { type: "module" });

  // The worker loads wasm asynchronously and only registers its message handler
  // afterwards, so it announces readiness; we hold requests until then (a
  // request sent earlier would be dropped on the worker side).
  ready = new Promise((resolve) => {
    const onReady = (e) => {
      if (e.data && e.data.ready) {
        worker.removeEventListener("message", onReady);
        resolve();
      }
    };
    worker.addEventListener("message", onReady);
  });

  worker.addEventListener("message", (e) => {
    if (e.data && e.data.ready) return; // handled by the readiness listener
    if (!pending) return;
    const p = pending;
    pending = null;
    if (e.data && e.data.error) {
      // `retriable` (budget exhaustion) tells the page whether "Keep searching"
      // makes sense; other errors won't be fixed by retrying.
      const err = new Error(e.data.error);
      err.retriable = !!e.data.retriable;
      p.reject(err);
    } else p.resolve(e.data); // { puzzle, solution, givens, seed, attempts, grade }
  });

  worker.addEventListener("error", (e) => {
    if (!pending) return;
    const p = pending;
    pending = null;
    p.reject(new Error(e.message || "worker crashed"));
  });
}

// Generate one puzzle. A campaign request gives `target` (a curriculum kindIndex)
// and `drill` (drill-mode over train); a custom-spec request gives `usages` (the
// per-kind usage array from spec.js), which takes precedence and is sent as the
// worker's `spec` field. `forceAny` (custom only) folds the Forced kinds into one
// disjunction -- the puzzle requires some member, not every one. `uncapped` lifts
// the worker's attempt budget so the search runs until it succeeds or is
// cancelled (the page offers this after a capped attempt gives up). `seed` (a u64
// decimal string) pins the RNG so every device generates the same puzzle -- the
// daily puzzle sends a per-(date, difficulty) seed; ordinary generation omits it
// and the worker draws its own. Resolves to { puzzle, solution, givens, seed }
// (seed is a decimal-string u64); rejects on a generation failure (capped budget
// exhausted) or if cancelled.
export function generate({ target, drill, usages, uncapped, forceAny, seed }) {
  if (pending) return Promise.reject(new Error("a generation is already running"));
  if (!worker) spawn();
  const msg = usages
    ? { spec: usages, uncapped: !!uncapped, forceAny: !!forceAny }
    : { target, drill, uncapped: !!uncapped };
  // A pinned seed rides either path (sent only when present, so ordinary requests
  // are byte-for-byte unchanged and the worker draws its own seed).
  if (seed != null) msg.seed = seed;
  return new Promise((resolve, reject) => {
    pending = { resolve, reject };
    const mine = pending;
    ready.then(() => {
      // Guard against a cancel() that fired between request and readiness.
      if (pending === mine) worker.postMessage(msg);
    });
  });
}

// Abort an in-flight generation: terminate the worker (the only way to stop the
// synchronous wasm loop), reject the pending promise, and drop the instance so
// the next generate() spins up a clean one.
export function cancel() {
  if (worker) {
    worker.terminate();
    worker = null;
    ready = null;
  }
  if (pending) {
    const p = pending;
    pending = null;
    p.reject(new DOMException("generation cancelled", "AbortError"));
  }
}

export function isGenerating() {
  return pending !== null;
}
