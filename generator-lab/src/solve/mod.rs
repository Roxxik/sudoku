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

pub use eliminate::{Eliminate, LogicBoard};
pub use fused::FusedLogicSolver;
pub use logic::LogicSolver;

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
