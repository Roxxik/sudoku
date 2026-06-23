//! Behavioural invariants of the Pelánek metrics (the model itself is unit-tested
//! inside `src/pelanek.rs`). The two load-bearing claims from the paper:
//! a singles-only puzzle records no refutation (metric ≈ 0), and a puzzle that
//! singles cannot finish forces the refutation fallback. Plus reproducibility.

use generator_lab::fill::random_solution;
use generator_lab::generate::{AttemptResult, attempt};
use generator_lab::pelanek::{Opts, grade_puzzle, sisus_run, solution_of};
use generator_lab::repr::DigitGrid;
use generator_lab::rng::Rng;
use generator_lab::spec::Spec;
use generator_lab::spec::kinds::NAKED_PAIR;

/// Cheap test knobs — a handful of runs is enough to exercise the averaging; the
/// paper's 30 is for correlation accuracy, not correctness.
fn test_opts() -> Opts {
    Opts { refutation_runs: 6, dependency_runs: 6, dependency_k: 12 }
}

/// The first generated puzzle for `spec` from `seed` upward (one seed = one
/// attempt, like `examples/find`).
fn first_puzzle(spec: &Spec, mut seed: u64) -> DigitGrid {
    loop {
        let mut rng = Rng::from_seed(seed);
        if let AttemptResult::Success(p) = attempt(&mut rng, spec) {
            return p.puzzle.0;
        }
        seed += 1;
    }
}

#[test]
fn singles_only_puzzle_records_no_refutation() {
    // A full solution with three pairwise-non-peer cells cleared: each is a naked
    // single (all 20 of its peers are filled), so the puzzle is singles-solvable
    // and the refutation fallback never fires.
    let sol = random_solution(&mut Rng::from_seed(12345));
    let mut puzzle = sol.0.clone();
    for c in [0usize, 40, 80] {
        puzzle.clear(c);
    }
    let m = grade_puzzle(&puzzle, 1, &test_opts()).expect("uniquely solvable");
    assert_eq!(m.refutation_sum, 0.0, "singles-only puzzle must score 0 refutation");
    // possibilities go 3, 2, 1 over the three independent singles, so the mean is 2.
    assert_eq!(m.dependency, 2.0, "three independent singles average to 2 options");
}

#[test]
fn non_singles_puzzle_forces_the_fallback() {
    // A puzzle that requires a naked pair: singles alone stall, so SiSuS must fall
    // back to refutation at least once.
    let spec = Spec::train(NAKED_PAIR);
    let puzzle = first_puzzle(&spec, 1);
    let solution = solution_of(&puzzle).expect("generated puzzles are uniquely solvable");

    let log = sisus_run(&puzzle, &solution, &mut Rng::from_seed(7));
    assert!(
        !log.recorded_scores.is_empty(),
        "a non-singles puzzle must record at least one stuck step",
    );
    // One cell filled per step; the per-step possibility curve has the same length.
    assert_eq!(log.possibilities.len(), puzzle.cells().iter().filter(|c| c.is_none()).count());

    let m = grade_puzzle(&puzzle, 1, &test_opts()).expect("solvable");
    assert!(m.refutation_sum.is_finite(), "refutation sum is finite");
    assert!(m.dependency > 0.0, "early frontier has at least one option");
}

#[test]
fn metrics_are_reproducible() {
    let puzzle = first_puzzle(&Spec::train(NAKED_PAIR), 100);
    let a = grade_puzzle(&puzzle, 42, &test_opts()).expect("solvable");
    let b = grade_puzzle(&puzzle, 42, &test_opts()).expect("solvable");
    assert_eq!(a.refutation_sum, b.refutation_sum, "same seed -> same refutation sum");
    assert_eq!(a.dependency, b.dependency, "same seed -> same dependency");
}
