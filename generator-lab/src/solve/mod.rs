//! `solve` — the **logic solver**: a technique-driven engine that solves a puzzle
//! by applying human deduction techniques to a fixpoint, **never backtracking**.
//! The counterpart of [`probe`](crate::probe): a prober *searches* (it branches and
//! guesses to count completions); a [`LogicSolver`] only *deduces* (it places what
//! the allowed toolbox forces and stops when nothing more is forced). It is the spec
//! oracle — the strip loop's difficulty gate — not a completion oracle.
//!
//! It was once called the `baseline` engine, renamed because "baseline" said nothing
//! about *what* it is. It exposes two spec primitives:
//!
//! - [`Solver::solve_tracked`]: solve with the `allowed` toolbox, easiest-first,
//!   reporting whether it [`solved`](SolveTrace::solved) and how many times each kind
//!   fired — the strip's solvability gate and requirement counter in one pass.
//! - [`Solver::min_target_uses`]: verify's avoid-target walk — how many times a
//!   `target` technique is *forced* given the rest of the `scope` toolbox.
//!
//! Like the prober, the engine is a swap point behind a trait so the bench and the
//! strip read `S: Solver<…>` rather than a concrete type. The one engine here today
//! is [`LogicSolver`], the **composable** reference: the full ladder
//! ([`techniques`]) written once over any [`LogicBoard`], packing-agnostic and the
//! kept default — exactly the role [`probe::Singles`](crate::probe::Singles) plays
//! for the prober. A fused per-band fast path (the shape of bb's `band_update`) can
//! later slot in behind the same `Solver` surface; the composable form stays the
//! fallback and the correctness oracle.

mod combinations;
mod eliminate;
mod fused;
mod logic;
mod techniques;

// The packed W=8 SoA baseline-solver machinery — the `solve` analogue of
// [`probe::simt`](crate::probe::simt): the vectorized baseline closure and scalar subset
// fallback the unified warp drives across SIMD lanes. An AVX native play (the wasm cdylib
// ships the scalar fused path), so keep it and its `std::simd` warp out of the wasm binary.
#[cfg(not(target_arch = "wasm32"))]
pub mod simt;

pub use eliminate::{Eliminate, LogicBoard};
pub use fused::FusedLogicSolver;
pub use logic::LogicSolver;
/// Confluence-test surface: the reorderable harder ladder and a fixpoint solve that
/// takes its order, for `examples/confluence.rs`.
pub use logic::{HARD_STEPS_DEFAULT, HardStep, solve_fixpoint_with_order};
/// Campaign difficulty grader surface (cold path): the instrumented easiest-first solve
/// and the step trace its per-puzzle signals are derived from (see [`crate::grade`]).
pub use logic::{CHEAP_KINDS, GradeStep, GradeTrace, solve_graded};
/// Trunk-bucket fill-path profiler (cold path): the frontier-width + locked-candidate signal
/// that gives a singles-solvable puzzle a continuous difficulty sub-order (see
/// [`crate::grade::trunk_rating`], `docs/grader-external-calibration.md` Stage 2).
/// [`trunk_profiles_rand`](logic::trunk_profiles_rand) is the Stage-4 randomized-frontier-average
/// refinement (§4.5).
pub use logic::{TrunkProfile, trunk_profile, trunk_profiles_rand};

/// Baseline-gate workload counters (`feature = "count"`), for the SIMT-baseline
/// solver design study — read by the `baselinestat` example.
#[cfg(feature = "count")]
pub use fused::{fstat_reset, fstat_snapshot};

/// Unified-warp utilization counters (`feature = "count"`) — read by the `simtutil`
/// example.
#[cfg(all(feature = "count", not(target_arch = "wasm32")))]
pub use simt::{uwstat_reset, uwstat_snapshot};

/// Harder-ladder memo counters (`feature = "count"`) — the cross-stall memo's
/// scan-skip and rebuild diagnostics (see `simt::LSTAT`).
#[cfg(all(feature = "count", not(target_arch = "wasm32")))]
pub use simt::{lstat_reset, lstat_snapshot};

/// Per-kind harder-technique check/fire census (`feature = "count"`) — `TCHK`/`TFIRE`,
/// read by `findpar-bench`'s `--techstats` table (see `simt::TCHK`). `THIST` + `THIST_CAP`
/// are the per-baseline-solve fire histogram behind `--techhist` (see `simt::THIST`).
#[cfg(all(feature = "count", not(target_arch = "wasm32")))]
pub use simt::{
    THIST_CAP, tchk_reset, tchk_snapshot, tfire_reset, tfire_snapshot, thist_reset,
    thist_snapshot,
};

/// Detailed kernel bookkeeping (`feature = "kernel_count"`): `KSTAT` (per-digit sums) and
/// `KHIST` (per-lane-pass distributions) with the `kc` slot map, read by `findpar-bench`
/// (see `simt::KSTAT` / `simt::KHIST` / `simt::kc`).
#[cfg(all(feature = "kernel_count", not(target_arch = "wasm32")))]
pub use simt::{
    kc, khist_reset, khist_snapshot, krbtick_reset, krbtick_snapshot, kstat_reset,
    kstat_snapshot,
};

use crate::spec::kinds::{KindMask, SolveTrace};

/// A technique-driven solver over board `B` — the swap point between engines, so the
/// strip loop and the bench commit to a capability, not a concrete type. Stateless:
/// the methods are associated functions on a marker type, mirroring
/// [`Prober`](crate::probe::Prober).
pub trait Solver<B> {
    /// Solve `board` with the `allowed` toolbox, easiest-first, tallying which kinds
    /// fired. `solved` and the per-kind counts are what the spec gate and requirement
    /// check read.
    fn solve_tracked(board: &B, allowed: KindMask) -> SolveTrace;

    /// The minimum number of times a `target` technique is *forced* to fire to solve
    /// `board` given the `scope` toolbox: prefer any non-target in-scope step, take a
    /// target step only when stuck on non-targets, counting those. `>= required`
    /// means the target is irreplaceable by the rest of `scope`.
    fn min_target_uses(board: &B, scope: KindMask, target: KindMask) -> usize;
}
