//! # generator-lab — clean-slate hidden-quad generator testing grounds
//!
//! A standalone, spec-driven sudoku generator built ONLY to benchmark and tune
//! the performance-critical generation path on native and mobile (wasm). It
//! reproduces core's `random`-method `train`/`drill` generation for the ladder
//! up to HiddenQuad, and nothing else: no local search, no construction, and
//! none of the play-time hint machinery (`Step`/`Deduction`/`focus_cells`).
//! Techniques mutate the board and report only which KIND fired — all the spec
//! gates need.
//!
//! Why separate from core: core's structures pull dual duty (generate hints
//! during play AND generate puzzles), and mixing those two cost profiles blocks
//! tuning the hot path. This crate is the hot path alone, on a clean slate, so
//! we can find what's gainable before touching core. NOT for backport this
//! session — it's a benchable PoC.
//!
//! ## Modules
//! - [`grid`] / [`rng`] / [`util`]: primitives copied from core (kept faithful;
//!   the strip stream reproduces core's for a given seed).
//! - [`bb`]: the shared bitboard core — ONE transposed digit-board
//!   representation for both the uniqueness prober (existence DFS) and the
//!   baseline technique engine, so candidate propagation isn't duplicated.
//! - [`techniques`]: scalar reference engine — `solve_tracked` and
//!   `min_target_uses` (verify's avoid-target walk, the cold path); also the
//!   kind indices/masks the spec and `bb` share.
//! - [`spec`]: compact `train`/`drill` spec, faithful to core's `Spec`.
//! - [`verify`]: spec verification reduced to a bool (scalar, cold path).
//! - [`generator`]: the random strip-generate pipeline.
//! - [`packed`] / [`warp`] (native only): the packed/SIMT existence prober — a
//!   warp of W=8 per-lane DFS searches (gather-free smear+ALU kernel) with
//!   streaming refill, and the host driver that batches the strip loop's
//!   uniqueness gates onto it. The scalar [`bb::ProberBoard`] prober is kept as
//!   the shipped wasm path, the correctness oracle, and the perf bar (SIMT is a
//!   native AVX play; on wasm simd128 the packing ceiling is too small to pay).

#![feature(portable_simd)]

pub mod bb;
pub mod generator;
pub mod grid;
pub mod rng;
pub mod spec;
pub mod techniques;
pub mod util;
pub mod verify;

// The packed prober is an AVX (W=8) native play; the wasm cdylib build uses the
// scalar prober via `wasm_exports`, so keep these out of the wasm binary.
#[cfg(not(target_arch = "wasm32"))]
pub mod packed;
#[cfg(not(target_arch = "wasm32"))]
pub mod warp;

use spec::Spec;
use techniques::HIDDEN_QUAD;

/// Build the PoC spec for a `mode`: 0 = train(HiddenQuad), else drill(HiddenQuad).
pub fn spec_for_mode(mode: u32) -> Spec {
    if mode == 0 {
        Spec::train(HIDDEN_QUAD)
    } else {
        Spec::drill(HIDDEN_QUAD)
    }
}

/// JS-callable bench entry points, exported from the wasm32 cdylib build, timed
/// JS-side with `performance.now()` exactly as a browser measures. Mirrors
/// solver-lab's wasm harness so the mobile front-end carries over.
#[cfg(target_arch = "wasm32")]
mod wasm_exports {
    use crate::rng::Rng;
    use crate::{generator, spec_for_mode};

    /// Run exactly `attempts` strip attempts for `mode` (0=train, 1=drill) from
    /// `seed`, returning a u32-truncated fingerprint over the produced puzzles
    /// (so JS needs no BigInt bridge; 32 bits is ample to catch divergence). JS
    /// times this with `performance.now()`.
    #[unsafe(no_mangle)]
    pub extern "C" fn bench(mode: u32, attempts: u32, seed: u32) -> u32 {
        let spec = spec_for_mode(mode);
        let mut rng = Rng::from_seed(seed as u64);
        let (_stats, fp) = generator::run_attempts(&mut rng, &spec, attempts as usize);
        fp as u32
    }

    /// Number of puzzles produced in `attempts` attempts (the yield), so the JS
    /// harness can report puzzles/sec alongside us/attempt.
    #[unsafe(no_mangle)]
    pub extern "C" fn bench_yield(mode: u32, attempts: u32, seed: u32) -> u32 {
        let spec = spec_for_mode(mode);
        let mut rng = Rng::from_seed(seed as u64);
        let (stats, _fp) = generator::run_attempts(&mut rng, &spec, attempts as usize);
        stats.successes as u32
    }

    /// Grid-sensitive cross-backend determinism fingerprint (u32-truncated):
    /// folds the solution grid + strip order of every attempt, so it diverges if
    /// native and wasm ever disagree on the RNG/fill output (unlike `bench`'s
    /// success-only fp). Must equal the native `determinism_fp` for the seed.
    #[unsafe(no_mangle)]
    pub extern "C" fn det_fp(attempts: u32, seed: u32) -> u32 {
        let mut rng = Rng::from_seed(seed as u64);
        generator::determinism_fp(&mut rng, attempts as usize) as u32
    }
}
