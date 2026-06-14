//! Generation web worker.
//!
//! Puzzle generation is a synchronous, potentially second-long CPU loop (hard
//! targets like a drilled Hidden Quad can run the full attempt budget). Running
//! it on the main thread would freeze the page, and once it's running there is
//! no way to interrupt a synchronous call. So generation lives here, in a
//! dedicated worker: the page posts a request, we generate and post the result
//! back, and **cancellation is `worker.terminate()`** on the page side (the only
//! way to stop a synchronous wasm call mid-flight) followed by respawning a fresh
//! worker. See `web/gen.js` for the page-side client.
//!
//! Trunk builds this as a `data-type="worker"` binary with a loader shim
//! (`gen_worker_loader.js`); the page does `new Worker("./gen_worker_loader.js",
//! { type: "module" })`. Built as a separate wasm binary from the `lib.rs`
//! bridge, so it re-links core — that's the cost of an isolated worker instance.
//!
//! Message protocol (plain structured-clone objects, no owned wasm handles):
//!   worker -> page, on init:        `{ ready: true }`
//!   page   -> worker, to generate:  `{ target: <kindIndex>, drill: <bool> }`
//!   worker -> page, on success:     `{ puzzle, solution, givens }`
//!   worker -> page, on failure:     `{ error: <string> }`
//! The worker seeds its own RNG (we want variety, not reproducibility), so the
//! page never sends a seed.

use js_sys::{Math, Object, Reflect};
use sudoku_core::lab;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{DedicatedWorkerGlobalScope, MessageEvent};

/// Rejection-sampling budget per request — mirrors `lib.rs`. Easy targets never
/// approach it; the hardest may exhaust it, surfacing as an `{ error }` reply.
const MAX_ATTEMPTS: usize = 10_000;

fn main() {
    let scope: DedicatedWorkerGlobalScope = js_sys::global().unchecked_into();
    let scope_for_msg = scope.clone();

    let onmessage = Closure::wrap(Box::new(move |msg: MessageEvent| {
        let data = msg.data();
        let target = num_field(&data, "target").unwrap_or(-1.0);
        let drill = bool_field(&data, "drill");
        let reply = generate(target, drill);
        // Best-effort: if the page already terminated us this never runs.
        let _ = scope_for_msg.post_message(&reply);
    }) as Box<dyn Fn(MessageEvent)>);
    scope.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
    onmessage.forget();

    // Announce readiness. wasm init yields to the JS event loop before our
    // onmessage handler is registered, so any request the page sent earlier was
    // dropped; the page waits for this before posting (see gen.js).
    let ready = Object::new();
    let _ = Reflect::set(&ready, &"ready".into(), &JsValue::TRUE);
    let _ = scope.post_message(&ready);
}

/// Generate one puzzle for `target` (a `lab::kinds` index) in train or drill
/// mode, returning the reply object the page expects.
fn generate(target: f64, drill: bool) -> JsValue {
    if !(target >= 0.0) || target as usize >= lab::kinds::NUM {
        return err(&format!("target kind {target} out of range (0..{})", lab::kinds::NUM));
    }
    let target = target as usize;
    let spec = if drill {
        lab::Spec::drill(target)
    } else {
        lab::Spec::train(target)
    };
    let mut rng = lab::Rng::from_seed(random_seed());
    let (generated, _stats) = lab::generate(&mut rng, &spec, MAX_ATTEMPTS);
    match generated {
        Some(g) => {
            let obj = Object::new();
            set_str(&obj, "puzzle", &g.puzzle.0.to_line());
            set_str(&obj, "solution", &g.solution.0.to_line());
            let _ = Reflect::set(&obj, &"givens".into(), &(g.givens as f64).into());
            obj.into()
        }
        None => err("could not generate a puzzle within the attempt budget"),
    }
}

/// A fresh 64-bit seed from two `Math.random()` draws. We only need variety
/// between puzzles, not cryptographic quality or reproducibility, and a worker
/// has no `SystemTime`.
fn random_seed() -> u64 {
    let hi = (Math::random() * (u32::MAX as f64 + 1.0)) as u64;
    let lo = (Math::random() * (u32::MAX as f64 + 1.0)) as u64;
    (hi << 32) | lo
}

fn err(message: &str) -> JsValue {
    let obj = Object::new();
    set_str(&obj, "error", message);
    obj.into()
}

fn set_str(obj: &Object, key: &str, value: &str) {
    let _ = Reflect::set(obj, &key.into(), &JsValue::from_str(value));
}

fn num_field(data: &JsValue, key: &str) -> Option<f64> {
    Reflect::get(data, &key.into()).ok().and_then(|v| v.as_f64())
}

fn bool_field(data: &JsValue, key: &str) -> bool {
    Reflect::get(data, &key.into())
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}
