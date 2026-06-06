//! Existence / uniqueness probers — count a board's completions, capped
//! (`cap = 1` = existence, `cap = 2` = uniqueness, `== 1`). Two interchangeable
//! engines live here so they cross-check each other and bench head to head:
//!
//! - [`search`]: the fast one — the fill's [`scan`](crate::scan) + [`sieve`](crate::sieve)
//!   machinery (naked-single sieve + dead/solved verdict + branch), generic over the
//!   [`BranchStrategy`](crate::scan::BranchStrategy) so `Mrv` vs `Bivalue` plug in
//!   and bench. Runs on a [`SearchState<M>`](crate::repr::SearchState).
//! - [`singles`]: the composable reference/fallback — drives the generic
//!   [`technique`](crate::technique) singles (naked + hidden) to a fixpoint, then
//!   branches. Slower, but uses the full technique set and runs on any
//!   [`SolveView`](crate::repr::SolveView). Kept as the correctness oracle and the
//!   "composable always works" baseline.
//!
//! Both are correct (completeness comes from the branch); they differ only in how
//! much they propagate before branching and how the branch cell is picked.

pub mod propagate;
pub mod search;
pub mod singles;

// The packed W=8 SoA prober — the new-`repr` twin of `crate::simt::prober`. An AVX
// native play; the wasm cdylib ships the scalar `Search` path, so keep it (and the
// `std::simd` warp it builds) out of the wasm binary, exactly as the old SIMT stack.
#[cfg(not(target_arch = "wasm32"))]
pub mod simt;

pub use propagate::Propagate;

use crate::repr::{SearchState, SolveView};
use crate::scan::BranchStrategy;
use core::marker::PhantomData;

/// A completion prober over board type `B` — the swap point between engines
/// ([`Search`], [`Singles`]) so the strip loop and the bench read `P: Prober<…>`
/// instead of committing to one. Stateless: the methods are associated functions on
/// a marker type. `count_completions` is the primitive; existence and uniqueness
/// derive from it.
pub trait Prober<B> {
    /// Completions of `board`, counted only up to `cap`.
    fn count_completions(board: B, cap: usize) -> usize;

    /// Whether `board` has at least one completion.
    fn has_completion(board: B) -> bool {
        Self::count_completions(board, 1) >= 1
    }

    /// Whether `board` has exactly one completion (the strip gate).
    fn is_unique(board: B) -> bool {
        Self::count_completions(board, 2) == 1
    }
}

/// The fast prober: [`search`] (scan/sieve) with branch strategy `S`. Probes a
/// [`SearchState`] at any [`Branchable`] packing. The sieve depth rides on the
/// strategy (`Mrv<D>`; Bivalue has no knob), so a bench sweeps it as `Search<Mrv<2>>`,
/// `Search<Mrv<9>>`, … — the same strategy at different candidate-count caps.
pub struct Search<S>(PhantomData<S>);

impl<M, S> Prober<SearchState<M>> for Search<S>
where
    M: Propagate,
    S: BranchStrategy,
{
    fn count_completions(board: SearchState<M>, cap: usize) -> usize {
        search::count_completions::<M, S>(board, cap)
    }
}

/// The composable reference prober: [`singles`]. Probes any [`SolveView`].
pub struct Singles;

impl<B: SolveView> Prober<B> for Singles {
    fn count_completions(board: B, cap: usize) -> usize {
        singles::count_completions(board, cap)
    }
}
