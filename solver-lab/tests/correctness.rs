//! Correctness nets for the lab.
//!
//! 1. Every registered prober must agree with the oracle (`count_solutions(..,
//!    1) > 0`) on random partial boards — the safety net for each solver's
//!    unsafe/SIMD code. Driven by `solvers::REGISTRY`, so a newly registered
//!    variant is checked automatically with no edit here (the check itself lives
//!    in `solver_lab::check`).
//! 2. The lab's ported `baseline_solvable` must agree with core's technique
//!    solver (`solve_filtered` over the up-to-HiddenQuad set) — this is what
//!    guarantees the lab's strip trajectory stays faithful to the real
//!    generator. Uses `sudoku-core` as a dev-dependency.

use solver_lab::generate::random_full_grid;
use solver_lab::grid::{Board, CELLS};
use solver_lab::oracle::count_solutions;
use solver_lab::rng::Rng;
use solver_lab::solvers::REGISTRY;
use solver_lab::techniques::baseline_solvable;

#[test]
fn every_registered_prober_matches_oracle() {
    assert!(!REGISTRY.is_empty(), "no probers registered");
    for v in REGISTRY {
        // Panics with the variant name on the first mismatch.
        (v.check)();
    }
}

/// Convert a lab board to core (via the 81-char line) and back, so we can run
/// core's technique solver on the same grid.
fn baseline_solvable_core(b: &Board) -> bool {
    use sudoku_core::{Board as CoreBoard, TechniqueKind, solve_filtered};
    let core = CoreBoard::parse(&b.to_line().replace('.', "0")).expect("valid line");
    // allow_up_to(HiddenQuad) = every technique with difficulty <= 45.
    let allow = |t: TechniqueKind| t.difficulty() <= TechniqueKind::HiddenQuad.difficulty();
    solve_filtered(core, allow).solved
}

#[test]
fn baseline_solvable_matches_core() {
    let mut rng = Rng::from_seed(0xBADC0DE);
    let mut checked = 0;
    for _ in 0..500 {
        // Only meaningful on uniquely-solvable boards (the strip-loop invariant
        // when the baseline gate runs). Random givens are almost never unique,
        // so build a *full* grid and clear a random subset of cells, keeping it
        // only if it stays unique — the same shape a real strip produces, and a
        // good mix of baseline-solvable / not.
        let mut b = random_full_grid(&mut rng);
        let mut cells: Vec<usize> = (0..CELLS).collect();
        rng.shuffle(&mut cells);
        let clears = rng.range(20) + 40; // clear 40..=59 -> 22..=41 givens
        for &c in cells.iter().take(clears) {
            b.clear_naked(c);
        }
        if count_solutions(&b, 2) != 1 {
            continue;
        }
        assert_eq!(
            baseline_solvable(&b),
            baseline_solvable_core(&b),
            "baseline_solvable diverged from core on {}",
            b.to_line()
        );
        checked += 1;
    }
    assert!(checked > 50, "too few unique boards exercised ({checked})");
}
