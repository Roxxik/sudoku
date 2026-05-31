//! # solver-lab — temporary fast-solver testing grounds
//!
//! Goal: find the **fastest existence prober** for the `train(HiddenQuad)`
//! generator, then port the winner back into `core::uniqueness`.
//!
//! ## Why this crate exists
//!
//! Profiling the real generator (`gen_cache --method random --target
//! hidden-quad`) showed ~49% of total time in the existence prober
//! (`FastSolver::solve_first` + `ExistenceProbe`), ~25% in baseline technique
//! solving, the rest in grid generation / stripping. The prober is the biggest
//! lever, but it is embedded in a large amount of generator code that makes
//! isolated benchmarking awkward — hence this self-contained lab.
//!
//! ## The key simplification
//!
//! The prober's entire workload is determined by the strip *trajectory*, and
//! the trajectory depends on exactly two gates per stripped cell: `unique` (the
//! prober) AND `baseline-solvable` (the technique solver). The generator's
//! `requirement_met` / `best` / `verify` machinery is computed *after* the
//! strip and only decides whether an *attempt succeeds* — it never changes the
//! trajectory, and success-rate is solver-independent (all correct provers give
//! identical answers, hence identical trajectories). So:
//!
//! - To compare provers we time [`gen::strip_attempt`] over a fixed seed set.
//! - We do NOT need `Step`, `Deduction`, `Spec`, requirement counts, or
//!   `verify`. The baseline gate is reduced to a single bool
//!   ([`techniques::baseline_solvable`]).
//!
//! ## Modules
//!
//! - [`grid`] / [`rng`] / [`util`]: primitives copied from `core` (kept in sync
//!   by the cross-checks in `tests/`, which use `sudoku-core` as a
//!   dev-dependency only — no production coupling).
//! - [`techniques`]: the minimal up-to-HiddenQuad baseline solver (fixed
//!   scenery, not what we optimize).
//! - [`oracle`]: canonical solution counter — the correctness reference every
//!   prober is cross-checked against.
//! - [`solvers`]: the prober variants under comparison. `simd-dense` is the
//!   current core implementation (the baseline to beat).
//! - [`generate`]: the ported strip loop, generic over the prober.

#![feature(portable_simd)]

pub mod check;
pub mod counters;
pub mod generate;
pub mod grid;
pub mod harness;
pub mod oracle;
pub mod rng;
pub mod solvers;
pub mod techniques;
pub mod util;

/// JS-callable bench entry points, exported from the wasm32 cdylib build.
///
/// These are *generic over the registry* — the JS front-ends call
/// `variant_count()` and `variant_name(i)` to discover the variants at runtime,
/// then `bench(i, attempts, seed)` to run each. So adding a prover (one line in
/// `solvers::register!`) needs no wasm or JS edit at all. Timing is done JS-side
/// with `performance.now()` — exactly how a browser measures. See
/// `solver-lab/web/bench.mjs`.
#[cfg(target_arch = "wasm32")]
mod wasm_exports {
    use crate::solvers::REGISTRY;

    /// Number of registered probers. JS loops `0..variant_count()`.
    #[unsafe(no_mangle)]
    pub extern "C" fn variant_count() -> u32 {
        REGISTRY.len() as u32
    }

    /// Pointer (into wasm linear memory) to variant `i`'s name bytes. The name
    /// is a `&'static str` living in the data segment, so the pointer is stable
    /// for the module's lifetime. Paired with [`variant_name_len`]; JS decodes
    /// `new Uint8Array(memory.buffer, ptr, len)` as UTF-8.
    #[unsafe(no_mangle)]
    pub extern "C" fn variant_name_ptr(i: u32) -> u32 {
        REGISTRY[i as usize].name.as_ptr() as u32
    }

    /// Byte length of variant `i`'s name. See [`variant_name_ptr`].
    #[unsafe(no_mangle)]
    pub extern "C" fn variant_name_len(i: u32) -> u32 {
        REGISTRY[i as usize].name.len() as u32
    }

    /// Run `attempts` strip attempts with variant `i` and return the trajectory
    /// fingerprint, truncated to u32 (so JS needs no BigInt<->i64 bridge; 32
    /// bits is ample to catch a diverging prover). JS times this with
    /// `performance.now()` and cross-checks the fingerprint across variants.
    #[unsafe(no_mangle)]
    pub extern "C" fn bench(i: u32, attempts: u32, seed: u32) -> u32 {
        (REGISTRY[i as usize].fingerprint)(attempts as usize, seed as u64).0 as u32
    }
}
