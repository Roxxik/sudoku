"use strict";

// Access to the main-thread wasm bridge (the cdylib in wasm/src/lib.rs).
//
// Trunk initializes the module asynchronously and, when done, exposes its
// exports on `window.wasmBindings` and fires `TrunkApplicationStarted`. Module
// scripts may run before or after that event, so we both check the global and
// listen for the event.

let cached = null;

export function ready() {
  return new Promise((resolve) => {
    if (window.wasmBindings) {
      cached = window.wasmBindings;
      return resolve(cached);
    }
    window.addEventListener(
      "TrunkApplicationStarted",
      () => {
        cached = window.wasmBindings;
        resolve(cached);
      },
      { once: true }
    );
  });
}

// Synchronous best-effort handle; null until the bridge has loaded. Use for the
// optional hint path, which already degrades gracefully when the solver isn't up
// yet.
export function bindings() {
  return cached || window.wasmBindings || null;
}
