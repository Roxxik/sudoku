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
    if (e.data && e.data.error) p.reject(new Error(e.data.error));
    else p.resolve(e.data); // { puzzle, solution, givens }
  });

  worker.addEventListener("error", (e) => {
    if (!pending) return;
    const p = pending;
    pending = null;
    p.reject(new Error(e.message || "worker crashed"));
  });
}

// Generate one puzzle. `target` is a curriculum kindIndex, `drill` picks
// drill-mode over train. Resolves to { puzzle, solution, givens }; rejects on a
// generation failure (budget exhausted) or if cancelled.
export function generate({ target, drill }) {
  if (pending) return Promise.reject(new Error("a generation is already running"));
  if (!worker) spawn();
  return new Promise((resolve, reject) => {
    pending = { resolve, reject };
    const mine = pending;
    ready.then(() => {
      // Guard against a cancel() that fired between request and readiness.
      if (pending === mine) worker.postMessage({ target, drill });
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
