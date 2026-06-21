//! Access to the `generator-lab` scalar generator — a re-export, NOT a port.
//!
//! `generator-lab` is a standalone, performance-tuned, spec-driven generator
//! crate (see `generator-lab/README.md`). Rather than reimplement it here, core
//! depends on it and re-exports its scalar `random`-method generate path through
//! this module, so the wasm bridge (and the webapp) can call the new generator
//! by going through core. Core keeps its own solver and play-time machinery
//! (`hint`, techniques, the legacy [`crate::generator`]); this module is the
//! generation entry point only.
//!
//! Only the **scalar** path is surfaced. generator-lab's W=8 SIMT warp host is a
//! native-only AVX play and is not ported to wasm, so it is intentionally not
//! re-exported here; the scalar [`generate`] runs on both native and wasm32.
//!
//! Note the names here are generator-lab's, distinct from core's own:
//! `lab::Spec`/`lab::Rng` are generator-lab types, not [`crate::Spec`]/[`crate::Rng`].
//! Build a spec with [`Spec::train`]/[`Spec::drill`] over a [`kinds`] target index.

/// The scalar generate entry point: run up to `max_attempts` strip attempts and
/// return the first puzzle satisfying `spec` (plus per-run [`Stats`]). The
/// produced [`GeneratedPuzzle`] carries the puzzle, its full solution, and the
/// given count.
pub use generator_lab::generate::{GeneratedPuzzle, Stats, generate};

/// generator-lab's seedable RNG (faithful to its strip stream). Distinct from
/// core's [`crate::Rng`]; seed it with [`Rng::from_seed`] for a deterministic run.
pub use generator_lab::rng::Rng;

/// generator-lab's compact generation [`Spec`] and its technique taxonomy
/// ([`kinds`] — the `NAKED_SINGLE`..`W_WING` target indices, `DIFFICULTY`,
/// `NAMES`, `Tier`, `Branch`). Build specs with [`Spec::train`]/[`Spec::drill`].
pub use generator_lab::spec::{self, Spec, kinds};

/// The grid newtypes a generated puzzle is made of; both wrap a `DigitGrid` whose
/// [`to_line`](generator_lab::repr::DigitGrid::to_line) renders the 81-char line
/// (`.` for an empty cell) the webapp loads.
pub use generator_lab::repr::{DigitGrid, Puzzle, Solution};

/// The per-node campaign difficulty grader. The webapp generates one puzzle at a time, so
/// it uses the absolute single-puzzle path [`grade::grade_one`] (returns the four
/// [`Signals`](generator_lab::grade::Signals) plus a stable gentle/medium/spicy band); the
/// relative batch path ([`grade::grade_batch`]/[`grade::grade_node`]) is generator-lab's.
pub use generator_lab::grade;

#[cfg(test)]
mod tests {
    use super::*;

    /// The re-exported scalar generator is reachable through core and produces a
    /// well-formed puzzle. A cheap Subset target (train(NAKED_PAIR)) keeps the
    /// attempt budget small — this only checks the access path, not the generator
    /// (generator-lab owns the correctness/faithfulness tests).
    #[test]
    fn scalar_generator_is_reachable_and_produces_a_puzzle() {
        let spec = Spec::train(kinds::NAKED_PAIR);
        let mut rng = Rng::from_seed(1);
        let (generated, stats) = generate(&mut rng, &spec, 10_000);
        let generated = generated.expect("a cheap target generates within the budget");
        assert!(stats.successes >= 1);

        let puzzle = generated.puzzle.0.to_line();
        let solution = generated.solution.0.to_line();
        assert_eq!(puzzle.len(), 81);
        assert_eq!(solution.len(), 81);
        // The given count matches the rendered clues, and the solution is full.
        assert_eq!(puzzle.chars().filter(|&c| c != '.').count(), generated.givens);
        assert_eq!(solution.chars().filter(|&c| c != '.').count(), 81);
    }
}
