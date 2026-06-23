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
//!   page   -> worker, campaign:     `{ target: <kindIndex>, drill: <bool>, uncapped: <bool> }`
//!   page   -> worker, custom spec:  `{ spec: [<usage per kind>], forceAny: <bool>, uncapped: <bool> }`
//!   worker -> page, on success:     `{ puzzle, solution, givens, seed, attempts, grade }`
//!   worker -> page, on failure:     `{ error: <string> }`
//! A `spec` field (an array of per-kind usage codes — 0 off / 1 Allowed /
//! 2 Forced / 3 Conceded, see `web/spec.js`) selects the custom-spec path and
//! takes precedence over `target`/`drill`. `forceAny` (default false) flips the
//! Forced kinds from a conjunction (each required) to one `force_any` set, of which
//! the generator picks a single member per puzzle from the seed and forces just that
//! one (so the set's techniques come up evenly, not collapsed onto the easiest member).
//! `uncapped` (default false) lifts the
//! per-request attempt budget so a hard target keeps searching until it finds a
//! puzzle or the page terminates us — the page offers it after a capped request
//! gives up.
//! The worker seeds its own RNG (we want variety, not reproducibility), so the
//! page never *sends* a seed — but the seed it drew is *returned* (as a decimal
//! string, since a u64 won't fit a JS Number) so the page can show it for
//! debugging and, with the same (seed, target, drill), reproduce the puzzle.

use js_sys::{Array, Math, Object, Reflect};
use sudoku_core::lab;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{DedicatedWorkerGlobalScope, MessageEvent};

/// Rejection-sampling budget for a normal request — mirrors `lib.rs`. Easy
/// targets never approach it; the hardest may exhaust it, surfacing as an
/// `{ error }` reply (which the page can retry uncapped — see `generate`).
const MAX_ATTEMPTS: usize = 10_000;

fn main() {
    let scope: DedicatedWorkerGlobalScope = js_sys::global().unchecked_into();
    let scope_for_msg = scope.clone();

    let onmessage = Closure::wrap(Box::new(move |msg: MessageEvent| {
        let data = msg.data();
        let uncapped = bool_field(&data, "uncapped");
        // A `spec` array is the custom-spec path; otherwise it's a campaign
        // request keyed by target/drill.
        let reply = match usages_field(&data, "spec") {
            Some(usages) => generate_custom(&usages, bool_field(&data, "forceAny"), uncapped),
            None => {
                let target = num_field(&data, "target").unwrap_or(-1.0);
                let drill = bool_field(&data, "drill");
                generate(target, drill, uncapped)
            }
        };
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
/// mode, returning the reply object the page expects. `uncapped` lifts the
/// attempt budget so the search runs until it succeeds (or the page terminates
/// us); the only way out of a runaway uncapped search is `worker.terminate()`.
///
/// Uses the **isolated** spec builders (`train_isolated`/`drill_isolated`): an
/// Expert target is required against every *easier* technique across all branches
/// (so e.g. an X-Wing puzzle can't be solved with a Naked Pair instead). The
/// legacy branch-scoped `train`/`drill` are kept for an in-flight debugging
/// effort; see `generator-lab/src/spec/mod.rs`.
fn generate(target: f64, drill: bool, uncapped: bool) -> JsValue {
    if !(target >= 0.0) || target as usize >= lab::kinds::NUM {
        return err(&format!("target kind {target} out of range (0..{})", lab::kinds::NUM));
    }
    let target = target as usize;
    let spec = if drill {
        lab::Spec::drill_isolated(target)
    } else {
        lab::Spec::train_isolated(target)
    };
    run_spec(&spec, uncapped)
}

/// Generate one puzzle for an **explicit**, UI-built spec. `usages` is one code
/// per `lab::kinds` index — 0 = off (out of scope), 1 = Allowed, 2 = Forced,
/// 3 = Conceded (see `web/spec.js`) — rebuilt into a [`lab::Spec`] via the same
/// `explicit().allow/force/concede` API the curriculum builders use. Entries past
/// `lab::kinds::NUM` are ignored; a Forced kind requires a single firing (count 1,
/// as every builder uses). With nothing Forced the generator just yields the
/// minimal puzzle the Allowed toolbox solves — the page gates Generate on at
/// least one Forced kind, so that case is not normally reached.
///
/// When `force_any` is set the Forced kinds are not required individually; instead
/// they fold into a single [`force_any`](lab::Spec::force_any) set. The generator
/// then pins one member per puzzle (chosen from the seed) and forces exactly it, so
/// the set's techniques come up evenly across puzzles instead of collapsing onto the
/// easiest member. `force_any` adds each member to the baseline itself, so the loop
/// only collects their mask.
fn generate_custom(usages: &[u8], force_any: bool, uncapped: bool) -> JsValue {
    let mut spec = lab::Spec::explicit();
    let mut any_mask: lab::kinds::KindMask = 0;
    for (idx, &u) in usages.iter().enumerate() {
        if idx >= lab::kinds::NUM {
            break;
        }
        spec = match u {
            1 => spec.allow(idx),
            2 if force_any => {
                any_mask |= 1 << idx;
                spec // baseline membership comes from force_any below
            }
            2 => spec.force(idx, 1),
            3 => spec.concede(idx),
            _ => spec, // 0 / unknown: leave out of scope
        };
    }
    if any_mask != 0 {
        spec = spec.force_any(any_mask, 1);
    }
    run_spec(&spec, uncapped)
}

/// Run the rejection-sampling generator for `spec` and build the page reply —
/// shared by the campaign and custom-spec paths. `uncapped` lifts the attempt
/// budget (the only way out of a runaway uncapped search is `worker.terminate()`).
fn run_spec(spec: &lab::Spec, uncapped: bool) -> JsValue {
    let seed = random_seed();
    let mut rng = lab::Rng::from_seed(seed);
    let budget = if uncapped { usize::MAX } else { MAX_ATTEMPTS };
    let (generated, stats) = lab::generate(&mut rng, spec, budget);
    match generated {
        Some(g) => {
            let obj = Object::new();
            set_str(&obj, "puzzle", &g.puzzle.0.to_line());
            set_str(&obj, "solution", &g.solution.0.to_line());
            let _ = Reflect::set(&obj, &"givens".into(), &(g.givens as f64).into());
            // Decimal string: the seed is a u64 and would lose precision as a
            // JS Number. The page persists it for the cheat-mode seed display.
            set_str(&obj, "seed", &seed.to_string());
            // Rejection-sampling attempts spent to find this puzzle (the count of
            // the successful attempt). Persisted for the cheat-mode display; well
            // within a JS Number.
            let _ = Reflect::set(&obj, &"attempts".into(), &(stats.attempts as f64).into());
            // Difficulty grade: the absolute Tier-C continuous rating (in [0,1]) shown on
            // the page's thumbnails as a 5-band sub-tier read, plus the raw signals for the
            // cheat-mode breakdown. A cold easiest-first solve off the hot generation loop.
            let (signals, rating) = lab::grade::grade_one(spec, &g.puzzle.0);
            set_grade(&obj, &signals, rating);
            obj.into()
        }
        // Budget exhaustion is the one failure worth retrying uncapped: a hard or
        // tightly-constrained spec may just need more attempts. The page offers
        // "Keep searching" only for this `retriable` error.
        None => err_retriable(&format!(
            "could not generate a puzzle within {MAX_ATTEMPTS} attempts"
        )),
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

/// An error the page may retry uncapped (budget exhaustion only). Adds
/// `retriable: true` so the page shows "Keep searching"; plain [`err`] does not,
/// so other failures (a bad request, a solver error) just offer "Close".
fn err_retriable(message: &str) -> JsValue {
    let obj = Object::new();
    set_str(&obj, "error", message);
    let _ = Reflect::set(&obj, &"retriable".into(), &JsValue::TRUE);
    obj.into()
}

fn set_str(obj: &Object, key: &str, value: &str) {
    let _ = Reflect::set(obj, &key.into(), &JsValue::from_str(value));
}

/// Attach the puzzle's difficulty grade to the success reply as a nested `grade`
/// object: `band` (0 gentle / 1 medium / 2 spicy — the thumbnail label) plus the four
/// raw sub-tier signals the cheat-mode breakdown spells out. `scarcity` is `u32::MAX`
/// when the puzzle needed no harder step (no bottleneck); it goes out as JS `null` then
/// so the page renders a dash rather than a meaningless sentinel.
fn set_grade(reply: &Object, signals: &lab::grade::Signals, rating: f64) {
    let grade = Object::new();
    let set_num = |key: &str, value: f64| {
        let _ = Reflect::set(&grade, &key.into(), &value.into());
    };
    // Tier-C output: the continuous within-technique rating in [0,1], the 5-band cut of it
    // (`band` 0..=4), and the band count so the UI need not hardcode it.
    set_num("rating", rating);
    set_num("bands", lab::grade::UI_BANDS as f64);
    set_num("band", lab::grade::band_of_rating(rating, lab::grade::UI_BANDS) as f64);
    // Raw sub-tier signals for the cheat-mode breakdown — including the granular stall signals
    // (`open`, `alts`) that now drive the grade.
    set_num("bottleneckCount", signals.bottleneck_count as f64);
    set_num("dryFirings", signals.dry_firings as f64);
    set_num("longestDryRun", signals.longest_dry_run as f64);
    set_num("scanWork", signals.scan_work as f64);
    set_num("open", signals.open as f64);
    set_num("alts", signals.alts as f64);
    set_num("camo", signals.camo as f64);
    let scarcity = if signals.scarcity == u32::MAX {
        JsValue::NULL
    } else {
        (signals.scarcity as f64).into()
    };
    let _ = Reflect::set(&grade, &"scarcity".into(), &scarcity);
    let _ = Reflect::set(reply, &"grade".into(), &grade.into());
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

/// Read `key` as an array of small integers (the per-kind usage codes), or `None`
/// when it's absent / not an array — which routes the request to the campaign
/// path instead. Non-numeric entries fall back to 0 (off).
fn usages_field(data: &JsValue, key: &str) -> Option<Vec<u8>> {
    let v = Reflect::get(data, &key.into()).ok()?;
    if !Array::is_array(&v) {
        return None;
    }
    let arr: Array = v.unchecked_into();
    let mut out = Vec::with_capacity(arr.length() as usize);
    for i in 0..arr.length() {
        out.push(arr.get(i).as_f64().unwrap_or(0.0) as u8);
    }
    Some(out)
}
