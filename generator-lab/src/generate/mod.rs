//! Puzzle generation built on the new `repr` foundations — the production
//! strip-generate pipeline.
//!
//! Each attempt fills a random solution ([`crate::fill`]) then strips clues in random
//! order, holding the puzzle uniquely solvable (the [`probe`](crate::probe) prober)
//! and meeting the spec's difficulty requirement (the [`solve`](crate::solve) gate).
//!
//! - [`random`]: the shipped scalar/wasm path — one attempt at a time over an
//!   incrementally-maintained dual-banded strip state.
//! - [`warp_host`] (native only): the W=8 SIMT warp host, running K attempts in
//!   lockstep over [`crate::probe::simt`] with per-lane refill.

mod random;

// The SIMT warp host (the production native generator): a job/lane-factored W=8 warp
// driving the `crate::probe::simt` packed prober over resumable per-lane strip coroutines.
// An AVX native play, so it (and the `std::simd` warp it batches onto) stays out of the
// wasm cdylib, which ships the scalar `run_attempts` path.
#[cfg(not(target_arch = "wasm32"))]
pub mod warp_host;

pub use random::{
    AttemptResult, GeneratedPuzzle, ReforceStat, Stats, StripView, attempt,
    baseline_fast_applicable, determinism_fp, generate, reforce_stat, run_attempts, verify,
};
// Prober propagation-toolbox experiment — native-only (reuses the native scalar LC kernel).
#[cfg(not(target_arch = "wasm32"))]
pub use random::{ToolboxStat, toolbox_stat};
// Deferred-strip (M1) + common-snapshot (M3) instrumentation — counter-gated.
#[cfg(feature = "count")]
pub use random::{DeferStat, defer_stat};
// True 2-digit-UA probe-skip catch measurement (M-UA-LIB) — counter-gated.
#[cfg(feature = "count")]
pub use random::{UaCatchStat, ua_catch_stat};
// Verify-share (M2) wall-time split — native, no counters.
#[cfg(not(target_arch = "wasm32"))]
pub use random::{VerifyShare, verify_share};
// UA pre-filter per-board build cost (stage-2 decision gate) — native, no counters.
#[cfg(not(target_arch = "wasm32"))]
pub use random::{ua_build_cost, ua_build_cost_pooled};
// Fill-byproduct (clue map + UA coordinate inverse) old-vs-new cost — native, no counters.
#[cfg(not(target_arch = "wasm32"))]
pub use random::fill_byproduct_cost_pooled;

use crate::probe::{Prober, Search};
use crate::repr::banded::{Bands, RowMajor};
use crate::repr::{Branchable, CELLS, DigitGrid, Marks, Puzzle, SolverState, Solution};
use crate::rng::Rng;
use crate::scan::Bivalue;

/// Strip `solution` down to a minimal uniquely-solvable puzzle, on packing `M` with
/// prober `P`: visit the 81 cells in a random order and remove each clue iff the
/// puzzle still has exactly one completion without it. Because every kept state
/// stays unique and the result is a subgrid of `solution`, the puzzle's unique
/// solution *is* `solution`. Generic over `P` so a bench can compare probers on the
/// same generation work.
pub fn strip_to_minimal<M, P>(rng: &mut Rng, solution: &Solution) -> Puzzle
where
    M: Branchable,
    P: Prober<SolverState<M>>,
{
    let mut digits: DigitGrid = solution.0.clone();
    let mut positions: [usize; CELLS] = core::array::from_fn(|i| i);
    rng.shuffle(&mut positions);
    // The candidate board is carried across the 81 attempts and reopened/reclosed in
    // place (bb's `apply_clear`/`apply_place`) rather than rebuilt with `from_digits`
    // each step. `clue` is the per-digit clue map the reopen needs.
    let mut state = SolverState::<M>::from_digits(&digits);
    let mut clue = SolverState::<M>::clue_map(&digits);
    for cell in positions {
        let Some(orig) = digits.get(cell) else {
            continue;
        };
        digits.clear(cell);
        let cand = state.clear_clue(&mut clue, cell, orig);
        debug_assert!(state == SolverState::<M>::from_digits(&digits), "incremental drift");
        // Fast path (bb's `alts == 0`): if the cleared cell still has just one naked
        // candidate, its peers already force `orig` there, so the clue was redundant
        // and uniqueness cannot have changed — keep it removed without probing. ~36%
        // of removals on a random solution, each a whole prober DFS skipped.
        if cand.len() == 1 {
            continue;
        }
        // The pre-clear puzzle was unique with `orig` at `cell`, so the cleared puzzle
        // still has that one completion; it is non-unique iff some *alternate* digit
        // also completes. Forbid `orig` at `cell` to restrict it to the alternates and
        // ask a single existence query (cap 1) — bb's `any_alt_solves`, strictly less
        // work than re-deriving the known completion via a count-to-2. The probe runs
        // on a clone so the carried board stays the clue-only naked-candidate state.
        let mut probe = state.clone();
        probe.forbid(cell, orig);
        if P::has_completion(probe) {
            digits.set(cell, orig); // an alternate completes too — keep the clue
            state.place_clue(&mut clue, cell, orig); // revert the reopen
        }
    }
    Puzzle(digits)
}

/// Generate a minimal uniquely-solvable puzzle from a fresh random solution, on the
/// production banded packing with the sieve/`Bivalue` prober at depth 3 (Bivalue's
/// natural cap — it only reads `exactly(2)`).
pub fn generate_unique(rng: &mut Rng) -> Puzzle {
    let solution = crate::fill::random_solution(rng);
    strip_to_minimal::<Bands<RowMajor>, Search<Bivalue>>(rng, &solution)
}

#[cfg(test)]
mod tests {
    use super::{generate_unique, strip_to_minimal};
    use crate::fill::random_solution;
    use crate::probe::{Propagate, Prober, Search, Singles};
    use crate::repr::banded::{Bands, RowMajor};
    use crate::repr::{CELLS, DigitGrid, FlatGridMask, Marks, SolverState};
    use crate::rng::Rng;
    use crate::scan::Bivalue;

    fn is_unique<M: Propagate>(digits: &DigitGrid) -> bool {
        Search::<Bivalue>::is_unique(SolverState::<M>::from_digits(digits))
    }

    /// Across several seeds, the stripped puzzle must be (1) a subgrid of the
    /// solution it came from, (2) uniquely solvable, and (3) minimal — removing any
    /// remaining clue makes it non-unique.
    #[test]
    fn strips_to_a_minimal_unique_puzzle() {
        for seed in 0..6 {
            let mut rng = Rng::from_seed(seed);
            let solution = random_solution(&mut rng);
            let puzzle = strip_to_minimal::<Bands<RowMajor>, Search<Bivalue>>(&mut rng, &solution);

            for c in 0..CELLS {
                if let Some(d) = puzzle.get(c) {
                    assert_eq!(Some(d), solution.get(c), "seed {seed} cell {c} off-solution");
                }
            }
            assert!(is_unique::<Bands<RowMajor>>(&puzzle.0), "seed {seed} not unique");
            for c in 0..CELLS {
                if puzzle.get(c).is_some() {
                    let mut without = puzzle.0.clone();
                    without.clear(c);
                    assert!(
                        !is_unique::<Bands<RowMajor>>(&without),
                        "seed {seed} clue {c} was removable — not minimal"
                    );
                }
            }
        }
    }

    /// The packing must not change which puzzle a seed strips to — flat and banded
    /// see the same uniqueness verdicts, so they make the same keep/remove choices.
    #[test]
    fn flat_and_banded_strip_identically() {
        for seed in 0..4 {
            let mut r_flat = Rng::from_seed(seed);
            let mut r_band = Rng::from_seed(seed);
            let sol_flat = random_solution(&mut r_flat);
            let sol_band = random_solution(&mut r_band);
            let flat = strip_to_minimal::<FlatGridMask, Search<Bivalue>>(&mut r_flat, &sol_flat);
            let band = strip_to_minimal::<Bands<RowMajor>, Search<Bivalue>>(&mut r_band, &sol_band);
            assert_eq!(flat.0.to_line(), band.0.to_line(), "seed {seed}");
        }
    }

    /// The prober must not change which puzzle a seed strips to either — the fast
    /// `Search` and the composable `Singles` give the same uniqueness verdicts, so
    /// the same keep/remove choices. Validates the `Prober` unification end to end.
    #[test]
    fn search_and_singles_strip_identically() {
        for seed in 0..3 {
            let mut r_search = Rng::from_seed(seed);
            let mut r_singles = Rng::from_seed(seed);
            let sol_search = random_solution(&mut r_search);
            let sol_singles = random_solution(&mut r_singles);
            let via_search =
                strip_to_minimal::<Bands<RowMajor>, Search<Bivalue>>(&mut r_search, &sol_search);
            let via_singles =
                strip_to_minimal::<Bands<RowMajor>, Singles>(&mut r_singles, &sol_singles);
            assert_eq!(via_search.0.to_line(), via_singles.0.to_line(), "seed {seed}");
        }
    }

    #[test]
    fn generate_unique_is_unique() {
        let mut rng = Rng::from_seed(42);
        let puzzle = generate_unique(&mut rng);
        assert!(is_unique::<Bands<RowMajor>>(&puzzle.0));
    }
}
