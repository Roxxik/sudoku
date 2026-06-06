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
//! - [`repr`]: the representation layer — the [`Digit`](repr::Digit)/[`Mark`](repr::Mark)
//!   value types, the [`DigitGrid`](repr::DigitGrid)/[`Board`](repr::Board) grids, and
//!   the banded SIMD packings ([`repr::banded`]) every solver/prober/generator is built
//!   on. The whole crate lives on this layer.
//! - [`rng`] / [`util`]: primitives kept faithful to core (the strip stream
//!   reproduces core's for a given seed) plus the shared FNV fingerprint.
//! - [`technique_kinds`]: the shared taxonomy — kind indices, `KindMask`,
//!   `DIFFICULTY`/`NAMES`, and the `SolveTrace` a logic solve returns.
//! - [`scan`] / [`sieve`]: the MRV/Bivalue branch strategies and the
//!   popcount-free naked-single sieve the fast prober runs on.
//! - [`solve`]: the **logic solver** — `LogicSolver` and its fused fast path
//!   `FusedLogicSolver`, the technique-driven, no-backtracking spec gate over the
//!   `repr` layer, parallel to [`probe`].
//! - [`probe`]: the existence/uniqueness probers (`Search`, `Singles`) over the
//!   `repr` layer, the composable [`technique`](probe::technique) singles the
//!   `Singles` reference prober drives, plus the native packed W=8 SIMT prober
//!   ([`probe::simt`]).
//! - [`spec`]: compact `train`/`drill` spec, faithful to core's `Spec`.
//! - [`fill`]: the random full-grid filler (banded-bitboard MRV search), the first
//!   half of every attempt.
//! - [`generate`]: the strip-generate pipeline on the `repr` layer (the [`probe`]
//!   prober + [`solve`] gate). [`generate::random`] is the shipped scalar/wasm
//!   path; [`generate::random_simt`] (native only) is the W=8 SIMT warp host that
//!   batches the strip loop's uniqueness gates onto [`probe::simt`]. SIMT is a
//!   native AVX play; on wasm simd128 the packing ceiling is too small to pay, so
//!   the wasm cdylib ships the scalar path.

#![feature(portable_simd)]
#![feature(const_trait_impl)]

pub mod repr;
pub(crate) mod counters;
pub mod fill;
pub mod generate;
pub mod probe;
pub mod rng;
pub mod scan;
pub mod sieve;
pub mod solve;
pub mod spec;
pub mod technique_kinds;
pub mod util;

use spec::Spec;
use technique_kinds::HIDDEN_QUAD;

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
    use crate::{generate, spec_for_mode};

    /// Run exactly `attempts` strip attempts for `mode` (0=train, 1=drill) from
    /// `seed`, returning a u32-truncated fingerprint over the produced puzzles
    /// (so JS needs no BigInt bridge; 32 bits is ample to catch divergence). JS
    /// times this with `performance.now()`.
    #[unsafe(no_mangle)]
    pub extern "C" fn bench(mode: u32, attempts: u32, seed: u32) -> u32 {
        let spec = spec_for_mode(mode);
        let mut rng = Rng::from_seed(seed as u64);
        let (_stats, fp) = generate::run_attempts(&mut rng, &spec, attempts as usize);
        fp as u32
    }

    /// Number of puzzles produced in `attempts` attempts (the yield), so the JS
    /// harness can report puzzles/sec alongside us/attempt.
    #[unsafe(no_mangle)]
    pub extern "C" fn bench_yield(mode: u32, attempts: u32, seed: u32) -> u32 {
        let spec = spec_for_mode(mode);
        let mut rng = Rng::from_seed(seed as u64);
        let (stats, _fp) = generate::run_attempts(&mut rng, &spec, attempts as usize);
        stats.successes as u32
    }

    /// Grid-sensitive cross-backend determinism fingerprint (u32-truncated):
    /// folds the solution grid + strip order of every attempt, so it diverges if
    /// native and wasm ever disagree on the RNG/fill output (unlike `bench`'s
    /// success-only fp). Must equal the native `determinism_fp` for the seed.
    #[unsafe(no_mangle)]
    pub extern "C" fn det_fp(attempts: u32, seed: u32) -> u32 {
        let mut rng = Rng::from_seed(seed as u64);
        generate::determinism_fp(&mut rng, attempts as usize) as u32
    }
}
